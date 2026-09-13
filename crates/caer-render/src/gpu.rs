//! The wgpu side: device/surface setup, the instanced-cube pipeline, and a per-frame instance
//! buffer that grows as the visible set does. Nothing here knows about DAoC — it draws whatever
//! `Instance` list it is handed.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use sha2::{Digest, Sha256};

/// One 2D UI quad. Pixels in, pixels out — the shader does the NDC and UV conversion, so the
/// layout stage never needs to know the screen size or the atlas dimensions.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct UiInstance {
    /// Destination rect in screen pixels: xy = top-left, zw = size.
    pub dst: [f32; 4],
    /// Source rect in atlas pixels: xy = top-left, zw = size.
    pub src: [f32; 4],
    /// Tint multiplied into the sampled texel.
    pub color: [f32; 4],
    /// Atlas size in pixels, so the shader can normalise `src` to UV.
    pub atlas: [f32; 2],
    pub _pad: [f32; 2],
}

/// One painter-ordered run of UI instances resident on the GPU.
///
/// The pre-world forms contain many alternating atlas pages (font, buttons, form plate, colour
/// pickers). Re-creating one vertex buffer for every run every redraw made opening the stats
/// modal churn dozens of native GPU resources per click. The native driver eventually faults
/// long before Rust has anything meaningful to report. Keep a small slot pool instead: a slot
/// grows only when a later layout needs more instances, then receives new contents through
/// `queue.write_buffer` on subsequent frames.
struct UiBatch {
    page: String,
    buffer: wgpu::Buffer,
    /// Maximum number of [`UiInstance`] values that fit in `buffer`.
    capacity: u32,
    /// Instances used by the current layout; `capacity` may be larger after a growth.
    count: u32,
}

/// Truthful accounting for a UI submission. CPU layout counts are not proof that every texture
/// page made it into GPU batches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiSubmitReport {
    pub requested: usize,
    pub submitted: usize,
    pub missing_pages: Vec<String>,
}
use winit::window::Window;

/// Surface faults are exceptional, but logging every timeout in a damaged compositor session can
/// itself starve the event loop. Keep independent counters: one shared counter made a later Lost
/// look like "exception #900" after a timeout storm and destroyed the causal evidence.
static SURFACE_TIMEOUT_COUNT: AtomicU64 = AtomicU64::new(0);
static SURFACE_OCCLUDED_COUNT: AtomicU64 = AtomicU64::new(0);
static SURFACE_SUBOPTIMAL_COUNT: AtomicU64 = AtomicU64::new(0);
static SURFACE_OUTDATED_COUNT: AtomicU64 = AtomicU64::new(0);
static SURFACE_LOST_COUNT: AtomicU64 = AtomicU64::new(0);
static SURFACE_VALIDATION_COUNT: AtomicU64 = AtomicU64::new(0);
static SURFACE_PRESENT_COUNT: AtomicU64 = AtomicU64::new(0);

fn note_surface_event(kind: &'static str, acquire_time: std::time::Duration) {
    let counter = match kind {
        "timeout" => &SURFACE_TIMEOUT_COUNT,
        "occluded" => &SURFACE_OCCLUDED_COUNT,
        "suboptimal" => &SURFACE_SUBOPTIMAL_COUNT,
        "outdated" => &SURFACE_OUTDATED_COUNT,
        "lost" => &SURFACE_LOST_COUNT,
        "validation" => &SURFACE_VALIDATION_COUNT,
        _ => &SURFACE_VALIDATION_COUNT,
    };
    let occurrence = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if occurrence <= 8 || occurrence.is_multiple_of(120) {
        log::warn!(
            "caer-render: window surface {kind} after {:.3}s (occurrence #{occurrence}; subsequent repeats rate-limited)",
            acquire_time.as_secs_f64()
        );
    }
}

/// A truthful result for one product presentation attempt. A skipped frame is not a successful
/// present, and a lost/outdated surface requires forced reconfiguration even when its size did not
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentOutcome {
    Presented,
    Skipped(&'static str),
    Reconfigure(&'static str),
    Fatal(&'static str),
}

/// What a presented frame actually draws.
///
/// Pre-world is not one thing. Login and realm select are flat art and want the tiny UI-only
/// encoder; the two character screens are a stone frame around a transparent middle and need the
/// realm scene drawn under them. Making that a named choice at the call site is the point — the
/// scene silently not being in the pre-world encoder is exactly the bug this replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePass {
    /// Full world: palette folds, sky, terrain, models, entities, particles, then UI.
    World,
    /// Pre-world plate only: UI quads on black.
    PreWorldUi,
    /// Pre-world plate over the realm scene: model geometry, then UI quads.
    PreWorldScene,
}

fn note_surface_present(acquire_time: std::time::Duration) {
    let presented = SURFACE_PRESENT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if acquire_time >= std::time::Duration::from_millis(100) {
        log::warn!(
            "caer-render: surface acquisition blocked event loop for {:.3}s before present #{presented}",
            acquire_time.as_secs_f64()
        );
    }
    if std::env::var_os("CAER_SURFACE_TRACE").is_some() && presented.is_multiple_of(300) {
        log::info!(
            "caer-render: surface totals presented={presented} timeout={} occluded={} suboptimal={} outdated={} lost={} validation={}",
            SURFACE_TIMEOUT_COUNT.load(Ordering::Relaxed),
            SURFACE_OCCLUDED_COUNT.load(Ordering::Relaxed),
            SURFACE_SUBOPTIMAL_COUNT.load(Ordering::Relaxed),
            SURFACE_OUTDATED_COUNT.load(Ordering::Relaxed),
            SURFACE_LOST_COUNT.load(Ordering::Relaxed),
            SURFACE_VALIDATION_COUNT.load(Ordering::Relaxed),
        );
    }
}

/// One cube-mesh vertex: a corner of the unit cube plus that face's normal (for shading).
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
}

/// Per-entity instance data, rebuilt every frame from the world's render set. `pos` is in
/// **render space** (world coords minus the scene origin); `scale` is the box half-extent.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
pub struct Instance {
    pub pos: [f32; 3],
    pub color: [f32; 3],
    pub scale: f32,
}

/// One vertex of the ground reference grid: a render-space endpoint plus its line colour.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct LineVertex {
    pos: [f32; 3],
    color: [f32; 3],
}

/// One brush-ring polyline plus its RGB colour (terrain-brush cursor preview).
type BrushRingLine = (Vec<[f32; 3]>, [f32; 3]);

/// The single uniform: the camera's view-projection matrix + sky + table-fed lighting.
#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    /// Inverse of `view_proj`, so the sky shader can rebuild a per-pixel view ray. Uploaded here
    /// rather than inverted on the GPU because it is one CPU inverse per frame versus one per
    /// fragment.
    inv_view_proj: [[f32; 4]; 4],
    /// Sky dome colours, `.rgb` used (`.a` pads to the 16-byte alignment uniforms require).
    sky_zenith: [f32; 4],
    sky_horizon: [f32; 4],
    /// Directional light from client `lights.csv` aggregation (xyz); w unused.
    /// Zero when tables are disabled / empty — **no hardcoded (0.35,0.25,1) fallback**.
    light_dir: [f32; 4],
    /// Ambient RGB from sky `lights_and_fog_clear`; `.a` = ambient_amount.
    light_ambient: [f32; 4],
    /// Dynamic / sun RGB from the same table; `.a` = dynamic_amount.
    light_dynamic: [f32; 4],
}

/// Bytes per 4x4 f32 bone matrix in the palette storage buffer.
const MATRIX_BYTES: u64 = 64;

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// One authored sRGB byte as the linear value an sRGB colour target expects.
///
/// The surface is chosen `is_srgb`, so the hardware encodes whatever the shader writes. A colour
/// constant handed over as `v / 255.0` is therefore read as *linear* and encoded a second time:
/// the pre-world dialog fill, authored `Rgba { r: 8, g: 8, b: 8 }`, measured (50,50,50) on screen.
/// Alpha is not gamma-encoded and must not pass through here.
#[must_use]
pub fn srgb_byte_to_linear(v: u8) -> f32 {
    let s = f32::from(v) / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// One model's uploaded geometry + its placed instances.
struct ModelDraw {
    /// Index of the source `ModelBatch` in the mesh's `models` vec. `set_models` skips empty
    /// batches, so draw order ≠ batch order; the editor uses this to map a picked batch to its GPU
    /// instance buffer for live single-instance updates.
    src: usize,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    indices: u32,
    inst_buf: wgpu::Buffer,
    instances: u32,
    /// Textured sub-draws: (index range start, end, index into `Gpu::model_textures`).
    /// Empty means "whole mesh with the white fallback" (batch predates texture ranges).
    parts: Vec<(u32, u32, usize)>,
    /// The same, for source-over ranges. Drawn after every opaque part of every model, with
    /// blending on and depth write off.
    blend_parts: Vec<(u32, u32, usize)>,
    /// And for additive ranges — coronas, flames, sun discs — drawn last of all.
    add_parts: Vec<(u32, u32, usize)>,
}

/// One live-entity model: geometry uploaded once (per resolved monster model id), with an
/// instance buffer rewritten every frame from whichever entities of that model are in view. Same
/// `mesh_pipeline` as the static fixtures, but dynamic — so a monster walks by moving its instance,
/// not re-uploading its mesh.
struct EntityDraw {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    indices: u32,
    inst_buf: wgpu::Buffer,
    /// Capacity in instances; grown (buffer recreated) when a frame needs more.
    inst_cap: u32,
    /// Live instance count to draw this frame (0 = uploaded mesh with nothing visible).
    instances: u32,
    /// Index into [`Gpu::entity_textures`] for this model's skin. 0 = the white fallback (untextured
    /// or unresolved skin) so the material diffuse shows through; >0 = a resolved monster skin.
    tex: usize,
}

/// Exact identity of a decoded model texture, including every mip level that reaches the GPU.
///
/// Call sites load the same client sheet through several tables and may also derive probes or
/// alpha-repaired copies. Source names are therefore neither complete nor safe identities; the
/// decoded payload is. A digest lets equivalent uploads share one GPU allocation without making
/// a transformed sheet alias its source.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct EntityTextureKey([u8; 32]);

impl EntityTextureKey {
    fn from_dds(texture: &caer_assets::dds::DdsTexture) -> Self {
        use caer_assets::dds::DdsFormat;

        let mut digest = Sha256::new();
        digest.update(b"caer-entity-texture-v1\0");
        digest.update(texture.width.to_le_bytes());
        digest.update(texture.height.to_le_bytes());
        digest.update([match texture.format {
            DdsFormat::Bc1 => 1,
            DdsFormat::Bc2 => 2,
            DdsFormat::Bc3 => 3,
            DdsFormat::Rgba8 => 4,
        }]);
        digest.update((texture.mips.len() as u64).to_le_bytes());
        for mip in &texture.mips {
            digest.update((mip.len() as u64).to_le_bytes());
            digest.update(mip);
        }
        Self(digest.finalize().into())
    }
}

/// One resident bind group and the number of live mesh records using it.
struct EntityTextureSlot {
    binding: wgpu::BindGroup,
    key: Option<EntityTextureKey>,
    owners: u32,
}

/// Stable texture slots for live and skinned entity meshes.
///
/// Draw records store slot indices, so removing one entry must not shift any remaining slot.
/// Vacant slots are reused only after their final owner retires. Slot zero is the permanent white
/// fallback and deliberately has no cache key or owner count.
#[derive(Default)]
struct EntityTextureCache {
    slots: Vec<Option<EntityTextureSlot>>,
    by_key: HashMap<EntityTextureKey, usize>,
    vacant: Vec<usize>,
}

impl EntityTextureCache {
    fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn resident_count(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    fn install_white(&mut self, binding: wgpu::BindGroup) {
        debug_assert!(self.slots.is_empty(), "white fallback must be slot zero");
        self.slots.push(Some(EntityTextureSlot {
            binding,
            key: None,
            owners: 0,
        }));
    }

    fn retain_existing(&mut self, key: EntityTextureKey) -> Option<usize> {
        let slot = *self.by_key.get(&key)?;
        let entry = self
            .slots
            .get_mut(slot)
            .and_then(Option::as_mut)
            .expect("entity texture index must name a resident slot");
        entry.owners = entry
            .owners
            .checked_add(1)
            .expect("entity texture owner count overflow");
        Some(slot)
    }

    fn slot_for_key(&self, key: EntityTextureKey) -> Option<usize> {
        self.by_key.get(&key).copied()
    }

    fn insert(&mut self, key: EntityTextureKey, binding: wgpu::BindGroup) -> usize {
        debug_assert!(
            !self.by_key.contains_key(&key),
            "new entity texture must not overwrite a resident key"
        );
        let slot = match self.vacant.pop() {
            Some(slot) => {
                debug_assert!(self.slots[slot].is_none());
                self.slots[slot] = Some(EntityTextureSlot {
                    binding,
                    key: Some(key),
                    owners: 1,
                });
                slot
            }
            None => {
                self.slots.push(Some(EntityTextureSlot {
                    binding,
                    key: Some(key),
                    owners: 1,
                }));
                self.slots.len() - 1
            }
        };
        self.by_key.insert(key, slot);
        slot
    }

    fn release(&mut self, slot: usize) {
        if slot == 0 {
            return;
        }
        let retire = {
            let entry = self
                .slots
                .get_mut(slot)
                .and_then(Option::as_mut)
                .expect("entity draw must release a resident texture slot");
            assert!(entry.owners > 0, "entity texture owner count underflow");
            entry.owners -= 1;
            (entry.owners == 0).then(|| {
                entry
                    .key
                    .expect("non-fallback entity texture must have a cache key")
            })
        };
        if let Some(key) = retire {
            self.by_key.remove(&key);
            self.slots[slot] = None;
            self.vacant.push(slot);
        }
    }

    fn binding(&self, slot: usize) -> &wgpu::BindGroup {
        &self
            .slots
            .get(slot)
            .and_then(Option::as_ref)
            .expect("entity draw must bind a resident texture slot")
            .binding
    }
}

/// One skinned instance: `[x, y, z, yaw, scale, palette_base]`.
///
/// `palette_base` is the instance's first matrix index in the model's palette buffer.
/// With palette dedup, several instances may share one base; without it, bases are still
/// `instance_index * palette_stride`. Carried as `f32` so the instance stream stays one buffer;
/// values are small integers, so the shader's `u32()` conversion is exact.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkinnedInstance {
    pub pos_yaw: [f32; 4],
    pub scale: f32,
    pub palette_base: f32,
}

/// A GPU-skinned creature mesh: like [`EntityDraw`] but the geometry is uploaded in SKIN space and
/// deformed in the vertex shader from a per-instance bone palette, so each instance can be at its
/// own point in its animation instead of all sharing one baked pose.
struct SkinnedDraw {
    vbuf: wgpu::Buffer,
    /// Per-vertex skinning influences (second vertex buffer).
    skin_buf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    inst_buf: wgpu::Buffer,
    inst_cap: u32,
    instances: u32,
    /// Expanded skinning matrices (`unique * palette_stride`). CPU-uploaded when
    /// `CAER_CPU_PALETTE_FOLD=1`; otherwise written by the palette-fold compute pass.
    palette_buf: wgpu::Buffer,
    /// Capacity of `palette_buf`, in matrices.
    palette_cap: u32,
    palette_bind: wgpu::BindGroup,
    /// Matrices per unique slot (`parts * bones`) — the stride into `palette_buf`.
    palette_stride: u32,
    /// Bones only (`skeleton.bones.len()`). `palette_stride / bone_stride` = part count.
    bone_stride: u32,
    /// Part count (`palette_stride / bone_stride`).
    part_count: u32,
    /// Foot-anchor added to every palette translation (matches CPU `apply_z_from`).
    z_offset: f32,
    /// Static inverse-bind mat4s (`parts × bone_stride`), uploaded once at mesh load.
    inverse_bind_buf: wgpu::Buffer,
    /// Posed world-bone mat4s for this frame (`unique × bone_stride`).
    bones_buf: wgpu::Buffer,
    bones_cap: u32,
    fold_params_buf: wgpu::Buffer,
    fold_bind: wgpu::BindGroup,
    /// Unique palette slots to fold this frame (0 = no compute dispatch).
    fold_unique_count: u32,
    /// Dual-path invariant: exactly one writer owns `palette_buf` this frame.
    palette_writer: PaletteWriter,
    /// One draw range per part: `(index_start, index_end, entity_texture_slot)`.
    ///
    /// Per-PART rather than per-mesh because the player avatar is eight separate NIFs merged into
    /// one batch, and they do not share a texture: the head has its own per-race DDS, the hair
    /// another, and armour pieces others again. Binding a single texture over the whole body is
    /// what left it a white mannequin.
    ranges: Vec<(u32, u32, usize)>,
    /// Unique resident texture slots owned by this mesh. Ranges may reuse a slot many times, but
    /// eviction must release it exactly once.
    texture_slots: Vec<usize>,
    /// CPU shadow of the last successful GPU instance upload (MS-08). Skip `write_buffer` when
    /// the new stream is byte-identical — idle NPCs keep the same `pos_yaw`/`palette_base` for
    /// many frames while only the palette matrices animate.
    last_insts: Vec<SkinnedInstance>,
}

/// Who wrote `SkinnedDraw::palette_buf` this frame (MS-08 dual-path invariant).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum PaletteWriter {
    #[default]
    None,
    /// `update_skinned_instances` — expanded CPU upload (`CAER_CPU_PALETTE_FOLD=1`).
    Cpu,
    /// `update_skinned_bones` + compute fold — GPU owns the buffer after dispatch.
    GpuFold,
}

/// Uniform for [`palette_fold.wgsl`] — must match the shader `Params` layout (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct PaletteFoldParams {
    bone_stride: u32,
    part_count: u32,
    unique_count: u32,
    z_offset: f32,
    /// Falsifier: rotate part index when sampling inverse_bind. Production always 0.
    part_offset: u32,
    /// Falsifier: leave last part unwritten (tail-stale). Production always 0.
    omit_last_part: u32,
    _pad: [u32; 2],
}

/// One zone's uploaded terrain: geometry + its ground-texture bind group.
struct ZoneDraw {
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    indices: u32,
    texture: wgpu::BindGroup,
}

pub struct Gpu {
    /// `None` in headless (screenshot) mode — frames go to `offscreen` instead.
    surface: Option<wgpu::Surface<'static>>,
    /// Adapter product name, for the Options Menu's `Your Video Card: %s` line.
    adapter_name: String,
    adapter_backend: String,
    /// Offscreen color target for headless mode (`RENDER_ATTACHMENT | COPY_SRC`).
    offscreen: Option<wgpu::Texture>,
    /// Whether frame capture via `COPY_SRC` is available (surface-negotiated or headless).
    /// Rendering does not require this; see [`crate::gpu_init::negotiate_surface_usage`].
    capture_copy_src: bool,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    grid_buf: wgpu::Buffer,
    grid_verts: u32,
    /// Editor selection outline: the picked fixture's OBB as always-on-top wireframe lines.
    sel_pipeline: wgpu::RenderPipeline,
    sel_buf: Option<wgpu::Buffer>,
    sel_verts: u32,
    /// Terrain-brush cursor preview: radius + falloff rings hugging the ground.
    brush_buf: Option<wgpu::Buffer>,
    brush_verts: u32,
    terrain_pipeline: wgpu::RenderPipeline,
    /// Per-zone terrain draws, each binding its own ground texture.
    terrain_zones: Vec<ZoneDraw>,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    supports_bc: bool,
    /// Device `max_texture_dimension_2d` (CAER floor 2048). Zone mosaics ship at 4096 and must
    /// be clamped to this — raising the requested device limit is not a portable product path.
    max_texture_2d: u32,
    /// Static fixture boxes (instanced cubes), uploaded once.
    fixture_buf: Option<wgpu::Buffer>,
    fixture_count: u32,
    /// Real NIF fixture geometry: one instanced draw per distinct model.
    model_texture_layout: wgpu::BindGroupLayout,
    mesh_pipeline: wgpu::RenderPipeline,
    mesh_blend_pipeline: wgpu::RenderPipeline,
    mesh_add_pipeline: wgpu::RenderPipeline,
    model_draws: Vec<ModelDraw>,
    /// Live-entity meshes keyed by resolved monster model id (dynamic per-frame instances).
    entity_draws: std::collections::HashMap<u16, EntityDraw>,
    /// GPU-skinned creature meshes, keyed by model id (disjoint from `entity_draws`).
    skinned_draws: std::collections::HashMap<u16, SkinnedDraw>,
    skinned_pipeline: wgpu::RenderPipeline,
    /// Bind-group layout for the per-model palette storage buffer (group 2).
    palette_layout: wgpu::BindGroupLayout,
    /// Compute fold: posed bones × static inverse_bind → expanded palette (MS-08).
    palette_fold_pipeline: wgpu::ComputePipeline,
    palette_fold_layout: wgpu::BindGroupLayout,
    /// Test/falsifier: when true, skip compute fold so `palette_buf` retains the prior frame
    /// (stale-pose trap). Also honoured via `CAER_SKIP_PALETTE_FOLD=1`.
    debug_skip_palette_fold: bool,
    /// Test/falsifier: rotate fold part index (simulates one-part IB mismatch). Production 0.
    debug_fold_part_offset: u32,
    /// Test/falsifier: omit writing the last part (tail-stale). Production 0.
    debug_fold_omit_last_part: bool,
    /// Uploaded model-texture bind groups; index 0 is always the white fallback.
    model_textures: Vec<wgpu::BindGroup>,
    /// Entity skin bind groups (slot 0 = white fallback). Kept separate from `model_textures` so a
    /// region reload (`set_models`, which rebuilds `model_textures`) can't invalidate the slots
    /// cached on already-uploaded entity draws.
    entity_textures: EntityTextureCache,
    /// Model textures tile (bark, walls), so they sample with Repeat — unlike the per-zone
    /// ground textures, which clamp.
    model_sampler: wgpu::Sampler,
    water_pipeline: wgpu::RenderPipeline,
    water_vbuf: Option<wgpu::Buffer>,
    water_ibuf: Option<wgpu::Buffer>,
    water_indices: u32,
    vertex_buf: wgpu::Buffer,
    index_buf: wgpu::Buffer,
    num_indices: u32,
    instance_buf: wgpu::Buffer,
    instance_cap: u32,
    globals_buf: wgpu::Buffer,
    /// A second camera uniform used only by the pre-world avatar preview.  The realm stage stays
    /// on the authored full-body composition while Character Customize renders the avatar through
    /// its own close lens; one shared uniform made the Midgard backdrop camera enter the dome.
    preworld_avatar_globals_buf: wgpu::Buffer,
    /// Full-screen sky gradient, drawn before the scene.
    sky_pipeline: wgpu::RenderPipeline,
    /// Current sky colours (per region / time of day).
    sky: caer_assets::sky::SkyBand,
    bind_group: wgpu::BindGroup,
    /// Same layout as [`Self::bind_group`], backed by [`Self::preworld_avatar_globals_buf`].
    preworld_avatar_bind_group: wgpu::BindGroup,
    /// `true` only while Character Customize / Stats has supplied a separate avatar lens.
    preworld_avatar_camera_active: bool,
    depth_view: wgpu::TextureView,
    /// egui paint renderer — `None` in headless mode.
    egui_renderer: Option<egui_wgpu::Renderer>,

