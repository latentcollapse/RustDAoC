//! Per-zone terrain heightmaps.
//!
//! A DAoC zone is 65,536 world units square (8 grid cells of 8,192). Its terrain is a **256×256**
//! heightmap packed inside `datNNN.mpk` as two 8-bit PCX images plus scale factors in `SECTOR.DAT`:
//!
//! ```text
//!   height(tx,ty) = terrain.pcx[tx,ty] * scalefactor  +  offset.pcx[tx,ty] * offsetfactor
//! ```
//!
//! This formula was calibrated against real server mob Z-coordinates (which sit on the ground):
//! over 3,532 mobs in zone 8 the median error was 15 units — i.e. it *is* the ground. Each of the
//! 256 samples spans `65536 / 256 = 256` world units.

use std::io;

use crate::pcx;

/// Samples per zone edge (heightmap resolution).
pub const SAMPLES: usize = 256;
/// World units spanned per heightmap sample.
pub const SAMPLE_UNITS: f32 = 65_536.0 / SAMPLES as f32;

/// A single zone's decoded terrain: world-space height at each of the 256×256 samples.
#[derive(Clone)]
pub struct ZoneTerrain {
    /// Row-major `SAMPLES × SAMPLES` heights, in world Z units.
    pub heights: Vec<f32>,
}

impl ZoneTerrain {
    /// Decode a zone's terrain from the bytes of its `datNNN.mpk`.
    pub fn from_dat_mpk(dat_mpk: &[u8]) -> io::Result<Self> {
        let members = crate::read(dat_mpk)?;
        let find = |name: &str| {
            members
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(name))
                .map(|m| m.data.as_slice())
        };
        let terrain_pcx = find("terrain.pcx").ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "terrain.pcx not in dat archive")
        })?;
        let offset_pcx = find("offset.pcx").ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "offset.pcx not in dat archive")
        })?;
        let sector = find("SECTOR.DAT").unwrap_or(b"");

        let terrain = pcx::decode8(terrain_pcx)?;
        let offset = pcx::decode8(offset_pcx)?;
        let (scalefactor, offsetfactor) = parse_factors(sector);

        // Both maps are 256×256; guard in case a zone ships something odd.
        let w = terrain.width.min(offset.width).min(SAMPLES);
        let h = terrain.height.min(offset.height).min(SAMPLES);
        let mut heights = vec![0.0_f32; SAMPLES * SAMPLES];
        for ty in 0..h {
            for tx in 0..w {
                let v = terrain.get(tx, ty) as f32 * scalefactor
                    + offset.get(tx, ty) as f32 * offsetfactor;
                heights[ty * SAMPLES + tx] = v;
            }
        }
        Ok(Self { heights })
    }

    #[inline]
    pub fn height(&self, tx: usize, ty: usize) -> f32 {
        self.heights[ty.min(SAMPLES - 1) * SAMPLES + tx.min(SAMPLES - 1)]
    }
}

/// Pull `scalefactor` / `offsetfactor` from a zone's `SECTOR.DAT` `[terrain]` block, defaulting to
/// the common 8 / 32 if absent.
fn parse_factors(sector: &[u8]) -> (f32, f32) {
    let text = String::from_utf8_lossy(sector);
    let grab = |key: &str, default: f32| {
        text.lines()
            .find_map(|l| {
                l.trim()
                    .strip_prefix(key)?
                    .trim()
                    .strip_prefix('=')?
                    .trim()
                    .parse::<f32>()
                    .ok()
            })
            .unwrap_or(default)
    };
    (grab("scalefactor", 8.0), grab("offsetfactor", 32.0))
}
