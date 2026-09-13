//! `caer-render` as a library — the shared rendering core, used by two shells:
//!
//! * the **`caer-render` binary** — the developer/inspection lens: free-fly camera, model editor,
//!   terrain brush, static-dump loading, screenshots. It sits a level *above* the game.
//! * the **`rustdaoc` binary** — the player-level client: a live server connection, a
//!   player-anchored camera, no editor chrome. This is the thing that becomes the drop-in client.
//!
//! Both draw the same world through the same GPU pipeline; only the camera, controls, and chrome
//! differ. This lib holds everything they share (the modules below + the scene helpers here) so
//! there is one renderer, not two. Hard rule preserved: these modules depend on the core crates
//! (`caer-world`/`caer-protocol`/`caer-assets`/`caer-client`), never the reverse.

pub mod adapters;
pub mod addon_product;
pub mod anim_skin;
pub mod atmosphere;
pub mod audio;
pub mod audio_bus;
pub mod camera;
pub mod capture_manifest;
pub mod cfx_adapters;
pub mod chat;
pub mod combat;
pub mod death;
pub mod display_settings;
pub mod dungeon_census;
pub mod dungeon_mesh;
pub mod entities;
pub mod event_audio;
pub mod gpu;
pub mod gpu_init;
pub mod harness;
pub mod harness_ingress;
pub mod host_clock;
pub mod hud;
pub mod init_lock;
pub mod jitter;
pub mod keybinds;
pub mod lifecycle;
pub mod live;
pub mod motion;
pub mod nameplates;
pub mod other_avatars;
pub mod particles;
pub mod preworld;
pub mod preworld_appearance;
pub mod preworld_camera;
pub mod preworld_camera_tune;
pub mod preworld_customize;
pub mod preworld_flow;
pub mod preworld_head_tune;
pub mod preworld_hitbox;
pub mod preworld_options;
pub mod preworld_product;
pub mod preworld_scene;
pub mod product_input;
pub mod product_loop;
pub mod product_ui;
pub mod quickbar;
pub mod rig_family;
pub mod scene_census;
pub mod shell;
pub mod skinhud;
pub mod skinui;
pub mod social_ui;
pub mod terrain;
pub mod ui;
pub mod uv_coverage;
pub mod walkable;

pub use audio_bus::{
    ActiveLoop, AudioBus, AudioDeviceKind, AudioSettings, BusCall, LogicalSoundEvent,
    NullAudioDevice, PlayOutcome, SoundCategory, SpatialParams, BUS_CALL_LOG_CAP, ONESHOT_SINK_CAP,
};
pub use display_settings::{
    apply as apply_display_settings, enumerate_modes, sanitize_mode, ApplyResult, DisplayMode,
    DisplayPanelAction, DisplaySettings, FullscreenIntent, WindowIntent, WindowMode,
    WINDOWED_FALLBACK,
};

use std::collections::{HashMap, HashSet};

use glam::Vec3;

use caer_protocol::session::ServerEvent;
use caer_world::{
    region_zone_offsets, world_data::load_mobs_tsv, Kind, OtherPlayerAvatar, WorldState, ZONE_UNIT,
};

use entities::EntityModels;
use gpu::{Gpu, Instance};

/// Radius (world units) of the near slice the world model culls to each frame.
pub const CULL_RADIUS: i32 = 50_000;
/// Placeholder-box half-extent in world units. Chunky enough to read from a high vantage.
pub const BOX_SIZE: f32 = 110.0;
/// Uniform scale applied to a live-entity NIF mesh. Monster NIF units ARE world units (like the
/// fixtures), so 1.0 renders at authored size; tune once against real captures if needed.
pub const ENTITY_SCALE: f32 = 1.0;

/// Convert the server's entity size byte into a render scale multiplier.
///
/// Size is a PERCENTAGE of normal humanoid scale (`caer_world::NORMAL_SIZE` = 50, the oracle's
/// `GameNPC` default). Measured across the captures: ambient rat 4–10, brownie 40–45, human NPCs
/// 48–57, **giant skeleton 151–199**, teleport effect 254.
///
/// Size 0 means "unspecified" rather than "infinitely small" — several capture rows carry it — so
/// it maps to normal scale, and the result is clamped so a malformed byte cannot make an entity
/// vanish or swallow the screen.
#[must_use]
pub fn entity_size_scale(size: u8) -> f32 {
    if size == 0 {
        return 1.0;
    }
    (f32::from(size) / f32::from(caer_world::NORMAL_SIZE)).clamp(0.05, 6.0)
}
/// How far an entity's own z may sit from our decoded ground before we STOP snapping it down.
///
/// Measured over region 1's 16,419 spawns (`mobground` bin): authored z minus our surface height is
/// a median of 0, with p25/p75 at ±1 unit and 85% inside ±16 — our terrain decode and the server
/// agree closely, so the packet z is trustworthy, not something to be overridden.
///
/// Snapping every entity unconditionally therefore fixed a problem that barely existed while
/// destroying the cases where an off-surface z is DELIBERATE: 576 spawns (3.5%) sit more than 256
/// units above the ground — up to 2180 — which is what a flying mob, a guard on a keep wall or an
/// upper floor, and a bridge spawn all look like. Another 130 sit far below, in cellars and caves.
/// Those were all being yanked to the dirt.
///
/// So snap only where the packet already agrees with our surface, which is exactly the case the
/// snap was introduced for (killing sub-unit float/sink and the old lattice stepping), and leave
/// anything deliberately off-surface where the server put it.
pub const MAX_GROUND_SNAP: f32 = 64.0;

/// Where to draw an entity vertically: its own z, corrected onto our terrain only when the two
/// already agree to within [`MAX_GROUND_SNAP`].
///
/// `anchor` is the mesh's own vertical origin offset — 0 for a foot-anchored NIF (DAoC z is the
/// feet), half the extent for a centre-anchored placeholder box. It applies in BOTH branches: a box
/// that keeps its packet z still needs lifting, or it sinks halfway into the surface it was placed on.
#[must_use]
pub fn entity_render_z(packet_z: f32, surface: Option<f32>, anchor: f32) -> f32 {
    match surface {
        Some(h) if (packet_z - h).abs() <= MAX_GROUND_SNAP => h + anchor,
        _ => packet_z + anchor,
    }
}

