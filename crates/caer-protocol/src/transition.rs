//! Authoritative realm / character / region transition controller (System 3).
//!
//! Sol's blocking finding 2: these three decisions were scattered (`--region` once, session realm,
//! overview identity). This type owns them. Drivers (rustdaoc, scenarios) apply wire events and
//! UI picks here; they do not keep a parallel copy of realm/region/identity that can drift.
//!
//! A committed snapshot is **atomic** across realm, region, identity, origin, camera, and
//! terrain-dirty. `RegionChanged` alone does **not** reload terrain — that waits until a
//! PlayerPosition commits origin+camera with the pending region (Sol stale-origin blocker).

use crate::overview::{decode_overview_race_gender, fig3_gender_from_db, CharacterSummary};

/// What must be true after a **committed** region change (Sol's six-field list + camera).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionSnapshot {
    pub session_realm: u8,
    pub packet_region: u16,
    /// Single semantic latch: `(eRace, fig3_gender)`. `(0,0)` means unset.
    pub identity: (u8, u8),
    pub origin_xy: (i32, i32),
    /// Follow-camera anchor XY — committed with origin, never left on the previous zone alone.
    pub camera_xy: (i32, i32),
    /// Terrain reload required — caller must replace meshes, not merely re-request.
    pub terrain_dirty: bool,
}

/// Effects the driver must act on after a controller mutation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransitionEffects {
    /// Re-fire CharacterOverviewRequest with [`WorldTransition::realm`].
    pub request_overview: bool,
    /// Reload terrain for the new region/origin (replacement, not a no-op re-call).
    pub reload_terrain: bool,
    /// Rebuild Self_ avatar from race/gender.
    pub rebuild_avatar: bool,
}

/// Single owner of realm, selected-character identity, and region/origin for world entry.
#[derive(Debug, Clone)]
pub struct WorldTransition {
    /// Session overview realm byte (1 Albion / 2 Midgard / 3 Hibernia).
    realm: u8,
    /// Committed packet/world region id (0 = unset).
    region: u16,
    /// Region from 0xB7 not yet paired with an origin — terrain must not reload yet.
    pending_region: Option<u16>,
    /// Render origin XY (world units). Z is camera/time elsewhere.
    origin_xy: (i32, i32),
    /// Camera look-at / follow anchor — kept coherent with origin on commit.
    camera_xy: (i32, i32),
    /// Selected character identity `(eRace, fig3_gender)`. `None` until overview names them.
    identity: Option<(u8, u8)>,
    /// Character name from overview (for relogin / region-change survival checks).
    character_name: Option<String>,
    terrain_dirty: bool,
}

impl Default for WorldTransition {
    fn default() -> Self {
        Self {
            realm: 1,
            region: 0,
            pending_region: None,
            origin_xy: (0, 0),
            camera_xy: (0, 0),
            identity: None,
            character_name: None,
            terrain_dirty: false,
        }
    }
}

impl WorldTransition {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn realm(&self) -> u8 {
        self.realm
    }

    /// Committed region. While a 0xB7 is awaiting origin, this stays on the previous commit
    /// so terrain/camera never pair a new region with a stale origin.
    #[must_use]
    pub fn region(&self) -> u16 {
        self.region
    }

    /// Region id from the latest 0xB7 that has not yet committed with an origin.
    #[must_use]
    pub fn pending_region(&self) -> Option<u16> {
        self.pending_region
    }

    #[must_use]
    pub fn awaiting_origin(&self) -> bool {
        self.pending_region.is_some()
    }

    #[must_use]
    pub fn origin_xy(&self) -> (i32, i32) {
        self.origin_xy
    }

    #[must_use]
    pub fn camera_xy(&self) -> (i32, i32) {
        self.camera_xy
    }

    /// Race+gender once the overview (or an explicit set) named the character.
    #[must_use]
    pub fn identity(&self) -> Option<(u8, u8)> {
        self.identity
    }

    #[must_use]
    pub fn character_name(&self) -> Option<&str> {
        self.character_name.as_deref()
    }

