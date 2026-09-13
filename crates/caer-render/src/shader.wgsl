// Instanced cube shader. One unit-cube mesh (locations 0-1) is drawn once per visible entity,
// with the per-instance translation/colour/scale in locations 2-4. Lighting from client tables
// via Globals (no hardcoded sun vector).

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
    @location(2) i_pos: vec3<f32>,
    @location(3) i_color: vec3<f32>,
    @location(4) i_scale: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world = in.pos * in.i_scale + in.i_pos;
    out.clip = globals.view_proj * vec4<f32>(world, 1.0);
    out.color = in.i_color;
    out.normal = in.normal;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let ld = globals.light_dir.xyz;
    let has_dir = dot(ld, ld) > 1e-8;
    let light = select(vec3<f32>(0.0, 0.0, 1.0), normalize(ld), has_dir);
    let d = select(0.0, max(dot(normalize(in.normal), light), 0.0), has_dir);
    let ambient = globals.light_ambient.rgb * globals.light_ambient.a;
    let dynamic = globals.light_dynamic.rgb * globals.light_dynamic.a * d;
    let lit = ambient + dynamic;
    return vec4<f32>(in.color * lit, 1.0);
}
