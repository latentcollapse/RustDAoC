//! Walkable surfaces: how high the ground is when the "ground" is a building.
//!
//! Player and NPC Z both came from `TerrainMesh::height_at`, which reads only the zone heightfield.
//! It knows nothing about placed fixture geometry, so standing on anything raised — the Cotswold
//! forge platform, docks, bridges, keep floors, stairs — sank you to terrain level. Matt's
//! screenshot at (561552, 510231) is the case: terrain 2368.6, the `b06_forgebase` fixture base at
//! 2385.4, and a stone slab on top of that.
//!
//! This casts a downward ray against the actual fixture triangles and reports the highest surface at
//! or below the entity. It is deliberately geometric rather than heuristic: a "raised platforms are
//! N units tall" rule would be another invented constant, and the mesh already knows the answer.
//!
//! ## Cost
//!
//! A naive test against every fixture in the region is ~19,000 instances per query. Instead the
//! index buckets instance world-AABBs into a coarse XY grid once per region load, so a query only
//! visits fixtures whose footprint actually covers the point — normally zero or one.
//!
//! Rays are transformed into MODEL space rather than transforming triangles into world space: a
//! model is thousands of triangles and the ray is one, and it means an instance's rotation
//! (including the authored pitch/roll now carried as a quaternion) is handled exactly, with no
//! per-instance geometry copy.

use std::collections::HashMap;

use crate::terrain::{ModelBatch, TerrainMesh};

/// Grid cell size in world units. Comfortably larger than most buildings' footprints, so a query
/// touches few cells, while small enough that a cell rarely gathers many candidates.
const CELL: f32 = 1024.0;

/// Extra head-room above an entity from which the downward ray starts, so a surface slightly above
/// the entity's own z (it is standing ON the slab, so its feet are AT the surface) is still hit.
const RAY_LIFT: f32 = 8.0;

/// How far BELOW the entity a surface may be and still count as what it is standing on. Without a
/// floor here, a character on a bridge would snap to the riverbed geometry far underneath.
const MAX_DROP: f32 = 512.0;

/// How far ABOVE the entity's feet a surface may be and still be stepped onto.
///
/// Needed because you walk ONTO things from lower ground: rejecting everything above the entity
/// meant a player at terrain height never rose onto a platform at all, so the surface lookup only
/// worked for someone already standing on it. Kept small so a doorway threshold or stair tread is
/// climbable while a roof 300 units up is not — you reach those by their ramp, whose cells carry the
/// intermediate heights.
///
/// UNVERIFIED against the original client's step height; this is the one number here not taken from
/// client data, and it should be pinned by observation rather than left at a guess.
const MAX_STEP_UP: f32 = 48.0;

/// Spatial index over placed fixture geometry, built once per region load.
#[derive(Default)]
pub struct SurfaceIndex {
    /// cell -> (model batch index, instance index within that batch)
    cells: HashMap<(i32, i32), Vec<(u32, u32)>>,
}

fn cell_of(x: f32, y: f32) -> (i32, i32) {
    ((x / CELL).floor() as i32, (y / CELL).floor() as i32)
}

/// Rotate `v` by the conjugate of unit quaternion `q` — i.e. undo the instance rotation.
fn qrot_inv(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    // conj(q) = (-x, -y, -z, w); then v + 2*c.xyz × (c.xyz × v + c.w·v).
    let c = [-q[0], -q[1], -q[2], q[3]];
    let t = [
        c[1] * v[2] - c[2] * v[1] + c[3] * v[0],
        c[2] * v[0] - c[0] * v[2] + c[3] * v[1],
        c[0] * v[1] - c[1] * v[0] + c[3] * v[2],
    ];
    [
        v[0] + 2.0 * (c[1] * t[2] - c[2] * t[1]),
        v[1] + 2.0 * (c[2] * t[0] - c[0] * t[2]),
        v[2] + 2.0 * (c[0] * t[1] - c[1] * t[0]),
    ]
}