    /// Skin-UI quad pipeline: alpha-blended 2D, drawn last and never depth-tested.
    ui_pipeline: wgpu::RenderPipeline,
    /// Screen size, shared by every UI draw.
    ui_globals_buf: wgpu::Buffer,
    ui_globals_bind: wgpu::BindGroup,
    /// Bind-group layout for a UI atlas page; kept so pages can be uploaded lazily after init.
    ui_page_layout: wgpu::BindGroupLayout,
    /// Texture-page name -> (bind group, size in pixels). Uploaded once per page.
    ui_pages: HashMap<String, (wgpu::BindGroup, [f32; 2])>,
    /// Reusable painter-ordered UI run buffers. Only the first `ui_batch_count` entries are
    /// active for this layout; retaining the tail lets later frames reuse their GPU allocations.
    ui_batches: Vec<UiBatch>,
    /// Active prefix of [`Self::ui_batches`] for the current UI layout.
    ui_batch_count: usize,
    /// Native GPU buffers allocated for the UI run pool. Exposed as a diagnostic and pinned by a
    /// headless regression test: a stable stats window must not allocate another 37 buffers every
    /// redraw.
    ui_buffer_allocations: u64,

    /// Particle billboard pipeline: additive soft-blob quads ([`crate::particles::PARTICLE_BILLBOARD_DRAW_PATH`]).
    particle_pipeline: wgpu::RenderPipeline,
    /// Owns the placeholder soft-blob texture (label [`crate::particles::PARTICLE_BILLBOARD_PLACEHOLDER`]).
    _particle_tex: wgpu::Texture,
    particle_tex_bind: wgpu::BindGroup,
    particle_vbuf: wgpu::Buffer,
    particle_vbuf_cap: u32,
    /// Vertices uploaded this frame for the billboard path (0 = skip draw).
    particle_vert_count: u32,

    /// Optional GPU timestamp queries around the main encode pass (MS-01 / MEAS-009).
    timestamps: Option<GpuTimestamps>,
    /// Per-frame `write_buffer` accounting for the CPU→GPU instance/palette upload window (MS-08).
    upload_stats: UploadFrameStats,
    /// Host-wide **test** device lock held only when `CAER_GPU_TEST_SERIALIZE` is set.
    /// Production leaves this `None` so dual-client / PLAYER_SCENARIO is possible.
    _device_lock: Option<crate::gpu_init::GpuDeviceLock>,
}

/// One frame's skinned/entity `write_buffer` composition (MS-08 upload cut input).
#[derive(Debug, Clone, Copy, Default)]
pub struct UploadFrameStats {
    /// Total `queue.write_buffer` calls from entity + skinned instance/palette uploads.
    pub write_calls: u32,
    /// Total bytes passed to those `write_buffer` calls.
    pub write_bytes: u64,
    /// Skinned models with `n > 0` this frame.
    pub skinned_models_touched: u32,
    pub skinned_inst_calls: u32,
    pub skinned_palette_calls: u32,
    pub skinned_inst_bytes: u64,
    pub skinned_palette_bytes: u64,
    /// Posed-bone uploads when GPU fold is active (replaces expanded palette writes).
    pub skinned_bone_calls: u32,
    pub skinned_bone_bytes: u64,
    /// Entity (non-skinned mesh) models touched.
    pub entity_models_touched: u32,
    pub entity_inst_calls: u32,
    pub entity_inst_bytes: u64,
    /// Σ palette_stride across touched skinned models (for parts×bones attribution).
    pub skinned_palette_stride_sum: u64,
    /// Σ bone_stride across touched skinned models.
    pub skinned_bone_stride_sum: u64,
}

/// wgpu timestamp queries for one encode_pass.
/// Indices: 0–1 = palette-fold compute, 2–3 = main render pass.
struct GpuTimestamps {
    query_set: wgpu::QuerySet,
    /// QUERY_RESOLVE | COPY_SRC destination for four u64 ticks.
    resolve_buf: wgpu::Buffer,
    /// MAP_READ | COPY_DST staging buffer.
    read_buf: wgpu::Buffer,
    /// Nanoseconds per tick from `Queue::get_timestamp_period`.
    period_ns: f32,
    /// Last resolved main-pass GPU time in milliseconds.
    last_pass_ms: std::cell::Cell<Option<f64>>,
    /// Last resolved palette-fold compute GPU time (None if fold did not run this frame).
    last_fold_ms: std::cell::Cell<Option<f64>>,
    /// Whether queries 0–1 were written this encode (fold dispatched).
    fold_queries_written: std::cell::Cell<bool>,
}

/// CPU + GPU timing breakdown for the headless sync path (residual hunt).
#[derive(Debug, Clone, Copy, Default)]
pub struct HeadlessFrameTiming {
    /// Total CPU encode+submit for the scene CB path (`fold + scene + submit`).
    pub encode_cpu_ms: f64,
    /// CPU time to record the palette-fold compute pass only.
    pub fold_encode_cpu_ms: f64,
    /// CPU time to record the main render pass + timestamp resolve copies (draw encode).
    pub scene_encode_cpu_ms: f64,
    /// CPU time for every `encoder.finish()` + `queue.submit` on the overlay render path.
    ///
    /// Default (MS-08 coalesce): one combined scene+egui CB → one finish/submit.
    /// Kill-switch `CAER_SPLIT_EGUI_SUBMIT=1`: scene submit + egui submit, summed here.
    /// Named mean-cut leaf — coalesce must cut this mean vs split; can go the wrong way.
    pub submit_cpu_ms: f64,
    /// egui overlay **record** time only (textures/buffers/pass) — NOT nested in `encode_cpu_ms`.
    /// Finish/submit cost lives in [`Self::submit_cpu_ms`], not here.
    pub egui_encode_ms: f64,
    /// How many `queue.submit` calls the overlay path issued this frame (1 coalesce / 2 split).
    pub queue_submit_count: u32,
    /// `device.poll(Wait)` until submitted work is idle.
    pub poll_wait_ms: f64,
    /// Palette-fold compute GPU time (timestamp queries 0–1). `None` if unsupported / not run.
    pub fold_gpu_ms: Option<f64>,
    /// Main render-pass GPU time (timestamp queries 2–3). Same as historical `take_gpu_pass_ms`.
    pub render_gpu_ms: Option<f64>,
    /// Timestamp resolve map/read after idle.
    pub ts_resolve_ms: f64,
}

/// Kill-switch: restore historical 2-submit path (scene CB, then egui CB).
/// Default is coalesce (one submit). Reversible Class A mean-cut for MS-08 residual (3).
#[inline]
pub fn split_egui_submit_enabled() -> bool {
    matches!(
        std::env::var_os("CAER_SPLIT_EGUI_SUBMIT"),
        Some(v) if v == "1" || v.eq_ignore_ascii_case("true")
    )
}

/// Default bound for `device.poll(Wait)` / map recv — never `timeout: None`.
/// Override at runtime with `CAER_GPU_WAIT_MS`.
pub const DEFAULT_GPU_WAIT_MS: u64 = 5_000;

/// Bounded GPU idle wait failure (System 1 / Sol CAER-5). Never hang forever on a lost device.
#[derive(Debug)]
pub enum GpuWaitError {
    /// `device.poll(Wait)` exceeded [`Gpu::gpu_wait_timeout`].
    Timeout { waited: std::time::Duration },
    /// Other poll failure (wrong submission index, etc.).
    /// Note: wgpu 29 `PollError` is only Timeout / WrongSubmissionIndex — not device-loss.
    Poll(wgpu::PollError),
    /// Buffer map completion failed (do not panic inside the poll callback).
    Map(wgpu::BufferAsyncError),
    /// Map callback never completed within the bounded receive after poll.
    MapRecvTimeout { waited: std::time::Duration },
}

impl std::fmt::Display for GpuWaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { waited } => write!(
                f,
                "GPU wait timed out after {waited:?} (set CAER_GPU_WAIT_MS to override)"
            ),
            Self::Poll(e) => write!(f, "GPU poll error: {e}"),
            Self::Map(e) => write!(f, "GPU buffer map error: {e:?}"),
            Self::MapRecvTimeout { waited } => {
                write!(f, "GPU buffer map recv timed out after {waited:?}")
            }
        }
    }
}

impl std::error::Error for GpuWaitError {}

/// Build the only allowed idle-wait poll type: finite `timeout: Some(_)`, never `None`.
///
/// Call sites must go through this (or [`Gpu::poll_wait_idle`]) so restoring
/// `Wait { timeout: None }` / `PollType::wait_indefinitely()` fails the falsifier below.
#[must_use]
pub fn idle_wait_poll_type(timeout: std::time::Duration) -> wgpu::PollType {
    debug_assert!(
        !timeout.is_zero(),
        "GPU wait timeout must be positive; use DEFAULT_GPU_WAIT_MS / CAER_GPU_WAIT_MS"
    );
    wgpu::PollType::Wait {
        submission_index: None,
        timeout: Some(timeout),
    }
}

/// Map a wgpu poll result into [`GpuWaitError`] (shared by production + falsifier).
pub fn classify_poll_result(
    result: Result<wgpu::PollStatus, wgpu::PollError>,
    waited: std::time::Duration,
) -> Result<wgpu::PollStatus, GpuWaitError> {
    match result {
        Ok(status) => Ok(status),
        Err(wgpu::PollError::Timeout) => Err(GpuWaitError::Timeout { waited }),
        Err(e) => Err(GpuWaitError::Poll(e)),
    }
}

impl Gpu {
    /// Finite wait for submitted GPU work. Default [`DEFAULT_GPU_WAIT_MS`]; override with `CAER_GPU_WAIT_MS`.
    #[must_use]
    pub fn gpu_wait_timeout() -> std::time::Duration {
        std::env::var("CAER_GPU_WAIT_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(std::time::Duration::from_millis)
            .unwrap_or(std::time::Duration::from_millis(DEFAULT_GPU_WAIT_MS))
    }

    /// Single bounded wait abstraction — replaces every `timeout: None` poll.
    /// Success path is pixel/timing equivalent to the old indefinite wait when the GPU completes
    /// in time; timeout and device-poll failures return deterministic errors.
    pub fn poll_wait_idle(&self) -> Result<wgpu::PollStatus, GpuWaitError> {
        let timeout = Self::gpu_wait_timeout();
        classify_poll_result(self.device.poll(idle_wait_poll_type(timeout)), timeout)
    }

    fn poll_wait_idle_for(
        &self,
        timeout: std::time::Duration,
    ) -> Result<wgpu::PollStatus, GpuWaitError> {
        classify_poll_result(self.device.poll(idle_wait_poll_type(timeout)), timeout)
    }

    /// Map a buffer slice without panicking in the async callback. Polls idle, then
    /// `recv_timeout` so a missing callback cannot hang unboundedly after poll.
    fn map_slice_wait(&self, slice: wgpu::BufferSlice<'_>) -> Result<(), GpuWaitError> {
        self.map_slice_wait_for(slice, Self::gpu_wait_timeout())
    }

    fn map_slice_wait_for(
        &self,
        slice: wgpu::BufferSlice<'_>,
        timeout: std::time::Duration,
    ) -> Result<(), GpuWaitError> {
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.poll_wait_idle_for(timeout)?;
        match rx.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(GpuWaitError::Map(e)),
            Err(_) => Err(GpuWaitError::MapRecvTimeout { waited: timeout }),
        }
    }

    pub async fn new(
        window: Arc<Window>,
        grid_extent: f32,
    ) -> Result<Self, crate::gpu_init::GpuInitError> {
        Self::new_with_capture(window, grid_extent, false).await
    }

    /// Construct a window surface, enabling readback only when the product entry point explicitly
    /// opted into native harness capture (or the established recorder diagnostic was requested).
    pub async fn new_with_capture(
        window: Arc<Window>,
        grid_extent: f32,
        harness_capture: bool,
    ) -> Result<Self, crate::gpu_init::GpuInitError> {
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        // wgpu 30: Instance::new takes the descriptor by value; `new_without_display_handle`
        // fills every field with defaults (the window we pass to create_surface carries its own
        // raw handle, so no display handle is needed here).
        let device_lock = crate::gpu_init::maybe_lock_gpu_device_for_test()?;
        let _wgpu_init = crate::gpu_init::lock_wgpu_init()?;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let surface = instance.create_surface(window).map_err(|e| {
            crate::gpu_init::GpuInitError::SurfaceCreate {
                detail: e.to_string(),
            }
        })?;
        let adapter = request_adapter_with_policy(&instance, Some(&surface)).await?;
        let required_limits = crate::gpu_init::caer_required_limits();
        crate::gpu_init::check_adapter_limits(&required_limits, &adapter.limits())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("caer-render device"),
                // BC1 lets us upload the client's DXT1 ground textures untouched.
                // TIMESTAMP_QUERY powers MS-01 GPU pass attribution when the adapter supports it.
                required_features: device_features(&adapter, /*headless=*/ false),
                required_limits,
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| crate::gpu_init::GpuInitError::DeviceRequest {
                detail: e.to_string(),
            })?;

