//! `caer-world` — the headless client-side world model.
//!
//! This is the core of the **CPU rewrite**: the substrate that ingests the decoded packet
//! stream (`caer-protocol` events) and maintains "what is in the world right now" — every
//! visible NPC, object, and player, with live position/heading/health as ObjectUpdates flow in.
//! It has **no renderer and no network**: it is pure CPU state, exercised and benchmarked in
//! isolation (feed it a captured or synthetic packet flood, measure throughput).
//!
//! ## Layout: struct-of-arrays
//! The store is **columnar**. The fields the per-frame render pass touches — position, heading,
//! speed, id — live in their own dense, contiguous `Vec`s; the cold fields (name, guild, kind,
//! health) live in separate columns. So the hot per-frame loop streams tight arrays with no
//! heap indirection and no cache pollution from the strings it never reads — which is what lets
//! it parallelise across cores instead of stalling on memory. An id→index map keeps per-packet
//! updates O(1); all columns are index-aligned.
//!
//! ## Coordinates
//! ObjectUpdate carries **zone-local** u16 coordinates; entity/player creates carry **world**
//! coordinates. We normalise everything to one world-space `[i32; 3]` using the zone grid
//! offset (`world = grid_offset * 8192 + local`, Z absolute) so spatial queries share a frame.

use std::collections::HashMap;

use caer_protocol::entities::{EntityUpdate, Npc, Player, StaticObject};
use caer_protocol::session::ServerEvent;
use caer_protocol::status::PlayerStatus;

pub mod cfx;
pub mod dungeon_zones;
pub mod eco;
pub mod grid;
pub mod other_player;
pub mod presentation;
pub mod replay;
pub mod world_data;
mod zone_names;
mod zone_offsets;
mod zones;
pub use caer_protocol::status::PlayerStatus as PlayerVitals;
pub use cfx::{PetOwnership, PetRemainder, PetState, MULTI_PET_RULES, NECRO_BODY_RULES};
pub use dungeon_zones::{
    classify_dat_mpk, dungeon_realm_or_category, enumerate_dungeon_zone_ids, find_dat_mpk,
    is_task_dungeon_skin_zone, load_dungeon_zone, pick_random_zone_ids, DungeonRealmOrCategory,
    DungeonRepresentative, ZoneClass, CANONICAL_CLASSIC_DUNGEON_LABEL,
    CANONICAL_CLASSIC_DUNGEON_REGION, CANONICAL_CLASSIC_DUNGEON_ZONE, DUNGEON_REPRESENTATIVES,
    FALSIFIER_SEED, NAMED_DUNGEON_LABEL, NAMED_DUNGEON_ZONE, REP_ALBION, REP_DARKNESS_FALLS,
    REP_HIBERNIA, REP_MIDGARD, REP_STONEHENGE_BARROWS, REP_TASK_PROCEDURAL,
};
pub use grid::SpatialGrid;
pub use other_player::OtherPlayerAvatar;
pub use presentation::{
    effect_spawn_satisfies_typed, map_effect_resource, CadenceMark, CastCadence, CastOutcome,
    CastPresentation, CombatPresentation, CombatResultView, ConBand, EffectRequest, EffectResource,
    SoundKind, SoundRequest, MAX_COMBAT_RESULTS, MAX_ICON_SLOTS, MAX_SOUND_REQUESTS,
};
pub use zone_names::zone_name;
pub use zones::{
    region_zone_offsets, world_from_local, zone_at, zone_grid_offset, zone_id_from_packet,
    zone_region, ZONE_UNIT,
};

/// Cap on retained spell-effect particle spawn records. The wire can emit effects faster than any
/// consumer drains them; without a bound this Vec is an unbounded session leak (System 2).
pub const MAX_PARTICLE_EFFECTS: usize = 256;

/// Display size meaning "normal humanoid scale" — the oracle's `GameNPC` default (`choosenSize`).
/// Sizes are a percentage of this, so a giant skeleton at 180 renders 3.6× a person.
pub const NORMAL_SIZE: u8 = 50;

/// A particle-system spawn requested by SpellEffectAnimation 0x1B (presence wire only).
/// Does **not** claim visual fidelity — SCN-07 asserts creation, not DAoC look-alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParticleEffectSpawn {
    pub caster_id: u16,
    pub target_id: u16,
    pub spell_id: u16,
    pub success: u8,
    /// SpellEffect bolt time (tenths of a second) — lifetime / travel hint, not a wall clock.
    pub bolt_time: u16,
    pub no_sound: bool,
    pub resource: presentation::EffectResource,
}

/// Cast-bar readout driven by SpellCastAnimation 0x72 (and cleared on effect / interrupt).
///
/// SCN-07 binding: HUD / scenario probes read this — never invent progress without the packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastBarState {
    pub caster_id: u16,
    pub spell_id: u16,
    /// Cast duration from the packet (oracle tenths-of-a-second units).
    pub cast_time: u16,
}

/// What kind of thing an entity is — drives rendering, targeting, and interest rules later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Npc,
    StaticObject,
    Player,
    /// Us.
    Self_,
    /// Known only by an ObjectUpdate so far — position is real, but the create (name/model/
    /// kind) hasn't arrived yet. Upgraded in place when the create lands. In a well-formed
    /// session every Unknown resolves within a few packets.
    Unknown,
}

/// A borrowed view of one entity, assembled from the columns on demand. The scalar fields are
/// copied (they are tiny); the strings are borrowed. This is what reads return — there is no
/// single contiguous `Entity` struct to hand out, by design.
#[derive(Debug, Clone, Copy)]
pub struct EntityView<'a> {
    pub object_id: u16,
    pub kind: Kind,
    /// World-space position (see module docs).
    pub pos: [i32; 3],
    pub heading: u16,
    pub speed: u16,
    pub health_pct: u8,
    /// True after PlayerDeath 0xAE until PlayerRevive 0x89. Distinct from `health_pct == 0`
    /// (StatusUpdate can report 0 HP without a death packet — REQ-020 anti-fake).
    pub is_dead: bool,
    /// Object id this entity currently targets (0 = none).
    pub target_id: u16,
    /// Creature model id (0 for self / not-yet-created). Resolves to a NIF for rendering.
    pub model: u16,
    pub name: &'a str,
    pub guild: &'a str,
    /// Display size percentage; 50 = normal. See `WorldState::size`.
    pub size: u8,
}

/// Minimum items per rayon task in the parallel render pass. Sized so each task is tens of µs
/// of work — enough to amortise task overhead while still giving a big population plenty of
/// tasks to fill the pool. Tuned against the 16k-mob real-population benchmark.
const PAR_MIN_CHUNK: usize = 1024;

/// A dedicated thread pool for the per-frame render pass, sized to the sweet spot for this
/// cheap-per-item work rather than the whole machine. The global rayon pool (one thread per
/// core) over-parallelises it and loses to serial; ~8 threads wins. Lazily built once.
fn render_pool() -> &'static rayon::ThreadPool {
    use std::sync::OnceLock;
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(8, |c| (c.get() / 4).clamp(4, 8));
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .expect("render pool")
    })
}

/// One entity's per-frame render decision: who to draw, how far, at what detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderItem {
    pub object_id: u16,
    /// Squared distance to the camera (for depth sort without a sqrt).
    pub dist2: u64,
    /// Level of detail: 0 = full, rising with distance.
    pub lod: u8,
}

/// The live world, stored columnar (struct-of-arrays). All columns are index-aligned: entity
/// `i` is `ids[i]`, `pos[i]`, `heading[i]`, … The hot columns come first so the per-frame pass
/// strides only what it needs.
#[derive(Debug)]
pub struct WorldState {
    // --- hot columns: touched every frame by the render/interest pass ---
    ids: Vec<u16>,
    pos: Vec<[i32; 3]>,
    heading: Vec<u16>,
    speed: Vec<u16>,
    // --- cold columns: touched per-packet or on demand, never in the hot loop ---
    kind: Vec<Kind>,
    health: Vec<u8>,
    /// Set by PlayerDeath 0xAE; cleared by PlayerRevive 0x89. Not inferred from health alone.
    dead: Vec<bool>,
    target: Vec<u16>,
    /// Creature model id (NPCs/objects), from the create packet — resolves to a NIF for rendering.
    /// 0 for self and un-created placeholders.
    model: Vec<u16>,
    name: Vec<String>,
    guild: Vec<String>,
    /// Display size as a PERCENTAGE, where 50 is normal humanoid scale (oracle `GameNPC` defaults
    /// `choosenSize = 50`). Measured across the captures: ambient rats 4–10, humans 48–57, and
    /// **giant skeletons 151–199** — so ignoring this renders a giant at the size of a person.
    size: Vec<u8>,
    // --- lookup + spatial index ---
    index: HashMap<u16, usize>,
    grid: SpatialGrid,
    /// How many updates arrived for an id we had no create for yet (each spawns an `Unknown`
    /// placeholder). A diagnostic, not an error: TCP batching legitimately interleaves a create
    /// and an early update. Every one should resolve to a real create shortly after.
    pub updates_before_create: u64,
    /// Our own vitals, from the latest `CharacterStatusUpdate` (0xAD). Kept as a scalar rather
    /// than a column because there is exactly one of us: the entity columns above carry *other*
    /// livings' health as a coarse percentage, while this is our absolute current/max.
    pub player_status: PlayerStatus,
    /// The player's own character sheet (0x16 subcode 0x03). `None` until the server sends it.
    sheet: Option<caer_protocol::charsheet::CharacterSheet>,
    /// StatsUpdate 0xFB attribute totals. `None` until the first attribute packet.
    char_stats: Option<caer_protocol::stats_update::CharStatsUpdate>,
    /// StatsUpdate 0xFB resist totals. `None` until the first resist packet.
    char_resists: Option<caer_protocol::stats_update::ResistBlock>,
    /// VariousUpdate 0x16:0x05 weapon damage / skill / AF. `None` until first packet.
    weapon_armor: Option<caer_protocol::weapon_armor::WeaponArmorStats>,
    /// TimerWindow 0xF3 (Shape-2). `None` until open; cleared on close form.
    timer: Option<caer_protocol::shape2_loop::TimerWindow>,
    /// AttackMode 0x74 stance. `None` until first packet.
    attack_mode: Option<bool>,
    /// Visible equipment per object id (0x15), for armour tiers, weapons and dyes.
    equipment: std::collections::HashMap<u16, caer_protocol::equipment::EquipmentUpdate>,
    /// Other-player identity owner (PlayerCreate → resolved fig3 or unresolved diagnostic).
    /// Cleared with the entity. Unresolved is stored explicitly so fallback cannot masquerade.
    player_avatar: std::collections::HashMap<u16, OtherPlayerAvatar>,
    /// Local-player inventory slots (0x02), merged across partial updates. `None` until first packet.
    inventory: Option<std::collections::HashMap<u8, Option<caer_protocol::inventory::ItemData>>>,
    /// Self_ object id from PositionAndObjectID / PlayerPosition. Used to project worn inventory
    /// slots into [`Self::equipment`] — DOL's `UpdateEquipmentAppearance` only broadcasts 0x15 to
    /// *other* players in radius, never to the wearer (OPEN_ORACLE GamePlayer.cs).
    self_object_id: Option<u16>,
    /// Local-player purse (0xFA). `None` until first MoneyUpdate.
    money: Option<caer_protocol::money::MoneyUpdate>,
    /// Open merchant catalogue page (0x17). `None` until first MerchantWindow; replaced on each page.
    merchant: Option<caer_protocol::merchant::MerchantWindow>,
    /// Generation-gated economy correlation (invverb/merchant/money/craft/bank). Local intent
    /// never awards these columns; [`eco::EcoState::apply`] is the authority fold.
    eco: eco::EcoState,
    /// Last EmoteAnimation 0xF9. Presentation only — never a typed particle / spell effect.
    last_emote: Option<caer_protocol::emote::EmoteAnimation>,
    last_siege_anim: Option<caer_protocol::siege::SiegeWeaponAnimation>,
    siege_interface_open: bool,
    /// Server-driven target oid from ChangeTarget 0xF6 (`None` = no packet yet; `Some(0)` = clear).
    server_target_oid: Option<u16>,
    /// Last UDPInitReply 0x2F (region IP + UDP port).
    udp_init: Option<caer_protocol::view_control::UdpInitReply>,
    /// Bad/Dup name-check + create reply (char-create path).
    name_check_bad: Option<caer_protocol::shape2_loop::BadNameCheckReply>,
    name_check_dup: Option<caer_protocol::shape2_loop::DupNameCheckReply>,
    create_reply: Option<caer_protocol::shape2_loop::CharacterCreateReply>,
    /// Pending LOS request the product should answer via CheckLOSResponse.
    pending_los: Option<caer_protocol::shape2_loop::CheckLosRequest>,
    disabled_skills: Option<caer_protocol::shape2_loop::DisableSkills>,
    last_play_sound: Option<caer_protocol::shape2_loop::PlaySound>,
    last_sound_effect: Option<caer_protocol::shape2_loop::SoundEffect>,
    last_delve: Option<caer_protocol::shape2_loop::DelveInfo>,
    last_riding: Option<caer_protocol::shape2_loop::Riding>,
    controlled_horse: Option<caer_protocol::shape2_loop::ControlledHorse>,
    /// Active cast wind-ups from SpellCastAnimation 0x72, keyed by caster. Peer casts must not
    /// overwrite the local player's bar.
    active_casts: HashMap<u16, caer_protocol::spells::SpellCastAnimation>,
    /// Most recent SpellEffectAnimation 0x1B.
    last_effect: Option<caer_protocol::spells::SpellEffectAnimation>,
    /// Particle systems requested by successful spell effects (wire only — no fidelity claim).
    particle_effects: Vec<ParticleEffectSpawn>,
    /// Typed cast outcomes per caster. Never from chat. Expired by [`Self::advance_cadence`].
    cast_outcomes: HashMap<u16, presentation::CastOutcome>,
    cast_outcome_tick: HashMap<u16, u64>,
    sim_tick: u64,
    /// Bounded 0xBC combat results for HUD floaters.
    combat_results: Vec<presentation::CombatResultView>,
    /// Logical sound requests for the AUD lane.
    sound_requests: Vec<presentation::SoundRequest>,
    /// Authoritative icon list (UpdateIcons 0x7F).
    icons: Vec<caer_protocol::effects::IconEntry>,
    /// Authoritative concentration list (0x75).
    concentration: Vec<caer_protocol::effects::ConcentrationEffect>,
    /// XP / points from CharacterPointsUpdate 0x91.
    points: Option<caer_protocol::points::CharacterPoints>,
    /// Create-packet levels for con color (NPC/Player).
    entity_level: std::collections::HashMap<u16, u8>,
    /// Timing instrument (ticks). Stopped-clock PNGs are not this.
    cadence: presentation::CastCadence,
    /// Controlled pet from PetWindow 0x88. One oid; necro/multi-pet remainder is UNKNOWN.
    pet: cfx::PetState,
    /// Current region skin id from world entry / RegionChanged. 0 = unknown.
    pub region_id: u16,
    /// The player's usable skills, accumulated across `VariousUpdate` pages. The quickbar's
    /// source of truth; empty until the server sends the list during world entry.
    pub skills: Vec<caer_protocol::skills::Skill>,
}

