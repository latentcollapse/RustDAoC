//! Dungeon render path — `dungeon.chunk/.place/.prop` → real Dnifs NIF meshes (+ FixtureBox fallback).
//!
//! Surface `load_region` correctly reports classic dungeon region 20 as empty (no `terrain.pcx`).
//! That is **not** dungeon coverage. This module is the product path: decode placements, resolve
//! NIF models under `zones/Dnifs` (and the shared model roots), place textured/diffuse geometry
//! into [`TerrainMesh::models`], and only emit [`FixtureBox`] when a chunk fails to parse or is
//! missing. FixtureBoxes remain a **diagnostic fallback**, never a completion claim.
//!
//! ## Canonical classic dungeon (Wave1 Lane D)
//!
//! | Field | Value |
//! |---|---|
//! | Region | **20** |
//! | Zone | **19** — Stonehenge Barrows |
//! | Surface return | region **1** (Camelot Hills) |
//!
//! Do **not** substitute region 51 (city/interior with `terrain.pcx`) and call it region 20.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use glam::Vec3;

use caer_world::dungeon_zones;
use caer_world::ZONE_UNIT;

use crate::terrain::{
    fixture_global_yaw_k_radians, fixture_render_rot, quat_axis_angle, quat_mul, untextured_batch,
    FixtureBox, ModelBatch, ModelInstance, ModelPartRange, TerrainMesh, TerrainVertex,
};

/// Canonical classic dungeon region — Stonehenge Barrows (zone 19).
pub const CANONICAL_DUNGEON_REGION: u16 = 20;
/// Zone id inside [`CANONICAL_DUNGEON_REGION`].
pub const CANONICAL_DUNGEON_ZONE: u16 = 19;
/// Human label (matches `zone_names` / Zones.xml).
pub const CANONICAL_DUNGEON_LABEL: &str = "Stonehenge Barrows";
/// Surface region used for the return half of the product scenario.
pub const CANONICAL_SURFACE_REGION: u16 = 1;
/// Deterministic Camelot Hills seat for surface half / return clear (matches rustdaoc screenshot default).
pub const CANONICAL_SURFACE_SEAT: [f32; 3] = [592_250.0, 537_900.0, 1_750.0];

/// Geometry fingerprint for one dungeon zone (render-consumer side).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DungeonFingerprint {
    pub zone_id: u16,
    pub chunk_palette: usize,
    pub fixture_boxes: usize,
    /// Distinct NIF models that parsed and received at least one instance.
    pub model_kinds: usize,
    /// Total placed NIF instances (real geometry).
    pub model_instances: usize,
}

impl DungeonFingerprint {
    /// True when the load produced real mesh instances (not boxes-only).
    #[must_use]
    pub fn has_real_geometry(&self) -> bool {
        self.model_instances > 0 && self.model_kinds > 0
    }
}

/// Stable semantic scene fingerprint for golden / scenario binding (not a pixel hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DungeonSceneFingerprint {
    pub region: u16,
    pub zone_id: u16,
    pub label: &'static str,
    pub chunk_palette: usize,
    pub model_kinds: usize,
    pub model_instances: usize,
    pub fixture_box_fallback: usize,
    pub total_vertices: usize,
    pub total_triangles: usize,
    /// FNV-1a over counts plus vertex payloads, instance transforms, and part ranges.
    pub content_hash: u64,
}

/// Result of integrating dungeon content into a mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DungeonLoadStats {
    pub zones: usize,
    pub model_instances: usize,
    pub model_kinds: usize,
    pub fixture_boxes: usize,
}

impl DungeonLoadStats {
    #[must_use]
    pub fn has_real_geometry(&self) -> bool {
        self.model_instances > 0
    }
}

/// World-space seat for the canonical dungeon product camera/player.
///
/// Derived from zone-19 grid offset + the authored `bdung1hall1_trans_mine` placement (transition
/// piece near the entrance). Recorded so screenshots and the product scenario share one origin.
#[must_use]
pub fn canonical_dungeon_seat() -> [f32; 3] {
    // Zone 19 offset (1,1); transition place local ≈ (-522.5, 1042.3, 256) from dat019.
    // Keep literals explicit so a placement drift fails loudly in the scenario fingerprint test.
    let (ox, oy) = (1_i32, 1_i32);
    let local = [-522.5_f32, 1042.3, 256.0];
    [
        ox as f32 * ZONE_UNIT as f32 + local[0],
        oy as f32 * ZONE_UNIT as f32 + local[1],
        local[2],
    ]
}

/// Product camera/player seat for any dungeon zone.
///
/// Canonical Stonehenge Barrows keeps the recorded entrance seat. Other zones use the first
/// authored place/prop (table-derived). Empty zones fall back to grid origin + 256 local — those
/// rows already fail census (`ok_empty`) and must not be treated as playable.
#[must_use]
pub fn dungeon_product_seat(client_root: &Path, zone_id: u16) -> [f32; 3] {
    if zone_id == CANONICAL_DUNGEON_ZONE {
        return canonical_dungeon_seat();
    }
    if let Ok(zone) = dungeon_zones::load_dungeon_zone(client_root, zone_id) {
        if let Some(p) = zone.places.first().or(zone.props.first()) {
            let (wx, wy) = dungeon_world_xy(zone_id, p.x, p.y);
            return [wx, wy, p.z];
        }
    }
    let (ox, oy) = caer_world::zone_grid_offset(zone_id).unwrap_or((0, 0));
    [
        ox as f32 * ZONE_UNIT as f32 + 256.0,
        oy as f32 * ZONE_UNIT as f32 + 256.0,
        256.0,
    ]
}

/// True when the mesh already carries dungeon (or leftover) placed geometry.
#[must_use]
pub fn mesh_has_dungeon_geometry(mesh: &TerrainMesh) -> bool {
    mesh.models.iter().any(|m| !m.instances.is_empty()) || !mesh.fixtures.is_empty()
}

/// World XY for a dungeon placement given its zone grid offset.
#[must_use]
pub fn dungeon_world_xy(zone_id: u16, local_x: f32, local_y: f32) -> (f32, f32) {
    let (ox, oy) = caer_world::zone_grid_offset(zone_id).unwrap_or((0, 0));
    (
        ox as f32 * ZONE_UNIT as f32 + local_x,
        oy as f32 * ZONE_UNIT as f32 + local_y,
    )
}

