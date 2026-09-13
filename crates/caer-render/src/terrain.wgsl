// Terrain: lit triangles with a per-vertex height colour. Lighting comes from the Globals
// uniform (client lights.csv + sky lights_and_fog) — there is NO hardcoded sun vector here.

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
// Per-zone pre-baked ground texture (1x1 white when the zone ships none — the vertex colour
// then carries the height-ramp fallback).
@group(1) @binding(0) var ground_tex: texture_2d<f32>;
@group(1) @binding(1) var ground_smp: sampler;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) uv: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = globals.view_proj * vec4<f32>(in.pos, 1.0);
    out.color = in.color;
    out.normal = in.normal;
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let ld = globals.light_dir.xyz;
    let has_dir = dot(ld, ld) > 1e-8;
    let light = select(vec3<f32>(0.0, 0.0, 1.0), normalize(ld), has_dir);
    // abs() so a back-facing (double-sided) terrain triangle still lights sensibly.
    let d = select(0.0, abs(dot(normalize(in.normal), light)), has_dir);
    let ambient = globals.light_ambient.rgb * globals.light_ambient.a;
    let dynamic = globals.light_dynamic.rgb * globals.light_dynamic.a * d;
    let lit = ambient + dynamic;
    let ground = textureSample(ground_tex, ground_smp, in.uv).rgb;
    return vec4<f32>(in.color * ground * lit, 1.0);
}
