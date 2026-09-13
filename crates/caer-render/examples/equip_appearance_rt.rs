//! SCN-06 REQ-020 appearance half — pin harness control ~0 + studded vs plate pixel proof.
//!
//! Claude (2026-08-09): SCN-06 must not Pass on wield chat. The binding assertion is screenshot
//! diff on the character under [`equipment_appearance`] pin (fixed camera / bind pose / day sky),
//! with the REQ-020 control still reading ~0 so the harness is live.
//!
//! ```bash
//! CAER_CLIENT=/path/to/retail-client cargo \
//!   run -p caer-render --example equip_appearance_rt
//! ```

use caer_assets::figures::GENDER_FEMALE;
use caer_assets::sky::Sky;
use caer_protocol::codec::PacketWriter;
use caer_protocol::equipment::{decode, slot};
use caer_render::camera::Camera;
use caer_render::entities::EntityModels;
use caer_render::gpu::{Gpu, SkinnedInstance};
use glam::Vec3;
use std::path::{Path, PathBuf};

const W: u32 = 640;
const H: u32 = 480;
const SCALE: f32 = 1.0;
const CLIP_T: f32 = 0.0;
const YAW: f32 = 0.0;
const EYE: [f32; 3] = [63.0, -180.0, 63.0];
const LOOK: [f32; 3] = [0.0, 0.0, 55.0];
const RACE: u8 = 3;
const STUDDED: u16 = 81;
const PLATE: u16 = 46;

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

fn render(gpu_id: u16, em: &EntityModels, gpu: &mut Gpu) -> Vec<u8> {
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

fn changed_pixels(a: &[u8], b: &[u8], thresh: u8) -> usize {
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

fn near_sky(r: u8, g: u8, b: u8) -> bool {
    let dr = i32::from(r) - 130;
    let dg = i32::from(g) - 160;
    let db = i32::from(b) - 208;
    dr * dr + dg * dg + db * db < 40 * 40
}

fn char_mean_shift(a: &[u8], b: &[u8]) -> (f64, u64) {
    let mut mean_a = [0u64; 3];
    let mut mean_b = [0u64; 3];
    let mut n = 0u64;
    for i in (0..a.len()).step_by(4) {
        if near_sky(a[i], a[i + 1], a[i + 2]) && near_sky(b[i], b[i + 1], b[i + 2]) {
            continue;
        }
        for c in 0..3 {
            mean_a[c] += u64::from(a[i + c]);
            mean_b[c] += u64::from(b[i + c]);
        }
        n += 1;
    }
    if n == 0 {
        return (0.0, 0);
    }
    let mut shift = 0.0;
    for c in 0..3 {
        let da = mean_a[c] as f64 / n as f64;
        let db = mean_b[c] as f64 / n as f64;
        shift += (da - db).powi(2);
    }
    (shift.sqrt(), n)
}

fn write_png(path: &Path, rgba: &[u8]) {
    let file = std::fs::File::create(path).expect("png create");
    let mut enc = png::Encoder::new(file, W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(rgba)
        .expect("png data");
}

fn equip_torso(model: u16) -> caer_protocol::equipment::EquipmentUpdate {
    let mut w = PacketWriter::new();
    w.u16(1).u8(0).u8(0).u8(0).u8(0).u8(1);
    w.u8(slot::TORSO).u16(model).u8(1);
    decode(w.as_slice()).expect("equip decode")
}

fn main() {
    caer_client::evidence::emit_from_env();
    let Some(_root) = caer_assets::client_dep::require_caer_client("equip_appearance_rt") else {
        eprintln!("FAIL CAER_CLIENT missing");
        std::process::exit(2);
    };
    let Some(mut em) = EntityModels::load() else {
        eprintln!("FAIL EntityModels::load");
        std::process::exit(3);
    };
    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 5_000.0)).expect("gpu init");
    apply_pin(&mut gpu);

    // Control: same unequipped avatar twice → ~0.
    let Some(ga) = em.ensure_avatar(
        &mut gpu,
        RACE,
        GENDER_FEMALE,
        caer_protocol::customization::Customization::default(),
        None,
    ) else {
        eprintln!("FAIL ensure_avatar unequipped");
        std::process::exit(4);
    };
    let a = render(ga, &em, &mut gpu);
    let b = render(ga, &em, &mut gpu);
    let control_changed = changed_pixels(&a, &b, 0);
    let out = artifacts_dir();
    let _ = std::fs::create_dir_all(&out);
    write_png(&out.join("equip_control_a.png"), &a);
    write_png(&out.join("equip_control_b.png"), &b);
    if control_changed != 0 {
        eprintln!("FAIL REQ-020 control changed_pixels={control_changed} (must be 0)");
        std::process::exit(5);
    }
    eprintln!("PASS equip_appearance_rt CONTROL: changed_pixels=0 under pin");

    // Appearance: studded vs plate must move character pixels.
    let eq_s = equip_torso(STUDDED);
    let eq_p = equip_torso(PLATE);
    let Some(gs) = em.ensure_avatar(
        &mut gpu,
        RACE,
        GENDER_FEMALE,
        caer_protocol::customization::Customization::default(),
        Some(&eq_s),
    ) else {
        eprintln!("FAIL ensure_avatar studded");
        std::process::exit(6);
    };
    let Some(gp) = em.ensure_avatar(
        &mut gpu,
        RACE,
        GENDER_FEMALE,
        caer_protocol::customization::Customization::default(),
        Some(&eq_p),
    ) else {
        eprintln!("FAIL ensure_avatar plate");
        std::process::exit(7);
    };
    if gs == gp {
        eprintln!("FAIL studded/plate returned same gpu id");
        std::process::exit(8);
    }
    let sa = render(gs, &em, &mut gpu);
    let sb = render(gp, &em, &mut gpu);
    let mut changed = 0usize;
    for i in (0..sa.len()).step_by(4) {
        let da = sa[i].abs_diff(sb[i]) as u64
            + sa[i + 1].abs_diff(sb[i + 1]) as u64
            + sa[i + 2].abs_diff(sb[i + 2]) as u64;
        if da > 24 {
            changed += 1;
        }
    }
    let (mean_shift, samples) = char_mean_shift(&sa, &sb);
    let frac = changed as f64 / (W * H) as f64;
    write_png(&out.join("equip_studded.png"), &sa);
    write_png(&out.join("equip_plate.png"), &sb);
    if changed == 0 || (frac < 0.005 && mean_shift < 8.0) {
        eprintln!(
            "FAIL appearance: changed={changed} frac={frac:.4} mean_shift={mean_shift:.2} samples={samples}"
        );
        std::process::exit(9);
    }
    eprintln!(
        "PASS equip_appearance_rt APPEARANCE: studded≠plate changed={changed} \
         mean_colour_shift={mean_shift:.2} char_samples={samples} control=0"
    );
    eprintln!("PASS equip_appearance_rt");
}