/// Load a dungeon zone and build placeholder fixture boxes in **render space** (`world − origin`,
/// Y mirrored) — diagnostic fallback path only.
pub fn load_dungeon_fixture_boxes(
    client_root: &Path,
    zone_id: u16,
    origin: Vec3,
) -> std::io::Result<(DungeonFingerprint, Vec<FixtureBox>)> {
    let zone = dungeon_zones::load_dungeon_zone(client_root, zone_id)?;
    let fixtures = zone.as_fixtures();
    let mut boxes = Vec::with_capacity(fixtures.len());
    for f in &fixtures {
        let (wx, wy) = dungeon_world_xy(zone_id, f.x, f.y);
        let half = (f.radius * f.scale / 100.0).clamp(30.0, 1500.0);
        let name = f.name.to_ascii_lowercase();
        let is_tree = ["tree", "elm", "oak", "pine", "bush", "shrub"]
            .iter()
            .any(|k| name.contains(k));
        let rp = [wx - origin.x, -(wy - origin.y), f.z - origin.z];
        boxes.push(FixtureBox {
            pos: [rp[0], rp[1], rp[2] + half],
            half,
            is_tree,
        });
    }
    Ok((
        DungeonFingerprint {
            zone_id,
            chunk_palette: zone.chunks.len(),
            fixture_boxes: boxes.len(),
            model_kinds: 0,
            model_instances: 0,
        },
        boxes,
    ))
}

fn model_dirs(client_root: &Path) -> Vec<PathBuf> {
    [
        "zones/Dnifs",
        "zones/Nifs",
        "zones/trees",
        "frontiers/dnifs",
        "frontiers/NIFS",
        "phousing/nifs",
    ]
    .iter()
    .map(|s| client_root.join(s))
    .collect()
}

fn build_npk_dds_index(client_root: &Path) -> (HashMap<String, PathBuf>, HashMap<String, PathBuf>) {
    let mut npk_index: HashMap<String, PathBuf> = HashMap::new();
    let mut dds_index: HashMap<String, PathBuf> = HashMap::new();
    for dir in model_dirs(client_root) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if let Some(stem) = name.strip_suffix(".npk") {
                npk_index
                    .entry(stem.to_string())
                    .or_insert_with(|| e.path());
            } else if name.ends_with(".dds") {
                dds_index.entry(name).or_insert_with(|| e.path());
            }
        }
    }
    (npk_index, dds_index)
}

/// The `.tga` beside a texture the NIF asked for as `.dds`.
///
/// NIFs name their textures `.dds` whatever actually shipped, and a good share of the client's
/// art is still TGA — `pregame/mushroom01.tga` and `pregame/Winter_bannerpole-SNOW.tga` are two
/// the character screens want. Indexing them and then only ever looking up the `.dds` key made
/// those files unreachable, so they bound the white fallback instead.
fn load_tga_page(
    index: &HashMap<String, PathBuf>,
    dds_key: &str,
) -> Option<caer_assets::dds::DdsTexture> {
    let stem = dds_key.strip_suffix(".dds")?;
    let img = index
        .get(&format!("{stem}.tga"))
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| caer_assets::tga::decode(&b).ok())?;
    Some(caer_assets::dds::DdsTexture {
        width: img.width,
        height: img.height,
        format: caer_assets::dds::DdsFormat::Rgba8,
        mips: vec![img.rgba],
    })
}

/// Resolve a NIF part's texture reference to a loaded page key, caching hits and misses.
pub(crate) fn load_model_texture(
    dds_index: &HashMap<String, PathBuf>,
    reference: &str,
    textures: &mut HashMap<String, caer_assets::dds::DdsTexture>,
    failed: &mut HashSet<String>,
) -> Option<String> {
    let base = reference
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(reference)
        .to_ascii_lowercase();
    let key = match base.rsplit_once('.') {
        Some((stem, ext)) if ext != "dds" => format!("{stem}.dds"),
        Some(_) => base,
        None => format!("{base}.dds"),
    };
    if textures.contains_key(&key) {
        return Some(key);
    }
    if failed.contains(&key) {
        return None;
    }
    let loaded = dds_index
        .get(&key)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| caer_assets::dds::read_model_dds(&b).ok())
        .or_else(|| load_tga_page(dds_index, &key));
    match loaded {
        Some(t) => {
            textures.insert(key.clone(), t);
            Some(key)
        }
        None => {
            failed.insert(key);
            None
        }
    }
}

fn nif_batch_from_path(
    path: &Path,
    dds_index: &HashMap<String, PathBuf>,
    textures: &mut HashMap<String, caer_assets::dds::DdsTexture>,
    tex_failed: &mut HashSet<String>,
) -> Option<ModelBatch> {
    let nif =
        caer_assets::open_first(path, |n| n.to_ascii_lowercase().ends_with(".nif")).ok()??;
    if nif.data.len() < 300 {
        return None;
    }
    let model = match caer_assets::nif::read_model(&nif.data) {
        Ok(m) => m,
        Err(e) if e.to_string().starts_with("no visible geometry") => return None,
        Err(_) => return None,
    };
    let nhd_path = path.with_extension("nhd");
    let nhd = std::fs::read(&nhd_path)
        .ok()
        .or_else(|| {
            let dir = nhd_path.parent()?;
            let want = nhd_path.file_name()?.to_string_lossy().to_ascii_lowercase();
            std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
                (e.file_name().to_string_lossy().to_ascii_lowercase() == want)
                    .then(|| std::fs::read(e.path()).ok())
                    .flatten()
            })
        })
        .and_then(|b| caer_assets::nhd::parse(&b).ok())
        .map(std::sync::Arc::new);

    // Prefer textured merge (same grouping as surface fixtures); fall back to untextured batch.
    let keys: Vec<Option<String>> = model
        .parts
        .iter()
        .map(|p| {
            p.texture
                .as_deref()
                .and_then(|t| load_model_texture(dds_index, t, textures, tex_failed))
        })
        .collect();
    if keys.iter().all(|k| k.is_none()) {
        let mut batch = untextured_batch(&model);
        batch.nhd = nhd;
        return Some(batch);
    }

    Some(batch_from_model(&model, &keys, &[], nhd))
}

