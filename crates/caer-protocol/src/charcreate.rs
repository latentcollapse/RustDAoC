//! CharacterCreateRequest (0xFF) — v1126+ (1.127) create/customize/delete body.
//!
//! Provenance: oracle `OpenDAoC-Core/.../CharacterCreateRequestHandler.CreationCharacterData`
//! for `client.Version > Version1124`. The 1125+ path is a **single** character record (no
//! account-name prefix, no 10-slot loop). Operation byte: `1` create, `2` customize, `3` delete.
//!
//! §14A joint validity (stats half): `CharacterStatValidator` requires the progressive cost of
//! points spent above race bases to equal `PointDistributionBudget` (30) exactly.

use crate::codec::PacketWriter;

/// Create / customize / delete discriminator (1125+ single-byte form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CreateOp {
    Create = 1,
    Customize = 2,
    Delete = 3,
}

/// Server budget for bonus points at create. Both oracles agree on 30:
/// `CharacterStatValidator.PointDistributionBudget` (retail client) and DOLSharp
/// `MaxStartingBonusPoints`. The value's single owner is `starting_stats`, so the
/// two copies that agreed today cannot disagree tomorrow.
pub const POINT_DISTRIBUTION_BUDGET: i32 = crate::starting_stats::MAX_STARTING_BONUS_POINTS as i32;

// The progressive-cost thresholds (10 / 15) live in `starting_stats::TIERS`; the old
// `COST_THRESHOLD_1/2` constants here were a dead second copy and are gone.

/// Wire / draft stat indices: STR, DEX, CON, QUI, INT, PIE, EMP, CHR
/// (`CreationCharacterData` ReadByte order).
pub const STAT_STR: usize = 0;
pub const STAT_DEX: usize = 1;
pub const STAT_CON: usize = 2;
pub const STAT_QUI: usize = 3;
pub const STAT_INT: usize = 4;
pub const STAT_PIE: usize = 5;
pub const STAT_EMP: usize = 6;
pub const STAT_CHR: usize = 7;

/// The lower eleven bits of the creation-model word name the race/gender body.
///
/// SoloDAoC's `GamePlayer.Model` documents the adjoining bits as the character-size adapter:
/// `0x0800` short, `0x1000` average, and `0x1800` tall.  `0x0000` is also read as average by
/// the server, which is the retail neutral/default representation.
pub const CREATION_MODEL_ID_MASK: u16 = 0x07FF;
pub const CREATION_MODEL_SIZE_MASK: u16 = 0x1800;
pub const CREATION_SIZE_SHORT: u8 = 1;
pub const CREATION_SIZE_AVERAGE: u8 = 2;
pub const CREATION_SIZE_TALL: u8 = 3;

/// Draft fields the create form collects before Continué submits 0xFF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterCreateDraft {
    /// Realm-local slot 0..9. Wire slot = `slot + (realm - 1) * 10`.
    pub slot: u8,
    pub name: String,
    /// 1 Albion / 2 Midgard / 3 Hibernia.
    pub realm: u8,
    pub class_id: u8,
    pub race: u8,
    /// 0 male / 1 female (packed into bit 7 of the race/gender byte).
    pub gender: u8,
    pub creation_model: u16,
    pub custom_mode: u8,
    pub eye_size: u8,
    pub lip_size: u8,
    pub eye_color: u8,
    pub hair_color: u8,
    pub face_type: u8,
    pub hair_style: u8,
    pub mood_type: u8,
    /// Stats: str, dex, con, qui, int, pie, emp, chr (wire order).
    pub stats: [u8; 8],
    /// 1126+ create Region byte (`CreationCharacterData1124`: after equipment, before NewConstitution).
    ///
    /// This is the **ClientRegionID filter key** StartupLocations matches on
    /// (`ClientRegionID == 0 || ClientRegionID == ch.Region`). For classic (non-tutorial)
    /// starts the OPEN_ORACLE wildcard rows use ClientRegionID 0 and destination `Region`
    /// 1/100/200 — we send that destination so the packet is not left at 0 / unset.
    pub region: u8,
    pub new_constitution: u8,
}

impl CharacterCreateDraft {
    /// The five appearance bytes as one value, for the renderer and for comparison.
    #[must_use]
    pub fn customization(&self) -> crate::customization::Customization {
        crate::customization::Customization {
            eye_color: self.eye_color,
            hair_color: self.hair_color,
            face_type: self.face_type,
            hair_style: self.hair_style,
            mood_type: self.mood_type,
        }
    }

    /// The complete source-authored look currently shown by the create preview.
    ///
    /// A fresh draft is allowed to retain `CustomMode == 0`: retail then leaves the base head at
    /// its neutral blend. Once any appearance control is touched, the UI seeds all four sliders at
    /// tick four and the outgoing mode preserves an explicit value of zero (the leftmost position).
    #[must_use]
    pub fn appearance(&self) -> crate::customization::AvatarAppearance {
        crate::customization::AvatarAppearance {
            customization: self.customization(),
            eye_size: self.eye_size,
            lip_size: self.lip_size,
            facial_morphs_enabled: self.custom_mode != 0
                || self.eye_size != 0
                || self.lip_size != 0
                || !self.customization().is_default(),
        }
    }

    fn facial_morphs_are_uninitialised(&self) -> bool {
        self.custom_mode == 0 && self.eye_size == 0 && self.lip_size == 0
    }

    fn seed_neutral_facial_morphs(&mut self) {
        if self.facial_morphs_are_uninitialised() {
            (self.eye_size, self.lip_size) = neutral_facial_morph_bytes();
        }
    }

    /// Mark this draft as an explicit source-customized appearance.
    ///
    /// DOLSharp persists the seven appearance bytes only when `CustomMode == 1`.  The four
    /// packed facial-slider nibbles are different from ordinary selector bytes: in an explicit
    /// record, wire zero is the leftmost authored position, not the neutral base.  Every path
    /// that enables customization must therefore seed an untouched head at the source midpoint
    /// before setting the mode.  Leaving it as `custom_mode = 1` at UI call sites made a plain
    /// Face/Hair click serialize four zero ticks and visibly collapse Highlander eyes.
    pub fn enable_customization(&mut self) {
        self.seed_neutral_facial_morphs();
        self.custom_mode = 1;
    }

