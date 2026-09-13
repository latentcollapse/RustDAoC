//! REQ-020 — equipment must change the *rendered* avatar/NPC mesh, not just an inventory struct.
//!
//! Asserts armour-tier filtering selects different part sets for different `EquipmentUpdate`
//! payloads. When `$CAER_CLIENT` is present, also runs the filter against a real multi-Body NIF.
//!
//! ## Pinned appearance harness (BLOCKING — Claude 2026-08-08 / REQ-020 tightened)
//!
//! MS-02b texture / fullset shots **must** use [`pin`]: fixed camera, bind-pose `t=0`, instance
//! yaw/scale, and the shader-fixed light `(0.35, 0.25, 1.0)` via an unchanged default day sky.
//! Baseline vs candidate may differ only by equipment.
//!
//! **Control diff (REQ-020):** same equipment in both captures must read ~0. Persisted as
//! `equip_control_{a,b}.png` + `equip_control_diff.json`. Every appearance claim cites it; a
//! non-trivial control invalidates the readout.

use caer_protocol::codec::PacketWriter;
use caer_protocol::equipment::{decode, slot};
use caer_render::entities::{keep_armour_tiers, ArmourTiers};

/// REQ-025: gate client-dependent appearance tests. Returns `None` only in non-strict skip mode.
///
/// Returns the `ClientDep` guard, not a bare path: its `Drop` is what emits the completion
/// marker, so the caller must hold it for the whole test body. Converting it to a `PathBuf`
/// here would drop it immediately and report the test complete before it had run — which is
/// the defect this type exists to prevent. It derefs to `Path`, so use sites are unchanged.
fn require_client(test: &str) -> Option<caer_assets::client_dep::ClientDep> {
    caer_assets::client_dep::require_caer_client(test)
}

fn require_subdir(test: &str, root: &std::path::Path, rel: &str) -> bool {
    if root.join(rel).is_dir() {
        true
    } else {
        caer_assets::client_dep::skip_or_fail(test, &format!("missing {rel} under CAER_CLIENT"));
        false
    }
}

/// Shared pin for Self_ appearance A/B shots. Do not drift these independently between baseline
/// and candidate — that was the pose contamination that made mean_colour_shift load-bearing.
mod pin {
    use caer_assets::sky::Sky;
    use caer_render::camera::Camera;
    use caer_render::entities::EntityModels;
    use caer_render::gpu::{Gpu, SkinnedInstance};
    use glam::Vec3;
    use std::path::{Path, PathBuf};

    pub const W: u32 = 640;
    pub const H: u32 = 480;
    pub const SCALE: f32 = 1.0;
    /// Bind-pose / first clip sample — not a mid-walk frame.
    pub const CLIP_T: f32 = 0.0;
    pub const YAW: f32 = 0.0;
    /// Front three-quarter framing used by MS-02b studded/plate and fullset.
    const EYE: [f32; 3] = [63.0, -180.0, 63.0]; // dist=180, *0.35 / -1 / *0.35
    const LOOK: [f32; 3] = [0.0, 0.0, 55.0];

    pub const CONTROL_JSON: &str = "equip_control_diff.json";
    pub const CONTROL_A_PNG: &str = "equip_control_a.png";
    pub const CONTROL_B_PNG: &str = "equip_control_b.png";

    /// Apply pinned camera + Albion day sky **and** table-fed sky lighting.
    ///
    /// `skinned.wgsl` lights from `atmosphere::published()`, not a shader hardcoded sun.
    /// Appearance tests do not `load_region`, so without this publish ambient/dynamic are 0 and
    /// every armour set is a black silhouette — a non-discriminating instrument.
    pub fn apply(gpu: &mut Gpu) {
        if let Ok(root) = std::env::var("CAER_CLIENT") {
            let atm = caer_render::atmosphere::Atmosphere::load_for_region(
                std::path::Path::new(&root),
                "sky_albion",
            );
            gpu.set_atmosphere(&atm);
        }
        let camera = Camera::new(
            Vec3::from_array(EYE),
            Vec3::from_array(LOOK),
            Vec3::ZERO,
            W as f32 / H as f32,
        );
        gpu.set_sky(Sky::default().day);
        gpu.set_view_proj(camera.view_proj().to_cols_array_2d());
    }