/// Cap on the ground grid's half-span (world units) so a far outlier mob can't blow it up.
pub const GRID_EXTENT_CAP: f32 = 160_000.0;
/// `--live` warm-up budget: how long to drain the live feed before sizing the scene + opening the
/// window, so the spawn position and initial population are present up front.
pub const LIVE_WARMUP: std::time::Duration = std::time::Duration::from_secs(12);

/// DAoC heading (0..4096 = a full turn) → yaw radians for the model shader.
///
/// The mesh's authored forward is render −Y, so a world heading `h` maps to a yaw of `+h`: the
/// render facing `(sin h, −cos h)` is exactly world `(sin h, cos h)` with the Y-mirror applied.
///
/// It was NEGATED for a long time, which is a MIRROR of the correct rotation rather than an offset.
/// Testing at headings 0 and 2048 cannot catch that — both formulas agree there, because those are
/// the two headings a mirror leaves fixed. What caught it was telemetry: during a turn the camera
/// yaw moved exactly opposite the character's heading (dHeading +3.7 against dCamYaw −3.7 across
/// 131 samples), so the character counter-rotated against a camera that was itself correct,
/// spinning relative to it at double rate. That is the "it turns without the camera" symptom.
///
/// The camera's azimuth is a WORLD angle (`Camera` applies the mirror itself); this is a RENDER
/// angle (already mirrored). Mixing the two spaces is what allowed one to be flipped relative to
/// the other for so long. Check against a controlled reference — `caer-render --avatars
/// --avatar-heading N` — and check at 1024/3072, not just 0/2048.
#[must_use]
pub fn heading_to_yaw(heading: u16) -> f32 {
    f32::from(heading) * (std::f32::consts::TAU / 4096.0)
}

/// Static fixture boxes → cube-pipeline instances (muted brown/green so live entities still pop).
#[must_use]
pub fn fixture_instances(fixtures: &[terrain::FixtureBox]) -> Vec<Instance> {
    fixtures
        .iter()
        .map(|f| Instance {
            pos: f.pos,
            color: if f.is_tree {
                [0.16, 0.30, 0.14]
            } else {
                [0.48, 0.38, 0.26]
            },
            scale: f.half,
        })
        .collect()
}

/// Load a static mob dump (TSV) into a `WorldState` via the same NpcInView path worldbench uses.
#[must_use]
pub fn load_world(path: &str) -> WorldState {
    let file = std::fs::File::open(path).unwrap_or_else(|e| {
        log::info!("caer-render: could not open {path}: {e}");
        std::process::exit(1);
    });
    let mobs = load_mobs_tsv(std::io::BufReader::new(file));
    let mut world = WorldState::new();
    for npc in mobs {
        world.apply(&ServerEvent::NpcInView(npc));
    }
    world
}

/// The mean XY of all entities (the render-space origin — keeps GPU coords small and the camera
/// centred) plus the population's XY half-span from that centre, capped, to size the ground grid.
/// Z is left absolute (the vertical spread is small). With no population, centre on the region's
/// zone table instead.
#[must_use]
pub fn scene_bounds(world: &WorldState, region: u16) -> (Vec3, f32) {
    let positions = world.positions();
    if positions.is_empty() {
        let ([min_x, min_y], [max_x, max_y]) = region_zone_bbox(region);
        let origin = Vec3::new(
            (min_x + max_x) as f32 / 2.0,
            (min_y + max_y) as f32 / 2.0,
            0.0,
        );
        let extent = ((max_x - min_x).max(max_y - min_y) as f32 / 2.0).min(GRID_EXTENT_CAP);
        return (origin, extent.max(CULL_RADIUS as f32));
    }
    let (mut sx, mut sy) = (0.0_f64, 0.0_f64);
    for p in positions {
        sx += f64::from(p[0]);
        sy += f64::from(p[1]);
    }
    let n = positions.len() as f64;
    let origin = Vec3::new((sx / n) as f32, (sy / n) as f32, 0.0);
    let mut extent = 0.0_f32;
    for p in positions {
        extent = extent
            .max((p[0] as f32 - origin.x).abs())
            .max((p[1] as f32 - origin.y).abs());
    }
    (origin, extent.min(GRID_EXTENT_CAP).max(CULL_RADIUS as f32))
}

/// World-space XY bounding box of the loaded population — or, unpopulated, of the whole region's
/// zone rectangles. Picks which zones' terrain loads.
#[must_use]
pub fn world_bbox(world: &WorldState, region: u16) -> ([i32; 2], [i32; 2]) {
    let positions = world.positions();
    if positions.is_empty() {
        return region_zone_bbox(region);
    }
    let mut min = [i32::MAX, i32::MAX];
    let mut max = [i32::MIN, i32::MIN];
    for p in positions {
        min[0] = min[0].min(p[0]);
        min[1] = min[1].min(p[1]);
        max[0] = max[0].max(p[0]);
        max[1] = max[1].max(p[1]);
    }
    (min, max)
}

/// Bounding box of every zone rectangle in `region` (world units).
#[must_use]
pub fn region_zone_bbox(region: u16) -> ([i32; 2], [i32; 2]) {
    let zones = region_zone_offsets(region);
    if zones.is_empty() {
        return ([0, 0], [0, 0]);
    }
    let min_x = zones.iter().map(|z| z.1).min().unwrap() * ZONE_UNIT;
    let min_y = zones.iter().map(|z| z.2).min().unwrap() * ZONE_UNIT;
    let max_x = zones.iter().map(|z| z.1).max().unwrap() * ZONE_UNIT + 65_536;
    let max_y = zones.iter().map(|z| z.2).max().unwrap() * ZONE_UNIT + 65_536;
    ([min_x, min_y], [max_x, max_y])
}

