//! MEAS-010 generated zone denominator (LANE ZON first slice).
//!
//! Two columns, never collapsed:
//! 1. client-authored render coverage
//! 2. server-authoritative playable transition endpoints
//!
//! A census is not playable coverage. Skip-append and empty decode must paint playable red.

use std::path::Path;

use caer_render::dungeon_census::{
    playable_from_product_reload, playable_transition_verdict, write_zone_denominator_tsv,
    zone_denominator, CensusCollision, CensusDecode, CensusVerdict,
};
use caer_render::dungeon_mesh::{
    load_product_terrain, product_reload_surface_dungeon, CANONICAL_DUNGEON_REGION,
    CANONICAL_DUNGEON_ZONE,
};
use caer_world::dungeon_zones::{
    enumerate_primary_zone_classes, meas010_partition_counts, ZoneClass, MEAS010_DUNGEON,
    MEAS010_INTERIOR, MEAS010_PRIMARY_ZONES, MEAS010_SKYCITY, MEAS010_SURFACE,
};
use glam::Vec3;

fn client_root() -> Option<std::path::PathBuf> {
    std::env::var_os("CAER_CLIENT").map(std::path::PathBuf::from)
}

fn generated_tsv_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/generated/zone_denominator.tsv")
}

#[test]
fn empty_decode_playable_column_is_red() {
    let (v, reason) = playable_transition_verdict(
        ZoneClass::Dungeon,
        CensusDecode::Ok,
        0,
        0,
        0,
        CensusCollision::NotEvaluated,
        Some(20),
        true,
    );
    assert_eq!(v, CensusVerdict::Fail);
    assert_eq!(reason, Some("ok_empty"));
}

#[test]
fn skip_append_playable_column_is_red() {
    let Some(root) = client_root() else {
        eprintln!("skip: CAER_CLIENT unset");
        return;
    };
    let red = product_reload_surface_dungeon(
        &root,
        CANONICAL_DUNGEON_REGION,
        CANONICAL_DUNGEON_ZONE,
        false,
    );
    let (v, reason) = playable_from_product_reload(&red);
    assert_eq!(
        v,
        CensusVerdict::Fail,
        "skip-append must paint playable red: {red:?}"
    );
    assert_eq!(reason, Some("append_disabled"));
    assert!(!red.representative_pass());
}

#[test]
fn mixed_boxes_cannot_be_281_playable() {
    let (v, reason) = playable_transition_verdict(
        ZoneClass::Dungeon,
        CensusDecode::Ok,
        8,
        7,
        1,
        CensusCollision::SurfacesPresent,
        Some(20),
        true,
    );
    assert_eq!(v, CensusVerdict::Fail);
    assert_eq!(reason, Some("mixed_boxes"));
}

/// A wrong count here usually means the wrong *tree*, not a classification bug.
///
/// MEAS-010 counts zone **directories** under `zones/`. An unmodified 1.127 tree ships 399. The
/// lab's Eden install reports 400 because it adds `zones/zone276` and also ships different data for
/// `zone490` (Surface there, Dungeon on an unmodified tree), so its partition is 97/281 against
/// 96/281. Each tree's own `zones/zones.mpk : zones.dat` settles which one you are on: unmodified
/// declares 491 `[zoneNNN]` sections, Eden 499, with 110/246/248/253/276/299/490/493 only there.
///
/// Do **not** reconcile a mismatch by copying zones between trees.
fn short_count_hint(root: &Path, got: usize) -> String {
    format!(
        "enumerated {got} primary zone directories under {}, MEAS-010 expects \
         {MEAS010_PRIMARY_ZONES}. This counts directories, not classifications, so the usual cause \
         is CAER_CLIENT pointing at a modified shard tree: 399 is an unmodified 1.127 client, 400 \
         is the Eden install (adds zone276, reclassifies zone490). Check the tree before touching \
         the constant.",
        root.display()
    )
}

