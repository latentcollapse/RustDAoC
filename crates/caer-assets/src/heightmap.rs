//! Region heightmap round-trip: export a whole region's stitched terrain to a 16-bit grayscale
//! PNG (+ a plain-text sidecar) that UE5 Landscape — or any heightmap tool — can sculpt, then
//! import the edited PNG back into per-zone height grids.
//!
//! Why this exists: hand-rolling a terrain-sculpt brush inside the renderer was the wrong altitude
//! (see `project-daoc-gamedll-rust` / the parity-harness memory). DAoC terrain is *already* a grid
//! of height samples, so the entire editing problem reduces to two small, testable functions —
//! stitch out, slice back in — and the actual sculpting happens in a mature tool.
//!
//! ## The lattice
//! Every zone's samples sit on ONE global 256-unit lattice: zone grid offsets are multiples of
//! `ZONE_UNIT` (8192) = 32 samples, and each zone is a 256×256 grid at [`SAMPLE_UNITS`] spacing.
//! So the region image is a straight linear map — pixel ↔ world, no interpolation — and every
//! in-bounds pixel lands exactly on some zone's sample.
//!
//! ## Orientation
//! Exported **y-north** (row 0 = northmost = the largest world Y), so what you sculpt matches the
//! on-screen / atlas view. World data is y-south, so the flip lives here, in exactly one place, on
//! both the export and import legs. (The y-mirror has bitten this project before — it gets one
//! documented conversion, not a scattering of negations.)
//!
//! ## Seam unification (deliberate, not a bug)
//! Where two staggered zones overlap the same world point with *different* heights (a seam fold),
//! the stitch keeps one value per lattice point and the import writes it back to every zone that
//! covers it — so after a round-trip the zones AGREE at the seam. That is the fold-removal we want;
//! the only thing "lost" is the pre-existing disagreement.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::Path;

use crate::terrain::{ZoneTerrain, SAMPLES, SAMPLE_UNITS};

/// DAoC zone-grid unit in world units. Mirrors `caer_world::ZONE_UNIT`; duplicated as a plain
/// constant so this asset crate needn't depend on the world crate for one integer.
pub const ZONE_UNIT: i32 = 8192;

/// World-unit span of one zone along an axis (256 samples × 256 units).
const ZONE_SPAN: i32 = SAMPLES as i32 * SAMPLE_UNITS as i32; // 65_536

/// UE5 Landscape recommended per-axis vertex counts (63-quad sections: `k·63 + 1`). Import needs
/// the landscape's dimensions to be one of these, so export pads up to the nearest that fits.
const UE_SIZES: [usize; 7] = [127, 253, 505, 1009, 2017, 4033, 8129];

/// A stitched region heightmap plus everything import needs to reverse it exactly. `pixels` is the
/// PADDED image (`pad_w × pad_h`, row-major); the real region data occupies the top-left
/// `data_w × data_h`, the rest is edge-clamped padding so a tool sees no cliff at the crop line.
#[derive(Clone)]
pub struct RegionHeightmap {
    pub region: u16,
    /// World Z that maps to pixel value 0 and 65535 — the reconstruction endpoints.
    pub min_z: f32,
    pub max_z: f32,
    /// World coords of the data's top-left pixel (px=0,py=0): westmost X, northmost Y.
    pub origin_wx: i32,
    pub origin_wy: i32,
    pub data_w: usize,
    pub data_h: usize,
    pub pad_w: usize,
    pub pad_h: usize,
    /// The zones this heightmap was stitched from (grid offsets) — import writes back into these.
    pub zones: Vec<(i32, i32)>,
    /// `pad_h × pad_w` row-major 16-bit samples.
    pub pixels: Vec<u16>,
}

impl RegionHeightmap {
    /// Convert a stored 16-bit sample to a world Z using the recorded endpoints.
    #[inline]
    fn decode_z(&self, v: u16) -> f32 {
        if self.max_z <= self.min_z {
            return self.min_z;
        }
        self.min_z + (v as f32 / 65535.0) * (self.max_z - self.min_z)
    }
}

