//! Lane DNG — generic dungeon product scenario (not a per-dungeon renderer).
//!
//! Proves:
//! 1. Canonical classic dungeon remains region **20** / zone **19** (not region 51).
//! 2. The same `append_dungeon_for_region` / `append_dungeon_for_zone` path loads
//!    representatives (Albion / Midgard / Hibernia / Darkness Falls / task skin) when
//!    authoritative table data exists.
//! 3. Surface → dungeon → surface reload clears stale geometry; re-entry fingerprints match.
//! 4. Falsifier: skipping dungeon append makes the representative red.
//! 5. Boxes-only / Ok(empty) cannot PASS.
//!
//! ```bash
//! CAER_CLIENT=… cargo run -p caer-render --example dungeon_product_scenario
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use caer_render::dungeon_mesh::{
    append_dungeon_for_region, append_dungeon_geometry_if_surface_empty, canonical_dungeon_seat,
    clear_dungeon_state, dungeon_product_seat, product_reload_surface_dungeon,
    CANONICAL_DUNGEON_LABEL, CANONICAL_DUNGEON_REGION, CANONICAL_DUNGEON_ZONE,
    CANONICAL_SURFACE_REGION, CANONICAL_SURFACE_SEAT,
};
use caer_render::terrain::{load_region, seam_blend, TerrainMesh};
use caer_world::DUNGEON_REPRESENTATIVES;
use glam::Vec3;