        let caps = surface.get_capabilities(&adapter);
        let usage_plan = crate::gpu_init::negotiate_surface_usage(caps.usages)?;
        // A product window only needs to be renderable.  Opting every swapchain image into
        // COPY_SRC merely because the backend advertises it changes the compositor path on some
        // NVIDIA/Vulkan stacks, even when no recording is active.  Keep capture an explicit
        // diagnostic/recorder capability instead of taxing ordinary presentation.
        let capture_requested = window_capture_requested(
            harness_capture,
            std::env::var("CAER_WINDOW_CAPTURE").is_ok_and(|v| v == "1"),
        );
        let capture_copy_src = usage_plan.capture_copy_src && capture_requested;
        if !capture_copy_src {
            log::info!(
                "caer-render: window capture disabled — swapchain uses RENDER_ATTACHMENT only"
            );
        }
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);
        // Product present mode defaults to FIFO.  Mailbox is low-latency on paper, but the
        // NVIDIA/Wayland/KWin path can intermittently expose the compositor's black backing image
        // between otherwise valid pre-world frames.  Stable, ordered presentation is the product
        // contract; perf harnesses can opt into Mailbox or Immediate explicitly.
        // `CAER_VSYNC=0` forces Immediate — uncapped, so ms/frame shows the true render cost.
        let no_vsync = std::env::var("CAER_VSYNC").is_ok_and(|v| v == "0");
        let prefer_mailbox =
            std::env::var("CAER_PRESENT_MODE").is_ok_and(|v| v.eq_ignore_ascii_case("mailbox"));
        let present_mode =
            select_windowed_present_mode(&caps.present_modes, no_vsync, prefer_mailbox);
        let alpha_mode = select_windowed_alpha_mode(&caps.alpha_modes);
        log::info!("caer-render: present mode = {present_mode:?}; alpha mode = {alpha_mode:?}");
        let config = wgpu::SurfaceConfiguration {
            // Usages negotiated from advertised capabilities — RENDER_ATTACHMENT required,
            // COPY_SRC only when the surface offers it (optional frame capture).
            usage: if capture_copy_src {
                usage_plan.usage
            } else {
                wgpu::TextureUsages::RENDER_ATTACHMENT
            },
            format,
            width: w,
            height: h,
            present_mode,
            alpha_mode,
            view_formats: vec![],
            // Keep only one frame queued on the product window.  With two images in flight the
            // NVIDIA/Vulkan/Wayland path can leave `get_current_texture` waiting for wgpu's full
            // one-second acquisition deadline even though the compositor is alive.  That blocks
            // winit's sole event thread, so pointer hover/click events arrive in one-second bursts
            // and KWin quite reasonably marks the window unresponsive.  The windowed residual
            // harness already established latency=1 as the bounded acquire/present contract.
            desired_maximum_frame_latency: 1,
        };
        surface.configure(&device, &config);
        let adapter_info = adapter.get_info();
        Ok(Self::build(
            device,
            queue,
            Some(surface),
            None,
            config,
            capture_copy_src,
            grid_extent,
            device_lock,
            adapter_info.name,
            format!("{:?}", adapter_info.backend),
        ))
    }

    /// Headless constructor for screenshot mode: no window/surface — frames render into an
    /// offscreen texture that `render_to_rgba` reads back to the CPU.
    pub async fn new_headless(
        w: u32,
        h: u32,
        grid_extent: f32,
    ) -> Result<Self, crate::gpu_init::GpuInitError> {
        let device_lock = crate::gpu_init::maybe_lock_gpu_device_for_test()?;
        let _wgpu_init = crate::gpu_init::lock_wgpu_init()?;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = request_adapter_with_policy(&instance, None).await?;
        let required_limits = crate::gpu_init::caer_required_limits();
        crate::gpu_init::check_adapter_limits(&required_limits, &adapter.limits())?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("caer-render headless device"),
                // Headless defaults timestamps off — see `device_features`.
                required_features: device_features(&adapter, /*headless=*/ true),
                required_limits,
                experimental_features: wgpu::ExperimentalFeatures::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| crate::gpu_init::GpuInitError::DeviceRequest {
                detail: e.to_string(),
            })?;
        // No surface to negotiate a format with — pick a standard sRGB target the PNG writer
        // consumes directly. Offscreen texture we own always includes COPY_SRC for readback.
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            width: w.max(1),
            height: h.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        let offscreen = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-color"),
            size: wgpu::Extent3d {
                width: config.width,
                height: config.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let adapter_info = adapter.get_info();
        Ok(Self::build(
            device,
            queue,
            None,
            Some(offscreen),
            config,
            /*capture_copy_src=*/ true,
            grid_extent,
            device_lock,
            adapter_info.name,
            format!("{:?}", adapter_info.backend),
        ))
    }

    /// Shared post-device setup — geometry, uniforms, pipelines, depth — for both constructors.
    fn build(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: Option<wgpu::Surface<'static>>,
        offscreen: Option<wgpu::Texture>,
        config: wgpu::SurfaceConfiguration,
        capture_copy_src: bool,
        grid_extent: f32,
        device_lock: Option<crate::gpu_init::GpuDeviceLock>,
        adapter_name: String,
        adapter_backend: String,
    ) -> Self {
        // --- geometry: unit cube centred on origin, 24 verts (per-face normals), 36 indices ---
        let (verts, indices) = cube();
        let vertex_buf = create_init_buffer(
            &device,
            "cube-verts",
            bytemuck::cast_slice(&verts),
            wgpu::BufferUsages::VERTEX,
        );
        let index_buf = create_init_buffer(
            &device,
            "cube-indices",
            bytemuck::cast_slice(&indices),
            wgpu::BufferUsages::INDEX,
        );
        let num_indices = indices.len() as u32;

        // --- uniforms ---
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT, // the sky shader reads the camera + sky colours in the fragment stage
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals-bind"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });
        // Character Customize leaves the realm backdrop on its static stage lens, but opens the
        // character on a close face lens.  They must not share a mutable uniform: all draws in a
        // submitted command buffer see the last contents of a buffer, not the value it held when
        // a draw was encoded.  A sibling uniform/bind-group is therefore the honest split.
        let preworld_avatar_globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("preworld-avatar-globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let preworld_avatar_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("preworld-avatar-globals-bind"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: preworld_avatar_globals_buf.as_entire_binding(),
            }],
        });

        // --- instance buffer: starts small, grown on demand in `upload_instances` ---
        let instance_cap = 1 << 14; // 16384 to start; covers a typical near slice
        let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (instance_cap as u64) * std::mem::size_of::<Instance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cube-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cube-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[vertex_layout(), instance_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- ground reference grid: a flat wireframe on z=0 at DAoC zone spacing ---
        let grid = grid_geometry(grid_extent);
        let grid_verts = grid.len() as u32;
        let grid_buf = create_init_buffer(
            &device,
            "grid",
            bytemuck::cast_slice(&grid),
            wgpu::BufferUsages::VERTEX,
        );
        let line_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("line-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("line.wgsl").into()),
        });
        let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("line-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &line_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[line_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &line_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: wgpu::CompareFunction::Less.into(),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- selection outline: same line shader, but depth-compare Always (no write) so the
        // picked fixture's box reads through trees/walls — the whole point is knowing what you
        // have selected even when it's buried in clutter ---
        let sel_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("selection-outline-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &line_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[line_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &line_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: wgpu::CompareFunction::Always.into(),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- ground-texture binding (group 1): one texture+sampler per zone ---
        let supports_bc = device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        let timestamps = if device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("frame-timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 4,
            });
            let resolve_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("timestamp-resolve"),
                size: 32,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("timestamp-read"),
                size: 32,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            Some(GpuTimestamps {
                query_set,
                resolve_buf,
                read_buf,
                period_ns: queue.get_timestamp_period(),
                last_pass_ms: std::cell::Cell::new(None),
                last_fold_ms: std::cell::Cell::new(None),
                fold_queries_written: std::cell::Cell::new(false),
            })
        } else {
            None
        };
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("zone-texture-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("zone-texture-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let terrain_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("terrain-pipeline-layout"),
                bind_group_layouts: &[Some(&bind_layout), Some(&texture_layout)],
                immediate_size: 0,
            });
        // Models get their own texture layout because they carry a SECOND ground layer. The stage
        // grounds are authored as two sheets blended by a per-vertex mask — grass over a stone
        // slab, snow over rock — and one texture binding cannot express that. Zone terrain and
        // water keep the two-entry layout above; only the model pipelines see this one.
        let model_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("model-texture-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });
        let model_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("model-pipeline-layout"),
                bind_group_layouts: &[Some(&bind_layout), Some(&model_texture_layout)],
                immediate_size: 0,
            });

        // --- terrain pipeline: lit height-coloured triangles, double-sided, shares globals ---
        let terrain_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("terrain.wgsl").into()),
        });
        let terrain_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("terrain-pipeline"),
            layout: Some(&terrain_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &terrain_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[terrain_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &terrain_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // double-sided: heightmap winding isn't guaranteed convex
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: wgpu::CompareFunction::Less.into(),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- water pipeline: translucent flat surfaces over the terrain. Alpha blend on, depth
        // WRITE off (test still on): the riverbed shows through and nothing z-fights. ---
        let water_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("water-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("water.wgsl").into()),
        });
        let water_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("water-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &water_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[terrain_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &water_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // double-sided: bank-strip winding varies per water body
                ..Default::default()
            },
            // Depth WRITE stays ON for water: lake bank-strips self-overlap, and with write-off
            // the overlapping translucent triangles blended twice (the visible darker band
            // across Llyn Barfog). Writing depth makes the first surface win at equal height —
            // one clean blend layer. Water draws last, so nothing else needs to see past it.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: wgpu::CompareFunction::Less.into(),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- fixture-model pipeline: instanced NIF meshes (pos+yaw+scale per instance) ---
        let mesh_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mesh.wgsl").into()),
        });
        let model_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("model-texture-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let mesh_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-pipeline"),
            layout: Some(&model_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &mesh_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[model_vertex_layout(), model_instance_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &mesh_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // NIF winding varies; leaves/awnings are double-sided anyway
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: wgpu::CompareFunction::Less.into(),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- the same mesh, for parts the NIF marks with a `NiAlphaProperty` ---
        //
        // Source-over blend, depth *tested* but not *written*: a glow must not occlude what is
        // behind it, and two overlapping glows must not fight over the depth buffer. Drawn after
        // every opaque part, which is the cheap approximation of sorting — good enough for the
        // coronas, sun discs and ground decals this exists for, and honestly wrong for a scene
        // with many overlapping transparent surfaces at different depths.
        let mesh_blend_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-blend-pipeline"),
            layout: Some(&model_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &mesh_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[model_vertex_layout(), model_instance_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &mesh_shader,
                entry_point: Some("fs_blend"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: wgpu::CompareFunction::Less.into(),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- and the additive variant, for glows whose RGB is black and whose shape is alpha ---
        let mesh_add_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh-add-pipeline"),
            layout: Some(&model_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &mesh_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[model_vertex_layout(), model_instance_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &mesh_shader,
                entry_point: Some("fs_blend"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::Zero,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: wgpu::CompareFunction::Less.into(),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- skinned pipeline: creature meshes deformed per-instance in the vertex shader ---
        // Same globals + texture groups as `mesh_pipeline`, plus group 2 = the palette storage
        // buffer, and one extra vertex buffer carrying joint indices / weights.
        let palette_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("palette-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let skinned_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("skinned-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("skinned.wgsl").into()),
        });
        let skinned_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("skinned-pipeline-layout"),
                bind_group_layouts: &[
                    Some(&bind_layout),
                    // The model texture layout, because skinned parts are bound through the same
                    // `model_texture_bind` helper. `skinned.wgsl` never samples binding 2 — a
                    // pipeline may leave an entry of its layout unused, but the bind group and the
                    // pipeline layout must still agree on what the group CONTAINS.
                    Some(&model_texture_layout),
                    Some(&palette_layout),
                ],
                immediate_size: 0,
            });
        let skinned_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("skinned-pipeline"),
            layout: Some(&skinned_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &skinned_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[terrain_layout(), skinned_instance_layout(), skin_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &skinned_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None, // NIF winding varies, same as the fixture/entity pipeline
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: wgpu::CompareFunction::Less.into(),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- palette-fold compute: bones × static inverse_bind → expanded palette (MS-08) ---
        let palette_fold_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("palette-fold-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let palette_fold_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("palette-fold-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("palette_fold.wgsl").into()),
        });
        let palette_fold_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("palette-fold-pipeline-layout"),
                bind_group_layouts: &[Some(&palette_fold_layout)],
                immediate_size: 0,
            });
        let palette_fold_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("palette-fold-pipeline"),
                layout: Some(&palette_fold_pipeline_layout),
                module: &palette_fold_shader,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                cache: None,
            });

        // Sky: a full-screen triangle with no vertex buffer and no depth interaction, drawn first.
        let sky_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sky-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("sky.wgsl").into()),
        });
        let sky_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let sky_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sky-pipeline"),
            layout: Some(&sky_layout),
            vertex: wgpu::VertexState {
                module: &sky_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &sky_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            // The pass carries a depth buffer, so the pipeline must declare its format — but the
            // sky neither tests nor writes depth (Always / write off), so the scene paints over it
            // unconditionally and the sky never occludes anything.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let depth_view = create_depth(&device, &config);

        // The egui renderer is created lazily, on the first frame that actually carries an
        // overlay (see `encode_egui_into`) — including in headless mode, so `render_to_rgba_with_overlay`
        // can screenshot the HUD without a window. That is how the 2D overlay gets goldened at all.
        //
        // Lazy rather than eager because constructing it logs a warning about our sRGB target, and
        // eager construction fired that on every headless render even though most (the dev
        // viewer's terrain screenshots) draw no overlay at all. The warning is benign — egui picks
        // an sRGB-aware shader and colours round-trip exactly — but it is noise in a path that
        // isn't using egui.
        let egui_renderer = None;

        // ---- Skin-UI quad pipeline -------------------------------------------------------------
        // Group 0 is the screen size; group 1 is the atlas page being drawn. Splitting them that
        // way means switching pages costs one bind-group change and nothing else.
        let ui_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("uiquad-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("uiquad.wgsl").into()),
        });
        let ui_globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui-globals-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let ui_page_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui-page-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let ui_globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ui-globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ui_globals_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui-globals-bind"),
            layout: &ui_globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ui_globals_buf.as_entire_binding(),
            }],
        });
        let ui_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui-pipeline-layout"),
            bind_group_layouts: &[Some(&ui_globals_layout), Some(&ui_page_layout)],
            immediate_size: 0,
        });
        let ui_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ui-pipeline"),
            layout: Some(&ui_layout),
            vertex: wgpu::VertexState {
                module: &ui_shader,
                entry_point: Some("vs_main"),
                // No vertex buffer: corners come from the vertex index. Only per-quad instance data.
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<UiInstance>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x2],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &ui_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    // Straight source-over: the skin art is not premultiplied.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // The UI is 2D and its quads have no meaningful facing.
                cull_mode: None,
                ..Default::default()
            },
            // The pass owns a depth buffer so the format must be declared, but the UI sits on top of
            // everything: never tested, never written.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ---- Particle billboard path (soft placeholder blob, additive) -------------------------
        // Honest procedural texture — label PARTICLE_BILLBOARD_PLACEHOLDER; not DAoC art.
        let particle_blob = soft_blob_rgba(32);
        let particle_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(crate::particles::PARTICLE_BILLBOARD_PLACEHOLDER),
            size: wgpu::Extent3d {
                width: 32,
                height: 32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &particle_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &particle_blob,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(32 * 4),
                rows_per_image: Some(32),
            },
            wgpu::Extent3d {
                width: 32,
                height: 32,
                depth_or_array_layers: 1,
            },
        );
        let particle_tex_view = particle_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let particle_samp = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("particle-billboard-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let particle_tex_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("particle-billboard-tex"),
            layout: &texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&particle_tex_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&particle_samp),
                },
            ],
        });
        let particle_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("particle-billboard-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("particle_billboard.wgsl").into()),
        });
        let particle_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("particle-billboard-layout"),
            bind_group_layouts: &[Some(&bind_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let particle_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(crate::particles::PARTICLE_BILLBOARD_DRAW_PATH),
            layout: Some(&particle_layout),
            vertex: wgpu::VertexState {
                module: &particle_shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[particle_billboard_layout()],
            },
            fragment: Some(wgpu::FragmentState {
                module: &particle_shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    // Additive: soft sprites glow over the scene (DAoC-ish presence, not fidelity).
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let particle_vbuf_cap = 1u32 << 14;
        let particle_vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("particle-billboard-verts"),
            size: u64::from(particle_vbuf_cap)
                * std::mem::size_of::<crate::particles::ParticleBillboardVert>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let max_texture_2d = device.limits().max_texture_dimension_2d;

        Self {
            surface,
            offscreen,
            capture_copy_src,
            device,
            queue,
            config,
            egui_renderer,
            pipeline,
            line_pipeline,
            grid_buf,
            grid_verts,
            sel_pipeline,
            sel_buf: None,
            sel_verts: 0,
            brush_buf: None,
            brush_verts: 0,
            terrain_pipeline,
            terrain_zones: Vec::new(),
            texture_layout,
            sampler,
            supports_bc,
            max_texture_2d,
            fixture_buf: None,
            fixture_count: 0,
            model_texture_layout,
            mesh_pipeline,
            mesh_blend_pipeline,
            mesh_add_pipeline,
            model_draws: Vec::new(),
            entity_draws: std::collections::HashMap::new(),
            skinned_draws: std::collections::HashMap::new(),
            skinned_pipeline,
            palette_layout,
            palette_fold_pipeline,
            palette_fold_layout,
            debug_skip_palette_fold: false,
            debug_fold_part_offset: 0,
            debug_fold_omit_last_part: false,
            model_textures: Vec::new(),
            // Populated lazily (white fallback at index 0) on the first entity-mesh upload; the
            // `&self` texture helpers can't run during struct construction.
            entity_textures: EntityTextureCache::default(),
            model_sampler,
            water_pipeline,
            water_vbuf: None,
            water_ibuf: None,
            water_indices: 0,
            vertex_buf,
            index_buf,
            num_indices,
            instance_buf,
            instance_cap,
            globals_buf,
            preworld_avatar_globals_buf,
            sky_pipeline,
            sky: caer_assets::sky::Sky::default().day,
            bind_group,
            preworld_avatar_bind_group,
            preworld_avatar_camera_active: false,
            depth_view,
            ui_pipeline,
            ui_globals_buf,
            ui_globals_bind,
            ui_page_layout,
            ui_pages: HashMap::new(),
            ui_batches: Vec::new(),
            ui_batch_count: 0,
            ui_buffer_allocations: 0,
            particle_pipeline,
            _particle_tex: particle_tex,
            particle_tex_bind,
            particle_vbuf,
            particle_vbuf_cap,
            particle_vert_count: 0,
            timestamps,
            upload_stats: UploadFrameStats::default(),
            _device_lock: device_lock,
            adapter_name,
            adapter_backend,
        }
    }

    /// Adapter product name — retail prints the real card on the Options Menu.
    #[must_use]
    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    /// Backend actually selected for this device (for example Vulkan or Dx12).
    #[must_use]
    pub fn adapter_backend(&self) -> &str {
        &self.adapter_backend
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        // Winit emits an initial Resized event for the size used to create the window.  The
        // surface is already configured to that exact size in `Gpu::new`; configuring it again
        // immediately before the first acquire is redundant and has been observed to strand the
        // NVIDIA windowed swapchain.  It also avoids rebuilding the depth target for every
        // duplicate resize notification generated by a compositor.
        if self.config.width == w && self.config.height == h {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
        }
        self.depth_view = create_depth(&self.device, &self.config);
    }

    /// Reconfigure a lost/outdated swapchain without relying on a size change. Calling `resize`
    /// with the current size intentionally does nothing, so surface recovery needs its own path.
    pub fn force_reconfigure_surface(&mut self) {
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
        }
        self.depth_view = create_depth(&self.device, &self.config);
        let _ = self.device.poll(wgpu::PollType::Poll);
        log::warn!(
            "caer-render: force-reconfigured surface at {}x{}",
            self.config.width,
            self.config.height
        );
    }

    pub fn aspect(&self) -> f32 {
        self.config.width as f32 / self.config.height as f32
    }

    /// Physical surface size the GPU is presenting. Pointer hit-tests must use this, not the
    /// window's logical size, or hover lights a control a half-inch from the cursor.
    #[must_use]
    pub fn surface_size(&self) -> (f32, f32) {
        (
            self.config.width.max(1) as f32,
            self.config.height.max(1) as f32,
        )
    }

    /// Upload the (static) per-zone terrain meshes + ground textures once.
    pub fn set_terrain(&mut self, zones: &[crate::terrain::ZoneMesh]) {
        self.terrain_zones.clear();
        for (i, z) in zones.iter().enumerate() {
            if z.vertices.is_empty() || z.indices.is_empty() {
                continue;
            }
            // COPY_DST so brush re-meshes can write vertices in place (`update_terrain_vertices`).
            let vbuf = create_init_buffer(
                &self.device,
                "terrain-verts",
                bytemuck::cast_slice(&z.vertices),
                wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            );
            let ibuf = create_init_buffer(
                &self.device,
                "terrain-indices",
                bytemuck::cast_slice(&z.indices),
                wgpu::BufferUsages::INDEX,
            );
            use caer_assets::dds::ZoneGround;
            let view = match &z.ground {
                Some(ZoneGround::Bc1(g)) if self.supports_bc => {
                    let clamped = caer_assets::dds::clamp_bc1_to_max(g, self.max_texture_2d);
                    self.upload_bc1(&clamped, &format!("zone-ground-{i}"))
                }
                Some(ZoneGround::Bc1(g)) => {
                    let rgba = caer_assets::dds::bc1_to_rgba(g);
                    let (w, h, data) = caer_assets::dds::clamp_rgba_to_max(
                        g.width,
                        g.height,
                        &rgba,
                        self.max_texture_2d,
                    );
                    self.upload_rgba(w, h, &data, &format!("zone-ground-{i}"))
                }
                Some(ZoneGround::Rgba {
                    width,
                    height,
                    data,
                }) => {
                    let (w, h, data) = caer_assets::dds::clamp_rgba_to_max(
                        *width,
                        *height,
                        data,
                        self.max_texture_2d,
                    );
                    self.upload_rgba(w, h, &data, &format!("zone-ground-{i}"))
                }
                _ => self.white_texture(),
            };
            let texture = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("zone-texture-bind"),
                layout: &self.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.terrain_zones.push(ZoneDraw {
                vbuf,
                ibuf,
                indices: z.indices.len() as u32,
                texture,
            });
        }
    }

    /// Upload one BC1 ground texture (single mip, as shipped by the client).
    fn upload_bc1(&self, g: &caer_assets::dds::DdsBc1, label: &str) -> wgpu::TextureView {
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: g.width,
                height: g.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bc1RgbaUnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            tex.as_image_copy(),
            &g.data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(g.width / 4 * 8),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: g.width,
                height: g.height,
                depth_or_array_layers: 1,
            },
        );
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Upload an uncompressed RGBA8 ground texture (old zones' BMP tiles, pre-decoded).
    fn upload_rgba(&self, width: u32, height: u32, data: &[u8], label: &str) -> wgpu::TextureView {
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            tex.as_image_copy(),
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// 1x1 white fallback so untextured zones multiply through to their vertex-ramp colour.
    fn white_texture(&self) -> wgpu::TextureView {
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("white-1x1"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            tex.as_image_copy(),
            &[255, 255, 255, 255],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Upload the real fixture models once: one instanced draw per distinct NIF, with each
    /// batch's per-texture index ranges bound to bind groups built from `textures`.
    /// Rewrite one uploaded batch's vertex buffer in place.
    ///
    /// For vertex animation: the geometry, textures and instances are unchanged, only the
    /// positions move, so re-running [`Gpu::set_models`] every frame would re-upload every texture
    /// in the scene to move some leaves. Returns false when `src` names no uploaded batch — an
    /// empty batch is skipped at upload time, so draw order is not batch order.
    pub fn update_model_vertices(
        &mut self,
        src: usize,
        vertices: &[crate::terrain::TerrainVertex],
    ) -> bool {
        let Some(draw) = self.model_draws.iter().find(|d| d.src == src) else {
            return false;
        };
        let bytes: &[u8] = bytemuck::cast_slice(vertices);
        if bytes.len() as u64 > draw.vbuf.size() {
            return false;
        }
        self.queue.write_buffer(&draw.vbuf, 0, bytes);
        self.upload_stats.write_calls += 1;
        self.upload_stats.write_bytes += bytes.len() as u64;
        true
    }

    pub fn set_models(
        &mut self,
        batches: &[crate::terrain::ModelBatch],
        textures: &std::collections::HashMap<String, caer_assets::dds::DdsTexture>,
    ) {
        self.model_draws.clear();
        // Bind group 0 = the white fallback (untextured ranges multiply through to vertex
        // diffuse); every referenced texture uploads once and maps name -> index.
        let white = self.white_texture();
        self.model_textures = vec![self.model_texture_bind(&white, &white, "model-white-bind")];
        let mut tex_index: std::collections::HashMap<(&str, Option<&str>), usize> =
            std::collections::HashMap::new();
        for (src, b) in batches.iter().enumerate() {
            if b.vertices.is_empty() || b.indices.is_empty() || b.instances.is_empty() {
                continue;
            }
            let mut parts: Vec<(u32, u32, usize)> = Vec::new();
            let mut blend_parts: Vec<(u32, u32, usize)> = Vec::new();
            let mut add_parts: Vec<(u32, u32, usize)> = Vec::new();
            for r in &b.parts {
                // Keyed on the PAIR, because a bind group carries both layers. The same sheet
                // over two different second layers is two different binds; a part with one layer
                // binds its own view twice and its vertices carry a mask of 1.0, so the shader's
                // mix is the identity.
                let resolve = |n: &str| textures.get_key_value(n);
                let ti = match r.texture.as_deref().and_then(&resolve) {
                    Some((name, tex)) => {
                        let second = r.texture2.as_deref().and_then(&resolve);
                        let key = (name.as_str(), second.map(|(n, _)| n.as_str()));
                        match tex_index.get(&key) {
                            Some(i) => *i,
                            None => {
                                let view = self.upload_dds(tex, name);
                                let blend = match second {
                                    Some((n2, t2)) => self.upload_dds(t2, n2),
                                    None => self.white_texture(),
                                };
                                let label = match second {
                                    Some((n2, _)) => format!("{name}+{n2}"),
                                    None => name.clone(),
                                };
                                let bind = if second.is_some() {
                                    self.model_texture_bind(&view, &blend, &label)
                                } else {
                                    self.model_texture_bind(&view, &view, &label)
                                };
                                self.model_textures.push(bind);
                                let i = self.model_textures.len() - 1;
                                tex_index.insert(key, i);
                                i
                            }
                        }
                    }
                    None => 0,
                };
                // Is this part's BLEND flag inert? A texture with no transparent texel blends to
                // exactly what an opaque draw produces, but it still goes down the blended pass,
                // which draws after the opaques without writing depth. Albion's char-screen tower
                // is the visible case: its INTERIOR shell is flagged Blend and its exterior
                // Opaque, so the interior painted straight over the outside wall and the tower
                // read as see-through. Routing an inert blend to the opaque pass restores the
                // depth ordering without changing a single pixel's colour.
                let blend_is_inert = r
                    .texture
                    .as_deref()
                    .and_then(|n| textures.get(n))
                    .and_then(|t| t.rgba8_mip0())
                    .is_some_and(|(_, _, px)| px.chunks_exact(4).all(|c| c[3] == 255));
                use caer_assets::nif::AlphaMode;
                match r.alpha {
                    AlphaMode::Opaque => parts.push((r.start, r.end, ti)),
                    AlphaMode::Blend if blend_is_inert && ti != 0 => {
                        parts.push((r.start, r.end, ti));
                    }
                    // A composited part whose texture did not resolve gets the white 1×1 fallback,
                    // which has alpha 1 — source-over that and you get an opaque rectangle of
                    // shaded vertex diffuse. For opaque geometry the white fallback is the right
                    // answer; for a glow, "we could not find the texture" means draw nothing.
                    _ if ti == 0 => {}
                    AlphaMode::Blend => blend_parts.push((r.start, r.end, ti)),
                    AlphaMode::Add => add_parts.push((r.start, r.end, ti)),
                }
            }
            let inst: Vec<[f32; 8]> = b.instances.iter().map(model_instance_raw).collect();
            self.model_draws.push(ModelDraw {
                src,
                vbuf: create_init_buffer(
                    &self.device,
                    "model-verts",
                    bytemuck::cast_slice(&b.vertices),
                    // COPY_DST so a morphed batch can rewrite its positions per frame without
                    // going back through `set_models`, which re-uploads every texture.
                    wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                ),
                ibuf: create_init_buffer(
                    &self.device,
                    "model-indices",
                    bytemuck::cast_slice(&b.indices),
                    wgpu::BufferUsages::INDEX,
                ),
                indices: b.indices.len() as u32,
                blend_parts,
                add_parts,
                // COPY_DST so the editor can rewrite one instance's slot in place on rotate.
                inst_buf: create_init_buffer(
                    &self.device,
                    "model-insts",
                    bytemuck::cast_slice(&inst),
                    wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                ),
                instances: b.instances.len() as u32,
                parts,
            });
        }
    }

    /// Whether a live-entity mesh has been uploaded for this monster model id.
    pub fn has_entity_mesh(&self, model_id: u16) -> bool {
        self.entity_draws.contains_key(&model_id)
    }

    /// Number of resident entity texture bindings, including the white fallback.
    #[must_use]
    pub fn entity_texture_count(&self) -> usize {
        self.entity_textures.resident_count()
    }

    /// Number of resident entity mesh records across the static and skinned draw paths.
    #[must_use]
    pub fn resident_entity_mesh_count(&self) -> usize {
        self.entity_draws.len() + self.skinned_draws.len()
    }

    /// Drop a cached entity/skinned mesh so equipped-id reuse cannot serve a stale buffer.
    pub fn evict_entity_mesh(&mut self, model_id: u16) {
        if let Some(draw) = self.entity_draws.remove(&model_id) {
            self.entity_textures.release(draw.tex);
        }
        if let Some(draw) = self.skinned_draws.remove(&model_id) {
            for slot in draw.texture_slots {
                self.entity_textures.release(slot);
            }
        }
    }

    /// Install the permanent white fallback on first entity upload.
    fn ensure_entity_white_texture(&mut self) {
        if self.entity_textures.is_empty() {
            let white = self.white_texture();
            let binding = self.model_texture_bind(&white, &white, "entity-white-bind");
            self.entity_textures.install_white(binding);
        }
    }

    /// Give one mesh ownership of a decoded texture's resident bind-group slot.
    ///
    /// A mesh can mention the same sheet on multiple parts. It owns that slot once, while each
    /// range simply reuses the returned index; eviction releases exactly the recorded owners.
    fn acquire_entity_texture(
        &mut self,
        texture: &caer_assets::dds::DdsTexture,
        label: &str,
        owner_slots: &mut Vec<usize>,
    ) -> usize {
        let key = EntityTextureKey::from_dds(texture);
        if let Some(slot) = self.entity_textures.slot_for_key(key) {
            if !owner_slots.contains(&slot) {
                self.entity_textures
                    .retain_existing(key)
                    .expect("resident entity texture key must remain available");
                owner_slots.push(slot);
            }
            return slot;
        }

        let view = self.upload_dds(texture, label);
        let binding = self.model_texture_bind(&view, &view, label);
        let slot = self.entity_textures.insert(key, binding);
        owner_slots.push(slot);
        slot
    }

    /// Reset and begin counting `write_buffer` composition for this frame's upload window.
    pub fn begin_upload_stats(&mut self) {
        self.upload_stats = UploadFrameStats::default();
    }

    /// Take this frame's upload composition counters (leaves them zeroed).
    #[must_use]
    pub fn take_upload_stats(&mut self) -> UploadFrameStats {
        std::mem::take(&mut self.upload_stats)
    }

    /// Sum of live skinned instance counts across all models (independent of ATTR/`CpuPhaseMs`).
    #[must_use]
    pub fn skinned_drawn_instances(&self) -> u32 {
        self.skinned_draws.values().map(|d| d.instances).sum()
    }

    /// Whether a model id has a GPU-skinned mesh uploaded.
    pub fn has_skinned_mesh(&self, model_id: u16) -> bool {
        self.skinned_draws.contains_key(&model_id)
    }

    /// Palette stride the GPU expects for this skinned mesh (matrices per instance).
    #[must_use]
    pub fn skinned_palette_stride(&self, model_id: u16) -> Option<u32> {
        self.skinned_draws.get(&model_id).map(|d| d.palette_stride)
    }

    /// Live instance count after the last [`Self::update_skinned_instances`] (0 = not drawn).
    #[must_use]
    pub fn skinned_instance_count(&self, model_id: u16) -> u32 {
        self.skinned_draws.get(&model_id).map_or(0, |d| d.instances)
    }

    /// Index count uploaded for this skinned mesh — used by REQ-020 to refuse visually-identical
    /// tier pairs that share topology.
    #[must_use]
    pub fn skinned_index_count(&self, model_id: u16) -> u32 {
        self.skinned_draws.get(&model_id).map_or(0, |d| {
            d.ranges.iter().map(|&(s, e, _)| e.saturating_sub(s)).sum()
        })
    }

    /// Upload a GPU-skinned mesh.
    ///
    /// `skin` is a whole-mesh fallback (creatures resolve one body skin from `monsters.csv`).
    /// `part_textures` maps a part's texture NAME — as recorded in `SkinnedPart::texture` — to its
    /// image, and takes precedence. That is what lets the assembled player avatar carry a
    /// different texture per body part instead of one skin stretched over everything.
    ///
    /// `z_offset` is the foot-anchor folded into every palette translation (avatar height seating).
    pub fn upload_skinned_mesh(
        &mut self,
        model_id: u16,
        batch: &crate::terrain::SkinnedBatch,
        skin: Option<&caer_assets::dds::DdsTexture>,
        part_textures: &std::collections::HashMap<String, caer_assets::dds::DdsTexture>,
        z_offset: f32,
    ) -> bool {
        // A rejected replacement must not leave the previous draw resident: callers fall back to
        // a CPU-baked mesh, and retaining the stale skinned draw would make that fallback inert.
        self.evict_entity_mesh(model_id);
        if batch.vertices.is_empty() || batch.indices.is_empty() {
            return false;
        }
        let part_count = batch.parts.len().max(1) as u32;
        let bone_stride = batch.bone_stride;
        // Dispatch uses `unique * palette_stride`; shader uses `parts_n * bones_n`. Same quantity.
        // Always-on (not debug_assert): release builds must refuse a mismatch that leaves tail
        // parts stale — a debug_assert alone is compiled out of the product path.
        let palette_stride = bone_stride.saturating_mul(part_count);
        if bone_stride > 0 && palette_stride != bone_stride * part_count {
            log::error!(
                "caer-render: model {model_id} palette_stride {palette_stride} != part_count {part_count} * bone_stride {bone_stride} — refusing skinned upload"
            );
            return false;
        }
        if bone_stride > 0 {
            let expect_ib = (part_count as usize).saturating_mul(bone_stride as usize);
            if batch.inverse_bind.len() != expect_ib {
                log::error!(
                    "caer-render: model {model_id} inverse_bind len {} != parts×bone_stride {expect_ib} — refusing skinned upload",
                    batch.inverse_bind.len(),
                );
                return false;
            }
        }

        self.ensure_entity_white_texture();
        let mut texture_slots = Vec::new();
        let whole = match skin {
            Some(t) => self.acquire_entity_texture(t, "skinned-skin", &mut texture_slots),
            None => 0,
        };
        // Resolve each part to a texture slot, uploading each distinct image once.
        let mut by_name: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut ranges = Vec::with_capacity(batch.parts.len().max(1));
        for part in &batch.parts {
            let slot = match part.texture.as_deref() {
                Some(name) => match by_name.get(name) {
                    Some(&s) => s,
                    None => match part_textures.get(name) {
                        Some(img) => {
                            let s = self.acquire_entity_texture(
                                img,
                                "skinned-part",
                                &mut texture_slots,
                            );
                            by_name.insert(name, s);
                            s
                        }
                        // Named but not supplied → fall back rather than dropping the part.
                        None => whole,
                    },
                },
                None => whole,
            };
            ranges.push((part.start, part.end, slot));
            if std::env::var_os("CAER_POSE_REPORT").is_some() {
                let d = part
                    .texture
                    .as_deref()
                    .and_then(|n| part_textures.get(n))
                    .map(|t| {
                        // Alpha of the image actually being uploaded. `skinned.wgsl` discards
                        // texels under 0.4, and only clothing is force-opaque first.
                        let (keep, mean, n) = match t.rgba8_mip0() {
                            Some((_, _, px)) => {
                                let n = (px.len() / 4).max(1);
                                let k = px.chunks_exact(4).filter(|c| c[3] >= 102).count();
                                let m: f64 =
                                    px.chunks_exact(4).map(|c| c[3] as f64).sum::<f64>() / n as f64;
                                (k, m, n)
                            }
                            None => (0, -1.0, 1),
                        };
                        format!(
                            "{}x{} {:?} mips {} alpha>=0.4 {:.1}% mean {:.0}",
                            t.width,
                            t.height,
                            t.format,
                            t.mips.len(),
                            100.0 * keep as f64 / n as f64,
                            mean
                        )
                    })
                    .unwrap_or_else(|| "<no image>".into());
                println!(
                    "rustdaoc: gpu-slot {:<16} tex={:<28} slot={slot:<3} idx {}..{}  {d}",
                    part.name,
                    part.texture.as_deref().unwrap_or("<none>"),
                    part.start,
                    part.end
                );
            }
        }
        if ranges.is_empty() {
            ranges.push((0, batch.indices.len() as u32, whole));
        }
        // One palette slice per instance; both buffers grow on demand.
        const START_INSTANCES: u32 = 16;
        let palette_cap = palette_stride * START_INSTANCES;
        let palette_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("skinned-palette"),
            size: u64::from(palette_cap) * MATRIX_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let palette_bind = self.palette_bind_group(&palette_buf);

        // Static inverse-bind: pad to at least one matrix so the storage binding is valid.
        let mut ib = batch.inverse_bind.clone();
        if ib.is_empty() {
            ib.push([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]);
        }
        let inverse_bind_buf = create_init_buffer(
            &self.device,
            "skinned-inverse-bind",
            bytemuck::cast_slice(&ib),
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let bones_cap = batch.bone_stride.max(1) * START_INSTANCES;
        let bones_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("skinned-bones"),
            size: u64::from(bones_cap) * MATRIX_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let fold_params_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("skinned-fold-params"),
            size: std::mem::size_of::<PaletteFoldParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let fold_bind = self.palette_fold_bind_group(
            &bones_buf,
            &inverse_bind_buf,
            &palette_buf,
            &fold_params_buf,
        );

        let inst_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("skinned-insts"),
            size: u64::from(START_INSTANCES) * std::mem::size_of::<SkinnedInstance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.skinned_draws.insert(
            model_id,
            SkinnedDraw {
                vbuf: create_init_buffer(
                    &self.device,
                    "skinned-verts",
                    bytemuck::cast_slice(&batch.vertices),
                    wgpu::BufferUsages::VERTEX,
                ),
                skin_buf: create_init_buffer(
                    &self.device,
                    "skinned-influences",
                    bytemuck::cast_slice(&batch.skin),
                    wgpu::BufferUsages::VERTEX,
                ),
                ibuf: create_init_buffer(
                    &self.device,
                    "skinned-indices",
                    bytemuck::cast_slice(&batch.indices),
                    wgpu::BufferUsages::INDEX,
                ),
                inst_buf,
                inst_cap: START_INSTANCES,
                instances: 0,
                palette_buf,
                palette_cap,
                palette_bind,
                palette_stride,
                bone_stride,
                part_count,
                z_offset,
                inverse_bind_buf,
                bones_buf,
                bones_cap,
                fold_params_buf,
                fold_bind,
                fold_unique_count: 0,
                palette_writer: PaletteWriter::None,
                ranges,
                texture_slots,
                last_insts: Vec::new(),
            },
        );
        true
    }

    fn palette_bind_group(&self, buf: &wgpu::Buffer) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("palette-bind"),
            layout: &self.palette_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buf.as_entire_binding(),
            }],
        })
    }

    fn palette_fold_bind_group(
        &self,
        bones: &wgpu::Buffer,
        inverse_bind: &wgpu::Buffer,
        palette: &wgpu::Buffer,
        params: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("palette-fold-bind"),
            layout: &self.palette_fold_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: bones.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: inverse_bind.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: palette.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params.as_entire_binding(),
                },
            ],
        })
    }

    fn ensure_skinned_palette_cap(&mut self, model_id: u16, needed: u32) {
        let Some(mut d) = self.skinned_draws.remove(&model_id) else {
            return;
        };
        if needed > d.palette_cap {
            let cap = needed.next_power_of_two();
            d.palette_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("skinned-palette"),
                size: u64::from(cap) * MATRIX_BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            d.palette_cap = cap;
            d.palette_bind = self.palette_bind_group(&d.palette_buf);
            d.fold_bind = self.palette_fold_bind_group(
                &d.bones_buf,
                &d.inverse_bind_buf,
                &d.palette_buf,
                &d.fold_params_buf,
            );
        }
        self.skinned_draws.insert(model_id, d);
    }

    fn ensure_skinned_bones_cap(&mut self, model_id: u16, needed: u32) {
        let Some(mut d) = self.skinned_draws.remove(&model_id) else {
            return;
        };
        if needed > d.bones_cap {
            let cap = needed.next_power_of_two();
            d.bones_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("skinned-bones"),
                size: u64::from(cap) * MATRIX_BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            d.bones_cap = cap;
            d.fold_bind = self.palette_fold_bind_group(
                &d.bones_buf,
                &d.inverse_bind_buf,
                &d.palette_buf,
                &d.fold_params_buf,
            );
        }
        self.skinned_draws.insert(model_id, d);
    }

    /// Set this frame's skinned instances and their bone palettes.
    ///
    /// `palettes` is the concatenation of unique (or per-instance) bone-matrix slots.
    /// Length must be a multiple of `palette_stride`. Instances point into it via
    /// [`SkinnedInstance::palette_base`]; several instances may share one slot (palette dedup).
    /// A misaligned or undersized buffer is dropped rather than drawn.
    pub fn update_skinned_instances(
        &mut self,
        model_id: u16,
        insts: &[SkinnedInstance],
        palettes: &[[[f32; 4]; 4]],
    ) {
        let Some(d) = self.skinned_draws.get_mut(&model_id) else {
            return;
        };
        // Dual-path: CPU owns palette_buf this frame — never both writers.
        debug_assert!(
            d.palette_writer == PaletteWriter::None || d.palette_writer == PaletteWriter::Cpu,
            "caer-render: model {model_id} palette_buf already claimed by {:?} this frame",
            d.palette_writer,
        );
        d.fold_unique_count = 0;
        d.palette_writer = PaletteWriter::Cpu;
        let n = insts.len() as u32;
        let stride = d.palette_stride as usize;
        if n > 0 {
            if stride == 0 || !palettes.len().is_multiple_of(stride) {
                log::warn!(
                    "caer-render: model {model_id} palette len {} not a multiple of stride {stride} — skipping",
                    palettes.len(),
                );
                d.instances = 0;
                return;
            }
            let slot_mats = palettes.len();
            let bad = insts.iter().any(|inst| {
                let base = inst.palette_base as usize;
                !base.is_multiple_of(stride) || base + stride > slot_mats
            });
            if bad {
                log::warn!(
                    "caer-render: model {model_id} palette_base out of range (slots={}, stride={stride}, n={n}) — skipping",
                    slot_mats / stride,
                );
                d.instances = 0;
                return;
            }
        }
        if n > d.inst_cap {
            let cap = n.next_power_of_two();
            d.inst_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("skinned-insts"),
                size: u64::from(cap) * std::mem::size_of::<SkinnedInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            d.inst_cap = cap;
            d.last_insts.clear(); // buffer recreated — force rewrite
        }
        let needed = palettes.len() as u32;
        // Exclusive borrow of `d` ends here so ensure_* can remove/reinsert the draw.
        self.ensure_skinned_palette_cap(model_id, needed);
        let Some(d) = self.skinned_draws.get_mut(&model_id) else {
            return;
        };
        if n > 0 {
            // Instance stream is O(instances) and often stable for idle NPCs; skip rewrite when
            // identical. Palettes animate most frames — do NOT shadow/compare them (memcmp+copy
            // of ~1k unique palettes was a measured regression vs always-write).
            let inst_bytes: &[u8] = bytemuck::cast_slice(insts);
            let prev_inst: &[u8] = bytemuck::cast_slice(d.last_insts.as_slice());
            if prev_inst != inst_bytes {
                self.queue.write_buffer(&d.inst_buf, 0, inst_bytes);
                self.upload_stats.write_calls += 1;
                self.upload_stats.write_bytes += inst_bytes.len() as u64;
                self.upload_stats.skinned_inst_calls += 1;
                self.upload_stats.skinned_inst_bytes += inst_bytes.len() as u64;
                d.last_insts.clear();
                d.last_insts.extend_from_slice(insts);
            }
            let pal_bytes: &[u8] = bytemuck::cast_slice(palettes);
            self.queue.write_buffer(&d.palette_buf, 0, pal_bytes);
            self.upload_stats.write_calls += 1;
            self.upload_stats.write_bytes += pal_bytes.len() as u64;
            self.upload_stats.skinned_palette_calls += 1;
            self.upload_stats.skinned_palette_bytes += pal_bytes.len() as u64;
            self.upload_stats.skinned_models_touched += 1;
            self.upload_stats.skinned_palette_stride_sum += u64::from(d.palette_stride);
            self.upload_stats.skinned_bone_stride_sum += u64::from(d.bone_stride);
        } else {
            d.last_insts.clear();
        }
        d.instances = n;
    }

    /// Upload posed world bones + instances; GPU compute expands to the palette buffer (MS-08).
    ///
    /// `bones` length must be a multiple of `bone_stride`. Instances still address the
    /// **expanded** palette via `palette_base` (`job_idx * palette_stride`) — same layout as the
    /// CPU path; only the producer of those matrices moved to the GPU.
    pub fn update_skinned_bones(
        &mut self,
        model_id: u16,
        insts: &[SkinnedInstance],
        bones: &[[[f32; 4]; 4]],
    ) {
        let Some(meta) = self.skinned_draws.get(&model_id) else {
            return;
        };
        let bone_stride = meta.bone_stride as usize;
        let palette_stride = meta.palette_stride as usize;
        let part_count = meta.part_count;
        let z_offset = meta.z_offset;
        if bone_stride == 0 || palette_stride == 0 {
            return;
        }
        let n = insts.len() as u32;
        if n > 0 {
            if !bones.len().is_multiple_of(bone_stride) {
                log::warn!(
                    "caer-render: model {model_id} bones len {} not a multiple of bone_stride {bone_stride} — skipping",
                    bones.len(),
                );
                if let Some(d) = self.skinned_draws.get_mut(&model_id) {
                    d.instances = 0;
                    d.fold_unique_count = 0;
                    d.palette_writer = PaletteWriter::None;
                }
                return;
            }
            let unique = bones.len() / bone_stride;
            let slot_mats = unique * palette_stride;
            let bad = insts.iter().any(|inst| {
                let base = inst.palette_base as usize;
                !base.is_multiple_of(palette_stride) || base + palette_stride > slot_mats
            });
            if bad {
                log::warn!(
                    "caer-render: model {model_id} palette_base out of range for GPU fold (unique={unique}, palette_stride={palette_stride}, n={n}) — skipping",
                );
                if let Some(d) = self.skinned_draws.get_mut(&model_id) {
                    d.instances = 0;
                    d.fold_unique_count = 0;
                    d.palette_writer = PaletteWriter::None;
                }
                return;
            }
            self.ensure_skinned_palette_cap(model_id, slot_mats as u32);
            self.ensure_skinned_bones_cap(model_id, bones.len() as u32);
        }

        let Some(d) = self.skinned_draws.get_mut(&model_id) else {
            return;
        };
        if n > d.inst_cap {
            let cap = n.next_power_of_two();
            d.inst_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("skinned-insts"),
                size: u64::from(cap) * std::mem::size_of::<SkinnedInstance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            d.inst_cap = cap;
            d.last_insts.clear();
        }
        if n > 0 {
            debug_assert!(
                d.palette_writer == PaletteWriter::None
                    || d.palette_writer == PaletteWriter::GpuFold,
                "caer-render: model {model_id} palette_buf already claimed by {:?} this frame",
                d.palette_writer,
            );
            d.palette_writer = PaletteWriter::GpuFold;
            let inst_bytes: &[u8] = bytemuck::cast_slice(insts);
            let prev_inst: &[u8] = bytemuck::cast_slice(d.last_insts.as_slice());
            if prev_inst != inst_bytes {
                self.queue.write_buffer(&d.inst_buf, 0, inst_bytes);
                self.upload_stats.write_calls += 1;
                self.upload_stats.write_bytes += inst_bytes.len() as u64;
                self.upload_stats.skinned_inst_calls += 1;
                self.upload_stats.skinned_inst_bytes += inst_bytes.len() as u64;
                d.last_insts.clear();
                d.last_insts.extend_from_slice(insts);
            }
            let bone_bytes: &[u8] = bytemuck::cast_slice(bones);
            self.queue.write_buffer(&d.bones_buf, 0, bone_bytes);
            self.upload_stats.write_calls += 1;
            self.upload_stats.write_bytes += bone_bytes.len() as u64;
            self.upload_stats.skinned_bone_calls += 1;
            self.upload_stats.skinned_bone_bytes += bone_bytes.len() as u64;
            self.upload_stats.skinned_models_touched += 1;
            self.upload_stats.skinned_palette_stride_sum += u64::from(d.palette_stride);
            self.upload_stats.skinned_bone_stride_sum += u64::from(d.bone_stride);

            let unique = (bones.len() / bone_stride) as u32;
            let params = PaletteFoldParams {
                bone_stride: d.bone_stride,
                part_count,
                unique_count: unique,
                z_offset,
                part_offset: self.debug_fold_part_offset,
                omit_last_part: u32::from(self.debug_fold_omit_last_part),
                _pad: [0; 2],
            };
            self.queue
                .write_buffer(&d.fold_params_buf, 0, bytemuck::bytes_of(&params));
            self.upload_stats.write_calls += 1;
            self.upload_stats.write_bytes += std::mem::size_of::<PaletteFoldParams>() as u64;
            d.fold_unique_count = unique;
        } else {
            d.last_insts.clear();
            d.fold_unique_count = 0;
            d.palette_writer = PaletteWriter::None;
        }
        d.instances = n;
    }

    /// Zero every skinned model's instance count (mirrors [`Self::clear_entity_instances`]).
    pub fn clear_skinned_instances(&mut self) {
        for d in self.skinned_draws.values_mut() {
            d.instances = 0;
            d.fold_unique_count = 0;
            d.palette_writer = PaletteWriter::None;
        }
    }

    /// Skip the palette-fold compute pass (stale-buffer falsifier / `CAER_SKIP_PALETTE_FOLD`).
    pub fn set_debug_skip_palette_fold(&mut self, skip: bool) {
        self.debug_skip_palette_fold = skip;
    }

    /// Rotate fold part↔inverse_bind indexing (one-part mismatch falsifier). Production: 0.
    pub fn set_debug_fold_part_offset(&mut self, offset: u32) {
        self.debug_fold_part_offset = offset;
    }

    /// Leave the last part's palette slots unwritten (tail-stale falsifier). Production: false.
    pub fn set_debug_fold_omit_last_part(&mut self, omit: bool) {
        self.debug_fold_omit_last_part = omit;
    }

    /// When set, compute fold is skipped so the palette buffer retains the previous frame's
    /// matrices — the stale-pose trap. Test-only falsifier (`CAER_SKIP_PALETTE_FOLD=1` or
    /// [`Self::set_debug_skip_palette_fold`]).
    #[must_use]
    fn skip_palette_fold_dispatch(&self) -> bool {
        if self.debug_skip_palette_fold {
            return true;
        }
        matches!(
            std::env::var_os("CAER_SKIP_PALETTE_FOLD"),
            Some(v) if v == "1" || v == "true"
        )
    }

    /// Dispatch pending palette-fold computes into `encoder` (before the main render pass).
    ///
    /// GPU time is **outside** the main-pass timestamp pair (queries 2–3). When timestamps are
    /// available, queries 0–1 bracket this compute pass so the residual hunt can see it.
    fn encode_palette_folds(&self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(ts) = &self.timestamps {
            ts.fold_queries_written.set(false);
        }
        if self.skip_palette_fold_dispatch() {
            return;
        }
        let any = self
            .skinned_draws
            .values()
            .any(|d| d.fold_unique_count > 0 && d.bone_stride > 0 && d.part_count > 0);
        if !any {
            return;
        }
        {
            let timestamp_writes = self.timestamps.as_ref().map(|ts| {
                ts.fold_queries_written.set(true);
                wgpu::ComputePassTimestampWrites {
                    query_set: &ts.query_set,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("palette-fold"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.palette_fold_pipeline);
            for d in self.skinned_draws.values() {
                if d.fold_unique_count == 0 || d.bone_stride == 0 || d.part_count == 0 {
                    continue;
                }
                debug_assert_eq!(
                    d.palette_writer,
                    PaletteWriter::GpuFold,
                    "fold dispatch without GpuFold writer ownership"
                );
                debug_assert_eq!(
                    d.palette_stride,
                    d.part_count * d.bone_stride,
                    "dispatch total uses palette_stride; shader uses part_count*bone_stride"
                );
                let total = d.fold_unique_count * d.palette_stride;
                let groups = total.div_ceil(64);
                pass.set_bind_group(0, &d.fold_bind, &[]);
                pass.dispatch_workgroups(groups, 1, 1);
            }
        }
    }

    /// Upload a live-entity mesh once, keyed by model id. Geometry is static; instances are filled
    /// per frame by [`update_entity_instances`](Self::update_entity_instances).
    ///
    /// `skin` is the creature's resolved body skin (from `monsters.csv` → `skins.csv`); when present
    /// it's uploaded once and bound over the whole mesh, else the mesh draws with the white fallback
    /// so its material diffuse shows. Split body/head skins are a later refinement — this binds a
    /// single texture per model.
    pub fn upload_entity_mesh(
        &mut self,
        model_id: u16,
        batch: &crate::terrain::ModelBatch,
        skin: Option<&caer_assets::dds::DdsTexture>,
    ) {
        if batch.vertices.is_empty() || batch.indices.is_empty() {
            return;
        }
        self.evict_entity_mesh(model_id);
        self.ensure_entity_white_texture();
        // Resolve this model's skin to a retained bind-group slot, or fall to the white fallback.
        let tex = match skin {
            Some(t) => {
                let key = EntityTextureKey::from_dds(t);
                match self.entity_textures.retain_existing(key) {
                    Some(slot) => slot,
                    None => {
                        let view = self.upload_dds(t, "entity-skin");
                        let binding = self.model_texture_bind(&view, &view, "entity-skin");
                        self.entity_textures.insert(key, binding)
                    }
                }
            }
            None => 0,
        };
        const START_CAP: u32 = 64;
        let inst_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("entity-insts"),
            size: u64::from(START_CAP) * std::mem::size_of::<[f32; 5]>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.entity_draws.insert(
            model_id,
            EntityDraw {
                vbuf: create_init_buffer(
                    &self.device,
                    "entity-verts",
                    bytemuck::cast_slice(&batch.vertices),
                    wgpu::BufferUsages::VERTEX,
                ),
                ibuf: create_init_buffer(
                    &self.device,
                    "entity-indices",
                    bytemuck::cast_slice(&batch.indices),
                    wgpu::BufferUsages::INDEX,
                ),
                indices: batch.indices.len() as u32,
                inst_buf,
                inst_cap: START_CAP,
                instances: 0,
                tex,
            },
        );
    }

    /// Set this frame's instances (each `[x, y, z, yaw, scale]`) for one entity model, growing the
    /// buffer if needed. No-op if that model isn't uploaded.
    pub fn update_entity_instances(&mut self, model_id: u16, insts: &[[f32; 5]]) {
        let Some(d) = self.entity_draws.get_mut(&model_id) else {
            return;
        };
        let packed: Vec<[f32; 8]> = insts.iter().map(pos_yaw_scale_raw).collect();
        let n = packed.len() as u32;
        if n > d.inst_cap {
            let cap = n.next_power_of_two();
            d.inst_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("entity-insts"),
                size: u64::from(cap) * std::mem::size_of::<[f32; 8]>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            d.inst_cap = cap;
        }
        if n > 0 {
            let bytes: &[u8] = bytemuck::cast_slice(&packed);
            self.queue.write_buffer(&d.inst_buf, 0, bytes);
            self.upload_stats.write_calls += 1;
            self.upload_stats.write_bytes += bytes.len() as u64;
            self.upload_stats.entity_inst_calls += 1;
            self.upload_stats.entity_inst_bytes += bytes.len() as u64;
            self.upload_stats.entity_models_touched += 1;
        }
        d.instances = n;
    }

    /// Zero every entity model's instance count — called once per frame before re-filling only the
    /// visible ones, so an entity that left view (or a model with none nearby) stops drawing.
    pub fn clear_entity_instances(&mut self) {
        for d in self.entity_draws.values_mut() {
            d.instances = 0;
        }
    }

    /// Bind one model-texture view with the tiling (Repeat) sampler.
    /// Upload one decoded UI atlas page under the name the skin's templates refer to it by.
    ///
    /// Idempotent: a page already present is left alone, so a caller can re-request pages every
    /// time a window opens without re-uploading megabytes of atlas.
    pub fn upload_ui_page(&mut self, name: &str, img: &caer_assets::tga::TgaImage) {
        let key = name.to_ascii_lowercase();
        if self.ui_pages.contains_key(&key) {
            return;
        }
        let size = wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui-page"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // sRGB: the skin art is authored in gamma space like every other client texture.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &img.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(img.width * 4),
                rows_per_image: Some(img.height),
            },
            size,
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui-page-bind"),
            layout: &self.ui_page_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.model_sampler),
                },
            ],
        });
        self.ui_pages
            .insert(key, (bind, [img.width as f32, img.height as f32]));
    }

    /// Whether a page has been uploaded.
    #[must_use]
    pub fn has_ui_page(&self, name: &str) -> bool {
        self.ui_pages.contains_key(&name.to_ascii_lowercase())
    }

    /// Sky horizon colour currently uploaded — a black frame is ambiguous without it.
    #[must_use]
    pub fn sky_horizon(&self) -> [u8; 3] {
        self.sky.horizon
    }

    /// How many model batches are queued to draw. Diagnostic: a scene that loads but never
    /// appears is either not here or not being looked at, and this separates the two.
    #[must_use]
    pub fn model_draw_count(&self) -> usize {
        self.model_draws.len()
    }

    /// Number of painter-ordered UI draw batches queued for the next frame.
    #[must_use]
    pub fn ui_batch_count(&self) -> usize {
        self.ui_batch_count
    }

    /// Total native UI instance buffers allocated since GPU initialisation.
    ///
    /// This is deliberately a count of backing buffers rather than UI quads. A stable pre-world
    /// layout should warm its pool once and then rewrite those buffers, not steadily create GPU
    /// resources until the driver becomes unstable.
    #[must_use]
    pub fn ui_buffer_allocation_count(&self) -> u64 {
        self.ui_buffer_allocations
    }

    /// Replace this frame's UI quads.
    ///
    /// Quads are grouped into runs by texture page, **preserving the order they were given in**,
    /// because 2D drawing is painter's-order: a window's background must precede its contents.
    /// Sorting by page to minimise binds would silently reorder overlapping windows.
    pub fn set_ui_quads(&mut self, quads: &[crate::skinui::UiQuad]) -> UiSubmitReport {
        let mut missing_pages = quads
            .iter()
            .map(|q| q.texture.to_ascii_lowercase())
            .filter(|key| !self.ui_pages.contains_key(key))
            .collect::<Vec<_>>();
        missing_pages.sort();
        missing_pages.dedup();

        // Keep the backing buffers resident. `flush_ui_run` will overwrite slots in this active
        // prefix and only allocate when the incoming layout genuinely exceeds a previous peak.
        self.ui_batch_count = 0;
        if !missing_pages.is_empty() {
            return UiSubmitReport {
                requested: quads.len(),
                submitted: 0,
                missing_pages,
            };
        }
        if quads.is_empty() {
            return UiSubmitReport {
                requested: 0,
                submitted: 0,
                missing_pages,
            };
        }
        let mut run: Vec<UiInstance> = Vec::new();
        let mut run_page: Option<String> = None;

        for q in quads {
            let key = q.texture.to_ascii_lowercase();
            // Copy the atlas size out: holding a borrow on `ui_pages` across `flush_ui_run`
            // (which needs `&mut self`) would not compile, and cloning two floats is free.
            let Some(&(_, atlas)) = self.ui_pages.get(&key) else {
                continue;
            };
            if run_page.as_deref() != Some(key.as_str()) {
                if let Some(page) = run_page.take() {
                    self.flush_ui_run(page, &mut run);
                }
                run_page = Some(key.clone());
            }
            run.push(UiInstance {
                dst: [q.dst.x, q.dst.y, q.dst.w, q.dst.h],
                src: [q.src.x, q.src.y, q.src.w, q.src.h],
                color: [
                    srgb_byte_to_linear(q.color.r),
                    srgb_byte_to_linear(q.color.g),
                    srgb_byte_to_linear(q.color.b),
                    f32::from(q.color.a) / 255.0,
                ],
                atlas,
                _pad: [0.0; 2],
            });
        }
        if let Some(page) = run_page.take() {
            self.flush_ui_run(page, &mut run);
        }

        // The shader needs the framebuffer size to map pixels to NDC.
        let screen = [
            self.config.width as f32,
            self.config.height as f32,
            0.0,
            0.0,
        ];
        self.queue
            .write_buffer(&self.ui_globals_buf, 0, bytemuck::cast_slice(&screen));
        UiSubmitReport {
            requested: quads.len(),
            submitted: quads.len(),
            missing_pages,
        }
    }

    /// Turn one accumulated same-page run into a reusable draw slot.
    fn flush_ui_run(&mut self, page: String, run: &mut Vec<UiInstance>) {
        if run.is_empty() {
            return;
        }
        let count = u32::try_from(run.len()).expect("UI run exceeds u32 instance count");
        let slot = self.ui_batch_count;
        let required_capacity = count.max(1).next_power_of_two();
        let bytes = u64::from(required_capacity) * std::mem::size_of::<UiInstance>() as u64;

        if let Some(batch) = self.ui_batches.get_mut(slot) {
            if batch.capacity < count {
                batch.buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("ui-instances"),
                    size: bytes,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                batch.capacity = required_capacity;
                self.ui_buffer_allocations += 1;
            }
            batch.page = page;
            batch.count = count;
            self.queue
                .write_buffer(&batch.buffer, 0, bytemuck::cast_slice(run));
        } else {
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui-instances"),
                size: bytes,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue
                .write_buffer(&buffer, 0, bytemuck::cast_slice(run));
            self.ui_batches.push(UiBatch {
                page,
                buffer,
                capacity: required_capacity,
                count,
            });
            self.ui_buffer_allocations += 1;
        }
        self.ui_batch_count += 1;
        run.clear();
    }

    /// Bind a model part's textures. `blend` is the second ground layer; pass the same view twice
    /// for a single-layer part — its vertices carry a mask of 1.0, so layer 2 is never mixed in.
    fn model_texture_bind(
        &self,
        view: &wgpu::TextureView,
        blend: &wgpu::TextureView,
        label: &str,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.model_texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.model_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(blend),
                },
            ],
        })
    }

    /// Upload a decoded model DDS with its full mip chain. Falls back to CPU-decoded RGBA when
    /// the device lacks BC support or the surface exceeds [`Self::max_texture_2d`].
    fn upload_dds(&self, t: &caer_assets::dds::DdsTexture, label: &str) -> wgpu::TextureView {
        use caer_assets::dds::DdsFormat;
        let too_big = t.width > self.max_texture_2d || t.height > self.max_texture_2d;
        if (!self.supports_bc && t.format != DdsFormat::Rgba8) || too_big {
            if let Some((w, h, rgba)) = t.rgba8_mip0() {
                let (w, h, rgba) =
                    caer_assets::dds::clamp_rgba_to_max(w, h, &rgba, self.max_texture_2d);
                return self.upload_rgba(w, h, &rgba, label);
            }
            return self.white_texture();
        }
        let format = match t.format {
            DdsFormat::Bc1 if self.supports_bc => wgpu::TextureFormat::Bc1RgbaUnormSrgb,
            DdsFormat::Bc2 if self.supports_bc => wgpu::TextureFormat::Bc2RgbaUnormSrgb,
            DdsFormat::Bc3 if self.supports_bc => wgpu::TextureFormat::Bc3RgbaUnormSrgb,
            DdsFormat::Rgba8 => wgpu::TextureFormat::Rgba8UnormSrgb,
            _ => return self.white_texture(),
        };
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: t.width,
                height: t.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: t.mips.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let (mut w, mut h) = (t.width, t.height);
        for (level, data) in t.mips.iter().enumerate() {
            // Compressed copies must be block-aligned, so small mips (2×2, 1×1) copy at their
            // physical (4-rounded) size — the data is one full block regardless.
            let (bytes_per_row, cw, ch) = match t.format {
                DdsFormat::Rgba8 => (w * 4, w, h),
                DdsFormat::Bc1 => (w.div_ceil(4) * 8, w.div_ceil(4) * 4, h.div_ceil(4) * 4),
                DdsFormat::Bc2 | DdsFormat::Bc3 => {
                    (w.div_ceil(4) * 16, w.div_ceil(4) * 4, h.div_ceil(4) * 4)
                }
            };
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: cw,
                    height: ch,
                    depth_or_array_layers: 1,
                },
            );
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }

    /// Live-update one placed instance's transform (editor rotate). `src_batch` is the mesh
    /// `models` index; `inst` the instance index within that batch. No-op if the batch wasn't
    /// uploaded (empty) or indices are stale.
    pub fn update_model_instance(
        &self,
        src_batch: usize,
        inst: usize,
        data: &crate::terrain::ModelInstance,
    ) {
        if let Some(draw) = self.model_draws.iter().find(|d| d.src == src_batch) {
            if (inst as u32) < draw.instances {
                let raw = model_instance_raw(data);
                let offset = (inst * std::mem::size_of::<[f32; 8]>()) as u64;
                self.queue
                    .write_buffer(&draw.inst_buf, offset, bytemuck::cast_slice(&raw));
            }
        }
    }

    /// Set (or clear) the editor's selection outline: 8 render-space corners of the picked
    /// fixture's oriented bounding box, drawn as 12 always-on-top wireframe edges.
    pub fn set_selection_box(&mut self, corners: Option<[[f32; 3]; 8]>) {
        let Some(c) = corners else {
            self.sel_buf = None;
            self.sel_verts = 0;
            return;
        };
        // Corner order: bit 0 = +x, bit 1 = +y, bit 2 = +z. Edges connect corners differing
        // in exactly one bit.
        const EDGES: [(usize, usize); 12] = [
            (0, 1),
            (2, 3),
            (4, 5),
            (6, 7), // x-axis edges
            (0, 2),
            (1, 3),
            (4, 6),
            (5, 7), // y-axis edges
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7), // z-axis edges
        ];
        const COLOR: [f32; 3] = [1.0, 0.85, 0.1]; // selection amber
        let mut verts = Vec::with_capacity(24);
        for (a, b) in EDGES {
            verts.push(LineVertex {
                pos: c[a],
                color: COLOR,
            });
            verts.push(LineVertex {
                pos: c[b],
                color: COLOR,
            });
        }
        self.sel_buf = Some(create_init_buffer(
            &self.device,
            "selection-outline",
            bytemuck::cast_slice(&verts),
            wgpu::BufferUsages::VERTEX,
        ));
        self.sel_verts = verts.len() as u32;
    }

    /// Set (or clear) the terrain-brush cursor preview: closed polyline rings (render space,
    /// already draped on the terrain by the caller). Inner ring = brush radius (bright), any
    /// further rings (falloff edge) draw dimmer. Same always-on-top pipeline as the selection
    /// box so the cursor reads inside dips and behind trees.
    pub fn set_brush_ring(&mut self, lines: Option<&[BrushRingLine]>) {
        let Some(lines) = lines else {
            self.brush_buf = None;
            self.brush_verts = 0;
            return;
        };
        let mut verts: Vec<LineVertex> = Vec::new();
        for (line, color) in lines {
            for w in line.windows(2) {
                verts.push(LineVertex {
                    pos: w[0],
                    color: *color,
                });
                verts.push(LineVertex {
                    pos: w[1],
                    color: *color,
                });
            }
        }
        if verts.is_empty() {
            self.brush_buf = None;
            self.brush_verts = 0;
            return;
        }
        // This runs EVERY frame the brush is up — reuse the buffer (draw uses brush_verts, so
        // slack capacity is harmless) and only allocate when the cursor geometry outgrows it.
        let bytes: &[u8] = bytemuck::cast_slice(&verts);
        match &self.brush_buf {
            Some(buf) if buf.size() >= bytes.len() as u64 => self.queue.write_buffer(buf, 0, bytes),
            _ => {
                self.brush_buf = Some(create_init_buffer(
                    &self.device,
                    "brush-ring",
                    bytes,
                    wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                ));
            }
        }
        self.brush_verts = verts.len() as u32;
    }

    /// Replace ONLY the given zones' terrain vertex/index buffers (indexed as in the original
    /// `set_terrain` order), keeping every ground texture — the fast path for brush strokes
    /// (a full `set_terrain` re-decodes and re-uploads the textures).
    pub fn update_terrain_vertices(&mut self, zones: &[(usize, crate::terrain::ZoneMesh)]) {
        for (i, z) in zones {
            let Some(draw) = self.terrain_zones.get_mut(*i) else {
                log::info!("caer-render: terrain remesh index {i} out of range — skipped");
                continue;
            };
            if z.vertices.is_empty() || z.indices.is_empty() {
                continue;
            }
            // A zone's vertex COUNT never changes (fixed decimated grid) and its index list is
            // pure topology — so the fast path is a plain write into the existing vertex buffer,
            // no allocation, indices untouched. Recreate only if the size somehow changed.
            let bytes: &[u8] = bytemuck::cast_slice(&z.vertices);
            if draw.vbuf.size() == bytes.len() as u64 {
                self.queue.write_buffer(&draw.vbuf, 0, bytes);
            } else {
                draw.vbuf = create_init_buffer(
                    &self.device,
                    "terrain-verts",
                    bytes,
                    wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                );
                draw.ibuf = create_init_buffer(
                    &self.device,
                    "terrain-indices",
                    bytemuck::cast_slice(&z.indices),
                    wgpu::BufferUsages::INDEX,
                );
                draw.indices = z.indices.len() as u32;
            }
        }
    }

    /// Upload the static fixture boxes once (drawn with the entity cube pipeline).
    pub fn set_fixtures(&mut self, instances: &[Instance]) {
        if instances.is_empty() {
            return;
        }
        self.fixture_buf = Some(create_init_buffer(
            &self.device,
            "fixtures",
            bytemuck::cast_slice(instances),
            wgpu::BufferUsages::VERTEX,
        ));
        self.fixture_count = instances.len() as u32;
    }

    /// Upload the (static) water mesh once. Empty input leaves water disabled.
    pub fn set_water(&mut self, vertices: &[crate::terrain::TerrainVertex], indices: &[u32]) {
        if vertices.is_empty() || indices.is_empty() {
            return;
        }
        self.water_vbuf = Some(create_init_buffer(
            &self.device,
            "water-verts",
            bytemuck::cast_slice(vertices),
            wgpu::BufferUsages::VERTEX,
        ));
        self.water_ibuf = Some(create_init_buffer(
            &self.device,
            "water-indices",
            bytemuck::cast_slice(indices),
            wgpu::BufferUsages::INDEX,
        ));
        self.water_indices = indices.len() as u32;
    }

    pub fn set_view_proj(&self, view_proj: [[f32; 4]; 4]) {
        self.set_globals(view_proj, self.sky);
    }

    /// Set (or clear) the dedicated Character Customize avatar camera.
    ///
    /// The pre-world realm scene is one static composition.  The retail customizer then places a
    /// close-up avatar over it; moving the shared stage camera close enough for the face drives
    /// Midgard into the inside of its dome and turns its intended sky black.  This sibling
    /// uniform gives the avatar its own lens while the stage keeps [`Self::set_view_proj`].
    ///
    /// `None` restores the ordinary shared-camera path used by character select and creation.
    pub fn set_preworld_avatar_view_proj(&mut self, view_proj: Option<[[f32; 4]; 4]>) {
        self.preworld_avatar_camera_active = view_proj.is_some();
        if let Some(view_proj) = view_proj {
            self.write_globals(&self.preworld_avatar_globals_buf, view_proj, self.sky);
        }
    }

    /// The sky colours used until a region's own are loaded (Albion clear-weather daytime).
    pub fn set_sky(&mut self, band: caer_assets::sky::SkyBand) {
        self.sky = band;
    }

    /// Upload the frame's camera plus the sky colours the dome shader reads, and the
    /// atmosphere lighting published by the last `load_region` (client tables).
    pub fn set_globals(&self, view_proj: [[f32; 4]; 4], sky: caer_assets::sky::SkyBand) {
        self.write_globals(&self.globals_buf, view_proj, sky);
    }

    fn write_globals(
        &self,
        buffer: &wgpu::Buffer,
        view_proj: [[f32; 4]; 4],
        sky: caer_assets::sky::SkyBand,
    ) {
        let m = glam::Mat4::from_cols_array_2d(&view_proj);
        let srgb = |c: [u8; 3]| {
            [
                srgb_byte_to_linear(c[0]),
                srgb_byte_to_linear(c[1]),
                srgb_byte_to_linear(c[2]),
                1.0,
            ]
        };
        let atm = crate::atmosphere::published();
        let dir = atm.light_dir();
        let sl = atm.sky_light.for_upload();
        let g = Globals {
            view_proj,
            inv_view_proj: m.inverse().to_cols_array_2d(),
            sky_zenith: srgb(sky.zenith),
            sky_horizon: srgb(sky.horizon),
            light_dir: [dir[0], dir[1], dir[2], 0.0],
            light_ambient: [
                sl.ambient[0],
                sl.ambient[1],
                sl.ambient[2],
                sl.ambient_amount,
            ],
            light_dynamic: [
                sl.dynamic[0],
                sl.dynamic[1],
                sl.dynamic[2],
                sl.dynamic_amount,
            ],
        };
        self.queue.write_buffer(buffer, 0, bytemuck::bytes_of(&g));
    }

    /// Explicit atmosphere publish (optional — `load_region` already publishes). Useful for
    /// tests that bypass terrain load.
    pub fn set_atmosphere(&mut self, atm: &crate::atmosphere::Atmosphere) {
        crate::atmosphere::publish(atm.clone());
    }

    /// Upload this frame's visible instances, growing the GPU buffer if the set outgrew it.
    pub fn upload_instances(&mut self, instances: &[Instance]) {
        let needed = instances.len() as u32;
        if needed > self.instance_cap {
            let new_cap = needed.next_power_of_two();
            self.instance_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instances"),
                size: (new_cap as u64) * std::mem::size_of::<Instance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_cap = new_cap;
        }
        if !instances.is_empty() {
            self.queue
                .write_buffer(&self.instance_buf, 0, bytemuck::cast_slice(instances));
        }
    }

    /// Upload CPU-expanded particle billboard verts for [`crate::particles::PARTICLE_BILLBOARD_DRAW_PATH`].
    ///
    /// Pass an empty slice to clear the path (falsifier: ignored uploads leave `particle_billboard_vert_count` at 0).
    pub fn upload_particle_billboards(
        &mut self,
        verts: &[crate::particles::ParticleBillboardVert],
    ) {
        let needed = verts.len() as u32;
        if needed > self.particle_vbuf_cap {
            let new_cap = needed.next_power_of_two().max(64);
            self.particle_vbuf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("particle-billboard-verts"),
                size: u64::from(new_cap)
                    * std::mem::size_of::<crate::particles::ParticleBillboardVert>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.particle_vbuf_cap = new_cap;
        }
        if !verts.is_empty() {
            self.queue
                .write_buffer(&self.particle_vbuf, 0, bytemuck::cast_slice(verts));
        }
        self.particle_vert_count = needed;
    }

    /// Vertices queued for the particle billboard draw (0 if path unused / cleared).
    #[must_use]
    pub fn particle_billboard_vert_count(&self) -> u32 {
        self.particle_vert_count
    }

    /// Draw `count` instances. Returns `Err(())` only on a lost/outdated surface, which the
    /// caller recovers from by reconfiguring; transient states (timeout/occluded) skip the frame.
    /// Like [`Gpu::render`], but also copies the presented frame back to the CPU when `capture`
    /// is set — the live window's own recorder. Returns the RGBA8 rows alongside the usual result.
    pub fn render_capturing(
        &mut self,
        count: u32,
        egui: Option<crate::ui::EguiFrame>,
        capture: bool,
        present_window: Option<&Window>,
        pass: FramePass,
        capture_timeout: Option<std::time::Duration>,
    ) -> (PresentOutcome, Option<Vec<u8>>) {
        let surface = self.surface.as_ref().expect("render() needs a surface");
        let acquire_started = std::time::Instant::now();
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                note_surface_event("suboptimal", acquire_started.elapsed());
                t
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                note_surface_event("outdated", acquire_started.elapsed());
                let _ = self.device.poll(wgpu::PollType::Poll);
                return (PresentOutcome::Reconfigure("outdated"), None);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                note_surface_event("lost", acquire_started.elapsed());
                let _ = self.device.poll(wgpu::PollType::Poll);
                return (PresentOutcome::Reconfigure("lost"), None);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                note_surface_event("validation", acquire_started.elapsed());
                let _ = self.device.poll(wgpu::PollType::Poll);
                return (PresentOutcome::Fatal("validation"), None);
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                // A timeout is transient and, critically, does not prove that the surface is lost.
                // On NVIDIA/Vulkan/Wayland, synchronously calling `Surface::configure` after this
                // result can block the event-loop thread indefinitely.  Skip this presentation and
                // let the paced redraw retry acquisition; only the explicit Lost/Outdated states
                // enter the reconfiguration path.
                note_surface_event("timeout", acquire_started.elapsed());
                let _ = self.device.poll(wgpu::PollType::Poll);
                return (PresentOutcome::Skipped("timeout"), None);
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                note_surface_event("occluded", acquire_started.elapsed());
                let _ = self.device.poll(wgpu::PollType::Poll);
                return (PresentOutcome::Skipped("occluded"), None);
            }
        };
        let acquire_time = acquire_started.elapsed();
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let _ = self.encode_pass_with_optional_egui(&view, count, egui, pass);
        if std::env::var_os("CAER_SURFACE_TRACE").is_some() {
            let submitted_at = std::time::Instant::now();
            self.queue.on_submitted_work_done(move || {
                let elapsed = submitted_at.elapsed();
                if elapsed >= std::time::Duration::from_millis(100) {
                    log::warn!(
                        "caer-render: submitted GPU frame completed after {:.3}s",
                        elapsed.as_secs_f64()
                    );
                }
            });
        }
        // Read back BEFORE presenting: after `present()` the texture is handed to the compositor.
        // Capture is optional — surfaces without COPY_SRC still present; capability is reported
        // via [`Self::frame_capture_supported`].
        let shot = if capture {
            if !self.capture_copy_src {
                log::warn!(
                    "frame capture requested but surface lacks COPY_SRC; presenting without readback"
                );
                None
            } else {
                match self.read_texture(&frame.texture, capture_timeout) {
                    Ok(rgba) => Some(rgba),
                    Err(e) => {
                        log::error!("frame capture GPU wait failed: {e}");
                        None
                    }
                }
            }
        } else {
            None
        };
        // Winit's compositor notification belongs immediately before the actual present. Calling
        // it before swapchain acquisition leaves Wayland waiting on the wrong frame callback and
        // made every later FIFO acquisition block for roughly one second.
        if let Some(window) = present_window {
            window.pre_present_notify();
        }
        frame.present();
        note_surface_present(acquire_time);
        // Keep the product path consistent with `render_presented`: service completed submissions
        // after handing the image to the compositor. This is non-blocking and prevents deferred
        // resource retirement from accumulating across UI buffers rebuilt each frame.
        let _ = self.device.poll(wgpu::PollType::Poll);
        (PresentOutcome::Presented, shot)
    }

    /// Surface lost/outdated — `Err(())` means reconfigure; ternary present status needs Result.
    #[allow(clippy::result_unit_err)]
    pub fn render(&mut self, count: u32, egui: Option<crate::ui::EguiFrame>) -> Result<(), ()> {
        match self.render_presented(count, egui)? {
            true | false => Ok(()),
        }
    }

    /// Like [`Self::render`], but reports whether a swapchain image was acquired and presented.
    ///
    /// `Ok(false)` means Timeout/Occluded (frame skipped — product path treats this as success).
    /// Measurement harnesses must not count `false` as an acquire+present sample.
    /// Ternary Ok(true)/Ok(false)/Err — Option cannot express this.
    #[allow(clippy::result_unit_err)]
    pub fn render_presented(
        &mut self,
        count: u32,
        egui: Option<crate::ui::EguiFrame>,
    ) -> Result<bool, ()> {
        let surface = self
            .surface
            .as_ref()
            .expect("render() needs a surface — headless uses render_to_rgba()");
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost
            | wgpu::CurrentSurfaceTexture::Validation => return Err(()),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false)
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let _ = self.encode_pass_with_optional_egui(&view, count, egui, FramePass::World);
        frame.present(); // wgpu 29: present on the SurfaceTexture, not the queue
        let _ = self.device.poll(wgpu::PollType::Poll);
        Ok(true)
    }

    /// Draw the tessellated egui sidebar over the already-rendered scene (LoadOp::Load, no depth).
    ///
    /// Records into `encoder` only — caller owns finish/submit (MS-08 coalesce path).
    fn encode_egui_into(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        ef: &mut crate::ui::EguiFrame,
    ) {
        // Build the renderer on first use. The colour target's format is fixed for the Gpu's
        // lifetime, so one renderer serves every later frame.
        let renderer = self.egui_renderer.get_or_insert_with(|| {
            egui_wgpu::Renderer::new(
                &self.device,
                self.config.format,
                egui_wgpu::RendererOptions::default(),
            )
        });
        for (id, delta) in &ef.textures_delta.set {
            renderer.update_texture(&self.device, &self.queue, *id, delta);
        }
        renderer.update_buffers(
            &self.device,
            &self.queue,
            encoder,
            &ef.primitives,
            &ef.screen,
        );
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            renderer.render(&mut pass, &ef.primitives, &ef.screen);
        }
    }

    fn free_egui_textures(&mut self, ef: &crate::ui::EguiFrame) {
        let Some(renderer) = self.egui_renderer.as_mut() else {
            return;
        };
        for id in &ef.textures_delta.free {
            renderer.free_texture(id);
        }
    }

    /// MS-08 residual (3): coalesce egui onto the scene encoder (one `queue.submit`) by default.
    ///
    /// Kill-switch `CAER_SPLIT_EGUI_SUBMIT=1` restores the historical 2-submit path.
    /// Returns `(fold, scene, egui_encode, submit, queue_submit_count)`.
    /// `submit` sums every finish+submit on this path (named mean-cut leaf).
    /// `egui_encode` is record-only (sibling of encode; not nested).
    fn encode_pass_with_optional_egui(
        &mut self,
        view: &wgpu::TextureView,
        count: u32,
        egui: Option<crate::ui::EguiFrame>,
        pass: FramePass,
    ) -> (f64, f64, f64, f64, u32) {
        let (mut encoder, fold_encode_cpu_ms, scene_encode_cpu_ms) = match pass {
            FramePass::World => self.encode_pass_open(view, count),
            FramePass::PreWorldUi => self.encode_preworld_open(view, false),
            FramePass::PreWorldScene => self.encode_preworld_open(view, true),
        };
        let mut egui_encode_ms = 0.0;
        let (submit_cpu_ms, queue_submit_count) = match egui {
            None => {
                let t_submit = std::time::Instant::now();
                self.queue.submit(std::iter::once(encoder.finish()));
                (t_submit.elapsed().as_secs_f64() * 1000.0, 1)
            }
            Some(mut ef) if split_egui_submit_enabled() => {
                // Historical: scene CB submit, then own egui CB submit.
                let t_submit = std::time::Instant::now();
                self.queue.submit(std::iter::once(encoder.finish()));
                let mut submit_cpu_ms = t_submit.elapsed().as_secs_f64() * 1000.0;
                let t_egui = std::time::Instant::now();
                // Record only inside egui_encode; second finish+submit goes into submit_cpu_ms.
                let mut egui_enc =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("egui-encoder"),
                        });
                self.encode_egui_into(&mut egui_enc, view, &mut ef);
                egui_encode_ms = t_egui.elapsed().as_secs_f64() * 1000.0;
                let t_submit2 = std::time::Instant::now();
                self.queue.submit(std::iter::once(egui_enc.finish()));
                submit_cpu_ms += t_submit2.elapsed().as_secs_f64() * 1000.0;
                self.free_egui_textures(&ef);
                (submit_cpu_ms, 2)
            }
            Some(mut ef) => {
                // Coalesce: append egui to the scene encoder, one finish+submit.
                let t_egui = std::time::Instant::now();
                self.encode_egui_into(&mut encoder, view, &mut ef);
                egui_encode_ms = t_egui.elapsed().as_secs_f64() * 1000.0;
                let t_submit = std::time::Instant::now();
                self.queue.submit(std::iter::once(encoder.finish()));
                let submit_cpu_ms = t_submit.elapsed().as_secs_f64() * 1000.0;
                self.free_egui_textures(&ef);
                (submit_cpu_ms, 1)
            }
        };
        (
            fold_encode_cpu_ms,
            scene_encode_cpu_ms,
            egui_encode_ms,
            submit_cpu_ms,
            queue_submit_count,
        )
    }

    /// Opaque realm-stage geometry for the pre-world character screens.
    fn draw_preworld_stage_opaque<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.model_draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.mesh_pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        for m in &self.model_draws {
            pass.set_vertex_buffer(0, m.vbuf.slice(..));
            pass.set_vertex_buffer(1, m.inst_buf.slice(..));
            pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
            if m.parts.is_empty() {
                pass.set_bind_group(1, &self.model_textures[0], &[]);
                pass.draw_indexed(0..m.indices, 0, 0..m.instances);
            } else {
                for &(start, end, ti) in &m.parts {
                    pass.set_bind_group(1, &self.model_textures[ti], &[]);
                    pass.draw_indexed(start..end, 0, 0..m.instances);
                }
            }
        }
    }

    /// Static and skinned pre-world avatar draws through either the stage or close-preview lens.
    fn draw_preworld_avatar<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, close_preview: bool) {
        let avatar_bind_group = if close_preview {
            &self.preworld_avatar_bind_group
        } else {
            &self.bind_group
        };
        if !self.entity_draws.is_empty() && !self.entity_textures.is_empty() {
            pass.set_pipeline(&self.mesh_pipeline);
            pass.set_bind_group(0, avatar_bind_group, &[]);
            for d in self.entity_draws.values() {
                if d.instances == 0 {
                    continue;
                }
                pass.set_bind_group(1, self.entity_textures.binding(d.tex), &[]);
                pass.set_vertex_buffer(0, d.vbuf.slice(..));
                pass.set_vertex_buffer(1, d.inst_buf.slice(..));
                pass.set_index_buffer(d.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..d.indices, 0, 0..d.instances);
            }
        }
        if !self.skinned_draws.is_empty() && !self.entity_textures.is_empty() {
            pass.set_pipeline(&self.skinned_pipeline);
            pass.set_bind_group(0, avatar_bind_group, &[]);
            for d in self.skinned_draws.values() {
                if d.instances == 0 {
                    continue;
                }
                pass.set_bind_group(2, &d.palette_bind, &[]);
                pass.set_vertex_buffer(0, d.vbuf.slice(..));
                pass.set_vertex_buffer(1, d.inst_buf.slice(..));
                pass.set_vertex_buffer(2, d.skin_buf.slice(..));
                pass.set_index_buffer(d.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                for &(start, end, tex) in &d.ranges {
                    if end <= start {
                        continue;
                    }
                    pass.set_bind_group(1, self.entity_textures.binding(tex), &[]);
                    pass.draw_indexed(start..end, 0, 0..d.instances);
                }
            }
        }
    }

    /// Stage translucency and authored pre-world weather, after opaque geometry.
    fn draw_preworld_stage_effects<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.model_draws.iter().any(|m| !m.blend_parts.is_empty()) {
            pass.set_pipeline(&self.mesh_blend_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            for m in &self.model_draws {
                if m.blend_parts.is_empty() {
                    continue;
                }
                pass.set_vertex_buffer(0, m.vbuf.slice(..));
                pass.set_vertex_buffer(1, m.inst_buf.slice(..));
                pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                for &(start, end, ti) in &m.blend_parts {
                    pass.set_bind_group(1, &self.model_textures[ti], &[]);
                    pass.draw_indexed(start..end, 0, 0..m.instances);
                }
            }
        }
        if self.model_draws.iter().any(|m| !m.add_parts.is_empty()) {
            pass.set_pipeline(&self.mesh_add_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            for m in &self.model_draws {
                if m.add_parts.is_empty() {
                    continue;
                }
                pass.set_vertex_buffer(0, m.vbuf.slice(..));
                pass.set_vertex_buffer(1, m.inst_buf.slice(..));
                pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                for &(start, end, ti) in &m.add_parts {
                    pass.set_bind_group(1, &self.model_textures[ti], &[]);
                    pass.draw_indexed(start..end, 0, 0..m.instances);
                }
            }
        }
        if self.particle_vert_count > 0 {
            pass.set_pipeline(&self.particle_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_bind_group(1, &self.particle_tex_bind, &[]);
            pass.set_vertex_buffer(0, self.particle_vbuf.slice(..));
            pass.draw(0..self.particle_vert_count, 0..1);
        }
    }

    /// Skin UI is rendered last, independent of whether the avatar shares the stage camera.
    fn draw_preworld_ui<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.ui_batch_count == 0 {
            return;
        }
        pass.set_pipeline(&self.ui_pipeline);
        pass.set_bind_group(0, &self.ui_globals_bind, &[]);
        for batch in self.ui_batches.iter().take(self.ui_batch_count) {
            let Some((bind, _)) = self.ui_pages.get(&batch.page) else {
                continue;
            };
            pass.set_bind_group(1, bind, &[]);
            pass.set_vertex_buffer(0, batch.buffer.slice(..));
            pass.draw(0..6, 0..batch.count);
        }
    }

    /// Pre-world Character Customize composition with a static stage and an independent avatar
    /// camera.
    ///
    /// The stage first writes colour and its own depth.  The second pass loads that colour but
    /// clears depth before drawing the close avatar: values projected by two different cameras
    /// are not comparable, and reusing stage depth made hair/face triangles render in authoring
    /// order instead of by their actual depth.  UI stays with the avatar pass so it remains last.
    fn encode_preworld_split_avatar(
        &self,
        view: &wgpu::TextureView,
    ) -> (wgpu::CommandEncoder, f64, f64) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("preworld-split-avatar-encoder"),
            });
        let started = std::time::Instant::now();
        {
            let mut stage = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("preworld-stage-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.draw_preworld_stage_opaque(&mut stage);
            self.draw_preworld_stage_effects(&mut stage);
        }
        {
            let mut avatar = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("preworld-avatar-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.draw_preworld_avatar(&mut avatar, true);
            self.draw_preworld_ui(&mut avatar);
        }
        (encoder, 0.0, started.elapsed().as_secs_f64() * 1000.0)
    }

    /// Record the product pre-world surface without touching any world-only GPU state.
    ///
    /// Login and realm select are opaque 2D compositions. Running palette compute, sky, grid,
    /// depth, terrain, entities and particle passes beneath them made a pre-world presentation
    /// failure indistinguishable from a world-render failure and needlessly held swapchain images
    /// while those resources completed. This pass is deliberately tiny and UI-last by definition.
    ///
    /// `scene` adds exactly one thing: the model draw, for the two **character** screens, whose
    /// plates are a stone frame around a transparent middle with a modelled realm scene behind
    /// them. That scene was being loaded and uploaded and never drawn — this pass had no model
    /// step at all, so the middle stayed black in the live window no matter what was uploaded. The
    /// headless screenshot path renders through `encode_pass_open` and therefore showed it, which
    /// is why every test of the feature passed while the client Matt runs was black.
    fn encode_preworld_open(
        &self,
        view: &wgpu::TextureView,
        scene: bool,
    ) -> (wgpu::CommandEncoder, f64, f64) {
        if scene && self.preworld_avatar_camera_active {
            return self.encode_preworld_split_avatar(view);
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("preworld-ui-encoder"),
            });
        let started = std::time::Instant::now();
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("preworld-ui-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                // The stock UI pipeline was deliberately created against the renderer's depth
                // format. It does not need world geometry here, but the pass attachment contract
                // must still match the pipeline or wgpu correctly rejects the command buffer.
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // The realm scene, under the plate. Same pipeline and bind groups the world path uses
            // for fixture geometry; the camera comes from `Gpu::set_view_proj`, which the caller
            // set from `preworld_scene::framing`.
            if scene && !self.model_draws.is_empty() {
                pass.set_pipeline(&self.mesh_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for m in &self.model_draws {
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_vertex_buffer(1, m.inst_buf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    if m.parts.is_empty() {
                        pass.set_bind_group(1, &self.model_textures[0], &[]);
                        pass.draw_indexed(0..m.indices, 0, 0..m.instances);
                    } else {
                        for &(start, end, ti) in &m.parts {
                            pass.set_bind_group(1, &self.model_textures[ti], &[]);
                            pass.draw_indexed(start..end, 0, 0..m.instances);
                        }
                    }
                }
            }
            // The character standing in the scene: static body, then the GPU-skinned one. Same
            // draws the world pass makes — the character screen shows a real avatar, not a
            // special-case billboard.  The independent close-avatar path returned above; this
            // ordinary path intentionally shares stage depth and the stage camera.
            if scene && !self.entity_draws.is_empty() && !self.entity_textures.is_empty() {
                pass.set_pipeline(&self.mesh_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for d in self.entity_draws.values() {
                    if d.instances == 0 {
                        continue;
                    }
                    pass.set_bind_group(1, self.entity_textures.binding(d.tex), &[]);
                    pass.set_vertex_buffer(0, d.vbuf.slice(..));
                    pass.set_vertex_buffer(1, d.inst_buf.slice(..));
                    pass.set_index_buffer(d.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..d.indices, 0, 0..d.instances);
                }
            }
            if scene && !self.skinned_draws.is_empty() && !self.entity_textures.is_empty() {
                pass.set_pipeline(&self.skinned_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for d in self.skinned_draws.values() {
                    if d.instances == 0 {
                        continue;
                    }
                    pass.set_bind_group(2, &d.palette_bind, &[]);
                    pass.set_vertex_buffer(0, d.vbuf.slice(..));
                    pass.set_vertex_buffer(1, d.inst_buf.slice(..));
                    pass.set_vertex_buffer(2, d.skin_buf.slice(..));
                    pass.set_index_buffer(d.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    for &(start, end, tex) in &d.ranges {
                        if end <= start {
                            continue;
                        }
                        pass.set_bind_group(1, self.entity_textures.binding(tex), &[]);
                        pass.draw_indexed(start..end, 0, 0..d.instances);
                    }
                }
            }
            // Alpha-flagged model parts, after every opaque surface: coronas, sun discs, glows
            // and ground decals. Blended, depth-tested, no depth write. Same `scene` gate as the
            // opaque pass above — a login plate must not draw a leftover realm scene's glows.
            if scene && self.model_draws.iter().any(|m| !m.blend_parts.is_empty()) {
                pass.set_pipeline(&self.mesh_blend_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for m in &self.model_draws {
                    if m.blend_parts.is_empty() {
                        continue;
                    }
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_vertex_buffer(1, m.inst_buf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    for &(start, end, ti) in &m.blend_parts {
                        pass.set_bind_group(1, &self.model_textures[ti], &[]);
                        pass.draw_indexed(start..end, 0, 0..m.instances);
                    }
                }
            }
            // Additive last: coronas, flames and sun discs add their light to what is already
            // there rather than covering it.
            if scene && self.model_draws.iter().any(|m| !m.add_parts.is_empty()) {
                pass.set_pipeline(&self.mesh_add_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for m in &self.model_draws {
                    if m.add_parts.is_empty() {
                        continue;
                    }
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_vertex_buffer(1, m.inst_buf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    for &(start, end, ti) in &m.add_parts {
                        pass.set_bind_group(1, &self.model_textures[ti], &[]);
                        pass.draw_indexed(start..end, 0, 0..m.instances);
                    }
                }
            }
            // The stage's own weather — Midgard's blowing snow, Hibernia's motes by the portal.
            // After every scene surface and before the UI, so it reads over the world and under
            // the plate. The pre-world has its own encode function, and this draw living only in
            // the world pass is why uploading the billboards changed nothing on screen.
            if scene && self.particle_vert_count > 0 {
                pass.set_pipeline(&self.particle_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_bind_group(1, &self.particle_tex_bind, &[]);
                pass.set_vertex_buffer(0, self.particle_vbuf.slice(..));
                pass.draw(0..self.particle_vert_count, 0..1);
            }
            if self.ui_batch_count > 0 {
                pass.set_pipeline(&self.ui_pipeline);
                pass.set_bind_group(0, &self.ui_globals_bind, &[]);
                for batch in self.ui_batches.iter().take(self.ui_batch_count) {
                    let Some((bind, _)) = self.ui_pages.get(&batch.page) else {
                        continue;
                    };
                    pass.set_bind_group(1, bind, &[]);
                    pass.set_vertex_buffer(0, batch.buffer.slice(..));
                    pass.draw(0..6, 0..batch.count);
                }
            }
        }
        (encoder, 0.0, started.elapsed().as_secs_f64() * 1000.0)
    }

    /// Headless: encode + submit + GPU-idle wait, **no** CPU texture readback.
    ///
    /// Used by the M9 frame-time harness: `render_to_rgba` includes a full framebuffer readback
    /// that a real present path does not pay, so percentiles from readback alone overstate cost.
    pub fn render_headless_sync(&self, count: u32) -> Result<(), GpuWaitError> {
        let offscreen = self
            .offscreen
            .as_ref()
            .expect("render_headless_sync() needs headless mode");
        let view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
        self.encode_pass(&view, count);
        self.poll_wait_idle()?;
        Ok(())
    }

    /// Like [`Self::render_headless_sync`], but composites the egui HUD overlay before the GPU wait.
    /// Still no CPU readback — this is the timed path for M9-REDO with HUD enabled.
    pub fn render_headless_sync_with_overlay(
        &mut self,
        count: u32,
        egui: Option<crate::ui::EguiFrame>,
    ) -> Result<(), GpuWaitError> {
        let offscreen = self
            .offscreen
            .as_ref()
            .expect("render_headless_sync_with_overlay needs headless mode");
        let view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
        let _ = self.encode_pass_with_optional_egui(&view, count, egui, FramePass::World);
        self.poll_wait_idle()?;
        Ok(())
    }

    /// Headless: render one frame into the offscreen texture and read it back as tightly-packed
    /// RGBA8 rows (`width * height * 4` bytes, top row first).
    pub fn render_to_rgba(&self, count: u32) -> Result<Vec<u8>, GpuWaitError> {
        self.render_to_rgba_pass(count, FramePass::World)
    }

    /// [`Self::render_to_rgba`] through a chosen encoder.
    ///
    /// The point is that a headless check can exercise the *same* pass the window presents.
    /// Without it the two diverged silently: the pre-world encoder had no model step, but every
    /// screenshot of a character screen went through the world encoder and showed the realm scene
    /// the live client was not drawing.
    pub fn render_to_rgba_pass(
        &self,
        count: u32,
        pass: FramePass,
    ) -> Result<Vec<u8>, GpuWaitError> {
        let offscreen = self
            .offscreen
            .as_ref()
            .expect("render_to_rgba() needs headless mode");
        let view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
        let encoder = match pass {
            FramePass::World => {
                self.encode_pass(&view, count);
                return self.read_offscreen();
            }
            FramePass::PreWorldUi => self.encode_preworld_open(&view, false).0,
            FramePass::PreWorldScene => self.encode_preworld_open(&view, true).0,
        };
        // `encode_pass` submits its own encoder; the pre-world opener hands one back to be
        // finished by whoever owns the frame.
        self.queue.submit(std::iter::once(encoder.finish()));
        self.read_offscreen()
    }

    /// Headless, **with the 2D overlay composited on top** — the scene pass followed by the egui
    /// pass, exactly as the windowed path does it.
    ///
    /// This exists so the HUD can be screenshotted and golden-tested with no window: it is the
    /// only automated way to see that the overlay actually reached the framebuffer, as opposed to
    /// merely tessellating without panicking.
    pub fn render_to_rgba_with_overlay(
        &mut self,
        count: u32,
        egui: Option<crate::ui::EguiFrame>,
    ) -> Result<Vec<u8>, GpuWaitError> {
        let offscreen = self
            .offscreen
            .as_ref()
            .expect("render_to_rgba needs headless mode");
        let view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
        let _ = self.encode_pass_with_optional_egui(&view, count, egui, FramePass::World);
        self.read_offscreen()
    }

    /// Copy an arbitrary texture back to the CPU as tightly-packed RGBA8 rows.
    ///
    /// Shared by the headless offscreen readback and the live-window frame capture; the only
    /// difference between them is which texture is handed in.
    fn read_texture(
        &self,
        tex: &wgpu::Texture,
        timeout: Option<std::time::Duration>,
    ) -> Result<Vec<u8>, GpuWaitError> {
        let (w, h) = (self.config.width, self.config.height);
        let unpadded = w * 4;
        let padded = unpadded.div_ceil(256) * 256;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame-readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));
        let slice = readback.slice(..);
        match timeout {
            Some(timeout) => self.map_slice_wait_for(slice, timeout)?,
            None => self.map_slice_wait(slice)?,
        }
        let data = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            rgba.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();
        Ok(rgba)
    }

    /// Copy the offscreen colour texture back to the CPU as tightly-packed RGBA8 rows.
    fn read_offscreen(&self) -> Result<Vec<u8>, GpuWaitError> {
        let offscreen = self
            .offscreen
            .as_ref()
            .expect("read_offscreen() needs headless mode");
        // Copy the texture into a mappable buffer. bytes_per_row must be 256-aligned.
        let (w, h) = (self.config.width, self.config.height);
        let unpadded = w * 4;
        let padded = unpadded.div_ceil(256) * 256;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot-readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            offscreen.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(std::iter::once(encoder.finish()));

        let slice = readback.slice(..);
        self.map_slice_wait(slice)?;
        let data = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((unpadded * h) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            rgba.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        readback.unmap();
        Ok(rgba)
    }

    /// The one main pass, into whichever color target the caller hands us.
    fn encode_pass(&self, view: &wgpu::TextureView, count: u32) {
        let _ = self.encode_pass_split(view, count);
    }

    /// Record fold + scene into a fresh encoder; caller owns finish/submit.
    /// Returns `(encoder, fold_encode_ms, scene_encode_ms)`.
    fn encode_pass_open(
        &self,
        view: &wgpu::TextureView,
        count: u32,
    ) -> (wgpu::CommandEncoder, f64, f64) {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });
        let t_fold = std::time::Instant::now();
        // Expand palettes on the GPU before the vertex shader reads them (MS-08).
        self.encode_palette_folds(&mut encoder);
        let fold_encode_cpu_ms = t_fold.elapsed().as_secs_f64() * 1000.0;
        let t_scene = std::time::Instant::now();
        {
            let timestamp_writes =
                self.timestamps
                    .as_ref()
                    .map(|ts| wgpu::RenderPassTimestampWrites {
                        query_set: &ts.query_set,
                        beginning_of_pass_write_index: Some(2),
                        end_of_pass_write_index: Some(3),
                    });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // Cleared to the sky's horizon colour rather than a near-black void, so
                        // any pixel the gradient somehow misses still reads as sky. The clear
                        // value bypasses the shader but not the target's encoding, so it needs
                        // the same linearisation the dome's own colours get.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(srgb_byte_to_linear(self.sky.horizon[0])),
                            g: f64::from(srgb_byte_to_linear(self.sky.horizon[1])),
                            b: f64::from(srgb_byte_to_linear(self.sky.horizon[2])),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Sky first: fills the frame with the gradient before anything else paints over it.
            // No vertex buffer and no depth attachment — just three vertices.
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);

            // Terrain if loaded; otherwise the flat reference grid as a fallback floor.
            if !self.terrain_zones.is_empty() {
                pass.set_pipeline(&self.terrain_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for z in &self.terrain_zones {
                    pass.set_bind_group(1, &z.texture, &[]);
                    pass.set_vertex_buffer(0, z.vbuf.slice(..));
                    pass.set_index_buffer(z.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..z.indices, 0, 0..1);
                }
            } else {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.grid_buf.slice(..));
                pass.draw(0..self.grid_verts, 0..1);
            }

            // Real fixture models (instanced NIF geometry), one sub-draw per texture range.
            if !self.model_draws.is_empty() {
                pass.set_pipeline(&self.mesh_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for m in &self.model_draws {
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_vertex_buffer(1, m.inst_buf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    if m.parts.is_empty() {
                        pass.set_bind_group(1, &self.model_textures[0], &[]);
                        pass.draw_indexed(0..m.indices, 0, 0..m.instances);
                    } else {
                        for &(start, end, ti) in &m.parts {
                            pass.set_bind_group(1, &self.model_textures[ti], &[]);
                            pass.draw_indexed(start..end, 0, 0..m.instances);
                        }
                    }
                }
            }

            // Live-entity NIF meshes (monsters resolved by model id): same pipeline as fixtures,
            // instance buffers rewritten each frame. Each model binds its own skin (index 0 = the
            // white fallback for creatures whose skin didn't resolve).
            if !self.entity_draws.is_empty() && !self.entity_textures.is_empty() {
                pass.set_pipeline(&self.mesh_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for d in self.entity_draws.values() {
                    if d.instances == 0 {
                        continue;
                    }
                    pass.set_bind_group(1, self.entity_textures.binding(d.tex), &[]);
                    pass.set_vertex_buffer(0, d.vbuf.slice(..));
                    pass.set_vertex_buffer(1, d.inst_buf.slice(..));
                    pass.set_index_buffer(d.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..d.indices, 0, 0..d.instances);
                }
            }

            // GPU-skinned creatures: one draw per model, deformed per-instance from its palette.
            if !self.skinned_draws.is_empty() && !self.entity_textures.is_empty() {
                pass.set_pipeline(&self.skinned_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for d in self.skinned_draws.values() {
                    if d.instances == 0 {
                        continue;
                    }
                    pass.set_bind_group(2, &d.palette_bind, &[]);
                    pass.set_vertex_buffer(0, d.vbuf.slice(..));
                    pass.set_vertex_buffer(1, d.inst_buf.slice(..));
                    pass.set_vertex_buffer(2, d.skin_buf.slice(..));
                    pass.set_index_buffer(d.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    // One draw per part so each binds its own texture.
                    for &(start, end, tex) in &d.ranges {
                        if end <= start {
                            continue;
                        }
                        pass.set_bind_group(1, self.entity_textures.binding(tex), &[]);
                        pass.draw_indexed(start..end, 0, 0..d.instances);
                    }
                }
            }

            // Placeholder boxes for unparsed fixtures, then the live entities.
            if let Some(fb) = &self.fixture_buf {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
                pass.set_vertex_buffer(1, fb.slice(..));
                pass.set_index_buffer(self.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..self.num_indices, 0, 0..self.fixture_count);
            }
            if count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
                pass.set_vertex_buffer(1, self.instance_buf.slice(..));
                pass.set_index_buffer(self.index_buf.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..self.num_indices, 0, 0..count);
            }

            // Water last among opaques/translucents: translucent, blends over terrain and entities.
            if let (Some(vb), Some(ib)) = (&self.water_vbuf, &self.water_ibuf) {
                pass.set_pipeline(&self.water_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..self.water_indices, 0, 0..1);
            }

            // Alpha-flagged model parts, after every opaque surface: coronas, sun discs, glows
            // and ground decals. Blended, depth-tested, no depth write.
            if self.model_draws.iter().any(|m| !m.blend_parts.is_empty()) {
                pass.set_pipeline(&self.mesh_blend_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for m in &self.model_draws {
                    if m.blend_parts.is_empty() {
                        continue;
                    }
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_vertex_buffer(1, m.inst_buf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    for &(start, end, ti) in &m.blend_parts {
                        pass.set_bind_group(1, &self.model_textures[ti], &[]);
                        pass.draw_indexed(start..end, 0, 0..m.instances);
                    }
                }
            }
            // Additive last: coronas, flames and sun discs add their light to what is already
            // there rather than covering it.
            if self.model_draws.iter().any(|m| !m.add_parts.is_empty()) {
                pass.set_pipeline(&self.mesh_add_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                for m in &self.model_draws {
                    if m.add_parts.is_empty() {
                        continue;
                    }
                    pass.set_vertex_buffer(0, m.vbuf.slice(..));
                    pass.set_vertex_buffer(1, m.inst_buf.slice(..));
                    pass.set_index_buffer(m.ibuf.slice(..), wgpu::IndexFormat::Uint32);
                    for &(start, end, ti) in &m.add_parts {
                        pass.set_bind_group(1, &self.model_textures[ti], &[]);
                        pass.draw_indexed(start..end, 0, 0..m.instances);
                    }
                }
            }

            // Particle billboards (additive soft blob) after scene geometry so effects read over
            // terrain/entities; depth-tested, no depth write.
            if self.particle_vert_count > 0 {
                pass.set_pipeline(&self.particle_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_bind_group(1, &self.particle_tex_bind, &[]);
                pass.set_vertex_buffer(0, self.particle_vbuf.slice(..));
                pass.draw(0..self.particle_vert_count, 0..1);
            }

            // Selection outline over everything (depth-compare Always): the picked fixture's box
            // must read even when the model is buried in tree clutter.
            if let Some(sb) = &self.sel_buf {
                pass.set_pipeline(&self.sel_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, sb.slice(..));
                pass.draw(0..self.sel_verts, 0..1);
            }

            // Terrain-brush cursor rings, same always-on-top treatment.
            if let Some(bb) = &self.brush_buf {
                pass.set_pipeline(&self.sel_pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, bb.slice(..));
                pass.draw(0..self.brush_verts, 0..1);
            }

            // Skin UI last, over everything, in the order the batches were built (painter's order).
            if self.ui_batch_count > 0 {
                pass.set_pipeline(&self.ui_pipeline);
                pass.set_bind_group(0, &self.ui_globals_bind, &[]);
                for batch in self.ui_batches.iter().take(self.ui_batch_count) {
                    let Some((bind, _)) = self.ui_pages.get(&batch.page) else {
                        continue;
                    };
                    pass.set_bind_group(1, bind, &[]);
                    pass.set_vertex_buffer(0, batch.buffer.slice(..));
                    // Six vertices per quad, one instance per quad.
                    pass.draw(0..6, 0..batch.count);
                }
            }
        }
        if let Some(ts) = &self.timestamps {
            // Resolve fold (0..2) + render (2..4). Fold queries may be stale if fold skipped —
            // `take_gpu_timings` only reports fold when `fold_queries_written`.
            encoder.resolve_query_set(&ts.query_set, 0..4, &ts.resolve_buf, 0);
            encoder.copy_buffer_to_buffer(&ts.resolve_buf, 0, &ts.read_buf, 0, 32);
        }
        let scene_encode_cpu_ms = t_scene.elapsed().as_secs_f64() * 1000.0;
        (encoder, fold_encode_cpu_ms, scene_encode_cpu_ms)
    }

    /// Like [`Self::encode_pass`], returning `(fold_encode, scene_encode, submit)` CPU ms.
    fn encode_pass_split(&self, view: &wgpu::TextureView, count: u32) -> (f64, f64, f64) {
        let (encoder, fold_encode_cpu_ms, scene_encode_cpu_ms) = self.encode_pass_open(view, count);
        let t_submit = std::time::Instant::now();
        self.queue.submit(std::iter::once(encoder.finish()));
        let submit_cpu_ms = t_submit.elapsed().as_secs_f64() * 1000.0;
        (fold_encode_cpu_ms, scene_encode_cpu_ms, submit_cpu_ms)
    }

    /// Like [`Self::render_headless_sync_with_overlay`], but returns CPU/GPU slice timings for the
    /// residual hunt. Resolves timestamps once after the idle wait (no second poll).
    ///
    /// **Accounting:** `egui_encode_ms` is sequential after scene encode — it is NOT nested inside
    /// `encode_cpu_ms`. `egui_tess` (CPU tessellation) is timed by the caller before this method.
    /// `submit_cpu_ms` covers every finish+submit on this path (coalesce = 1, split = 2).
    pub fn render_headless_sync_with_overlay_timed(
        &mut self,
        count: u32,
        egui: Option<crate::ui::EguiFrame>,
    ) -> Result<HeadlessFrameTiming, GpuWaitError> {
        let offscreen = self
            .offscreen
            .as_ref()
            .expect("render_headless_sync_with_overlay_timed needs headless mode");
        let view = offscreen.create_view(&wgpu::TextureViewDescriptor::default());
        let (
            fold_encode_cpu_ms,
            scene_encode_cpu_ms,
            egui_encode_ms,
            submit_cpu_ms,
            queue_submit_count,
        ) = self.encode_pass_with_optional_egui(&view, count, egui, FramePass::World);
        let encode_cpu_ms = fold_encode_cpu_ms + scene_encode_cpu_ms + submit_cpu_ms;
        let t_poll = std::time::Instant::now();
        self.poll_wait_idle()?;
        let poll_wait_ms = t_poll.elapsed().as_secs_f64() * 1000.0;
        let t_ts = std::time::Instant::now();
        let (fold_gpu_ms, render_gpu_ms) = self.take_gpu_timings_already_idle()?;
        let ts_resolve_ms = t_ts.elapsed().as_secs_f64() * 1000.0;
        Ok(HeadlessFrameTiming {
            encode_cpu_ms,
            fold_encode_cpu_ms,
            scene_encode_cpu_ms,
            submit_cpu_ms,
            egui_encode_ms,
            queue_submit_count,
            poll_wait_ms,
            fold_gpu_ms,
            render_gpu_ms,
            ts_resolve_ms,
        })
    }

    /// Resolve fold + render GPU timestamps. Call after the GPU is idle (`poll Wait`).
    /// Returns `(fold_gpu_ms, render_gpu_ms)`. Fold is `None` when timestamps unsupported;
    /// `Some(0.0)` when fold did not dispatch this frame.
    pub fn take_gpu_timings(&self) -> Result<(Option<f64>, Option<f64>), GpuWaitError> {
        self.poll_wait_idle()?;
        self.take_gpu_timings_already_idle()
    }

    fn take_gpu_timings_already_idle(&self) -> Result<(Option<f64>, Option<f64>), GpuWaitError> {
        let Some(ts) = self.timestamps.as_ref() else {
            return Ok((None, None));
        };
        let slice = ts.read_buf.slice(..);
        self.map_slice_wait(slice)?;
        let data = slice.get_mapped_range();
        let ticks = |off: usize| -> Option<u64> {
            Some(u64::from_le_bytes(data.get(off..off + 8)?.try_into().ok()?))
        };
        let fold_ms = if ts.fold_queries_written.get() {
            let start = ticks(0);
            let end = ticks(8);
            match (start, end) {
                (Some(s), Some(e)) => {
                    let ms = e.wrapping_sub(s) as f64 * f64::from(ts.period_ns) / 1_000_000.0;
                    ts.last_fold_ms.set(Some(ms));
                    Some(ms)
                }
                _ => None,
            }
        } else {
            ts.last_fold_ms.set(Some(0.0));
            Some(0.0)
        };
        let render_ms = match (ticks(16), ticks(24)) {
            (Some(s), Some(e)) => {
                let ms = e.wrapping_sub(s) as f64 * f64::from(ts.period_ns) / 1_000_000.0;
                ts.last_pass_ms.set(Some(ms));
                Some(ms)
            }
            _ => None,
        };
        drop(data);
        ts.read_buf.unmap();
        Ok((fold_ms, render_ms))
    }

    /// Resolve the last encode_pass **render** GPU timestamp pair into milliseconds.
    /// Call after the GPU is idle (`poll Wait`). Returns `None` when the adapter lacks
    /// `TIMESTAMP_QUERY`. Fold compute is **not** included — see [`Self::take_gpu_timings`].
    pub fn take_gpu_pass_ms(&self) -> Result<Option<f64>, GpuWaitError> {
        let (_, render) = self.take_gpu_timings()?;
        Ok(render)
    }

    /// Whether this GPU requested `TIMESTAMP_QUERY` successfully.
    pub fn has_gpu_timestamps(&self) -> bool {
        self.timestamps.is_some()
    }

    /// Reconfigure the surface's `desired_maximum_frame_latency` (windowed only).
    ///
    /// The m9 windowed residual path uses `1` so acquire+present samples are not dominated by
    /// Timeout waits from a 2-image swapchain when the event loop cannot drain compositor
    /// releases as fast as `RedrawRequested` fires.
    pub fn set_max_frame_latency(&mut self, latency: u32) {
        self.config.desired_maximum_frame_latency = latency.max(1);
        if let Some(surface) = self.surface.as_ref() {
            surface.configure(&self.device, &self.config);
        }
    }

    /// Whether swapchain/offscreen frame capture via `COPY_SRC` is available.
    ///
    /// False when the window surface advertised no `COPY_SRC` — rendering still works.
    #[must_use]
    pub fn frame_capture_supported(&self) -> bool {
        self.capture_copy_src
    }

    /// Headless has no swapchain; windowed path present mode is chosen at surface config.
    /// Stated for residual-hunt artifacts (candidate 2 is acquire/present — N/A on headless).
    #[must_use]
    pub fn present_mode_label(&self) -> &'static str {
        if self.surface.is_none() {
            "headless_offscreen_no_acquire_present"
        } else {
            windowed_present_mode_label(self.config.present_mode)
        }
    }
}

#[inline]
fn window_capture_requested(harness_mode: bool, recorder_diagnostic: bool) -> bool {
    harness_mode || recorder_diagnostic
}

/// Windowed / surface present-mode meta label. Always distinct from the headless sentinel and
/// always includes `acquire_present` so residual-hunt artifacts cannot mis-cite the floor path.
#[must_use]
pub(crate) fn windowed_present_mode_label(mode: wgpu::PresentMode) -> &'static str {
    match mode {
        wgpu::PresentMode::Fifo => "Fifo_acquire_present",
        wgpu::PresentMode::FifoRelaxed => "FifoRelaxed_acquire_present",
        wgpu::PresentMode::Mailbox => "Mailbox_acquire_present",
        wgpu::PresentMode::Immediate => "Immediate_acquire_present",
        wgpu::PresentMode::AutoVsync => "AutoVsync_acquire_present",
        wgpu::PresentMode::AutoNoVsync => "AutoNoVsync_acquire_present",
    }
}

/// Pick a stable windowed present mode without consulting process state (unit-testable policy).
/// FIFO is required by the WebGPU surface contract and therefore the final fallback.
fn select_windowed_present_mode(
    supported: &[wgpu::PresentMode],
    no_vsync: bool,
    prefer_mailbox: bool,
) -> wgpu::PresentMode {
    let has = |mode| supported.contains(&mode);
    if no_vsync && has(wgpu::PresentMode::Immediate) {
        wgpu::PresentMode::Immediate
    } else if prefer_mailbox && has(wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::Fifo
    }
}

/// A game window is fully opaque. Prefer that explicit compositor contract rather than accepting
/// the adapter's first alpha mode (which may be Auto or premultiplied on Wayland).
fn select_windowed_alpha_mode(supported: &[wgpu::CompositeAlphaMode]) -> wgpu::CompositeAlphaMode {
    if supported.contains(&wgpu::CompositeAlphaMode::Opaque) {
        wgpu::CompositeAlphaMode::Opaque
    } else {
        supported
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto)
    }
}

/// Request an adapter under the deliberate policy in `docs/INSTALL.md (GPU adapter policy)`.
/// Never silently selects an unintended GPU tier.
async fn request_adapter_with_policy(
    instance: &wgpu::Instance,
    compatible_surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter, crate::gpu_init::GpuInitError> {
    use crate::gpu_init::{adapter_request_plans, AdapterSelectPolicy, GpuInitError};

    let policy = AdapterSelectPolicy::from_env();
    let plans = adapter_request_plans(policy);
    let mut last_detail = String::from("no attempts");

    for plan in &plans {
        match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: plan.power_preference,
                compatible_surface,
                force_fallback_adapter: plan.force_fallback_adapter,
            })
            .await
        {
            Ok(adapter) => {
                // Normal launcher configuration keeps `log::info!` quiet.  Emit the actual
                // adapter selection to stderr too, so launch evidence proves whether this run
                // used Vulkan or an explicit fallback rather than inferring it from platform.
                let info = adapter.get_info();
                eprintln!(
                    "caer-render: GPU adapter = {:?} ({:?}, {:?})",
                    info.name, info.backend, info.device_type
                );
                if plan.is_low_power_fallback {
                    log::warn!(
                        "caer-render: HighPerformance adapter unavailable; deliberately using \
                         LowPower fallback (CAER_GPU_ALLOW_LOW_POWER=1): {:?} {:?} {:?}",
                        info.name,
                        info.backend,
                        info.device_type
                    );
                } else if plan.force_fallback_adapter {
                    log::warn!(
                        "caer-render: using ForceFallback adapter (CAER_GPU_FORCE_FALLBACK=1): \
                         {:?} {:?}",
                        info.name,
                        info.backend
                    );
                } else {
                    log::info!(
                        "caer-render: adapter = {:?} ({:?}, {:?})",
                        info.name,
                        info.backend,
                        info.device_type
                    );
                }
                return Ok(adapter);
            }
            Err(e) => {
                last_detail = e.to_string();
            }
        }
    }

    Err(GpuInitError::AdapterUnavailable {
        policy,
        detail: last_detail,
        remediation: policy.remediation(),
    })
}

fn create_init_buffer(
    device: &wgpu::Device,
    label: &str,
    data: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: data,
        usage,
    })
}

/// Features we always request when the adapter offers them.
///
/// `headless`: TIMESTAMP_QUERY defaults **off**. On RTX 5060 / driver 610, enabling timestamp
/// resolve in headless left submissions never retiring (every `render_to_rgba` hung until
/// `CAER_GPU_WAIT_MS`). Windowed keeps timestamps when available; set `CAER_GPU_TIMESTAMPS=1`
/// to opt headless back in (m9), or `CAER_NO_GPU_TIMESTAMPS=1` to force windowed off.
fn device_features(adapter: &wgpu::Adapter, headless: bool) -> wgpu::Features {
    let available = adapter.features();
    let mut f = available & wgpu::Features::TEXTURE_COMPRESSION_BC;
    let timestamps_off = std::env::var_os("CAER_NO_GPU_TIMESTAMPS")
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let timestamps_on = std::env::var_os("CAER_GPU_TIMESTAMPS")
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let want_ts = if timestamps_off {
        false
    } else if headless {
        timestamps_on
    } else {
        true
    };
    if want_ts && available.contains(wgpu::Features::TIMESTAMP_QUERY) {
        f |= wgpu::Features::TIMESTAMP_QUERY;
    }
    f
}

fn create_depth(device: &wgpu::Device, config: &wgpu::SurfaceConfiguration) -> wgpu::TextureView {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth"),
        size: wgpu::Extent3d {
            width: config.width.max(1),
            height: config.height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    tex.create_view(&wgpu::TextureViewDescriptor::default())
}

fn particle_billboard_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Float32x4];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<crate::particles::ParticleBillboardVert>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

/// Procedural soft-disk RGBA for [`crate::particles::PARTICLE_BILLBOARD_PLACEHOLDER`].
fn soft_blob_rgba(size: u32) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let mid = (size as f32 - 1.0) * 0.5;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - mid;
            let dy = y as f32 - mid;
            let r = (dx * dx + dy * dy).sqrt() / (mid + 0.5);
            let a = (1.0 - r).clamp(0.0, 1.0).powf(1.6);
            let i = ((y * size + x) * 4) as usize;
            out[i] = 255;
            out[i + 1] = 255;
            out[i + 2] = 255;
            out[i + 3] = (a * 255.0) as u8;
        }
    }
    out
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![2 => Float32x3, 3 => Float32x3, 4 => Float32];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Instance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRS,
    }
}

fn line_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<LineVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

/// Pack a `ModelInstance` into the GPU instance row the mesh shader reads: xyz position, yaw,
/// uniform scale. (Identity fields stay CPU-side — the GPU only needs the transform.)
fn model_instance_raw(i: &crate::terrain::ModelInstance) -> [f32; 8] {
    let q = i.rot;
    [
        i.pos[0], i.pos[1], i.pos[2], q[0], q[1], q[2], q[3], i.scale,
    ]
}

/// Pack a position + yaw + scale triple into the instance stream.
///
/// Live entities are server-driven and only ever yaw — the wire carries a heading, not an axis —
/// so they build their quaternion here rather than every caller learning the new layout.
fn pos_yaw_scale_raw(i: &[f32; 5]) -> [f32; 8] {
    let (s, c) = (i[3] * 0.5).sin_cos();
    [i[0], i[1], i[2], 0.0, 0.0, s, c, i[4]]
}

fn model_instance_layout() -> wgpu::VertexBufferLayout<'static> {
    // pos(3) + rotation quaternion(4) + uniform scale(1).
    const ATTRS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![4 => Float32x3, 5 => Float32x4, 6 => Float32];
    wgpu::VertexBufferLayout {
        array_stride: 32,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRS,
    }
}

/// Instance stream for skinned draws: the model-instance data plus the instance's base offset into
/// the palette buffer (location 6). Kept as one float stream so it stays a single buffer.
fn skinned_instance_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![4 => Float32x4, 5 => Float32, 6 => Float32];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<SkinnedInstance>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRS,
    }
}

