//! Dungeon zone discovery and loading (REQ-015 / System 5 Stream B).
//!
//! Surface zones are read via `terrain.pcx` + `fixtures.csv`. The **281** classic dungeon
//! zones ship neither — their content is `dungeon.chunk` / `.place` / `.prop` inside
//! `datNNN.mpk`. This module is the world-side entry: classify, enumerate, and load those
//! zones so `fixtures > 0` is an observable fact rather than a surface-zone assumption.

use std::io;
use std::path::{Path, PathBuf};

use caer_assets::dungeon::{dat_is_dungeon_mpk, DungeonZone};

/// MEAS-010 primary `zones/` partition (client-authored directories, not playable coverage).
///
/// Counted on an **unmodified 1.127 client tree**, which is the world CAER is aiming at. These
/// were previously 400/97, which is the Eden freeshard's world: that tree adds `zones/zone276`
/// *and* ships different data for `zone490`, so it classifies Surface there and Dungeon on an
/// unmodified tree. It is not retail plus one zone — it is a different world, and `rustdaoc`
/// already refuses to treat it as fidelity evidence ("modified shard asset tree detected").
///
/// Nothing at runtime reads these; the client enumerates whatever tree the player has, so a
/// freeshard's extra zones load normally. They are the denominator for coverage measurement only,
/// which is why the tree they were counted on matters. A side effect worth keeping: pointing
/// `CAER_CLIENT` at a modified tree now fails `zone_denominator` loudly instead of quietly grading
/// against a different world.
pub const MEAS010_SURFACE: usize = 96;
pub const MEAS010_DUNGEON: usize = 281;
pub const MEAS010_SKYCITY: usize = 17;
pub const MEAS010_INTERIOR: usize = 5;
pub const MEAS010_PRIMARY_ZONES: usize = 399;

/// Authored zone class from client `datNNN.mpk` membership (MEAS-010 partition).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZoneClass {
    Surface,
    Dungeon,
    Skycity,
    /// City/interior packs without terrain or dungeon/skycity chunk files, or missing dat.
    Interior,
}

impl ZoneClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Surface => "surface",
            Self::Dungeon => "dungeon",
            Self::Skycity => "skycity",
            Self::Interior => "interior",
        }
    }
}

/// One primary `zones/zoneNNN` directory with its authored class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrimaryZoneEntry {
    pub zone_id: u16,
    pub class: ZoneClass,
}

/// Parallel zone-data roots the client ships (same layout the renderer scans).
pub fn zone_roots(client_root: &Path) -> Vec<PathBuf> {
    [
        "zones",
        "frontiers/zones",
        "phousing/zones",
        "Tutorial/zones",
    ]
    .iter()
    .map(|s| client_root.join(s))
    .collect()
}

/// Primary overworld `zones/` tree — MEAS-010's dungeon denominator lives here.
pub fn primary_zone_root(client_root: &Path) -> PathBuf {
    client_root.join("zones")
}

/// Resolve `zoneNNN` / `ZONE008`-style directory under a zone root.
pub fn find_zone_dir(zone_root: &Path, zone_id: u16) -> Option<PathBuf> {
    let needle = format!("zone{zone_id:03}");
    let rd = std::fs::read_dir(zone_root).ok()?;
    for e in rd.flatten() {
        let name = e.file_name();
        let s = name.to_string_lossy();
        if s.eq_ignore_ascii_case(&needle) && e.path().is_dir() {
            return Some(e.path());
        }
    }
    None
}

/// Locate `datNNN.mpk` for a zone id across all zone roots.
pub fn find_dat_mpk(client_root: &Path, zone_id: u16) -> Option<PathBuf> {
    let dat_name = format!("dat{zone_id:03}.mpk");
    for root in zone_roots(client_root) {
        let Some(zdir) = find_zone_dir(&root, zone_id) else {
            continue;
        };
        // Case-insensitive dat file match (Windows client trees vary).
        if let Ok(rd) = std::fs::read_dir(&zdir) {
            for e in rd.flatten() {
                let n = e.file_name();
                if n.to_string_lossy().eq_ignore_ascii_case(&dat_name) {
                    return Some(e.path());
                }
            }
        }
    }
    None
}