    /// Set the whole look at once, and mark the draft customised so the bytes survive the wire.
    ///
    /// `CustomMode` is not decoration: `CharacterCreateRequestHandler` copies the appearance bytes
    /// onto the character only when it reads `1`, so a look sent with mode `0` is stored as zeros
    /// and the created character does not match the figure the player just accepted.
    pub fn set_customization(&mut self, c: crate::customization::Customization) {
        if !c.is_default() {
            self.enable_customization();
        }
        self.eye_color = c.eye_color;
        self.hair_color = c.hair_color;
        self.face_type = c.face_type;
        self.hair_style = c.hair_style;
        self.mood_type = c.mood_type;
    }

    /// Is any persisted visual attribute different from retail's all-zero default?
    ///
    /// The two morph bytes are outside [`Self::customization`] because the renderer's mesh/skin
    /// cache key currently uses the five discrete texture/mesh values. They still belong in the
    /// create packet's `CustomMode` decision: setting only Nose or Jaw must not cause the server
    /// to discard that accepted slider value.
    #[must_use]
    pub fn has_custom_appearance(&self) -> bool {
        self.eye_size != 0 || self.lip_size != 0 || !self.customization().is_default()
    }

    /// One visible source-order facial slider tick (0 through 8).
    #[must_use]
    pub fn facial_morph_tick(&self, slot: crate::customization::FacialMorphSlot) -> u8 {
        self.appearance().facial_morph_tick(slot)
    }

    /// Set one visible source-order facial slider tick without overwriting the other three.
    pub fn set_facial_morph_tick(&mut self, slot: crate::customization::FacialMorphSlot, tick: u8) {
        self.enable_customization();
        (self.eye_size, self.lip_size) =
            crate::customization::with_facial_morph_tick(self.eye_size, self.lip_size, slot, tick);
    }

    /// The record a **delete** request carries: a realm, a slot, and nothing else.
    ///
    /// `OPEN_ORACLE` `CharacterCreateRequestHandler._HandlePacket1124` `case 3` acts only when
    /// `CharName` is empty — a delete carrying a name is silently ignored — so the empty name is the
    /// operation, not an oversight. Every other field is unread on this path, which is why this is a
    /// constructor rather than a `Default` a caller could accidentally send as a *create*.
    #[must_use]
    pub fn for_delete(realm: u8, slot: u8) -> Self {
        Self {
            slot,
            name: String::new(),
            realm,
            class_id: 0,
            race: 0,
            gender: 0,
            creation_model: 0,
            custom_mode: 0,
            eye_size: 0,
            lip_size: 0,
            eye_color: 0,
            hair_color: 0,
            face_type: 0,
            hair_style: 0,
            mood_type: 0,
            stats: [0; 8],
            region: 0,
            new_constitution: 0,
        }
    }

    /// Albion Briton Armsman starter that spends the required 30 bonus points.
    ///
    /// Class 2 is **Armsman** (Fighter is 14). Allocation: +10 STR, +10 DEX, +10 CON on Briton
    /// bases of 60 — progressive cost 10+10+10 = 30 (`CharacterStatValidator`).
    #[must_use]
    pub fn albion_briton_stub(name: impl Into<String>, slot: u8) -> Self {
        let mut d = Self {
            slot,
            name: name.into(),
            realm: 1,
            class_id: 2, // Armsman (eCharacterClass::Armsman = 2; Fighter, the base class, is 14)
            race: 1,     // Briton (low 5 bits)
            gender: 0,
            // Seeded from the oracle resolver below; see `resync_model`.
            creation_model: 0,
            custom_mode: 0,
            eye_size: 0,
            lip_size: 0,
            eye_color: 0,
            hair_color: 0,
            face_type: 0,
            hair_style: 0,
            mood_type: 0,
            stats: [0; 8],
            region: 0,
            new_constitution: 0,
        };
        d.reset_stats_to_race_bases();
        d.allocate_default_bonus();
        d.region = create_packet_region(d.realm, d.race, d.class_id)
            .expect("OPEN_ORACLE StartupLocation must resolve Albion Briton Armsman");
        // B4: resolve the model from (race, gender) rather than a literal. The Albion stub used to
        // carry 0x07D1, which is not any race's eLivingModel.
        d.resync_model();
        d
    }

    /// Midgard Norseman Warrior starter (30 bonus points) — same bar as [`albion_briton_stub`].
    #[must_use]
    pub fn midgard_norseman_stub(name: impl Into<String>, slot: u8) -> Self {
        let mut d = Self {
            slot,
            name: name.into(),
            realm: 2,
            class_id: 22, // Warrior
            race: 5,      // Norseman
            gender: 0,
            creation_model: 0,
            custom_mode: 0,
            eye_size: 0,
            lip_size: 0,
            eye_color: 0,
            hair_color: 0,
            face_type: 0,
            hair_style: 0,
            mood_type: 0,
            stats: [0; 8],
            region: 0,
            new_constitution: 0,
        };
        d.reset_stats_to_race_bases();
        d.allocate_default_bonus();
        d.region = create_packet_region(d.realm, d.race, d.class_id)
            .expect("OPEN_ORACLE StartupLocation must resolve Midgard Norseman Warrior");
        // B4: resolve the model from (race, gender) rather than a literal. The Albion stub used to
        // carry 0x07D1, which is not any race's eLivingModel.
        d.resync_model();
        d
    }

