//! Census every `monsters.csv` model for mesh resolution and render the first `CAER_NPC_SAMPLE`
//! models into a contact sheet. The sampled render gate catches models that resolve but never reach
//! visible pixels; it does not claim to prove coverage for the unsampled remainder.

mod common;

use common::{blit_cell, coverage, root};

use caer_render::camera::Camera;
use caer_render::entities::EntityModels;
use caer_render::gpu::{FramePass, Gpu};
use glam::Vec3;

const CELL: u32 = 160;
const COLS: u32 = 12;
const MIN_VISIBLE_COVERAGE: f64 = 0.005;
const CENSUS_GPU_BATCH: usize = 64;

fn is_blank_frame(rgba: &[u8]) -> bool {
    coverage(rgba) < MIN_VISIBLE_COVERAGE
}

fn new_headless_gpu() -> Gpu {
    pollster::block_on(Gpu::new_headless(CELL, CELL, 60_000.0)).expect("gpu init")
}

#[test]
fn creature_census_resolves_all_and_sample_renders_nonblank() {
    let root = root();
    // SAFETY: single-threaded test entry; `EntityModels::load` reads the client root from env.
    std::env::set_var("CAER_CLIENT", &root);

    let mut gpu = new_headless_gpu();
    // The known-bad control uses the same pre-world path with no instances.
    let cleared = gpu
        .render_to_rgba_pass(0, FramePass::PreWorldScene)
        .expect("cleared-frame render");
    assert!(
        is_blank_frame(&cleared),
        "a cleared pre-world frame has coverage {:.4}, so the blank detector no longer controls",
        coverage(&cleared)
    );
    let mut models = EntityModels::load().expect("entity models from the retail tree");
    caer_render::atmosphere::publish(caer_render::atmosphere::Atmosphere::load_for_region(
        &root,
        "sky_default",
    ));

    let ids = models.creatures().model_ids();
    assert!(
        ids.len() > 1000,
        "monsters.csv named only {} models — the table join is broken, not the client",
        ids.len()
    );

    let requested_sample: usize = std::env::var("CAER_NPC_SAMPLE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(96);
    let sample = requested_sample.clamp(1, ids.len());
    let sheet_rows = (sample as u32).div_ceil(COLS);
    let (sheet_w, sheet_h) = (COLS * CELL, sheet_rows * CELL);
    let mut sheet = vec![0u8; (sheet_w * sheet_h * 4) as usize];

    // Why a model produced no mesh, not just that it did not. Three different owners hide behind
    // one count: the tables naming no NIF, the archive not shipping the file, and the decoder or
    // upload failing on a file that is present.
    let mut reason_no_nif: Vec<u16> = Vec::new();
    let mut reason_absent: Vec<u16> = Vec::new();
    let mut reason_decode: Vec<u16> = Vec::new();
    // Every loose NIF the client ships, by lowercased base name.
    let mut shipped: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for dir in ["figures", "zones/Nifs", "zones/Dnifs"] {
        if let Ok(rd) = std::fs::read_dir(root.join(dir)) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                if let Some(stem) = n.strip_suffix(".nif") {
                    shipped.insert(stem.to_string());
                }
            }
        }
    }
    let mut no_mesh: Vec<u16> = Vec::new();
    let mut blank: Vec<u16> = Vec::new();
    let mut no_skin: Vec<u16> = Vec::new();
    let mut first_resolved = None;

    for (i, &id) in ids.iter().enumerate() {
        if i != 0 && i % CENSUS_GPU_BATCH == 0 {
            // This is an asset census, not a long-lived scene: retain only the current batch so
            // backend allocator retention cannot turn independent uploads into a false OOM.
            models.evict_unseen_meshes(&mut gpu, &std::collections::HashSet::new());
            gpu.poll_wait_idle().expect("finish census GPU batch");
            drop(gpu);
            gpu = new_headless_gpu();
        }
        let has_mesh = models.ensure_mesh(&mut gpu, id);
        if !has_mesh {
            no_mesh.push(id);
            match models.creatures().nif_name(id) {
                None => reason_no_nif.push(id),
                Some(n) if !shipped.contains(&n.to_ascii_lowercase()) => reason_absent.push(id),
                Some(_) => reason_decode.push(id),
            }
            continue;
        }
        first_resolved.get_or_insert(id);
        let live = std::collections::HashSet::from([id]);
        models.evict_unseen_meshes(&mut gpu, &live);
        assert_eq!(
            gpu.resident_entity_mesh_count(),
            1,
            "the census keeps only its current model resident; GPU meshes may not grow with every row"
        );
        if models.creatures().body_skin(id).is_none() {
            no_skin.push(id);
        }

        // Only the sampled prefix is rendered; the census above covers everything.
        if i >= sample {
            continue;
        }

        gpu.clear_skinned_instances();
        gpu.clear_entity_instances();
        let height = models.model_height(id).unwrap_or(70.0).max(8.0);
        let back = height * 1.4 / (30.0_f32.to_radians()).tan();
        let mut cam = Camera::new(
            Vec3::new(0.0, -back, height * 0.55),
            Vec3::new(0.0, 0.0, height * 0.5),
            Vec3::ZERO,
            1.0,
        );
        cam.ensure_far(60_000.0);
        gpu.set_view_proj(cam.view_proj().to_cols_array_2d());
        // Creatures come up the SKINNED path, same as player bodies. Drawing them as static
        // entity instances renders nothing — which is what made 93 of 96 look "blank" on the
        // first run. That was the harness lying, not the client failing.
        let scale = models.model_scale(id);
        let palettes = models.skinned_rig(id).map(|rig| {
            caer_render::anim_skin::build_palettes(
                rig,
                &[caer_render::anim_skin::UniquePaletteJob {
                    loco: caer_render::entities::Loco::Idle,
                    t: 0.0,
                    blend: None,
                }],
            )
        });
        match palettes {
            Some(p) if !p.is_empty() => gpu.update_skinned_instances(
                id,
                &[caer_render::gpu::SkinnedInstance {
                    pos_yaw: [0.0, 0.0, 0.0, std::f32::consts::PI],
                    scale,
                    palette_base: 0.0,
                }],
                &p,
            ),
            _ => gpu.update_entity_instances(id, &[[0.0, 0.0, 0.0, std::f32::consts::PI, scale]]),
        }

        let cell = gpu
            .render_to_rgba_pass(0, FramePass::PreWorldScene)
            .expect("cell render");
        if is_blank_frame(&cell) {
            blank.push(id);
        }

        blit_cell(&mut sheet, sheet_w, COLS, &cell, CELL, CELL, i as u32);
    }

    // See `preworld_avatars`: a relative "target/" lands in the crate directory, not the
    // workspace target.
    let out = std::env::var("CAER_NPC_SHEET_OUT")
        .unwrap_or_else(|_| format!("{}/npc_sheet.png", env!("CARGO_TARGET_TMPDIR")));
    if let Some(parent) = std::path::Path::new(&out).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = std::fs::File::create(&out) {
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), sheet_w, sheet_h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        if let Ok(mut w) = enc.write_header() {
            let _ = w.write_image_data(&sheet);
        }
    }

    let total = ids.len();
    println!("\n=== creature bank: {total} models named by monsters.csv ===");
    println!("  mesh reached the GPU : {}", total - no_mesh.len());
    println!("  no mesh at all       : {}", no_mesh.len());
    println!("  no body skin         : {}", no_skin.len());
    println!(
        "  rendered blank       : {} (of {sample} sampled)",
        blank.len()
    );
    println!("  contact sheet        : {out} ({sheet_w}x{sheet_h}, first {sample})");
    let show = |label: &str, v: &[u16]| {
        if !v.is_empty() {
            let names: Vec<String> = v.iter().take(20).map(|id| format!("{id}")).collect();
            println!(
                "  {label}: {}{}",
                names.join(" "),
                if v.len() > 20 { " …" } else { "" }
            );
        }
    };
    println!("\n  why the {} no-mesh models failed:", no_mesh.len());
    println!("    tables name no NIF        : {}", reason_no_nif.len());
    println!("    NIF named but not shipped : {}", reason_absent.len());
    println!("    present but failed decode : {}", reason_decode.len());
    // Every decode failure carries a message. Grouping by that message turns "130 broken" into a
    // short list of distinct causes, which is the difference between a number and a work item.
    if !reason_decode.is_empty() {
        use std::collections::BTreeMap;
        let mut causes: BTreeMap<String, (usize, Vec<u16>)> = BTreeMap::new();
        for &id in &reason_decode {
            let Some(nif) = models.creatures().nif_name(id) else {
                continue;
            };
            let mut path = None;
            for dir in ["figures", "zones/Nifs", "zones/Dnifs"] {
                let p = root.join(dir).join(format!("{nif}.nif"));
                if p.exists() {
                    path = Some(p);
                    break;
                }
            }
            let Some(path) = path else { continue };
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            // Ask both decoders; the static path is what `ensure_mesh` falls back to.
            let cause = match caer_assets::nif::read_model(&bytes) {
                Ok(m) if m.parts.is_empty() => "decodes but has zero mesh parts".to_string(),
                Ok(m) if m.parts.iter().all(|p| p.positions.is_empty()) => {
                    "decodes but every part has zero vertices".to_string()
                }
                Ok(_) => match caer_assets::nif::read_rigged(&bytes) {
                    Ok(_) => "static decode OK — upload/gate rejected it".to_string(),
                    Err(e) => format!("rigged decode: {e}"),
                },
                Err(e) => format!("static decode: {e}"),
            };
            let entry = causes.entry(cause).or_default();
            entry.0 += 1;
            if entry.1.len() < 6 {
                entry.1.push(id);
            }
        }
        // What block types sit just before the reported failure index? A desync shows up as the
        // same unusual type recurring right before every truncation.
        if let Some(&probe_id) = reason_decode.first() {
            if let Some(nif) = models.creatures().nif_name(probe_id) {
                for dir in ["figures", "zones/Nifs", "zones/Dnifs"] {
                    let p = root.join(dir).join(format!("{nif}.nif"));
                    if let Ok(bytes) = std::fs::read(&p) {
                        if let Ok(types) = caer_assets::nif::block_type_sequence(&bytes) {
                            use std::collections::BTreeMap;
                            let mut hist: BTreeMap<&str, usize> = BTreeMap::new();
                            for t in &types {
                                *hist.entry(t.as_str()).or_default() += 1;
                            }
                            println!("\n  probe {nif}.nif ({} blocks): {hist:?}", types.len());
                        }
                        break;
                    }
                }
            }
        }
        println!("\n  decode failures grouped by cause:");
        for (cause, (n, sample)) in &causes {
            println!("    {n:>4}  {cause}  (e.g. {sample:?})");
        }
    }

    show("  no-NIF ids", &reason_no_nif);
    show("  not-shipped ids", &reason_absent);
    show("  decode-failed ids", &reason_decode);
    show("first blank ids", &blank);
    show("first no-skin ids", &no_skin);

    models.evict_unseen_meshes(&mut gpu, &std::collections::HashSet::new());
    assert_eq!(
        gpu.resident_entity_mesh_count(),
        0,
        "releasing the census view must release every GPU mesh record"
    );
    if let Some(id) = first_resolved {
        assert!(
            models.ensure_mesh(&mut gpu, id),
            "an evicted model must be reloadable when it returns to view"
        );
        assert_eq!(
            gpu.resident_entity_mesh_count(),
            1,
            "the reloaded model must be the only resident census mesh"
        );
    }
    assert!(
        blank.is_empty(),
        "{} of {sample} sampled creatures rendered blank: {blank:?}",
        blank.len()
    );

    // A collapse here means the tables or archive search broke before the rendering path.
    let resolved = (total - no_mesh.len()) as f64 / total as f64;
    assert!(
        resolved > 0.50,
        "only {:.1}% of {total} creature models produced a mesh — the resolver is broken, not the \
         client's data",
        resolved * 100.0
    );
}
