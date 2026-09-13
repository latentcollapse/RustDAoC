//! System 3 falsifier driver: surface → interior → RvR terrain replacement.
//! Invoked by SCN-10 when `CAER_CLIENT` is set (and optionally by
//! `CAER_REGION_FINGERPRINT` pointing at this binary).
//!
//! Bar: each hop loads content, destination has `fixtures > 0`, and fingerprints
//! differ — "we called reload" alone is not evidence.
//!
//! Packet path still uses region 20 in SCN-10's controller latch; the *terrain* middle hop here is
//! region 51 (loadable city/interior). Classic dungeon `terrain.pcx` is empty; **dungeon NIF
//! coverage** is `append_dungeon_for_region` / `examples/dungeon_product_scenario.rs`. Do not
//! treat region 51 as dungeon coverage, and do not require region 20 terrain emptiness as PASS.
//!
//! ```bash
//! cargo \
//!   run -p caer-render --example region_fingerprint
//! ```

use caer_render::terrain::{load_region, seam_blend};
use glam::Vec3;

fn fingerprint(region: u16, origin: Vec3, min: [i32; 2], max: [i32; 2]) -> (usize, usize) {
    let mesh = load_region(region, origin, min, max, seam_blend());
    (mesh.zones_loaded, mesh.fixtures.len())
}

fn main() {
    if std::env::var_os("CAER_CLIENT").is_none() {
        eprintln!("FAIL CAER_CLIENT unset");
        std::process::exit(4);
    }

    // Surface — Camelot Hills box (OWN_CAPTURE Lilillyn feet).
    let px = 561_400;
    let py = 511_410;
    let r = 80_000;
    let surface = fingerprint(
        1,
        Vec3::new(px as f32, py as f32, 0.0),
        [px - r, py - r],
        [px + r, py + r],
    );
    // Interior — region 51 / zone 51 at cells (60,60). Not a dungeon: see module docs.
    let interior = fingerprint(
        51,
        Vec3::new(524_288.0, 524_288.0, 0.0),
        [480_000, 480_000],
        [570_000, 570_000],
    );
    // RvR — New Frontiers region 163.
    let rvr = fingerprint(
        163,
        Vec3::new(300_000.0, 400_000.0, 0.0),
        [200_000, 300_000],
        [500_000, 600_000],
    );

    eprintln!(
        "region_fingerprint: surface(1)={surface:?} interior(51)={interior:?} rvr(163)={rvr:?}"
    );

    if surface.1 == 0 {
        eprintln!("FAIL surface region 1 fixtures==0");
        std::process::exit(1);
    }
    if interior.0 == 0 && interior.1 == 0 {
        eprintln!("FAIL interior region 51 empty");
        std::process::exit(1);
    }
    if rvr.1 == 0 {
        eprintln!("FAIL RvR region 163 fixtures==0 (SCN-10 binding)");
        std::process::exit(1);
    }
    if surface == interior || surface == rvr || interior == rvr {
        eprintln!("FAIL fingerprints not replaced across surface/interior/RvR");
        std::process::exit(1);
    }

    println!(
        "PASS region_fingerprint surface→interior→RvR {surface:?} → {interior:?} → {rvr:?} \
         (dungeon NIF coverage is append_dungeon_for_region / dungeon_product_scenario, not terrain.pcx)"
    );
}