    /// Hibernia Celt Hero starter (30 bonus points) — same bar as [`albion_briton_stub`].
    #[must_use]
    pub fn hibernia_celt_stub(name: impl Into<String>, slot: u8) -> Self {
        let mut d = Self {
            slot,
            name: name.into(),
            realm: 3,
            class_id: 44, // Hero
            race: 9,      // Celt
            gender: 0,
            creation_model: 0,
            custom_mode: 0,
            eye_size: 0,
            lip_size: 0,
            eye_color: 0,
            hair_color: 0,
            face_type: 0,
            hair_style: 0,
            mood_type: 0,
            stats: [0; 8],
            region: 0,
            new_constitution: 0,
        };
        d.reset_stats_to_race_bases();
        d.allocate_default_bonus();
        d.region = create_packet_region(d.realm, d.race, d.class_id)
            .expect("OPEN_ORACLE StartupLocation must resolve Hibernia Celt Hero");
        // B4: resolve the model from (race, gender) rather than a literal. The Albion stub used to
        // carry 0x07D1, which is not any race's eLivingModel.
        d.resync_model();
        d
    }

    /// Realm-parameterised starter used by the live create falsifier (1/2/3).
    #[must_use]
    pub fn stub_for_realm(realm: u8, name: impl Into<String>, slot: u8) -> Self {
        match realm {
            2 => Self::midgard_norseman_stub(name, slot),
            3 => Self::hibernia_celt_stub(name, slot),
            _ => Self::albion_briton_stub(name, slot),
        }
    }

    /// Lowest realm-local slot 0..9 not occupied in `occupied_slots` (from CharacterOverview).
    #[must_use]
    pub fn first_free_slot(occupied_slots: impl IntoIterator<Item = u8>) -> Option<u8> {
        let mut used = [false; 10];
        for s in occupied_slots {
            if let Some(slot) = used.get_mut(usize::from(s)) {
                *slot = true;
            }
        }
        used.iter().position(|&u| !u).map(|i| i as u8)
    }

    /// Race base stats from `GlobalConstants.STARTING_STATS_DICT` (OPEN_ORACLE).
    ///
    /// HalfOgre and the three Minotaur rows are **live** in both local oracles (DOLSharp and
    /// the SoloDAoC fork) — an earlier comment claimed they were commented-out lines, which a
    /// mechanical row-by-row diff against both `GlobalConstants.cs` files refuted (2026-08-23,
    /// 21/21 rows match). Upstream OpenDAoC's validator KeyNotFound's those races; ours does not.
    #[must_use]
    pub fn starting_stats_for_race(race: u8) -> Option<[u8; 8]> {
        // Delegates to `starting_stats` — the single authority for this table, transcribed once
        // from DOLSharp GlobalConstants.cs. This wrapper keeps the historical call sites; the
        // data lives in one place so two copies that agreed today cannot disagree tomorrow.
        if race == 0 || race >= 22 {
            return None;
        }
        Some(crate::starting_stats::STARTING_STATS[race as usize])
    }

    /// Progressive cost of `above` points on one stat. Delegates to
    /// `crate::starting_stats::spent_on` — same rule, one owner. Returns -1 for negative
    /// input (the historical contract of this wrapper).
    #[must_use]
    pub fn progressive_cost(above: i32) -> i32 {
        if above < 0 {
            return -1;
        }
        i32::try_from(crate::starting_stats::spent_on(above.max(0) as u32)).unwrap_or(i32::MAX)
    }

    /// Points spent above race bases at level 1 (class level-bonus loop never runs).
    /// Returns `None` if any stat is below its race floor or the race is unknown.
    #[must_use]
    pub fn distributed_points(stats: &[u8; 8], race: u8) -> Option<i32> {
        let bases = Self::starting_stats_for_race(race)?;
        let mut points = 0_i32;
        for i in 0..8 {
            let above = i32::from(stats[i]) - i32::from(bases[i]);
            if above < 0 {
                return None;
            }
            points += Self::progressive_cost(above);
        }
        Some(points)
    }

    #[must_use]
    pub fn points_spent(&self) -> Option<i32> {
        Self::distributed_points(&self.stats, self.race)
    }

    #[must_use]
    pub fn points_valid(&self) -> bool {
        self.points_spent() == Some(POINT_DISTRIBUTION_BUDGET)
    }

    /// Reset stats to race bases (0 bonus spent). Caller must re-allocate before Continué.
    pub fn reset_stats_to_race_bases(&mut self) {
        if let Some(bases) = Self::starting_stats_for_race(self.race) {
            self.stats = bases;
            self.new_constitution = bases[STAT_CON];
        }
    }

    /// Spend exactly 30 bonus points: +10 on STR, DEX, CON (wire indices 0,1,2).
    /// Each +10 costs 10 under the progressive rule; 3 × 10 = 30.
    pub fn allocate_default_bonus(&mut self) {
        self.reset_stats_to_race_bases();
        for idx in [STAT_STR, STAT_DEX, STAT_CON] {
            self.stats[idx] = self.stats[idx].saturating_add(10);
        }
        self.new_constitution = self.stats[STAT_CON];
        debug_assert!(self.points_valid());
    }

    /// Try to raise one wire-order stat by 1 if the progressive cost still fits in the budget.
    pub fn try_increment_stat(&mut self, wire_index: usize) -> bool {
        if wire_index >= 8 {
            return false;
        }
        let Some(bases) = Self::starting_stats_for_race(self.race) else {
            return false;
        };
        let cur = self.stats[wire_index];
        if cur == u8::MAX {
            return false;
        }
        let mut next = self.stats;
        next[wire_index] = cur + 1;
        let Some(pts) = Self::distributed_points(&next, self.race) else {
            return false;
        };
        if pts > POINT_DISTRIBUTION_BUDGET {
            return false;
        }
        // Also reject if the single-stat above would make cost jump past remaining:
        // distributed_points already sums; budget check is enough.
        let _ = bases;
        self.stats = next;
        self.new_constitution = self.stats[STAT_CON];
        true
    }

    /// Lower one wire-order stat by 1, not below race base.
    pub fn try_decrement_stat(&mut self, wire_index: usize) -> bool {
        if wire_index >= 8 {
            return false;
        }
        let Some(bases) = Self::starting_stats_for_race(self.race) else {
            return false;
        };
        if self.stats[wire_index] <= bases[wire_index] {
            return false;
        }
        self.stats[wire_index] -= 1;
        self.new_constitution = self.stats[STAT_CON];
        true
    }