#[test]
fn meas010_partition_reconciles() {
    let Some(root) = client_root() else {
        eprintln!("skip: CAER_CLIENT unset");
        return;
    };
    let entries = enumerate_primary_zone_classes(&root).expect("enumerate");
    assert_eq!(
        entries.len(),
        MEAS010_PRIMARY_ZONES,
        "{}",
        short_count_hint(&root, entries.len())
    );
    let (surface, dungeon, skycity, interior) = meas010_partition_counts(&entries);
    assert_eq!(
        (surface, dungeon, skycity, interior),
        (
            MEAS010_SURFACE,
            MEAS010_DUNGEON,
            MEAS010_SKYCITY,
            MEAS010_INTERIOR
        )
    );
}

#[test]
fn zone_denominator_writes_split_columns_and_rejects_census_as_playable() {
    let Some(root) = client_root() else {
        eprintln!("skip: CAER_CLIENT unset");
        return;
    };
    let rows = zone_denominator(&root).expect("denominator");
    assert_eq!(
        rows.len(),
        MEAS010_PRIMARY_ZONES,
        "{}",
        short_count_hint(&root, rows.len())
    );
    let dungeon_rows: Vec<_> = rows
        .iter()
        .filter(|r| r.class == ZoneClass::Dungeon)
        .collect();
    assert_eq!(dungeon_rows.len(), MEAS010_DUNGEON);

    let playable_dungeons = dungeon_rows.iter().filter(|r| r.playable_pass()).count();
    assert!(
        playable_dungeons < MEAS010_DUNGEON,
        "census/denominator must not claim 281 playable (playable_dungeons={playable_dungeons})"
    );

    for r in &rows {
        if r.placement_count == 0 && r.class == ZoneClass::Dungeon {
            assert!(!r.playable_pass(), "zone {} ok_empty playable PASS", r.zone);
            assert_eq!(r.playable_reason, Some("ok_empty"));
        }
        if r.real_nif_count == 0 && r.fallback_count > 0 {
            assert!(
                !r.playable_pass(),
                "zone {} boxes-only playable PASS",
                r.zone
            );
        }
        if r.fallback_count > 0 && r.class == ZoneClass::Dungeon {
            assert!(!r.playable_pass(), "mixed_boxes cannot be playable: {r:?}");
            assert_eq!(r.playable_reason, Some("mixed_boxes"));
        }
        if r.class == ZoneClass::Skycity || r.class == ZoneClass::Interior {
            assert!(!r.playable_pass());
            assert!(!r.render_pass());
        }
    }

    let tsv = write_zone_denominator_tsv(&generated_tsv_path(), &rows);
    tsv.expect("write zone_denominator.tsv");
    let text = std::fs::read_to_string(generated_tsv_path()).expect("read tsv");
    assert!(text.contains("render_coverage"));
    assert!(text.contains("playable_transition"));
    assert!(text.contains("census is NOT playable coverage"));
    assert!(
        text.lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with("zone\t"))
            .count()
            >= MEAS010_PRIMARY_ZONES
    );
}

#[test]
fn load_product_terrain_canonical_is_not_a_281_playable_claim() {
    let Some(root) = client_root() else {
        eprintln!("skip: CAER_CLIENT unset");
        return;
    };
    let seat = caer_render::dungeon_mesh::dungeon_product_seat(&root, CANONICAL_DUNGEON_ZONE);
    let origin = Vec3::new(seat[0], seat[1], 0.0);
    let (mesh, stats) = load_product_terrain(
        &root,
        CANONICAL_DUNGEON_REGION,
        origin,
        [seat[0] as i32 - 12_000, seat[1] as i32 - 12_000],
        [seat[0] as i32 + 12_000, seat[1] as i32 + 12_000],
    );
    assert!(
        stats.model_instances > 0 && stats.has_real_geometry(),
        "canonical product terrain must append real NIF: {stats:?}"
    );
    assert!(!mesh.surfaces.is_empty());
    // FixtureBox leftovers keep the playable column red even when the product path has NIFs.
    if stats.fixture_boxes > 0 {
        let green = product_reload_surface_dungeon(
            &root,
            CANONICAL_DUNGEON_REGION,
            CANONICAL_DUNGEON_ZONE,
            true,
        );
        let (v, reason) = playable_from_product_reload(&green);
        assert_eq!(v, CensusVerdict::Fail);
        assert_eq!(reason, Some("mixed_boxes"));
    }
}
