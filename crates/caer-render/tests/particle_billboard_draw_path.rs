//! Named falsifier: `particle_billboard_presence_pixels`.
//!
//! **Observation that fails if broken:** deleting the billboard upload/draw path, or ignoring
//! particle mesh uploads (`upload_particle_billboards` / encode draw), yields zero non-sky
//! pixels and/or `particle_billboard_vert_count() == 0` while a live presence_burst is ticking.
//!
//! Uses only the billboard path (no cube instances). Placeholder texture is labelled
//! [`caer_render::particles::PARTICLE_BILLBOARD_PLACEHOLDER`] — not DAoC art.

use caer_render::camera::Camera;
use caer_render::gpu::Gpu;
use caer_render::particles::{
    presence_burst, ParticleSystem, PARTICLE_BILLBOARD_DRAW_PATH, PARTICLE_BILLBOARD_PLACEHOLDER,
};
use glam::Vec3;

const W: u32 = 640;
const H: u32 = 480;
const SEED: u64 = 0xCAE0_B1BB_0ADDu64;
const DT: f32 = 1.0 / 30.0;

fn pin_camera(gpu: &mut Gpu) -> Camera {
    let camera = Camera::new(
        Vec3::new(63.0, -180.0, 63.0),
        Vec3::new(0.0, 0.0, 55.0),
        Vec3::ZERO,
        W as f32 / H as f32,
    );
    gpu.set_sky(caer_assets::sky::Sky::default().day);
    gpu.set_view_proj(camera.view_proj().to_cols_array_2d());
    camera
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

#[test]
fn particle_billboard_presence_pixels() {
    assert_eq!(PARTICLE_BILLBOARD_DRAW_PATH, "particle_billboard_draw_path");
    assert_eq!(
        PARTICLE_BILLBOARD_PLACEHOLDER,
        "PLACEHOLDER_SOFT_BLOB_NOT_DAOC_ART"
    );

    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 5_000.0)).expect("gpu init");
    let camera = pin_camera(&mut gpu);
    let (right_v, up_v) = camera.billboard_axes();
    let right = [right_v.x, right_v.y, right_v.z];
    let up = [up_v.x, up_v.y, up_v.z];

    let mut sys = ParticleSystem::from_def(presence_burst([0.0, 0.0, 55.0], 407), SEED);
    let mut max_non_sky = 0u64;
    let mut max_verts = 0u32;
    for _ in 0..12 {
        sys.tick(DT);
        let mesh = sys.billboard_mesh(right, up);
        gpu.upload_particle_billboards(&mesh);
        max_verts = max_verts.max(gpu.particle_billboard_vert_count());
        // Cube path deliberately unused — count stays 0 so pixels must come from billboards.
        gpu.upload_instances(&[]);
        let rgba = gpu.render_to_rgba(0).expect("GPU wait");
        max_non_sky = max_non_sky.max(non_sky_samples(&rgba));
    }

    assert!(
        max_verts > 0,
        "falsifier particle_billboard_presence_pixels: upload path ignored \
         (particle_billboard_vert_count stayed 0 while emitter was live)"
    );
    assert!(
        max_non_sky > 50,
        "falsifier particle_billboard_presence_pixels: billboard draw path deleted or ignored \
         (non-sky pixels={max_non_sky}; expected soft-blob sprites over sky)"
    );

    // Negative control: clearing the upload must zero the draw counter (encode skips draw).
    gpu.upload_particle_billboards(&[]);
    assert_eq!(
        gpu.particle_billboard_vert_count(),
        0,
        "falsifier: clearing billboards must zero particle_billboard_vert_count"
    );
}