impl Default for WorldState {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            pos: Vec::new(),
            heading: Vec::new(),
            speed: Vec::new(),
            kind: Vec::new(),
            health: Vec::new(),
            dead: Vec::new(),
            target: Vec::new(),
            model: Vec::new(),
            name: Vec::new(),
            guild: Vec::new(),
            size: Vec::new(),
            index: HashMap::new(),
            // Cell a bit above the typical interest radius: a query touches only a 2×2–3×3 block.
            grid: SpatialGrid::new(ZONE_UNIT),
            updates_before_create: 0,
            player_status: PlayerStatus::default(),
            sheet: None,
            char_stats: None,
            char_resists: None,
            weapon_armor: None,
            timer: None,
            attack_mode: None,
            equipment: std::collections::HashMap::new(),
            player_avatar: std::collections::HashMap::new(),
            inventory: None,
            self_object_id: None,
            money: None,
            merchant: None,
            eco: eco::EcoState::default(),
            last_emote: None,
            last_siege_anim: None,
            siege_interface_open: false,
            server_target_oid: None,
            udp_init: None,
            name_check_bad: None,
            name_check_dup: None,
            create_reply: None,
            pending_los: None,
            disabled_skills: None,
            last_play_sound: None,
            last_sound_effect: None,
            last_delve: None,
            last_riding: None,
            controlled_horse: None,
            active_casts: HashMap::new(),
            last_effect: None,
            particle_effects: Vec::new(),
            cast_outcomes: HashMap::new(),
            cast_outcome_tick: HashMap::new(),
            sim_tick: 0,
            combat_results: Vec::new(),
            sound_requests: Vec::new(),
            icons: Vec::new(),
            concentration: Vec::new(),
            points: None,
            entity_level: std::collections::HashMap::new(),
            cadence: presentation::CastCadence::default(),
            pet: cfx::PetState::default(),
            region_id: 0,
            skills: Vec::new(),
        }
    }
}