    pub fn artifacts_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/council/measurements/artifacts")
    }

    pub fn render(gpu_id: u16, em: &EntityModels, gpu: &mut Gpu) -> Vec<u8> {
        let rig = em.skinned_rig(gpu_id).expect("rig");
        let palette = rig.palette_of(&rig.clip, CLIP_T);
        let inst = SkinnedInstance {
            pos_yaw: [0.0, 0.0, 0.0, YAW],
            scale: SCALE,
            palette_base: 0.0,
        };
        gpu.clear_entity_instances();
        gpu.clear_skinned_instances();
        gpu.update_skinned_instances(gpu_id, &[inst], &palette);
        gpu.render_to_rgba(0).expect("GPU wait")
    }

    /// Count pixels whose RGB L1 exceeds `thresh` (alpha ignored).
    pub fn changed_pixels(a: &[u8], b: &[u8], thresh: u8) -> usize {
        assert_eq!(a.len(), b.len());
        let mut n = 0usize;
        for i in (0..a.len()).step_by(4) {
            let da = a[i].abs_diff(b[i]) as u32
                + a[i + 1].abs_diff(b[i + 1]) as u32
                + a[i + 2].abs_diff(b[i + 2]) as u32;
            if da > u32::from(thresh) {
                n += 1;
            }
        }
        n
    }

    pub fn near_sky(r: u8, g: u8, b: u8) -> bool {
        let dr = i32::from(r) - 130;
        let dg = i32::from(g) - 160;
        let db = i32::from(b) - 208;
        dr * dr + dg * dg + db * db < 40 * 40
    }

    /// Character-sample (non-sky) mean RGB for each shot + Euclidean shift — lead statistic under REQ-020.
    pub fn char_mean_shift(a: &[u8], b: &[u8]) -> (f64, u64, [f64; 3], [f64; 3]) {
        let mut mean_a = [0u64; 3];
        let mut mean_b = [0u64; 3];
        let mut samples = 0u64;
        for i in (0..a.len()).step_by(4) {
            if near_sky(a[i], a[i + 1], a[i + 2]) && near_sky(b[i], b[i + 1], b[i + 2]) {
                continue;
            }
            mean_a[0] += a[i] as u64;
            mean_a[1] += a[i + 1] as u64;
            mean_a[2] += a[i + 2] as u64;
            mean_b[0] += b[i] as u64;
            mean_b[1] += b[i + 1] as u64;
            mean_b[2] += b[i + 2] as u64;
            samples += 1;
        }
        if samples == 0 {
            return (0.0, 0, [0.0; 3], [0.0; 3]);
        }
        let sa = [
            mean_a[0] as f64 / samples as f64,
            mean_a[1] as f64 / samples as f64,
            mean_a[2] as f64 / samples as f64,
        ];
        let sb = [
            mean_b[0] as f64 / samples as f64,
            mean_b[1] as f64 / samples as f64,
            mean_b[2] as f64 / samples as f64,
        ];
        let shift =
            ((sa[0] - sb[0]).powi(2) + (sa[1] - sb[1]).powi(2) + (sa[2] - sb[2]).powi(2)).sqrt();
        (shift, samples, sa, sb)
    }

    pub fn write_png(path: &Path, rgba: &[u8]) {
        let mut file = std::fs::File::create(path).expect("artifact png");
        let mut enc = png::Encoder::new(&mut file, W, H);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(rgba).unwrap();
    }

    /// REQ-020 control: same unequipped avatar, two captures under [`apply`]. Persists PNG pair +
    /// JSON. Returns changed pixel count (must be 0). Non-trivial control → readout invalid.
    pub fn run_and_persist_control(
        em: &mut EntityModels,
        gpu: &mut Gpu,
        race: u8,
        gender: u8,
    ) -> usize {
        apply(gpu);
        let Some(ga) = em.ensure_avatar(
            gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            panic!("control: ensure_avatar unequipped failed");
        };
        let Some(gb) = em.ensure_avatar(
            gpu,
            race,
            gender,
            caer_protocol::customization::Customization::default(),
            None,
        ) else {
            panic!("control: twin ensure_avatar failed");
        };
        let a = render(ga, em, gpu);
        let b = render(gb, em, gpu);
        let a2 = render(ga, em, gpu);
        let same_id = changed_pixels(&a, &a2, 0);
        let twin = changed_pixels(&a, &b, 0);
        // `ensure_avatar` caches, so the twin request usually returns the SAME gpu id. When it
        // does, `twin` is a second same-mesh re-render, NOT an independent-upload comparison —
        // record that rather than let the artifact imply a control it did not run. REQ-020 only
        // needs render determinism (that is what lets the harness separate armour from artifact),
        // so a coincident id is not a failure; silently reporting it as a twin upload would be.
        let twin_distinct_upload = ga != gb;
        let (mean_shift, char_samples, mean_a, mean_b) = char_mean_shift(&a, &b);

        let out = artifacts_dir();
        let _ = std::fs::create_dir_all(&out);
        write_png(&out.join(CONTROL_A_PNG), &a);
        write_png(&out.join(CONTROL_B_PNG), &b);
        // Visual control aid — must be black when harness is pinned.
        let mut vis = vec![0u8; a.len()];
        for i in (0..a.len()).step_by(4) {
            let d = (0..3)
                .map(|c| a[i + c].abs_diff(b[i + c]))
                .max()
                .unwrap_or(0);
            vis[i] = d;
            vis[i + 1] = d;
            vis[i + 2] = d;
            vis[i + 3] = 255;
        }
        write_png(&out.join("equip_control_diff_vis.png"), &vis);

        let frac = twin as f64 / (W * H) as f64;
        let metrics = format!(
            "{{\n  \"kind\": \"control_diff\",\n  \"req\": \"REQ-020\",\n  \
             \"equipment\": \"unequipped\",\n  \"race\": {race},\n  \"gender\": {gender},\n  \
             \"gpu_ids\": [{ga}, {gb}],\n  \
             \"character_samples\": {char_samples},\n  \
             \"mean_colour_shift\": {mean_shift:.6},\n  \
             \"mean_rgb_a\": [{:.4}, {:.4}, {:.4}],\n  \
             \"mean_rgb_b\": [{:.4}, {:.4}, {:.4}],\n  \
             \"changed_pixels\": {twin},\n  \"changed_fraction\": {frac:.8},\n  \
             \"same_id_changed_pixels\": {same_id},\n  \
             \"twin_distinct_upload\": {twin_distinct_upload},\n  \
             \"twin_note\": \"{}\",\n  \
             \"threshold\": 0,\n  \"valid\": {},\n  \
             \"resolution\": [{W}, {H}],\n  \"pinned\": true,\n  \
             \"camera_eye\": {:?},\n  \"camera_look\": {:?},\n  \
             \"yaw\": {YAW}, \"scale\": {SCALE}, \"clip_t\": {CLIP_T},\n  \
             \"artifacts\": [\"{CONTROL_A_PNG}\", \"{CONTROL_B_PNG}\", \"equip_control_diff_vis.png\"]\n}}\n",
            mean_a[0],
            mean_a[1],
            mean_a[2],
            mean_b[0],
            mean_b[1],
            mean_b[2],
            if twin_distinct_upload {
                "two independently uploaded meshes compared"
            } else {
                "ensure_avatar returned a cached id; twin diff is a same-mesh re-render, not an independent upload"
            },
            twin == 0 && same_id == 0,
            EYE,
            LOOK
        );
        std::fs::write(out.join(CONTROL_JSON), metrics).unwrap();
        eprintln!(
            "REQ-020 control: same_id={same_id} twin={twin} mean_colour_shift={mean_shift:.4} char_samples={char_samples} → {}",
            out.join(CONTROL_JSON).display()
        );
        assert_eq!(
            same_id, 0,
            "REQ-020 control REJECT: same gpu_id re-render not bit-identical (changed={same_id})"
        );
        assert_eq!(
            twin, 0,
            "REQ-020 control REJECT: non-trivial control diff (changed={twin}) — every appearance claim resting on this harness is invalid"
        );
        twin
    }

    /// Appearance claims must cite a valid control artifact (REQ-020).
    ///
    /// This ALWAYS runs the control. It must not short-circuit on an existing
    /// `equip_control_diff.json`, however recently written: the requirement is that *this run's*
    /// harness is a working instrument, and a committed artifact only proves some past run's was.
    /// Reading `"valid": true` off disk makes REQ-020's falsifier unfireable — the harness could
    /// regress to an unpinned camera or a nondeterministic render and every appearance claim would
    /// still "cite a valid control" that never executed. The control is one avatar render; that is
    /// not a cost worth trading the gate for.
    pub fn require_valid_control(
        em: &mut EntityModels,
        gpu: &mut Gpu,
        race: u8,
        gender: u8,
    ) -> &'static str {
        // Asserts internally on same_id != 0 or twin != 0 — a bad control fails the run here.
        let _ = run_and_persist_control(em, gpu, race, gender);
        CONTROL_JSON
    }
}