/// Classify a zone from its `datNNN.mpk` member names.
pub fn classify_dat_mpk(dat_mpk: &[u8]) -> io::Result<ZoneClass> {
    let names = caer_assets::list_names_bytes(dat_mpk)?;
    let lower: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    if lower
        .iter()
        .any(|n| n == "dungeon.chunk" || n == "dungeon.place")
    {
        return Ok(ZoneClass::Dungeon);
    }
    if lower
        .iter()
        .any(|n| n == "skycity.chunk" || n == "skycity.place")
    {
        return Ok(ZoneClass::Skycity);
    }
    if lower.iter().any(|n| n == "terrain.pcx") {
        return Ok(ZoneClass::Surface);
    }
    Ok(ZoneClass::Interior)
}

fn parse_zone_dir_id(name: &str) -> Option<u16> {
    if !name.to_ascii_lowercase().starts_with("zone") {
        return None;
    }
    let digits: String = name.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn dat_path_in_zone_dir(zdir: &Path, zone_id: u16) -> Option<PathBuf> {
    let dat_name = format!("dat{zone_id:03}.mpk");
    let rd = std::fs::read_dir(zdir).ok()?;
    rd.flatten().find_map(|f| {
        let n = f.file_name();
        if n.to_string_lossy().eq_ignore_ascii_case(&dat_name) {
            Some(f.path())
        } else {
            None
        }
    })
}

/// Enumerate every primary `zones/` directory (deduped, sorted) with MEAS-010 class.
///
/// Frontier/housing/tutorial trees are not folded into this count. Missing dat → Interior.
pub fn enumerate_primary_zone_classes(client_root: &Path) -> io::Result<Vec<PrimaryZoneEntry>> {
    let mut entries = Vec::new();
    let root = primary_zone_root(client_root);
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Ok(entries);
    };
    for e in rd.flatten() {
        let zdir = e.path();
        if !zdir.is_dir() {
            continue;
        }
        let Some(zid) = parse_zone_dir_id(&e.file_name().to_string_lossy()) else {
            continue;
        };
        let class = match dat_path_in_zone_dir(&zdir, zid) {
            Some(dat) => match std::fs::read(&dat) {
                Ok(bytes) => classify_dat_mpk(&bytes).unwrap_or(ZoneClass::Interior),
                Err(_) => ZoneClass::Interior,
            },
            None => ZoneClass::Interior,
        };
        entries.push(PrimaryZoneEntry {
            zone_id: zid,
            class,
        });
    }
    entries.sort_by_key(|e| e.zone_id);
    entries.dedup_by_key(|e| e.zone_id);
    Ok(entries)
}

/// Counts `(surface, dungeon, skycity, interior)` over a primary-zone enumeration.
#[must_use]
pub fn meas010_partition_counts(entries: &[PrimaryZoneEntry]) -> (usize, usize, usize, usize) {
    let mut surface = 0usize;
    let mut dungeon = 0usize;
    let mut skycity = 0usize;
    let mut interior = 0usize;
    for e in entries {
        match e.class {
            ZoneClass::Surface => surface += 1,
            ZoneClass::Dungeon => dungeon += 1,
            ZoneClass::Skycity => skycity += 1,
            ZoneClass::Interior => interior += 1,
        }
    }
    (surface, dungeon, skycity, interior)
}

/// Load and decode a dungeon zone. Errors if the dat is missing or is not a dungeon archive.
pub fn load_dungeon_zone(client_root: &Path, zone_id: u16) -> io::Result<DungeonZone> {
    let path = find_dat_mpk(client_root, zone_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("dat{zone_id:03}.mpk not found under client zone roots"),
        )
    })?;
    let bytes = std::fs::read(&path)?;
    if !dat_is_dungeon_mpk(&bytes)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("zone {zone_id} dat is not a dungeon.chunk/.place archive"),
        ));
    }
    DungeonZone::from_dat_mpk(&bytes)
}

/// Enumerate every dungeon zone id under the client's **primary** `zones/` tree
/// (deduped, sorted).
///
/// A zone counts as dungeon iff its dat lists `dungeon.chunk` or `dungeon.place`.
/// Spec denominator: **281** (MEAS-010). Frontier/housing/tutorial trees are not folded
/// into that count — they may carry additional or overlapping zone ids.
pub fn enumerate_dungeon_zone_ids(client_root: &Path) -> io::Result<Vec<u16>> {
    Ok(enumerate_primary_zone_classes(client_root)?
        .into_iter()
        .filter(|e| e.class == ZoneClass::Dungeon)
        .map(|e| e.zone_id)
        .collect())
}