impl WorldState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Assemble a view of the entity at column index `i`.
    fn view_at(&self, i: usize) -> EntityView<'_> {
        EntityView {
            object_id: self.ids[i],
            kind: self.kind[i],
            pos: self.pos[i],
            heading: self.heading[i],
            speed: self.speed[i],
            health_pct: self.health[i],
            is_dead: self.dead[i],
            target_id: self.target[i],
            model: self.model[i],
            name: &self.name[i],
            guild: &self.guild[i],
            size: self.size[i],
        }
    }

    #[must_use]
    pub fn get(&self, object_id: u16) -> Option<EntityView<'_>> {
        self.index.get(&object_id).map(|&i| self.view_at(i))
    }

    /// Whether `object_id` is marked dead by PlayerDeath 0xAE (not merely 0 HP).
    #[must_use]
    pub fn is_dead(&self, object_id: u16) -> bool {
        self.index
            .get(&object_id)
            .map(|&i| self.dead[i])
            .unwrap_or(false)
    }

    /// Iterate all entities as views (cold path — clones nothing, borrows the columns).
    pub fn iter(&self) -> impl Iterator<Item = EntityView<'_>> {
        (0..self.ids.len()).map(move |i| self.view_at(i))
    }

    /// The raw position column, for spatial code and benchmarks that want the hot data directly.
    #[must_use]
    pub fn positions(&self) -> &[[i32; 3]] {
        &self.pos
    }

    /// Insert or replace an entity by id, keeping the spatial grid in sync. `pos`'s XY drives
    /// the grid; the rest are stored verbatim.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        id: u16,
        kind: Kind,
        pos: [i32; 3],
        heading: u16,
        speed: u16,
        health: u8,
        target: u16,
        model: u16,
        name: String,
        guild: String,
        size: u8,
    ) {
        let new_xy = [pos[0], pos[1]];
        if let Some(&i) = self.index.get(&id) {
            let old_xy = [self.pos[i][0], self.pos[i][1]];
            self.grid.update(id, old_xy, new_xy);
            self.ids[i] = id;
            self.pos[i] = pos;
            self.heading[i] = heading;
            self.speed[i] = speed;
            self.kind[i] = kind;
            self.health[i] = health;
            // Re-place / update must not clear a death flag — only PlayerRevive does.
            self.target[i] = target;
            self.model[i] = model;
            self.name[i] = name;
            self.guild[i] = guild;
            self.size[i] = size;
        } else {
            // Recycled object ids must not inherit side-map residue from a prior occupant that
            // left without a clean delete (or whose delete raced the next create).
            self.clear_object_components(id);
            self.grid.insert(id, new_xy);
            self.index.insert(id, self.ids.len());
            self.ids.push(id);
            self.pos.push(pos);
            self.heading.push(heading);
            self.speed.push(speed);
            self.kind.push(kind);
            self.health.push(health);
            self.dead.push(false);
            self.target.push(target);
            self.model.push(model);
            self.name.push(name);
            self.guild.push(guild);
            self.size.push(size);
        }
    }

    /// Remove an entity that left our view (`ObjectDelete` 0xE1). Returns whether it was present.
    ///
    /// Without this the world only ever grew: every mob that ever came into view stayed forever at
    /// its last known position, so a long session accumulated a crowd of ghosts that the server had
    /// long since despawned.
    ///
    /// **Central cleanup (System 2 / Sol finding 7):** every per-object component dies with the
    /// entity — columnar row, spatial grid slot, **and** side maps (equipment, particle spawns
    /// keyed to this id, active cast if this caster). Object-id reuse must not inherit a previous
    /// occupant's armour.
    ///
    /// `swap_remove` on every column keeps them dense and index-aligned, which the hot render pass
    /// depends on. The subtlety is the lookup map: swapping moves the LAST entity into slot `i`, so
    /// its index entry has to be rewritten or it would point past the end (or at a stranger). The
    /// spatial grid is keyed by position, so it must be told the *old* position to unhook from.
    pub fn remove(&mut self, object_id: u16) -> bool {
        // Side maps first — even if the columnar row is already gone (update-before-create ghost),
        // a delete must still scrub leftover components so a recycled id starts clean.
        self.clear_object_components(object_id);

        let Some(i) = self.index.remove(&object_id) else {
            return false;
        };
        self.grid
            .remove(object_id, [self.pos[i][0], self.pos[i][1]]);

        self.ids.swap_remove(i);
        self.pos.swap_remove(i);
        self.heading.swap_remove(i);
        self.speed.swap_remove(i);
        self.kind.swap_remove(i);
        self.health.swap_remove(i);
        self.dead.swap_remove(i);
        self.target.swap_remove(i);
        self.model.swap_remove(i);
        self.name.swap_remove(i);
        self.guild.swap_remove(i);
        self.size.swap_remove(i);

        // If `i` wasn't the last slot, something was swapped into it — repoint its index.
        if i < self.ids.len() {
            self.index.insert(self.ids[i], i);
        }
        true
    }

    /// Drop every non-columnar component owned by `object_id`. Called from [`Self::remove`] and
    /// from the *new* insert path so a recycled id cannot inherit stale state.
    fn clear_object_components(&mut self, object_id: u16) {
        self.equipment.remove(&object_id);
        self.player_avatar.remove(&object_id);
        self.entity_level.remove(&object_id);
        self.particle_effects
            .retain(|p| p.caster_id != object_id && p.target_id != object_id);
        self.combat_results
            .retain(|c| c.attacker_id != object_id && c.defender_id != object_id);
        self.sound_requests.retain(|s| s.source_id != object_id);
        self.active_casts.remove(&object_id);
        self.cast_outcomes.remove(&object_id);
        self.cast_outcome_tick.remove(&object_id);
        if self
            .last_effect
            .is_some_and(|e| e.caster_id == object_id || e.target_id == object_id)
        {
            self.last_effect = None;
        }
        // Clear target locks that pointed at the departed entity (other livings).
        for t in &mut self.target {
            if *t == object_id {
                *t = 0;
            }
        }
    }

    /// Move the player's own avatar (the `Kind::Self_` entity) to a new world position + heading.
    /// The client integrates its OWN movement locally (client-authoritative), so it drives the
    /// rendered box here every frame; the server only re-seeds it on spawn/teleport (PlayerPosition).
    /// Lean (no string re-alloc): updates position, heading, and the spatial grid in place. No-op
    /// until the avatar has spawned.
    /// `speed` is the character's CURRENT travel speed in world units/second, and it is
    /// load-bearing: the animation layer picks idle/walk/run from the entity's speed column
    /// (`SkinnedRig::state_for_speed`). This method used to update only position and heading, so
    /// the player's speed stayed 0 forever and the avatar played the idle clip no matter how fast
    /// it was actually moving — the "no running/walking animation" bug.
    pub fn move_self_to(&mut self, pos: [i32; 3], heading: u16, speed: u16) {
        if let Some(i) = self.kind.iter().position(|k| *k == Kind::Self_) {
            let id = self.ids[i];
            let old_xy = [self.pos[i][0], self.pos[i][1]];
            self.grid.update(id, old_xy, [pos[0], pos[1]]);
            self.pos[i] = pos;
            self.heading[i] = heading;
            self.speed[i] = speed;
        }
    }

    /// Ingest a decoded server event. Unrelated events are ignored, so the whole session stream
    /// can be piped through unfiltered.
    /// What `object_id` is visibly wearing, if the server has told us.
    #[must_use]
    pub fn equipment_of(
        &self,
        object_id: u16,
    ) -> Option<&caer_protocol::equipment::EquipmentUpdate> {
        self.equipment.get(&object_id)
    }

    /// Race+gender for another player when PlayerCreate's living-model bits decoded.
    /// Unresolved identities return `None` (never a default body).
    #[must_use]
    pub fn player_avatar_of(&self, object_id: u16) -> Option<(u8, u8)> {
        self.player_avatar
            .get(&object_id)
            .and_then(|a| a.race_gender())
    }

    /// Typed other-player identity, including unresolved living-model diagnostics.
    #[must_use]
    pub fn other_player_avatar(&self, object_id: u16) -> Option<OtherPlayerAvatar> {
        self.player_avatar.get(&object_id).copied()
    }

    /// INT hook: ingest a PlayerCreate into the typed owner (same path as `PlayerInView`).
    pub fn apply_player_create(&mut self, player: &Player) {
        self.apply_player(player);
    }

    /// Genuinely unresolved other-player living models (visible diagnostic denominator).
    #[must_use]
    pub fn unresolved_living_model_count(&self) -> usize {
        self.player_avatar
            .values()
            .filter(|a| a.is_unresolved())
            .count()
    }

    /// Object ids whose PlayerCreate living-model bits did not hit OPEN_ORACLE `eLivingModel`.
    #[must_use]
    pub fn unresolved_living_model_ids(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = self
            .player_avatar
            .iter()
            .filter(|(_, a)| a.is_unresolved())
            .map(|(&id, _)| id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Nearest `Kind::Player` within `max_range` (world XY). Self_ is not a Player.
    #[must_use]
    pub fn pick_other_player(&self, from: [i32; 2], max_range: i32) -> Option<u16> {
        let r2 = (max_range as u64).saturating_mul(max_range as u64);
        self.nearest(from, 64)
            .into_iter()
            .find(|(e, d)| e.kind == Kind::Player && *d <= r2)
            .map(|(e, _)| e.object_id)
    }

    /// Local-player inventory slot map once any InventoryUpdate has arrived.
    #[must_use]
    pub fn inventory(
        &self,
    ) -> Option<&std::collections::HashMap<u8, Option<caer_protocol::inventory::ItemData>>> {
        self.inventory.as_ref()
    }

    /// Item in a local inventory slot, if the slot is occupied.
    #[must_use]
    pub fn inventory_item(&self, slot: u8) -> Option<&caer_protocol::inventory::ItemData> {
        self.inventory.as_ref()?.get(&slot)?.as_ref()
    }

    /// Project worn inventory slots onto `equipment[self]` so Self_ mesh binding sees equip
    /// changes. No-op until both Self_ oid and inventory exist. Does not invent peer 0x15.
    fn sync_self_equipment_from_inventory(&mut self) {
        use caer_protocol::equipment::{slot as eslot, EquipmentUpdate, VisibleItem};

        let Some(oid) = self.self_object_id else {
            return;
        };
        let Some(inv) = self.inventory.as_ref() else {
            return;
        };

        let mut items = Vec::new();
        // DOL MinEquipable..=MaxEquipable (10..=37). Gaps (quiver 14..=17, jewellery, …) are fine.
        for slot in 10u8..=37 {
            let Some(Some(it)) = inv.get(&slot) else {
                continue;
            };
            let extension = if !(eslot::RIGHTHAND..=eslot::RANGED).contains(&slot) {
                Some(it.extension)
            } else {
                None
            };
            items.push(VisibleItem {
                slot,
                model: it.model,
                extension,
                texture: if it.color_or_emblem != 0 {
                    Some(it.color_or_emblem)
                } else {
                    None
                },
                effect: if it.effect != 0 {
                    Some(it.effect)
                } else {
                    None
                },
                new_emblem: false,
            });
        }
        items.sort_by_key(|i| i.slot);
        self.equipment.insert(
            oid,
            EquipmentUpdate {
                object_id: oid,
                active_weapon_slots: 0,
                speed: 0,
                cloak_hidden: false,
                helm_hidden: false,
                hood_up: false,
                active_quiver: 0,
                items,
            },
        );
    }

    /// Local-player purse once any MoneyUpdate has arrived.
    #[must_use]
    pub fn money(&self) -> Option<&caer_protocol::money::MoneyUpdate> {
        self.money.as_ref()
    }

    /// Open merchant page once any MerchantWindow 0x17 has arrived.
    #[must_use]
    pub fn merchant(&self) -> Option<&caer_protocol::merchant::MerchantWindow> {
        self.merchant.as_ref()
    }

    /// Generation-gated economy store. Local intent never writes these columns.
    #[must_use]
    pub fn eco(&self) -> &eco::EcoState {
        &self.eco
    }

    /// For INT to fold still-Raw S2C (0x1E / 0xDE) via [`eco::EcoState::apply_s2c`].
    pub fn eco_mut(&mut self) -> &mut eco::EcoState {
        &mut self.eco
    }

    #[must_use]
    pub fn last_emote(&self) -> Option<caer_protocol::emote::EmoteAnimation> {
        self.last_emote
    }

    #[must_use]
    pub fn last_siege_anim(&self) -> Option<caer_protocol::siege::SiegeWeaponAnimation> {
        self.last_siege_anim
    }

    #[must_use]
    pub fn siege_interface_open(&self) -> bool {
        self.siege_interface_open
    }

    fn apply_visual_s2c(&mut self, code: u8, payload: &[u8]) {
        use caer_protocol::codes;
        if code == codes::server::VariousUpdate {
            if let Ok(Some(wa)) = caer_protocol::weapon_armor::decode(payload) {
                self.weapon_armor = Some(wa);
            }
            return;
        }
        if code == 0xF3 {
            // TimerWindow — OPEN_ORACLE PacketLib; Shape-2. B3 may later promote to typed SessionEvent.
            if let Ok(t) = caer_protocol::shape2_loop::decode_timer_window(payload) {
                self.timer = if t.open { Some(t) } else { None };
            }
            return;
        }
        if code == codes::server::EmoteAnimation {
            if let Ok(e) = caer_protocol::emote::decode(payload) {
                self.last_emote = Some(e);
            }
            return;
        }
        if code == codes::server::SiegeWeaponAnimation {
            if let Ok(a) = caer_protocol::siege::decode_animation(payload) {
                self.last_siege_anim = Some(a);
            }
            return;
        }
        if code == codes::server::SiegeWeaponInterface {
            if let Ok(i) = caer_protocol::siege::decode_interface(payload) {
                self.siege_interface_open = !i.is_close();
            }
        }
    }

    /// Active SpellCastAnimation 0x72 for the local player when known; otherwise the sole
    /// in-flight cast (SCN-07 single-caster fixtures). Concurrent peer casts are in [`Self::cast_for`].
    #[must_use]
    pub fn active_cast(&self) -> Option<&caer_protocol::spells::SpellCastAnimation> {
        if let Some(id) = self.self_object_id {
            return self.active_casts.get(&id);
        }
        if self.active_casts.len() == 1 {
            return self.active_casts.values().next();
        }
        None
    }

    /// In-flight 0x72 for a specific caster, independent of the local HUD.
    #[must_use]
    pub fn cast_for(&self, caster_id: u16) -> Option<&caer_protocol::spells::SpellCastAnimation> {
        self.active_casts.get(&caster_id)
    }

    /// Cast-bar fields for an in-flight 0x72 (SCN-07 / spell_rt world probes).
    /// Prefers the local player when they are casting; otherwise any peer/NPC wind-up.
    #[must_use]
    pub fn cast_bar(&self) -> Option<CastBarState> {
        let c = if let Some(id) = self.self_object_id {
            self.active_casts
                .get(&id)
                .or_else(|| self.active_casts.values().next())?
        } else {
            self.active_casts.values().next()?
        };
        Some(CastBarState {
            caster_id: c.caster_id,
            spell_id: c.spell_id,
            cast_time: c.cast_time,
        })
    }

    /// Self HUD cast bar — `Some` only when the active 0x72 caster is **our** object id.
    ///
    /// Peer/NPC casts must not light the local cast UI (REDTEAM-B blocking). Until
    /// `self_object_id` is known, returns `None` so we never invent "I am casting."
    #[must_use]
    pub fn self_cast_bar(&self) -> Option<CastBarState> {
        let self_id = self.self_object_id?;
        let c = self.active_casts.get(&self_id)?;
        Some(CastBarState {
            caster_id: c.caster_id,
            spell_id: c.spell_id,
            cast_time: c.cast_time,
        })
    }

    /// Active 0x72 whose caster is `target_id` (peer/NPC target bar). Never the local HUD.
    #[must_use]
    pub fn target_cast_bar(&self, target_id: u16) -> Option<CastBarState> {
        if self.self_object_id == Some(target_id) {
            return None;
        }
        let c = self.active_casts.get(&target_id)?;
        Some(CastBarState {
            caster_id: c.caster_id,
            spell_id: c.spell_id,
            cast_time: c.cast_time,
        })
    }

    #[must_use]
    pub fn self_object_id(&self) -> Option<u16> {
        self.self_object_id
    }

    #[must_use]
    pub fn last_cast_outcome(&self) -> Option<presentation::CastOutcome> {
        if let Some(id) = self.self_object_id {
            return self.cast_outcomes.get(&id).copied();
        }
        if self.cast_outcomes.len() == 1 {
            return self.cast_outcomes.values().next().copied();
        }
        None
    }

    #[must_use]
    pub fn last_cast_outcome_for(&self, caster_id: u16) -> Option<presentation::CastOutcome> {
        self.cast_outcomes.get(&caster_id).copied()
    }

    #[must_use]
    pub fn cast_presentation(&self) -> presentation::CastPresentation {
        let self_bar = self.self_cast_bar();
        let other_bar = self.active_casts.values().find_map(|c| {
            if self.self_object_id == Some(c.caster_id) {
                None
            } else {
                Some(CastBarState {
                    caster_id: c.caster_id,
                    spell_id: c.spell_id,
                    cast_time: c.cast_time,
                })
            }
        });
        presentation::CastPresentation {
            self_bar,
            other_bar,
            outcome: self.last_cast_outcome(),
        }
    }

    #[must_use]
    pub fn combat_presentation(&self) -> presentation::CombatPresentation<'_> {
        presentation::CombatPresentation {
            cast: self.cast_presentation(),
            combat_results: &self.combat_results,
            icons: &self.icons,
            concentration: &self.concentration,
            points: self.points,
            pending_sounds: &self.sound_requests,
            pending_effects: &self.particle_effects,
            pet: self.pet.owned(),
        }
    }

    #[must_use]
    pub fn combat_results(&self) -> &[presentation::CombatResultView] {
        &self.combat_results
    }

    #[must_use]
    pub fn icons(&self) -> &[caer_protocol::effects::IconEntry] {
        &self.icons
    }

    #[must_use]
    pub fn concentration_effects(&self) -> &[caer_protocol::effects::ConcentrationEffect] {
        &self.concentration
    }

    #[must_use]
    pub fn character_points(&self) -> Option<caer_protocol::points::CharacterPoints> {
        self.points
    }

    #[must_use]
    pub fn sound_requests(&self) -> &[presentation::SoundRequest] {
        &self.sound_requests
    }

    pub fn drain_sound_requests(&mut self) -> Vec<presentation::SoundRequest> {
        std::mem::take(&mut self.sound_requests)
    }

    #[must_use]
    pub fn cadence(&self) -> &presentation::CastCadence {
        &self.cadence
    }

    pub fn advance_cadence(&mut self, ticks: u64) {
        self.cadence.advance(ticks);
        self.sim_tick = self.sim_tick.saturating_add(ticks);
        self.expire_cast_outcomes();
    }

    fn expire_cast_outcomes(&mut self) {
        const TTL: u64 = 90;
        let now = self.sim_tick;
        let expired: Vec<u16> = self
            .cast_outcome_tick
            .iter()
            .filter(|(_, at)| now.saturating_sub(**at) >= TTL)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.cast_outcome_tick.remove(&id);
            self.cast_outcomes.remove(&id);
        }
    }

    fn remember_outcome(&mut self, caster_id: u16, outcome: presentation::CastOutcome) {
        self.cast_outcomes.insert(caster_id, outcome);
        self.cast_outcome_tick.insert(caster_id, self.sim_tick);
    }

    /// Create-packet level for con. `None` until NPCCreate / PlayerCreate.
    #[must_use]
    pub fn level_of(&self, object_id: u16) -> Option<u8> {
        self.entity_level.get(&object_id).copied()
    }

    /// Con band of `target_id` vs the local sheet level. `None` without both levels.
    #[must_use]
    pub fn con_of(&self, target_id: u16) -> Option<presentation::ConBand> {
        let viewer = self.sheet.as_ref().map(|s| s.level)?;
        let target = self.level_of(target_id)?;
        Some(presentation::ConBand::from_levels(viewer, target))
    }

    fn apply_icons(&mut self, u: &caer_protocol::effects::UpdateIcons) {
        for slot in &u.slots {
            match slot {
                caer_protocol::effects::IconSlot::Cleared { index } => {
                    self.icons.retain(|e| e.index != *index);
                }
                caer_protocol::effects::IconSlot::Live(e) => {
                    if let Some(existing) = self.icons.iter_mut().find(|x| x.index == e.index) {
                        *existing = e.clone();
                    } else if self.icons.len() < presentation::MAX_ICON_SLOTS {
                        self.icons.push(e.clone());
                    }
                }
            }
        }
        self.icons.sort_by_key(|e| e.index);
    }

    /// Most recent SpellEffectAnimation 0x1B.
    #[must_use]
    pub fn last_effect(&self) -> Option<&caer_protocol::spells::SpellEffectAnimation> {
        self.last_effect.as_ref()
    }

    /// Particle systems spawned from successful spell effects (presence only).
    #[must_use]
    pub fn particle_effects(&self) -> &[ParticleEffectSpawn] {
        &self.particle_effects
    }

    /// Drain retained particle spawns for a consumer (render / VFX). Leaves the queue empty.
    pub fn drain_particle_effects(&mut self) -> Vec<ParticleEffectSpawn> {
        std::mem::take(&mut self.particle_effects)
    }

    /// Append a particle spawn, dropping the oldest when the queue is at [`MAX_PARTICLE_EFFECTS`].
    fn push_particle(&mut self, spawn: ParticleEffectSpawn) {
        if self.particle_effects.len() >= MAX_PARTICLE_EFFECTS {
            self.particle_effects.remove(0);
        }
        self.particle_effects.push(spawn);
    }

    /// Clear player-local transients that are invalid across a zone/region boundary.
    fn clear_zone_transients(&mut self) {
        self.merchant = None;
        self.active_casts.clear();
        self.last_effect = None;
        self.cast_outcomes.clear();
        self.cast_outcome_tick.clear();
        self.particle_effects.clear();
        self.last_emote = None;
        self.last_siege_anim = None;
        self.siege_interface_open = false;
        self.combat_results.clear();
        self.sound_requests.clear();
        self.icons.clear();
        self.concentration.clear();
        self.cadence.clear();
        self.pet.clear();
    }

    /// Typed region hook for INT (`AudioBus::on_region_changed` is the audio twin).
    pub fn on_region_changed(&mut self, region_id: u16) {
        self.begin_region_change(region_id);
    }

    /// In-world region transition: cull every visible entity and latch the new region id.
    /// The server re-sends creates for the destination; keeping old entities would leave ghosts
    /// from the previous region at recycled object ids.
    pub fn begin_region_change(&mut self, region_id: u16) {
        self.ids.clear();
        self.pos.clear();
        self.heading.clear();
        self.speed.clear();
        self.kind.clear();
        self.health.clear();
        self.dead.clear();
        self.target.clear();
        self.model.clear();
        self.name.clear();
        self.guild.clear();
        self.size.clear();
        self.index.clear();
        self.grid = SpatialGrid::new(ZONE_UNIT);
        self.equipment.clear();
        self.player_avatar.clear();
        self.entity_level.clear();
        self.clear_zone_transients();
        self.updates_before_create = 0;
        self.region_id = region_id;
    }

    /// Session teardown on clean logout ([`ServerEvent::LoggedOut`]): cull entities **and** drop
    /// player-local caches (inventory, money, sheet, skills, status, self oid).
    ///
    /// Region change keeps those — same character, new zone. Logout must not: a reconnect that
    /// restored stale local inventory/equipment would look like a successful re-entry while
    /// silently disagreeing with the server (SCN-12).
    pub fn clear_for_logout(&mut self) {
        self.begin_region_change(0);
        self.player_status = PlayerStatus::default();
        self.sheet = None;
        self.char_stats = None;
        self.char_resists = None;
        self.weapon_armor = None;
        self.timer = None;
        self.attack_mode = None;
        self.inventory = None;
        self.self_object_id = None;
        self.money = None;
        self.skills.clear();
        self.points = None;
        self.icons.clear();
        self.concentration.clear();
        self.cast_outcomes.clear();
        self.cast_outcome_tick.clear();
        self.combat_results.clear();
        self.sound_requests.clear();
        self.cadence.clear();
        self.pet.clear();
    }

    /// Unit-level session reset INT calls on logout/reconnect.
    pub fn on_logout(&mut self) {
        self.clear_for_logout();
    }

    /// Reconnect uses the same reset as logout (stale local caches must not survive).
    pub fn on_reconnect(&mut self) {
        self.clear_for_logout();
    }

    #[must_use]
    pub fn owned_pet(&self) -> Option<&cfx::PetOwnership> {
        self.pet.owned()
    }

    /// The player's own character sheet, once the server has sent it. `None` before then — the
    /// summary window shows an empty field rather than a plausible-looking lie.
    #[must_use]
    pub fn character_sheet(&self) -> Option<&caer_protocol::charsheet::CharacterSheet> {
        self.sheet.as_ref()
    }

    #[must_use]
    pub fn char_stats(&self) -> Option<&caer_protocol::stats_update::CharStatsUpdate> {
        self.char_stats.as_ref()
    }

    #[must_use]
    pub fn char_resists(&self) -> Option<&caer_protocol::stats_update::ResistBlock> {
        self.char_resists.as_ref()
    }

    #[must_use]
    pub fn weapon_armor(&self) -> Option<&caer_protocol::weapon_armor::WeaponArmorStats> {
        self.weapon_armor.as_ref()
    }

    #[must_use]
    pub fn timer_window(&self) -> Option<&caer_protocol::shape2_loop::TimerWindow> {
        self.timer.as_ref()
    }

    #[must_use]
    pub fn attack_mode(&self) -> Option<bool> {
        self.attack_mode
    }

    pub fn apply(&mut self, ev: &ServerEvent) {
        let eco_result = self.eco.apply(ev);
        if let ServerEvent::Raw { code, payload } = ev {
            let _ = self.eco.apply_s2c(*code, payload);
            self.apply_visual_s2c(*code, payload);
        }
        match ev {
            ServerEvent::NpcInView(npc) => self.apply_npc(npc),
            ServerEvent::ObjectInView(obj) => self.apply_object(obj),
            ServerEvent::PlayerInView(p) => self.apply_player(p),
            ServerEvent::EntityUpdated(u) => self.apply_update(u),
            ServerEvent::StatusUpdate(s) => self.player_status = *s,
            ServerEvent::CharacterSheet(sheet) => {
                self.sheet = Some(sheet.clone());
                if let Some(oid) = self.self_object_id {
                    self.entity_level.insert(oid, sheet.level);
                }
            }
            ServerEvent::StatsUpdated(s) => match s {
                caer_protocol::stats_update::StatsUpdate::Attributes(a) => {
                    self.char_stats = Some(a.clone());
                }
                caer_protocol::stats_update::StatsUpdate::Resists(r) => {
                    self.char_resists = Some(*r);
                }
            },
            ServerEvent::AttackMode { attacking } => {
                self.attack_mode = Some(*attacking);
            }
            ServerEvent::EmoteAnimation(e) => {
                self.last_emote = Some(*e);
            }
            ServerEvent::SiegeWeaponAnimation(a) => {
                self.last_siege_anim = Some(*a);
            }
            ServerEvent::SiegeWeaponInterface(i) => {
                self.siege_interface_open = !i.is_close();
            }
            ServerEvent::EquipmentUpdated(e) => {
                self.equipment.insert(e.object_id, e.clone());
            }
            ServerEvent::InventoryUpdated(u) => {
                // Vault/consignment 0x02 pages are bank authority, not the worn/bag map.
                // Stale eco generations must not mutate player-visible inventory.
                if u.is_vault_or_consignment() || eco_result != eco::EcoApplyResult::Applied {
                    // eco already folded vault into BankCorr when Applied.
                } else {
                    let map = self
                        .inventory
                        .get_or_insert_with(std::collections::HashMap::new);
                    for entry in &u.items {
                        map.insert(entry.slot, entry.item.clone());
                    }
                    // Self avatar mesh binds through equipment_of; peers use 0x15. Project worn
                    // inventory onto Self_ so equip verbs change pixels without inventing a self-0x15.
                    self.sync_self_equipment_from_inventory();
                }
            }
            ServerEvent::MoneyUpdated(m) => {
                if eco_result == eco::EcoApplyResult::Applied {
                    self.money = Some(*m);
                }
            }
            ServerEvent::MerchantWindow(w) => {
                if eco_result == eco::EcoApplyResult::Applied {
                    self.merchant = Some(w.clone());
                }
            }
            ServerEvent::SpellCast(c) => {
                self.active_casts.insert(c.caster_id, *c);
                self.cast_outcomes.remove(&c.caster_id);
                self.cast_outcome_tick.remove(&c.caster_id);
                presentation::push_bounded(
                    &mut self.sound_requests,
                    presentation::sound_for_cast(c),
                    presentation::MAX_SOUND_REQUESTS,
                );
                self.cadence.observe(presentation::CadenceMark::CastStart {
                    caster_id: c.caster_id,
                    spell_id: c.spell_id,
                });
            }
            ServerEvent::SpellEffect(e) => {
                self.last_effect = Some(*e);
                // Cast finished (success or resist) — cast bar completes; do not leave it stuck.
                self.active_casts.remove(&e.caster_id);
                self.remember_outcome(e.caster_id, presentation::outcome_from_effect(e));
                if e.succeeded() {
                    self.push_particle(ParticleEffectSpawn {
                        caster_id: e.caster_id,
                        target_id: e.target_id,
                        spell_id: e.spell_id,
                        success: e.success,
                        bolt_time: e.bolt_time,
                        no_sound: e.no_sound,
                        resource: presentation::map_effect_resource(e.spell_id),
                    });
                    if let Some(s) = presentation::sound_for_effect(e) {
                        presentation::push_bounded(
                            &mut self.sound_requests,
                            s,
                            presentation::MAX_SOUND_REQUESTS,
                        );
                    }
                    self.cadence
                        .observe(presentation::CadenceMark::EffectSuccess {
                            caster_id: e.caster_id,
                            spell_id: e.spell_id,
                        });
                } else {
                    self.cadence.observe(presentation::CadenceMark::EffectFail {
                        caster_id: e.caster_id,
                        spell_id: e.spell_id,
                    });
                }
            }
            ServerEvent::RegionHandoff { .. } => {
                // Merchant pages, casts, and local particle queues are zone-scoped UI/transients.
                // Inventory/money survive handoff (same character); eco merchant must not linger.
                self.clear_zone_transients();
                self.eco.merchant = eco::MerchantCorr::default();
            }
            ServerEvent::RegionChanged(r) => {
                self.begin_region_change(r.region_id);
            }
            ServerEvent::LoggedOut { .. } => {
                self.clear_for_logout();
            }
            ServerEvent::SpellInterrupted(i) => {
                self.active_casts.remove(&i.object_id);
                self.remember_outcome(i.object_id, presentation::outcome_from_interrupt(i));
                // Interrupted casts must not leave windup, land sound, or a completed outcome.
                self.sound_requests.retain(|s| s.source_id != i.object_id);
                self.cadence.observe(presentation::CadenceMark::Interrupt {
                    object_id: i.object_id,
                });
            }
            ServerEvent::PlayerDied(d) => {
                if let Some(&i) = self.index.get(&d.object_id) {
                    self.dead[i] = true;
                    self.health[i] = 0;
                    self.speed[i] = 0;
                }
                self.active_casts.remove(&d.object_id);
                self.cast_outcomes.remove(&d.object_id);
                self.cast_outcome_tick.remove(&d.object_id);
                self.particle_effects
                    .retain(|p| p.caster_id != d.object_id && p.target_id != d.object_id);
            }
            ServerEvent::PlayerRevived(v) => {
                if let Some(&i) = self.index.get(&v.object_id) {
                    self.dead[i] = false;
                    // HP comes back on a subsequent StatusUpdate / ObjectUpdate — don't invent it.
                }
            }
            ServerEvent::CombatAnimation(a) => {
                // Apply defender HP% from the packet — structured, not chat-scraped.
                if a.defender_id != 0 {
                    if let Some(&i) = self.index.get(&a.defender_id) {
                        self.health[i] = a.target_health_pct;
                    }
                }
                presentation::push_bounded(
                    &mut self.combat_results,
                    presentation::CombatResultView::from_anim(a),
                    presentation::MAX_COMBAT_RESULTS,
                );
            }
            ServerEvent::UpdateIcons(u) => {
                self.apply_icons(u);
            }
            ServerEvent::ConcentrationList(l) => {
                self.concentration = l.effects.clone();
                if self.concentration.len() > presentation::MAX_ICON_SLOTS {
                    self.concentration.truncate(presentation::MAX_ICON_SLOTS);
                }
            }
            ServerEvent::CharacterPoints(p) => {
                self.points = Some(*p);
            }
            ServerEvent::PetWindow(w) => {
                self.pet.apply_window(w);
            }
            ServerEvent::SkillsPage(page) => {
                caer_protocol::skills::apply_page(&mut self.skills, page.clone());
            }
            ServerEvent::ObjectRemoved { object_id } => {
                self.pet.on_object_removed(*object_id);
                self.remove(*object_id);
            }
            ServerEvent::RemoveObject(o) => {
                self.pet.on_object_removed(o.object_id);
                self.remove(o.object_id);
            }
            ServerEvent::TargetChanged(t) => {
                self.server_target_oid = Some(t.object_id);
            }
            ServerEvent::UdpInitReply(u) => {
                self.udp_init = Some(u.clone());
            }
            ServerEvent::BadNameCheckReply(r) => {
                self.name_check_bad = Some(r.clone());
            }
            ServerEvent::DupNameCheckReply(r) => {
                self.name_check_dup = Some(r.clone());
            }
            ServerEvent::CharacterCreateReply(r) => {
                self.create_reply = Some(r.clone());
            }
            ServerEvent::CheckLosRequest(r) => {
                self.pending_los = Some(*r);
            }
            ServerEvent::TimerWindow(t) => {
                self.timer = if t.open { Some(t.clone()) } else { None };
            }
            ServerEvent::DisableSkills(d) => {
                self.disabled_skills = Some(d.clone());
            }
            ServerEvent::PlaySound(s) => {
                self.last_play_sound = Some(*s);
            }
            ServerEvent::SoundEffect(s) => {
                self.last_sound_effect = Some(*s);
            }
            ServerEvent::ModelChange(m) => {
                if let Some(&i) = self.index.get(&m.object_id) {
                    self.model[i] = m.new_model;
                    self.size[i] = m.new_size;
                }
            }
            ServerEvent::MovingObjectCreate(m) => {
                self.place(
                    m.object_id,
                    Kind::Npc,
                    [m.x as i32, m.y as i32, m.z as i32],
                    m.heading,
                    0,
                    100,
                    0,
                    m.model,
                    m.name.clone(),
                    String::new(),
                    NORMAL_SIZE,
                );
            }
            ServerEvent::ObjectDataUpdate(o) => {
                if let Some(&i) = self.index.get(&o.object_id) {
                    if !o.strings_omitted {
                        if !o.name.is_empty() {
                            self.name[i] = o.name.clone();
                        }
                        if !o.guild.is_empty() {
                            self.guild[i] = o.guild.clone();
                        }
                    }
                }
                self.entity_level.insert(o.object_id, o.level);
            }
            ServerEvent::Riding(r) => {
                self.last_riding = Some(*r);
            }
            ServerEvent::PlayerModelTypeChange(_p) => {
                // Appearance flag latch; mesh path consumes model_type when fig3 resolves it.
            }
            ServerEvent::DelveInfo(d) => {
                self.last_delve = Some(d.clone());
            }
            ServerEvent::ControlledHorse(h) => {
                self.controlled_horse = Some(*h);
            }
            ServerEvent::PlayerPosition {
                x,
                y,
                z,
                object_id,
                heading,
            } => {
                self.self_object_id = Some(*object_id);
                self.place(
                    *object_id,
                    Kind::Self_,
                    [*x as i32, *y as i32, *z as i32],
                    *heading,
                    0,
                    100,
                    0,
                    0,
                    "<self>".into(),
                    String::new(),
                    NORMAL_SIZE,
                );
                self.sync_self_equipment_from_inventory();
            }
            _ => {}
        }
    }

    fn apply_npc(&mut self, npc: &Npc) {
        self.place(
            npc.object_id,
            Kind::Npc,
            [npc.x as i32, npc.y as i32, npc.z as i32],
            npc.heading,
            npc.speed,
            100,
            0,
            npc.model,
            npc.name.clone(),
            npc.guild.clone(),
            npc.size,
        );
        self.entity_level.insert(npc.object_id, npc.level);
    }

    /// Ingest another player (PlayerCreate 0x4B).
    ///
    /// The creature-model column stays **0**: `model_unverified` is not a monsters.csv id (see
    /// [`caer_protocol::entities::Player`]). Identity is stored as [`OtherPlayerAvatar`]: OPEN_ORACLE
    /// `eLivingModel` hits latch race+gender; unknown shorts stay [`OtherPlayerAvatar::Unresolved`]
    /// (diagnostic box + counter — never a default body).
    fn apply_player(&mut self, p: &Player) {
        if self.self_object_id == Some(p.object_id) {
            // Local oid is Kind::Self_. PlayerCreate must not demote it into the other-player map.
            self.player_avatar.remove(&p.object_id);
            return;
        }
        self.place(
            p.object_id,
            Kind::Player,
            [p.x as i32, p.y as i32, p.z as i32],
            p.heading,
            0,
            100,
            0,
            0,
            p.name.clone(),
            p.guild.clone(),
            // PlayerCreate carries no size byte — other players are normal humanoid scale.
            NORMAL_SIZE,
        );
        self.player_avatar
            .insert(p.object_id, OtherPlayerAvatar::from_player_create(p));
        self.entity_level.insert(p.object_id, p.level);
    }

    fn apply_object(&mut self, obj: &StaticObject) {
        self.place(
            obj.object_id,
            Kind::StaticObject,
            [obj.x as i32, obj.y as i32, obj.z as i32],
            obj.heading,
            0,
            100,
            0,
            obj.model,
            obj.name.clone(),
            String::new(),
            NORMAL_SIZE,
        );
    }

    /// Apply a live movement/state update. For a known entity this is the hot per-packet path:
    /// touch only the hot columns + the grid, never the name/guild strings. For an unknown id,
    /// spawn an `Unknown` placeholder at the update's position — a later create upgrades it.
    fn apply_update(&mut self, u: &EntityUpdate) {
        let zone_id = crate::zone_id_from_packet(u.zone, u.flags);
        let pos = world_from_local(u.local_x, u.local_y, u.z, zone_id);
        match self.index.get(&u.object_id).copied() {
            Some(i) => {
                let old_xy = [self.pos[i][0], self.pos[i][1]];
                self.grid.update(u.object_id, old_xy, [pos[0], pos[1]]);
                self.pos[i] = pos;
                self.heading[i] = u.heading;
                self.speed[i] = u.speed;
                self.health[i] = u.health_pct;
                self.target[i] = u.target_id;
            }
            None => {
                self.updates_before_create += 1;
                self.place(
                    u.object_id,
                    Kind::Unknown,
                    pos,
                    u.heading,
                    u.speed,
                    u.health_pct,
                    u.target_id,
                    0,
                    String::new(),
                    String::new(),
                    NORMAL_SIZE,
                );
            }
        }
    }

    /// Entities still known only by position (no create yet). Should be empty once a session's
    /// create packets have all arrived — a ground-truth invariant.
    #[must_use]
    pub fn unresolved(&self) -> usize {
        self.kind.iter().filter(|&&k| k == Kind::Unknown).count()
    }

    /// Object ids we have a position for but no create — the ones to ask the server about with
    /// `CreateNPCRequest`. These are the entities that would otherwise sit in the world as
    /// unidentified forever.
    #[must_use]
    pub fn unresolved_ids(&self) -> Vec<u16> {
        self.kind
            .iter()
            .enumerate()
            .filter(|(_, &k)| k == Kind::Unknown)
            .map(|(i, _)| self.ids[i])
            .collect()
    }

    /// The `k` entities nearest a world point (2-D distance; Z rarely matters for interest).
    /// Naive O(N) scan over the position column — the honest baseline the grid beats.
    #[must_use]
    pub fn nearest(&self, world: [i32; 2], k: usize) -> Vec<(EntityView<'_>, u64)> {
        let mut scored: Vec<(usize, u64)> = self
            .pos
            .iter()
            .enumerate()
            .map(|(i, &p)| (i, dist2(p, world)))
            .collect();
        scored.sort_unstable_by_key(|&(_, d)| d);
        scored.truncate(k);
        scored
            .into_iter()
            .map(|(i, d)| (self.view_at(i), d))
            .collect()
    }

    /// Count of entities within `radius` (world units) of a point — the interest-management
    /// primitive (how many things must I render/simulate here). Naive O(N) baseline.
    #[must_use]
    pub fn count_within(&self, world: [i32; 2], radius: i32) -> usize {
        let r2 = (radius as u64) * (radius as u64);
        self.pos.iter().filter(|&&p| dist2(p, world) <= r2).count()
    }

    /// Grid-accelerated `nearest` — the O(nearby) path.
    #[must_use]
    pub fn nearest_indexed(&self, world: [i32; 2], k: usize) -> Vec<(EntityView<'_>, u64)> {
        self.grid
            .nearest(world, k)
            .into_iter()
            .filter_map(|(id, d)| self.get(id).map(|e| (e, d)))
            .collect()
    }

    /// Grid-accelerated `count_within` — the O(nearby) path.
    #[must_use]
    pub fn count_within_indexed(&self, world: [i32; 2], radius: i32) -> usize {
        self.grid.count_within(world, radius)
    }

    /// The spatial index, for direct benchmarking/inspection.
    #[must_use]
    pub fn grid(&self) -> &SpatialGrid {
        &self.grid
    }

    /// Build the render set for a frame: for every entity within `radius` of the camera,
    /// extrapolate its position from its last update, compute its distance, pick an LOD bucket.
    /// Streams only the hot columns (id/pos/heading/speed) — no strings touched. Serial version.
    #[must_use]
    pub fn render_set(&self, camera: [i32; 2], radius: i32, dt_ms: u32) -> Vec<RenderItem> {
        (0..self.ids.len())
            .filter_map(|i| {
                render_item(
                    self.ids[i],
                    self.pos[i],
                    self.heading[i],
                    self.speed[i],
                    camera,
                    radius,
                    dt_ms,
                )
            })
            .collect()
    }

    /// Parallel render set — the same hot-column pass spread across a **dedicated, right-sized
    /// thread pool** (see [`render_pool`]). Because it strides only the dense position/heading/
    /// speed arrays (no heap indirection) it is compute-bound and scales — but only up to a
    /// point: this per-item work is cheap enough that splitting it across *all* cores loses to
    /// coordination overhead. Measured on the 16k-mob real population, ~8 threads is the sweet
    /// spot; the global 36-thread pool over-parallelises and is *slower* than serial. `with_min_len`
    /// additionally keeps each task coarse so the result-merge stays cheap.
    #[must_use]
    pub fn render_set_par(&self, camera: [i32; 2], radius: i32, dt_ms: u32) -> Vec<RenderItem> {
        use rayon::prelude::*;
        render_pool().install(|| {
            (0..self.ids.len())
                .into_par_iter()
                .with_min_len(PAR_MIN_CHUNK)
                .filter_map(|i| {
                    render_item(
                        self.ids[i],
                        self.pos[i],
                        self.heading[i],
                        self.speed[i],
                        camera,
                        radius,
                        dt_ms,
                    )
                })
                .collect()
        })
    }
}