fn part_names(rig: &caer_assets::nif::RiggedModel) -> Vec<String> {
    let mut v: Vec<_> = rig
        .parts
        .iter()
        .map(|p| p.name.to_ascii_lowercase())
        .collect();
    v.sort();
    v
}

#[test]
fn equipment_tier_filter_changes_part_set_on_names() {
    // Name-level proof (always runnable): the filter selects different BodyN parts.
    let t1 = ArmourTiers {
        body: 1,
        ..ArmourTiers::default()
    };
    let t3 = ArmourTiers {
        body: 3,
        ..ArmourTiers::default()
    };
    let names = ["Body1", "Body2", "Body3", "Arms1", "HeadA1"];
    let keep = |tiers: ArmourTiers| -> Vec<&str> {
        names
            .into_iter()
            .filter(|n| {
                let n = n.to_ascii_lowercase();
                if let Some(rest) = n.strip_prefix("body") {
                    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    return digits.parse::<u8>().ok() == Some(tiers.body);
                }
                true
            })
            .collect()
    };
    assert_eq!(keep(t1), vec!["Body1", "Arms1", "HeadA1"]);
    assert_eq!(keep(t3), vec!["Body3", "Arms1", "HeadA1"]);
    assert_ne!(
        keep(t1),
        keep(t3),
        "REQ-020: different equipment must change visible parts"
    );

    // And the public from_equipment path feeds those tiers.
    let mut w = PacketWriter::new();
    w.u16(7).u8(0).u8(0).u8(0).u8(0).u8(1);
    w.u8(slot::TORSO).u16(0x0100).u8(3);
    let eq = decode(w.as_slice()).unwrap();
    assert_eq!(ArmourTiers::from_equipment(&eq).body, 3);
}

#[test]
fn live_nif_armour_filter_when_client_present() {
    let Some(root) = require_client("live_nif_armour_filter_when_client_present") else {
        return;
    };
    if !require_subdir(
        "live_nif_armour_filter_when_client_present",
        &root,
        "figures",
    ) {
        return;
    }
    let mut found = None;
    let Ok(rd) = std::fs::read_dir(root.join("figures")) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("nif")) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        let Ok(Some(rig)) = caer_assets::nif::read_rigged(&bytes) else {
            continue;
        };
        let bodies: Vec<_> = rig
            .parts
            .iter()
            .filter(|part| part.name.to_ascii_lowercase().starts_with("body"))
            .map(|part| part.name.clone())
            .collect();
        if bodies.len() >= 2 {
            found = Some((p, rig, bodies));
            break;
        }
    }
    let Some((path, rig, bodies)) = found else {
        caer_assets::client_dep::skip_or_fail(
            "live_nif_armour_filter_when_client_present",
            &format!(
                "no multi-body nif found under {}",
                root.join("figures").display()
            ),
        );
        return;
    };
    eprintln!(
        "equipment appearance nif: {} bodies={bodies:?}",
        path.display()
    );

    let other = bodies
        .iter()
        .filter_map(|n| {
            n.to_ascii_lowercase()
                .strip_prefix("body")
                .map(|r| {
                    r.chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>()
                })
                .and_then(|d| d.parse::<u8>().ok())
        })
        .find(|&d| d != 1)
        .unwrap_or(2);

    let mut a = caer_assets::nif::read_rigged(&std::fs::read(&path).unwrap())
        .ok()
        .flatten()
        .expect("re-read nif");
    let mut b = caer_assets::nif::read_rigged(&std::fs::read(&path).unwrap())
        .ok()
        .flatten()
        .expect("re-read nif");
    let _ = rig;
    keep_armour_tiers(
        &mut a,
        ArmourTiers {
            body: 1,
            ..Default::default()
        },
    );
    keep_armour_tiers(
        &mut b,
        ArmourTiers {
            body: other,
            ..Default::default()
        },
    );
    assert_ne!(
        part_names(&a),
        part_names(&b),
        "REQ-020: live NIF part sets must differ between equipment tiers"
    );
}