fn main() {
    let Some(root) = std::env::var_os("CAER_CLIENT") else {
        eprintln!("FAIL CAER_CLIENT unset");
        std::process::exit(4);
    };
    let root = PathBuf::from(root);
    std::env::set_var("CAER_RT_EXAMPLE", "dungeon_product_scenario");
    caer_client::evidence::emit("dungeon_product_scenario");

    eprintln!(
        "canonical: region={CANONICAL_DUNGEON_REGION} zone={CANONICAL_DUNGEON_ZONE} \
         label={CANONICAL_DUNGEON_LABEL} (NOT region 51)"
    );
    if CANONICAL_DUNGEON_REGION == 51 {
        eprintln!("FAIL canonical region must not be 51");
        std::process::exit(1);
    }

    // --- Surface control (region 1) -----------------------------------------------------------
    let surface = load_region(
        CANONICAL_SURFACE_REGION,
        Vec3::new(CANONICAL_SURFACE_SEAT[0], CANONICAL_SURFACE_SEAT[1], 0.0),
        [
            CANONICAL_SURFACE_SEAT[0] as i32 - 12_000,
            CANONICAL_SURFACE_SEAT[1] as i32 - 12_000,
        ],
        [
            CANONICAL_SURFACE_SEAT[0] as i32 + 12_000,
            CANONICAL_SURFACE_SEAT[1] as i32 + 12_000,
        ],
        seam_blend(),
    );
    if surface.zones_loaded == 0 {
        eprintln!("FAIL surface region 1 zones_loaded==0");
        std::process::exit(1);
    }
    let surface_zones = surface.zones_loaded;
    let surface_models = surface.models.len();
    let mut surface_guard = surface;
    let noop = append_dungeon_geometry_if_surface_empty(
        &mut surface_guard,
        &root,
        CANONICAL_DUNGEON_REGION,
    );
    if noop.model_instances != 0 || noop.fixture_boxes != 0 {
        eprintln!("FAIL dungeon append mutated loaded surface region 1: {noop:?}");
        std::process::exit(1);
    }
    if surface_guard.zones_loaded != surface_zones || surface_guard.models.len() != surface_models {
        eprintln!("FAIL surface region 1 geometry changed after dungeon append");
        std::process::exit(1);
    }
    eprintln!(
        "PASS surface unchanged: region {CANONICAL_SURFACE_REGION} zones={surface_zones} \
         models={surface_models}"
    );

    // --- Falsifier: deleting append makes canonical representative red ------------------------
    let skipped = product_reload_surface_dungeon(
        &root,
        CANONICAL_DUNGEON_REGION,
        CANONICAL_DUNGEON_ZONE,
        false,
    );
    if skipped.representative_pass() {
        eprintln!("FAIL skip_dungeon_append falsifier was green");
        std::process::exit(1);
    }
    eprintln!("PASS falsifier skip_dungeon_append is red");

    // --- Generic representatives (same append path) -------------------------------------------
    for rep in DUNGEON_REPRESENTATIVES {
        let proof = product_reload_surface_dungeon(&root, rep.region_id, rep.zone_id, true);
        if !proof.representative_pass() {
            eprintln!(
                "FAIL representative {} zone {} region {} cat={} — not product PASS: {:?}",
                rep.label,
                rep.zone_id,
                rep.region_id,
                rep.realm_or_category.as_str(),
                proof
            );
            std::process::exit(1);
        }
        eprintln!(
            "PASS representative {} zone={} region={} cat={} nif={} kinds={} boxes={} \
             hash=0x{:016x} provenance={}",
            rep.label,
            rep.zone_id,
            rep.region_id,
            rep.realm_or_category.as_str(),
            proof.dungeon_fingerprint.model_instances,
            proof.dungeon_fingerprint.model_kinds,
            proof.dungeon_fingerprint.fixture_box_fallback,
            proof.dungeon_fingerprint.content_hash,
            rep.provenance
        );
        if let Ok(dir) = std::env::var("CAER_DUNGEON_FINGERPRINT_OUT") {
            let seat = dungeon_product_seat(&root, rep.zone_id);
            let fp = &proof.dungeon_fingerprint;
            let body = format!(
                "region={}\nzone={}\nlabel={}\ncategory={}\nmodel_kinds={}\nmodel_instances={}\n\
                 fixture_box_fallback={}\ntotal_vertices={}\ntotal_triangles={}\n\
                 content_hash=0x{:016x}\nseat={:.3},{:.3},{:.3}\nnot_a_golden=true\n",
                fp.region,
                fp.zone_id,
                fp.label,
                rep.realm_or_category.as_str(),
                fp.model_kinds,
                fp.model_instances,
                fp.fixture_box_fallback,
                fp.total_vertices,
                fp.total_triangles,
                fp.content_hash,
                seat[0],
                seat[1],
                seat[2]
            );
            let path = PathBuf::from(&dir).join(format!(
                "dungeon_zone{}_{}.fingerprint.txt",
                rep.zone_id,
                rep.realm_or_category.as_str()
            ));
            if let Err(e) = std::fs::write(&path, body) {
                eprintln!("WARN fingerprint write {path:?}: {e}");
            } else {
                eprintln!("wrote semantic fingerprint → {}", path.display());
            }
        }
    }

    // --- Canonical region-20 empty-terrain negative + seat walk sample ------------------------
    let seat = canonical_dungeon_seat();
    let mut dungeon = TerrainMesh {
        origin: Vec3::new(seat[0], seat[1], 0.0),
        ..TerrainMesh::default()
    };
    let terrain_only = load_region(
        CANONICAL_DUNGEON_REGION,
        dungeon.origin,
        [seat[0] as i32 - 20_000, seat[1] as i32 - 20_000],
        [seat[0] as i32 + 20_000, seat[1] as i32 + 20_000],
        seam_blend(),
    );
    if terrain_only.zones_loaded != 0 {
        eprintln!(
            "FAIL terrain region 20 unexpectedly non-empty zones={}",
            terrain_only.zones_loaded
        );
        std::process::exit(1);
    }
    let stats = append_dungeon_for_region(&mut dungeon, &root, CANONICAL_DUNGEON_REGION);
    if !stats.has_real_geometry() {
        eprintln!(
            "FAIL region 20 produced no real NIF geometry (boxes-only is not completion): {stats:?}"
        );
        std::process::exit(1);
    }
    if dungeon.surfaces.is_empty() {
        eprintln!("FAIL SurfaceIndex empty after dungeon NIF load — no collision surfaces");
        std::process::exit(1);
    }
    if let Some(h) = dungeon.walk_height_at(seat[0], seat[1], seat[2] + 500.0) {
        eprintln!("PASS walk_height_at seat → {h:.1} (collision sample)");
    } else {
        eprintln!("WARN walk_height_at seat returned None (nhd/ray miss) — mesh still present");
    }

    clear_dungeon_state(&mut dungeon);
    if !dungeon.models.is_empty() || !dungeon.fixtures.is_empty() || dungeon.zones_loaded != 0 {
        eprintln!("FAIL clear_dungeon_state left residual geometry");
        std::process::exit(1);
    }

    // --- Optional product screenshot via anchored rustdaoc binary ----------------------------
    if let Ok(png) = std::env::var("CAER_DUNGEON_SCREENSHOT") {
        let status = run_rustdaoc_screenshot(&png);
        if !status {
            eprintln!("FAIL rustdaoc dungeon screenshot");
            std::process::exit(1);
        }
        if !png_is_nonblank(Path::new(&png)) {
            eprintln!("FAIL screenshot is blank/missing: {png}");
            std::process::exit(1);
        }
        eprintln!("PASS product screenshot nonblank → {png}");
    } else {
        eprintln!("skip product screenshot (set CAER_DUNGEON_SCREENSHOT=path.png to capture)");
    }

    eprintln!(
        "PASS dungeon_product_scenario — generic append path; representatives proven; \
         skip-append falsifier red; no 281-playable claim"
    );
}