impl<'a> IntoIterator for &'a WorldState {
    type Item = EntityView<'a>;
    type IntoIter =
        std::iter::Map<std::ops::Range<usize>, Box<dyn Fn(usize) -> EntityView<'a> + 'a>>;
    fn into_iter(self) -> Self::IntoIter {
        let f: Box<dyn Fn(usize) -> EntityView<'a> + 'a> = Box::new(move |i| self.view_at(i));
        (0..self.ids.len()).map(f)
    }
}

/// The per-entity frame work: extrapolate the entity's position along its heading by `dt_ms`
/// (dead-reckoning between server updates — what a smooth client does every frame), cull by
/// radius, and bucket LOD by distance. Non-trivial per entity (a trig pair + distance), which
/// is exactly why doing it for a whole zerg serially melts a core.
#[inline]
fn render_item(
    id: u16,
    pos: [i32; 3],
    heading: u16,
    speed: u16,
    camera: [i32; 2],
    radius: i32,
    dt_ms: u32,
) -> Option<RenderItem> {
    // DAoC heading is 0..4096 over a full turn.
    let theta = f32::from(heading) * (std::f32::consts::TAU / 4096.0);
    let travel = f32::from(speed) * (dt_ms as f32 / 1000.0);
    let px = pos[0] + (travel * theta.sin()) as i32;
    let py = pos[1] + (travel * theta.cos()) as i32;

    let dx = i64::from(px - camera[0]);
    let dy = i64::from(py - camera[1]);
    let d2 = (dx * dx + dy * dy) as u64;

    let r = i64::from(radius);
    if d2 > (r * r) as u64 {
        return None; // culled: outside the render radius
    }
    // LOD rings at 1/3 and 2/3 of the radius.
    let third = (r / 3).max(1) as u64;
    let lod = if d2 <= third * third {
        0
    } else if d2 <= (2 * third) * (2 * third) {
        1
    } else {
        2
    };
    Some(RenderItem {
        object_id: id,
        dist2: d2,
        lod,
    })
}