/// Merge a parsed NIF's parts into one batch, grouped by texture page.
///
/// Extracted from the dungeon path so the pre-world character-screen scenes build geometry the same
/// way instead of growing a second copy of it.
/// `keys2` is the resolved SECOND texture layer per part, parallel to `keys`. Pass an empty slice
/// when the caller does not resolve one — every part then draws single-layered, exactly as before.
pub(crate) fn batch_from_model(
    model: &caer_assets::nif::Model,
    keys: &[Option<String>],
    keys2: &[Option<String>],
    nhd: Option<std::sync::Arc<caer_assets::nhd::NhdGrid>>,
) -> ModelBatch {
    let mut batch = ModelBatch {
        nhd,
        vertices: Vec::new(),
        indices: Vec::new(),
        parts: Vec::new(),
        instances: Vec::new(),
        bound_center_z: 0.0,
        bound_radius: 0.0,
        bound_min: [0.0; 3],
        bound_max: [0.0; 3],
        morphs: Vec::new(),
        uv_anims: Vec::new(),
    };
    // Opaque parts, then source-over, then additive, grouped by texture within each. `AlphaMode`
    // orders exactly that way on purpose. The mode has to come first in the sort key: a blended
    // part sharing a texture with an opaque one must not merge into its range, or the whole range
    // gets drawn in the wrong pass.
    let mut order: Vec<usize> = (0..model.parts.len()).collect();
    order.sort_by(|&a, &b| (model.parts[a].alpha, &keys[a]).cmp(&(model.parts[b].alpha, &keys[b])));
    for &pi in &order {
        let part = &model.parts[pi];
        let key = &keys[pi];
        // A second layer only exists once BOTH the asset names one and it resolved to a decoded
        // texture. A named-but-missing sheet must fall back to single-layer rather than blending
        // the part against the white fallback, which would wash it out.
        let key2 = keys2.get(pi).cloned().flatten();
        let masked = key2.is_some();
        let texture2_mode = part.texture2_mode;
        let base = batch.vertices.len() as u32;
        let start = batch.indices.len() as u32;
        let color = if key.is_none() && part.alpha.is_blended() && part.diffuse == [1.0, 1.0, 1.0] {
            [0.35, 0.32, 0.28]
        } else {
            part.diffuse
        };
        for (i, &p) in part.positions.iter().enumerate() {
            let uv = part.uvs.get(i).copied().unwrap_or([0.0, 0.0]);
            batch.vertices.push(TerrainVertex {
                pos: p,
                normal: part.normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]),
                color,
                uv,
                // The base map's UV controller may move `uv`; Decal 0 must keep the authored
                // coordinate. `mesh.wgsl` selects this only for the OverlayAlpha sentinel.
                overlay_uv: uv,
                // 1.0 unless this part really is two-layered: the shader mixes unconditionally,
                // so a stray mask on a single-layer part would blend it against itself at best
                // and against the white fallback at worst.
                blend: if masked {
                    match texture2_mode {
                        caer_assets::nif::TextureLayerMode::VertexAlpha => {
                            part.colors.get(i).map_or(1.0, |c| c[3])
                        }
                        // Negative is outside the legitimate 0..1 vertex-mask domain and is the
                        // compact model-vertex selector consumed by mesh.wgsl. It says: sample
                        // texture2 as an RGBA Decal 0 overlay, not as a ground-style mask.
                        caer_assets::nif::TextureLayerMode::OverlayAlpha => -1.0,
                    }
                } else {
                    1.0
                },
            });
        }
        if let Some(anim) = part.uv_anim.clone() {
            batch.uv_anims.push(crate::terrain::UvBinding {
                base_vertex: base,
                rest_uv: (0..part.positions.len())
                    .map(|i| part.uvs.get(i).copied().unwrap_or([0.0, 0.0]))
                    .collect(),
                anim,
            });
        }
        // Bind the vertex animation to where this part actually landed in the merged buffer.
        // `base` is that offset and it is only correct here, before the next part appends.
        if let Some(anim) = part.morph.clone().filter(|anim| anim.is_animated()) {
            batch.morphs.push(crate::terrain::MorphBinding {
                base_vertex: base,
                rest: part.positions.clone(),
                anim,
            });
        }
        batch.indices.extend(part.indices.iter().map(|&i| base + i));
        let end = batch.indices.len() as u32;
        match batch.parts.last_mut() {
            // The second layer joins the merge key. Two parts sharing a base sheet but blending
            // against different second layers are different draws, and folding them into one
            // range would give one of them the other's stone.
            Some(r)
                if r.texture == *key
                    && r.texture2 == key2
                    && r.texture2_mode == texture2_mode
                    && r.alpha == part.alpha =>
            {
                r.end = end;
            }
            _ => batch.parts.push(ModelPartRange {
                start,
                end,
                texture: key.clone(),
                alpha: part.alpha,
                texture2: key2,
                texture2_mode,
            }),
        }
    }
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in &batch.vertices {
        for a in 0..3 {
            lo[a] = lo[a].min(v.pos[a]);
            hi[a] = hi[a].max(v.pos[a]);
        }
    }
    if !batch.vertices.is_empty() {
        batch.bound_center_z = (lo[2] + hi[2]) * 0.5;
        let half = [
            (hi[0] - lo[0]) * 0.5,
            (hi[1] - lo[1]) * 0.5,
            (hi[2] - lo[2]) * 0.5,
        ];
        batch.bound_radius = (half[0] * half[0] + half[1] * half[1] + half[2] * half[2]).sqrt();
        batch.bound_min = lo;
        batch.bound_max = hi;
    }
    batch
}

/// Shared NIF/NPK index for repeated dungeon appends (census / multi-representative reload).
/// Templates are cloned into each mesh; do not share a cache across concurrent meshes.
pub struct DungeonNifCache {
    npk_index: HashMap<String, PathBuf>,
    dds_index: HashMap<String, PathBuf>,
    templates: HashMap<String, Option<ModelBatch>>,
    textures: HashMap<String, caer_assets::dds::DdsTexture>,
}

impl DungeonNifCache {
    #[must_use]
    pub fn new(client_root: &Path) -> Self {
        let (npk_index, dds_index) = build_npk_dds_index(client_root);
        Self {
            npk_index,
            dds_index,
            templates: HashMap::new(),
            textures: HashMap::new(),
        }
    }