/// Stitch every zone in `zones` into one region heightmap. `zones` is keyed by grid offset
/// `(ox, oy)`; heights are world Z. Deterministic: overlapping lattice points resolve to the
/// lowest `(ox, oy)` owner, so the export is reproducible regardless of map iteration order.
pub fn export(zones: &HashMap<(i32, i32), ZoneTerrain>, region: u16) -> RegionHeightmap {
    assert!(!zones.is_empty(), "export: no zones to stitch");
    // Deterministic owner order (lowest offset wins on an overlap).
    let ordered: BTreeMap<(i32, i32), &ZoneTerrain> = zones.iter().map(|(k, v)| (*k, v)).collect();

    // World bbox of the sample lattice. A zone at offset (ox,oy) has samples at
    // ox·ZONE_UNIT + tx·SAMPLE_UNITS for tx in 0..SAMPLES (last sample = corner + 65_280).
    let last = (SAMPLES as i32 - 1) * SAMPLE_UNITS as i32;
    let mut min_wx = i32::MAX;
    let mut max_wx = i32::MIN;
    let mut min_wy = i32::MAX;
    let mut max_wy = i32::MIN;
    for &(ox, oy) in ordered.keys() {
        let (cx, cy) = (ox * ZONE_UNIT, oy * ZONE_UNIT);
        min_wx = min_wx.min(cx);
        max_wx = max_wx.max(cx + last);
        min_wy = min_wy.min(cy);
        max_wy = max_wy.max(cy + last);
    }
    let data_w = ((max_wx - min_wx) / SAMPLE_UNITS as i32 + 1) as usize;
    let data_h = ((max_wy - min_wy) / SAMPLE_UNITS as i32 + 1) as usize;

    // Sample the surface at every data pixel (y-north: row 0 = max world Y). Track the height
    // range for quantization; remember holes (pixels no zone covers) to fill flat afterwards.
    let sample = |wx: i32, wy: i32| -> Option<f32> {
        for (&(ox, oy), t) in &ordered {
            let (dx, dy) = (wx - ox * ZONE_UNIT, wy - oy * ZONE_UNIT);
            if (0..ZONE_SPAN).contains(&dx) && (0..ZONE_SPAN).contains(&dy) {
                return Some(t.height(
                    dx as usize / SAMPLE_UNITS as usize,
                    dy as usize / SAMPLE_UNITS as usize,
                ));
            }
        }
        None
    };
    let mut heights = vec![f32::NAN; data_w * data_h];
    let mut min_z = f32::INFINITY;
    let mut max_z = f32::NEG_INFINITY;
    for py in 0..data_h {
        let wy = max_wy - py as i32 * SAMPLE_UNITS as i32;
        for px in 0..data_w {
            let wx = min_wx + px as i32 * SAMPLE_UNITS as i32;
            if let Some(z) = sample(wx, wy) {
                heights[py * data_w + px] = z;
                min_z = min_z.min(z);
                max_z = max_z.max(z);
            }
        }
    }
    if !min_z.is_finite() {
        // No covered samples at all — degenerate, but keep the endpoints sane.
        min_z = 0.0;
        max_z = 0.0;
    }

    let quant = |z: f32| -> u16 {
        if max_z <= min_z {
            return 32768;
        }
        (((z - min_z) / (max_z - min_z)).clamp(0.0, 1.0) * 65535.0).round() as u16
    };

    // Pad each axis up to the nearest UE landscape size, edge-clamping the border outward.
    let pad_w = ue_size(data_w);
    let pad_h = ue_size(data_h);
    let mut pixels = vec![0u16; pad_w * pad_h];
    for py in 0..pad_h {
        let sy = py.min(data_h - 1);
        for px in 0..pad_w {
            let sx = px.min(data_w - 1);
            let z = heights[sy * data_w + sx];
            // Holes read as the floor so the tool sees flat low ground, not garbage.
            pixels[py * pad_w + px] = if z.is_nan() { 0 } else { quant(z) };
        }
    }

    RegionHeightmap {
        region,
        min_z,
        max_z,
        origin_wx: min_wx,
        origin_wy: max_wy,
        data_w,
        data_h,
        pad_w,
        pad_h,
        zones: ordered.keys().copied().collect(),
        pixels,
    }
}

/// Reconstruct per-zone height grids from an (edited) region heightmap. Every zone listed in the
/// heightmap is rebuilt: each of its 256×256 samples reads the pixel at its world position, so all
/// zones come back consistent with the edited surface. Pixels outside the data rect (padding) or
/// off-image are left at the zone's decoded fallback — but for a well-formed region every zone
/// sample maps inside the data.
pub fn import(hm: &RegionHeightmap) -> HashMap<(i32, i32), ZoneTerrain> {
    let mut out = HashMap::with_capacity(hm.zones.len());
    for &(ox, oy) in &hm.zones {
        let mut heights = vec![0.0f32; SAMPLES * SAMPLES];
        for ty in 0..SAMPLES {
            for tx in 0..SAMPLES {
                let wx = ox * ZONE_UNIT + tx as i32 * SAMPLE_UNITS as i32;
                let wy = oy * ZONE_UNIT + ty as i32 * SAMPLE_UNITS as i32;
                // Invert the pixel↔world map (y-north flip on the row).
                let px = (wx - hm.origin_wx) / SAMPLE_UNITS as i32;
                let py = (hm.origin_wy - wy) / SAMPLE_UNITS as i32;
                let z =
                    if px >= 0 && py >= 0 && (px as usize) < hm.data_w && (py as usize) < hm.data_h
                    {
                        hm.decode_z(hm.pixels[py as usize * hm.pad_w + px as usize])
                    } else {
                        hm.min_z
                    };
                heights[ty * SAMPLES + tx] = z;
            }
        }
        out.insert((ox, oy), ZoneTerrain { heights });
    }
    out
}