    /// Apply a newly chosen race: refresh bases and re-spend the default 30.
    pub fn set_race(&mut self, race: u8) {
        self.race = race;
        self.allocate_default_bonus();
        // Male-only races (oracle RACE_GENDER_CONSTRAINTS_DICT).
        if matches!(race, 19..=21) {
            self.gender = 0;
        }
        if let Some(r) = create_packet_region(self.realm, self.race, self.class_id) {
            self.region = r;
        }
        self.resync_model();
    }

    /// Set gender (0 male / 1 female) and keep the model in step.
    ///
    /// B4: gender used to be assigned directly at the call sites, which left `creation_model`
    /// holding the other gender's value.
    pub fn set_gender(&mut self, gender: u8) {
        self.gender = gender & 1;
        self.resync_model();
    }

    /// Current source size adapter derived from the model bits.
    ///
    /// Retail and DOLSharp both treat an unset size mask as Average, so a freshly constructed
    /// draft displays `Average` without inventing an extra model flag.
    #[must_use]
    pub const fn creation_size(&self) -> u8 {
        match self.creation_model & CREATION_MODEL_SIZE_MASK {
            0x0800 => CREATION_SIZE_SHORT,
            0x1800 => CREATION_SIZE_TALL,
            _ => CREATION_SIZE_AVERAGE,
        }
    }

    /// Apply one source `height_text` selection to the encoded creation model.
    ///
    /// `Average` uses DOLSharp's explicit `eSize.Average` value. The separate
    /// [`Self::reset_creation_size`] preserves the retail all-zero default when the form's
    /// Default button is pressed.
    pub fn set_creation_size(&mut self, size: u8) -> bool {
        let size_bits = match size {
            CREATION_SIZE_SHORT => 0x0800,
            CREATION_SIZE_AVERAGE => 0x1000,
            CREATION_SIZE_TALL => 0x1800,
            _ => return false,
        };
        let Some(model) = crate::career::race_model(self.race, self.gender) else {
            self.creation_model = 0;
            return false;
        };
        self.creation_model = model | size_bits;
        true
    }

    /// Restore the neutral retail Average representation with no size bits set.
    pub fn reset_creation_size(&mut self) {
        self.creation_model = crate::career::race_model(self.race, self.gender).unwrap_or(0);
    }

    /// Re-resolve the race/gender body while retaining the selected, model-owned Size bits.
    ///
    /// B4: the model was set once by the realm stub constructor and never updated, so changing
    /// race or gender shipped a stale model on the wire. An unresolvable pair (unknown race, or a
    /// female Minotaur, which has no model in the oracle) leaves the model at 0 so
    /// [`Self::model_matches_identity`] refuses the encode instead of inventing one.
    pub fn resync_model(&mut self) {
        let size_bits = self.creation_model & CREATION_MODEL_SIZE_MASK;
        self.creation_model =
            crate::career::race_model(self.race, self.gender).map_or(0, |model| model | size_bits);
    }

    /// Whether `creation_model` is the oracle model for the draft's current `(race, gender)`.
    ///
    /// Encoding must refuse when this is false — that is the B4 falsifier's teeth.
    #[must_use]
    pub fn model_matches_identity(&self) -> bool {
        crate::career::race_model(self.race, self.gender)
            .is_some_and(|m| (self.creation_model & CREATION_MODEL_ID_MASK) == m)
    }

    #[must_use]
    pub fn wire_slot(&self) -> u8 {
        let realm = self.realm.max(1);
        self.slot
            .saturating_add(realm.saturating_sub(1).saturating_mul(10))
    }

    #[must_use]
    pub fn race_gender_byte(&self) -> u8 {
        (self.race & 0x1F) | ((self.gender & 1) << 7)
    }
}

/// The four source sliders at their no-op centre position.  This is kept next to packet creation
/// instead of scattered as `0x44` literals: a source custom record with a discrete choice must
/// carry these values, while a historical mode-zero record must remain byte-for-byte untouched.
fn neutral_facial_morph_bytes() -> (u8, u8) {
    use crate::customization::{
        with_facial_morph_tick, FacialMorphSlot, FACIAL_MORPH_NEUTRAL_TICK,
    };

    let neutral = FACIAL_MORPH_NEUTRAL_TICK;
    let (eye_size, lip_size) = with_facial_morph_tick(0, 0, FacialMorphSlot::Nose, neutral);
    let (eye_size, lip_size) =
        with_facial_morph_tick(eye_size, lip_size, FacialMorphSlot::Eyes, neutral);
    let (eye_size, lip_size) =
        with_facial_morph_tick(eye_size, lip_size, FacialMorphSlot::LipsOrEars, neutral);
    with_facial_morph_tick(eye_size, lip_size, FacialMorphSlot::JawOrChin, neutral)
}