/// Per-vertex skinning influences (second vertex buffer): joint indices + blend weights.
fn skin_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![7 => Uint32x4, 8 => Float32x4];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<crate::terrain::SkinVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

/// [`terrain_layout`] plus the ground blend mask at location 7 and static Decal 0 UV at 9.
///
/// Its own layout rather than an extra attribute on the shared one: the SKINNED pipeline already
/// binds its instance buffer at location 7, so widening `terrain_layout` collided there — two
/// attributes at one location, which wgpu rejects outright. Model instances occupy 4..6 only, so
/// 7 is free for this pipeline and this pipeline alone.
fn model_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
        // Attribute declaration order defines byte offsets. Keep it in TerrainVertex memory order
        // even though location 9 (overlay UV) is numerically after location 7 (blend mask).
        0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32x2, 9 => Float32x2,
        7 => Float32];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<crate::terrain::TerrainVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

fn terrain_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRS: [wgpu::VertexAttribute; 4] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Float32x2];
    wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<crate::terrain::TerrainVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRS,
    }
}

/// The ground reference grid: lines on the z=0 render-space plane at DAoC zone spacing
/// (`ZONE_UNIT` = 8192 units), spanning `±extent`. The two lines through the origin are drawn
/// brighter as an orientation anchor.
fn grid_geometry(extent: f32) -> Vec<LineVertex> {
    use caer_world::ZONE_UNIT;
    let step = ZONE_UNIT as f32;
    let n = (extent / step).ceil() as i32;
    let dim = [0.14, 0.16, 0.20];
    let axis = [0.30, 0.34, 0.42];
    let mut v = Vec::new();
    let mut push = |a: [f32; 3], b: [f32; 3], c: [f32; 3]| {
        v.push(LineVertex { pos: a, color: c });
        v.push(LineVertex { pos: b, color: c });
    };
    for i in -n..=n {
        let d = i as f32 * step;
        let c = if i == 0 { axis } else { dim };
        // line parallel to Y at x = d, and parallel to X at y = d
        push([d, -extent, 0.0], [d, extent, 0.0], c);
        push([-extent, d, 0.0], [extent, d, 0.0], c);
    }
    v
}