/// Per-`Kind` base colour for the placeholder box; dimmed by LOD so distant boxes read as further.
#[must_use]
pub fn color_for(kind: Kind, lod: u8) -> [f32; 3] {
    let base = match kind {
        Kind::Npc => [0.85, 0.5, 0.2],
        Kind::Player => [0.3, 0.8, 0.35],
        Kind::StaticObject => [0.5, 0.5, 0.55],
        Kind::Self_ => [1.0, 1.0, 1.0],
        Kind::Unknown => [0.8, 0.2, 0.7],
    };
    let dim = match lod {
        0 => 1.0,
        1 => 0.82,
        _ => 0.65,
    };
    [base[0] * dim, base[1] * dim, base[2] * dim]
}

/// Build one frame's draw set from the world's near slice, shared by every shell (dev lens, game
/// client, headless screenshot). Fills `boxes` with placeholder-box instances (self, objects,
/// unresolved models) and pushes each resolvable creature's mesh instances straight into the GPU
/// entity buffers grouped by model id. `mesh_instances` is a caller-owned scratch map reused across
/// frames so the per-model instance vecs don't reallocate.
///
/// Returns the count of box instances (the caller uploads `boxes` and draws that many).
#[allow(clippy::too_many_arguments)]
/// Seconds since the first call — the wall clock GPU-skinned animation advances on.
///
/// Interactive frames pass this; headless `--screenshot` frames pass a fixed value instead, so a
/// golden image renders the same pose every run. That split is why `render_world` takes the time as
/// a parameter rather than reading a clock internally.
pub fn anim_clock() -> f32 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f32()
}

/// CPU-side phase split for `render_world` (MS-01 / MEAS-009 attribution).
///
/// `cull` = spatial query; `build` = instance gather + bone palettes; `upload` = GPU buffer writes.
/// When timing is enabled, `anim_skin_*` further splits the palette rebuild (QA-4).
///
/// **Parallel path (`CAER_PARALLEL_ANIM=1`):** wall-clock spans only. `keyframe_ms` /
/// `bone_compose_ms` / `palette_assemble_ms` stay 0 and `bone_subattr` is false — those Instant
/// probes are single-threaded and would force the serial builder (ATTR-blind to the win). Use
/// `palette_build_ms` instead for the matrix-build wall under the flag.
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuPhaseMs {
    pub cull_ms: f64,
    pub build_ms: f64,
    pub upload_ms: f64,
    pub total_ms: f64,
    /// Instance gather + motion (build wall time before palette loop).
    pub gather_ms: f64,
    /// Palette rebuild wall time (subset of `build_ms`): discover + tick + matrix build.
    pub anim_skin_ms: f64,
    pub keyframe_ms: f64,
    pub bone_compose_ms: f64,
    pub palette_assemble_ms: f64,
    /// `tick_anim` + prune inside the anim_skin window (QA-4 residual naming).
    pub tick_anim_ms: f64,
    /// Job discover / clip sample / dedup (wall under parallel; Instant-sum under serial ATTR).
    pub skin_dispatch_ms: f64,
    /// Wall time of matrix builds only (`build_all_model_palettes` / timed serial loop).
    pub palette_build_ms: f64,
    /// True when this frame's palettes came from the parallel strangler path.
    pub parallel_anim: bool,
    /// True when keyframe/compose/assemble Instant sub-buckets are meaningful (serial ATTR only).
    pub bone_subattr: bool,
    /// Skinned instances drawn this frame.
    pub skinned_instances: u32,
    /// Distinct `(model, loco, phase_bucket)` keys at 30 Hz quantization — palette-dedup ceiling.
    pub distinct_palette_keys_30hz: u32,
    /// Unique palettes actually built this frame after dedup (≤ skinned_instances).
    pub unique_palettes_built: u32,
    /// Upload composition (MS-08) — populated when timing is on.
    pub upload_write_calls: u32,
    pub upload_write_bytes: u64,
    pub upload_skinned_models: u32,
    pub upload_skinned_inst_calls: u32,
    pub upload_skinned_palette_calls: u32,
    pub upload_skinned_inst_bytes: u64,
    pub upload_skinned_palette_bytes: u64,
    pub upload_skinned_bone_calls: u32,
    pub upload_skinned_bone_bytes: u64,
    /// True when this frame used GPU compute palette fold (bones upload only).
    pub gpu_palette_fold: bool,
    pub upload_entity_models: u32,
    pub upload_palette_stride_sum: u64,
    pub upload_bone_stride_sum: u64,
}

pub fn render_world(
    world: &WorldState,
    origin: Vec3,
    cull_center: [i32; 2],
    cull_radius: i32,
    entity_models: Option<&mut EntityModels>,
    gpu: &mut Gpu,
    boxes: &mut Vec<Instance>,
    mesh_instances: &mut HashMap<u16, Vec<[f32; 5]>>,
    ground: Option<&terrain::TerrainMesh>,
    self_model: Option<u16>,
    self_motion: Option<entities::Motion>,
    motion_tracker: Option<&mut motion::MotionTracker>,
    dt: f32,
    anim_time: f32,
) -> u32 {
    render_world_timed(
        world,
        origin,
        cull_center,
        cull_radius,
        entity_models,
        gpu,
        boxes,
        mesh_instances,
        ground,
        self_model,
        self_motion,
        motion_tracker,
        dt,
        anim_time,
        None,
    )
}