/// Encode a v1126+ CharacterCreateRequest **payload** (after the packet header).
#[must_use]
pub fn encode_create_request_1126(draft: &CharacterCreateDraft, op: CreateOp) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(128);
    w.u8(draft.wire_slot());
    pascal_string_int_le(&mut w, &draft.name);
    // Constant seen after the name in 1125+ captures / oracle Skip(4) comment.
    w.bytes(&[0x18, 0x00, 0x00, 0x00]);
    // A draft whose fields were set field-by-field can still be carrying mode 0, which would tell
    // the server to ignore every appearance byte that follows. Derive it rather than trust it; an
    // explicit 2 or 3 (customise / auto-config) is left alone. `has_custom_appearance` includes
    // EyeSize/LipSize, not only the five mesh/texture-selection bytes.
    let custom_mode = if draft.custom_mode == 0 && draft.has_custom_appearance() {
        1
    } else {
        draft.custom_mode
    };
    // Public draft fields are intentionally available to the UI/tests, so defend the packet
    // boundary too: a caller that chose Face/Hair field-by-field but never touched a morph slider
    // must not accidentally encode four source-left sliders.  Explicit modes keep zero intact,
    // because mode 1/2/3 is what makes an all-left selection meaningful on the wire.
    let (eye_size, lip_size) = if draft.custom_mode == 0
        && draft.eye_size == 0
        && draft.lip_size == 0
        && !draft.customization().is_default()
    {
        neutral_facial_morph_bytes()
    } else {
        (draft.eye_size, draft.lip_size)
    };
    w.u8(custom_mode);
    w.u8(eye_size);
    w.u8(lip_size);
    w.u8(draft.eye_color);
    w.u8(draft.hair_color);
    w.u8(draft.face_type);
    w.u8(draft.hair_style);
    w.bytes(&[0, 0, 0]); // skip 3
    w.u8(draft.mood_type);
    w.bytes(&[0; 9]); // skip 9
    w.u8(op as u8);
    w.u8(0); // CustomizeType (unused on create)
    w.bytes(&[0, 0]); // rest of supposed int
                      // Empty location / class name / race name — each is a 5-byte empty int-pascal (`01 00 00 00 00`).
    empty_pascal_int_le(&mut w);
    empty_pascal_int_le(&mut w);
    empty_pascal_int_le(&mut w);
    // 1126+: class + realm (no level byte)
    w.u8(draft.class_id);
    w.u8(draft.realm);
    w.u8(draft.race_gender_byte());
    w.u16_le(draft.creation_model);
    for s in draft.stats {
        w.u8(s);
    }
    // 1126+: Skip(16*2 + 4*2) equipment, then Region byte, Skip(3), NewConstitution
    // (OPEN_ORACLE CreationCharacterData1124). Pre-1126 had Region *before* stats — we target 1126+.
    w.bytes(&[0; 40]);
    w.u8(draft.region);
    w.bytes(&[0; 3]);
    w.u8(draft.new_constitution);
    w.into_bytes()
}

/// Tutorial ClientRegionID (`StartupLocations.TUTORIAL_REGIONID`).
const TUTORIAL_CLIENT_REGION: u8 = 27;

/// OPEN_ORACLE `StartupLocation.xml` wildcard rows (RaceID=0, ClassID=0) — the rows advanced
/// classes like Armsman (no per-class row) actually match. Full race/class matrix is larger;
/// when no wildcard fits, [`create_packet_region`] returns None (provenance gap — do not invent).
///
/// Columns: (Region, RealmID, ClientRegionID).
const STARTUP_WILDCARDS: &[(u16, u8, u8)] = &[
    (27, 1, 27), // Albion tutorial
    (1, 1, 0),   // Albion classic → Camelot Hills / Cotswold
    (27, 2, 27), // Midgard tutorial
    (100, 2, 0), // Midgard classic
    (27, 3, 27), // Hibernia tutorial
    (200, 3, 0), // Hibernia classic
];

/// Region byte for a 1126+ CharacterCreateRequest from OPEN_ORACLE StartupLocations.
///
/// Prefers non-tutorial (ClientRegionID ≠ 27). When the chosen row's ClientRegionID is 0
/// (classic wildcard), returns that row's destination `Region` so the wire value is not 0.
#[must_use]
pub fn create_packet_region(realm: u8, _race: u8, _class_id: u8) -> Option<u8> {
    // Wildcards (RaceID=0, ClassID=0) match any race/class for the realm — same filter as
    // StartupLocations.GetAllStartupLocationForCharacter for ClassID/RaceID zero rows.
    let classic = STARTUP_WILDCARDS
        .iter()
        .copied()
        .filter(|&(_, realm_id, client_region)| {
            realm_id == realm && client_region != TUTORIAL_CLIENT_REGION
        })
        .max_by_key(|&(_, _, client_region)| client_region);
    let picked = classic.or_else(|| {
        STARTUP_WILDCARDS
            .iter()
            .copied()
            .filter(|&(_, realm_id, _)| realm_id == realm)
            .max_by_key(|&(_, _, client_region)| client_region)
    })?;
    let (dest_region, _, client_region) = picked;
    if client_region != 0 {
        Some(client_region)
    } else {
        u8::try_from(dest_region).ok()
    }
}

fn pascal_string_int_le(w: &mut PacketWriter, s: &str) {
    let bytes: Vec<u8> = s.chars().map(|c| c as u8).collect();
    w.u32_le((bytes.len() + 1) as u32);
    w.bytes(&bytes);
    w.u8(0);
}