/// Smallest UE5 landscape size that fits `n`; falls back to `n` itself if it exceeds the largest
/// recommended size (8129 — a region that big would need tiling, out of scope for now).
fn ue_size(n: usize) -> usize {
    UE_SIZES.iter().copied().find(|&s| s >= n).unwrap_or(n)
}

// ------------------------------- PNG + sidecar I/O -------------------------------

/// Write the heightmap to `png_path` (16-bit grayscale) and its sidecar to the same stem `.txt`.
pub fn write_png(hm: &RegionHeightmap, png_path: impl AsRef<Path>) -> io::Result<()> {
    let png_path = png_path.as_ref();
    let file = std::fs::File::create(png_path)?;
    let mut enc = png::Encoder::new(io::BufWriter::new(file), hm.pad_w as u32, hm.pad_h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Sixteen);
    let mut writer = enc.write_header().map_err(png_err)?;
    // PNG 16-bit is big-endian on the wire.
    let mut bytes = Vec::with_capacity(hm.pixels.len() * 2);
    for &v in &hm.pixels {
        bytes.extend_from_slice(&v.to_be_bytes());
    }
    writer.write_image_data(&bytes).map_err(png_err)?;
    write_sidecar(hm, &png_path.with_extension("txt"))
}

/// Read a heightmap PNG + its sidecar back into a `RegionHeightmap`.
pub fn read_png(png_path: impl AsRef<Path>) -> io::Result<RegionHeightmap> {
    let png_path = png_path.as_ref();
    let mut hm = read_sidecar(&png_path.with_extension("txt"))?;
    let decoder = png::Decoder::new(std::fs::File::open(png_path)?);
    let mut reader = decoder.read_info().map_err(png_err)?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(png_err)?;
    if info.bit_depth != png::BitDepth::Sixteen || info.color_type != png::ColorType::Grayscale {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "heightmap PNG must be 16-bit grayscale",
        ));
    }
    if info.width as usize != hm.pad_w || info.height as usize != hm.pad_h {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "PNG dimensions disagree with sidecar",
        ));
    }
    let n = hm.pad_w * hm.pad_h;
    let mut pixels = Vec::with_capacity(n);
    for chunk in buf[..n * 2].chunks_exact(2) {
        pixels.push(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    hm.pixels = pixels;
    Ok(hm)
}

fn write_sidecar(hm: &RegionHeightmap, path: &Path) -> io::Result<()> {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "# caer region heightmap sidecar — reconstruction params for {}.png",
        path.file_stem()
            .and_then(|x| x.to_str())
            .unwrap_or("heightmap")
    );
    let _ = writeln!(s, "region\t{}", hm.region);
    let _ = writeln!(s, "min_z\t{}", hm.min_z);
    let _ = writeln!(s, "max_z\t{}", hm.max_z);
    let _ = writeln!(s, "origin_wx\t{}", hm.origin_wx);
    let _ = writeln!(
        s,
        "origin_wy\t{}\t# world Y of row 0 (northmost)",
        hm.origin_wy
    );
    let _ = writeln!(s, "sample_units\t{}", SAMPLE_UNITS as i32);
    let _ = writeln!(s, "data_w\t{}", hm.data_w);
    let _ = writeln!(s, "data_h\t{}", hm.data_h);
    let _ = writeln!(s, "pad_w\t{}", hm.pad_w);
    let _ = writeln!(s, "pad_h\t{}", hm.pad_h);
    for (ox, oy) in &hm.zones {
        let _ = writeln!(s, "zone\t{ox}\t{oy}");
    }
    std::fs::write(path, s)
}