/// Deterministic pick of `k` distinct ids from a sorted slice (LCG, no `rand` dep).
///
/// Selection method for the Stream B falsifier: seed `0xCAE5_5D5B` ("CAER S5B"), walk an LCG
/// and take modulo remaining pool (Fisher–Yates partial shuffle).
pub fn pick_random_zone_ids(ids: &[u16], k: usize, seed: u64) -> Vec<u16> {
    if ids.is_empty() || k == 0 {
        return Vec::new();
    }
    let mut pool: Vec<u16> = ids.to_vec();
    let mut state = seed;
    let n = k.min(pool.len());
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        // Numerical Recipes LCG
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        let j = i + (state as usize % (pool.len() - i));
        pool.swap(i, j);
        out.push(pool[i]);
    }
    out
}

/// Stream B falsifier seed — recorded so the three dungeon IDs are reproducible.
pub const FALSIFIER_SEED: u64 = 0xCAE5_5D5B;

/// Named dungeon used for the single-zone content evidence (zone 21 — Tomb of Mithra).
pub const NAMED_DUNGEON_ZONE: u16 = 21;
pub const NAMED_DUNGEON_LABEL: &str = "Tomb of Mithra";

/// Wave1 Lane D canonical classic dungeon transition (region 20 — not region 51).
///
/// Packet/region id **20**, zone **19** (Stonehenge Barrows). Surface return is region 1.
/// Authoritative product constants live in `caer_render::dungeon_mesh` (`CANONICAL_DUNGEON_*`);
/// these mirror the zone-side facts for world-layer tests.
pub const CANONICAL_CLASSIC_DUNGEON_REGION: u16 = 20;
pub const CANONICAL_CLASSIC_DUNGEON_ZONE: u16 = 19;
pub const CANONICAL_CLASSIC_DUNGEON_LABEL: &str = "Stonehenge Barrows";

/// Realm or dungeon category derived from **authoritative tables only**.
///
/// - Realm tokens come from OpenDAoC / Zones.xml display names (`zone_names`).
/// - `DarknessFalls` is the exact Zones.xml name `"Darkness Falls"`.
/// - `TaskProcedural` is membership in SoloDAoC `TaskDungeonMission` skin-region arrays
///   (`OPEN_ORACLE`). Those arrays are region ids that match classic zone ids for these skins.
/// - Everything else is [`DungeonRealmOrCategory::Unresolved`]. Geography is not a mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DungeonRealmOrCategory {
    Albion,
    Midgard,
    Hibernia,
    DarknessFalls,
    TaskProcedural,
    Unresolved,
}

impl DungeonRealmOrCategory {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Albion => "Albion",
            Self::Midgard => "Midgard",
            Self::Hibernia => "Hibernia",
            Self::DarknessFalls => "DarknessFalls",
            Self::TaskProcedural => "TaskProcedural",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Task-dungeon skin zone/region ids from SoloDAoC
/// `GameServer/quests/Missions/TaskDungeonMission.cs` (`OPEN_ORACLE`).
/// Sorted unique. Do not invent additional skins.
const TASK_DUNGEON_SKIN_ZONE_IDS: &[u16] = &[
    48, 256, 257, 258, 278, 279, 280, 281, 282, 283, 284, 285, 286, 287, 288, 289, 290, 291, 292,
    293, 294, 295, 296, 297, 298, 300, 301, 302, 303, 304, 305, 306, 307, 308, 309, 310, 311, 312,
    313, 314, 315, 316, 317, 318, 319, 320, 321, 322, 323, 324, 379, 382, 383, 386, 387, 388, 400,
    401, 402, 403, 404, 405, 406, 407, 408, 409, 410, 411, 412, 413, 414, 415, 416, 417, 418, 419,
    420, 421, 422, 423, 424, 427, 428, 431, 432, 441, 444, 445, 448, 449, 450, 451, 452, 453, 454,
    455, 456, 457, 458, 459, 460, 461, 462, 463, 464, 465, 466, 467, 468, 469, 471, 472, 473, 474,
    477, 478, 481, 482, 485, 486,
];

/// True when `zone_id` is an `OPEN_ORACLE` task-dungeon skin id.
#[must_use]
pub fn is_task_dungeon_skin_zone(zone_id: u16) -> bool {
    TASK_DUNGEON_SKIN_ZONE_IDS.binary_search(&zone_id).is_ok()
}

/// Derive realm/category from Zones.xml names and the task-skin id table.
///
/// Precedence: Darkness Falls name, then task-skin id, then name-embedded realm token.
/// Never infers Albion from "Stonehenge Barrows" or similar unlabeled classic dungeons.
#[must_use]
pub fn dungeon_realm_or_category(zone_id: u16) -> DungeonRealmOrCategory {
    if let Some(name) = crate::zone_name(zone_id) {
        if name.eq_ignore_ascii_case("Darkness Falls") {
            return DungeonRealmOrCategory::DarknessFalls;
        }
    }
    if is_task_dungeon_skin_zone(zone_id) {
        return DungeonRealmOrCategory::TaskProcedural;
    }
    let Some(name) = crate::zone_name(zone_id) else {
        return DungeonRealmOrCategory::Unresolved;
    };
    let lower = name.to_ascii_lowercase();
    if lower.contains("albion") || lower.starts_with("alb ") {
        return DungeonRealmOrCategory::Albion;
    }
    if lower.contains("midgard") || lower.starts_with("mid ") {
        return DungeonRealmOrCategory::Midgard;
    }
    if lower.contains("hibernia") || lower.starts_with("hib ") {
        return DungeonRealmOrCategory::Hibernia;
    }
    DungeonRealmOrCategory::Unresolved
}

/// One MB2 Lane DNG representative with table provenance (not a playable-coverage claim).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DungeonRepresentative {
    pub zone_id: u16,
    pub region_id: u16,
    pub label: &'static str,
    pub realm_or_category: DungeonRealmOrCategory,
    pub provenance: &'static str,
}