fn empty_pascal_int_le(w: &mut PacketWriter) {
    // len=1 including NUL → five bytes; mirrors oracle `Skip(5)` on empty strings.
    w.bytes(&[0x01, 0x00, 0x00, 0x00, 0x00]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::PacketReader;

    #[test]
    fn progressive_cost_examples_from_spec() {
        assert_eq!(CharacterCreateDraft::progressive_cost(10), 10);
        assert_eq!(CharacterCreateDraft::progressive_cost(16), 23);
        assert_eq!(CharacterCreateDraft::progressive_cost(15), 20);
        assert_eq!(CharacterCreateDraft::progressive_cost(11), 12);
    }

    #[test]
    fn default_stub_spends_exactly_thirty() {
        let d = CharacterCreateDraft::albion_briton_stub("Testchar", 0);
        assert_eq!(d.stats, [70, 70, 70, 60, 60, 60, 60, 60]);
        assert_eq!(d.new_constitution, 70);
        assert_eq!(d.points_spent(), Some(30));
        assert!(d.points_valid());
    }

    #[test]
    fn mid_hib_stubs_stamp_realm_and_spend_thirty() {
        let m = CharacterCreateDraft::midgard_norseman_stub("Norse", 0);
        assert_eq!(m.realm, 2);
        assert_eq!(m.race, 5);
        assert_eq!(m.class_id, 22);
        assert_ne!(m.region, 0);
        assert!(m.points_valid());

        let h = CharacterCreateDraft::hibernia_celt_stub("Celt", 0);
        assert_eq!(h.realm, 3);
        assert_eq!(h.race, 9);
        assert_eq!(h.class_id, 44);
        assert_ne!(h.region, 0);
        assert!(h.points_valid());

        assert_eq!(CharacterCreateDraft::stub_for_realm(1, "A", 0).realm, 1);
        assert_eq!(CharacterCreateDraft::stub_for_realm(2, "B", 0).realm, 2);
        assert_eq!(CharacterCreateDraft::stub_for_realm(3, "C", 0).realm, 3);
    }

    #[test]
    fn zero_bonus_is_rejected_by_points_rule() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Testchar", 0);
        d.reset_stats_to_race_bases();
        assert_eq!(d.points_spent(), Some(0));
        assert!(!d.points_valid());
    }

    #[test]
    fn increment_respects_budget() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Testchar", 0);
        // Already at 30 — further increments must fail.
        assert!(!d.try_increment_stat(STAT_STR));
        d.reset_stats_to_race_bases();
        assert!(d.try_increment_stat(STAT_STR));
        assert_eq!(d.stats[STAT_STR], 61);
        assert_eq!(d.points_spent(), Some(1));
    }

    #[test]
    fn create_payload_starts_with_wire_slot_and_name() {
        let d = CharacterCreateDraft::albion_briton_stub("Testchar", 0);
        let body = encode_create_request_1126(&d, CreateOp::Create);
        assert_eq!(body[0], 0, "Albion slot 0 → wire 0");
        let mut r = PacketReader::new(&body[1..]);
        assert_eq!(r.pascal_string_int_le().unwrap(), "Testchar");
        assert_eq!(&body[1 + 4 + 9..1 + 4 + 9 + 4], &[0x18, 0x00, 0x00, 0x00]);
    }

    /// What the player accepted on screen is what the server must store.
    ///
    /// `CharacterCreateRequestHandler` copies the seven appearance bytes onto the character only
    /// when `CustomMode` reads 1. A draft carrying a look with mode 0 therefore renders one
    /// character on the create screen and saves a different one — the appearance bytes are on the
    /// wire, and the server ignores every one of them.
    #[test]
    fn a_customised_draft_tells_the_server_to_keep_the_look() {
        // slot(1) + name-pascal(4-byte LE length + bytes + NUL) + const(4) → CustomMode, then the
        // seven look bytes with eye/lip size in front.
        let at = |name: &str| 1 + 4 + name.len() + 1 + 4;

        let plain = CharacterCreateDraft::albion_briton_stub("Plainjane", 0);
        let body = encode_create_request_1126(&plain, CreateOp::Create);
        assert_eq!(
            body[at("Plainjane")],
            0,
            "an uncustomised draft must not claim to be customised"
        );

        let mut fancy = CharacterCreateDraft::albion_briton_stub("Plainjane", 0);
        fancy.set_customization(crate::customization::Customization {
            eye_color: 3,
            hair_color: 4,
            face_type: 5,
            hair_style: 6,
            mood_type: 7,
        });
        let body = encode_create_request_1126(&fancy, CreateOp::Create);
        let i = at("Plainjane");
        assert_eq!(body[i], 1, "a look on the wire needs CustomMode 1");
        assert_eq!(&body[i + 3..i + 7], &[3, 4, 5, 6], "eye, hair, face, style");

        // Setting the fields without going through the setter must not defeat it: the encoder
        // derives the mode rather than trusting a field a caller can leave stale.
        let mut raw = CharacterCreateDraft::albion_briton_stub("Plainjane", 0);
        raw.face_type = 5;
        let body = encode_create_request_1126(&raw, CreateOp::Create);
        assert_eq!(body[i], 1, "a look set field-by-field still has to survive");

        // The four facial sliders live in the two bytes before the five discrete look values.
        // This is the regression that matters for the authored morphology controls: modifying
        // only a slider must still force CustomMode, or DOL stores the uncustomised defaults.
        let mut morph_only = CharacterCreateDraft::albion_briton_stub("Plainjane", 0);
        morph_only.set_facial_morph_tick(crate::customization::FacialMorphSlot::Nose, 8);
        morph_only.custom_mode = 0; // prove the encoder, not the setter, owns this invariant.
        let body = encode_create_request_1126(&morph_only, CreateOp::Create);
        assert_eq!(body[i], 1, "a morph-only look must survive CustomMode");
        assert_eq!(body[i + 1], 0x84, "Nose is EyeSize's high nibble");
        assert_eq!(
            body[i + 2],
            0x44,
            "untouched sliders are explicitly seeded at source-neutral tick four"
        );
        assert_eq!(
            morph_only.facial_morph_tick(crate::customization::FacialMorphSlot::Eyes),
            4,
            "changing Nose must preserve the neighbouring Eyes slider"
        );

        assert_eq!(
            &body[i + 1..i + 3],
            &[0x84, 0x44],
            "the wire packet retains all four source slider positions"
        );

        // An explicit mode is a deliberate choice (2 = customise, 3 = auto-config) and is kept.
        let mut explicit = fancy.clone();
        explicit.custom_mode = 2;
        let body = encode_create_request_1126(&explicit, CreateOp::Create);
        assert_eq!(body[i], 2, "an explicit CustomMode must not be overwritten");
    }

    /// The draft's look and its five fields are one value read two ways; they cannot drift.
    #[test]
    fn the_draft_look_round_trips() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Rt", 0);
        let want = crate::customization::Customization {
            eye_color: 1,
            hair_color: 2,
            face_type: 3,
            hair_style: 4,
            mood_type: 5,
        };
        d.set_customization(want);
        assert_eq!(d.customization(), want);
        assert!(CharacterCreateDraft::albion_briton_stub("Rt", 0)
            .customization()
            .is_default());
    }

    #[test]
    fn midgard_slot_offsets_by_ten() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Norse", 2);
        d.realm = 2;
        assert_eq!(d.wire_slot(), 12);
        let body = encode_create_request_1126(&d, CreateOp::Create);
        assert_eq!(body[0], 12);
    }

    #[test]
    fn operation_byte_is_create_one() {
        let d = CharacterCreateDraft::albion_briton_stub("Testchar", 0);
        let body = encode_create_request_1126(&d, CreateOp::Create);
        // slot(1) + name-pascal(4+9) + const(4) + face(7) + skip3 + mood + skip9 = 38 → op at [38]
        let op_at = 1 + 4 + (d.name.len() + 1) + 4 + 7 + 3 + 1 + 9;
        assert_eq!(op_at, 38);
        assert_eq!(
            body.get(op_at).copied(),
            Some(CreateOp::Create as u8),
            "hex={}",
            body.iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    /// **Ledger B5.** A delete carries operation `3` and an **empty** name.
    ///
    /// `OPEN_ORACLE` `CharacterCreateRequestHandler._HandlePacket1124`: `case 3` acts only when
    /// `CharName` is empty, so a delete carrying a name is silently ignored by the server — the
    /// character survives and the client shows no error. Both halves are asserted because either one
    /// alone produces a packet that looks fine and does nothing.
    ///
    /// Version-routing matters here and is asserted too. The **pre-1124** handler switches on magic
    /// values (`Delete = 0x12345678`); the 1124+ path this client targets uses 1/2/3. CAER targets
    /// 1126+, so a byte of `0x78` would be reading the wrong branch of that file — a documented trap.
    #[test]
    fn delete_request_carries_operation_three_and_an_empty_name() {
        let d = CharacterCreateDraft::for_delete(1, 4);
        assert!(d.name.is_empty(), "the empty name IS the delete operation");
        let body = encode_create_request_1126(&d, CreateOp::Delete);

        // Wire slot first, then the empty int-pascal name.
        assert_eq!(body[0], d.wire_slot());
        assert_eq!(
            &body[1..5],
            &1u32.to_le_bytes(),
            "an empty int-pascal string still writes its length"
        );

        // slot(1) + name-pascal(4 + len + 1) + const(4) + face(7) + skip3 + mood(1) + skip9
        let op_at = 1 + 4 + (d.name.len() + 1) + 4 + 7 + 3 + 1 + 9;
        assert_eq!(
            body.get(op_at).copied(),
            Some(3),
            "1124+ delete is operation 3, not the pre-1124 magic 0x12345678"
        );
        assert_eq!(CreateOp::Delete as u8, 3);
        assert_ne!(
            body.get(op_at).copied(),
            Some(0x78),
            "0x78 would be the low byte of the pre-1124 magic — wrong branch of the oracle"
        );

        // A create of the same draft differs in exactly that byte, which is what makes the
        // discriminator load-bearing rather than incidental.
        let create = encode_create_request_1126(&d, CreateOp::Create);
        assert_eq!(create.len(), body.len());
        let diffs: Vec<usize> = (0..body.len()).filter(|i| body[*i] != create[*i]).collect();
        assert_eq!(
            diffs,
            vec![op_at],
            "create and delete of one draft must differ only in the operation byte"
        );
    }

    /// The slot a delete names is the one the player selected, per realm.
    ///
    /// The server resolves the target as `CharacterSlot + Realm * 100`, so the slot byte is the whole
    /// payload of this packet: a wrong one deletes a different character, irreversibly, with no error.
    #[test]
    fn delete_names_the_selected_slot_in_its_realm() {
        for (realm, slot) in [(1u8, 0u8), (1, 9), (2, 3), (3, 7)] {
            let d = CharacterCreateDraft::for_delete(realm, slot);
            assert_eq!(d.realm, realm);
            assert_eq!(d.slot, slot);
            let body = encode_create_request_1126(&d, CreateOp::Delete);
            assert_eq!(
                body[0],
                d.wire_slot(),
                "realm {realm} slot {slot} must encode its own wire slot"
            );
        }
        // Distinct realms must not collide on the wire, or Delete in one realm hits another's slot.
        let mut seen = std::collections::HashSet::new();
        for realm in 1..=3u8 {
            for slot in 0..10u8 {
                assert!(
                    seen.insert(CharacterCreateDraft::for_delete(realm, slot).wire_slot()),
                    "realm {realm} slot {slot} collides with an earlier wire slot"
                );
            }
        }
    }

    #[test]
    fn female_sets_high_bit_on_race_byte() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Testchar", 0);
        d.gender = 1;
        assert_eq!(d.race_gender_byte(), 0x81);
    }

    #[test]
    fn continue_packet_carries_ui_chosen_name_and_race() {
        // Anti-fake: Continué must encode the draft the form collected, not a hardcoded stub.
        let mut d = CharacterCreateDraft::albion_briton_stub("UiChosen", 3);
        d.set_race(4);
        d.class_id = 5;
        d.realm = 1;
        let body = encode_create_request_1126(&d, CreateOp::Create);
        assert_eq!(body[0], 3);
        let mut r = PacketReader::new(&body[1..]);
        assert_eq!(r.pascal_string_int_le().unwrap(), "UiChosen");
        assert_eq!(d.race_gender_byte() & 0x1F, 4);
        assert!(d.points_valid());
    }

    #[test]
    fn minotaur_forces_male() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Mino", 0);
        d.gender = 1;
        d.set_race(19);
        assert_eq!(d.gender, 0);
    }

    #[test]
    fn first_free_slot_skips_occupied() {
        assert_eq!(CharacterCreateDraft::first_free_slot([]), Some(0));
        assert_eq!(CharacterCreateDraft::first_free_slot([0]), Some(1));
        assert_eq!(CharacterCreateDraft::first_free_slot([0, 1, 3]), Some(2));
        assert_eq!(
            CharacterCreateDraft::first_free_slot(0..10),
            None,
            "full plate has no free slot"
        );
    }

    #[test]
    fn albion_classic_region_is_cotswold_from_oracle() {
        // OPEN_ORACLE StartupLocation.xml: RealmID=1, RaceID=0, ClassID=0, ClientRegionID=0 → Region 1.
        assert_eq!(create_packet_region(1, 1, 2), Some(1));
        let d = CharacterCreateDraft::albion_briton_stub("Reg", 0);
        assert_eq!(d.region, 1);
        let body = encode_create_request_1126(&d, CreateOp::Create);
        // Region sits after creation_model(2) + stats(8) + equip(40).
        let mut r = PacketReader::new(&body[1..]);
        let _ = r.pascal_string_int_le().unwrap();
        r.bytes(4).unwrap(); // 0x18 const
        r.bytes(7).unwrap(); // face block
        r.bytes(3).unwrap();
        r.u8().unwrap(); // mood
        r.bytes(9).unwrap();
        r.u8().unwrap(); // op
        r.u8().unwrap(); // customize type
        r.bytes(2).unwrap();
        for _ in 0..3 {
            let _ = r.pascal_string_int_le().unwrap();
        }
        r.u8().unwrap(); // class
        r.u8().unwrap(); // realm
        r.u8().unwrap(); // race/gender
        r.u16_le().unwrap(); // model
        r.bytes(8).unwrap(); // stats
        r.bytes(40).unwrap(); // equip
        assert_eq!(r.u8().unwrap(), 1, "1126+ Region byte");
    }
}

