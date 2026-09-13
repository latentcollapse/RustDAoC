//! A uniform spatial grid — the index that turns "what's near me" from an O(N) scan of the
//! whole world into an O(nearby) scan of a handful of cells.
//!
//! This is the crux of the perf thesis. The naive [`crate::WorldState`] queries walk every
//! entity every frame; at zerg scale that saturates a core. The grid buckets entities into
//! fixed-size cells, so an interest query only touches the cells the query radius overlaps —
//! independent of how big the rest of the world is. It is maintained incrementally as entities
//! spawn, move, and despawn, so it stays correct in real time, not just at load.

use std::collections::HashMap;

/// One entity's presence in a cell: its id and inline position (so distance filtering during a
/// query needs no second lookup into the entity store).
type Member = (u16, [i32; 2]);

/// Uniform grid over world XY. Cells are `cell`-sized squares; each holds the ids and positions
/// of the entities currently inside it.
#[derive(Debug, Clone)]
pub struct SpatialGrid {
    cell: i32,
    cells: HashMap<(i32, i32), Vec<Member>>,
}

impl SpatialGrid {
    /// Create a grid with the given cell size (world units). A good size is a bit larger than
    /// the typical interest radius, so a query touches only a 2×2–3×3 block of cells.
    #[must_use]
    pub fn new(cell: i32) -> Self {
        assert!(cell > 0, "cell size must be positive");
        Self {
            cell,
            cells: HashMap::new(),
        }
    }

    #[inline]
    fn key(&self, p: [i32; 2]) -> (i32, i32) {
        (p[0].div_euclid(self.cell), p[1].div_euclid(self.cell))
    }

    /// Insert an entity at a position.
    pub fn insert(&mut self, id: u16, pos: [i32; 2]) {
        self.cells.entry(self.key(pos)).or_default().push((id, pos));
    }

    /// Remove an entity known to be at `pos`. No-op if not present.
    pub fn remove(&mut self, id: u16, pos: [i32; 2]) {
        if let Some(bucket) = self.cells.get_mut(&self.key(pos)) {
            if let Some(i) = bucket.iter().position(|&(bid, _)| bid == id) {
                bucket.swap_remove(i);
            }
        }
    }

