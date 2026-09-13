//! Per-zone `SECTOR.DAT` parsing — currently the water bodies.
//!
//! `SECTOR.DAT` is an INI-style text file inside each zone's `datNNN.mpk`. Its `[riverNN]`
//! blocks define every water body in the zone (rivers AND lakes — a lake is just a wide strip)
//! as a strip of left/right bank-point pairs on the zone's 256×256 heightmap sample grid, plus
//! a world-Z surface height:
//!
//! ```text
//!   [river00]
//!   name=Lake Avalon
//!   height=1444          ; water surface, world Z units
//!   bankpoints=16        ; advisory — the real pair list can be longer, walk until it ends
//!   left00=44,213,0      ; sample-grid x,y (0..255) — z field unused (0) and often absent
//!   right00=56,213,0
//!   ...
//! ```
//!
//! Consecutive pairs (L[i],R[i]) → (L[i+1],R[i+1]) form quads; the renderer meshes them at
//! `height`. Grid coords scale to zone-local world units by `65536 / 256`.

use std::io;

/// One water body: a strip of bank-point pairs at a fixed surface height.
pub struct WaterBody {
    pub name: String,
    /// Water surface, world Z units.
    pub height: f32,
    /// Left/right bank points, zone-local world units (sample grid × 256), paired by index.
    pub left: Vec<[f32; 2]>,
    pub right: Vec<[f32; 2]>,
}

/// World units per heightmap sample-grid step (65,536 / 256) — same lattice as the terrain.
const GRID_UNITS: f32 = 256.0;

/// Parse every `[riverNN]` block out of a zone's `SECTOR.DAT` bytes. Unknown keys are ignored;
/// a block without a height or with fewer than 2 bank pairs is dropped (nothing to mesh).
pub fn water_bodies(sector: &[u8]) -> Vec<WaterBody> {
    let text = String::from_utf8_lossy(sector);
    let mut bodies = Vec::new();
    let mut cur: Option<WaterBody> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            // entering any new section closes the current river block
            if let Some(b) = cur.take() {
                if b.left.len() >= 2 && b.right.len() >= 2 {
                    bodies.push(b);
                }
            }
            if line.to_ascii_lowercase().starts_with("[river") {
                cur = Some(WaterBody {
                    name: String::new(),
                    height: f32::NAN,
                    left: Vec::new(),
                    right: Vec::new(),
                });
            }
            continue;
        }
        let Some(b) = cur.as_mut() else { continue };
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let (key, val) = (key.trim().to_ascii_lowercase(), val.trim());
        if key == "name" {
            b.name = val.to_string();
        } else if key == "height" {
            b.height = val.parse().unwrap_or(f32::NAN);
        } else if let Some(rest) = key.strip_prefix("left") {
            if rest.chars().all(|c| c.is_ascii_digit()) {
                if let Some(p) = parse_point(val) {
                    b.left.push(p);
                }
            }
        } else if let Some(rest) = key.strip_prefix("right") {
            if rest.chars().all(|c| c.is_ascii_digit()) {
                if let Some(p) = parse_point(val) {
                    b.right.push(p);
                }
            }
        }
    }
    if let Some(b) = cur.take() {
        if b.left.len() >= 2 && b.right.len() >= 2 {
            bodies.push(b);
        }
    }
    bodies.retain(|b| b.height.is_finite());
    bodies
}

/// `"x,y"` or `"x,y,z"` (z ignored) on the sample grid → zone-local world units.
fn parse_point(val: &str) -> Option<[f32; 2]> {
    let mut it = val.split(',').map(str::trim);
    let x: f32 = it.next()?.parse().ok()?;
    let y: f32 = it.next()?.parse().ok()?;
    Some([x * GRID_UNITS, y * GRID_UNITS])
}

/// Convenience: pull `SECTOR.DAT` out of a zone's `datNNN.mpk` bytes and parse its water.
pub fn water_from_dat_mpk(dat_mpk: &[u8]) -> io::Result<Vec<WaterBody>> {
    let members = crate::read(dat_mpk)?;
    let sector = members
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case("SECTOR.DAT"))
        .map(|m| m.data.as_slice())
        .unwrap_or(b"");
    Ok(water_bodies(sector))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_river_block() {
        let s = b"[river00]\nname=Lake Avalon\nheight=1444\nbankpoints=2\nleft00=44,213,0\nright00=56,213,0\nleft01=34,206\nright01=65,206\n\n[Holes]\nNum=0\n";
        let w = water_bodies(s);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0].name, "Lake Avalon");
        assert_eq!(w[0].height, 1444.0);
        assert_eq!(w[0].left.len(), 2);
        assert_eq!(w[0].left[0], [44.0 * 256.0, 213.0 * 256.0]);
    }

    #[test]
    fn drops_heightless_or_degenerate_blocks() {
        let s = b"[river00]\nleft00=1,1\nright00=2,2\n[river01]\nheight=100\nleft00=1,1\nright00=2,1\nleft01=1,5\nright01=2,5\n";
        let w = water_bodies(s);
        assert_eq!(
            w.len(),
            1,
            "river00 has no height and 1 pair; river01 is valid"
        );
        assert_eq!(w[0].height, 100.0);
    }
}