/// BLOCKING REQ-020 control: same unequipped avatar twice under [`pin`] → ~0, **persisted** as
/// `equip_control_*.png` + `equip_control_diff.json` so appearance claims can cite it.
#[test]
fn pinned_unequipped_vs_unequipped_is_near_zero() {
    use caer_assets::figures::GENDER_FEMALE;
    use caer_render::entities::EntityModels;
    use caer_render::gpu::Gpu;

    let Some(root) = require_client("pinned_unequipped_vs_unequipped_is_near_zero") else {
        return;
    };
    if !require_subdir(
        "pinned_unequipped_vs_unequipped_is_near_zero",
        &root,
        "figures",
    ) {
        return;
    }
    let Some(mut em) = EntityModels::load() else {
        caer_assets::client_dep::skip_or_fail(
            "pinned_unequipped_vs_unequipped_is_near_zero",
            "EntityModels::load failed",
        );
        return;
    };
    let mut gpu = pollster::block_on(Gpu::new_headless(pin::W, pin::H, 5_000.0)).expect("gpu init");
    const RACE: u8 = 3;
    let _ = pin::run_and_persist_control(&mut em, &mut gpu, RACE, GENDER_FEMALE);
}

/// REQ-020 pixel proof: same model, same camera, two equipment tiers → character pixels differ.
///
/// Chair rule (2026-08-07): acceptance artifacts MUST report differing-pixel count, percentage,
/// and diff bounding box; the test MUST FAIL when the count is zero. Boolean "pixels differ" is
/// not enough — a zero-diff pair must never pass.
#[test]
fn equipment_changes_rendered_pixels_when_client_present() {
    use caer_render::camera::Camera;
    use caer_render::entities::EntityModels;
    use caer_render::gpu::{Gpu, SkinnedInstance};
    use glam::Vec3;

    let Some(root) = require_client("equipment_changes_rendered_pixels_when_client_present") else {
        return;
    };
    if !require_subdir(
        "equipment_changes_rendered_pixels_when_client_present",
        &root,
        "figures",
    ) {
        return;
    }
    let Some(mut em) = EntityModels::load() else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_changes_rendered_pixels_when_client_present",
            "EntityModels::load failed",
        );
        return;
    };

    fn equip(object_id: u16, torso_ext: u8) -> caer_protocol::equipment::EquipmentUpdate {
        let mut w = PacketWriter::new();
        w.u16(object_id).u8(0).u8(0).u8(0).u8(0).u8(1);
        w.u8(slot::TORSO).u16(0x0100).u8(torso_ext);
        decode(w.as_slice()).expect("equip decode")
    }

    /// Diff metrics required by the chair for REQ-020 acceptance artifacts.
    type PixelDiff = (usize, f64, f64, Option<(u32, u32, u32, u32)>);
    fn pixel_diff(a: &[u8], b: &[u8], w: u32, h: u32) -> PixelDiff {
        assert_eq!(a.len(), b.len());
        assert_eq!(a.len(), (w * h * 4) as usize);
        let mut changed = 0usize;
        let mut sum = 0u64;
        let mut min_x = w;
        let mut min_y = h;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let mut hot = false;
                for c in 0..4 {
                    let d = a[i + c].abs_diff(b[i + c]);
                    sum += u64::from(d);
                    if d > 8 {
                        hot = true;
                    }
                }
                if hot {
                    changed += 1;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        let px = (w * h) as usize;
        let frac = changed as f64 / px as f64;
        let mean = sum as f64 / a.len() as f64;
        let bbox = if changed == 0 {
            None
        } else {
            Some((min_x, min_y, max_x, max_y))
        };
        (changed, frac, mean, bbox)
    }

    const W: u32 = 640;
    const H: u32 = 480;
    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 5_000.0)).expect("gpu init");

    // Prefer a model whose tier meshes differ substantially in topology. Same-silhouette
    // flat-red pairs (e.g. model 61, ~5% index delta) produced only a ~0.08% pixel change —
    // not visible. Seed with model 1265 (Body1 vs Body5 ≈ 3.3× indices) then fall back to a
    // capped scan — uploading thousands of skinned meshes OOMs wgpu.
    let mut found: Option<(u16, u16, u16, u32, u32, u8)> = None;
    let mut best_ratio = 1.0_f64;
    let seed = [1265u16]
        .into_iter()
        .chain(1u16..=2500)
        .filter(|&id| id != 1265 || true);
    'scan: for &ext_b in &[5u8, 4, 3] {
        for model_id in seed.clone() {
            let a = em.ensure_mesh_for_equipment(&mut gpu, model_id, Some(&equip(1, 1)));
            let b = em.ensure_mesh_for_equipment(&mut gpu, model_id, Some(&equip(1, ext_b)));
            let (Some(ga), Some(gb)) = (a, b) else {
                continue;
            };
            if ga == gb || !gpu.has_skinned_mesh(ga) || !gpu.has_skinned_mesh(gb) {
                continue;
            }
            let ia = gpu.skinned_index_count(ga);
            let ib = gpu.skinned_index_count(gb);
            if ia == 0 || ib == 0 {
                continue;
            }
            let ratio = (ia.max(ib) as f64) / (ia.min(ib) as f64);
            if ratio > best_ratio {
                best_ratio = ratio;
                found = Some((model_id, ga, gb, ia, ib, ext_b));
            }
            if ratio >= 2.0 {
                break 'scan;
            }
        }
    }
    let Some((model_id, ga, gb, ia, ib, ext_b)) = found.filter(|_| best_ratio >= 1.25) else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_changes_rendered_pixels_when_client_present",
            "no skinned model with topologically distinct equipment meshes",
        );
        return;
    };
    eprintln!(
        "REQ-020 pixel model={model_id} gpu_ids={ga},{gb} indices={ia},{ib} ratio={best_ratio:.2} torso_ext=1vs{ext_b}"
    );

    // Creature NIFs vary wildly in authored units. Try a few scales and keep the framing that
    // maximises the visible equipment delta — a 32-pixel hairline is not what Matt needs to see.
    let base = em.model_scale(model_id).clamp(0.05, 2.0);
    type BestFrame = (
        f32,
        Vec<u8>,
        Vec<u8>,
        usize,
        f64,
        f64,
        Option<(u32, u32, u32, u32)>,
    );
    let mut best: Option<BestFrame> = None;
    for &mult in &[0.5_f32, 1.0, 2.0, 4.0, 8.0, 16.0] {
        let scale = base * mult;
        let dist = (120.0 * scale).clamp(40.0, 800.0);
        let camera = Camera::new(
            Vec3::new(0.0, -dist, dist * 0.25),
            Vec3::new(0.0, 0.0, 30.0 * scale.min(2.0)),
            Vec3::ZERO,
            W as f32 / H as f32,
        );
        gpu.set_view_proj(camera.view_proj().to_cols_array_2d());

        let render_id = |gpu_id: u16, em: &EntityModels, gpu: &mut Gpu, scale: f32| -> Vec<u8> {
            let rig = em
                .skinned_rig(gpu_id)
                .expect("rig retained for equipped mesh");
            let palette = rig.palette_of(&rig.clip, 0.0);
            let gpu_stride = gpu
                .skinned_palette_stride(gpu_id)
                .expect("uploaded skinned mesh") as usize;
            assert_eq!(
                palette.len(),
                gpu_stride,
                "palette len {} != GPU stride {gpu_stride} — update_skinned_instances would drop the draw",
                palette.len()
            );
            let inst = SkinnedInstance {
                pos_yaw: [0.0, 0.0, 0.0, 0.0],
                scale,
                palette_base: 0.0,
            };
            gpu.clear_entity_instances();
            gpu.clear_skinned_instances();
            gpu.update_skinned_instances(gpu_id, &[inst], &palette);
            assert!(
                gpu.skinned_instance_count(gpu_id) > 0,
                "skinned instance count is 0 after update — draw would be skipped"
            );
            gpu.render_to_rgba(0).expect("GPU wait")
        };

        let a = render_id(ga, &em, &mut gpu, scale);
        let b = render_id(gb, &em, &mut gpu, scale);
        let (changed, frac, mean, bbox) = pixel_diff(&a, &b, W, H);
        eprintln!(
            "REQ-020 try scale={scale:.3} changed={changed} ({:.3}%)",
            frac * 100.0
        );
        let better = best.as_ref().is_none_or(|b| changed > b.3);
        if better {
            best = Some((scale, a, b, changed, frac, mean, bbox));
        }
        // Good enough for a visible armour swap.
        if frac >= 0.01 {
            break;
        }
    }
    let (_scale, a, b, changed, frac, mean, bbox) = best.expect("at least one scale tried");
    let bbox_s = bbox
        .map(|(x0, y0, x1, y1)| format!("[{x0},{y0}]-[{x1},{y1}]"))
        .unwrap_or_else(|| "None".into());
    eprintln!(
        "REQ-020 pixel diff: changed={changed}/{total} ({pct:.4}%) mean={mean:.3} bbox={bbox_s} model={model_id}",
        total = (W * H) as usize,
        pct = frac * 100.0
    );

    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/council/measurements/artifacts");
    let _ = std::fs::create_dir_all(&out_dir);
    for (name, rgba) in [("equip_tier1.png", &a), ("equip_tier5.png", &b)] {
        let path = out_dir.join(name);
        let mut file = std::fs::File::create(&path).expect("artifact file");
        let mut enc = png::Encoder::new(&mut file, W, H);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .expect("png hdr")
            .write_image_data(rgba)
            .expect("png data");
    }
    // Visual diff aid for chair review.
    {
        let mut vis = vec![0u8; a.len()];
        for i in (0..a.len()).step_by(4) {
            let d = (0..3)
                .map(|c| a[i + c].abs_diff(b[i + c]))
                .max()
                .unwrap_or(0);
            vis[i] = d;
            vis[i + 1] = d;
            vis[i + 2] = d;
            vis[i + 3] = 255;
        }
        let path = out_dir.join("equip_tier_diff_vis.png");
        let mut file = std::fs::File::create(&path).expect("diff vis");
        let mut enc = png::Encoder::new(&mut file, W, H);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&vis).unwrap();
    }
    let metrics = format!(
        "{{\n  \"model_id\": {model_id},\n  \"gpu_ids\": [{ga}, {gb}],\n  \"index_counts\": [{ia}, {ib}],\n  \
         \"index_ratio\": {best_ratio:.4},\n  \"torso_extensions\": [1, {ext_b}],\n  \
         \"changed_pixels\": {changed},\n  \"changed_fraction\": {frac:.8},\n  \"mean_abs_diff\": {mean:.6},\n  \
         \"diff_bbox\": {bbox_json},\n  \"resolution\": [{W}, {H}],\n  \
         \"artifacts\": [\"equip_tier1.png\", \"equip_tier5.png\", \"equip_tier_diff_vis.png\"]\n}}\n",
        bbox_json = bbox
            .map(|(x0, y0, x1, y1)| format!("[{x0}, {y0}, {x1}, {y1}]"))
            .unwrap_or_else(|| "null".into())
    );
    std::fs::write(out_dir.join("equip_tier_diff.json"), metrics).expect("diff metrics");
    eprintln!(
        "wrote {}/equip_tier{{1,5}}.png + equip_tier_diff.json",
        out_dir.display()
    );

    assert!(
        changed > 0,
        "REQ-020 REJECT: zero differing pixels (mean={mean:.3}, bbox={bbox_s}, model={model_id}, indices={ia}/{ib})"
    );
    // Visible armour change: ≥0.5% of the frame, or a bbox covering ≥2% of the frame.
    let bbox_area = bbox
        .map(|(x0, y0, x1, y1)| (x1 - x0 + 1) * (y1 - y0 + 1))
        .unwrap_or(0);
    let bbox_frac = bbox_area as f64 / f64::from(W * H);
    assert!(
        frac >= 0.005 || bbox_frac >= 0.02,
        "REQ-020: change too subtle for a player to see (changed={frac:.4}, bbox_frac={bbox_frac:.4}, bbox={bbox_s})"
    );
}