fn read_sidecar(path: &Path) -> io::Result<RegionHeightmap> {
    let text = std::fs::read_to_string(path)?;
    let mut region = 0u16;
    let (mut min_z, mut max_z) = (0.0f32, 0.0f32);
    let (mut origin_wx, mut origin_wy) = (0i32, 0i32);
    let (mut data_w, mut data_h, mut pad_w, mut pad_h) = (0usize, 0usize, 0usize, 0usize);
    let mut zones = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let bad = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad sidecar line: {line}"),
            )
        };
        match f[0] {
            "region" => region = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?,
            "min_z" => min_z = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?,
            "max_z" => max_z = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?,
            "origin_wx" => origin_wx = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?,
            "origin_wy" => origin_wy = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?,
            "data_w" => data_w = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?,
            "data_h" => data_h = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?,
            "pad_w" => pad_w = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?,
            "pad_h" => pad_h = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?,
            "sample_units" => {}
            "zone" => {
                let ox = f.get(1).and_then(|s| s.parse().ok()).ok_or_else(bad)?;
                let oy = f.get(2).and_then(|s| s.parse().ok()).ok_or_else(bad)?;
                zones.push((ox, oy));
            }
            _ => {}
        }
    }
    Ok(RegionHeightmap {
        region,
        min_z,
        max_z,
        origin_wx,
        origin_wy,
        data_w,
        data_h,
        pad_w,
        pad_h,
        zones,
        pixels: Vec::new(),
    })
}

fn png_err(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("png: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zone whose height at sample (tx,ty) is a smooth deterministic function of its WORLD
    /// position — so neighbouring zones agree on the shared lattice and there are no seams to
    /// unify. Lets the round-trip assert exact-within-quantization equality everywhere.
    fn synth_zone(ox: i32, oy: i32) -> ((i32, i32), ZoneTerrain) {
        let mut heights = vec![0.0f32; SAMPLES * SAMPLES];
        for ty in 0..SAMPLES {
            for tx in 0..SAMPLES {
                let wx = (ox * ZONE_UNIT + tx as i32 * SAMPLE_UNITS as i32) as f32;
                let wy = (oy * ZONE_UNIT + ty as i32 * SAMPLE_UNITS as i32) as f32;
                heights[ty * SAMPLES + tx] = 2000.0 + 0.001 * wx + 0.0007 * wy;
            }
        }
        ((ox, oy), ZoneTerrain { heights })
    }

    fn field(offsets: &[(i32, i32)]) -> HashMap<(i32, i32), ZoneTerrain> {
        offsets.iter().map(|&(ox, oy)| synth_zone(ox, oy)).collect()
    }

    #[test]
    fn roundtrip_reconstructs_within_quantization() {
        // Four zones tiling a 2×2 block (offsets 8 apart = one full zone, no overlap).
        let zones = field(&[(0, 0), (8, 0), (0, 8), (8, 8)]);
        let hm = export(&zones, 1);
        let back = import(&hm);
        let tol = (hm.max_z - hm.min_z) / 65535.0 * 1.5; // one quantization step + slack

        assert_eq!(back.len(), zones.len());
        let mut worst = 0.0f32;
        for (k, orig) in &zones {
            let got = &back[k];
            for i in 0..SAMPLES * SAMPLES {
                worst = worst.max((orig.heights[i] - got.heights[i]).abs());
            }
        }
        assert!(
            worst <= tol,
            "worst per-sample error {worst} exceeds quantization tol {tol}"
        );
    }

    #[test]
    fn import_is_idempotent() {
        let zones = field(&[(0, 0), (8, 0)]);
        let hm1 = export(&zones, 1);
        let back = import(&hm1);
        let hm2 = export(&back, 1);
        assert_eq!(
            hm1.pixels, hm2.pixels,
            "a second round-trip must reproduce the same image"
        );
        assert_eq!((hm1.data_w, hm1.data_h), (hm2.data_w, hm2.data_h));
    }

    #[test]
    fn pads_to_ue_landscape_size() {
        let zones = field(&[(0, 0)]); // one zone = 256 data samples per axis
        let hm = export(&zones, 1);
        assert_eq!(hm.data_w, SAMPLES);
        assert_eq!(hm.data_h, SAMPLES);
        assert_eq!(hm.pad_w, 505, "256 pads up to the next UE size (505)");
        assert_eq!(hm.pad_h, 505);
        assert_eq!(hm.pixels.len(), 505 * 505);
    }

    #[test]
    fn edit_in_image_space_flows_back_to_zones() {
        // Raise every pixel by a fixed amount in image space; every zone sample should follow.
        let zones = field(&[(0, 0), (8, 0)]);
        let mut hm = export(&zones, 1);
        let bump = ((hm.max_z - hm.min_z) * 0.1).max(1.0);
        let bump_steps = (bump / (hm.max_z - hm.min_z) * 65535.0) as u16;
        for v in hm.pixels.iter_mut() {
            *v = v.saturating_add(bump_steps);
        }
        let back = import(&hm);
        let orig = &zones[&(0, 0)];
        let got = &back[&(0, 0)];
        let delta = got.heights[SAMPLES * SAMPLES / 2] - orig.heights[SAMPLES * SAMPLES / 2];
        assert!(
            (delta - bump).abs() < bump * 0.05,
            "raise did not flow back: {delta} vs {bump}"
        );
    }
}