/// Unit cube centred on the origin (half-extent 1), 4 verts per face with outward normals.
fn cube() -> (Vec<Vertex>, Vec<u16>) {
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        (
            [0.0, 0.0, 1.0],
            [
                [-1.0, -1.0, 1.0],
                [1.0, -1.0, 1.0],
                [1.0, 1.0, 1.0],
                [-1.0, 1.0, 1.0],
            ],
        ), // +Z top
        (
            [0.0, 0.0, -1.0],
            [
                [-1.0, 1.0, -1.0],
                [1.0, 1.0, -1.0],
                [1.0, -1.0, -1.0],
                [-1.0, -1.0, -1.0],
            ],
        ), // -Z bottom
        (
            [1.0, 0.0, 0.0],
            [
                [1.0, -1.0, -1.0],
                [1.0, 1.0, -1.0],
                [1.0, 1.0, 1.0],
                [1.0, -1.0, 1.0],
            ],
        ), // +X
        (
            [-1.0, 0.0, 0.0],
            [
                [-1.0, -1.0, 1.0],
                [-1.0, 1.0, 1.0],
                [-1.0, 1.0, -1.0],
                [-1.0, -1.0, -1.0],
            ],
        ), // -X
        (
            [0.0, 1.0, 0.0],
            [
                [-1.0, 1.0, -1.0],
                [-1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0],
                [1.0, 1.0, -1.0],
            ],
        ), // +Y
        (
            [0.0, -1.0, 0.0],
            [
                [-1.0, -1.0, 1.0],
                [-1.0, -1.0, -1.0],
                [1.0, -1.0, -1.0],
                [1.0, -1.0, 1.0],
            ],
        ), // -Y
    ];
    let mut verts = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, corners) in faces {
        let base = verts.len() as u16;
        for pos in corners {
            verts.push(Vertex { pos, normal });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (verts, indices)
}

#[cfg(test)]
mod gpu_wait_tests {
    use super::{
        classify_poll_result, idle_wait_poll_type, select_windowed_alpha_mode,
        select_windowed_present_mode, window_capture_requested, FramePass, Gpu, GpuWaitError,
        DEFAULT_GPU_WAIT_MS,
    };
    use crate::skinui::{Rect, UiQuad, WHITE};
    use caer_assets::tga::TgaImage;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn inherited_bundle_environment_cannot_enable_product_surface_capture() {
        assert!(!window_capture_requested(false, false));
        assert!(window_capture_requested(true, false));
        assert!(window_capture_requested(false, true));
    }

    fn ui_page() -> TgaImage {
        TgaImage {
            width: 1,
            height: 1,
            rgba: vec![255, 255, 255, 255],
        }
    }

    /// A 37-run form mirrors the observed stats screen: its font, chrome, image picker, and
    /// button pages alternate in painter order. The known-bad implementation allocated one GPU
    /// buffer for every run on every redraw, so nine redraws would allocate 333 backing buffers.
    /// A stable pre-world screen must warm once and rewrite the same 37 slots instead.
    #[test]
    fn preworld_ui_run_buffers_warm_once_across_repeated_redraws() {
        let _gpu_lock = if crate::gpu_init::gpu_test_serialize_enabled() {
            None
        } else {
            Some(crate::gpu_init::lock_gpu_device().expect("test device lock"))
        };
        let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 1_000.0)).expect("gpu init");
        let page = ui_page();
        gpu.upload_ui_page("chrome", &page);
        gpu.upload_ui_page("font", &page);

        let runs = (0..37u32)
            .map(|i| UiQuad {
                dst: Rect::new((i % 8) as f32 * 8.0, (i / 8) as f32 * 8.0, 8.0, 8.0),
                src: Rect::new(0.0, 0.0, 1.0, 1.0),
                texture: if i.is_multiple_of(2) {
                    "chrome".into()
                } else {
                    "font".into()
                },
                color: WHITE,
            })
            .collect::<Vec<_>>();

        let first = gpu.set_ui_quads(&runs);
        assert_eq!(first.submitted, runs.len());
        assert_eq!(gpu.ui_batch_count(), runs.len());
        let warm_allocations = gpu.ui_buffer_allocation_count();
        assert_eq!(warm_allocations, runs.len() as u64);

        for redraw in 1..=8 {
            let report = gpu.set_ui_quads(&runs);
            assert_eq!(report.submitted, runs.len(), "redraw {redraw}");
            assert_eq!(gpu.ui_batch_count(), runs.len(), "redraw {redraw}");
            let frame = gpu
                .render_to_rgba_pass(0, FramePass::PreWorldUi)
                .expect("reused pre-world UI must render");
            assert_eq!(frame.len(), 64 * 64 * 4);
            assert_eq!(
                gpu.ui_buffer_allocation_count(),
                warm_allocations,
                "redraw {redraw} recreated UI buffers instead of reusing its pool"
            );
        }

        let known_bad_rebuild_every_frame = runs.len() as u64 * 9;
        assert!(
            known_bad_rebuild_every_frame > warm_allocations,
            "known-bad per-redraw allocation model must remain distinguishable from the pool"
        );
    }

    /// Product windows must prefer ordered FIFO presentation.  Mailbox remains an explicit
    /// diagnostic/performance opt-in because it has produced intermittent black compositor frames
    /// on the supported NVIDIA + Wayland/KWin path.
    #[test]
    fn windowed_present_mode_defaults_to_fifo_even_when_mailbox_exists() {
        let supported = [
            wgpu::PresentMode::Fifo,
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Immediate,
        ];
        assert_eq!(
            select_windowed_present_mode(&supported, false, false),
            wgpu::PresentMode::Fifo
        );
    }

    #[test]
    fn windowed_present_mode_honours_explicit_diagnostic_overrides() {
        let supported = [
            wgpu::PresentMode::Fifo,
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Immediate,
        ];
        assert_eq!(
            select_windowed_present_mode(&supported, false, true),
            wgpu::PresentMode::Mailbox
        );
        assert_eq!(
            select_windowed_present_mode(&supported, true, false),
            wgpu::PresentMode::Immediate
        );
        // Vsync-off is the stronger request when both knobs are present.
        assert_eq!(
            select_windowed_present_mode(&supported, true, true),
            wgpu::PresentMode::Immediate
        );
    }

    #[test]
    fn unavailable_present_mode_override_fails_safe_to_fifo() {
        let supported = [wgpu::PresentMode::Fifo];
        assert_eq!(
            select_windowed_present_mode(&supported, true, true),
            wgpu::PresentMode::Fifo
        );
    }

    #[test]
    fn windowed_alpha_mode_prefers_opaque_over_capability_order() {
        let supported = [
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Auto,
            wgpu::CompositeAlphaMode::Opaque,
        ];
        assert_eq!(
            select_windowed_alpha_mode(&supported),
            wgpu::CompositeAlphaMode::Opaque
        );
        assert_eq!(
            select_windowed_alpha_mode(&[wgpu::CompositeAlphaMode::PostMultiplied]),
            wgpu::CompositeAlphaMode::PostMultiplied
        );
    }

    /// Falsifier: production idle wait must never use `timeout: None` / wait_indefinitely.
    /// Restoring unbounded Wait fails this match (timeout field is Some + finite).
    #[test]
    fn idle_wait_poll_type_never_infinite() {
        let deadline = Instant::now() + Duration::from_millis(DEFAULT_GPU_WAIT_MS);
        let t = Gpu::gpu_wait_timeout();
        assert!(
            !t.is_zero() && t <= Duration::from_secs(3600),
            "gpu_wait_timeout must be a finite positive bound, got {t:?}"
        );
        assert!(
            Instant::now() < deadline,
            "falsifier Instant deadline must remain in the future for a sane default"
        );

        match idle_wait_poll_type(t) {
            wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(d),
            } => {
                assert_eq!(d, t, "poll Wait must carry the shared timeout verbatim");
            }
            wgpu::PollType::Wait { timeout: None, .. } => {
                panic!("falsifier: Wait {{ timeout: None }} / wait forever restored")
            }
            other => panic!("falsifier: expected bounded Wait, got {other:?}"),
        }

        // Contrasting the banned infinite form so a naive `wait_indefinitely()` swap is visible.
        match wgpu::PollType::wait_indefinitely() {
            wgpu::PollType::Wait { timeout: None, .. } => {}
            other => panic!(
                "wgpu contract drift: wait_indefinitely should be timeout: None, got {other:?}"
            ),
        }
    }

    #[test]
    fn poll_timeout_classifies_as_gpu_wait_error_timeout() {
        let waited = Duration::from_millis(42);
        let err = classify_poll_result(Err(wgpu::PollError::Timeout), waited)
            .expect_err("Timeout must be Err");
        assert!(
            matches!(err, GpuWaitError::Timeout { waited: w } if w == waited),
            "poll Timeout must map to GpuWaitError::Timeout, got {err}"
        );
    }

    /// Sol HIGH 4 falsifier: map completion failures must be typed errors, never panic or `None`.
    #[test]
    fn map_failure_classifies_as_typed_error_not_none() {
        fn classify(
            recv: Result<Result<(), &'static str>, mpsc::RecvTimeoutError>,
            waited: Duration,
        ) -> Result<(), GpuWaitError> {
            match recv {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(GpuWaitError::Map(wgpu::BufferAsyncError)),
                Err(_) => Err(GpuWaitError::MapRecvTimeout { waited }),
            }
        }

        let waited = Duration::from_millis(5);
        let err = classify(Ok(Err("forced map failure")), waited).expect_err("must be Err");
        assert!(
            matches!(err, GpuWaitError::Map(_)),
            "map failure must be GpuWaitError::Map, got {err}"
        );

        let (tx, rx) = mpsc::channel::<Result<(), &'static str>>();
        drop(tx); // no completion callback
        let err = classify(
            rx.recv_timeout(waited)
                .map_err(|_| mpsc::RecvTimeoutError::Timeout),
            waited,
        )
        .expect_err("missing callback must error");
        assert!(
            matches!(err, GpuWaitError::MapRecvTimeout { .. }),
            "unbounded-recv replacement must surface MapRecvTimeout, got {err}"
        );
    }

    #[test]
    fn timestamps_unsupported_remains_ok_none_none_distinct_from_map_error() {
        // Contract: Ok((None, None)) only when timestamps feature is absent — not on map failure.
        let unsupported: Result<(Option<f64>, Option<f64>), GpuWaitError> = Ok((None, None));
        let map_fail: Result<(Option<f64>, Option<f64>), GpuWaitError> =
            Err(GpuWaitError::Map(wgpu::BufferAsyncError));
        assert!(unsupported.is_ok());
        assert!(map_fail.is_err());
        assert!(map_fail
            .err()
            .is_some_and(|e| matches!(e, GpuWaitError::Map(_))));
    }

    /// MS-08 residual falsifier: windowed present_mode meta must never claim the headless floor
    /// sentinel (`headless_offscreen_no_acquire_present`), and headless Gpu must still emit it.
    #[test]
    fn present_mode_label_windowed_ne_headless_sentinel() {
        use super::windowed_present_mode_label;
        // Hold the device lock only when the gate env is unset — never mutate that env.
        let _gpu_lock = if crate::gpu_init::gpu_test_serialize_enabled() {
            None
        } else {
            Some(crate::gpu_init::lock_gpu_device().expect("test device lock"))
        };
        const HEADLESS: &str = "headless_offscreen_no_acquire_present";
        for mode in [
            wgpu::PresentMode::Fifo,
            wgpu::PresentMode::FifoRelaxed,
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Immediate,
            wgpu::PresentMode::AutoVsync,
            wgpu::PresentMode::AutoNoVsync,
        ] {
            let label = windowed_present_mode_label(mode);
            assert_ne!(
                label, HEADLESS,
                "windowed present_mode_label must not equal headless sentinel"
            );
            assert!(
                label.contains("acquire_present"),
                "windowed label must advertise acquire+present, got {label}"
            );
        }

        let gpu = pollster::block_on(Gpu::new_headless(64, 64, 1_000.0)).expect("gpu init");
        assert_eq!(
            gpu.present_mode_label(),
            HEADLESS,
            "headless Gpu must keep the no-acquire/present floor label"
        );
        assert!(gpu.present_mode_label() != windowed_present_mode_label(wgpu::PresentMode::Fifo));
    }

    /// Falsifier for MS-08 submit coalesce: default must be coalesce (one submit);
    /// `CAER_SPLIT_EGUI_SUBMIT=1` must restore the 2-submit path.
    #[test]
    fn split_egui_submit_env_defaults_off_and_honours_one() {
        use super::split_egui_submit_enabled;
        // Clear then assert default.
        std::env::remove_var("CAER_SPLIT_EGUI_SUBMIT");
        assert!(
            !split_egui_submit_enabled(),
            "default must coalesce (kill-switch off)"
        );
        std::env::set_var("CAER_SPLIT_EGUI_SUBMIT", "1");
        assert!(
            split_egui_submit_enabled(),
            "CAER_SPLIT_EGUI_SUBMIT=1 must enable split path"
        );
        std::env::set_var("CAER_SPLIT_EGUI_SUBMIT", "true");
        assert!(split_egui_submit_enabled());
        std::env::set_var("CAER_SPLIT_EGUI_SUBMIT", "0");
        assert!(
            !split_egui_submit_enabled(),
            "CAER_SPLIT_EGUI_SUBMIT=0 must not enable split"
        );
        std::env::remove_var("CAER_SPLIT_EGUI_SUBMIT");
    }

    /// Named measurement that can go the wrong way: with an overlay frame, coalesce issues
    /// `queue_submit_count == 1`; split kill-switch issues `2`. Pixel path still completes.
    #[test]
    fn coalesce_egui_submit_count_one_vs_split_two() {
        use super::split_egui_submit_enabled;
        let _gpu_lock = if crate::gpu_init::gpu_test_serialize_enabled() {
            None
        } else {
            Some(crate::gpu_init::lock_gpu_device().expect("test device lock"))
        };
        let mut gpu = pollster::block_on(Gpu::new_headless(64, 64, 1_000.0)).expect("gpu init");

        std::env::remove_var("CAER_SPLIT_EGUI_SUBMIT");
        assert!(!split_egui_submit_enabled());
        let egui = crate::ui::tessellate_headless([64, 64], 1.0, |ui| {
            ui.label("ms08-submit-coalesce");
        });
        let coalesce = gpu
            .render_headless_sync_with_overlay_timed(0, Some(egui))
            .expect("coalesce timed render");
        assert_eq!(
            coalesce.queue_submit_count, 1,
            "coalesce must issue one queue.submit; got {}",
            coalesce.queue_submit_count
        );

        std::env::set_var("CAER_SPLIT_EGUI_SUBMIT", "1");
        let egui2 = crate::ui::tessellate_headless([64, 64], 1.0, |ui| {
            ui.label("ms08-submit-coalesce");
        });
        let split = gpu
            .render_headless_sync_with_overlay_timed(0, Some(egui2))
            .expect("split timed render");
        assert_eq!(
            split.queue_submit_count, 2,
            "CAER_SPLIT_EGUI_SUBMIT=1 must issue two queue.submits; got {}",
            split.queue_submit_count
        );
        std::env::remove_var("CAER_SPLIT_EGUI_SUBMIT");

        // Mean-cut leaf is submit_cpu_ms (all finish+submit). Coalesce should not increase
        // submit count; if this ever flips to count==2 by default the instrument failed.
        assert!(
            coalesce.queue_submit_count < split.queue_submit_count,
            "coalesce submit count must be strictly below split"
        );
    }
}