    #[must_use]
    pub fn terrain_dirty(&self) -> bool {
        self.terrain_dirty
    }

    /// Committed snapshot for falsifiers — identity `(0,0)` when unset.
    #[must_use]
    pub fn snapshot(&self) -> TransitionSnapshot {
        TransitionSnapshot {
            session_realm: self.realm,
            packet_region: self.region,
            identity: self.identity.unwrap_or((0, 0)),
            origin_xy: self.origin_xy,
            camera_xy: self.camera_xy,
            terrain_dirty: self.terrain_dirty,
        }
    }

    /// Pre-world realm pick. Updates the session realm the overview request must carry.
    pub fn choose_realm(&mut self, realm: u8) -> TransitionEffects {
        let realm = realm.clamp(1, 3);
        self.realm = realm;
        TransitionEffects {
            request_overview: true,
            ..Default::default()
        }
    }

    /// Latch identity (+ optional starting region) from a character-select overview slot.
    pub fn select_character(&mut self, c: &CharacterSummary) -> TransitionEffects {
        let (race, e_gender) = decode_overview_race_gender(c.race_gender);
        let fig3 = fig3_gender_from_db(e_gender);
        let mut fx = TransitionEffects::default();
        let next = (race, fig3);
        if self.identity != Some(next) {
            self.identity = Some(next);
            fx.rebuild_avatar = true;
        }
        self.character_name = Some(c.name.clone());
        if c.region != 0 && self.region != u16::from(c.region) {
            // Character-select region is authoritative with the slot's known location — commit
            // origin stays until PlayerPosition; mark dirty so boot reload uses the slot region.
            self.region = u16::from(c.region);
            self.terrain_dirty = true;
            fx.reload_terrain = true;
        }
        fx
    }

    /// Wire RegionChanged (0xB7): park the new region until a PlayerPosition commits origin+camera.
    /// Does **not** set `reload_terrain` — that would load new-region meshes on a stale origin.
    pub fn apply_region_changed(&mut self, region_id: u16) -> TransitionEffects {
        self.pending_region = Some(region_id);
        TransitionEffects::default()
    }

    /// Authoritative feet position after (or without) a pending region change.
    ///
    /// - If a region is pending: commit region + origin + camera atomically and request terrain replace.
    /// - Otherwise: update origin + camera (teleport / zone seat) and request terrain replace.
    pub fn apply_teleport_origin(&mut self, x: f32, y: f32) -> TransitionEffects {
        let xy = (x as i32, y as i32);
        if let Some(rid) = self.pending_region.take() {
            self.region = rid;
        }
        self.origin_xy = xy;
        self.camera_xy = xy;
        self.terrain_dirty = true;
        TransitionEffects {
            reload_terrain: true,
            ..Default::default()
        }
    }

    /// Driver finished replacing terrain meshes for the current region/origin.
    pub fn clear_terrain_dirty(&mut self) {
        self.terrain_dirty = false;
    }