/// Wave1 canonical slice. Zones.xml name has **no** realm token → category unresolved.
pub const REP_STONEHENGE_BARROWS: DungeonRepresentative = DungeonRepresentative {
    zone_id: 19,
    region_id: 20,
    label: CANONICAL_CLASSIC_DUNGEON_LABEL,
    realm_or_category: DungeonRealmOrCategory::Unresolved,
    provenance: "Zones.xml Name=\"Stonehenge Barrows\"; zone_offsets region 20. Realm unresolved (no realm token in name; Zones.Realm=0).",
};

/// Provenance: Zones.xml Name contains \"Albion\".
pub const REP_ALBION: DungeonRepresentative = DungeonRepresentative {
    zone_id: 59,
    region_id: 59,
    label: "Albion's Glashtin Forges",
    realm_or_category: DungeonRealmOrCategory::Albion,
    provenance: "Zones.xml Name=\"Albion's Glashtin Forges\" (zone_names). Not inferred from overworld adjacency.",
};

/// Provenance: Zones.xml Name contains \"Midgard\". Zone 58 is skycity (not in the 281).
pub const REP_MIDGARD: DungeonRepresentative = DungeonRepresentative {
    zone_id: 188,
    region_id: 188,
    label: "Midgard's Darkspire",
    realm_or_category: DungeonRealmOrCategory::Midgard,
    provenance: "Zones.xml Name=\"Midgard's Darkspire\" (zone_names). dat188 has dungeon.chunk/.place. Zone 58 Midgard's Underground Forest is skycity.chunk — not a classic dungeon archive.",
};

/// Provenance: Zones.xml Name contains \"Hibernia\". Zone 96 is skycity (not in the 281).
pub const REP_HIBERNIA: DungeonRepresentative = DungeonRepresentative {
    zone_id: 98,
    region_id: 98,
    label: "Hibernia's Darkspire",
    realm_or_category: DungeonRealmOrCategory::Hibernia,
    provenance: "Zones.xml Name=\"Hibernia's Darkspire\" (zone_names). dat098 has dungeon.chunk/.place. Zone 96 Hibernia's Underground Forest is skycity.chunk — not a classic dungeon archive.",
};

/// Provenance: Zones.xml Name is exactly \"Darkness Falls\".
pub const REP_DARKNESS_FALLS: DungeonRepresentative = DungeonRepresentative {
    zone_id: 249,
    region_id: 249,
    label: "Darkness Falls",
    realm_or_category: DungeonRealmOrCategory::DarknessFalls,
    provenance: "Zones.xml Name=\"Darkness Falls\" (zone_names). zone_offsets region 249.",
};

/// Provenance: SoloDAoC TaskDungeonMission.damp_cavern_long includes 278.
pub const REP_TASK_PROCEDURAL: DungeonRepresentative = DungeonRepresentative {
    zone_id: 278,
    region_id: 278,
    label: "Damp Cavern",
    realm_or_category: DungeonRealmOrCategory::TaskProcedural,
    provenance: "OPEN_ORACLE TaskDungeonMission.damp_cavern_long = {278,279,280,301}; Zones.xml Name=\"Damp Cavern\".",
};