/// Squared 2-D distance from a position to a world point (u64 to avoid overflow at DAoC's
/// ~500k coordinate magnitudes).
fn dist2(pos: [i32; 3], world: [i32; 2]) -> u64 {
    let dx = (pos[0] - world[0]) as i64;
    let dy = (pos[1] - world[1]) as i64;
    (dx * dx + dy * dy) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::entities::{Npc, StaticObject};

    /// Removal must keep every column, the lookup index and the spatial grid consistent — and in
    /// particular must fix up the entity that `swap_remove` relocates. Removing the FIRST of
    /// several is the case that exercises that; removing the last does not.
    #[test]
    fn removing_an_entity_keeps_columns_index_and_grid_consistent() {
        let mut w = WorldState::default();
        for (id, x) in [(10u16, 1000i32), (20, 2000), (30, 3000), (40, 4000)] {
            w.apply(&npc(id, x, x, &format!("mob{id}")));
        }
        assert_eq!(w.len(), 4);

        // Remove the first: the last entity (40) gets swapped into slot 0.
        assert!(w.remove(10));
        assert_eq!(w.len(), 3);
        assert!(w.get(10).is_none(), "removed entity still resolvable");

        // Every survivor must still be findable BY ID and carry its own data — a stale index would
        // hand back the wrong entity here rather than failing outright.
        for (id, x) in [(20u16, 2000i32), (30, 3000), (40, 4000)] {
            let e = w
                .get(id)
                .unwrap_or_else(|| panic!("entity {id} lost after removal"));
            assert_eq!(e.object_id, id);
            assert_eq!(e.pos[0], x, "entity {id} has another entity's position");
            assert_eq!(
                e.name,
                format!("mob{id}"),
                "entity {id} has another entity's name"
            );
        }

        // The spatial index must agree with the columns, or culling queries would resurrect ghosts.
        assert_eq!(w.iter().count(), 3);
        let near: Vec<u16> = w
            .nearest([1000, 1000], 4)
            .into_iter()
            .map(|(e, _)| e.object_id)
            .collect();
        assert!(
            !near.contains(&10),
            "removed entity still in the spatial grid: {near:?}"
        );

        // Removing something unknown is a no-op, not a panic: the server can delete an object we
        // never saw created (TCP interleaving, or one that spawned outside our interest radius).
        assert!(!w.remove(999));
        assert_eq!(w.len(), 3);

        // Draining everything must leave a genuinely empty world.
        for id in [20, 30, 40] {
            assert!(w.remove(id));
        }
        assert!(w.is_empty());
        assert_eq!(w.iter().count(), 0);
    }

    /// Another player must enter the world as `Kind::Player`, keep their name and guild, and
    /// carry model 0 — the "assemble me from the avatar tables" marker. Filling the model column
    /// from the packet's unverified model short would render a player as an arbitrary creature.
    #[test]
    fn players_enter_the_world_with_no_guessed_model() {
        let mut w = WorldState::default();
        w.apply(&ServerEvent::PlayerInView(Player {
            object_id: 16745,
            session_id: 1,
            x: 561_400.0,
            y: 511_410.0,
            z: 2329.0,
            heading: 3040,
            // Captured Feile: low-11 living model 0x102 = 258 — not an OPEN_ORACLE eLivingModel.
            model_unverified: 0x9102,
            level: 29,
            realm: 1,
            flags: 0x04,
            name: "Feile".into(),
            guild: "Clan Cotswold".into(),
            last_name: String::new(),
            custom: caer_protocol::customization::Customization::default(),
            eye_size: 0,
            lip_size: 0,
        }));

        let e = w.get(16745).expect("player not in world");
        assert_eq!(e.kind, Kind::Player);
        assert_eq!(e.name, "Feile");
        assert_eq!(e.guild, "Clan Cotswold");
        assert_eq!(e.pos, [561_400, 511_410, 2329]);
        assert_eq!(e.heading, 3040);
        assert_eq!(
            e.model, 0,
            "the unverified model short must NOT drive monsters.csv"
        );
        assert!(
            w.player_avatar_of(16745).is_none(),
            "unknown living model must not invent race/gender"
        );
        assert_eq!(
            w.other_player_avatar(16745),
            Some(OtherPlayerAvatar::Unresolved { living_model: 258 })
        );
        assert_eq!(w.unresolved_living_model_count(), 1);

        // Known OPEN_ORACLE HighlanderFemale (43) with size/hair bits packed above → avatar latch.
        w.apply(&ServerEvent::PlayerInView(Player {
            object_id: 99,
            session_id: 2,
            x: 100.0,
            y: 100.0,
            z: 10.0,
            heading: 0,
            model_unverified: 0x8000 | 43, // hair bits + HighlanderFemale
            level: 50,
            realm: 1,
            flags: 0x04,
            name: "Known".into(),
            guild: String::new(),
            last_name: String::new(),
            custom: caer_protocol::customization::Customization::default(),
            eye_size: 0,
            lip_size: 0,
        }));
        assert_eq!(w.player_avatar_of(99), Some((3, 2)));

        // And a player leaving view is culled like anything else — avatar map too.
        w.apply(&ServerEvent::ObjectRemoved { object_id: 16745 });
        assert!(w.get(16745).is_none());
        w.apply(&ServerEvent::ObjectRemoved { object_id: 99 });
        assert!(w.player_avatar_of(99).is_none());
    }

    /// The player's own speed must reach the entity columns: the animation layer selects
    /// idle/walk/run from it, so a permanently-zero speed pins the avatar in its idle clip however
    /// fast it is actually moving. That was the "no walking animation" bug — `move_self_to` wrote
    /// position and heading but never speed.
    #[test]
    fn moving_the_player_updates_its_speed_column() {
        let mut w = WorldState::default();
        w.apply(&ServerEvent::PlayerPosition {
            x: 100.0,
            y: 100.0,
            z: 10.0,
            object_id: 7,
            heading: 0,
        });
        assert_eq!(
            w.get(7).expect("self entity").speed,
            0,
            "standing still starts at zero"
        );

        w.move_self_to([200, 100, 10], 512, 150);
        let e = w.get(7).expect("self entity");
        assert_eq!(e.speed, 150, "a moving player must report its speed");
        assert_eq!(e.pos, [200, 100, 10]);
        assert_eq!(e.heading, 512);

        // Stopping must return it to zero, or the avatar would run on the spot forever.
        w.move_self_to([200, 100, 10], 512, 0);
        assert_eq!(w.get(7).expect("self entity").speed, 0);
    }

    /// An NPC's size must survive into the world model — it drives render scale, and dropping it
    /// rendered giants at human height.
    #[test]
    fn npc_size_reaches_the_world() {
        let mut w = WorldState::default();
        let mut giant = match npc(1, 500, 500, "giant skeleton") {
            ServerEvent::NpcInView(n) => n,
            _ => unreachable!(),
        };
        giant.size = 180;
        w.apply(&ServerEvent::NpcInView(giant));
        assert_eq!(w.get(1).expect("npc").size, 180, "size must not be dropped");
    }

    /// The whole point of the slice: a departed entity must actually leave the world.
    #[test]
    fn object_delete_culls_the_entity() {
        let mut w = WorldState::default();
        w.apply(&npc(77, 5000, 5000, "a boar"));
        assert!(w.get(77).is_some());
        w.apply(&ServerEvent::ObjectRemoved { object_id: 77 });
        assert!(w.get(77).is_none(), "ObjectDelete did not cull the entity");
    }

    /// REDTEAM-B: peer 0x72 must not light the local cast HUD.
    #[test]
    fn self_cast_bar_ignores_peer_casters() {
        let mut w = WorldState::default();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 100,
            heading: 0,
        });
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 200,
                spell_id: 407,
                cast_time: 30,
            },
        ));
        assert!(
            w.cast_bar().is_some(),
            "world probe still sees any active cast"
        );
        assert!(
            w.self_cast_bar().is_none(),
            "HUD must stay dark for peer caster 200 while self is 100"
        );
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 100,
                spell_id: 407,
                cast_time: 30,
            },
        ));
        assert_eq!(
            w.self_cast_bar().map(|b| b.caster_id),
            Some(100),
            "self 0x72 must raise the HUD bar"
        );
    }

    /// Sol finding 7: ObjectDelete must scrub equipment (and other side maps) so a recycled
    /// object id cannot inherit the previous occupant's armour.
    #[test]
    fn object_delete_then_id_reuse_inherits_no_equipment() {
        let mut w = WorldState::default();
        w.apply(&npc(77, 5000, 5000, "armoured boar"));
        w.apply(&ServerEvent::EquipmentUpdated(
            caer_protocol::equipment::EquipmentUpdate {
                object_id: 77,
                items: vec![caer_protocol::equipment::VisibleItem {
                    slot: caer_protocol::equipment::slot::TORSO,
                    model: 401,
                    extension: Some(1),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ));
        assert!(
            w.equipment_of(77).is_some(),
            "equipment must latch before delete"
        );

        // A neighbour targeting 77 must lose that lock when 77 departs.
        w.apply(&npc(78, 5100, 5100, "hunter"));
        if let Some(i) = w.index.get(&78).copied() {
            w.target[i] = 77;
        }

        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 77,
                spell_id: 9,
                cast_time: 2000,
            },
        ));
        w.apply(&ServerEvent::SpellEffect(
            caer_protocol::spells::SpellEffectAnimation {
                caster_id: 77,
                target_id: 78,
                spell_id: 9,
                bolt_time: 0,
                no_sound: false,
                success: 1,
            },
        ));
        assert!(!w.particle_effects().is_empty());
        assert!(w.active_cast().is_none(), "SpellEffect completes cast bar");
        // Re-arm so ObjectRemoved clearing cast remains load-bearing.
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 77,
                spell_id: 9,
                cast_time: 2000,
            },
        ));
        assert!(w.active_cast().is_some());

        w.apply(&ServerEvent::ObjectRemoved { object_id: 77 });
        assert!(w.get(77).is_none());
        assert!(
            w.equipment_of(77).is_none(),
            "equipment must die with the object"
        );
        assert!(
            w.active_cast().is_none(),
            "caster delete clears active cast"
        );
        assert!(
            w.particle_effects()
                .iter()
                .all(|p| p.caster_id != 77 && p.target_id != 77),
            "particles keyed to deleted id must be scrubbed"
        );
        assert_eq!(
            w.get(78).expect("hunter").target_id,
            0,
            "stale target locks on deleted id must clear"
        );

        // Recycled id: a new occupant must start with no inherited components.
        w.apply(&npc(77, 6000, 6000, "fresh rat"));
        assert!(w.get(77).is_some());
        assert!(
            w.equipment_of(77).is_none(),
            "recycled object id must not inherit prior equipment"
        );
        assert_eq!(w.get(77).unwrap().name, "fresh rat");
    }

    /// System 4: DOL never sends LivingEquipmentUpdate 0x15 to the wearer. Worn inventory
    /// (0x02) must still populate equipment_of(Self_) so the avatar mesh binding can move.
    #[test]
    fn worn_inventory_projects_onto_self_equipment() {
        use caer_protocol::equipment::slot;
        use caer_protocol::inventory::{InventoryItem, InventoryUpdate, ItemData};

        let mut w = WorldState::default();
        w.apply(&ServerEvent::PlayerPosition {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            object_id: 95,
            heading: 0,
        });
        assert!(w.equipment_of(95).is_none());

        let sword = ItemData {
            unique_id: 1,
            level: 1,
            value1: 0,
            value2: 0,
            hand_byte: 0,
            object_type_byte: 3,
            unk_1112: 0,
            weight: 10,
            condition_pct: 100,
            durability_pct: 100,
            quality: 100,
            bonus: 0,
            bonus_level: 0,
            model: 3,
            extension: 0,
            color_or_emblem: 0,
            flag: 0,
            effect: 0,
            name: "bronze short sword".into(),
        };
        w.apply(&ServerEvent::InventoryUpdated(InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0,
            window_type: 0,
            items: vec![InventoryItem {
                slot: slot::RIGHTHAND,
                item: Some(sword),
            }],
        }));
        let eq = w.equipment_of(95).expect("self equipment from worn inv");
        assert_eq!(eq.item(slot::RIGHTHAND).map(|i| i.model), Some(3));
    }

    /// Falsifier: SpellEffect flood without drain must not grow past [`MAX_PARTICLE_EFFECTS`].
    /// Deletes the cap check in [`WorldState::push_particle`] ⇒ this fails (len == N >> cap).
    /// Refuse-push instead of drop-oldest ⇒ first retained spell_id stays 0 (also fails).
    #[test]
    fn particle_effects_queue_is_bounded() {
        let mut w = WorldState::default();
        let overflow = 64u16;
        let n = MAX_PARTICLE_EFFECTS as u16 + overflow;
        for i in 0..n {
            w.apply(&ServerEvent::SpellEffect(
                caer_protocol::spells::SpellEffectAnimation {
                    caster_id: 1,
                    target_id: 2,
                    spell_id: i,
                    bolt_time: 0,
                    no_sound: false,
                    success: 1,
                },
            ));
        }
        assert_eq!(
            w.particle_effects().len(),
            MAX_PARTICLE_EFFECTS,
            "undrained flood must stay ≤ MAX_PARTICLE_EFFECTS (got {})",
            w.particle_effects().len()
        );
        assert_eq!(
            w.particle_effects()[0].spell_id,
            overflow,
            "cap must drop oldest: first retained spell_id should be {overflow}"
        );
        assert_eq!(
            w.particle_effects()[MAX_PARTICLE_EFFECTS - 1].spell_id,
            n - 1,
            "newest spawn must remain at the tail"
        );
        let drained = w.drain_particle_effects();
        assert_eq!(drained.len(), MAX_PARTICLE_EFFECTS);
        assert!(w.particle_effects().is_empty());
    }

    #[test]
    fn region_changed_culls_world_and_latches_region() {
        let mut w = WorldState::default();
        w.apply(&npc(10, 1000, 1000, "old zone mob"));
        w.apply(&ServerEvent::EquipmentUpdated(
            caer_protocol::equipment::EquipmentUpdate {
                object_id: 10,
                items: vec![caer_protocol::equipment::VisibleItem {
                    slot: caer_protocol::equipment::slot::TORSO,
                    model: 1,
                    extension: Some(1),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ));
        assert_eq!(w.len(), 1);
        assert!(w.equipment_of(10).is_some());

        w.apply(&ServerEvent::RegionChanged(
            caer_protocol::region::RegionChanged {
                region_id: 51,
                zone_skin_id: 50,
                cause: 1,
                server_id: 0x0C,
            },
        ));
        assert_eq!(w.len(), 0, "region change must cull prior entities");
        assert!(w.equipment_of(10).is_none());
        assert_eq!(w.region_id, 51);
        assert!(w.merchant().is_none());
    }

    /// SCN-12: LoggedOut must scrub entities **and** player-local caches; recycled object ids
    /// must not inherit prior equipment. Observation that fails if clear_for_logout is deleted:
    /// inventory/money/equipment survive and id-reuse wears the previous torso.
    #[test]
    fn logged_out_clears_world_and_blocks_stale_equipment_reuse() {
        use caer_protocol::equipment::slot;
        use caer_protocol::inventory::{InventoryItem, InventoryUpdate, ItemData};
        use caer_protocol::money::MoneyUpdate;

        let mut w = WorldState::default();
        w.apply(&npc(77, 5000, 5000, "armoured boar"));
        w.apply(&ServerEvent::EquipmentUpdated(
            caer_protocol::equipment::EquipmentUpdate {
                object_id: 77,
                items: vec![caer_protocol::equipment::VisibleItem {
                    slot: slot::TORSO,
                    model: 401,
                    extension: Some(1),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ));
        w.apply(&ServerEvent::PlayerPosition {
            x: 1.0,
            y: 2.0,
            z: 3.0,
            object_id: 95,
            heading: 0,
        });
        w.apply(&ServerEvent::InventoryUpdated(InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0,
            window_type: 0,
            items: vec![InventoryItem {
                slot: slot::RIGHTHAND,
                item: Some(ItemData {
                    unique_id: 1,
                    level: 1,
                    value1: 0,
                    value2: 0,
                    hand_byte: 0,
                    object_type_byte: 3,
                    unk_1112: 0,
                    weight: 10,
                    condition_pct: 100,
                    durability_pct: 100,
                    quality: 100,
                    bonus: 0,
                    bonus_level: 0,
                    model: 3,
                    extension: 0,
                    color_or_emblem: 0,
                    flag: 0,
                    effect: 0,
                    name: "bronze short sword".into(),
                }),
            }],
        }));
        w.apply(&ServerEvent::MoneyUpdated(MoneyUpdate {
            copper: 9,
            silver: 8,
            gold: 7,
            mithril: 0,
            platinum: 0,
        }));
        assert!(w.len() >= 2);
        assert!(w.equipment_of(77).is_some());
        assert!(w.inventory().is_some());
        assert!(w.money().is_some());

        w.apply(&ServerEvent::LoggedOut {
            total_out: true,
            level: 50,
        });
        assert_eq!(w.len(), 0, "logout must cull all entities");
        assert!(w.equipment_of(77).is_none());
        assert!(w.inventory().is_none(), "logout must drop inventory cache");
        assert!(w.money().is_none(), "logout must drop money cache");
        assert_eq!(w.region_id, 0);

        // Reconnect rebuild: recycled oid must not wear prior armour until a fresh 0x15 arrives.
        w.apply(&npc(77, 6000, 6000, "new boar"));
        assert!(
            w.equipment_of(77).is_none(),
            "recycled object id must not inherit pre-logout equipment"
        );
    }

    #[test]
    fn region_handoff_clears_zone_transients() {
        let mut w = WorldState::default();
        w.apply(&ServerEvent::MerchantWindow(
            caer_protocol::merchant::MerchantWindow {
                window_type: 0,
                page: 0,
                items: vec![],
            },
        ));
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 1,
                spell_id: 2,
                cast_time: 1000,
            },
        ));
        w.apply(&ServerEvent::SpellEffect(
            caer_protocol::spells::SpellEffectAnimation {
                caster_id: 1,
                target_id: 2,
                spell_id: 2,
                bolt_time: 0,
                no_sound: false,
                success: 1,
            },
        ));
        assert!(w.merchant().is_some());
        assert!(
            w.active_cast().is_none(),
            "SpellEffect already completed cast bar"
        );
        assert!(!w.particle_effects().is_empty());
        // Leave an active cast so handoff clearing is still exercised.
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 1,
                spell_id: 2,
                cast_time: 1000,
            },
        ));
        assert!(w.active_cast().is_some());

        w.apply(&ServerEvent::RegionHandoff {
            ip: "127.0.0.1".into(),
            port: 10300,
        });
        assert!(w.merchant().is_none());
        assert!(w.active_cast().is_none());
        assert!(w.particle_effects().is_empty());
    }

    /// A status update must latch into `player_status`, and the pre-first-packet default must read
    /// as alive — an empty health bar on spawn would look like a death, not a missing packet.
    #[test]
    fn status_updates_latch_into_the_world() {
        let mut w = WorldState::default();
        assert!(
            (w.player_status.health_frac() - 1.0).abs() < 1e-6,
            "default must read as alive"
        );

        let s = PlayerStatus {
            health: 1848,
            max_health: 2004,
            health_pct: 92,
            ..PlayerStatus::default()
        };
        w.apply(&ServerEvent::StatusUpdate(s));
        assert_eq!(w.player_status.health, 1848);
        assert!((w.player_status.health_frac() - 0.922).abs() < 0.01);

        // A later update replaces rather than merges — vitals are a snapshot, not a delta.
        let mut hurt = s;
        hurt.health = 100;
        w.apply(&ServerEvent::StatusUpdate(hurt));
        assert_eq!(w.player_status.health, 100);
    }

    fn npc(id: u16, x: i32, y: i32, name: &str) -> ServerEvent {
        ServerEvent::NpcInView(Npc {
            object_id: id,
            speed: 0,
            heading: 0,
            x: x as u32,
            y: y as u32,
            z: 2400,
            model: 1,
            size: 50,
            level: 50,
            flags: 0,
            name: name.into(),
            guild: String::new(),
        })
    }

    #[test]
    fn create_then_update_moves_the_entity() {
        let mut w = WorldState::new();
        w.apply(&npc(12846, 561375, 509674, "Lundeg Tranyth"));
        assert_eq!(w.len(), 1);
        assert_eq!(w.get(12846).unwrap().pos, [561375, 509674, 2400]);

        // move it via a zone-local ObjectUpdate (zone offset 67/59)
        w.apply(&ServerEvent::EntityUpdated(EntityUpdate {
            object_id: 12846,
            speed: 100,
            heading: 0x0400,
            local_x: 13000,
            local_y: 27000,
            z: 2445,
            target_id: 0,
            health_pct: 90,
            flags: 0,
            zone: 0,
        }));
        let e = w.get(12846).unwrap();
        assert_eq!(e.pos, [67 * 8192 + 13000, 59 * 8192 + 27000, 2445]);
        assert_eq!(e.health_pct, 90);
        assert_eq!(e.speed, 100);
        assert_eq!(w.len(), 1, "update must not create a second entity");
    }

    #[test]
    fn update_before_create_spawns_placeholder_then_upgrades() {
        let mut w = WorldState::new();
        // update first: an Unknown placeholder appears at the update's position
        w.apply(&ServerEvent::EntityUpdated(EntityUpdate {
            object_id: 12846,
            speed: 0,
            heading: 0,
            local_x: 12511,
            local_y: 26346,
            z: 2445,
            target_id: 0,
            health_pct: 100,
            flags: 0,
            zone: 0,
        }));
        assert_eq!(w.len(), 1);
        assert_eq!(w.updates_before_create, 1);
        assert_eq!(w.unresolved(), 1);
        assert_eq!(w.get(12846).unwrap().kind, Kind::Unknown);

        // the create lands: same id upgrades in place, no duplicate
        w.apply(&npc(12846, 561375, 509674, "Lundeg Tranyth"));
        assert_eq!(w.len(), 1);
        assert_eq!(w.unresolved(), 0);
        assert_eq!(w.get(12846).unwrap().name, "Lundeg Tranyth");
    }

    #[test]
    fn nearest_and_radius_queries() {
        let mut w = WorldState::new();
        w.apply(&npc(1, 1000, 1000, "near"));
        w.apply(&npc(2, 5000, 5000, "mid"));
        w.apply(&npc(3, 50000, 50000, "far"));
        let n = w.nearest([1100, 1100], 2);
        assert_eq!(n[0].0.object_id, 1);
        assert_eq!(n.len(), 2);
        assert_eq!(w.count_within([1000, 1000], 6000), 2); // ids 1 and 2
    }

    #[test]
    fn upsert_replaces_not_duplicates() {
        let mut w = WorldState::new();
        w.apply(&npc(1, 0, 0, "first"));
        w.apply(&npc(1, 100, 100, "renamed"));
        assert_eq!(w.len(), 1);
        assert_eq!(w.get(1).unwrap().name, "renamed");
    }

    #[test]
    fn indexed_queries_agree_with_naive() {
        let mut w = WorldState::new();
        for i in 0..300u16 {
            let x = 500_000 + (i as i32 * 733).rem_euclid(40_000);
            let y = 500_000 + (i as i32 * 991).rem_euclid(40_000);
            w.apply(&npc(i + 1, x, y, "e"));
        }
        // move a bunch, so the grid's incremental update path is exercised
        for i in 0..150u16 {
            w.apply(&ServerEvent::EntityUpdated(EntityUpdate {
                object_id: i + 1,
                speed: 50,
                heading: 0,
                local_x: (i * 200) % 60_000,
                local_y: (i * 137) % 60_000,
                z: 2400,
                target_id: 0,
                health_pct: 100,
                flags: 0,
                zone: 0,
            }));
        }
        let q = [520_000, 520_000];
        assert_eq!(w.count_within_indexed(q, 8000), w.count_within(q, 8000));
        let naive: Vec<u16> = w.nearest(q, 10).iter().map(|(e, _)| e.object_id).collect();
        let indexed: Vec<u16> = w
            .nearest_indexed(q, 10)
            .iter()
            .map(|(e, _)| e.object_id)
            .collect();
        assert_eq!(indexed, naive, "grid nearest must match naive nearest");
    }

    #[test]
    fn render_set_parallel_equals_serial() {
        let mut w = WorldState::new();
        for i in 0..1000u16 {
            let x = 500_000 + (i as i32 * 617).rem_euclid(80_000);
            let y = 500_000 + (i as i32 * 431).rem_euclid(80_000);
            let mut ev = npc(i + 1, x, y, "e");
            if let ServerEvent::NpcInView(n) = &mut ev {
                n.heading = (i * 7) % 4096;
                n.speed = i % 200;
            }
            w.apply(&ev);
        }
        let cam = [520_000, 520_000];
        let mut a = w.render_set(cam, 30_000, 100);
        let mut b = w.render_set_par(cam, 30_000, 100);
        a.sort_unstable_by_key(|r| r.object_id);
        b.sort_unstable_by_key(|r| r.object_id);
        assert_eq!(a, b, "parallel render set must equal serial");
        assert!(!a.is_empty() && a.len() < w.len(), "some culled, some kept");
    }

    #[test]
    fn columns_stay_aligned_after_moves() {
        // After a mix of creates and in-place updates, every column must still line up: a view
        // fetched by id must carry that entity's own name and current position.
        let mut w = WorldState::new();
        w.apply(&npc(10, 1000, 1000, "ten"));
        w.apply(&npc(20, 2000, 2000, "twenty"));
        w.apply(&npc(30, 3000, 3000, "thirty"));
        w.apply(&ServerEvent::EntityUpdated(EntityUpdate {
            object_id: 20,
            speed: 0,
            heading: 0,
            local_x: 100,
            local_y: 200,
            z: 50,
            target_id: 0,
            health_pct: 77,
            flags: 0,
            zone: 0,
        }));
        assert_eq!(w.get(20).unwrap().name, "twenty");
        assert_eq!(w.get(20).unwrap().health_pct, 77);
        assert_eq!(w.get(10).unwrap().name, "ten");
        assert_eq!(w.get(30).unwrap().name, "thirty");
        assert_eq!(w.iter().count(), 3);
    }

    #[test]
    fn static_object_ingest() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::ObjectInView(StaticObject {
            object_id: 42,
            emblem: 0,
            heading: 0,
            x: 560000,
            y: 510000,
            z: 2400,
            model: 100,
            name: "Forge".into(),
        }));
        assert_eq!(w.get(42).unwrap().kind, Kind::StaticObject);
    }

    /// Named falsifier `peer_cast_cannot_drive_local_cast_bar` (CastPresentation).
    #[test]
    fn peer_cast_cannot_drive_local_cast_bar() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 10,
            heading: 0,
        });
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 99,
                spell_id: 1,
                cast_time: 20,
            },
        ));
        let p = w.cast_presentation();
        assert!(p.self_bar.is_none());
        assert_eq!(p.other_bar.map(|b| b.caster_id), Some(99));
        assert!(w.self_cast_bar().is_none());
    }

    /// Named falsifier: a peer 0x72 must not erase the local player's in-flight bar.
    #[test]
    fn peer_cast_does_not_erase_local_cast_bar() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 10,
            heading: 0,
        });
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 10,
                spell_id: 7,
                cast_time: 40,
            },
        ));
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 99,
                spell_id: 1,
                cast_time: 20,
            },
        ));
        assert_eq!(w.self_cast_bar().map(|b| b.spell_id), Some(7));
        assert_eq!(w.cast_for(10).map(|c| c.spell_id), Some(7));
        assert_eq!(w.cast_for(99).map(|c| c.spell_id), Some(1));
        w.apply(&ServerEvent::SpellEffect(
            caer_protocol::spells::SpellEffectAnimation {
                caster_id: 99,
                target_id: 1,
                spell_id: 1,
                bolt_time: 0,
                no_sound: false,
                success: 1,
            },
        ));
        assert_eq!(
            w.self_cast_bar().map(|b| b.spell_id),
            Some(7),
            "peer effect must not complete the local bar"
        );
        assert!(w.last_cast_outcome_for(99).is_some());
        assert!(w.last_cast_outcome_for(10).is_none());
    }

    /// Named falsifier: peer start/interrupt/complete must never clear `cast_for(self_id)`.
    /// A single global slot would fail this.
    #[test]
    fn peer_start_interrupt_complete_never_clears_self_cast() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 10,
            heading: 0,
        });
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 10,
                spell_id: 7,
                cast_time: 40,
            },
        ));
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 99,
                spell_id: 1,
                cast_time: 20,
            },
        ));
        assert_eq!(w.cast_for(10).map(|c| c.spell_id), Some(7));
        w.apply(&ServerEvent::SpellInterrupted(
            caer_protocol::spells::InterruptSpellCast { object_id: 99 },
        ));
        assert_eq!(
            w.cast_for(10).map(|c| c.spell_id),
            Some(7),
            "peer interrupt must not clear self"
        );
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 99,
                spell_id: 2,
                cast_time: 10,
            },
        ));
        w.apply(&ServerEvent::SpellEffect(
            caer_protocol::spells::SpellEffectAnimation {
                caster_id: 99,
                target_id: 1,
                spell_id: 2,
                bolt_time: 0,
                no_sound: false,
                success: 1,
            },
        ));
        assert_eq!(
            w.cast_for(10).map(|c| c.spell_id),
            Some(7),
            "peer complete must not clear self"
        );
        assert!(w.cast_for(99).is_none());
    }

    #[test]
    fn on_region_changed_clears_transient_combat_cast_effects() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 10,
            heading: 0,
        });
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 10,
                spell_id: 7,
                cast_time: 40,
            },
        ));
        w.apply(&ServerEvent::SpellEffect(
            caer_protocol::spells::SpellEffectAnimation {
                caster_id: 99,
                target_id: 1,
                spell_id: 1,
                bolt_time: 0,
                no_sound: false,
                success: 1,
            },
        ));
        w.apply(&ServerEvent::CombatAnimation(
            caer_protocol::combat_anim::CombatAnimation {
                attacker_id: 1,
                defender_id: 2,
                weapon_id: 0,
                shield_id: 0,
                style: 0,
                stance: 0,
                result: caer_protocol::combat_anim::CombatResult::HitUnstyled,
                target_health_pct: 80,
                unk: 0,
            },
        ));
        w.apply(&ServerEvent::PetWindow(caer_protocol::pets::PetWindow {
            pet_id: 44,
            action: caer_protocol::pets::PetWindowAction::Open,
            aggro: caer_protocol::pets::PetAggro::Passive,
            walk: caer_protocol::pets::PetWalk::Follow,
            icons: vec![],
        }));
        assert!(w.cast_for(10).is_some());
        assert!(!w.particle_effects().is_empty());
        assert!(!w.combat_results().is_empty());
        assert!(w.owned_pet().is_some());
        w.on_region_changed(51);
        assert!(w.cast_for(10).is_none());
        assert!(w.particle_effects().is_empty());
        assert!(w.combat_results().is_empty());
        assert!(w.sound_requests().is_empty());
        assert!(w.owned_pet().is_none());
        assert_eq!(w.region_id, 51);
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 10,
                spell_id: 9,
                cast_time: 10,
            },
        ));
        w.on_logout();
        assert!(w.cast_for(10).is_none());
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 10,
                spell_id: 9,
                cast_time: 10,
            },
        ));
        w.on_reconnect();
        assert!(w.cast_for(10).is_none());
        assert!(w.self_object_id().is_none());
    }

    #[test]
    fn pet_window_owns_single_oid_and_close_releases() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PetWindow(caer_protocol::pets::PetWindow {
            pet_id: 77,
            action: caer_protocol::pets::PetWindowAction::Open,
            aggro: caer_protocol::pets::PetAggro::Aggressive,
            walk: caer_protocol::pets::PetWalk::Stay,
            icons: vec![0x11],
        }));
        assert_eq!(w.owned_pet().map(|p| p.pet_id), Some(77));
        assert_eq!(w.combat_presentation().pet.map(|p| p.pet_id), Some(77));
        w.apply(&ServerEvent::ObjectRemoved { object_id: 77 });
        assert!(w.owned_pet().is_none());
        assert_eq!(NECRO_BODY_RULES, PetRemainder::Unknown);
        assert_eq!(MULTI_PET_RULES, PetRemainder::Unknown);
    }

    #[test]
    fn cast_outcome_expires_on_cadence_ticks() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 1,
                spell_id: 9,
                cast_time: 10,
            },
        ));
        w.apply(&ServerEvent::SpellEffect(
            caer_protocol::spells::SpellEffectAnimation {
                caster_id: 1,
                spell_id: 9,
                target_id: 2,
                bolt_time: 0,
                no_sound: true,
                success: 1,
            },
        ));
        assert!(w.last_cast_outcome().is_some());
        w.advance_cadence(89);
        assert!(w.last_cast_outcome().is_some());
        w.advance_cadence(2);
        assert!(
            w.last_cast_outcome().is_none(),
            "stale combat presentation must TTL"
        );
    }

    /// Named falsifier `interrupted_cast_leaves_no_completed_particle_or_land_sound`.
    #[test]
    fn interrupted_cast_leaves_no_completed_particle_or_land_sound() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 7,
            heading: 0,
        });
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 7,
                spell_id: 407,
                cast_time: 30,
            },
        ));
        w.advance_cadence(5);
        w.apply(&ServerEvent::SpellInterrupted(
            caer_protocol::spells::InterruptSpellCast { object_id: 7 },
        ));
        assert!(w.self_cast_bar().is_none());
        assert!(!w.last_cast_outcome().unwrap().is_completed());
        assert!(w.particle_effects().is_empty());
        assert!(
            w.sound_requests().is_empty(),
            "interrupt must scrub CastWindup and EffectLand"
        );
        let gap = w
            .cadence()
            .gap_after(
                |m| matches!(m, CadenceMark::CastStart { .. }),
                |m| matches!(m, CadenceMark::Interrupt { .. }),
            )
            .expect("cadence");
        assert_eq!(gap, 5);
    }

    /// Named falsifier `failed_effect_leaves_no_particle_or_land_sound`.
    #[test]
    fn failed_effect_leaves_no_particle_or_land_sound() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 1,
                spell_id: 9,
                cast_time: 10,
            },
        ));
        w.apply(&ServerEvent::SpellEffect(
            caer_protocol::spells::SpellEffectAnimation {
                caster_id: 1,
                spell_id: 9,
                target_id: 2,
                bolt_time: 4,
                no_sound: false,
                success: 0,
            },
        ));
        assert!(w.active_cast().is_none());
        assert!(matches!(
            w.last_cast_outcome(),
            Some(CastOutcome::Failed { spell_id: 9, .. })
        ));
        assert!(w.particle_effects().is_empty());
        assert!(w
            .sound_requests()
            .iter()
            .all(|s| s.kind != SoundKind::EffectLand));
    }

    /// Named falsifier `combat_chat_cannot_pass_numeric_status`.
    #[test]
    fn combat_chat_cannot_pass_numeric_status() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::ChatMessage {
            chat_type: 0x11,
            text: "You attack X with your sword and hit for 125 (+129) damage!".into(),
        });
        assert!(
            w.combat_results().is_empty(),
            "Message 0xAF must not create CombatAnimation status"
        );
        w.apply(&ServerEvent::CombatAnimation(
            caer_protocol::combat_anim::CombatAnimation {
                attacker_id: 1,
                defender_id: 2,
                weapon_id: 0,
                shield_id: 0,
                style: 0,
                stance: 0,
                result: caer_protocol::combat_anim::CombatResult::HitUnstyled,
                target_health_pct: 80,
                unk: 0,
            },
        ));
        assert_eq!(w.combat_results().len(), 1);
        assert_eq!(w.combat_results()[0].provenance, "CombatAnimation 0xBC");
        assert_eq!(w.combat_results()[0].target_health_pct, 80);
    }

    /// Named falsifier `no_effect_control_leaves_presentation_empty`.
    #[test]
    fn no_effect_control_leaves_presentation_empty() {
        let w = WorldState::new();
        let p = w.combat_presentation();
        assert!(p.cast.self_bar.is_none());
        assert!(p.cast.outcome.is_none());
        assert!(p.combat_results.is_empty());
        assert!(p.pending_effects.is_empty());
        assert!(p.pending_sounds.is_empty());
        assert!(p.icons.is_empty());
        assert!(p.points.is_none());
    }

    #[test]
    fn success_effect_queues_presence_placeholder_not_fidelity() {
        let mut w = WorldState::new();
        w.apply(&ServerEvent::SpellEffect(
            caer_protocol::spells::SpellEffectAnimation {
                caster_id: 1,
                spell_id: 407,
                target_id: 2,
                bolt_time: 3,
                no_sound: false,
                success: 1,
            },
        ));
        assert_eq!(w.particle_effects().len(), 1);
        assert_eq!(
            w.particle_effects()[0].resource,
            EffectResource::PresencePlaceholder
        );
        assert_eq!(w.particle_effects()[0].bolt_time, 3);
        assert!(matches!(
            w.last_cast_outcome(),
            Some(CastOutcome::Completed { spell_id: 407, .. })
        ));
    }

    #[test]
    fn icons_and_points_are_packet_owned_and_logout_clears() {
        let mut w = WorldState::new();
        let icons = caer_protocol::effects::UpdateIcons {
            icons_flag: 0,
            slots: vec![caer_protocol::effects::IconSlot::Live(
                caer_protocol::effects::IconEntry {
                    index: 0,
                    list_index: 0,
                    immun: 0,
                    icon: 0x22,
                    remaining_secs: 8,
                    spell_internal_id: 1,
                    negative: true,
                    name: "mez".into(),
                },
            )],
        };
        w.apply(&ServerEvent::UpdateIcons(icons));
        w.apply(&ServerEvent::CharacterPoints(
            caer_protocol::points::CharacterPoints {
                realm_points: 10,
                level_permill: 250,
                skill_specialty_points: 0,
                bounty_points: 4,
                realm_specialty_points: 0,
                champion_level_permill: 0,
                experience: Some(100),
                experience_for_next_level: Some(200),
            },
        ));
        assert_eq!(w.icons().len(), 1);
        assert!(w.icons()[0].is_debuff_or_cc());
        assert_eq!(w.character_points().unwrap().level_permill, 250);
        w.apply(&ServerEvent::LoggedOut {
            total_out: true,
            level: 1,
        });
        assert!(w.icons().is_empty());
        assert!(w.character_points().is_none());
        assert!(w.sound_requests().is_empty());
    }

    #[test]
    fn death_clears_victim_cast_and_con_needs_levels() {
        let mut w = WorldState::new();
        w.apply(&npc(42, 1000, 1000, "victim"));
        w.apply(&ServerEvent::CharacterSheet(
            caer_protocol::charsheet::CharacterSheet {
                level: 50,
                ..Default::default()
            },
        ));
        assert_eq!(w.con_of(42), Some(ConBand::from_levels(50, 50)));
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 42,
                spell_id: 2,
                cast_time: 10,
            },
        ));
        w.apply(&ServerEvent::PlayerDied(
            caer_protocol::death::PlayerDeath {
                object_id: 42,
                killer_id: 0,
            },
        ));
        assert!(w.active_cast().is_none());
        assert!(w.is_dead(42));
    }

    /// Named falsifier: PlayerCreate for the local oid must not demote Kind::Self_ into the
    /// other-player map. Delete the `self_object_id` early return in `apply_player` → this fails.
    #[test]
    fn player_create_for_self_does_not_demote_into_other_player_map() {
        use caer_protocol::entities::Player;
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 10.0,
            y: 20.0,
            z: 30.0,
            object_id: 7,
            heading: 0,
        });
        assert_eq!(w.get(7).map(|e| e.kind), Some(Kind::Self_));
        w.apply_player_create(&Player {
            object_id: 7,
            session_id: 7,
            x: 10.0,
            y: 20.0,
            z: 30.0,
            heading: 0,
            model_unverified: 32,
            level: 50,
            realm: 1,
            flags: 0,
            name: "Self".into(),
            guild: String::new(),
            last_name: String::new(),
            custom: caer_protocol::customization::Customization::default(),
            eye_size: 0,
            lip_size: 0,
        });
        assert_eq!(w.get(7).map(|e| e.kind), Some(Kind::Self_));
        assert!(
            w.other_player_avatar(7).is_none(),
            "local oid must not enter player_avatar"
        );
    }

    /// Vault/consignment 0x02 must not become the player bag/worn map (bank is eco.bank).
    #[test]
    fn vault_inventory_update_does_not_populate_player_inventory() {
        use caer_protocol::inventory::{window_type, InventoryItem, InventoryUpdate, ItemData};
        let mut w = WorldState::new();
        let item = ItemData {
            unique_id: 1,
            level: 1,
            value1: 0,
            value2: 0,
            hand_byte: 0,
            object_type_byte: 3,
            unk_1112: 0,
            weight: 10,
            condition_pct: 100,
            durability_pct: 100,
            quality: 100,
            bonus: 0,
            bonus_level: 0,
            model: 3,
            extension: 0,
            color_or_emblem: 0,
            flag: 0,
            effect: 0,
            name: "vault sword".into(),
        };
        w.apply(&ServerEvent::InventoryUpdated(InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0,
            window_type: window_type::PLAYER_VAULT,
            items: vec![InventoryItem {
                slot: 40,
                item: Some(item),
            }],
        }));
        assert!(
            w.inventory_item(40).is_none(),
            "vault page must not write player inventory()"
        );
        assert!(w.eco().inventory.item(40).is_none());
        assert_eq!(
            w.eco().bank.vault_item(40).map(|i| i.name.as_str()),
            Some("vault sword")
        );
    }

    /// ConsignmentMerchantMoney 0x1E arrives as ServerEvent::Raw until ABI grows a discriminant.
    #[test]
    fn raw_consignment_money_folds_via_eco_apply_s2c() {
        use caer_protocol::codes;
        use caer_protocol::money::{encode_consignment, ConsignmentMerchantMoney};
        let mut w = WorldState::new();
        let payload = encode_consignment(&ConsignmentMerchantMoney {
            copper: 1,
            silver: 2,
            gold: 7,
            mithril: 0,
            platinum: 0,
        });
        w.apply(&ServerEvent::Raw {
            code: codes::server::ConsignmentMerchantMoney,
            payload,
        });
        assert_eq!(w.eco().bank.consignment().map(|m| m.gold), Some(7));
        assert!(
            w.money().is_none(),
            "0x1E must not become player 0xFA purse"
        );
    }

    #[test]
    fn raw_encumberance_folds_via_eco_apply_s2c() {
        use caer_protocol::codes;
        use caer_protocol::encumberance::{encode, Encumberance};
        let mut w = WorldState::new();
        w.apply(&ServerEvent::Raw {
            code: codes::server::Encumberance,
            payload: encode(&Encumberance { max: 200, used: 80 }),
        });
        assert_eq!(w.eco().encumberance.current().map(|e| e.used), Some(80));
    }

    #[test]
    fn raw_emote_is_presentation_not_typed_particle() {
        use caer_protocol::codes;
        use caer_protocol::emote::{encode, EmoteAnimation};
        let mut w = WorldState::new();
        w.apply(&ServerEvent::Raw {
            code: codes::server::EmoteAnimation,
            payload: encode(&EmoteAnimation {
                object_id: 12,
                emote: 7,
            }),
        });
        assert_eq!(w.last_emote().map(|e| e.emote), Some(7));
        assert!(
            w.particle_effects().is_empty(),
            "emote must not enqueue spell-effect particles"
        );
    }

    #[test]
    fn raw_siege_interface_close_clears_open() {
        use caer_protocol::codes;
        use caer_protocol::siege::{
            encode_animation_prefix, encode_interface_close, SiegeWeaponAnimation,
        };
        let mut w = WorldState::new();
        w.apply(&ServerEvent::Raw {
            code: codes::server::SiegeWeaponAnimation,
            payload: encode_animation_prefix(&SiegeWeaponAnimation {
                object_id: 9,
                aim_x: 1,
                aim_y: 2,
                aim_z: 3,
                target_oid: 0,
                effect: 1,
                timer: 2,
                action: 2,
            }),
        });
        assert_eq!(w.last_siege_anim().map(|a| a.object_id), Some(9));
        w.apply(&ServerEvent::Raw {
            code: codes::server::SiegeWeaponInterface,
            payload: encode_interface_close(),
        });
        assert!(!w.siege_interface_open());
    }

    #[test]
    fn region_handoff_clears_merchant_but_keeps_inventory() {
        use caer_protocol::inventory::{InventoryItem, InventoryUpdate, ItemData};
        use caer_protocol::merchant::{MerchantOffer, MerchantWindow};
        let mut w = WorldState::new();
        let item = ItemData {
            unique_id: 1,
            level: 1,
            value1: 0,
            value2: 0,
            hand_byte: 0,
            object_type_byte: 3,
            unk_1112: 0,
            weight: 1,
            condition_pct: 100,
            durability_pct: 100,
            quality: 100,
            bonus: 0,
            bonus_level: 0,
            model: 1,
            extension: 0,
            color_or_emblem: 0,
            flag: 0,
            effect: 0,
            name: "bag".into(),
        };
        w.apply(&ServerEvent::InventoryUpdated(InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0,
            window_type: 2,
            items: vec![InventoryItem {
                slot: 40,
                item: Some(item),
            }],
        }));
        w.apply(&ServerEvent::MerchantWindow(MerchantWindow {
            items: vec![MerchantOffer {
                slot: 0,
                level: 1,
                value1: 0,
                spd_abs: 0,
                hand_byte: 0,
                object_type_byte: 0,
                usable: true,
                value2: 0,
                price: 1,
                model: 1,
                name: "wares".into(),
            }],
            window_type: 0,
            page: 0,
        }));
        assert!(w.inventory_item(40).is_some());
        assert!(w.merchant().is_some());
        w.apply(&ServerEvent::RegionHandoff {
            ip: "127.0.0.1".into(),
            port: 10312,
        });
        assert!(w.inventory_item(40).is_some(), "handoff keeps bag");
        assert!(w.merchant().is_none(), "handoff drops merchant page");
        assert!(w.eco().merchant.catalogue().is_none());
    }

    #[test]
    fn typed_encumberance_and_emote_fold_without_raw() {
        use caer_protocol::emote::EmoteAnimation;
        use caer_protocol::encumberance::Encumberance;
        let mut w = WorldState::new();
        w.apply(&ServerEvent::Encumberance(Encumberance {
            max: 200,
            used: 80,
        }));
        assert_eq!(w.eco().encumberance.current().map(|e| e.used), Some(80));
        w.apply(&ServerEvent::EmoteAnimation(EmoteAnimation {
            object_id: 12,
            emote: 7,
        }));
        assert_eq!(w.last_emote().map(|e| e.emote), Some(7));
        assert!(w.particle_effects().is_empty());
    }

    #[test]
    fn typed_siege_interface_close_clears_open() {
        use caer_protocol::siege::{SiegeWeaponAnimation, SiegeWeaponInterface};
        let mut w = WorldState::new();
        w.apply(&ServerEvent::SiegeWeaponAnimation(SiegeWeaponAnimation {
            object_id: 9,
            aim_x: 0,
            aim_y: 0,
            aim_z: 0,
            target_oid: 0,
            effect: 0,
            timer: 0,
            action: 1,
        }));
        assert_eq!(w.last_siege_anim().map(|a| a.object_id), Some(9));
        w.apply(&ServerEvent::SiegeWeaponInterface(SiegeWeaponInterface {
            flag: 0,
            close: 0,
        }));
        assert!(w.siege_interface_open());
        w.apply(&ServerEvent::SiegeWeaponInterface(SiegeWeaponInterface {
            flag: 0,
            close: 1,
        }));
        assert!(!w.siege_interface_open());
    }

    #[test]
    fn typed_market_trainer_findgroup_emblem_fold_eco() {
        use caer_protocol::emblem::EmblemDialogue;
        use caer_protocol::findgroup::FindGroupUpdate;
        use caer_protocol::market::MarketExplorer;
        use caer_protocol::money::ConsignmentMerchantMoney;
        use caer_protocol::social::ObjectGuildId;
        let mut w = WorldState::new();
        w.apply(&ServerEvent::MarketExplorer(MarketExplorer {
            count: 255,
            page: 0,
            max_page: 0,
        }));
        assert!(w
            .eco()
            .market_explorer
            .current()
            .is_some_and(|m| m.is_empty_close()));
        w.apply(&ServerEvent::FindGroupUpdate(FindGroupUpdate {
            count: 0,
            empty_list: true,
        }));
        assert!(w.eco().find_group.is_some_and(|f| f.empty_list));
        w.apply(&ServerEvent::EmblemDialogue(EmblemDialogue));
        assert!(w.eco().emblem_dialogue);
        w.apply(&ServerEvent::ConsignmentMerchantMoney(
            ConsignmentMerchantMoney {
                copper: 1,
                silver: 0,
                gold: 0,
                mithril: 0,
                platinum: 0,
            },
        ));
        assert!(w.eco().bank.consignment().is_some());
        w.apply(&ServerEvent::ObjectGuildId(ObjectGuildId {
            object_id: 5,
            guild_id: 9,
        }));
        assert_eq!(w.eco().bank.guild_id(5), Some(9));
    }

    #[test]
    fn state_apply_registry_matches_world_and_eco_arms() {
        let world = include_str!("lib.rs");
        let start = world
            .find("pub fn apply(&mut self, ev: &ServerEvent)")
            .expect("WorldState::apply");
        let apply = &world[start..];
        let end = apply.find("\n    fn apply_npc").unwrap_or(apply.len());
        let apply = &apply[..end];
        let eco = include_str!("eco/mod.rs");
        let eco_start = eco
            .find("pub fn apply(&mut self, ev: &ServerEvent)")
            .expect("EcoState::apply");
        let eco_apply = &eco[eco_start..];
        for name in caer_protocol::coverage::STATE_APPLY_EVENTS {
            let needle = format!("ServerEvent::{name}");
            assert!(
                apply.contains(&needle) || eco_apply.contains(&needle),
                "{name} is STATE_APPLY but has no WorldState/EcoState match arm"
            );
        }
    }
}
