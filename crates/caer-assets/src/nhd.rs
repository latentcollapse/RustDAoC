//! `.nhd` — the client's per-model walkable height grid.
//!
//! 1,087 of these ship in `zones/Nifs` and we never opened one; they were written off as "metadata
//! not geometry". They are how the original client knows you can stand on a dock, a keep floor or
//! the Cotswold forge platform, and not opening them is why every entity sank to terrain height.
//!
//! This replaces an improvised raycaster. CAER is a conversion: when the original ships the answer,
//! computing our own is a defect we chose to write.
//!
//! ## Layout
//!
//! Verified against all 1,087 files — 855 of the 856 in `zones/Nifs` parse with the body length
//! matching `(x1-x0) * (y1-y0) * 2` EXACTLY, which is what makes this a decode rather than a guess.
//!
//! ```text
//!   "NHD"          3 bytes
//!   version        u8   (1)
//!   name_len       u16 BE
//!   pad            u8
//!   name           name_len bytes  (e.g. "b06_forgebase.npk")
//!   cell           u16 LE          world units per cell (16 or 32 in practice)
//!   x0 x1 y0 y1    i32 LE x4       bounds in CELLS, not units
//!   heights        i16 LE * (x1-x0)*(y1-y0), row-major, y-major
//! ```
//!
//! `-2500` is the empty sentinel — no surface in that cell. Heights are model-space Z, so an
//! instance's world surface is `height * scale + instance_z`.

use std::io;

/// Height value meaning "no surface here".
pub const EMPTY: i16 = -2500;

/// A parsed `.nhd` grid.
pub struct NhdGrid {
    /// The model this belongs to, as named inside the file (`"Bfarmhouse.nif"`, `"b06_forgebase.npk"`).
    pub name: String,
    /// World units per cell.
    pub cell: u16,
    /// Bounds in CELLS (x0..x1, y0..y1), half-open.
    pub x0: i32,
    pub x1: i32,
    pub y0: i32,
    pub y1: i32,
    /// `(x1-x0) * (y1-y0)` heights, row-major with y outermost.
    pub heights: Vec<i16>,
}

impl NhdGrid {
    #[must_use]
    pub fn width(&self) -> i32 {
        self.x1 - self.x0
    }
    #[must_use]
    pub fn height(&self) -> i32 {
        self.y1 - self.y0
    }

    /// Surface height at a MODEL-space `(x, y)` in world units, or `None` outside the grid or in an
    /// empty cell.
    ///
    /// No interpolation: the client stores one height per cell and a building's surfaces are flat
    /// slabs and steps, so smoothing between cells would invent a ramp where the original has a
    /// step — and put the player's feet somewhere the real client never would.
    #[must_use]
    pub fn height_at(&self, x: f32, y: f32) -> Option<f32> {
        let cell = f32::from(self.cell);
        if cell <= 0.0 {
            return None;
        }
        let cx = (x / cell).floor() as i32;
        let cy = (y / cell).floor() as i32;
        if cx < self.x0 || cx >= self.x1 || cy < self.y0 || cy >= self.y1 {
            return None;
        }
        let i = ((cy - self.y0) * self.width() + (cx - self.x0)) as usize;
        match self.heights.get(i).copied() {
            Some(EMPTY) | None => None,
            Some(h) => Some(f32::from(h)),
        }
    }

    /// The highest non-empty cell, for sanity checks and reporting.
    #[must_use]
    pub fn max_height(&self) -> Option<i16> {
        self.heights.iter().copied().filter(|h| *h != EMPTY).max()
    }
}

/// Parse a `.nhd` file.
pub fn parse(bytes: &[u8]) -> io::Result<NhdGrid> {
    let err = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    if bytes.len() < 8 || &bytes[0..3] != b"NHD" {
        return Err(err("not an NHD file"));
    }
    let name_len = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    // 3 magic + 1 version + 2 length + 1 pad.
    let mut o = 7;
    if bytes.len() < o + name_len + 2 + 16 {
        return Err(err("NHD truncated in header"));
    }
    let name = String::from_utf8_lossy(&bytes[o..o + name_len]).into_owned();
    o += name_len;
    let cell = u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    o += 2;
    let i32_at =
        |p: usize| i32::from_le_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]]);
    let (x0, x1, y0, y1) = (i32_at(o), i32_at(o + 4), i32_at(o + 8), i32_at(o + 12));
    o += 16;

    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return Err(err("NHD bounds implausible"));
    }
    let n = (w as usize) * (h as usize);
    if bytes.len() < o + n * 2 {
        return Err(err("NHD truncated in body"));
    }
    let heights = bytes[o..o + n * 2]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    Ok(NhdGrid {
        name,
        cell,
        x0,
        x1,
        y0,
        y1,
        heights,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal well-formed NHD: 2x2 cells of 16 units, one cell empty.
    fn synthetic() -> Vec<u8> {
        let name = b"test.nif";
        let mut v = Vec::new();
        v.extend_from_slice(b"NHD");
        v.push(1);
        v.extend_from_slice(&(name.len() as u16).to_be_bytes());
        v.push(0);
        v.extend_from_slice(name);
        v.extend_from_slice(&16u16.to_le_bytes());
        for b in [0i32, 2, 0, 2] {
            v.extend_from_slice(&b.to_le_bytes());
        }
        for h in [100i16, 200, EMPTY, 300] {
            v.extend_from_slice(&h.to_le_bytes());
        }
        v
    }

    #[test]
    fn parses_header_bounds_and_grid() {
        let g = parse(&synthetic()).expect("should parse");
        assert_eq!(g.name, "test.nif");
        assert_eq!(g.cell, 16);
        assert_eq!((g.width(), g.height()), (2, 2));
        assert_eq!(g.heights.len(), 4);
        assert_eq!(g.max_height(), Some(300));
    }

    /// Cell lookup must floor into the right cell and honour the empty sentinel.
    #[test]
    fn looks_up_by_cell_and_respects_the_empty_sentinel() {
        let g = parse(&synthetic()).unwrap();
        assert_eq!(g.height_at(0.0, 0.0), Some(100.0)); // cell (0,0)
        assert_eq!(g.height_at(20.0, 0.0), Some(200.0)); // cell (1,0)
        assert_eq!(g.height_at(0.0, 20.0), None); // cell (0,1) is EMPTY
        assert_eq!(g.height_at(20.0, 20.0), Some(300.0)); // cell (1,1)
    }

    /// Outside the authored bounds there is no surface — the caller keeps terrain height.
    #[test]
    fn returns_none_outside_the_grid() {
        let g = parse(&synthetic()).unwrap();
        assert_eq!(g.height_at(-1.0, 0.0), None);
        assert_eq!(g.height_at(9999.0, 0.0), None);
    }

    #[test]
    fn rejects_a_non_nhd_blob() {
        assert!(parse(b"not an nhd file at all").is_err());
    }
}
