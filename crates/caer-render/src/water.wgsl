// Water: same vertex layout/globals as terrain, but a translucent flat surface. Drawn last
// (after terrain and entities) with alpha blending on and depth WRITE off, so the riverbed
// stays visible through it and nothing z-fights. Also draws the global ocean plane.

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
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
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
    // Soft table lighting so ocean/lakes respond when atmosphere tables are present.
    let ambient = globals.light_ambient.rgb * globals.light_ambient.a;
    let lit = max(ambient + globals.light_dynamic.rgb * globals.light_dynamic.a * 0.35, vec3<f32>(0.15));
    return vec4<f32>(in.color * lit, 0.62);
}