    fn copy_textures_into(&self, mesh: &mut TerrainMesh) {
        for (k, v) in &self.textures {
            mesh.textures.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }

    /// Resolve a NIF stem against shipped `Dnifs` (cached). `None` = missing/unparseable.
    pub fn template_for_stem(&mut self, stem: &str) -> Option<&ModelBatch> {
        if !self.templates.contains_key(stem) {
            let mut tex_failed = HashSet::new();
            let Some(path) = self.npk_index.get(stem).cloned() else {
                self.templates.insert(stem.to_string(), None);
                return None;
            };
            let parsed =
                nif_batch_from_path(&path, &self.dds_index, &mut self.textures, &mut tex_failed)
                    .map(|mut batch| {
                        batch.instances.clear();
                        batch
                    });
            self.templates.insert(stem.to_string(), parsed);
        }
        self.templates.get(stem).and_then(|t| t.as_ref())
    }
}

/// Typed INT hook: append dungeon geometry for every dungeon zone in `region` when the surface
/// mesh is empty. Same path as [`append_dungeon_geometry_if_surface_empty`] — not a per-dungeon
/// special renderer.
pub fn append_dungeon_for_region(
    mesh: &mut TerrainMesh,
    client_root: &Path,
    region: u16,
) -> DungeonLoadStats {
    append_dungeon_geometry_if_surface_empty(mesh, client_root, region)
}

/// Product terrain load used by `rustdaoc` (`load_terrain` and screenshot).
///
/// Surface `load_region` plus dungeon append when the surface mesh is empty. Tests this function
/// — not `include_str!` of the binary — so skip-append / dead wiring fails on geometry.
pub fn load_product_terrain(
    client_root: &Path,
    region: u16,
    origin: Vec3,
    min: [i32; 2],
    max: [i32; 2],
) -> (TerrainMesh, DungeonLoadStats) {
    let mut mesh =
        crate::terrain::load_region(region, origin, min, max, crate::terrain::seam_blend());
    let stats = append_dungeon_for_region(&mut mesh, client_root, region);
    (mesh, stats)
}

/// Typed INT hook: append dungeon geometry for a single zone via the generic region path.
pub fn append_dungeon_for_zone(
    mesh: &mut TerrainMesh,
    client_root: &Path,
    zone_id: u16,
) -> DungeonLoadStats {
    let mut cache = DungeonNifCache::new(client_root);
    append_dungeon_geometry_filtered(mesh, client_root, None, Some(zone_id), &mut cache)
}

/// Append real dungeon NIF geometry (and FixtureBox fallback) for every dungeon zone in `region`
/// when the surface mesh is empty. Rebuilds walkable surfaces when anything lands.
///
/// Returns stats. Surface regions with `zones_loaded > 0` are untouched (negative control).
pub fn append_dungeon_geometry_if_surface_empty(
    mesh: &mut TerrainMesh,
    client_root: &Path,
    region: u16,
) -> DungeonLoadStats {
    let mut cache = DungeonNifCache::new(client_root);
    append_dungeon_geometry_filtered(mesh, client_root, Some(region), None, &mut cache)
}

/// Generic append used by region, zone, and census. `region`/`only_zone` select which dungeon
/// archives to load; neither branch invents a second renderer.
pub fn append_dungeon_geometry_filtered(
    mesh: &mut TerrainMesh,
    client_root: &Path,
    region: Option<u16>,
    only_zone: Option<u16>,
    cache: &mut DungeonNifCache,
) -> DungeonLoadStats {
    if mesh.zones_loaded > 0 {
        return DungeonLoadStats {
            zones: 0,
            model_instances: 0,
            model_kinds: 0,
            fixture_boxes: 0,
        };
    }
    let mut model_cache: HashMap<String, Option<usize>> = HashMap::new();
    let mut tex_failed: HashSet<String> = HashSet::new();
    let mut empty_stubs: HashSet<String> = HashSet::new();
    let mut stats = DungeonLoadStats {
        zones: 0,
        model_instances: 0,
        model_kinds: 0,
        fixture_boxes: 0,
    };
    let global_k = fixture_global_yaw_k_radians();
    let origin = mesh.origin;

    let zone_ids = dungeon_zone_ids_for_append(region, only_zone);
    for zid in zone_ids {
        let zone = match dungeon_zones::load_dungeon_zone(client_root, zid) {
            Ok(z) => z,
            Err(e) => {
                log::debug!("caer-render: skip dungeon zone {zid} (region {region:?}): {e}");
                continue;
            }
        };
        if zone.fixture_count() == 0 {
            log::warn!("caer-render: dungeon zone {zid} loaded with 0 placements (Ok(empty))");
            continue;
        }
        stats.zones += 1;
        let fixtures = zone.as_fixtures();
        for f in &fixtures {
            let (wx, wy) = dungeon_world_xy(zid, f.x, f.y);
            let rp = [wx - origin.x, -(wy - origin.y), f.z - origin.z];
            let stem = f.filename.to_ascii_lowercase();
            let stem = stem.strip_suffix(".nif").unwrap_or(&stem).to_string();
            if empty_stubs.contains(&stem) {
                continue;
            }
            let slot = if let Some(&cached) = model_cache.get(&stem) {
                cached
            } else {
                let resolved = resolve_dungeon_template(cache, &stem, mesh, &mut tex_failed);
                if resolved.is_none() {
                    empty_stubs.insert(stem.clone());
                }
                model_cache.insert(stem.clone(), resolved);
                resolved
            };
            if let Some(idx) = slot {
                let rot = quat_mul(
                    quat_axis_angle([0.0, 0.0, 1.0], global_k),
                    fixture_render_rot(f.axis, f.angle_raw),
                );
                let yaw = f.angle + global_k;
                mesh.models[idx].instances.push(ModelInstance {
                    pos: rp,
                    base_pos: rp,
                    yaw,
                    base_yaw: f.angle,
                    rot,
                    base_rot: fixture_render_rot(f.axis, f.angle_raw),
                    scale: f.scale / 100.0,
                    zone_id: zid,
                    fixture_id: f.id,
                });
                stats.model_instances += 1;
            } else {
                // Missing or unparseable NIF → diagnostic FixtureBox fallback.
                let half = (f.radius * f.scale / 100.0).clamp(80.0, 1500.0);
                mesh.fixtures.push(FixtureBox {
                    pos: [rp[0], rp[1], rp[2] + half],
                    half,
                    is_tree: false,
                });
                stats.fixture_boxes += 1;
            }
        }
    }
    stats.model_kinds = mesh
        .models
        .iter()
        .filter(|m| !m.instances.is_empty())
        .count();
    if stats.model_instances > 0 || stats.fixture_boxes > 0 {
        mesh.surfaces = crate::walkable::SurfaceIndex::build(mesh);
    }
    stats
}

fn dungeon_zone_ids_for_append(region: Option<u16>, only_zone: Option<u16>) -> Vec<u16> {
    if let Some(zid) = only_zone {
        if let Some(region) = region {
            let mut ids: Vec<u16> = caer_world::region_zone_offsets(region)
                .into_iter()
                .map(|(id, _, _)| id)
                .filter(|&id| id == zid)
                .collect();
            if ids.is_empty() {
                ids.push(zid);
            }
            return ids;
        }
        return vec![zid];
    }
    let Some(region) = region else {
        return Vec::new();
    };
    caer_world::region_zone_offsets(region)
        .into_iter()
        .map(|(id, _, _)| id)
        .collect()
}

fn resolve_dungeon_template(
    cache: &mut DungeonNifCache,
    stem: &str,
    mesh: &mut TerrainMesh,
    tex_failed: &mut HashSet<String>,
) -> Option<usize> {
    if cache.templates.contains_key(stem) {
        cache.copy_textures_into(mesh);
        return cache.templates.get(stem).and_then(|existing| {
            existing.as_ref().map(|batch| {
                mesh.models.push(batch.clone());
                mesh.models.len() - 1
            })
        });
    }
    let Some(path) = cache.npk_index.get(stem) else {
        log::debug!("caer-render: dungeon model MISSING npk: {stem}");
        cache.templates.insert(stem.to_string(), None);
        return None;
    };
    let path = path.clone();
    match nif_batch_from_path(&path, &cache.dds_index, &mut cache.textures, tex_failed) {
        Some(batch) => {
            let mut stored = batch.clone();
            stored.instances.clear();
            cache.templates.insert(stem.to_string(), Some(stored));
            cache.copy_textures_into(mesh);
            mesh.models.push(batch);
            Some(mesh.models.len() - 1)
        }
        None => {
            cache.templates.insert(stem.to_string(), None);
            None
        }
    }
}

/// Backward-compatible wrapper: returns FixtureBox count only (diagnostic). Prefer
/// [`append_dungeon_geometry_if_surface_empty`] for product paths.
pub fn append_dungeon_fixtures_if_surface_empty(
    mesh: &mut TerrainMesh,
    client_root: &Path,
    region: u16,
) -> usize {
    let before_models: usize = mesh.models.iter().map(|m| m.instances.len()).sum();
    let before_boxes = mesh.fixtures.len();
    let stats = append_dungeon_geometry_if_surface_empty(mesh, client_root, region);
    let after_models: usize = mesh.models.iter().map(|m| m.instances.len()).sum();
    // Report total dungeon placements landed (real + fallback) so callers keep seeing nonzero.
    let _ = (before_models, after_models, stats);
    mesh.fixtures.len() - before_boxes + (after_models.saturating_sub(before_models))
}

fn fnv1a_mix(hash: &mut u64, v: u64) {
    *hash ^= v;
    *hash = hash.wrapping_mul(0x0100_0000_01b3);
}

fn fnv1a_f32(hash: &mut u64, v: f32) {
    fnv1a_mix(hash, u64::from(v.to_bits()));
}

/// Compute a semantic fingerprint for a loaded dungeon mesh (region 20 canonical).
#[must_use]
pub fn scene_fingerprint(mesh: &TerrainMesh, region: u16, zone_id: u16) -> DungeonSceneFingerprint {
    let mut rows: Vec<(String, usize, usize)> = mesh
        .models
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.instances.is_empty())
        .map(|(i, m)| (format!("model{i}"), m.instances.len(), m.vertices.len()))
        .collect();
    rows.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, n_inst, n_vert) in &rows {
        for b in name.as_bytes() {
            fnv1a_mix(&mut hash, u64::from(*b));
        }
        fnv1a_mix(&mut hash, *n_inst as u64);
        fnv1a_mix(&mut hash, *n_vert as u64);
    }
    for (i, m) in mesh.models.iter().enumerate() {
        fnv1a_mix(&mut hash, i as u64);
        for v in &m.vertices {
            for c in v.pos {
                fnv1a_f32(&mut hash, c);
            }
            for c in v.color {
                fnv1a_f32(&mut hash, c);
            }
            for c in v.uv {
                fnv1a_f32(&mut hash, c);
            }
        }
        for idx in &m.indices {
            fnv1a_mix(&mut hash, u64::from(*idx));
        }
        for p in &m.parts {
            fnv1a_mix(&mut hash, u64::from(p.start));
            fnv1a_mix(&mut hash, u64::from(p.end));
            if let Some(tex) = &p.texture {
                for b in tex.as_bytes() {
                    fnv1a_mix(&mut hash, u64::from(*b));
                }
            }
        }
        for inst in &m.instances {
            for c in inst.pos {
                fnv1a_f32(&mut hash, c);
            }
            for c in inst.rot {
                fnv1a_f32(&mut hash, c);
            }
            fnv1a_f32(&mut hash, inst.scale);
            fnv1a_mix(&mut hash, u64::from(inst.zone_id));
            fnv1a_mix(&mut hash, u64::from(inst.fixture_id));
        }
    }
    let total_vertices: usize = mesh.models.iter().map(|m| m.vertices.len()).sum();
    let total_triangles: usize = mesh.models.iter().map(|m| m.indices.len() / 3).sum();
    let model_instances: usize = mesh.models.iter().map(|m| m.instances.len()).sum();
    DungeonSceneFingerprint {
        region,
        zone_id,
        label: caer_world::zone_name(zone_id).unwrap_or(if zone_id == CANONICAL_DUNGEON_ZONE {
            CANONICAL_DUNGEON_LABEL
        } else {
            "dungeon"
        }),
        chunk_palette: 0,
        model_kinds: rows.len(),
        model_instances,
        fixture_box_fallback: mesh.fixtures.len(),
        total_vertices,
        total_triangles,
        content_hash: hash,
    }
}

