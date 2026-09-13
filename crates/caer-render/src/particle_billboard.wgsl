// Particle billboard sprites: camera-facing quads expanded on the CPU, textured with the
// honest PLACEHOLDER soft blob (not DAoC art). Additive blend + soft alpha — closer to
// effect look than opaque Instance cubes; fidelity still not claimed.

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
@group(1) @binding(0) var sprite_tex: texture_2d<f32>;
@group(1) @binding(1) var sprite_samp: sampler;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = globals.view_proj * vec4<f32>(in.pos, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let tex = textureSample(sprite_tex, sprite_samp, in.uv);
    // Premultiply soft blob alpha into RGB for additive blend.
    let a = tex.a * in.color.a;
    return vec4<f32>(in.color.rgb * tex.rgb * a, a);
}