impl SurfaceIndex {
    /// Bucket every placed instance by the XY cells its world AABB covers.
    #[must_use]
    pub fn build(mesh: &TerrainMesh) -> Self {
        let mut cells: HashMap<(i32, i32), Vec<(u32, u32)>> = HashMap::new();
        for (mi, batch) in mesh.models.iter().enumerate() {
            for (ii, inst) in batch.instances.iter().enumerate() {
                let Some((lo, hi)) = instance_aabb_xy(batch, inst.rot, inst.scale, inst.pos) else {
                    continue;
                };
                let (c0, c1) = (cell_of(lo[0], lo[1]), cell_of(hi[0], hi[1]));
                for cy in c0.1..=c1.1 {
                    for cx in c0.0..=c1.0 {
                        cells
                            .entry((cx, cy))
                            .or_default()
                            .push((mi as u32, ii as u32));
                    }
                }
            }
        }
        Self { cells }
    }

    /// Highest fixture surface at `(x, y)` that an entity at `z` could be standing on.
    ///
    /// `None` when no fixture covers the point — the caller then keeps the terrain height. Returns
    /// RENDER-space z, matching the coordinates the instances are stored in.
    #[must_use]
    pub fn surface_at(&self, mesh: &TerrainMesh, x: f32, y: f32, z: f32) -> Option<f32> {
        let candidates = self.cells.get(&cell_of(x, y))?;
        let origin = [x, y, z + RAY_LIFT];
        let mut best: Option<f32> = None;
        for &(mi, ii) in candidates {
            let batch = mesh.models.get(mi as usize)?;
            let inst = batch.instances.get(ii as usize)?;
            // The client's own grid first; our raycaster only where the client ships none.
            let hit = nhd_height(batch, inst.rot, inst.scale, inst.pos, origin)
                .or_else(|| raycast_down(batch, inst.rot, inst.scale, inst.pos, origin));
            if let Some(hit) = hit {
                // Only surfaces at or below the entity, and not absurdly far below.
                if hit - origin[2] <= MAX_STEP_UP && origin[2] - hit <= MAX_DROP + RAY_LIFT {
                    best = Some(best.map_or(hit, |b: f32| b.max(hit)));
                }
            }
        }
        best
    }