/// Surface ↔ dungeon product reload (typed INT hook; not wired into rustdaoc).
///
/// Always clears stale geometry before the destination load. `append=false` is the named
/// falsifier arm: a representative must go red if dungeon append is deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DungeonProductReload {
    pub dungeon_region: u16,
    pub dungeon_zone: u16,
    pub append_invoked: bool,
    pub dungeon_stats: DungeonLoadStats,
    pub dungeon_fingerprint: DungeonSceneFingerprint,
    pub collision_present: bool,
    pub surface_zones_before: usize,
    pub surface_zones_after: usize,
    pub reentry_fingerprint: Option<DungeonSceneFingerprint>,
}

impl DungeonProductReload {
    /// Product PASS for this representative: real NIF, collision index, surface survived,
    /// re-entry matched, and append was actually invoked.
    ///
    /// FixtureBox leftovers are a census `mixed_boxes` Fail (not 281-playable). They do not
    /// invalidate skip-append / reload proof as long as real NIF geometry is present.
    #[must_use]
    pub fn representative_pass(&self) -> bool {
        self.append_invoked
            && self.dungeon_stats.has_real_geometry()
            && self.dungeon_stats.model_instances > 0
            && self.collision_present
            && self.surface_zones_before > 0
            && self.surface_zones_after > 0
            && self.reentry_fingerprint.as_ref().is_some_and(|fp| {
                fp.content_hash == self.dungeon_fingerprint.content_hash
                    && fp.model_instances == self.dungeon_fingerprint.model_instances
            })
    }
}