/// Like [`render_world`], but fills `timing` with per-phase CPU milliseconds when provided.
pub fn render_world_timed(
    world: &WorldState,
    origin: Vec3,
    cull_center: [i32; 2],
    cull_radius: i32,
    mut entity_models: Option<&mut EntityModels>,
    gpu: &mut Gpu,
    boxes: &mut Vec<Instance>,
    mesh_instances: &mut HashMap<u16, Vec<[f32; 5]>>,
    ground: Option<&terrain::TerrainMesh>,
    self_model: Option<u16>,
    // Which way the PLAYER is travelling relative to their facing. Only the shell that owns the
    // input knows this, and speed alone cannot recover it — backing up and sidestepping are the
    // same wire speed as walking forward. `None` (the dev lens) leaves everything on Forward.
    self_motion: Option<entities::Motion>,
    // Smooths NPC movement between server updates. `None` (the dev lens, screenshots) renders raw
    // packet positions, which is what a deterministic golden needs.
    mut motion_tracker: Option<&mut motion::MotionTracker>,
    // Seconds since the previous frame, for the smoothing step.
    dt: f32,
    // `anim_time`: seconds since launch, driving GPU-skinned animation. Passed in rather than read
    // from a global clock so a headless/screenshot frame renders a deterministic, reproducible pose.
    anim_time: f32,
    timing: Option<&mut CpuPhaseMs>,
) -> u32 {
    let t_all = std::time::Instant::now();
    let t0 = std::time::Instant::now();
    let items = world.render_set_par(cull_center, cull_radius, 0);
    let cull_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let t_build = std::time::Instant::now();
    // Skinned instances are gathered per frame (not retained like `mesh_instances`) because each
    // carries an animation phase; the palettes below are rebuilt every frame regardless.
    type SkinnedGather = (u16, u16, entities::Loco, gpu::SkinnedInstance, f32);
    let mut skinned_instances: HashMap<u16, Vec<SkinnedGather>> = HashMap::new();
    boxes.clear();
    boxes.reserve(items.len());
    for insts in mesh_instances.values_mut() {
        insts.clear();
    }
    for item in &items {
        if let Some(v) = world.get(item.object_id) {
            // An `Unknown` entity is one we have a POSITION for and nothing else — its create
            // packet hasn't arrived (or never will, for something that spawned outside our
            // interest radius). It used to draw as a bright magenta placeholder box, which is the
            // "pink squares" littering the world: we were advertising our own missing data to the
            // player. Draw nothing until we know what it is; it upgrades in place the moment the
            // create lands.
            if v.kind == Kind::Unknown {
                continue;
            }
            // PlayerDeath 0xAE / PlayerRevive 0x89 → WorldState.is_dead. Living draw skips corpses
            // (they take the corpse path below); inventing this from 0 HP alone is forbidden.
            if !death::in_living_draw(v.is_dead) && !death::in_corpse_draw(v.is_dead) {
                continue;
            }
            // Which mesh (if any) draws for this entity. The player (`Self_`) uses its assembled
            // avatar mesh (pre-uploaded by the caller via `EntityModels::ensure_avatar`). Other
            // players use the same fig3 path when WorldState decoded race+gender from PlayerCreate's
            // living-model bits; NPCs/objects use creatures.csv. `None` → placeholder box.
            let mesh_model: Option<u16> = if v.kind == Kind::Self_ {
                self_model.filter(|&id| gpu.has_entity_mesh(id) || gpu.has_skinned_mesh(id))
            } else if v.kind == Kind::Player {
                match (
                    entity_models.as_deref_mut(),
                    world.other_player_avatar(v.object_id),
                ) {
                    (
                        Some(em),
                        Some(OtherPlayerAvatar::Resolved {
                            race,
                            gender,
                            appearance,
                            ..
                        }),
                    ) => em.ensure_avatar(
                        gpu,
                        race,
                        gender,
                        appearance,
                        world.equipment_of(v.object_id),
                    ),
                    // Unresolved living-model: diagnostic box, never a default fig3.
                    (_, Some(OtherPlayerAvatar::Unresolved { .. })) => None,
                    _ => None,
                }
            } else if v.model != 0 {
                match entity_models.as_deref_mut() {
                    Some(em) => {
                        em.ensure_mesh_for_equipment(gpu, v.model, world.equipment_of(v.object_id))
                    }
                    None => None,
                }
            } else {
                None
            };
            // Seat the entity on our terrain only where its own z already agrees with it — see
            // `entity_render_z`. Box placeholders are centre-anchored (lift by BOX_SIZE to rest on
            // the surface); NIF meshes are foot-anchored, matching DAoC's feet-origin z.
            //
            // This used to snap unconditionally, which flattened every deliberately-elevated mob
            // (flying, keep walls, upper floors) onto the dirt. The measurement that settled it is
            // in MAX_GROUND_SNAP.
            // Smooth NPC movement between server updates. Without this a walking mob stands still
            // playing its walk cycle and then jumps — the position is only as fresh as the last
            // packet. The player's own entity is excluded: the client is authoritative for itself
            // and already integrates its own movement every frame.
            let raw = [v.pos[0] as f32, v.pos[1] as f32, v.pos[2] as f32];
            let motion = match (&mut motion_tracker, v.kind) {
                (Some(t), k) if k != Kind::Self_ => t.resolve(v.object_id, raw, anim_time, dt),
                // No tracker (dev lens / screenshots) or our own entity: raw packet position, and
                // fall back to the wire's speed for the locomotion clip.
                _ => motion::Motion {
                    pos: raw,
                    measured_speed: f32::from(v.speed),
                },
            };
            let smoothed = motion.pos;
            // Drive the locomotion clip from OBSERVED speed, not the wire's. The wire can report a
            // speed while the entity is not actually moving, which is what made mobs walk in place.
            let effective_speed = motion
                .measured_speed
                .round()
                .clamp(0.0, f32::from(u16::MAX)) as u16;
            let anchor = if mesh_model.is_some() { 0.0 } else { BOX_SIZE };
            let ez = entity_render_z(
                smoothed[2],
                // Walkable surface, not bare terrain: NPCs stand on docks, keep floors and the
                // Cotswold forge platform, and the heightfield knows about none of them.
                ground.and_then(|g| g.walk_height_at(smoothed[0], smoothed[1], smoothed[2])),
                anchor,
            );
            let rp = [
                smoothed[0] - origin.x,
                // World is y-south, render space y-north — negate on emission (see camera.rs).
                -(smoothed[1] - origin.y),
                ez - origin.z,
            ];
            // Two independent scale factors, and both are needed:
            //   * the per-INSTANCE size byte below — is this a big or small example of its kind;
            //   * the per-MODEL `Scale` from monsters.csv — was this MESH authored at world scale.
            // Ignoring the model scale drew the Dragonfly (authored ~1694 units across, Scale 10)
            // about twenty times too large, and Large Skeleton (Scale 125) a quarter too short.
            let model_scale = entity_models
                .as_deref()
                .map_or(1.0, |em| em.model_scale(v.model));
            // Creature scale from the server's size byte (percentage; 50 = normal humanoid).
            // Ignoring it rendered every creature at one size — an ambient rat (size 4) as big as
            // a person, and a giant skeleton (size 151–199 in the captures) at a person's height
            // rather than the 3–4× it should be.
            let entity_scale = ENTITY_SCALE * entity_size_scale(v.size) * model_scale;
            if let Some(m) = mesh_model {
                if gpu.has_skinned_mesh(m) {
                    // Phase each entity separately so a group of the same creature doesn't breathe
                    // in lockstep. The golden-ratio step spreads ids evenly without a hash, and is
                    // derived from object_id so an entity's phase is stable frame to frame.
                    let phase = (f32::from(v.object_id) * 0.618_034).fract();
                    // The player's direction of travel comes from the shell that owns the keys;
                    // every other entity is assumed to be moving the way it faces until the wire's
                    // strafe bits are decoded.
                    let facing = match (v.kind, self_motion) {
                        (Kind::Self_, Some(m)) => m,
                        _ => entities::Motion::Forward,
                    };
                    let want = entities::SkinnedRig::state_for_motion(effective_speed, facing);
                    skinned_instances.entry(m).or_default().push((
                        v.object_id,
                        effective_speed,
                        want,
                        gpu::SkinnedInstance {
                            pos_yaw: [rp[0], rp[1], rp[2], heading_to_yaw(v.heading)],
                            scale: entity_scale,
                            palette_base: 0.0, // filled in below, once the per-model order is known
                        },
                        phase,
                    ));
                } else {
                    mesh_instances.entry(m).or_default().push([
                        rp[0],
                        rp[1],
                        rp[2],
                        heading_to_yaw(v.heading),
                        entity_scale,
                    ]);
                }
            }
            if let (Some(em), Some(eq)) = (
                entity_models.as_deref_mut(),
                world.equipment_of(v.object_id),
            ) {
                use caer_protocol::equipment::slot;
                for (slot_id, lift) in [(slot::RIGHTHAND, 42.0_f32), (slot::LEFTHAND, 38.0)] {
                    let Some(item) = eq.item(slot_id) else {
                        continue;
                    };
                    if item.model == 0 {
                        continue;
                    }
                    if em.ensure_mesh(gpu, item.model) {
                        // Bone-attach is not in this slice; origin+lift is the visible weapon/shield.
                        mesh_instances.entry(item.model).or_default().push([
                            rp[0],
                            rp[1],
                            rp[2] + lift,
                            heading_to_yaw(v.heading),
                            entity_scale * 0.45,
                        ]);
                    }
                }
            }
            if mesh_model.is_none() {
                boxes.push(Instance {
                    pos: rp,
                    color: if v.kind == Kind::Player {
                        other_avatars::box_color(world.other_player_avatar(v.object_id), item.lod)
                    } else {
                        color_for(v.kind, item.lod)
                    },
                    scale: BOX_SIZE * entity_size_scale(v.size),
                });
            }
        }
    }
    // Drop motion state for entities that have left view, so the map does not grow with every mob
    // that has ever been near us. Generous window: an entity briefly culled and re-seen keeps its
    // smoothing rather than snapping on return.
    if let Some(t) = motion_tracker.as_mut() {
        t.prune(anim_time, 10.0);
    }

    // Skinned models: build each instance's bone palette on the CPU before any GPU upload so the
    // MS-01 phase timer can separate animation/skinning from buffer writes.
    // Palette dedup (QA-4 hyp 1): instances that share (loco, 30 Hz phase bucket) reuse one
    // palette slot — compact buffer, shared `palette_base`. Blend frames skip the cache.
    let gather_ms = t_build.elapsed().as_secs_f64() * 1000.0;
    let t_skin = std::time::Instant::now();
    let parallel = anim_skin::parallel_anim_enabled();
    let use_gpu_fold = anim_skin::gpu_palette_fold_enabled();
    let want_timing = timing.is_some();
    let mut bone_timing = caer_assets::nif::BoneMatrixTiming::default();
    let mut skinned_n = 0u32;
    let mut unique_built = 0u32;
    let mut tick_anim_ms = 0.0_f64;
    let mut skin_dispatch_ms = 0.0_f64;
    let mut palette_build_ms = 0.0_f64;
    let mut bone_subattr = false;
    let mut palette_keys: std::collections::HashSet<(u16, entities::Loco, u32)> =
        std::collections::HashSet::new();
    let mut skinned_uploads: Vec<anim_skin::ModelGpuUpload> = Vec::new();
    if let Some(em) = entity_models.as_mut() {
        let t_tick = want_timing.then(std::time::Instant::now);
        let mut states: HashMap<u16, entities::EntityAnim> = HashMap::new();
        for entries in skinned_instances.values() {
            for (object_id, _speed, want, _, _) in entries {
                states.insert(*object_id, em.tick_anim(*object_id, *want, anim_time));
            }
        }
        em.prune_anim_states(anim_time);
        em.release_anim_not_in_world(world);
        if let Some(t0) = t_tick {
            tick_anim_ms = t0.elapsed().as_secs_f64() * 1000.0;
        }
        // §20 strangler: discover unique palette jobs in encounter order (serial), then build
        // matrices serially (`CAER_PARALLEL_ANIM=0`) or in parallel (shipped default).
        // When the flag is on, ATTR uses the SAME parallel builder + wall-clock spans — never the
        // BoneMatrixTiming serial path (that made ATTR blind to the claimed win).
        let t_discover = want_timing.then(std::time::Instant::now);
        let mut prepared: Vec<anim_skin::ModelPaletteWork> = Vec::new();
        for (model, entries) in skinned_instances.iter_mut() {
            let Some(rig) = em.skinned_rig(*model) else {
                continue;
            };
            let stride = rig.palette_stride();
            let mut jobs: Vec<anim_skin::UniquePaletteJob> = Vec::new();
            // (loco, phase_bucket) → job index in `jobs` (compact buffer base = idx * stride).
            let mut dedup: HashMap<(entities::Loco, u32), u32> = HashMap::new();
            let mut insts: Vec<gpu::SkinnedInstance> = Vec::with_capacity(entries.len());
            let sample =
                |clip: &caer_assets::nif::Clip, stride_units: f32, speed: u16, phase: f32| {
                    clip_sample_time(clip, stride_units, speed, phase, anim_time)
                };
            for (object_id, speed, want, inst, phase) in entries.iter_mut() {
                // Serial ATTR keeps per-instance Instant for residual naming; parallel ATTR uses
                // one wall around the whole discover loop (below) so Instant tax doesn't invent work.
                let t_dispatch = (want_timing && !parallel).then(std::time::Instant::now);
                let want = *want;
                let anim = states.get(object_id).copied();
                let (clip, su) = rig.clip_for_state(want);
                let t = sample(clip, su, *speed, *phase);
                let bucket = (t * 30.0).floor() as u32;
                if want_timing {
                    palette_keys.insert((*model, want, bucket));
                }
                skinned_n += 1;
                let blending = anim.and_then(|a| a.blend_weight(anim_time).map(|w| (a, w)));
                if let Some(t0) = t_dispatch {
                    skin_dispatch_ms += t0.elapsed().as_secs_f64() * 1000.0;
                }

                let job_idx = match blending {
                    Some((a, w)) => {
                        // Cross-fade: unique per instance (prev/weight not in the 30 Hz key).
                        let prev = a.prev.unwrap_or(want);
                        let (pclip, psu) = rig.clip_for_state(prev);
                        let pt = sample(pclip, psu, *speed, *phase);
                        let idx = jobs.len() as u32;
                        jobs.push(anim_skin::UniquePaletteJob {
                            loco: want,
                            t,
                            blend: Some(anim_skin::BlendJob {
                                prev,
                                prev_t: pt,
                                w,
                            }),
                        });
                        unique_built += 1;
                        idx
                    }
                    None => {
                        let key = (want, bucket);
                        if let Some(&cached) = dedup.get(&key) {
                            cached
                        } else {
                            let idx = jobs.len() as u32;
                            jobs.push(anim_skin::UniquePaletteJob {
                                loco: want,
                                t,
                                blend: None,
                            });
                            unique_built += 1;
                            dedup.insert(key, idx);
                            idx
                        }
                    }
                };
                inst.palette_base = (job_idx as usize * stride) as f32;
                insts.push(*inst);
            }
            prepared.push(anim_skin::ModelPaletteWork {
                model: *model,
                insts,
                jobs,
            });
        }
        if let Some(t0) = t_discover {
            if parallel {
                // Wall for the whole discover pass (replaces Instant-sum skin_dispatch).
                skin_dispatch_ms = t0.elapsed().as_secs_f64() * 1000.0;
            }
        }

        let t_pal = want_timing.then(std::time::Instant::now);
        if use_gpu_fold {
            // Pose bones only; compute expands parts×bones on the GPU (MS-08).
            // ATTR: always wall-clock — BoneMatrixTiming Instant probes cover the old CPU fold.
            skinned_uploads = anim_skin::build_all_model_bones(em, prepared);
            bone_subattr = false;
        } else if parallel {
            skinned_uploads = anim_skin::build_all_model_palettes(em, prepared);
            bone_subattr = false;
        } else if want_timing {
            // Serial ATTR + CPU fold: Instant-probed bone path (historical QA-4 sub-buckets).
            for w in prepared {
                let Some(rig) = em.skinned_rig(w.model) else {
                    continue;
                };
                let palettes = anim_skin::build_palettes_timed(rig, &w.jobs, &mut bone_timing);
                skinned_uploads.push((w.model, w.insts, palettes));
            }
            bone_subattr = true;
        } else {
            skinned_uploads = anim_skin::build_all_model_palettes(em, prepared);
            bone_subattr = false;
        }
        if let Some(t0) = t_pal {
            palette_build_ms = t0.elapsed().as_secs_f64() * 1000.0;
        }
    }
    let anim_skin_ms = t_skin.elapsed().as_secs_f64() * 1000.0;
    let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;
    let t_upload = std::time::Instant::now();
    gpu.begin_upload_stats();

    // Push this frame's entity-mesh instances (0-fill the rest so departed mobs stop drawing).
    gpu.clear_entity_instances();
    for (model, insts) in mesh_instances.iter() {
        gpu.update_entity_instances(*model, insts);
    }
    gpu.clear_skinned_instances();
    if use_gpu_fold {
        for (model, insts, bones) in &skinned_uploads {
            gpu.update_skinned_bones(*model, insts, bones);
        }
    } else {
        for (model, insts, palettes) in &skinned_uploads {
            gpu.update_skinned_instances(*model, insts, palettes);
        }
    }
    let mut live_meshes: HashSet<u16> = mesh_instances
        .iter()
        .filter(|(_, insts)| !insts.is_empty())
        .map(|(&model, _)| model)
        .collect();
    live_meshes.extend(skinned_uploads.iter().map(|(model, _, _)| *model));
    if let Some(model) = self_model {
        live_meshes.insert(model);
    }
    if let Some(em) = entity_models.as_mut() {
        em.evict_unseen_meshes(gpu, &live_meshes);
    }
    let upload_stats = gpu.take_upload_stats();
    let upload_ms = t_upload.elapsed().as_secs_f64() * 1000.0;
    if let Some(t) = timing {
        t.cull_ms = cull_ms;
        t.build_ms = build_ms;
        t.gather_ms = gather_ms;
        t.anim_skin_ms = anim_skin_ms;
        t.keyframe_ms = bone_timing.keyframe_ns as f64 / 1_000_000.0;
        t.bone_compose_ms = bone_timing.bone_compose_ns as f64 / 1_000_000.0;
        t.palette_assemble_ms = bone_timing.assemble_ns as f64 / 1_000_000.0;
        t.tick_anim_ms = tick_anim_ms;
        t.skin_dispatch_ms = skin_dispatch_ms;
        t.palette_build_ms = palette_build_ms;
        t.parallel_anim = parallel;
        t.bone_subattr = bone_subattr;
        t.skinned_instances = skinned_n;
        t.distinct_palette_keys_30hz = palette_keys.len() as u32;
        t.unique_palettes_built = unique_built;
        t.upload_ms = upload_ms;
        t.upload_write_calls = upload_stats.write_calls;
        t.upload_write_bytes = upload_stats.write_bytes;
        t.upload_skinned_models = upload_stats.skinned_models_touched;
        t.upload_skinned_inst_calls = upload_stats.skinned_inst_calls;
        t.upload_skinned_palette_calls = upload_stats.skinned_palette_calls;
        t.upload_skinned_inst_bytes = upload_stats.skinned_inst_bytes;
        t.upload_skinned_palette_bytes = upload_stats.skinned_palette_bytes;
        t.upload_skinned_bone_calls = upload_stats.skinned_bone_calls;
        t.upload_skinned_bone_bytes = upload_stats.skinned_bone_bytes;
        t.gpu_palette_fold = use_gpu_fold;
        t.upload_entity_models = upload_stats.entity_models_touched;
        t.upload_palette_stride_sum = upload_stats.skinned_palette_stride_sum;
        t.upload_bone_stride_sum = upload_stats.skinned_bone_stride_sum;
        t.total_ms = t_all.elapsed().as_secs_f64() * 1000.0;
    }
    boxes.len() as u32
}