#[cfg(test)]
mod b4_model_sync_tests {
    use super::*;
    use crate::codec::PacketReader;

    /// B4 falsifier: the model on the wire must always match the draft's current identity.
    #[test]
    fn changing_race_updates_the_wire_model() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Tester", 0);
        assert_eq!(d.creation_model, 32, "Briton male");
        assert!(d.model_matches_identity());
        d.set_race(16); // HalfOgre
        assert_eq!(
            d.creation_model, 1008,
            "HalfOgre male — stale Briton model would be 32"
        );
        assert!(d.model_matches_identity());
    }

    /// Gender must move the model too; it used to be assigned around the resolver.
    #[test]
    fn changing_gender_updates_the_wire_model() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Tester", 0);
        d.set_gender(1);
        assert_eq!(d.creation_model, 35, "BritonFemale");
        assert!(d.model_matches_identity());
        d.set_gender(0);
        assert_eq!(d.creation_model, 32, "BritonMale");
    }

    /// `character_customize.xml`'s Size row owns bits 11–12 of the model word, not a spare
    /// starting-stat byte. The model still identifies the current race/gender after selecting a
    /// size or changing gender.
    #[test]
    fn source_size_bits_encode_and_survive_identity_changes() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Tester", 0);
        assert_eq!(d.creation_size(), CREATION_SIZE_AVERAGE);

        assert!(d.set_creation_size(CREATION_SIZE_SHORT));
        assert_eq!(d.creation_model, 32 | 0x0800);
        assert_eq!(d.creation_size(), CREATION_SIZE_SHORT);
        assert!(d.model_matches_identity());

        d.set_gender(1);
        assert_eq!(
            d.creation_model,
            35 | 0x0800,
            "gender replaces only body bits"
        );
        assert!(d.model_matches_identity());

        assert!(d.set_creation_size(CREATION_SIZE_TALL));
        assert_eq!(d.creation_model, 35 | 0x1800);
        assert_eq!(d.creation_size(), CREATION_SIZE_TALL);
        assert!(d.model_matches_identity());

        // The model is not merely draft state: 1126+ writes this exact little-endian word after
        // class / realm / race-gender.  Pin the outbound packet too so a later encoder cleanup
        // cannot silently strip the source Size bits.
        let body = encode_create_request_1126(&d, CreateOp::Create);
        let mut r = PacketReader::new(&body[1..]);
        let _ = r.pascal_string_int_le().unwrap();
        r.bytes(4 + 7 + 3 + 1 + 9 + 1 + 1 + 2 + 15 + 3).unwrap();
        assert_eq!(r.u16_le().unwrap(), 35 | 0x1800);

        assert!(d.set_creation_size(CREATION_SIZE_AVERAGE));
        assert_eq!(d.creation_model, 35 | 0x1000);
        d.reset_creation_size();
        assert_eq!(
            d.creation_model, 35,
            "Default restores retail's neutral Average bits"
        );
        assert_eq!(d.creation_size(), CREATION_SIZE_AVERAGE);
        assert!(!d.set_creation_size(4));
    }

    /// Every realm stub seeds a model consistent with its own identity.
    #[test]
    fn every_realm_stub_seeds_a_consistent_model() {
        for realm in 1..=3u8 {
            let d = CharacterCreateDraft::stub_for_realm(realm, "Tester", 0);
            assert!(
                d.model_matches_identity(),
                "realm {realm} stub model {} does not match race {} gender {}",
                d.creation_model,
                d.race,
                d.gender
            );
            assert_ne!(d.creation_model, 0, "realm {realm} stub resolved no model");
        }
    }

    /// A female Minotaur has no oracle model, so it must fail closed rather than keep the male one.
    #[test]
    fn female_minotaur_fails_closed_instead_of_keeping_a_stale_model() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Tester", 0);
        d.set_race(19); // Korazh — set_race forces gender male
        assert_eq!(d.gender, 0);
        assert_eq!(d.creation_model, 1395);
        // Force the illegal pairing the way a mis-wired UI would.
        d.gender = 1;
        d.resync_model();
        assert_eq!(
            d.creation_model, 0,
            "no female Minotaur model may be invented"
        );
        assert!(!d.model_matches_identity(), "encode must refuse this draft");
    }

    /// Proves the check is not vacuous: a deliberately desynced model is rejected.
    #[test]
    fn model_identity_check_rejects_a_desynced_draft() {
        let mut d = CharacterCreateDraft::albion_briton_stub("Tester", 0);
        assert!(d.model_matches_identity());
        d.creation_model = 1008; // HalfOgre model on a Briton draft
        assert!(
            !d.model_matches_identity(),
            "a mismatched model must be detectable, or the guard is decorative"
        );
    }
}
