//! REQ-020 particle subsystem evidence: deterministic offscreen frame sequences under `pin`.
//!
//! Fixed timestep + fixed seed. Identical-seed twin must diff ~0. Presence is asserted; **effect
//! fidelity is not claimed** (awaits perf — Claude GO 2026-08-08).

use caer_assets::nif::read_particle_emitters;
use caer_render::camera::Camera;
use caer_render::gpu::Gpu;
use caer_render::particles::ParticleSystem;
use glam::Vec3;

const W: u32 = 640;
const H: u32 = 480;
const SEED: u64 = 0xCAE0_DEAD_BEEFu64;
const DT: f32 = 1.0 / 30.0;
const FRAMES: usize = 8;

fn pin_camera(gpu: &mut Gpu) {
    let camera = Camera::new(
        Vec3::new(63.0, -180.0, 63.0),
        Vec3::new(0.0, 0.0, 55.0),
        Vec3::ZERO,
        W as f32 / H as f32,
    );
    gpu.set_sky(caer_assets::sky::Sky::default().day);
    gpu.set_view_proj(camera.view_proj().to_cols_array_2d());
}

fn near_sky(r: u8, g: u8, b: u8) -> bool {
    let dr = i32::from(r) - 130;
    let dg = i32::from(g) - 160;
    let db = i32::from(b) - 208;
    dr * dr + dg * dg + db * db < 40 * 40
}

fn non_sky_samples(rgba: &[u8]) -> u64 {
    let mut n = 0u64;
    for i in (0..rgba.len()).step_by(4) {
        if !near_sky(rgba[i], rgba[i + 1], rgba[i + 2]) {
            n += 1;
        }
    }
    n
}

fn changed_pixels(a: &[u8], b: &[u8]) -> usize {
    assert_eq!(a.len(), b.len());
    let mut n = 0usize;
    for i in (0..a.len()).step_by(4) {
        let da = a[i].abs_diff(b[i]) as u32
            + a[i + 1].abs_diff(b[i + 1]) as u32
            + a[i + 2].abs_diff(b[i + 2]) as u32;
        if da > 0 {
            n += 1;
        }
    }
    n
}

fn write_png(path: &std::path::Path, rgba: &[u8]) {
    let mut file = std::fs::File::create(path).expect("png");
    let mut enc = png::Encoder::new(&mut file, W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

fn load_emitter() -> Option<caer_assets::nif::ParticleEmitterDef> {
    let root = caer_assets::client_dep::require_caer_client(
        "particle_sequence_deterministic_under_pin_when_client_present",
    )?;
    let acid = root.join("effects/acidglob.NIF");
    if acid.is_file() {
        let bytes = std::fs::read(&acid).ok()?;
        let mut emitters = read_particle_emitters(&bytes).ok()?;
        return emitters.pop();
    }
    let npk = root.join("zones/Nifs/BAvTeleporter.npk");
    if npk.is_file() {
        let members = caer_assets::open(&npk).ok()?;
        let nif = members
            .iter()
            .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))?;
        let emitters = read_particle_emitters(&nif.data).ok()?;
        return emitters
            .into_iter()
            .max_by(|a, b| a.emit_rate.total_cmp(&b.emit_rate));
    }
    caer_assets::client_dep::skip_or_fail(
        "particle_sequence_deterministic_under_pin_when_client_present",
        "no acidglob.NIF or BAvTeleporter.npk under CAER_CLIENT",
    );
    None
}

fn capture_sequence(sys: &mut ParticleSystem, gpu: &mut Gpu) -> Vec<Vec<u8>> {
    let mut frames = Vec::with_capacity(FRAMES);
    for _ in 0..FRAMES {
        sys.tick(DT);
        let inst = sys.instances();
        // Scale down huge NIF sizes so cubes sit in the pin frustum without filling the sky.
        let inst: Vec<_> = inst
            .into_iter()
            .map(|mut i| {
                i.scale = i.scale.clamp(0.5, 8.0);
                // Lift origin into camera look region if authored at mesh-local zero.
                i.pos[2] += 40.0;
                i
            })
            .collect();
        gpu.upload_instances(&inst);
        frames.push(gpu.render_to_rgba(inst.len() as u32).expect("GPU wait"));
    }
    frames
}

#[test]
fn particle_sequence_deterministic_under_pin_when_client_present() {
    let Some(def) = load_emitter() else {
        return;
    };
    eprintln!(
        "particle evidence emitter={:?} rate={} life={} size={}",
        def.name, def.emit_rate, def.lifetime, def.size
    );

    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 5_000.0)).expect("gpu init");
    pin_camera(&mut gpu);

    let mut a = ParticleSystem::from_def(def.clone(), SEED);
    let mut b = ParticleSystem::from_def(def, SEED);
    let frames_a = capture_sequence(&mut a, &mut gpu);
    let frames_b = capture_sequence(&mut b, &mut gpu);

    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/council/measurements/artifacts");
    let _ = std::fs::create_dir_all(&out);

    let mut control_changed = 0usize;
    let mut max_non_sky = 0u64;
    let mut vs_f0 = Vec::new();
    for i in 0..FRAMES {
        control_changed += changed_pixels(&frames_a[i], &frames_b[i]);
        let ns = non_sky_samples(&frames_a[i]);
        max_non_sky = max_non_sky.max(ns);
        let d0 = changed_pixels(&frames_a[0], &frames_a[i]);
        vs_f0.push(d0);
        write_png(&out.join(format!("particle_seq_f{i}.png")), &frames_a[i]);
    }

    let control_json = format!(
        "{{\n  \"kind\": \"particle_seq_control_diff\",\n  \"req\": \"REQ-020\",\n  \
         \"seed\": {SEED},\n  \"dt\": {DT},\n  \"frames\": {FRAMES},\n  \
         \"changed_pixels_total\": {control_changed},\n  \"valid\": {},\n  \
         \"resolution\": [{W}, {H}],\n  \"pinned\": true\n}}\n",
        control_changed == 0
    );
    std::fs::write(out.join("particle_seq_control_diff.json"), control_json).unwrap();

    let seq_json = format!(
        "{{\n  \"kind\": \"particle_sequence\",\n  \"seed\": {SEED},\n  \"dt\": {DT},\n  \
         \"frames\": {FRAMES},\n  \
         \"character_samples_max\": {max_non_sky},\n  \
         \"changed_vs_frame0\": {vs_f0:?},\n  \
         \"control_diff\": \"particle_seq_control_diff.json\",\n  \
         \"fidelity_claimed\": false,\n  \
         \"fidelity_note\": \"OPEN until perf — presence + determinism only\",\n  \
         \"artifacts\": [\"particle_seq_f0.png\" .. \"particle_seq_f{}.png\"]\n}}\n",
        FRAMES - 1
    );
    std::fs::write(out.join("particle_seq.json"), seq_json).unwrap();

    eprintln!(
        "particle seq: control_changed={control_changed} max_non_sky={max_non_sky} vs_f0={vs_f0:?}"
    );

    assert_eq!(
        control_changed, 0,
        "REQ-020: identical-seed twin sequence must diff 0 (got {control_changed})"
    );
    assert!(
        max_non_sky > 100,
        "presence: expected non-sky particle pixels, got max_non_sky={max_non_sky}"
    );
    // Motion / emission over time — not a flat freeze.
    assert!(
        vs_f0.iter().any(|&d| d > 0),
        "sequence must change across frames (vs_f0={vs_f0:?})"
    );
}
