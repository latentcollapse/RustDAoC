//! Palette fold compute: `palette[slot,part,bone] = bones[slot,bone] * inverse_bind[part,bone]`.
//!
//! The CPU streams posed world bones only; inverse-bind is static per model. This replaces the
//! parts×bones PCIe upload (MS-08). Vertex shader is unchanged — it still reads the expanded palette.
//!
//! **Uniform-scale dependency:** CPU `xform_mul` keeps scale as a separate scalar; these mat4s have
//! scale already folded into the 3×3 columns. `bones * inverse_bind` matches `xform_mul(world, ib)`
//! only while scale is a **uniform scalar** (scalars commute through the rot product and the
//! translation term). A non-uniform scale in `Xform` would break this fold silently.

struct Params {
    bone_stride: u32,
    part_count: u32,
    unique_count: u32,
    z_offset: f32,
    /// Debug/falsifier: rotate which inverse-bind part is sampled (`0` in production).
    /// A `palette_stride ≠ parts×bones` mismatch is the same class of error — wrong part's IB.
    part_offset: u32,
    /// Debug/falsifier: leave the last part's palette slots unwritten (tail-stale signature).
    omit_last_part: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> bones: array<mat4x4<f32>>;
@group(0) @binding(1) var<storage, read> inverse_bind: array<mat4x4<f32>>;
@group(0) @binding(2) var<storage, read_write> palette: array<mat4x4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    let bones_n = params.bone_stride;
    let parts_n = params.part_count;
    let stride = parts_n * bones_n;
    let total = params.unique_count * stride;
    if i >= total || bones_n == 0u || parts_n == 0u {
        return;
    }
    let slot = i / stride;
    let within = i % stride;
    let part = (within / bones_n + params.part_offset) % parts_n;
    let bone = within % bones_n;
    // Tail-stale falsifier: skip writing the last part so prior-frame matrices remain.
    if params.omit_last_part != 0u && part + 1u >= parts_n {
        return;
    }
    // Column-major mat4 mul: applies inverse_bind then bones (matches CPU xform_mul(world, ib)).
    var m = bones[slot * bones_n + bone] * inverse_bind[part * bones_n + bone];
    // Foot-anchor: CPU apply_z_from adds z_offset to every palette translation.
    m[3][2] = m[3][2] + params.z_offset;
    palette[i] = m;
}
