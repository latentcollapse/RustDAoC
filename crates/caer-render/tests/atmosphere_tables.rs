//! Atmosphere tables are load-bearing (MS-05 / leg 11 falsifier).
//!
//! With `CAER_ATMOSPHERE_TABLES=0`, `load_region` must not emit an ocean plane and must publish
//! zero lighting — proving the old hardcoded shader constants are gone and tables drive the look.

use std::sync::Mutex;

use glam::Vec3;

use caer_render::atmosphere::{published, tables_enabled};
use caer_render::terrain::{client_root, load_region, seam_blend};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Albion (region 1) world box used by existing render tests — zones sit near 560k, not origin.
const ALBION_MIN: [i32; 2] = [560_000, 500_000];
const ALBION_MAX: [i32; 2] = [630_000, 570_000];

#[test]
fn client_tables_feed_ocean_lights_materials_and_sky_light() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("CAER_ATMOSPHERE_TABLES");
    assert!(tables_enabled());

    let root = client_root();
    if !root.join("zones").is_dir() {
        eprintln!("skip: no CAER_CLIENT zones at {}", root.display());
        return;
    }

    let origin = Vec3::new(561_400.0, 511_410.0, 2000.0);
    let mesh = load_region(1, origin, ALBION_MIN, ALBION_MAX, seam_blend());
    assert!(
        mesh.zones_loaded > 0,
        "expected region-1 zones under {}",
        root.display()
    );

    let atm = &mesh.atmosphere;
    assert!(
        !atm.lights.is_empty(),
        "lights.csv must be read (got 0 lights)"
    );
    assert!(
        atm.sky_light.ambient_amount > 0.0 && atm.sky_light.dynamic_amount > 0.0,
        "sky lights_and_fog must feed ambient/dynamic, got {:?}",
        atm.sky_light
    );
    assert!(
        !atm.materials.layers.is_empty(),
        "textures.csv from ter*.mpk must be read"
    );
    assert!(
        atm.materials.resolve_rate() > 0.0,
        "TerrainTex DDS must resolve for at least one layer"
    );
    assert!(
        atm.ocean.is_some(),
        "global ocean plane missing — blue-void gap would remain"
    );
    assert!(
        mesh.water_vertices.len() >= 4,
        "ocean (+ lakes) must produce water verts, got {}",
        mesh.water_vertices.len()
    );

    let dir = atm.light_dir();
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    assert!(
        (len - 1.0).abs() < 1e-3,
        "light_dir from lights.csv must be unit, got {dir:?}"
    );

    // Published snapshot must match (GPU path).
    let pubd = published();
    assert_eq!(pubd.lights.len(), atm.lights.len());
    assert!(pubd.ocean.is_some());
}

#[test]
fn deleting_table_reads_removes_ocean_and_lighting() {
    let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("CAER_ATMOSPHERE_TABLES", "0");
    assert!(!tables_enabled());

    let root = client_root();
    if !root.join("zones").is_dir() {
        std::env::remove_var("CAER_ATMOSPHERE_TABLES");
        eprintln!("skip: no CAER_CLIENT zones at {}", root.display());
        return;
    }

    let origin = Vec3::new(561_400.0, 511_410.0, 2000.0);
    let mesh = load_region(1, origin, ALBION_MIN, ALBION_MAX, seam_blend());

    // Lakes from SECTOR.DAT may still exist — ocean plane must not.
    assert!(
        mesh.atmosphere.ocean.is_none(),
        "falsifier: ocean must vanish when tables disabled"
    );
    assert!(
        mesh.atmosphere.lights.is_empty(),
        "falsifier: lights.csv must not be read"
    );
    assert!(
        mesh.atmosphere.materials.is_empty(),
        "falsifier: textures.csv must not be read"
    );
    assert_eq!(mesh.atmosphere.sky_light.ambient_amount, 0.0);
    assert_eq!(mesh.atmosphere.sky_light.dynamic_amount, 0.0);
    assert_eq!(
        mesh.atmosphere.light_dir(),
        [0.0, 0.0, 0.0],
        "falsifier: no hardcoded sun vector when tables off"
    );

    let pubd = published();
    assert!(pubd.ocean.is_none());
    assert_eq!(pubd.light_dir(), [0.0, 0.0, 0.0]);

    std::env::remove_var("CAER_ATMOSPHERE_TABLES");
}

#[test]
fn hardcoded_light_vector_absent_from_shaders() {
    // Deletion check: the old magic constants must not remain in WGSL sources.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for name in ["terrain.wgsl", "mesh.wgsl", "skinned.wgsl", "shader.wgsl"] {
        let src = std::fs::read_to_string(root.join(name)).unwrap();
        assert!(
            !src.contains("0.35, 0.25, 1.0") && !src.contains("0.4, 0.3, 1.0"),
            "{name} still contains a hardcoded light vector — tables are not load-bearing"
        );
        assert!(
            src.contains("light_dir") && src.contains("light_ambient"),
            "{name} must read table-fed Globals lighting fields"
        );
    }
}
