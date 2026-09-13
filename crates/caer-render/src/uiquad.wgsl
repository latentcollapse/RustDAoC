// 2D textured-quad shader for the skin UI.
//
// One instance == one quad. The four corners are generated in the vertex shader from the vertex
// index, so there is no vertex buffer at all: a window is a handful of instances and nothing else
// has to be uploaded per frame.
//
// Coordinates are PIXELS on the way in (top-left origin, matching both the skin XML and the TGA
// decoder's output) and are converted to NDC here. Keeping pixels as the interface means the layout
// code never has to know the screen size or the clip-space convention.

struct Globals {
    // Framebuffer size in pixels.
    screen: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> g: Globals;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_samp: sampler;

struct Inst {
    // Destination rect in screen pixels: xy = top-left, zw = size.
    @location(0) dst: vec4<f32>,
    // Source rect in atlas pixels: xy = top-left, zw = size.
    @location(1) src: vec4<f32>,
    // Tint, multiplied into the sampled texel.
    @location(2) color: vec4<f32>,
    // Atlas dimensions in pixels, to normalise the source rect to UV.
    @location(3) atlas: vec2<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Inst) -> VsOut {
    // Two triangles as a unit square. Same winding for both, and the pipeline disables culling
    // anyway, so a quad can never be dropped for facing the "wrong" way.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[vi];
    let px = inst.dst.xy + c * inst.dst.zw;

    var o: VsOut;
    // Pixels (y down from the top) to NDC (y up from the centre).
    o.pos = vec4<f32>(px.x / g.screen.x * 2.0 - 1.0, 1.0 - px.y / g.screen.y * 2.0, 0.0, 1.0);
    // Atlas pixels to UV. The layout stage works entirely in pixels, so this is the single place
    // texel coordinates get normalised.
    o.uv = (inst.src.xy + c * inst.src.zw) / inst.atlas;
    o.color = inst.color;
    return o;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(atlas_tex, atlas_samp, in.uv) * in.color;
}
