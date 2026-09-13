//! Behavioral dungeon product transition (B2). Not `include_str!` of rustdaoc.
//!
//! Skip-append must fail `representative_pass`. Canonical region 20 with append must pass
//! `load_product_terrain` geometry (real NIF instances).

use std::path::Path;

use caer_render::dungeon_mesh::{
    load_product_terrain, product_reload_surface_dungeon, CANONICAL_DUNGEON_REGION,
    CANONICAL_DUNGEON_ZONE,
};
use glam::Vec3;

fn client_root() -> Option<std::path::PathBuf> {
    std::env::var_os("CAER_CLIENT").map(std::path::PathBuf::from)
}

/// Falsifier `skip_append_cannot_pass_product_transition`.
#[test]
fn skip_append_is_red_and_append_is_green() {
    let Some(root) = client_root() else {
        panic!("CAER_CLIENT unset — this dungeon product transition test requires real assets");
    };
    let root = Path::new(&root);
    let red = product_reload_surface_dungeon(
        root,
        CANONICAL_DUNGEON_REGION,
        CANONICAL_DUNGEON_ZONE,
        false,
    );
    assert!(
        !red.representative_pass(),
        "skip-append must be red: {red:?}"
    );
    let green = product_reload_surface_dungeon(
        root,
        CANONICAL_DUNGEON_REGION,
        CANONICAL_DUNGEON_ZONE,
        true,
    );
    assert!(
        green.representative_pass(),
        "canonical append must PASS: {green:?}"
    );
}

/// Falsifier `load_product_terrain_is_the_rustdaoc_path`.
#[test]
fn load_product_terrain_appends_dungeon_geometry() {
    let Some(root) = client_root() else {
        panic!("CAER_CLIENT unset — this dungeon product transition test requires real assets");
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
        "product terrain for region 20 must append real NIF geometry: {stats:?} models={}",
        mesh.models.len()
    );
    assert!(
        !mesh.surfaces.is_empty(),
        "dungeon collision index must be present"
    );
}
