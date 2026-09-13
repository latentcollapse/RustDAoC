//! Global ocean / sea plane — fills the sky-through-void gaps between authored land tiles.

use crate::terrain::{TerrainMesh, TerrainVertex};

/// Authored ocean plane parameters derived from client `water.pcx` + heightmaps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OceanPlane {
    /// World-space sea surface Z.
    pub height: f32,
    /// Axis-aligned bounds in world XY (inclusive-ish; plane extends to these edges).
    pub min_xy: [f32; 2],
    pub max_xy: [f32; 2],
}

/// Derive sea height from a zone heightmap and its `water.pcx` mask.
///
/// On coastal zones, `water.pcx == 0` marks low/sea cells; inland zones may have few zeros.
/// Returns `None` when the mask has no sea cells.
#[must_use]
pub fn ocean_height_from_water_mask(
    heights: &[f32],
    samples: usize,
    water: &super::ShadeMap,
) -> Option<f32> {
    if heights.len() < samples * samples || water.width == 0 || water.height == 0 {
        return None;
    }
    let mut sea: Vec<f32> = Vec::new();
    let h = samples.min(water.height);
    let w = samples.min(water.width);
    for ty in 0..h {
        for tx in 0..w {
            if water.get(tx, ty) == 0 {
                sea.push(heights[ty * samples + tx]);
            }
        }
    }
    if sea.len() < 4 {
        return None;
    }
    sea.sort_by(|a, b| a.total_cmp(b));
    // 5th percentile — coastal flats, not the single deepest hole.
    let i = (sea.len() as f32 * 0.05) as usize;
    Some(sea[i.min(sea.len() - 1)])
}

/// Append a single large translucent quad at `plane.height` covering `min_xy..max_xy`
/// (world space), transformed into the mesh's render space (`origin`, Y mirrored).
pub fn append_ocean_plane(mesh: &mut TerrainMesh, plane: OceanPlane) {
    let origin = mesh.origin;
    let z = plane.height - origin.z;
    let (x0, y0) = (plane.min_xy[0], plane.min_xy[1]);
    let (x1, y1) = (plane.max_xy[0], plane.max_xy[1]);
    // Render space mirrors world Y.
    let corners = [
        [x0 - origin.x, -(y0 - origin.y), z],
        [x1 - origin.x, -(y0 - origin.y), z],
        [x1 - origin.x, -(y1 - origin.y), z],
        [x0 - origin.x, -(y1 - origin.y), z],
    ];
    let base = mesh.water_vertices.len() as u32;
    // Slightly greener/deeper than lake strips so the global sea reads as ocean, still using
    // the water pipeline's alpha.
    let color = [0.12, 0.28, 0.48];
    for c in corners {
        mesh.water_vertices.push(TerrainVertex {
            pos: c,
            normal: [0.0, 0.0, 1.0],
            color,
            uv: [0.0, 0.0],
            overlay_uv: [0.0, 0.0],
            blend: 1.0,
        });
    }
    mesh.water_indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atmosphere::ShadeMap;

    #[test]
    fn height_from_mask_uses_sea_cells() {
        let samples = 4;
        let mut heights = vec![1000.0; samples * samples];
        heights[0] = 40.0;
        heights[1] = 50.0;
        heights[2] = 45.0;
        heights[3] = 60.0;
        // mark first row as sea (0)
        let mut data = vec![255u8; 16];
        for d in data.iter_mut().take(4) {
            *d = 0;
        }
        let water = ShadeMap {
            width: 4,
            height: 4,
            data,
        };
        let h = ocean_height_from_water_mask(&heights, samples, &water).unwrap();
        assert!((40.0..=60.0).contains(&h), "got {h}");
    }

    #[test]
    fn append_adds_two_tris() {
        let mut mesh = TerrainMesh::default();
        append_ocean_plane(
            &mut mesh,
            OceanPlane {
                height: 50.0,
                min_xy: [0.0, 0.0],
                max_xy: [1000.0, 1000.0],
            },
        );
        assert_eq!(mesh.water_vertices.len(), 4);
        assert_eq!(mesh.water_indices.len(), 6);
    }
}
