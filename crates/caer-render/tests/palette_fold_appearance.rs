//! MS-08 GPU palette-fold appearance gate (Claude blocking ask + follow-up gaps).
//!
//! Perf is insensitive to a wrong fold. Pin-based instrument:
//! 1. CONTROL — noise floor (GPU fold ×2)
//! 2. CLAIM — CPU fold vs GPU fold within tol, **with upload-counter witnesses** that prove
//!    which producer ran (MS-02 shape: 0≡0 without a witness is vacuous)
//! 3. FALSIFIER (gross) — skip dispatch; MUST exceed tol
//! 4. FALSIFIER (one-part) — `part_offset=1`; scored by **max-band** shift so a localized error
//!    is not diluted by whole-image mean under parts≈18
//!
//! Pin knobs match `equipment_appearance::pin` (REQ-020): fixed camera/pose/light/sky.

use caer_assets::figures::GENDER_FEMALE;
use caer_assets::sky::Sky;
use caer_render::camera::Camera;
use caer_render::entities::{EntityModels, Loco};
use caer_render::gpu::{Gpu, SkinnedInstance};
use glam::Vec3;
use std::path::{Path, PathBuf};

const W: u32 = 640;
const H: u32 = 480;
const SCALE: f32 = 1.0;
const YAW: f32 = 0.0;
const EYE: [f32; 3] = [63.0, -180.0, 63.0];
const LOOK: [f32; 3] = [0.0, 0.0, 55.0];
const RACE: u8 = 3;

/// Bind-pose sample used by the control + claim.
const CLIP_T0: f32 = 0.0;

/// Claim band for character-sample mean RGB Euclidean shift (0–255 scale).
///
/// **Justification (stated before measurement, not tuned to pass):** GPU mat4 fold vs CPU
/// `xform_mul`→mat4 differs in low bits (~1e-3 abs on matrix elements per `FOLD_ABS_TOL`). On an
/// 8-bit framebuffer that error is sub-LSB for nearly all shaded pixels, so the expected claim
/// shift is on the order of the control noise floor (0 changed pixels / ≪1 mean shift). A band of
/// **1.0** is above any plausible sub-LSB flicker while remaining well below armour deltas (46–92)
/// and below an idle↔walk pose change under this pin. Widening it to make the claim pass would be
/// the inverted MS-02 failure mode Claude named.
const CLAIM_MEAN_SHIFT_TOL: f64 = 1.0;

/// Returns the guard, not a path: `ClientDep::drop` emits the completion marker, so the caller
/// holds it for the test body. It derefs to `Path`, so use sites are unchanged.
fn require_client(test: &str) -> Option<caer_assets::client_dep::ClientDep> {
    caer_assets::client_dep::require_caer_client(test)
}

fn artifacts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/council/measurements/artifacts")
}

fn apply_pin(gpu: &mut Gpu) {
    let camera = Camera::new(
        Vec3::from_array(EYE),
        Vec3::from_array(LOOK),
        Vec3::ZERO,
        W as f32 / H as f32,
    );
    gpu.set_sky(Sky::default().day);
    gpu.set_view_proj(camera.view_proj().to_cols_array_2d());
}

fn near_sky(r: u8, g: u8, b: u8) -> bool {
    let dr = i32::from(r) - 130;
    let dg = i32::from(g) - 160;
    let db = i32::from(b) - 208;
    dr * dr + dg * dg + db * db < 40 * 40
}