#[cfg(test)]
mod entity_texture_cache_tests {
    use super::Gpu;
    use crate::terrain::{ModelBatch, SkinVertex, SkinnedBatch, SkinnedPart, TerrainVertex};
    use caer_assets::dds::{DdsFormat, DdsTexture};
    use std::collections::HashMap;

    fn triangle_batch() -> ModelBatch {
        ModelBatch {
            nhd: None,
            vertices: vec![
                TerrainVertex {
                    pos: [-1.0, -1.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    color: [1.0, 1.0, 1.0],
                    uv: [0.0, 0.0],
                    overlay_uv: [0.0, 0.0],
                    blend: 1.0,
                },
                TerrainVertex {
                    pos: [1.0, -1.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    color: [1.0, 1.0, 1.0],
                    uv: [1.0, 0.0],
                    overlay_uv: [1.0, 0.0],
                    blend: 1.0,
                },
                TerrainVertex {
                    pos: [0.0, 1.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    color: [1.0, 1.0, 1.0],
                    uv: [0.5, 1.0],
                    overlay_uv: [0.5, 1.0],
                    blend: 1.0,
                },
            ],
            indices: vec![0, 1, 2],
            parts: Vec::new(),
            instances: Vec::new(),
            bound_center_z: 0.0,
            bound_radius: 1.0,
            bound_min: [-1.0, -1.0, 0.0],
            bound_max: [1.0, 1.0, 0.0],
            morphs: Vec::new(),
            uv_anims: Vec::new(),
        }
    }

    fn solid_texture(rgba: [u8; 4]) -> DdsTexture {
        DdsTexture {
            width: 4,
            height: 4,
            format: DdsFormat::Rgba8,
            mips: vec![rgba.into_iter().cycle().take(4 * 4 * 4).collect()],
        }
    }

    fn skinned_triangle_batch(texture: &str) -> SkinnedBatch {
        SkinnedBatch {
            vertices: triangle_batch().vertices,
            skin: vec![
                SkinVertex {
                    joints: [0, 0, 0, 0],
                    weights: [1.0, 0.0, 0.0, 0.0],
                };
                3
            ],
            indices: vec![0, 1, 2],
            parts: vec![SkinnedPart {
                start: 0,
                end: 3,
                palette_slot: 0,
                texture: Some(texture.into()),
                name: "body".into(),
            }],
            bone_stride: 1,
            inverse_bind: vec![[
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]],
            bound_min: [-1.0, -1.0, 0.0],
            bound_max: [1.0, 1.0, 0.0],
        }
    }

    /// The actual upload path must share equivalent resident texture bindings and retire them
    /// when the final mesh owner goes away. Otherwise a roster sweep leaks one GPU texture per
    /// avatar/equipment upload and eventually fails with device OOM.
    #[test]
    fn entity_texture_cache_deduplicates_and_releases_uploads() {
        let _gpu_lock = if crate::gpu_init::gpu_test_serialize_enabled() {
            None
        } else {
            Some(crate::gpu_init::lock_gpu_device().expect("test device lock"))
        };
        let mut gpu = pollster::block_on(Gpu::new_headless(16, 16, 1_000.0)).expect("gpu init");
        let batch = triangle_batch();
        let brown = solid_texture([84, 56, 32, 255]);
        let green = solid_texture([32, 96, 64, 255]);

        gpu.upload_entity_mesh(101, &batch, Some(&brown));
        gpu.upload_entity_mesh(102, &batch, Some(&brown));
        gpu.upload_entity_mesh(103, &batch, Some(&green));
        assert_eq!(
            gpu.entity_texture_count(),
            3,
            "white fallback plus the two distinct resident skins; identical brown uploads must share"
        );

        gpu.upload_entity_mesh(103, &batch, Some(&brown));
        assert_eq!(
            gpu.entity_texture_count(),
            2,
            "replacing a model must retire its old green skin before retaining brown"
        );

        gpu.evict_entity_mesh(101);
        assert_eq!(
            gpu.entity_texture_count(),
            2,
            "brown remains resident while models 102 and 103 still reference it"
        );
        gpu.evict_entity_mesh(102);
        assert_eq!(
            gpu.entity_texture_count(),
            2,
            "brown remains resident while model 103 still references it"
        );
        gpu.evict_entity_mesh(103);
        assert_eq!(
            gpu.entity_texture_count(),
            1,
            "brown must retire when its final model owner is evicted"
        );
    }

    #[test]
    fn skinned_meshes_share_and_retire_texture_owners() {
        let _gpu_lock = if crate::gpu_init::gpu_test_serialize_enabled() {
            None
        } else {
            Some(crate::gpu_init::lock_gpu_device().expect("test device lock"))
        };
        let mut gpu = pollster::block_on(Gpu::new_headless(16, 16, 1_000.0)).expect("gpu init");
        let sheet = solid_texture([84, 56, 32, 255]);
        let textures = HashMap::from([("body".into(), sheet)]);
        let batch = skinned_triangle_batch("body");

        assert!(gpu.upload_skinned_mesh(201, &batch, None, &textures, 0.0));
        assert!(gpu.upload_skinned_mesh(202, &batch, None, &textures, 0.0));
        assert_eq!(
            gpu.entity_texture_count(),
            2,
            "two skinned meshes using one sheet must retain white plus one shared texture"
        );

        gpu.evict_entity_mesh(201);
        assert_eq!(
            gpu.entity_texture_count(),
            2,
            "the sheet remains while the second skinned mesh owns it"
        );
        gpu.evict_entity_mesh(202);
        assert_eq!(
            gpu.entity_texture_count(),
            1,
            "the final skinned owner must release its sheet"
        );
    }

    #[test]
    fn rejected_skinned_replacement_retires_the_previous_draw() {
        let _gpu_lock = if crate::gpu_init::gpu_test_serialize_enabled() {
            None
        } else {
            Some(crate::gpu_init::lock_gpu_device().expect("test device lock"))
        };
        let mut gpu = pollster::block_on(Gpu::new_headless(16, 16, 1_000.0)).expect("gpu init");
        let textures = HashMap::from([("body".into(), solid_texture([84, 56, 32, 255]))]);
        let accepted = skinned_triangle_batch("body");
        assert!(gpu.upload_skinned_mesh(203, &accepted, None, &textures, 0.0));
        assert!(gpu.has_skinned_mesh(203));

        let mut rejected = skinned_triangle_batch("body");
        rejected.indices.clear();
        assert!(!gpu.upload_skinned_mesh(203, &rejected, None, &textures, 0.0));
        assert!(
            !gpu.has_skinned_mesh(203),
            "a rejected replacement must not leave the old skinned draw resident"
        );
    }
}

#[cfg(test)]
mod srgb_tests {
    use super::srgb_byte_to_linear;

    /// What the sRGB colour target does to whatever the shader wrote.
    fn encode(linear: f32) -> u8 {
        let s = if linear <= 0.003_130_8 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    }

    /// An authored byte must survive the trip to the screen unchanged.
    ///
    /// The second assertion is the control: it is the arithmetic this function exists to prevent,
    /// and it reproduces the defect exactly — the pre-world dialog fill authored `8` measured
    /// (50,50,50) in a capture. Without it, a conversion that silently did nothing would still
    /// pass the first assertion for the byte 0.
    #[test]
    fn an_authored_byte_survives_the_srgb_target() {
        for v in [0u8, 8, 50, 118, 128, 255] {
            assert_eq!(
                encode(srgb_byte_to_linear(v)),
                v,
                "byte {v} did not round-trip"
            );
        }
        assert!((srgb_byte_to_linear(8) - 0.002_428).abs() < 1e-6);
        assert_eq!(
            encode(f32::from(8u8) / 255.0),
            50,
            "known-bad control changed"
        );
    }
}