#[cfg(test)]
mod grounding_tests {
    use super::*;

    /// A mob standing on the ground gets seated on OUR surface, so small disagreements between our
    /// terrain decode and the server's don't leave it floating or half-sunk.
    #[test]
    fn a_mob_near_the_surface_is_seated_on_it() {
        // Packet says 2325, our decode says 2323 — within the noise, so our surface wins.
        assert!((entity_render_z(2325.0, Some(2323.0), 0.0) - 2323.0).abs() < 1e-6);
        // Exactly at the tolerance still snaps (the boundary belongs to the ground case).
        assert!(
            (entity_render_z(2323.0 + MAX_GROUND_SNAP, Some(2323.0), 0.0) - 2323.0).abs() < 1e-6
        );
    }

    /// The regression this exists to prevent: an entity deliberately ABOVE the ground — flying, on a
    /// keep wall, on an upper floor — must keep its own z. 576 of region 1's spawns (3.5%) sit more
    /// than 256 units up, and snapping flattened every one of them onto the dirt.
    #[test]
    fn a_deliberately_elevated_mob_keeps_its_own_height() {
        let flying = entity_render_z(3400.0, Some(2323.0), 0.0);
        assert!(
            (flying - 3400.0).abs() < 1e-6,
            "an airborne mob was dragged to {flying}"
        );
        // …and one below the surface (a cellar or cave) is not shoved up through the floor.
        let cellar = entity_render_z(1300.0, Some(2323.0), 0.0);
        assert!(
            (cellar - 1300.0).abs() < 1e-6,
            "a mob under the terrain was surfaced at {cellar}"
        );
    }