    /// The highest authored surface at `(x, y)`, with no step-up or drop limits.
    ///
    /// For TOOLING and diagnostics: `surface_at` answers "what can this entity stand on from where
    /// it is", which is deliberately bounded, whereas an audit wants "what did the artist put here"
    /// regardless of reachability. Keeping them separate stops a tool's needs loosening the
    /// gameplay rule.
    #[must_use]
    pub fn highest_surface(&self, mesh: &TerrainMesh, x: f32, y: f32) -> Option<f32> {
        let candidates = self.cells.get(&cell_of(x, y))?;
        let origin = [x, y, 0.0];
        let mut best: Option<f32> = None;
        for &(mi, ii) in candidates {
            let batch = mesh.models.get(mi as usize)?;
            let inst = batch.instances.get(ii as usize)?;
            if let Some(h) = nhd_height(batch, inst.rot, inst.scale, inst.pos, origin) {
                best = Some(best.map_or(h, |b: f32| b.max(h)));
            }
        }
        best
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// World-space XY bounds of one instance, from the model's local AABB.
///
/// All eight corners are rotated rather than the two extremes: under a general rotation the AABB's
/// min/max corners do not map to the rotated box's min/max, so rotating only those two silently
/// shrinks the footprint and a query near an edge misses the fixture entirely.
fn instance_aabb_xy(
    batch: &ModelBatch,
    rot: [f32; 4],
    scale: f32,
    pos: [f32; 3],
) -> Option<([f32; 2], [f32; 2])> {
    let (lo, hi) = (batch.bound_min, batch.bound_max);
    if !lo[0].is_finite() || !hi[0].is_finite() {
        return None;
    }
    let mut mn = [f32::MAX; 2];
    let mut mx = [f32::MIN; 2];
    for i in 0..8 {
        let c = [
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        ];
        let w = crate::terrain::qrot(rot, [c[0] * scale, c[1] * scale, c[2] * scale]);
        for k in 0..2 {
            mn[k] = mn[k].min(w[k] + pos[k]);
            mx[k] = mx[k].max(w[k] + pos[k]);
        }
    }
    Some((mn, mx))
}

/// Surface height from the model's `.nhd` grid, if the client ships one.
///
/// This is the authoritative answer — it is what the original client stands entities on — and it is
/// a single cell lookup rather than thousands of ray-triangle tests. `None` means either no grid or
/// no authored surface at that spot, and the caller falls back to the render mesh.
fn nhd_height(
    batch: &ModelBatch,
    rot: [f32; 4],
    scale: f32,
    pos: [f32; 3],
    origin: [f32; 3],
) -> Option<f32> {
    let grid = batch.nhd.as_ref()?;
    if scale.abs() < 1e-6 {
        return None;
    }
    // World -> model space, exactly as the raycast path does.
    let rel = [origin[0] - pos[0], origin[1] - pos[1], origin[2] - pos[2]];
    let m = qrot_inv(rot, rel);
    let h = grid.height_at(m[0] / scale, m[1] / scale)?;
    Some(h * scale + pos[2])
}

/// Cast a straight-down world ray at one instance; returns the world z of the highest triangle hit.
fn raycast_down(
    batch: &ModelBatch,
    rot: [f32; 4],
    scale: f32,
    pos: [f32; 3],
    origin: [f32; 3],
) -> Option<f32> {
    let s = if scale.abs() < 1e-6 {
        return None;
    } else {
        scale
    };
    // World -> model: undo translation, rotation and uniform scale.
    let rel = [origin[0] - pos[0], origin[1] - pos[1], origin[2] - pos[2]];
    let om = qrot_inv(rot, rel);
    let o = [om[0] / s, om[1] / s, om[2] / s];
    let d = qrot_inv(rot, [0.0, 0.0, -1.0]); // direction needs no scaling, only rotation

    let mut best_t: Option<f32> = None;
    for tri in batch.indices.chunks_exact(3) {
        let (a, b, c) = (
            batch.vertices.get(tri[0] as usize)?.pos,
            batch.vertices.get(tri[1] as usize)?.pos,
            batch.vertices.get(tri[2] as usize)?.pos,
        );
        if let Some(t) = ray_tri(o, d, a, b, c) {
            if t >= 0.0 {
                best_t = Some(best_t.map_or(t, |x: f32| x.min(t)));
            }
        }
    }
    // t is in model units along a unit-length direction; uniform scale converts it to world units.
    best_t.map(|t| origin[2] - t * s)
}

/// Möller–Trumbore, double-sided.
///
/// Double-sided on purpose: fixture meshes are not reliably wound outward, and a one-sided test
/// would drop the floor of any model whose top face happens to point away.
fn ray_tri(o: [f32; 3], d: [f32; 3], a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<f32> {
    let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let p = [
        d[1] * e2[2] - d[2] * e2[1],
        d[2] * e2[0] - d[0] * e2[2],
        d[0] * e2[1] - d[1] * e2[0],
    ];
    let det = e1[0] * p[0] + e1[1] * p[1] + e1[2] * p[2];
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let tv = [o[0] - a[0], o[1] - a[1], o[2] - a[2]];
    let u = (tv[0] * p[0] + tv[1] * p[1] + tv[2] * p[2]) * inv;
    if !(-1e-5..=1.000_01).contains(&u) {
        return None;
    }
    let q = [
        tv[1] * e1[2] - tv[2] * e1[1],
        tv[2] * e1[0] - tv[0] * e1[2],
        tv[0] * e1[1] - tv[1] * e1[0],
    ];
    let v = (d[0] * q[0] + d[1] * q[1] + d[2] * q[2]) * inv;
    if v < -1e-5 || u + v > 1.000_01 {
        return None;
    }
    Some((e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]) * inv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::{ModelInstance, ModelPartRange, TerrainVertex};

    fn vert(p: [f32; 3]) -> TerrainVertex {
        TerrainVertex {
            pos: p,
            normal: [0.0, 0.0, 1.0],
            color: [1.0; 3],
            uv: [0.0, 0.0],
            overlay_uv: [0.0, 0.0],
            blend: 1.0,
        }
    }

    /// A flat 200x200 slab at model z = 50, placed at `pos` with `rot`/`scale`.
    fn slab(pos: [f32; 3], rot: [f32; 4], scale: f32) -> (TerrainMesh, SurfaceIndex) {
        let vertices = vec![
            vert([-100.0, -100.0, 50.0]),
            vert([100.0, -100.0, 50.0]),
            vert([100.0, 100.0, 50.0]),
            vert([-100.0, 100.0, 50.0]),
        ];
        let indices = vec![0, 1, 2, 0, 2, 3];
        let batch = ModelBatch {
            nhd: None,
            vertices,
            indices,
            parts: vec![ModelPartRange {
                start: 0,
                end: 6,
                texture: None,
                alpha: caer_assets::nif::AlphaMode::Opaque,
                texture2: None,
                texture2_mode: caer_assets::nif::TextureLayerMode::VertexAlpha,
            }],
            instances: vec![ModelInstance {
                pos,
                base_pos: pos,
                yaw: 0.0,
                base_yaw: 0.0,
                rot,
                base_rot: rot,
                scale,
                zone_id: 0,
                fixture_id: 1,
            }],
            bound_center_z: 50.0,
            bound_radius: 150.0,
            bound_min: [-100.0, -100.0, 50.0],
            bound_max: [100.0, 100.0, 50.0],
            morphs: Vec::new(),
            uv_anims: Vec::new(),
        };
        let mesh = TerrainMesh {
            models: vec![batch],
            ..TerrainMesh::default()
        };
        let idx = SurfaceIndex::build(&mesh);
        (mesh, idx)
    }

    /// The core case: an entity standing over a raised slab gets the SLAB's height, not the terrain.
    #[test]
    fn reports_the_slab_surface_not_the_ground() {
        let (mesh, idx) = slab([0.0, 0.0, 100.0], [0.0, 0.0, 0.0, 1.0], 1.0);
        // Slab surface is at 100 + 50 = 150. Entity standing on it (feet at 150).
        let h = idx
            .surface_at(&mesh, 10.0, -20.0, 150.0)
            .expect("slab should be hit");
        assert!((h - 150.0).abs() < 0.5, "expected surface 150, got {h}");
    }

    /// Off the slab's footprint there is no fixture surface, and the caller keeps terrain height.
    #[test]
    fn returns_none_beside_the_slab() {
        let (mesh, idx) = slab([0.0, 0.0, 100.0], [0.0, 0.0, 0.0, 1.0], 1.0);
        assert!(idx.surface_at(&mesh, 5000.0, 5000.0, 150.0).is_none());
    }

    /// Uniform scale must scale the surface HEIGHT too, not just the footprint.
    #[test]
    fn honours_instance_scale() {
        let (mesh, idx) = slab([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 2.0);
        // Model z 50 at scale 2 => 100.
        let h = idx
            .surface_at(&mesh, 0.0, 0.0, 100.0)
            .expect("scaled slab should be hit");
        assert!((h - 100.0).abs() < 0.5, "expected 100 at scale 2, got {h}");
    }

    /// A yaw does not change a centred slab's height, but it must not break the query either —
    /// this is the case a rotated-AABB bug would silently drop.
    #[test]
    fn survives_a_rotated_instance() {
        let a = std::f32::consts::FRAC_PI_4;
        let rot = [0.0, 0.0, (a * 0.5).sin(), (a * 0.5).cos()];
        let (mesh, idx) = slab([0.0, 0.0, 100.0], rot, 1.0);
        let h = idx
            .surface_at(&mesh, 0.0, 0.0, 150.0)
            .expect("rotated slab should still be hit");
        assert!((h - 150.0).abs() < 0.5, "expected 150 under yaw, got {h}");
    }

    /// A surface far below the entity is not what it is standing on (bridge over a riverbed).
    #[test]
    fn ignores_surfaces_far_below() {
        let (mesh, idx) = slab([0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0], 1.0);
        assert!(idx
            .surface_at(&mesh, 0.0, 0.0, 50.0 + MAX_DROP + 100.0)
            .is_none());
    }
}
