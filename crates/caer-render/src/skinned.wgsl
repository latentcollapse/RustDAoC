// Skinned creature meshes: the same instanced draw as `mesh.wgsl`, but each vertex is deformed by
// a per-INSTANCE bone palette before the instance transform is applied.
//
// This is what makes per-entity animation possible. The CPU path it replaces skinned a mesh once at
// load and baked the result, so every instance of a model was frozen at one shared clip time; here
// the mesh is uploaded once in skin space and each instance supplies its own palette, so two
// skeletons standing side by side can be at different points in their idle.
//
// Palette layout: one contiguous run of `parts * bones` matrices per instance. A vertex's joint
// index already includes its part's offset (baked in by `terrain::skinned_batch`), so the lookup is
// just `palette[inst_palette_base + joint]` — no per-part draw state, so the whole mesh draws in a
// single call.
//
// The blend below is verified against the CPU reference implementation in
// `gpu_skinned_batch_reproduces_the_cpu_posed_batch`, so if this shader ever disagrees visually,
// the bug is in binding/upload, not in the maths.

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
// Column-major mat4x4 per bone, from `RiggedModel::bone_matrices`.
@group(2) @binding(0) var<storage, read> palette: array<mat4x4<f32>>;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
    @location(3) uv: vec2<f32>,
    // instance: xyz = render-space position, w = yaw (radians)
    @location(4) inst_pos_yaw: vec4<f32>,
    @location(5) inst_scale: f32,
    // First palette matrix belonging to this instance. Carried as f32 to keep the instance buffer
    // a single float stream; values are small integers so the conversion is exact.
    @location(6) inst_palette_base: f32,
    // Skinning influences (second vertex buffer).
    @location(7) joints: vec4<u32>,
    @location(8) weights: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let base = u32(in.inst_palette_base);

    // Linear blend skinning: sum(w_i * M_i * v). Weights sum to ~1; unused slots carry weight 0,
    // and are skipped rather than trusted to be harmless (an unused slot's joint index is not
    // guaranteed meaningful).
    var skinned = vec3<f32>(0.0, 0.0, 0.0);
    var skinned_n = vec3<f32>(0.0, 0.0, 0.0);
    var total = 0.0;
    for (var k = 0u; k < 4u; k = k + 1u) {
        let w = in.weights[k];
        if (w <= 0.0) {
            continue;
        }
        let m = palette[base + in.joints[k]];
        skinned = skinned + w * (m * vec4<f32>(in.pos, 1.0)).xyz;
        // Normals take rotation/scale only — no translation, hence w = 0.
        skinned_n = skinned_n + w * (m * vec4<f32>(in.normal, 0.0)).xyz;
        total = total + w;
    }
    // A vertex with no influences would collapse to the origin; fall back to its bind position so a
    // gap in the weights shows as an unanimated vertex rather than a spike through the world.
    if (total <= 0.0) {
        skinned = in.pos;
        skinned_n = in.normal;
    }

    // Then the ordinary instance transform: uniform scale, yaw about Z, translate.
    let c = cos(in.inst_pos_yaw.w);
    let s = sin(in.inst_pos_yaw.w);
    let p = skinned * in.inst_scale;
    let rotated = vec3<f32>(c * p.x - s * p.y, s * p.x + c * p.y, p.z);
    let n = normalize(skinned_n);
    let rn = vec3<f32>(c * n.x - s * n.y, s * n.x + c * n.y, n.z);

    var out: VsOut;
    out.clip = globals.view_proj * vec4<f32>(rotated + in.inst_pos_yaw.xyz, 1.0);
    out.color = in.color;
    out.normal = rn;
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Identical to mesh.wgsl's fragment stage on purpose: creatures must light exactly like the
    // fixtures around them, and matching it byte-for-byte means the skinning change cannot shift
    // shading. Any lighting change belongs in BOTH, or in a shared include.
    let texel = textureSample(base_tex, base_samp, in.uv);
    if texel.a < 0.4 {
        discard;
    }
    let ld = globals.light_dir.xyz;
    let has_dir = dot(ld, ld) > 1e-8;
    let light = select(vec3<f32>(0.0, 0.0, 1.0), normalize(ld), has_dir);
    let d = select(0.0, abs(dot(normalize(in.normal), light)), has_dir);
    let ambient = globals.light_ambient.rgb * globals.light_ambient.a;
    let dynamic = globals.light_dynamic.rgb * globals.light_dynamic.a * d;
    let lit = ambient + dynamic;
    return vec4<f32>(in.color * texel.rgb * lit, 1.0);
}