/// Run surface → dungeon → surface → dungeon re-entry on one mesh pair.
///
/// Boxes-only and skipped append cannot PASS.
pub fn product_reload_surface_dungeon(
    client_root: &Path,
    dungeon_region: u16,
    dungeon_zone: u16,
    append: bool,
) -> DungeonProductReload {
    let surface_origin = Vec3::new(CANONICAL_SURFACE_SEAT[0], CANONICAL_SURFACE_SEAT[1], 0.0);
    let surface = crate::terrain::load_region(
        CANONICAL_SURFACE_REGION,
        surface_origin,
        [
            CANONICAL_SURFACE_SEAT[0] as i32 - 12_000,
            CANONICAL_SURFACE_SEAT[1] as i32 - 12_000,
        ],
        [
            CANONICAL_SURFACE_SEAT[0] as i32 + 12_000,
            CANONICAL_SURFACE_SEAT[1] as i32 + 12_000,
        ],
        crate::terrain::seam_blend(),
    );
    let surface_zones_before = surface.zones_loaded;
    let seat = dungeon_product_seat(client_root, dungeon_zone);
    let mut dungeon = TerrainMesh {
        origin: Vec3::new(seat[0], seat[1], 0.0),
        ..TerrainMesh::default()
    };
    let dungeon_stats = if append {
        append_dungeon_for_region(&mut dungeon, client_root, dungeon_region)
    } else {
        DungeonLoadStats {
            zones: 0,
            model_instances: 0,
            model_kinds: 0,
            fixture_boxes: 0,
        }
    };
    let dungeon_fingerprint = scene_fingerprint(&dungeon, dungeon_region, dungeon_zone);
    let collision_present = !dungeon.surfaces.is_empty();

    clear_dungeon_state(&mut dungeon);
    let surface_return = crate::terrain::load_region(
        CANONICAL_SURFACE_REGION,
        surface_origin,
        [
            CANONICAL_SURFACE_SEAT[0] as i32 - 12_000,
            CANONICAL_SURFACE_SEAT[1] as i32 - 12_000,
        ],
        [
            CANONICAL_SURFACE_SEAT[0] as i32 + 12_000,
            CANONICAL_SURFACE_SEAT[1] as i32 + 12_000,
        ],
        crate::terrain::seam_blend(),
    );
    let surface_zones_after = surface_return.zones_loaded;

    let reentry_fingerprint = if append {
        let mut again = TerrainMesh {
            origin: Vec3::new(seat[0], seat[1], 0.0),
            ..TerrainMesh::default()
        };
        append_dungeon_for_region(&mut again, client_root, dungeon_region);
        Some(scene_fingerprint(&again, dungeon_region, dungeon_zone))
    } else {
        None
    };

    let _ = surface;
    DungeonProductReload {
        dungeon_region,
        dungeon_zone,
        append_invoked: append,
        dungeon_stats,
        dungeon_fingerprint,
        collision_present,
        surface_zones_before,
        surface_zones_after,
        reentry_fingerprint,
    }
}