/// MS-02b — equipment textures: studded vs plate must look like different armour, not a
/// silhouette-only swap of the same white/fallback texel.
///
/// Uses the **player fig3 avatar** path (`ensure_avatar` + equipment) — the same path `Self_`
/// draws — with real `objects.csv` model ids (81 studded vest, 46 Alb plate breast) →
/// `pskins.csv` → `items/pskins/`. Acceptance is perceptual: chair judges the screenshot pair;
/// the automated floor is a non-zero pixel diff plus a mean-colour shift on character pixels
/// (texture content, not just outline / sky).
#[test]
fn equipment_textures_differ_between_armour_sets_when_client_present() {
    use caer_assets::figures::GENDER_FEMALE;
    use caer_render::entities::EntityModels;
    use caer_render::gpu::Gpu;

    let Some(root) =
        require_client("equipment_textures_differ_between_armour_sets_when_client_present")
    else {
        return;
    };
    if !require_subdir(
        "equipment_textures_differ_between_armour_sets_when_client_present",
        &root,
        "items/pskins",
    ) {
        return;
    }
    let Some(mut em) = EntityModels::load() else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_textures_differ_between_armour_sets_when_client_present",
            "EntityModels::load failed",
        );
        return;
    };

    /// Torso object model id into the EquipmentUpdate (objects.csv id).
    fn equip_torso(model: u16) -> caer_protocol::equipment::EquipmentUpdate {
        let mut w = PacketWriter::new();
        w.u16(1).u8(0).u8(0).u8(0).u8(0).u8(1);
        w.u8(slot::TORSO).u16(model).u8(1); // extension 1 — same mesh tier both sides
        decode(w.as_slice()).expect("equip decode")
    }

    const STUDDED: u16 = 81; // objects.csv Studded Studs Vest → b_std2_body1m / fem
    const PLATE: u16 = 46; // Alb Plate 1 Breast → b_plt1_body1m / fem
    const RACE: u8 = 3; // Highlander — matches live Self_ race

    let mut gpu = pollster::block_on(Gpu::new_headless(pin::W, pin::H, 5_000.0)).expect("gpu init");
    // REQ-020: control must be ~0 before any appearance claim; cite the persisted artifact.
    let control = pin::require_valid_control(&mut em, &mut gpu, RACE, GENDER_FEMALE);

    let eq_s = equip_torso(STUDDED);
    let eq_p = equip_torso(PLATE);
    let Some(ga) = em.ensure_avatar(
        &mut gpu,
        RACE,
        GENDER_FEMALE,
        caer_protocol::customization::Customization::default(),
        Some(&eq_s),
    ) else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_textures_differ_between_armour_sets_when_client_present",
            "ensure_avatar studded failed",
        );
        return;
    };
    let Some(gb) = em.ensure_avatar(
        &mut gpu,
        RACE,
        GENDER_FEMALE,
        caer_protocol::customization::Customization::default(),
        Some(&eq_p),
    ) else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_textures_differ_between_armour_sets_when_client_present",
            "ensure_avatar plate failed",
        );
        return;
    };
    assert_ne!(
        ga, gb,
        "MS-02b: studded and plate must upload distinct GPU meshes"
    );
    assert!(
        gpu.has_skinned_mesh(ga) && gpu.has_skinned_mesh(gb),
        "MS-02b: avatar must be GPU-skinned for per-part pskins"
    );
    eprintln!(
        "MS-02b avatar texture race={RACE} female gpu_ids={ga},{gb} studded={STUDDED} plate={PLATE}"
    );

    let a = pin::render(ga, &em, &mut gpu);
    let b = pin::render(gb, &em, &mut gpu);

    let mut changed = 0usize;
    for i in (0..a.len()).step_by(4) {
        let da = a[i].abs_diff(b[i]) as u64
            + a[i + 1].abs_diff(b[i + 1]) as u64
            + a[i + 2].abs_diff(b[i + 2]) as u64;
        if da > 24 {
            changed += 1;
        }
    }
    let (mean_shift, samples, _, _) = pin::char_mean_shift(&a, &b);
    let frac = changed as f64 / (pin::W * pin::H) as f64;
    eprintln!(
        "MS-02b texture: mean_colour_shift={mean_shift:.2} char_samples={samples} (lead); changed={changed} ({:.3}% frame)",
        frac * 100.0
    );

    let out_dir = pin::artifacts_dir();
    let _ = std::fs::create_dir_all(&out_dir);
    for (name, rgba) in [("equip_studded.png", &a), ("equip_plate.png", &b)] {
        pin::write_png(&out_dir.join(name), rgba);
    }
    let metrics = format!(
        "{{\n  \"path\": \"ensure_avatar\",\n  \"race\": {RACE},\n  \"gender\": \"female\",\n  \
         \"control_diff\": \"{control}\",\n  \
         \"character_samples\": {samples},\n  \
         \"mean_colour_shift\": {mean_shift:.4},\n  \
         \"gpu_ids\": [{ga}, {gb}],\n  \
         \"object_models\": [{STUDDED}, {PLATE}],\n  \"labels\": [\"studded\", \"plate\"],\n  \
         \"changed_pixels\": {changed},\n  \"changed_fraction\": {frac:.8},\n  \
         \"resolution\": [{}, {}],\n  \
         \"pinned\": true,\n  \
         \"artifacts\": [\"equip_studded.png\", \"equip_plate.png\"]\n}}\n",
        pin::W,
        pin::H
    );
    std::fs::write(out_dir.join("equip_texture_diff.json"), metrics).unwrap();

    assert!(
        changed > 0,
        "MS-02b REJECT: zero differing pixels — equipment textures did not bind (mean_shift={mean_shift})"
    );
    assert!(
        frac >= 0.005 || mean_shift >= 8.0,
        "MS-02b: armour sets not perceptually distinct (frac={frac:.4}, mean_shift={mean_shift:.2})"
    );
}

