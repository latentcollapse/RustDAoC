// Fixture models: NIF geometry with per-part base texture × vertex diffuse, instanced with
// position + a full rotation QUATERNION + uniform scale. Lighting from Globals (client tables).
// Untextured ranges bind a 1×1 white texture so the diffuse shows through unchanged.
//
// The instance rotation was a scalar yaw until it turned out that 2,559 region-1 fixtures author a
// rotation axis that is not ±Z — leaning stones, wrecked boats, tents, burnt trees. Those were
// parsed correctly and then drawn bolt upright, because a yaw has nowhere to put a pitch or roll.
// A quaternion carries the authored rotation exactly and costs one extra float per instance.

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
@group(1) @binding(0) var base_tex: texture_2d<f32>;
@group(1) @binding(1) var base_samp: sampler;
// Second ground layer. The stage grounds are two sheets blended by a per-vertex mask painted in
// the NIF's vertex colours — grass over a stone slab in Albion, snow over rock in Midgard. Parts
// with one layer bind this to the same texture and carry a mask of 1.0.
@group(1) @binding(2) var blend_tex: texture_2d<f32>;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) uv: vec2<f32>,
    @location(7) blend: f32,
    @location(9) overlay_uv: vec2<f32>,
    // instance: render-space position, rotation quaternion (x,y,z,w), uniform scale
    @location(4) inst_pos: vec3<f32>,
    @location(5) inst_rot: vec4<f32>,
    @location(6) inst_scale: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) blend: f32,
    @location(4) overlay_uv: vec2<f32>,
};

// Rotate a vector by a unit quaternion: v + 2q_xyz × (q_xyz × v + q_w·v).
fn qrot(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    return v + 2.0 * cross(q.xyz, cross(q.xyz, v) + q.w * v);
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let p = in.pos * in.inst_scale;
    let rotated = qrot(in.inst_rot, p);
    // The rotation is rigid (unit quaternion, uniform scale), so normals take the same transform.
    let rn = qrot(in.inst_rot, in.normal);
    var out: VsOut;
    out.clip = globals.view_proj * vec4<f32>(rotated + in.inst_pos, 1.0);
    out.color = in.color;
    out.normal = rn;
    out.uv = in.uv;
    out.blend = in.blend;
    out.overlay_uv = in.overlay_uv;
    return out;
}

// The part's diffuse texel: layer 1 where the mask is 1, layer 2 where it is 0.
//
// `mix` runs unconditionally rather than behind a branch on blend == 1.0. A single-layer part
// binds the same view to both slots, so the mix is the identity there, and a uniform branch buys
// nothing while a divergent one costs. The sole exception is the negative sentinel: a fixed-map
// Decal 0 carries its OWN alpha mask (Hibernia's forest panorama over its cloud dome), so it must
// compose `blend_tex` over the base rather than use the unrelated vertex-colour mask.
fn layered(in: VsOut) -> vec4<f32> {
    let a = textureSample(base_tex, base_samp, in.uv);
    // A fixed-map Decal 0 is a separate material stage. Its UVs do not inherit the base map's
    // texture-transform controller: Hibernia scrolls its clouds but keeps the forest panorama
    // stationary. Vertex-mask blends retain the shared base UV as before.
    let b_uv = select(in.uv, in.overlay_uv, in.blend < 0.0);
    let b = textureSample(blend_tex, base_samp, b_uv);
    if (in.blend < 0.0) {
        return vec4<f32>(mix(a.rgb, b.rgb, b.a), a.a);
    }
    return mix(b, a, clamp(in.blend, 0.0, 1.0));
}

// Shared shading: client-table lighting applied to vertex diffuse × base texel.
fn shade(in: VsOut, texel: vec4<f32>) -> vec3<f32> {
    let ld = globals.light_dir.xyz;
    let has_dir = dot(ld, ld) > 1e-8;
    let light = select(vec3<f32>(0.0, 0.0, 1.0), normalize(ld), has_dir);
    let d = select(0.0, abs(dot(normalize(in.normal), light)), has_dir);
    let ambient = globals.light_ambient.rgb * globals.light_ambient.a;
    let dynamic = globals.light_dynamic.rgb * globals.light_dynamic.a * d;
    return in.color * texel.rgb * (ambient + dynamic);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = layered(in);
    // Alpha-test cutout: kills DXT1 punch-through texels and DXT3/5 foliage edges without
    // needing blending or depth sorting. The white fallback has a=1, so untextured parts pass.
    if texel.a < 0.4 {
        discard;
    }
    return vec4<f32>(shade(in, texel), 1.0);
}

// Parts the NIF flags with a `NiAlphaProperty`: glows, coronas, sun discs, water sheets, decals
// laid over ground. Drawn after the opaque pass with source-over blending and no depth write.
//
// No alpha test here on purpose. These textures are frequently *fully opaque* in their alpha
// channel and get their transparency from the blend mode alone — which is why the alpha-test-only
// path drew Hibernia's sun corona as a solid orange disc and Albion's ground glow as a black
// quad. Testing them changes nothing; blending them is the whole point.
@fragment
fn fs_blend(in: VsOut) -> @location(0) vec4<f32> {
    let texel = layered(in);
    return vec4<f32>(shade(in, texel), texel.a);
}
