// Sky dome — a full-screen gradient between the client's own zenith and horizon colours.
//
// Drawn FIRST, with no depth test and no depth write, so the whole framebuffer starts as sky and
// everything else paints over it. That is cheaper than a dome mesh and cannot crack at the seams.
//
// The gradient is by the elevation of the VIEW RAY, not by screen Y: reconstructing the ray from
// the inverse view-projection means the horizon stays put as the camera pitches and rolls, which a
// screen-space gradient gets wrong the moment you look up.

struct Globals {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    // .rgb colour, .a unused (std140 keeps these 16-byte aligned).
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
    light_dir: vec4<f32>,
    light_ambient: vec4<f32>,
    light_dynamic: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // Clip-space xy carried through so the fragment stage can rebuild the ray.
    @location(0) ndc: vec2<f32>,
};

// One oversized triangle covering the screen — no vertex buffer needed.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    var pts = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    let p = pts[vi];
    var out: VsOut;
    // z = 1.0 is the far plane in wgpu's 0..1 depth range: the sky sits behind everything.
    out.clip = vec4<f32>(p, 1.0, 1.0);
    out.ndc = p;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Unproject two depths and difference them to get the view ray for this pixel. Doing it from
    // the matrix keeps the sky correct for any camera orientation.
    let near = globals.inv_view_proj * vec4<f32>(in.ndc, 0.0, 1.0);
    let far  = globals.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = normalize((far.xyz / far.w) - (near.xyz / near.w));

    // Render space is Z-up. Elevation 0 at the horizon, 1 straight up.
    let elevation = clamp(dir.z, 0.0, 1.0);
    // Bias the blend toward the horizon so the band near eye level is wide, as in the original —
    // a linear ramp puts far too much zenith colour low in the frame.
    let t = pow(elevation, 0.45);

    let colour = mix(globals.sky_horizon.rgb, globals.sky_zenith.rgb, t);
    return vec4<f32>(colour, 1.0);
}