    /// Seed region once at boot (CLI `--region`) before any wire event. Marks terrain dirty.
    pub fn seed_region(&mut self, region: u16, origin_x: i32, origin_y: i32) {
        self.region = region;
        self.pending_region = None;
        self.origin_xy = (origin_x, origin_y);
        self.camera_xy = (origin_x, origin_y);
        self.terrain_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overview::CharacterSummary;
    use crate::region::{self, RegionChanged};

    fn summary(name: &str, race_gender: u8, region: u8, realm: u8) -> CharacterSummary {
        CharacterSummary {
            slot: 0,
            level: 50,
            name: name.into(),
            location: "Test".into(),
            class_name: "Test".into(),
            race_name: "Test".into(),
            region,
            class_id: 2,
            realm,
            stats: [60; 8],
            race_gender,
            ..Default::default()
        }
    }

    /// Mock driver: tracks resident terrain region so stale-origin reloads are observable.
    struct MockDriver {
        t: WorldTransition,
        /// Last region whose meshes were actually replaced (None = never loaded).
        terrain_resident: Option<u16>,
        camera: (i32, i32),
        origin: (i32, i32),
    }

    impl MockDriver {
        fn new() -> Self {
            Self {
                t: WorldTransition::new(),
                terrain_resident: None,
                camera: (0, 0),
                origin: (0, 0),
            }
        }

        fn apply_effects(&mut self, fx: TransitionEffects) {
            if fx.reload_terrain {
                assert!(
                    !self.t.awaiting_origin(),
                    "must not reload terrain while origin is pending"
                );
                let snap = self.t.snapshot();
                assert_eq!(
                    snap.origin_xy, snap.camera_xy,
                    "committed snapshot must keep origin and camera coherent"
                );
                self.terrain_resident = Some(snap.packet_region);
                self.origin = snap.origin_xy;
                self.camera = snap.camera_xy;
                self.t.clear_terrain_dirty();
            }
        }

        fn on_b7(&mut self, region_id: u16) {
            let body = region::encode(&RegionChanged {
                region_id,
                zone_skin_id: region_id.saturating_sub(1),
                cause: 1,
                server_id: 0x0C,
            });
            let dec = region::decode(&body).expect("0xB7 decode");
            let fx = self.t.apply_region_changed(dec.region_id);
            assert!(
                !fx.reload_terrain,
                "RegionChanged alone must not reload terrain (stale-origin bug)"
            );
            self.apply_effects(fx);
        }

        fn on_position(&mut self, x: f32, y: f32) {
            let fx = self.t.apply_teleport_origin(x, y);
            self.apply_effects(fx);
        }
    }

    #[test]
    fn choose_midgard_changes_session_realm() {
        let mut t = WorldTransition::new();
        assert_eq!(t.realm(), 1);
        let fx = t.choose_realm(2);
        assert_eq!(t.realm(), 2);
        assert!(fx.request_overview);
        let fx = t.choose_realm(3);
        assert_eq!(t.realm(), 3);
        assert!(fx.request_overview);
    }

    #[test]
    fn select_character_latches_identity_without_highlander_default() {
        let mut t = WorldTransition::new();
        assert!(t.identity().is_none(), "no product default before overview");
        // Lilillyn golden: 0x13 → race 3, eGender 1 → fig3 female (2)
        let fx = t.select_character(&summary("Lilillyn", 0x13, 1, 1));
        assert_eq!(t.identity(), Some((3, 2)));
        assert!(fx.rebuild_avatar);
        assert_eq!(t.character_name(), Some("Lilillyn"));
    }

    #[test]
    fn region_change_awaits_origin_before_terrain_reload() {
        let mut t = WorldTransition::new();
        t.select_character(&summary("Lilillyn", 0x13, 1, 1));
        t.clear_terrain_dirty();
        let before = t.snapshot();
        let fx = t.apply_region_changed(20);
        assert!(!fx.reload_terrain);
        assert!(t.awaiting_origin());
        assert_eq!(
            t.region(),
            before.packet_region,
            "committed region unchanged"
        );
        assert_eq!(t.pending_region(), Some(20));
        // Under-threshold / delayed position still commits atomically.
        let fx = t.apply_teleport_origin(400_000.0, 400_000.0);
        assert!(fx.reload_terrain);
        assert!(!t.awaiting_origin());
        let after = t.snapshot();
        assert_eq!(after.packet_region, 20);
        assert_eq!(after.origin_xy, (400_000, 400_000));
        assert_eq!(after.camera_xy, after.origin_xy);
        assert_eq!(after.identity, before.identity);
    }

    #[test]
    fn six_field_snapshot_covers_sol_list() {
        let mut t = WorldTransition::new();
        t.choose_realm(2);
        t.select_character(&summary("Norse", 0x05, 100, 2));
        t.apply_region_changed(165);
        t.apply_teleport_origin(100.0, 200.0);
        let s = t.snapshot();
        assert_eq!(s.session_realm, 2);
        assert_eq!(s.packet_region, 165);
        assert_ne!(s.identity, (0, 0));
        assert_eq!(s.origin_xy, (100, 200));
        assert_eq!(s.camera_xy, (100, 200));
        assert!(s.terrain_dirty);
    }

    /// Encoded 0xB7 + position sequences: surface→dungeon→RvR, including delayed / missing /
    /// under-threshold position. Old terrain must not stay paired with a new committed region.
    #[test]
    fn b7_plus_position_commits_coherent_snapshot() {
        let mut d = MockDriver::new();
        d.t.choose_realm(1);
        d.t.select_character(&summary("Hero", 0x01, 1, 1));
        d.apply_effects(TransitionEffects {
            reload_terrain: true,
            ..Default::default()
        });
        assert_eq!(d.terrain_resident, Some(1));

        // Surface → dungeon: 0xB7 without position must leave old terrain resident.
        d.on_b7(20);
        assert_eq!(
            d.terrain_resident,
            Some(1),
            "missing position: old terrain stays"
        );
        assert_eq!(d.t.region(), 1);
        // Delayed under-threshold-style seat (driver always commits after pending).
        d.on_position(10.0, 20.0);
        assert_eq!(d.terrain_resident, Some(20));
        assert_eq!(d.origin, (10, 20));
        assert_eq!(d.camera, (10, 20));

        // Dungeon → RvR.
        d.on_b7(163);
        assert_eq!(
            d.terrain_resident,
            Some(20),
            "pending: old dungeon still resident"
        );
        d.on_position(50.0, 60.0);
        assert_eq!(d.terrain_resident, Some(163));
        let snap = d.t.snapshot();
        assert_eq!(snap.packet_region, 163);
        assert_eq!(snap.origin_xy, snap.camera_xy);
        assert_eq!(snap.identity.0, 1);
    }

    /// Three realms share one controller; zone-kind ids latch without inventing identity.
    #[test]
    fn three_realms_and_zone_kinds_share_controller() {
        for realm in [1_u8, 2, 3] {
            let mut t = WorldTransition::new();
            t.choose_realm(realm);
            assert_eq!(t.realm(), realm);
            let rg = match realm {
                2 => 0x05,
                3 => 0x09,
                _ => 0x01,
            };
            t.select_character(&summary("Hero", rg, 1, realm));
            let id = t.identity().expect("overview must name identity");
            t.clear_terrain_dirty();
            for region in [1_u16, 20, 163] {
                t.apply_region_changed(region);
                assert!(t.awaiting_origin());
                let fx = t.apply_teleport_origin(10.0 * f32::from(region), 20.0);
                assert!(fx.reload_terrain);
                assert_eq!(t.region(), region);
                assert_eq!(t.identity(), Some(id), "identity survives region {region}");
                assert_eq!(t.realm(), realm, "realm survives region {region}");
                assert_eq!(t.camera_xy(), t.origin_xy());
                t.clear_terrain_dirty();
            }
        }
    }

    /// Falsifier: Highlander Female must be unreachable as a product default.
    #[test]
    fn no_highlander_female_product_default() {
        let t = WorldTransition::new();
        assert!(t.identity().is_none());
        assert!(t.character_name().is_none());
        let s = t.snapshot();
        assert_eq!(s.identity, (0, 0));
    }

    /// Sol HIGH 3: named startup selection (all three realms) latches identity without a UI click.
    #[test]
    fn named_startup_selection_latches_identity_all_realms() {
        // Non-Highlander packs: Alb Briton♂, Mid Norseman♂, Hib Celt♂.
        for (realm, rg, name) in [
            (1_u8, 0x01, "Albion"),
            (2, 0x05, "Midgard"),
            (3, 0x09, "Hibernia"),
        ] {
            let mut t = WorldTransition::new();
            t.choose_realm(realm);
            // --char path: SelectCharacter + apply_identity_from_summary equivalent.
            let fx = t.select_character(&summary(name, rg, 1, realm));
            assert!(fx.rebuild_avatar, "realm {realm} must rebuild avatar");
            let id = t
                .identity()
                .expect("named startup must resolve identity before render");
            assert_ne!(id, (3, 2), "must not silently become Highlander Female");
            assert_eq!(t.character_name(), Some(name));
        }
    }
}