fn run_rustdaoc_screenshot(out: &str) -> bool {
    let exe = std::env::var_os("CARGO_BIN_EXE_rustdaoc").map(PathBuf::from);
    let mut cmd = if let Some(bin) = exe {
        let mut c = Command::new(bin);
        c.arg("--region")
            .arg(CANONICAL_DUNGEON_REGION.to_string())
            .arg("--screenshot")
            .arg(out)
            .arg("--size")
            .arg("640x400");
        c
    } else {
        let mut c = Command::new("cargo");
        c.arg("run")
            .arg("-q")
            .arg("-p")
            .arg("caer-render")
            .arg("--bin")
            .arg("rustdaoc")
            .arg("--")
            .arg("--region")
            .arg(CANONICAL_DUNGEON_REGION.to_string())
            .arg("--screenshot")
            .arg(out)
            .arg("--size")
            .arg("640x400");
        c
    };
    eprintln!("running: {cmd:?}");
    match cmd.status() {
        Ok(s) if s.success() => true,
        Ok(s) => {
            eprintln!("rustdaoc exited {s}");
            false
        }
        Err(e) => {
            eprintln!("rustdaoc spawn failed: {e}");
            false
        }
    }
}

fn png_is_nonblank(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    if bytes.len() < 64 {
        return false;
    }
    let dec = png::Decoder::new(std::io::Cursor::new(&bytes));
    let Ok(mut reader) = dec.read_info() else {
        return false;
    };
    let info = reader.info();
    let (w, h) = (info.width as usize, info.height as usize);
    let mut buf = vec![0; reader.output_buffer_size()];
    if reader.next_frame(&mut buf).is_err() {
        return false;
    };
    let x0 = w / 4;
    let x1 = (3 * w) / 4;
    let y0 = h / 4;
    let y1 = (3 * h) / 4;
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let o = (y * w + x) * 4;
            sum[0] += u64::from(buf[o]);
            sum[1] += u64::from(buf[o + 1]);
            sum[2] += u64::from(buf[o + 2]);
            n += 1;
        }
    }
    if n == 0 {
        return false;
    }
    let mean = [sum[0] / n, sum[1] / n, sum[2] / n];
    let mut changed = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let o = (y * w + x) * 4;
            if (0..3).any(|c| u64::from(buf[o + c]).abs_diff(mean[c]) > 18) {
                changed += 1;
            }
        }
    }
    let frac = changed as f64 / n as f64;
    eprintln!(
        "screenshot center variance={:.2}% mean_rgb={:?} bytes={} (need >8%)",
        frac * 100.0,
        mean,
        bytes.len()
    );
    frac > 0.08 && bytes.len() > 8_000
}