    /// Off the loaded terrain (no heightfield, or beyond its edge) the packet z is all we have.
    #[test]
    fn without_a_surface_the_packet_z_stands() {
        assert!((entity_render_z(1750.0, None, 0.0) - 1750.0).abs() < 1e-6);
    }

    /// The mesh's own vertical origin must be applied whether or not we snapped — a centre-anchored
    /// placeholder box that keeps its packet z would otherwise sink halfway into the ground.
    #[test]
    fn the_anchor_applies_on_both_paths() {
        // Snapped: box centre sits one half-extent above the surface.
        assert!(
            (entity_render_z(2325.0, Some(2323.0), BOX_SIZE) - (2323.0 + BOX_SIZE)).abs() < 1e-6
        );
        // Not snapped: same lift, applied to the packet z it kept.
        assert!(
            (entity_render_z(3400.0, Some(2323.0), BOX_SIZE) - (3400.0 + BOX_SIZE)).abs() < 1e-6
        );
        assert!((entity_render_z(1750.0, None, BOX_SIZE) - (1750.0 + BOX_SIZE)).abs() < 1e-6);
    }

    /// The tolerance has to sit above our decode noise but well below a real elevation. Measured
    /// over 16,419 region-1 spawns: p75 of |authored - surface| is 1 unit and 85% fall inside 16,
    /// while genuine elevations start in the hundreds.
    #[test]
    fn the_snap_tolerance_separates_decode_noise_from_real_elevation() {
        const {
            assert!(MAX_GROUND_SNAP >= 16.0);
            assert!(MAX_GROUND_SNAP < 256.0);
        }
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;

    /// Scale must follow the server's size byte, anchored on the sizes actually seen in the
    /// captures. Before this, every creature drew at one scale — the bug Matt spotted as
    /// "those skeletons should be way bigger".
    #[test]
    fn size_scale_matches_the_captured_range() {
        // 50 is normal humanoid scale (oracle GameNPC default).
        assert!((entity_size_scale(50) - 1.0).abs() < 1e-6);

        // Human NPCs cluster at 48–57 and must stay near 1×.
        for s in [48u8, 52, 57] {
            let k = entity_size_scale(s);
            assert!((0.9..=1.2).contains(&k), "human size {s} scaled to {k}");
        }

        // A giant skeleton (151–199 in the captures) must be several times a person.
        let giant = entity_size_scale(180);
        assert!(giant > 3.0, "a giant skeleton should tower; got {giant}");
        assert!(giant > entity_size_scale(52) * 3.0);

        // An ambient rat (4–10) must be far smaller than a person.
        assert!(entity_size_scale(8) < 0.25, "an ambient rat should be tiny");
    }

    /// Size 0 appears in the captures and means "unspecified", not "zero-sized" — mapping it
    /// literally would make those entities disappear.
    #[test]
    fn unspecified_size_renders_at_normal_scale() {
        assert!((entity_size_scale(0) - 1.0).abs() < 1e-6);
    }

    /// A malformed byte must not be able to make an entity vanish or fill the screen.
    #[test]
    fn extreme_sizes_are_clamped() {
        assert!(entity_size_scale(255) <= 6.0);
        assert!(entity_size_scale(1) >= 0.05);
        assert!(entity_size_scale(255).is_finite());
    }
}

/// Where in a clip's key timeline to sample, in AUTHORED seconds.
///
/// Two regimes, and the distinction is the whole point:
///
/// * **Moving** (`stride_units > 0` and `speed > 0`) — the stride match wins. One cycle must cover
///   `stride_units` of ground, so the rate is `speed / stride`, which is what keeps feet planted
///   instead of skating. The authored rate is deliberately ignored here.
/// * **Standing** — advance at the clip's own playback rate, `fps / base_fps` from `animnifs.csv`.
///
/// The standing case used to be a bare `1.0 / d`, i.e. always the authoring rate. `I_hm` — the
/// humanoid idle nearly every NPC retargets — is 30 frames authored at 15fps but played at 4fps, so
/// it ran 3.75x too fast, while a creature with its own authored-rate idle (the minotaur) looked
/// correct. That split is exactly what the defect looked like in game.
pub fn clip_sample_time(
    clip: &caer_assets::nif::Clip,
    stride_units: f32,
    speed: u16,
    phase: f32,
    anim_time: f32,
) -> f32 {
    let d = clip.duration.max(0.001);
    let rate = if stride_units > 0.0 && speed > 0 {
        f32::from(speed) / stride_units
    } else {
        clip.rate.max(0.001) / d
    };
    ((anim_time + phase * d) * rate * d % d).abs()
}

/// Idle sample time for a preworld avatar. Same convention as the world renderer — not
/// `anim_clock().rem_euclid(clip.duration)`, which plays `I_hm` 3.75× too fast.
#[must_use]
pub fn preworld_idle_t(clip: &caer_assets::nif::Clip, anim_time: f32) -> f32 {
    clip_sample_time(clip, 0.0, 0, 0.0, anim_time)
}

#[cfg(test)]
mod anim_rate_tests {
    use super::*;