/// MS-02b-full — full plate suit on Self_ (Claude hold before SCN-09).
///
/// Equips Helm+Torso+Arms+Gloves+Legs+Boots (+Cloak) via 0x15 object models from the Alb Plate 1
/// set, renders against torso-only plate, and asserts every equipped slot either binds a pskin or
/// is named as unresolved (honest). Falsifier: a slot stays white while its 0x15 item is present
/// and the pskin resolves in client data.
#[test]
fn equipment_fullset_plate_binds_all_slots_when_client_present() {
    use caer_assets::figures::GENDER_FEMALE;
    use caer_assets::monsters::SkinSlot;
    use caer_assets::pskins::ObjectSkins;
    use caer_render::entities::EntityModels;
    use caer_render::gpu::Gpu;

    let Some(root) = require_client("equipment_fullset_plate_binds_all_slots_when_client_present")
    else {
        return;
    };
    if !require_subdir(
        "equipment_fullset_plate_binds_all_slots_when_client_present",
        &root,
        "items/pskins",
    ) {
        return;
    }
    let Ok(skins) = ObjectSkins::load(root.join("gamedata.mpk")) else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_fullset_plate_binds_all_slots_when_client_present",
            "ObjectSkins::load failed",
        );
        return;
    };
    let Some(mut em) = EntityModels::load() else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_fullset_plate_binds_all_slots_when_client_present",
            "EntityModels::load failed",
        );
        return;
    };

    // Alb Plate 1 set. Cloak is proven separately — wearing it in the acceptance shot hides
    // arms/legs/torso and defeats the "head-to-toe plate" perceptual check.
    const HELM: u16 = 64;
    const TORSO: u16 = 46;
    const LEGS: u16 = 47;
    const ARMS: u16 = 48;
    const GLOVES: u16 = 49;
    const BOOTS: u16 = 50;
    const CLOAK: u16 = 57;
    const RACE: u8 = 3;

    let plate_slots: &[(u8, u16, &str)] = &[
        (slot::HELM, HELM, "Helm"),
        (slot::TORSO, TORSO, "Torso"),
        (slot::ARMS, ARMS, "Arms"),
        (slot::HANDS, GLOVES, "Gloves"),
        (slot::LEGS, LEGS, "Legs"),
        (slot::FEET, BOOTS, "Boots"),
    ];
    let cloak_slot: (u8, u16, &str) = (slot::CLOAK, CLOAK, "Cloak");

    // Per-slot pskin resolution (female) — honest log of anything missing in client data.
    let mut slot_status: Vec<(String, String, String)> = Vec::new();
    for &(_, model, name) in plate_slots.iter().chain(std::iter::once(&cloak_slot)) {
        let resolved = skins.skins_for_object(model, true);
        if resolved.is_empty() {
            slot_status.push((
                name.into(),
                "unresolved_pskin".into(),
                format!("object {model}"),
            ));
            continue;
        }
        let dds: Vec<_> = resolved
            .iter()
            .map(|(s, r)| format!("{s:?}:{}", r.dds))
            .collect();
        slot_status.push((name.into(), "pskin_ok".into(), dds.join(",")));
    }
    for (name, st, detail) in &slot_status {
        eprintln!("MS-02b-full slot {name}: {st} ({detail})");
    }
    assert!(
        slot_status
            .iter()
            .filter(|(_, st, _)| st == "pskin_ok")
            .count()
            >= 7,
        "expected Alb Plate 1 + cloak to resolve pskins"
    );

    fn equip_items(items: &[(u8, u16)]) -> caer_protocol::equipment::EquipmentUpdate {
        let mut w = PacketWriter::new();
        w.u16(1).u8(0).u8(0).u8(0).u8(0).u8(items.len() as u8);
        for &(s, model) in items {
            w.u8(s).u16(model).u8(1);
        }
        decode(w.as_slice()).expect("equip decode")
    }

    let torso_only = equip_items(&[(slot::TORSO, TORSO)]);
    let plate: Vec<(u8, u16)> = plate_slots.iter().map(|&(s, m, _)| (s, m)).collect();
    let plate_set = equip_items(&plate);
    let mut with_cloak = plate.clone();
    with_cloak.push((cloak_slot.0, cloak_slot.1));
    let cloak_set = equip_items(&with_cloak);

    let mut gpu = pollster::block_on(Gpu::new_headless(pin::W, pin::H, 5_000.0)).expect("gpu init");
    let control = pin::require_valid_control(&mut em, &mut gpu, RACE, GENDER_FEMALE);

    let Some(g_torso) = em.ensure_avatar(
        &mut gpu,
        RACE,
        GENDER_FEMALE,
        caer_protocol::customization::Customization::default(),
        Some(&torso_only),
    ) else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_fullset_plate_binds_all_slots_when_client_present",
            "torso-only avatar failed",
        );
        return;
    };
    let Some(g_full) = em.ensure_avatar(
        &mut gpu,
        RACE,
        GENDER_FEMALE,
        caer_protocol::customization::Customization::default(),
        Some(&plate_set),
    ) else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_fullset_plate_binds_all_slots_when_client_present",
            "full-set avatar failed",
        );
        return;
    };
    let Some(g_cloak) = em.ensure_avatar(
        &mut gpu,
        RACE,
        GENDER_FEMALE,
        caer_protocol::customization::Customization::default(),
        Some(&cloak_set),
    ) else {
        caer_assets::client_dep::skip_or_fail(
            "equipment_fullset_plate_binds_all_slots_when_client_present",
            "cloak-set avatar failed",
        );
        return;
    };
    assert_ne!(g_torso, g_full);
    assert!(gpu.has_skinned_mesh(g_full), "full set must be GPU-skinned");
    assert!(
        gpu.has_skinned_mesh(g_cloak),
        "cloak set must be GPU-skinned"
    );

    // Mesh coverage: every SkinSlot contributed by the suit must appear as a named batch part.
    let part_names_of = |em: &EntityModels, id: u16| -> Vec<String> {
        em.skinned_rig(id)
            .expect("rig")
            .rigs
            .iter()
            .flat_map(|r| r.parts.iter().map(|p| p.name.to_ascii_lowercase()))
            .collect()
    };
    let part_names = part_names_of(&em, g_full);
    let cloak_names = part_names_of(&em, g_cloak);
    let has = |names: &[String], prefix: &str| names.iter().any(|n| n.starts_with(prefix));
    let mesh_checks = [
        ("body", has(&part_names, "body")),
        ("lbody", has(&part_names, "lbody")),
        ("arms", has(&part_names, "arms")),
        ("gloves", has(&part_names, "gloves")),
        ("legs", has(&part_names, "legs")),
        ("boots", has(&part_names, "boots")),
        ("helm", has(&part_names, "helm")),
        ("cloak", has(&cloak_names, "cloak")),
    ];
    for (slot, ok) in mesh_checks {
        eprintln!(
            "MS-02b-full mesh part {slot}: {}",
            if ok { "present" } else { "MISSING" }
        );
        assert!(
            ok,
            "MS-02b-full: fig3 mesh missing for slot {slot} while 0x15 is present"
        );
    }

    // Confirm ObjectSkins maps each equipped model onto a SkinSlot that from_part_name can hit.
    for &(_, model, name) in plate_slots {
        for (sk, _) in skins.skins_for_object(model, true) {
            let prefix = match sk {
                SkinSlot::Body => "body",
                SkinSlot::Lbody => "lbody",
                SkinSlot::Arms => "arms",
                SkinSlot::Gloves => "gloves",
                SkinSlot::Legs => "legs",
                SkinSlot::Boots => "boots",
                SkinSlot::Helm => "helm",
                SkinSlot::Cloak => "cloak",
                other => panic!("unexpected slot {other:?} on {name}"),
            };
            assert!(
                has(&part_names, prefix),
                "MS-02b-full: pskin slot {sk:?} from {name} has no matching fig3 part"
            );
        }
    }
    assert!(
        has(&cloak_names, "cloak"),
        "MS-02b-full: cloak pskin has no matching fig3 Cloak part"
    );

    // Pinned camera/pose — same [`pin`] as studded/plate MS-02b (no independent framing).
    let a = pin::render(g_torso, &em, &mut gpu);
    let b = pin::render(g_full, &em, &mut gpu);

    let mut changed = 0usize;
    for i in (0..a.len()).step_by(4) {
        let da = a[i].abs_diff(b[i]) as u64
            + a[i + 1].abs_diff(b[i + 1]) as u64
            + a[i + 2].abs_diff(b[i + 2]) as u64;
        if da > 24 {
            changed += 1;
        }
    }
    let (mean_shift, samples, _, _) = pin::char_mean_shift(&a, &b);
    let frac = changed as f64 / (pin::W * pin::H) as f64;
    eprintln!(
        "MS-02b-full vs torso-only: mean_colour_shift={mean_shift:.2} char_samples={samples} (lead); changed={changed} ({:.3}% frame)",
        frac * 100.0
    );

    let out_dir = pin::artifacts_dir();
    let _ = std::fs::create_dir_all(&out_dir);
    pin::write_png(&out_dir.join("equip_fullset_plate.png"), &b);
    let slots_json = slot_status
        .iter()
        .map(|(n, st, d)| {
            format!("    {{\"slot\": \"{n}\", \"status\": \"{st}\", \"detail\": \"{d}\"}}")
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let metrics = format!(
        "{{\n  \"path\": \"ensure_avatar_fullset\",\n  \"race\": {RACE},\n  \"gender\": \"female\",\n  \
         \"control_diff\": \"{control}\",\n  \
         \"character_samples\": {samples},\n  \
         \"mean_colour_shift\": {mean_shift:.4},\n  \
         \"baseline\": \"torso-only plate under pin\",\n  \
         \"note\": \"acceptance shot omits cloak so limbs stay visible; cloak mesh+pskin proven on gpu_id {g_cloak}\",\n  \
         \"gpu_ids\": [{g_torso}, {g_full}, {g_cloak}],\n  \
         \"object_models\": {{\"helm\": {HELM}, \"torso\": {TORSO}, \"legs\": {LEGS}, \"arms\": {ARMS}, \
\"gloves\": {GLOVES}, \"boots\": {BOOTS}, \"cloak\": {CLOAK}}},\n  \
         \"slots\": [\n{slots_json}\n  ],\n  \
         \"changed_pixels\": {changed},\n  \"changed_fraction\": {frac:.8},\n  \
         \"resolution\": [{}, {}],\n  \"pinned\": true,\n  \
         \"artifacts\": [\"equip_fullset_plate.png\"]\n}}\n",
        pin::W,
        pin::H
    );
    std::fs::write(out_dir.join("equip_fullset_diff.json"), metrics).unwrap();

    assert!(
        changed > 0 && mean_shift >= 8.0,
        "MS-02b-full: full suit not perceptually distinct from torso-only (frac={frac:.4}, mean_shift={mean_shift:.2})"
    );
}
