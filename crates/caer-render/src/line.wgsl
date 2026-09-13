// Flat coloured lines for the ground reference grid. No lighting, no instancing — just the
// camera transform applied to per-vertex world (render-space) positions.
// Globals layout must match `gpu.rs` (shared uniform buffer).

struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
    light_dir: vec4<f32>,
    light_ambient: vec4<f32>,
    light_dynamic: vec4<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = globals.view_proj * vec4<f32>(in.pos, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