    fn clip(duration: f32, rate: f32) -> caer_assets::nif::Clip {
        caer_assets::nif::Clip {
            tracks: Default::default(),
            duration,
            rate,
        }
    }

    /// A standing clip must take `duration / rate` real seconds to complete one cycle.
    ///
    /// The real case: `I_hm` is 30 frames authored at 15fps (a 2.0s key timeline) played at 4fps,
    /// so rate = 4/15 and one idle cycle should last 7.5 real seconds. Before the fix it lasted
    /// 2.0s. Asserting the CYCLE LENGTH rather than a sampled value is what makes this catch the
    /// bug instead of restating the formula.
    #[test]
    fn a_standing_clip_plays_over_its_playback_duration_not_its_authored_one() {
        let c = clip(2.0, 4.0 / 15.0);
        // Just before one full playback cycle we are near the end of the key timeline; just after,
        // it has wrapped back to the start.
        let near_end = clip_sample_time(&c, 0.0, 0, 0.0, 7.49);
        let wrapped = clip_sample_time(&c, 0.0, 0, 0.0, 7.51);
        assert!(
            near_end > 1.9,
            "at 7.49s the 7.5s idle should be nearly finished, got {near_end}"
        );
        assert!(
            wrapped < 0.1,
            "at 7.51s the 7.5s idle should have wrapped, got {wrapped}"
        );
        // And the old behaviour must NOT hold: at 2.0s it should be a third of the way in, not wrapped.
        let at_authored = clip_sample_time(&c, 0.0, 0, 0.0, 2.0);
        assert!(
            at_authored > 0.4 && at_authored < 0.7,
            "at 2.0s a 7.5s idle should be ~0.53s in (authored time), got {at_authored}"
        );
        // The preworld wrapper must not be the old rem_euclid(duration) path.
        assert!((preworld_idle_t(&c, 2.0) - at_authored).abs() < 1e-4);
        assert!(
            preworld_idle_t(&c, 2.0) > 0.4,
            "preworld idle at 2s must not wrap the 2s authored timeline"
        );
    }

    /// A clip played at its authored rate is unchanged — the ordinary case must not move.
    #[test]
    fn an_authored_rate_clip_is_unaffected() {
        let c = clip(2.0, 1.0);
        assert!((clip_sample_time(&c, 0.0, 0, 0.0, 0.5) - 0.5).abs() < 1e-4);
        assert!(clip_sample_time(&c, 0.0, 0, 0.0, 2.01) < 0.05);
    }

    /// Moving keeps the stride match: ground speed drives the cycle, not the authored rate.
    #[test]
    fn locomotion_still_matches_stride_and_ignores_the_playback_rate() {
        let slow = clip(2.0, 0.1);
        let fast = clip(2.0, 1.0);
        // Same stride and speed => same phase regardless of the clips' playback rates.
        let a = clip_sample_time(&slow, 100.0, 200, 0.0, 1.0);
        let b = clip_sample_time(&fast, 100.0, 200, 0.0, 1.0);
        assert!(
            (a - b).abs() < 1e-4,
            "stride-matched sampling must ignore clip.rate ({a} vs {b})"
        );
    }
}