/// Clear dungeon geometry from a mesh (used when returning to a surface region).
pub fn clear_dungeon_state(mesh: &mut TerrainMesh) {
    mesh.models.clear();
    mesh.fixtures.clear();
    mesh.textures.clear();
    mesh.surfaces = crate::walkable::SurfaceIndex::default();
    mesh.zones.clear();
    mesh.zone_order.clear();
    mesh.zones_loaded = 0;
    mesh.heights.clear();
    mesh.heights_raw.clear();
    mesh.water_vertices.clear();
    mesh.water_indices.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_ids_are_stonehenge_barrows_region_20() {
        assert_eq!(CANONICAL_DUNGEON_REGION, 20);
        assert_eq!(CANONICAL_DUNGEON_ZONE, 19);
        assert_eq!(
            caer_world::zone_name(CANONICAL_DUNGEON_ZONE),
            Some(CANONICAL_DUNGEON_LABEL)
        );
        assert_eq!(
            caer_world::zone_region(CANONICAL_DUNGEON_ZONE),
            Some(CANONICAL_DUNGEON_REGION)
        );
        // Region 51 must never be silently treated as the canonical dungeon.
        assert_ne!(CANONICAL_DUNGEON_REGION, 51);
    }

    #[test]
    fn canonical_seat_uses_zone_offset() {
        let seat = canonical_dungeon_seat();
        assert!(
            (seat[0] - 7669.5).abs() < 1.0 && (seat[1] - 9234.3).abs() < 1.0,
            "seat drifted: {seat:?}"
        );
    }

    #[test]
    fn tomb_of_mithra_render_path_has_nonzero_fixture_boxes() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = Path::new(&root);
        let (fp, boxes) =
            load_dungeon_fixture_boxes(root, dungeon_zones::NAMED_DUNGEON_ZONE, Vec3::ZERO)
                .expect("Tomb of Mithra dungeon must load via render consumer");
        assert!(
            fp.fixture_boxes > 0 && !boxes.is_empty(),
            "M1: dungeon render consumer returned empty geometry {fp:?}"
        );
        assert!(fp.chunk_palette > 0, "palette empty: {fp:?}");
        assert_eq!(fp.fixture_boxes, boxes.len());
    }

    #[test]
    fn append_dungeon_fills_empty_surface_for_region_20_with_real_geometry() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = Path::new(&root);
        let seat = canonical_dungeon_seat();
        let mut mesh = TerrainMesh {
            origin: Vec3::new(seat[0], seat[1], 0.0),
            ..TerrainMesh::default()
        };
        assert_eq!(mesh.zones_loaded, 0);
        let stats = append_dungeon_geometry_if_surface_empty(&mut mesh, root, 20);
        assert!(
            stats.has_real_geometry(),
            "Lane D: region 20 must land real NIF instances, got {stats:?}"
        );
        assert!(
            stats.model_kinds >= 10,
            "expected many distinct chunk models, got {stats:?}"
        );
        // Surface region with terrain must not be overwritten.
        mesh.zones_loaded = 1;
        let before_inst: usize = mesh.models.iter().map(|m| m.instances.len()).sum();
        let before_fix = mesh.fixtures.len();
        let again = append_dungeon_geometry_if_surface_empty(&mut mesh, root, 20);
        assert_eq!(again.model_instances, 0);
        assert_eq!(again.fixture_boxes, 0);
        assert_eq!(
            mesh.models.iter().map(|m| m.instances.len()).sum::<usize>(),
            before_inst
        );
        assert_eq!(mesh.fixtures.len(), before_fix);
    }

    #[test]
    fn surface_region_1_unchanged_by_dungeon_append() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = Path::new(&root);
        let mut mesh = crate::terrain::load_region(
            1,
            Vec3::new(CANONICAL_SURFACE_SEAT[0], CANONICAL_SURFACE_SEAT[1], 0.0),
            [
                CANONICAL_SURFACE_SEAT[0] as i32 - 8_000,
                CANONICAL_SURFACE_SEAT[1] as i32 - 8_000,
            ],
            [
                CANONICAL_SURFACE_SEAT[0] as i32 + 8_000,
                CANONICAL_SURFACE_SEAT[1] as i32 + 8_000,
            ],
            crate::terrain::seam_blend(),
        );
        assert!(mesh.zones_loaded > 0, "region 1 must load surface terrain");
        let before_zones = mesh.zones_loaded;
        let before_models = mesh.models.len();
        let stats = append_dungeon_geometry_if_surface_empty(&mut mesh, root, 20);
        assert_eq!(stats.model_instances, 0);
        assert_eq!(stats.fixture_boxes, 0);
        assert_eq!(mesh.zones_loaded, before_zones);
        assert_eq!(mesh.models.len(), before_models);
    }

    #[test]
    fn dungeon_to_surface_return_clears_dungeon_state() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = Path::new(&root);
        let seat = canonical_dungeon_seat();
        let mut mesh = TerrainMesh {
            origin: Vec3::new(seat[0], seat[1], 0.0),
            ..TerrainMesh::default()
        };
        let stats = append_dungeon_geometry_if_surface_empty(&mut mesh, root, 20);
        assert!(stats.has_real_geometry());
        clear_dungeon_state(&mut mesh);
        assert!(mesh.models.is_empty());
        assert!(mesh.fixtures.is_empty());
        assert_eq!(mesh.zones_loaded, 0);
        assert!(mesh.surfaces.is_empty());
    }

    #[test]
    fn scene_fingerprint_stable_for_canonical_dungeon() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = Path::new(&root);
        let seat = canonical_dungeon_seat();
        let mut a = TerrainMesh {
            origin: Vec3::new(seat[0], seat[1], 0.0),
            ..TerrainMesh::default()
        };
        let mut b = TerrainMesh {
            origin: Vec3::new(seat[0], seat[1], 0.0),
            ..TerrainMesh::default()
        };
        append_dungeon_geometry_if_surface_empty(&mut a, root, 20);
        append_dungeon_geometry_if_surface_empty(&mut b, root, 20);
        let fa = scene_fingerprint(&a, 20, 19);
        let fb = scene_fingerprint(&b, 20, 19);
        assert_eq!(fa.content_hash, fb.content_hash);
        assert!(fa.model_instances > 0);
        assert!(fa.total_triangles > 0);
    }

    fn dummy_batch(pos: [f32; 3], vert_z: f32, texture: Option<String>) -> ModelBatch {
        ModelBatch {
            nhd: None,
            vertices: vec![TerrainVertex {
                pos: [0.0, 0.0, vert_z],
                normal: [0.0, 0.0, 1.0],
                color: [1.0, 1.0, 1.0],
                uv: [0.0, 0.0],
                overlay_uv: [0.0, 0.0],
                blend: 1.0,
            }],
            indices: vec![0, 0, 0],
            parts: vec![ModelPartRange {
                alpha: caer_assets::nif::AlphaMode::Opaque,
                start: 0,
                end: 3,
                texture,
                texture2: None,
                texture2_mode: caer_assets::nif::TextureLayerMode::VertexAlpha,
            }],
            instances: vec![ModelInstance {
                pos,
                base_pos: pos,
                yaw: 0.0,
                base_yaw: 0.0,
                rot: [0.0, 0.0, 0.0, 1.0],
                base_rot: [0.0, 0.0, 0.0, 1.0],
                scale: 1.0,
                zone_id: 19,
                fixture_id: 1,
            }],
            bound_center_z: 0.0,
            bound_radius: 1.0,
            bound_min: [0.0; 3],
            bound_max: [1.0; 3],
            morphs: Vec::new(),
            uv_anims: Vec::new(),
        }
    }

    /// W1-05: counts-only hash is insufficient — transform / vertex / material must move the hash.
    #[test]
    fn scene_fingerprint_changes_when_transform_vertex_or_material_changes() {
        let mut a = TerrainMesh::default();
        a.models
            .push(dummy_batch([1.0, 2.0, 3.0], 0.0, Some("stone.dds".into())));
        let ha = scene_fingerprint(&a, 20, 19).content_hash;

        let mut b = TerrainMesh::default();
        b.models
            .push(dummy_batch([9.0, 2.0, 3.0], 0.0, Some("stone.dds".into())));
        let hb = scene_fingerprint(&b, 20, 19).content_hash;
        assert_ne!(ha, hb, "instance transform must change fingerprint");

        let mut c = TerrainMesh::default();
        c.models
            .push(dummy_batch([1.0, 2.0, 3.0], 4.0, Some("stone.dds".into())));
        let hc = scene_fingerprint(&c, 20, 19).content_hash;
        assert_ne!(ha, hc, "vertex payload must change fingerprint");

        let mut d = TerrainMesh::default();
        d.models
            .push(dummy_batch([1.0, 2.0, 3.0], 0.0, Some("grass.dds".into())));
        let hd = scene_fingerprint(&d, 20, 19).content_hash;
        assert_ne!(ha, hd, "material/texture must change fingerprint");
    }

    #[test]
    fn skip_dungeon_append_makes_representative_product_red() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = Path::new(&root);
        let red = product_reload_surface_dungeon(
            root,
            CANONICAL_DUNGEON_REGION,
            CANONICAL_DUNGEON_ZONE,
            false,
        );
        assert!(
            !red.representative_pass(),
            "falsifier skip_dungeon_append must be red: {red:?}"
        );
        assert!(!red.append_invoked);
        assert!(!red.dungeon_stats.has_real_geometry());
        let green = product_reload_surface_dungeon(
            root,
            CANONICAL_DUNGEON_REGION,
            CANONICAL_DUNGEON_ZONE,
            true,
        );
        assert!(
            green.representative_pass(),
            "canonical representative must PASS with append: {green:?}"
        );
    }

    #[test]
    fn stale_surface_or_prior_dungeon_state_fails() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = Path::new(&root);
        let alb = caer_world::REP_ALBION;
        let seat_a = canonical_dungeon_seat();
        let mut a = TerrainMesh {
            origin: Vec3::new(seat_a[0], seat_a[1], 0.0),
            ..TerrainMesh::default()
        };
        let stats_a = append_dungeon_for_region(&mut a, root, CANONICAL_DUNGEON_REGION);
        assert!(stats_a.has_real_geometry());
        let fp_a = scene_fingerprint(&a, CANONICAL_DUNGEON_REGION, CANONICAL_DUNGEON_ZONE);

        // Stale: append another dungeon without clear — geometry accumulates.
        let seat_b = dungeon_product_seat(root, alb.zone_id);
        a.origin = Vec3::new(seat_b[0], seat_b[1], 0.0);
        let _ = append_dungeon_for_zone(&mut a, root, alb.zone_id);
        let fp_stale = scene_fingerprint(&a, alb.region_id, alb.zone_id);
        assert_ne!(
            fp_stale.content_hash, fp_a.content_hash,
            "stale prior-dungeon state must change the fingerprint"
        );
        assert!(
            fp_stale.model_instances >= fp_a.model_instances,
            "stale append accumulated instances"
        );

        // Control: clear then load B alone.
        clear_dungeon_state(&mut a);
        a.origin = Vec3::new(seat_b[0], seat_b[1], 0.0);
        let stats_b = append_dungeon_for_zone(&mut a, root, alb.zone_id);
        assert!(
            stats_b.has_real_geometry(),
            "Albion representative must have real NIF after clear: {stats_b:?}"
        );
        let fp_b = scene_fingerprint(&a, alb.region_id, alb.zone_id);
        assert_ne!(fp_stale.content_hash, fp_b.content_hash);

        // Stale surface: loaded region 1 must refuse dungeon append.
        let mut surface = crate::terrain::load_region(
            CANONICAL_SURFACE_REGION,
            Vec3::new(CANONICAL_SURFACE_SEAT[0], CANONICAL_SURFACE_SEAT[1], 0.0),
            [
                CANONICAL_SURFACE_SEAT[0] as i32 - 8_000,
                CANONICAL_SURFACE_SEAT[1] as i32 - 8_000,
            ],
            [
                CANONICAL_SURFACE_SEAT[0] as i32 + 8_000,
                CANONICAL_SURFACE_SEAT[1] as i32 + 8_000,
            ],
            crate::terrain::seam_blend(),
        );
        assert!(surface.zones_loaded > 0);
        let before = scene_fingerprint(&surface, CANONICAL_SURFACE_REGION, 0);
        let skipped = append_dungeon_for_region(&mut surface, root, CANONICAL_DUNGEON_REGION);
        assert_eq!(skipped.model_instances, 0);
        let after = scene_fingerprint(&surface, CANONICAL_SURFACE_REGION, 0);
        assert_eq!(before.content_hash, after.content_hash);
    }

    #[test]
    fn representatives_have_real_nif_not_boxes_only() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = Path::new(&root);
        // This is an invariant of the whole retail root, not of one representative.  Rebuilding
        // the 281-zone census inside this six-case loop made the coverage test do the same I/O
        // six times and turned a source check into a multi-minute serial bottleneck.
        let ids = caer_world::enumerate_dungeon_zone_ids(root).expect("enumerate");
        for rep in caer_world::DUNGEON_REPRESENTATIVES {
            assert_eq!(
                caer_world::dungeon_realm_or_category(rep.zone_id),
                rep.realm_or_category,
                "representative {} category drifted from table derivation",
                rep.label
            );
            let seat = dungeon_product_seat(root, rep.zone_id);
            let mut mesh = TerrainMesh {
                origin: Vec3::new(seat[0], seat[1], 0.0),
                ..TerrainMesh::default()
            };
            let stats = append_dungeon_for_zone(&mut mesh, root, rep.zone_id);
            assert!(
                stats.has_real_geometry(),
                "{} zone {} boxes-only/empty is not PASS: {stats:?}",
                rep.label,
                rep.zone_id
            );
            assert!(
                !mesh.surfaces.is_empty(),
                "{} zone {} collision surfaces empty",
                rep.label,
                rep.zone_id
            );
            let fp = scene_fingerprint(&mesh, rep.region_id, rep.zone_id);
            assert_eq!(fp.label, rep.label);
            assert!(fp.model_instances > 0 && fp.total_triangles > 0);
            assert!(
                ids.contains(&rep.zone_id),
                "{} zone {} must be in the MEAS-010 dungeon enumeration (not skycity/other)",
                rep.label,
                rep.zone_id
            );
            eprintln!(
                "REP {} zone={} region={} cat={} nif={} kinds={} boxes={} hash=0x{:016x}",
                rep.label,
                rep.zone_id,
                rep.region_id,
                rep.realm_or_category.as_str(),
                fp.model_instances,
                fp.model_kinds,
                fp.fixture_box_fallback,
                fp.content_hash
            );
        }
    }

    #[test]
    fn dungeon_census_enumerates_281_and_rejects_ok_empty() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let root = Path::new(&root);
        let rows = crate::dungeon_census::dungeon_census(root).expect("census");
        assert_eq!(
            rows.len(),
            281,
            "MEAS-010 denominator: census must cover every enumerated dungeon zone"
        );
        assert!(
            rows.iter()
                .all(|r| r.failure_reason != Some("ok_empty") || !r.is_pass()),
            "ok_empty row must not PASS"
        );
        for r in &rows {
            if r.placement_count == 0 {
                assert!(!r.is_pass(), "zone {} Ok(empty) PASS", r.zone);
                assert_eq!(r.failure_reason, Some("ok_empty"));
            }
            if r.real_nif_count == 0 && r.fallback_count > 0 {
                assert!(!r.is_pass(), "zone {} boxes-only PASS", r.zone);
            }
        }
        let pass = rows.iter().filter(|r| r.is_pass()).count();
        let fail = rows.len() - pass;
        eprintln!(
            "dungeon_census: rows={} pass={} fail={}",
            rows.len(),
            pass,
            fail
        );
        for rep in caer_world::DUNGEON_REPRESENTATIVES {
            let row = rows
                .iter()
                .find(|r| r.zone == rep.zone_id)
                .unwrap_or_else(|| panic!("representative {} missing from census", rep.label));
            assert!(
                row.real_nif_count > 0,
                "representative {} has no real NIF: {row:?}",
                rep.label
            );
            assert_ne!(
                row.failure_reason,
                Some("ok_empty"),
                "representative {} must not be ok_empty",
                rep.label
            );
            assert_ne!(
                row.failure_reason,
                Some("boxes_only"),
                "representative {} must not be boxes-only",
                rep.label
            );
            if row.fallback_count > 0 {
                assert!(
                    !row.is_pass(),
                    "mixed_boxes cannot PASS (not 281-playable): {row:?}"
                );
                assert_eq!(row.failure_reason, Some("mixed_boxes"));
            }
        }
    }

    /// W1-05: committed PNG/fingerprint under diagnostics/ are captures, not an ungated golden.
    #[test]
    fn stonehenge_capture_is_diagnostic_not_golden() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let diag = root.join("tests/diagnostics/dungeon_stonehenge_barrows.fingerprint.txt");
        let golden = root.join("tests/goldens/dungeon_stonehenge_barrows.fingerprint.txt");
        assert!(
            diag.is_file(),
            "diagnostic capture must live under tests/diagnostics/"
        );
        assert!(
            !golden.is_file(),
            "must not present the capture as tests/goldens/"
        );
        let text = std::fs::read_to_string(&diag).expect("read diagnostic fingerprint");
        assert!(
            text.contains("diagnostic_capture=true") || text.contains("not_a_golden"),
            "metadata must refuse golden status:\n{text}"
        );
    }
}