    /// Move an entity from `old` to `new`, touching cell buckets only if the cell changed.
    pub fn update(&mut self, id: u16, old: [i32; 2], new: [i32; 2]) {
        let (ko, kn) = (self.key(old), self.key(new));
        if ko == kn {
            // Same cell: patch the stored position in place.
            if let Some(bucket) = self.cells.get_mut(&ko) {
                if let Some(slot) = bucket.iter_mut().find(|(bid, _)| *bid == id) {
                    slot.1 = new;
                    return;
                }
            }
            // Fell through (wasn't there yet) — insert.
            self.cells.entry(kn).or_default().push((id, new));
        } else {
            self.remove(id, old);
            self.insert(id, new);
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.values().map(Vec::len).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.values().all(Vec::is_empty)
    }

    /// The range of cell coordinates a radius around a point overlaps.
    fn cell_range(&self, c: [i32; 2], radius: i32) -> ((i32, i32), (i32, i32)) {
        let min = self.key([c[0] - radius, c[1] - radius]);
        let max = self.key([c[0] + radius, c[1] + radius]);
        (min, max)
    }

    /// Count entities within `radius` (world units) of a point — visiting only overlapping cells.
    #[must_use]
    pub fn count_within(&self, c: [i32; 2], radius: i32) -> usize {
        let r2 = i64::from(radius) * i64::from(radius);
        let ((minx, miny), (maxx, maxy)) = self.cell_range(c, radius);
        let mut n = 0;
        for cx in minx..=maxx {
            for cy in miny..=maxy {
                if let Some(bucket) = self.cells.get(&(cx, cy)) {
                    n += bucket.iter().filter(|&&(_, p)| dist2(p, c) <= r2).count();
                }
            }
        }
        n
    }

    /// The `k` nearest entities to a point. Widens the searched cell ring until it holds at
    /// least `k` candidates *and* one full ring beyond the k-th distance, so the result is exact
    /// (an entity in a not-yet-searched cell can't be closer than the ring boundary).
    #[must_use]
    pub fn nearest(&self, c: [i32; 2], k: usize) -> Vec<(u16, u64)> {
        if k == 0 {
            return Vec::new();
        }
        let center = self.key(c);
        let mut found: Vec<(u16, u64)> = Vec::new();
        let mut ring = 0;
        loop {
            // Gather everything in the square ring at Chebyshev distance == `ring`.
            collect_ring(&self.cells, center, ring, c, &mut found);
            // Stop once we have k candidates whose k-th distance is inside the guaranteed-
            // complete radius (ring fully searched => anything closer than ring*cell is found).
            if found.len() >= k {
                found.sort_unstable_by_key(|&(_, d)| d);
                let kth = found[k - 1].1;
                let safe = i64::from(ring) * i64::from(self.cell);
                if (kth as i64) <= safe * safe || ring > MAX_RING {
                    found.truncate(k);
                    return found;
                }
            } else if ring > MAX_RING {
                found.sort_unstable_by_key(|&(_, d)| d);
                found.truncate(k);
                return found;
            }
            ring += 1;
        }
    }
}

/// Safety valve so a sparse world can't loop forever widening rings.
const MAX_RING: i32 = 4096;

/// Collect the entities in every cell whose Chebyshev distance from `center` equals `ring`
/// (i.e. the square shell at that radius; `ring == 0` is the centre cell).
fn collect_ring(
    cells: &HashMap<(i32, i32), Vec<Member>>,
    center: (i32, i32),
    ring: i32,
    point: [i32; 2],
    out: &mut Vec<(u16, u64)>,
) {
    let mut visit = |cx: i32, cy: i32| {
        if let Some(bucket) = cells.get(&(cx, cy)) {
            out.extend(bucket.iter().map(|&(id, p)| (id, dist2(p, point) as u64)));
        }
    };
    if ring == 0 {
        visit(center.0, center.1);
        return;
    }
    for cx in (center.0 - ring)..=(center.0 + ring) {
        visit(cx, center.1 - ring);
        visit(cx, center.1 + ring);
    }
    for cy in (center.1 - ring + 1)..(center.1 + ring) {
        visit(center.0 - ring, cy);
        visit(center.0 + ring, cy);
    }
}

#[inline]
fn dist2(p: [i32; 2], c: [i32; 2]) -> i64 {
    let dx = i64::from(p[0] - c[0]);
    let dy = i64::from(p[1] - c[1]);
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_count_and_remove() {
        let mut g = SpatialGrid::new(8192);
        g.insert(1, [1000, 1000]);
        g.insert(2, [2000, 2000]);
        g.insert(3, [500_000, 500_000]);
        assert_eq!(g.len(), 3);
        assert_eq!(g.count_within([1000, 1000], 3000), 2); // 1 and 2, not the far one
        g.remove(2, [2000, 2000]);
        assert_eq!(g.count_within([1000, 1000], 3000), 1);
    }

    #[test]
    fn update_moves_between_cells() {
        let mut g = SpatialGrid::new(1000);
        g.insert(7, [100, 100]);
        assert_eq!(g.count_within([100, 100], 50), 1);
        g.update(7, [100, 100], [50_000, 50_000]);
        assert_eq!(g.count_within([100, 100], 50), 0);
        assert_eq!(g.count_within([50_000, 50_000], 50), 1);
        assert_eq!(g.len(), 1, "no duplicate after move");
    }

    #[test]
    fn nearest_matches_bruteforce() {
        // Random-ish scatter; the grid's nearest must equal a brute-force nearest.
        let mut g = SpatialGrid::new(4096);
        let pts: Vec<(u16, [i32; 2])> = (0..500u16)
            .map(|i| {
                let h = (u32::from(i)).wrapping_mul(2_654_435_761);
                (i, [(h % 1_000_000) as i32, ((h >> 11) % 1_000_000) as i32])
            })
            .collect();
        for &(id, p) in &pts {
            g.insert(id, p);
        }
        let q = [500_000, 500_000];
        let mut brute: Vec<(u16, i64)> = pts.iter().map(|&(id, p)| (id, dist2(p, q))).collect();
        brute.sort_unstable_by_key(|&(_, d)| d);
        let grid = g.nearest(q, 8);
        let brute_ids: Vec<u16> = brute.iter().take(8).map(|&(id, _)| id).collect();
        let grid_ids: Vec<u16> = grid.iter().map(|&(id, _)| id).collect();
        assert_eq!(grid_ids, brute_ids, "grid nearest must match brute force");
    }

    #[test]
    fn nearest_on_empty_and_k_zero() {
        let g = SpatialGrid::new(1000);
        assert!(g.nearest([0, 0], 4).is_empty());
        let mut g2 = SpatialGrid::new(1000);
        g2.insert(1, [0, 0]);
        assert!(g2.nearest([0, 0], 0).is_empty());
    }
}
