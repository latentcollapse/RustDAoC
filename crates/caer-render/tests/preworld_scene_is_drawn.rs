//! Named falsifier: the realm scene reaches the framebuffer **through the pre-world encoder**.
//!
//! The bug this exists for: `preworld_scene::load` worked, the camera framing was right, the
//! upload happened, `preworld_scene_load` passed, and the live client's character screens were
//! still black. The pre-world pass — the only encoder the window uses while `at_preworld` — drew
//! UI quads and nothing else. It had no model step at all. Every screenshot went through the
//! *world* encoder, so no capture and no test could ever show the defect.
//!
//! **Observation that fails if broken:** delete the model draw from `encode_preworld_open`, or
//! route the window back to the UI-only pass on the character screens, and
//! `FramePass::PreWorldScene` renders the same all-black frame `FramePass::PreWorldUi` does.
//!
//! **Fails** when the retail tree is absent (REQ-025). Set `CAER_CLIENT`.

use std::path::PathBuf;

use caer_render::camera::Camera;
use caer_render::gpu::{FramePass, Gpu};
use glam::Vec3;

const W: u32 = 640;
const H: u32 = 480;

fn root() -> PathBuf {
    let r = caer_assets::client_dep::required_caer_client_root("preworld_scene_is_drawn");
    assert!(
        r.join("pregame").is_dir(),
        "CAER_CLIENT has no pregame directory: {} — REQ-025: a test that cannot run must not \
         report pass.",
        r.display()
    );
    r
}

/// Fraction of pixels that are not the cleared black the pre-world pass starts from.
fn lit_fraction(rgba: &[u8]) -> f64 {
    let mut lit = 0u64;
    let total = (rgba.len() / 4) as u64;
    for px in rgba.chunks_exact(4) {
        if px[0] > 8 || px[1] > 8 || px[2] > 8 {
            lit += 1;
        }
    }
    lit as f64 / total.max(1) as f64
}

#[test]
fn preworld_scene_pass_paints_the_realm_scene() {
    let root = root();
    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 60_000.0)).expect("gpu init");
    // Neutral daylight from the client's own table. Without a published atmosphere every surface
    // multiplies to black, which is a second, independent way to reproduce the reported symptom.
    caer_render::atmosphere::publish(caer_render::atmosphere::Atmosphere::load_for_region(
        &root,
        "sky_default",
    ));

    for (realm, name) in [(1u8, "Albion"), (2, "Midgard"), (3, "Hibernia")] {
        let scene = caer_render::preworld_scene::load(&root, realm)
            .unwrap_or_else(|| panic!("{name} scene must load"));
        // A texture the search path cannot answer binds the white fallback, which draws as a
        // blank sheet in the middle of the screen and lights every pixel-count assertion below
        // just as well as the real art would. All four misses this caught were our own gaps —
        // shared world art outside `pregame/`, `.tga` files reachable only under a `.dds` key,
        // and a 24-bpp uncompressed DDS we rejected — not absent client data.
        assert!(
            scene.missing_textures.is_empty(),
            "{name}: {} texture(s) fell through to the white fallback: {:?}",
            scene.missing_textures.len(),
            scene.missing_textures
        );
        gpu.set_models(&scene.models, &scene.textures);

        let (eye, focus) = caer_render::preworld_scene::framing(&scene, 16.0 / 9.0);
        let unmirror = |v: Vec3| Vec3::new(v.x, -v.y, v.z);
        let mut camera = Camera::new(
            unmirror(eye),
            unmirror(focus),
            Vec3::ZERO,
            W as f32 / H as f32,
        );
        camera.ensure_far(60_000.0);
        gpu.set_view_proj(camera.view_proj().to_cols_array_2d());

        let scene_frame = gpu
            .render_to_rgba_pass(0, FramePass::PreWorldScene)
            .expect("scene pass");
        let ui_frame = gpu
            .render_to_rgba_pass(0, FramePass::PreWorldUi)
            .expect("ui pass");

        let lit = lit_fraction(&scene_frame);
        assert!(
            lit > 0.20,
            "{name}: the pre-world scene pass painted {:.1}% of the frame — the character screen \
             is still black behind its plate",
            lit * 100.0
        );
        // The UI-only pass has the same models uploaded and the same camera. If it also lights up,
        // this test is measuring something other than the model step and proves nothing.
        let ui_lit = lit_fraction(&ui_frame);
        assert!(
            ui_lit < 0.01,
            "{name}: the UI-only pass painted {:.1}% with no UI quads uploaded — the two passes \
             are not being told apart, so a green scene pass is not evidence",
            ui_lit * 100.0
        );
        println!(
            "{name}: scene pass {:.1}% lit, ui pass {ui_lit:.4}",
            lit * 100.0
        );

        let cx = (W / 2) as usize;
        let cy = (H / 2) as usize;
        let i = (cy * W as usize + cx) * 4;
        let center_lit = scene_frame[i] > 8 || scene_frame[i + 1] > 8 || scene_frame[i + 2] > 8;
        assert!(
            center_lit,
            "{name}: scene pass left the center pixel black — models are not under the transparent plate"
        );
        let ui_center = ui_frame[i] > 8 || ui_frame[i + 1] > 8 || ui_frame[i + 2] > 8;
        assert!(
            !ui_center,
            "{name}: UI-only pass lit the center with no UI quads — passes are not distinct"
        );
    }
}