/// Representatives this lane attempts to prove with real NIF geometry (not 281 playable).
pub const DUNGEON_REPRESENTATIVES: &[DungeonRepresentative] = &[
    REP_STONEHENGE_BARROWS,
    REP_ALBION,
    REP_MIDGARD,
    REP_HIBERNIA,
    REP_DARKNESS_FALLS,
    REP_TASK_PROCEDURAL,
];

#[cfg(test)]
mod tests {
    use super::*;
    use caer_assets::client_dep::{require_caer_client, skip_or_fail};

    #[test]
    fn pick_random_is_deterministic_for_recorded_seed() {
        let ids: Vec<u16> = (0..281).map(|i| 100 + i).collect();
        let a = pick_random_zone_ids(&ids, 3, FALSIFIER_SEED);
        let b = pick_random_zone_ids(&ids, 3, FALSIFIER_SEED);
        assert_eq!(a, b);
        assert_eq!(a.len(), 3);
        assert_eq!(a.iter().collect::<std::collections::HashSet<_>>().len(), 3);
    }

    /// Named dungeon loads with fixtures > 0 (geometry from dungeon.place/.prop).
    #[test]
    fn named_dungeon_tomb_of_mithra_loads_with_fixtures() {
        let Some(root) = require_caer_client("named_dungeon_tomb_of_mithra_loads_with_fixtures")
        else {
            return;
        };
        let zone = match load_dungeon_zone(&root, NAMED_DUNGEON_ZONE) {
            Ok(z) => z,
            Err(e) => {
                skip_or_fail(
                    "named_dungeon_tomb_of_mithra_loads_with_fixtures",
                    &format!("load zone {NAMED_DUNGEON_ZONE}: {e}"),
                );
                return;
            }
        };
        assert!(
            zone.fixture_count() > 0,
            "{NAMED_DUNGEON_LABEL} (zone {NAMED_DUNGEON_ZONE}) fixtures==0 — dungeon reader absent?"
        );
        assert!(
            !zone.chunks.is_empty(),
            "{NAMED_DUNGEON_LABEL}: dungeon.chunk palette empty"
        );
        assert_eq!(
            crate::zone_name(NAMED_DUNGEON_ZONE),
            Some(NAMED_DUNGEON_LABEL),
            "zone_names table must label the named dungeon"
        );
        let fx = zone.as_fixtures();
        assert_eq!(fx.len(), zone.fixture_count());
        assert!(fx.iter().any(|f| !f.filename.is_empty()));
        eprintln!(
            "named dungeon {NAMED_DUNGEON_LABEL} zone={NAMED_DUNGEON_ZONE}: \
             chunks={} places={} props={} fixtures={}",
            zone.chunks.len(),
            zone.places.len(),
            zone.props.len(),
            zone.fixture_count()
        );
    }

    /// Mandatory falsifier: three dungeons chosen by seeded RNG from the 281, each asserted
    /// independently. Selecting one and generalising is rejected by construction.
    #[test]
    fn random_three_dungeons_each_load_with_fixtures() {
        let Some(root) = require_caer_client("random_three_dungeons_each_load_with_fixtures")
        else {
            return;
        };
        let ids = match enumerate_dungeon_zone_ids(&root) {
            Ok(v) => v,
            Err(e) => {
                skip_or_fail(
                    "random_three_dungeons_each_load_with_fixtures",
                    &format!("enumerate: {e}"),
                );
                return;
            }
        };
        assert_eq!(
            ids.len(),
            281,
            "expected 281 dungeon zones (MEAS-010 / REQ-015), found {}",
            ids.len()
        );
        let sample = pick_random_zone_ids(&ids, 3, FALSIFIER_SEED);
        assert_eq!(sample.len(), 3);
        eprintln!(
            "Stream B falsifier seed=0x{FALSIFIER_SEED:08X} method=LCG-FisherYates sample={sample:?}"
        );
        for &zid in &sample {
            let zone = load_dungeon_zone(&root, zid).unwrap_or_else(|e| {
                panic!("dungeon zone {zid} failed to load: {e}");
            });
            assert!(
                zone.fixture_count() > 0,
                "dungeon zone {zid} fixtures==0 (chunks={}, places={}, props={})",
                zone.chunks.len(),
                zone.places.len(),
                zone.props.len()
            );
            eprintln!(
                "  zone {zid}: fixtures={} chunks={}",
                zone.fixture_count(),
                zone.chunks.len()
            );
        }
    }

