//! Codex M1 + Lane D — dungeon-zone render fingerprint.
//!
//! Distinct from [`region_fingerprint`]: that driver correctly keeps **terrain** region 20 empty
//! (no `terrain.pcx`). This binary proves the **dungeon reader → real NIF / FixtureBox fallback**
//! path for classic zone 19 (Stonehenge Barrows / region 20), with a negative control that terrain
//! region 20 stays empty.
//!
//! ```bash
//! CAER_CLIENT=… cargo run -p caer-render --example dungeon_fingerprint
//! ```

use std::path::PathBuf;

use caer_render::dungeon_mesh::{
    append_dungeon_geometry_if_surface_empty, canonical_dungeon_seat, load_dungeon_fixture_boxes,
    scene_fingerprint, CANONICAL_DUNGEON_LABEL, CANONICAL_DUNGEON_ZONE,
};
use caer_render::terrain::{load_region, seam_blend, TerrainMesh};
use caer_world::dungeon_zones::{NAMED_DUNGEON_LABEL, NAMED_DUNGEON_ZONE};
use glam::Vec3;

fn main() {
    let Some(root) = std::env::var_os("CAER_CLIENT") else {
        eprintln!("FAIL CAER_CLIENT unset");
        std::process::exit(4);
    };
    let root = PathBuf::from(root);

    // Positive: named dungeon via box consumer (legacy M1 path still works).
    let (mithra, boxes) = match load_dungeon_fixture_boxes(&root, NAMED_DUNGEON_ZONE, Vec3::ZERO) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("FAIL load {NAMED_DUNGEON_LABEL} zone {NAMED_DUNGEON_ZONE}: {e}");
            std::process::exit(1);
        }
    };

    // Positive: classic zone 19 / region 20 — real NIF geometry.
    let seat = canonical_dungeon_seat();
    let mut mesh = TerrainMesh {
        origin: Vec3::new(seat[0], seat[1], 0.0),
        ..TerrainMesh::default()
    };
    let stats = append_dungeon_geometry_if_surface_empty(&mut mesh, &root, 20);
    let fp = scene_fingerprint(&mesh, 20, CANONICAL_DUNGEON_ZONE);

    // Negative: terrain path for region 20 must stay empty.
    let terrain20 = load_region(
        20,
        Vec3::new(seat[0], seat[1], 0.0),
        [0, 0],
        [100_000, 100_000],
        seam_blend(),
    );

    eprintln!(
        "dungeon_fingerprint: {NAMED_DUNGEON_LABEL}_boxes={} {CANONICAL_DUNGEON_LABEL}={fp:?} \
         stats={stats:?} terrain_region20=(zones={}, fixtures={}) boxes_sample={}",
        mithra.fixture_boxes,
        terrain20.zones_loaded,
        terrain20.fixtures.len(),
        boxes.len().min(3)
    );

    if mithra.fixture_boxes == 0 {
        eprintln!("FAIL {NAMED_DUNGEON_LABEL} fixture_boxes==0");
        std::process::exit(1);
    }
    if !stats.has_real_geometry() {
        eprintln!("FAIL zone 19 / region 20 produced no real NIF geometry: {stats:?}");
        std::process::exit(1);
    }
    if terrain20.zones_loaded != 0 || !terrain20.fixtures.is_empty() {
        eprintln!(
            "FAIL terrain region 20 unexpectedly non-empty — negative control broken: \
             zones={} fixtures={}",
            terrain20.zones_loaded,
            terrain20.fixtures.len()
        );
        std::process::exit(1);
    }

    eprintln!(
        "PASS dungeon_fingerprint — {CANONICAL_DUNGEON_LABEL} nif_instances={} nif_kinds={} \
         box_fallback={}; terrain region 20 still empty (negative control)",
        fp.model_instances, fp.model_kinds, fp.fixture_box_fallback
    );
}