fn char_mean_shift(a: &[u8], b: &[u8]) -> (f64, u64) {
    let mut mean_a = [0u64; 3];
    let mut mean_b = [0u64; 3];
    let mut samples = 0u64;
    for i in (0..a.len()).step_by(4) {
        if near_sky(a[i], a[i + 1], a[i + 2]) && near_sky(b[i], b[i + 1], b[i + 2]) {
            continue;
        }
        mean_a[0] += u64::from(a[i]);
        mean_a[1] += u64::from(a[i + 1]);
        mean_a[2] += u64::from(a[i + 2]);
        mean_b[0] += u64::from(b[i]);
        mean_b[1] += u64::from(b[i + 1]);
        mean_b[2] += u64::from(b[i + 2]);
        samples += 1;
    }
    if samples == 0 {
        return (0.0, 0);
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
    (shift, samples)
}

/// Max per-band character mean shift — resists dilution of a one-part error by parts≈18.
///
/// Horizontal bands over the frame; sky-only bands are skipped. Lead statistic for the
/// part-offset falsifier (Claude: whole-image mean of a 1/18 error ≈ 0.18 ≪ tol 1.0).
fn max_band_shift(a: &[u8], b: &[u8], bands: u32) -> (f64, u32) {
    assert_eq!(a.len(), b.len());
    assert_eq!(a.len(), (W * H * 4) as usize);
    let bands = bands.max(1);
    let rows_per = H.div_ceil(bands);
    let mut max_shift = 0.0_f64;
    let mut bands_scored = 0u32;
    for band in 0..bands {
        let y0 = band * rows_per;
        let y1 = ((band + 1) * rows_per).min(H);
        if y0 >= y1 {
            continue;
        }
        let mut mean_a = [0u64; 3];
        let mut mean_b = [0u64; 3];
        let mut samples = 0u64;
        for y in y0..y1 {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                if near_sky(a[i], a[i + 1], a[i + 2]) && near_sky(b[i], b[i + 1], b[i + 2]) {
                    continue;
                }
                mean_a[0] += u64::from(a[i]);
                mean_a[1] += u64::from(a[i + 1]);
                mean_a[2] += u64::from(a[i + 2]);
                mean_b[0] += u64::from(b[i]);
                mean_b[1] += u64::from(b[i + 1]);
                mean_b[2] += u64::from(b[i + 2]);
                samples += 1;
            }
        }
        if samples == 0 {
            continue;
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
        max_shift = max_shift.max(shift);
        bands_scored += 1;
    }
    (max_shift, bands_scored)
}

fn changed_pixels(a: &[u8], b: &[u8], thresh: u8) -> usize {
    assert_eq!(a.len(), b.len());
    let mut n = 0usize;
    for i in (0..a.len()).step_by(4) {
        let da = u32::from(a[i].abs_diff(b[i]))
            + u32::from(a[i + 1].abs_diff(b[i + 1]))
            + u32::from(a[i + 2].abs_diff(b[i + 2]));
        if da > u32::from(thresh) {
            n += 1;
        }
    }
    n
}

fn write_png(path: &Path, rgba: &[u8]) {
    let mut file = std::fs::File::create(path).expect("artifact png");
    let mut enc = png::Encoder::new(&mut file, W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

fn render_cpu_fold(
    gpu_id: u16,
    em: &EntityModels,
    gpu: &mut Gpu,
    loco: Loco,
    clip_t: f32,
) -> Vec<u8> {
    let rig = em.skinned_rig(gpu_id).expect("rig");
    let (clip, _) = rig.clip_for_state(loco);
    // `clip_t` is absolute; clamp into clip duration for stable sampling.
    let t = if clip.duration > 0.0 {
        clip_t.rem_euclid(clip.duration)
    } else {
        0.0
    };
    let palette = rig.palette_of(clip, t);
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

fn render_gpu_fold(
    gpu_id: u16,
    em: &EntityModels,
    gpu: &mut Gpu,
    loco: Loco,
    clip_t: f32,
) -> Vec<u8> {
    let rig = em.skinned_rig(gpu_id).expect("rig");
    let (clip, _) = rig.clip_for_state(loco);
    let t = if clip.duration > 0.0 {
        clip_t.rem_euclid(clip.duration)
    } else {
        0.0
    };
    let mut world = Vec::new();
    let mut bones = Vec::new();
    rig.world_bones_of_extend(clip, t, &mut world, &mut bones);
    let inst = SkinnedInstance {
        pos_yaw: [0.0, 0.0, 0.0, YAW],
        scale: SCALE,
        palette_base: 0.0,
    };
    gpu.clear_entity_instances();
    gpu.clear_skinned_instances();
    gpu.update_skinned_bones(gpu_id, &[inst], &bones);
    gpu.render_to_rgba(0).expect("GPU wait")
}

/// Three-point fold correctness: control → claim → falsifier. All three required.
#[test]
fn palette_fold_pin_control_claim_falsifier_when_client_present() {
    let Some(root) = require_client("palette_fold_pin_control_claim_falsifier_when_client_present")
    else {
        return;
    };
    if !root.join("figures").is_dir() {
        caer_assets::client_dep::skip_or_fail(
            "palette_fold_pin_control_claim_falsifier_when_client_present",
            "missing figures under CAER_CLIENT",
        );
        return;
    }
    let Some(mut em) = EntityModels::load() else {
        caer_assets::client_dep::skip_or_fail(
            "palette_fold_pin_control_claim_falsifier_when_client_present",
            "EntityModels::load failed",
        );
        return;
    };
    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 5_000.0)).expect("gpu init");
    apply_pin(&mut gpu);
    let Some(gpu_id) = em.ensure_avatar(
        &mut gpu,
        RACE,
        GENDER_FEMALE,
        caer_protocol::customization::Customization::default(),
        None,
    ) else {
        caer_assets::client_dep::skip_or_fail(
            "palette_fold_pin_control_claim_falsifier_when_client_present",
            "ensure_avatar failed",
        );
        return;
    };

    // --- (1) CONTROL: same GPU-fold path twice → noise floor ---
    gpu.set_debug_skip_palette_fold(false);
    let ctrl_a = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Idle, CLIP_T0);
    let ctrl_b = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Idle, CLIP_T0);
    let ctrl_changed = changed_pixels(&ctrl_a, &ctrl_b, 0);
    let (ctrl_shift, ctrl_samples) = char_mean_shift(&ctrl_a, &ctrl_b);
    assert!(
        ctrl_samples > 0,
        "control: no character samples (avatar missing / all sky?)"
    );
    assert_eq!(
        ctrl_changed, 0,
        "control: GPU-fold twin captures must be bit-identical under pin (changed={ctrl_changed}, mean_shift={ctrl_shift:.4})"
    );

    // --- (2) CLAIM: CPU fold vs GPU fold + upload-counter witnesses (prove producers differ) ---
    gpu.begin_upload_stats();
    let cpu = render_cpu_fold(gpu_id, &em, &mut gpu, Loco::Idle, CLIP_T0);
    let cpu_up = gpu.take_upload_stats();
    assert!(
        cpu_up.skinned_palette_bytes > 0 && cpu_up.skinned_bone_bytes == 0,
        "claim witness: CPU path must upload palette bytes and zero bone bytes \
         (palette={}, bone={}) — without this, 0≡0 claim cannot prove CAER_CPU_PALETTE_FOLD engaged",
        cpu_up.skinned_palette_bytes,
        cpu_up.skinned_bone_bytes,
    );

    gpu.begin_upload_stats();
    let gpu_img = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Idle, CLIP_T0);
    let gpu_up = gpu.take_upload_stats();
    assert!(
        gpu_up.skinned_bone_bytes > 0 && gpu_up.skinned_palette_bytes == 0,
        "claim witness: GPU fold path must upload bone bytes and zero palette bytes \
         (palette={}, bone={})",
        gpu_up.skinned_palette_bytes,
        gpu_up.skinned_bone_bytes,
    );

    let (claim_shift, claim_samples) = char_mean_shift(&cpu, &gpu_img);
    let (claim_max_band, claim_bands) = max_band_shift(&cpu, &gpu_img, 16);
    let claim_changed = changed_pixels(&cpu, &gpu_img, 0);
    assert!(
        claim_shift <= CLAIM_MEAN_SHIFT_TOL,
        "claim: CPU vs GPU fold mean_colour_shift={claim_shift:.4} exceeds justified tol {CLAIM_MEAN_SHIFT_TOL} (changed={claim_changed}, samples={claim_samples})"
    );
    assert!(
        claim_max_band <= CLAIM_MEAN_SHIFT_TOL,
        "claim: max_band_shift={claim_max_band:.4} exceeds tol {CLAIM_MEAN_SHIFT_TOL} (bands={claim_bands}) — score claim on max-band, not only whole-image mean"
    );

    // --- (3) FALSIFIER (gross): seed idle, skip dispatch while uploading walk ---
    let _seed = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Idle, CLIP_T0);
    gpu.set_debug_skip_palette_fold(true);
    let stale = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Walk, 0.45);
    gpu.set_debug_skip_palette_fold(false);
    let correct_walk = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Walk, 0.45);
    let (false_shift, false_samples) = char_mean_shift(&stale, &correct_walk);
    let idle = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Idle, CLIP_T0);
    let walk = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Walk, 0.45);
    let (pose_delta, _) = char_mean_shift(&idle, &walk);
    assert!(
        pose_delta > CLAIM_MEAN_SHIFT_TOL,
        "falsifier vacuous: idle vs walk mean_shift={pose_delta:.4} ≤ tol — walk clip missing or static"
    );
    assert!(
        false_shift > CLAIM_MEAN_SHIFT_TOL,
        "falsifier: skipped fold must EXCEED tol {CLAIM_MEAN_SHIFT_TOL}, got mean_shift={false_shift:.4} (samples={false_samples}) — tolerance unfalsifiable / fold not load-bearing"
    );

    // --- (4) FALSIFIER (part-index offset): part_offset=1 — must fire under max-band ---
    const BANDS: u32 = 16;
    gpu.set_debug_fold_part_offset(0);
    let part_ok = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Idle, CLIP_T0);
    gpu.set_debug_fold_part_offset(1);
    let part_bad = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Idle, CLIP_T0);
    gpu.set_debug_fold_part_offset(0);
    let (part_whole, part_samples) = char_mean_shift(&part_ok, &part_bad);
    let (part_max_band, bands_scored) = max_band_shift(&part_ok, &part_bad, BANDS);
    assert!(bands_scored > 0, "part-offset falsifier: no bands scored");
    assert!(
        part_max_band > CLAIM_MEAN_SHIFT_TOL,
        "part-offset falsifier: max_band_shift={part_max_band:.4} must EXCEED tol {CLAIM_MEAN_SHIFT_TOL} \
         (whole_mean={part_whole:.4}) — part-index errors must be visible"
    );

    // --- (5) FALSIFIER (tail-stale / one-part): omit last part while folding walk over idle seed ---
    // Most of the body updates; last part retains idle. Whole-image mean can dilute below tol;
    // max-band must still fire (Claude: 1/parts dilution under whole-image scoring).
    let _idle_seed = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Idle, CLIP_T0);
    gpu.set_debug_fold_omit_last_part(true);
    let tail_stale = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Walk, 0.45);
    gpu.set_debug_fold_omit_last_part(false);
    let full_walk = render_gpu_fold(gpu_id, &em, &mut gpu, Loco::Walk, 0.45);
    let (tail_whole, tail_samples) = char_mean_shift(&tail_stale, &full_walk);
    let (tail_max_band, tail_bands) = max_band_shift(&tail_stale, &full_walk, BANDS);
    assert!(
        tail_max_band > CLAIM_MEAN_SHIFT_TOL,
        "tail-stale falsifier: max_band_shift={tail_max_band:.4} must EXCEED tol {CLAIM_MEAN_SHIFT_TOL} \
         (whole_mean={tail_whole:.4}, samples={tail_samples}, bands={tail_bands})"
    );

    let out = artifacts_dir();
    let _ = std::fs::create_dir_all(&out);
    write_png(&out.join("fold_pin_control_a.png"), &ctrl_a);
    write_png(&out.join("fold_pin_cpu.png"), &cpu);
    write_png(&out.join("fold_pin_gpu.png"), &gpu_img);
    write_png(&out.join("fold_pin_stale.png"), &stale);
    write_png(&out.join("fold_pin_correct_walk.png"), &correct_walk);
    write_png(&out.join("fold_pin_part_offset.png"), &part_bad);
    write_png(&out.join("fold_pin_tail_stale.png"), &tail_stale);
    let json = format!(
        "{{\n  \"kind\": \"palette_fold_pin\",\n  \"req\": \"MS-08 fold correctness\",\n  \
         \"claim_mean_shift_tol\": {CLAIM_MEAN_SHIFT_TOL},\n  \
         \"tol_justification\": \"sub-LSB expected from FOLD_ABS_TOL mat4 error on 8-bit FB; 1.0 >> control(0), << armour/idle-walk deltas\",\n  \
         \"control\": {{\"changed_pixels\": {ctrl_changed}, \"mean_colour_shift\": {ctrl_shift:.6}, \"character_samples\": {ctrl_samples}}},\n  \
         \"claim\": {{\"mean_colour_shift\": {claim_shift:.6}, \"max_band_shift\": {claim_max_band:.6}, \"changed_pixels\": {claim_changed}, \"character_samples\": {claim_samples}, \
\"cpu_palette_bytes\": {}, \"cpu_bone_bytes\": {}, \"gpu_palette_bytes\": {}, \"gpu_bone_bytes\": {}, \"pass\": {}}},\n  \
         \"falsifier_gross\": {{\"mean_colour_shift\": {false_shift:.6}, \"character_samples\": {false_samples}, \"pose_delta_idle_walk\": {pose_delta:.6}, \"pass\": {}}},\n  \
         \"falsifier_part_offset\": {{\"whole_mean_shift\": {part_whole:.6}, \"max_band_shift\": {part_max_band:.6}, \"bands\": {BANDS}, \"bands_scored\": {bands_scored}, \"character_samples\": {part_samples}, \"pass\": {}}},\n  \
         \"falsifier_tail_stale\": {{\"whole_mean_shift\": {tail_whole:.6}, \"max_band_shift\": {tail_max_band:.6}, \"bands\": {BANDS}, \"bands_scored\": {tail_bands}, \"character_samples\": {tail_samples}, \"whole_below_tol\": {}, \"pass\": {}}},\n  \
         \"resolution\": [{W}, {H}], \"clip_t0\": {CLIP_T0},\n  \
         \"pinned\": true\n}}\n",
        cpu_up.skinned_palette_bytes,
        cpu_up.skinned_bone_bytes,
        gpu_up.skinned_palette_bytes,
        gpu_up.skinned_bone_bytes,
        claim_shift <= CLAIM_MEAN_SHIFT_TOL && claim_max_band <= CLAIM_MEAN_SHIFT_TOL,
        false_shift > CLAIM_MEAN_SHIFT_TOL,
        part_max_band > CLAIM_MEAN_SHIFT_TOL,
        tail_whole < CLAIM_MEAN_SHIFT_TOL,
        tail_max_band > CLAIM_MEAN_SHIFT_TOL,
    );
    std::fs::write(out.join("fold_pin_diff.json"), json).expect("write fold_pin_diff.json");
    eprintln!(
        "fold_pin: control={ctrl_shift:.4} claim={claim_shift:.4} (cpu_pal={} gpu_bone={}) gross={false_shift:.4} part_whole={part_whole:.4} part_max_band={part_max_band:.4} tail_whole={tail_whole:.4} tail_max_band={tail_max_band:.4} tol={CLAIM_MEAN_SHIFT_TOL}",
        cpu_up.skinned_palette_bytes,
        gpu_up.skinned_bone_bytes,
    );
}