    #[test]
    fn realm_or_category_is_table_derived_never_invented() {
        assert_eq!(
            dungeon_realm_or_category(CANONICAL_CLASSIC_DUNGEON_ZONE),
            DungeonRealmOrCategory::Unresolved,
            "Stonehenge Barrows must not be silently labeled Albion"
        );
        assert_eq!(
            dungeon_realm_or_category(NAMED_DUNGEON_ZONE),
            DungeonRealmOrCategory::Unresolved,
            "Tomb of Mithra has no realm token"
        );
        assert_eq!(
            dungeon_realm_or_category(REP_ALBION.zone_id),
            DungeonRealmOrCategory::Albion
        );
        assert_eq!(
            dungeon_realm_or_category(REP_MIDGARD.zone_id),
            DungeonRealmOrCategory::Midgard
        );
        assert_eq!(
            dungeon_realm_or_category(REP_HIBERNIA.zone_id),
            DungeonRealmOrCategory::Hibernia
        );
        assert_eq!(
            dungeon_realm_or_category(REP_DARKNESS_FALLS.zone_id),
            DungeonRealmOrCategory::DarknessFalls
        );
        assert_eq!(
            dungeon_realm_or_category(REP_TASK_PROCEDURAL.zone_id),
            DungeonRealmOrCategory::TaskProcedural
        );
        assert!(TASK_DUNGEON_SKIN_ZONE_IDS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(crate::zone_name(REP_ALBION.zone_id), Some(REP_ALBION.label));
        assert_eq!(
            crate::zone_region(REP_ALBION.zone_id),
            Some(REP_ALBION.region_id)
        );
        assert_eq!(
            crate::zone_region(REP_DARKNESS_FALLS.zone_id),
            Some(REP_DARKNESS_FALLS.region_id)
        );
        assert_eq!(
            crate::zone_name(REP_MIDGARD.zone_id),
            Some(REP_MIDGARD.label)
        );
        assert_eq!(
            crate::zone_name(REP_HIBERNIA.zone_id),
            Some(REP_HIBERNIA.label)
        );
        assert_eq!(
            crate::zone_region(REP_MIDGARD.zone_id),
            Some(REP_MIDGARD.region_id)
        );
        assert_eq!(
            crate::zone_region(REP_HIBERNIA.zone_id),
            Some(REP_HIBERNIA.region_id)
        );
    }

    #[test]
    fn meas010_primary_partition_matches_an_unmodified_tree() {
        let Some(root) =
            require_caer_client("meas010_primary_partition_matches_an_unmodified_tree")
        else {
            return;
        };
        let entries = enumerate_primary_zone_classes(&root).expect("enumerate primary zones");
        assert_eq!(
            entries.len(),
            MEAS010_PRIMARY_ZONES,
            "MEAS-010 primary zone directories"
        );
        let (surface, dungeon, skycity, interior) = meas010_partition_counts(&entries);
        assert_eq!(surface, MEAS010_SURFACE, "surface");
        assert_eq!(dungeon, MEAS010_DUNGEON, "dungeon");
        assert_eq!(skycity, MEAS010_SKYCITY, "skycity");
        assert_eq!(interior, MEAS010_INTERIOR, "interior");
        assert_eq!(
            surface + dungeon + skycity + interior,
            MEAS010_PRIMARY_ZONES
        );
        let dungeon_ids = enumerate_dungeon_zone_ids(&root).expect("dungeon ids");
        assert_eq!(dungeon_ids.len(), MEAS010_DUNGEON);
    }

    /// Classic region-20 dungeon (zone 19) must load via the dungeon reader — this is the
    /// content `terrain.pcx` cannot supply.
    #[test]
    fn classic_zone019_dungeon_reader_not_terrain() {
        let Some(root) = require_caer_client("classic_zone019_dungeon_reader_not_terrain") else {
            return;
        };
        let dat = match find_dat_mpk(&root, 19) {
            Some(p) => p,
            None => {
                skip_or_fail(
                    "classic_zone019_dungeon_reader_not_terrain",
                    "dat019.mpk missing",
                );
                return;
            }
        };
        let bytes = std::fs::read(&dat).expect("read dat019");
        assert!(
            caer_assets::terrain::ZoneTerrain::from_dat_mpk(&bytes).is_err(),
            "zone 19 must lack terrain.pcx — otherwise this is not the dungeon gap"
        );
        let zone = DungeonZone::from_dat_mpk(&bytes).expect("dungeon reader for zone 19");
        assert!(zone.fixture_count() > 0, "zone 19 dungeon fixtures==0");
    }
}
