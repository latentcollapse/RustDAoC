//! Minimal DDS reader for the client's zone ground textures.
//!
//! The zone `texNNN.mpk` archives hold an 8×8 grid of `texGY-GX.dds` tiles (512×512, DXT1/BC1,
//! no mips) that together form the zone's pre-baked 4096×4096 ground texture. BC1 is uploaded
//! to the GPU as-is (`Bc1RgbaUnormSrgb`), so "decoding" here is just header validation.

use std::io;

/// One decoded (validated) DDS surface: BC1 block data ready for GPU upload.
#[derive(Clone)]
pub struct DdsBc1 {
    pub width: u32,
    pub height: u32,
    /// Top mip level only — 8 bytes per 4×4 block, row-major block order.
    pub data: Vec<u8>,
}

/// A zone's assembled ground texture, in whichever encoding its tiles shipped.
pub enum ZoneGround {
    /// Newer zones: DXT1 tiles, kept compressed for direct GPU upload.
    Bc1(DdsBc1),
    /// Older zones: 8-bit paletted BMP tiles, decoded to RGBA8 (tightly packed, top row first).
    Rgba {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
}

/// Decode an 8-bit paletted, bottom-up BMP (the old zones' tile format) to RGBA8 top-down.
fn read_bmp8(bytes: &[u8]) -> io::Result<(u32, u32, Vec<u8>)> {
    let err = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    if bytes.len() < 54 || &bytes[0..2] != b"BM" {
        return Err(err("not a BMP"));
    }
    let u32_at =
        |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let data_off = u32_at(10) as usize;
    let width = u32_at(18);
    let height = u32_at(22); // positive = bottom-up, the only case these tiles use
    let bpp = u16::from_le_bytes([bytes[28], bytes[29]]);
    if bpp != 8 {
        return Err(err("expected 8-bit paletted BMP"));
    }
    let palette = &bytes[54..54 + 1024]; // 256 × BGRA
    let row_stride = (width as usize).div_ceil(4) * 4;
    if bytes.len() < data_off + row_stride * height as usize {
        return Err(err("truncated BMP payload"));
    }
    let mut out = vec![0u8; (width * height * 4) as usize];
    for y in 0..height as usize {
        let src_row = data_off + (height as usize - 1 - y) * row_stride;
        for x in 0..width as usize {
            let pi = bytes[src_row + x] as usize * 4;
            let dst = (y * width as usize + x) * 4;
            out[dst] = palette[pi + 2]; // R (palette is BGRA)
            out[dst + 1] = palette[pi + 1];
            out[dst + 2] = palette[pi];
            out[dst + 3] = 255;
        }
    }
    Ok((width, height, out))
}

const HEADER_LEN: usize = 128; // magic (4) + DDS_HEADER (124)

/// Compressed-format flavour of a model texture, mirroring the wgpu BC formats it uploads to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DdsFormat {
    Bc1, // DXT1: 8 bytes/block, 1-bit alpha
    Bc2, // DXT3: 16 bytes/block, explicit alpha
    Bc3, // DXT5: 16 bytes/block, interpolated alpha
    /// The rare uncompressed 32-bpp files, converted to tightly-packed RGBA8.
    Rgba8,
}

impl DdsFormat {
    /// Bytes per 4×4 block (compressed) — meaningless for Rgba8.
    fn block_bytes(self) -> usize {
        match self {
            DdsFormat::Bc1 => 8,
            DdsFormat::Bc2 | DdsFormat::Bc3 => 16,
            DdsFormat::Rgba8 => 0,
        }
    }
}

/// A decoded model texture: every mip level present in the file, ready for GPU upload.
/// (Ground tiles use [`DdsBc1`] — single mip; model DDS files ship full mip chains.)
#[derive(Clone)]
pub struct DdsTexture {
    pub width: u32,
    pub height: u32,
    pub format: DdsFormat,
    /// Mip levels, largest first. `mips[i]` is the raw payload for level `i`
    /// (block data for BC*, tightly-packed RGBA8 rows otherwise).
    pub mips: Vec<Vec<u8>>,
}

/// One DXT5 alpha texel from its two endpoints and 3-bit index.
fn alpha_from_index(a0: u8, a1: u8, idx: u8) -> u8 {
    let (x, y) = (f32::from(a0), f32::from(a1));
    let lerp = |n: f32, d: f32| ((x * (d - n) + y * n) / d).round() as u8;
    if a0 > a1 {
        match idx {
            0 => a0,
            1 => a1,
            n => lerp(f32::from(n) - 1.0, 7.0),
        }
    } else {
        match idx {
            0 => a0,
            1 => a1,
            6 => 0,
            7 => 255,
            n => lerp(f32::from(n) - 1.0, 5.0),
        }
    }
}

/// Byte size of one mip level of `w×h` in `format` (BC blocks round up to 4).
fn mip_bytes(format: DdsFormat, w: u32, h: u32) -> usize {
    match format {
        DdsFormat::Rgba8 => (w * h * 4) as usize,
        bc => (w as usize).div_ceil(4) * (h as usize).div_ceil(4) * bc.block_bytes(),
    }
}

/// Parse a model DDS file (DXT1/DXT3/DXT5 or uncompressed 32-bpp) with its full mip chain.
/// The mip count is clamped to what the payload actually contains — some client files declare
/// more levels than they ship — but level 0 must be complete.
pub fn read_model_dds(bytes: &[u8]) -> io::Result<DdsTexture> {
    let err = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    if bytes.len() < HEADER_LEN || &bytes[0..4] != b"DDS " {
        return Err(err("not a DDS file"));
    }
    let u32_at =
        |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let height = u32_at(12);
    let width = u32_at(16);
    let declared_mips = u32_at(28).max(1);
    let pf_flags = u32_at(80);
    // Bytes per pixel *in the file* for the uncompressed formats. Both are stored BGR(A) and both
    // decode to tightly-packed RGBA8, so they share one `DdsFormat` — 24-bpp gains an opaque alpha
    // channel on the way in. `pregame/hib_clouds.dds` is one of the 24-bpp files, and rejecting it
    // is why Hibernia's character screen had a white sky.
    let (format, src_bpp) = if pf_flags & 0x4 != 0 {
        let bc = match &bytes[84..88] {
            b"DXT1" => DdsFormat::Bc1,
            b"DXT2" | b"DXT3" => DdsFormat::Bc2,
            b"DXT4" | b"DXT5" => DdsFormat::Bc3,
            other => {
                return Err(err(&format!(
                    "unsupported fourCC {:?}",
                    String::from_utf8_lossy(other)
                )))
            }
        };
        (bc, 0)
    } else {
        match u32_at(88) {
            32 => (DdsFormat::Rgba8, 4usize),
            24 => (DdsFormat::Rgba8, 3usize),
            other => return Err(err(&format!("unsupported uncompressed bit depth {other}"))),
        }
    };
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err(err("absurd DDS dimensions"));
    }
    // wgpu requires the top mip of a BC texture to be block-aligned.
    if format != DdsFormat::Rgba8 && (width % 4 != 0 || height % 4 != 0) {
        return Err(err("BC dimensions must be block-aligned"));
    }

    let mut mips = Vec::new();
    let (mut w, mut h, mut off) = (width, height, HEADER_LEN);
    for _ in 0..declared_mips {
        // What the level occupies in the file, which is not what it occupies once decoded.
        let n = if format == DdsFormat::Rgba8 {
            w as usize * h as usize * src_bpp
        } else {
            mip_bytes(format, w, h)
        };
        if bytes.len() < off + n {
            break; // file ships fewer levels than the header claims — keep what's real
        }
        if format == DdsFormat::Rgba8 {
            // Both uncompressed layouts store B,G,R(,A) in memory order.
            let src = &bytes[off..off + n];
            let px = if src_bpp == 4 {
                let mut px = src.to_vec();
                for c in px.chunks_exact_mut(4) {
                    c.swap(0, 2);
                }
                px
            } else {
                let mut px = Vec::with_capacity(w as usize * h as usize * 4);
                for c in src.chunks_exact(3) {
                    px.extend_from_slice(&[c[2], c[1], c[0], 255]);
                }
                px
            };
            mips.push(px);
        } else {
            mips.push(bytes[off..off + n].to_vec());
        }
        off += n;
        if w == 1 && h == 1 {
            break;
        }
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    if mips.is_empty() {
        return Err(err("truncated DDS payload (no complete mip 0)"));
    }
    Ok(DdsTexture {
        width,
        height,
        format,
        mips,
    })
}

impl DdsTexture {
    /// Top mip as RGBA8. BC1 decoded; BC2/BC3 use the colour block (alpha flattened to opaque)
    /// so armour still differs when the device has no BC feature.
    #[must_use]
    pub fn rgba8_mip0(&self) -> Option<(u32, u32, Vec<u8>)> {
        let data = self.mips.first()?;
        match self.format {
            DdsFormat::Rgba8 => Some((self.width, self.height, data.clone())),
            DdsFormat::Bc1 => {
                let g = DdsBc1 {
                    width: self.width,
                    height: self.height,
                    data: data.clone(),
                };
                Some((self.width, self.height, bc1_to_rgba(&g)))
            }
            DdsFormat::Bc2 | DdsFormat::Bc3 => {
                let bw = (self.width as usize).div_ceil(4);
                let bh = (self.height as usize).div_ceil(4);
                let blocks = bw * bh;
                let mut bc1 = vec![0u8; blocks * 8];
                for i in 0..blocks {
                    let src = i * 16 + 8;
                    if src + 8 > data.len() {
                        return None;
                    }
                    bc1[i * 8..i * 8 + 8].copy_from_slice(&data[src..src + 8]);
                }
                let g = DdsBc1 {
                    width: self.width,
                    height: self.height,
                    data: bc1,
                };
                let mut rgba = bc1_to_rgba(&g);
                // Decode the ALPHA half of each block too. Dropping it and letting BC1's implicit
                // 255 stand made every alpha reading "100% opaque" no matter what the file held —
                // a measurement that could not fail. Face sheets average ~0.2 alpha, and
                // `skinned.wgsl` discards under 0.4, which is what erased every face.
                for by in 0..bh {
                    for bx in 0..bw {
                        let off = (by * bw + bx) * 16;
                        if off + 8 > data.len() {
                            return None;
                        }
                        let a = &data[off..off + 8];
                        for ty in 0..4 {
                            for tx in 0..4 {
                                let (px, py) = (bx * 4 + tx, by * 4 + ty);
                                if px >= self.width as usize || py >= self.height as usize {
                                    continue;
                                }
                                let t = ty * 4 + tx;
                                let alpha = if self.format == DdsFormat::Bc2 {
                                    // DXT3: 4 bits per texel, straight from the nibble.
                                    let n = (a[t / 2] >> ((t % 2) * 4)) & 0x0F;
                                    n * 17
                                } else {
                                    // DXT5: two endpoints then 3-bit indices, LSB-first.
                                    let (a0, a1) = (a[0], a[1]);
                                    let bits = u64::from_le_bytes([
                                        a[2], a[3], a[4], a[5], a[6], a[7], 0, 0,
                                    ]);
                                    let idx = ((bits >> (3 * t)) & 0x07) as u8;
                                    alpha_from_index(a0, a1, idx)
                                };
                                let o = (py * self.width as usize + px) * 4 + 3;
                                if let Some(slot) = rgba.get_mut(o) {
                                    *slot = alpha;
                                }
                            }
                        }
                    }
                }
                Some((self.width, self.height, rgba))
            }
        }
    }
}

/// Parse a DDS file, requiring an uncompressed-header DXT1 (BC1) surface. Only the top mip is
/// returned (the zone tiles ship without mips anyway).
pub fn read_bc1(bytes: &[u8]) -> io::Result<DdsBc1> {
    let err = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    if bytes.len() < HEADER_LEN || &bytes[0..4] != b"DDS " {
        return Err(err("not a DDS file"));
    }
    let u32_at =
        |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let height = u32_at(12);
    let width = u32_at(16);
    let fourcc = &bytes[84..88];
    if fourcc != b"DXT1" {
        return Err(err("expected DXT1 fourCC"));
    }
    if width % 4 != 0 || height % 4 != 0 {
        return Err(err("BC1 dimensions must be block-aligned"));
    }
    let blocks = (width as usize / 4) * (height as usize / 4);
    let need = blocks * 8;
    if bytes.len() < HEADER_LEN + need {
        return Err(err("truncated BC1 payload"));
    }
    Ok(DdsBc1 {
        width,
        height,
        data: bytes[HEADER_LEN..HEADER_LEN + need].to_vec(),
    })
}

/// Assemble a zone's ground texture from its `texNNN.mpk` bytes: an `n×n` grid of tiles
/// (`tex<COL>-<ROW>.dds` DXT1, or `.bmp` 8-bit paletted in old zones) packed into one image,
/// row-major. Missing tiles stay black.
pub fn zone_ground(tex_mpk: &[u8]) -> io::Result<ZoneGround> {
    let members = crate::read(tex_mpk)?;
    // Old zones ship BMP tiles: decode each to RGBA8 and mosaic on the CPU.
    if members
        .iter()
        .any(|m| m.name.to_ascii_lowercase().ends_with(".bmp"))
    {
        return zone_ground_rgba(&members);
    }
    zone_ground_bc1(&members).map(ZoneGround::Bc1)
}

/// RGBA8 mosaic of 8-bit BMP tiles (old zones).
fn zone_ground_rgba(members: &[crate::MpakEntry]) -> io::Result<ZoneGround> {
    let mut tiles: Vec<(u32, u32, u32, Vec<u8>)> = Vec::new(); // gx, gy, side, rgba
    for m in members {
        let name = m.name.to_ascii_lowercase();
        let Some(rest) = name.strip_prefix("tex") else {
            continue;
        };
        let Some(rest) = rest.strip_suffix(".bmp") else {
            continue;
        };
        let Some((gx, gy)) = rest.split_once('-') else {
            continue;
        };
        let (Ok(gx), Ok(gy)) = (gx.parse::<u32>(), gy.parse::<u32>()) else {
            continue;
        };
        if let Ok((w, h, rgba)) = read_bmp8(&m.data) {
            if w == h {
                tiles.push((gx, gy, w, rgba));
            }
        }
    }
    if tiles.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no BMP tex tiles in archive",
        ));
    }
    let n = tiles
        .iter()
        .map(|(gx, gy, _, _)| gx.max(gy) + 1)
        .max()
        .unwrap_or(1);
    let tile_side = tiles[0].2;
    let side = tile_side * n;
    let mut data = vec![0u8; (side * side * 4) as usize];
    for (gx, gy, ts, rgba) in tiles {
        if ts != tile_side {
            continue;
        }
        for y in 0..ts as usize {
            let src = y * ts as usize * 4;
            let dst =
                ((gy * tile_side) as usize + y) * side as usize * 4 + (gx * tile_side) as usize * 4;
            data[dst..dst + ts as usize * 4].copy_from_slice(&rgba[src..src + ts as usize * 4]);
        }
    }
    Ok(ZoneGround::Rgba {
        width: side,
        height: side,
        data,
    })
}

/// BC1 mosaic of DXT1 tiles (newer zones).
fn zone_ground_bc1(members: &[crate::MpakEntry]) -> io::Result<DdsBc1> {
    let mut tiles: Vec<(u32, u32, DdsBc1)> = Vec::new();
    for m in members {
        let name = m.name.to_ascii_lowercase();
        let Some(rest) = name.strip_prefix("tex") else {
            continue;
        };
        let Some(rest) = rest.strip_suffix(".dds") else {
            continue;
        };
        // Verified against the world: names are tex<COLUMN>-<ROW>.dds — the road network only
        // connects (and farm fields only align under their plow fixtures) with this order.
        let Some((gx, gy)) = rest.split_once('-') else {
            continue;
        };
        let (Ok(gx), Ok(gy)) = (gx.parse::<u32>(), gy.parse::<u32>()) else {
            continue;
        };
        if let Ok(dds) = read_bc1(&m.data) {
            tiles.push((gx, gy, dds));
        }
    }
    if tiles.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no BC1 tex tiles in archive",
        ));
    }
    let n = tiles
        .iter()
        .map(|(gx, gy, _)| gx.max(gy) + 1)
        .max()
        .unwrap_or(1);
    let tile_side = tiles[0].2.width; // tiles are uniform (512) — trust but verify below
    let side = tile_side * n;
    let blocks_per_row_tile = (tile_side / 4) as usize;
    let blocks_per_row = (side / 4) as usize;
    let mut data = vec![0u8; blocks_per_row * (side as usize / 4) * 8];
    for (gx, gy, dds) in tiles {
        if dds.width != tile_side || dds.height != tile_side {
            continue; // odd-sized tile: leave its cell black rather than corrupt the layout
        }
        for by in 0..(tile_side as usize / 4) {
            let src = by * blocks_per_row_tile * 8;
            let dst = ((gy as usize * blocks_per_row_tile + by) * blocks_per_row
                + gx as usize * blocks_per_row_tile)
                * 8;
            data[dst..dst + blocks_per_row_tile * 8]
                .copy_from_slice(&dds.data[src..src + blocks_per_row_tile * 8]);
        }
    }
    Ok(DdsBc1 {
        width: side,
        height: side,
        data,
    })
}

/// Nearest-block 2× downsample of a BC1 surface (stays compressed).
///
/// Zone mosaics are 4096²; CAER's portable device floor is 2048² (`caer_required_limits`).
/// Sampling every other 4×4 block keeps UVs in 0..1 without raising the hardware cap.
#[must_use]
pub fn downsample_bc1_half(src: &DdsBc1) -> DdsBc1 {
    let src_bw = (src.width / 4).max(1) as usize;
    let src_bh = (src.height / 4).max(1) as usize;
    let dst_bw = src_bw.max(2) / 2;
    let dst_bh = src_bh.max(2) / 2;
    let mut data = vec![0u8; dst_bw * dst_bh * 8];
    for y in 0..dst_bh {
        for x in 0..dst_bw {
            let sx = (x * 2).min(src_bw - 1);
            let sy = (y * 2).min(src_bh - 1);
            let src_off = (sy * src_bw + sx) * 8;
            let dst_off = (y * dst_bw + x) * 8;
            data[dst_off..dst_off + 8].copy_from_slice(&src.data[src_off..src_off + 8]);
        }
    }
    DdsBc1 {
        width: (dst_bw as u32) * 4,
        height: (dst_bh as u32) * 4,
        data,
    }
}

/// Decode a BC1 surface to tightly packed RGBA8 (sRGB bytes, opaque unless 1-bit punch).
#[must_use]
pub fn bc1_to_rgba(src: &DdsBc1) -> Vec<u8> {
    let bw = (src.width / 4) as usize;
    let bh = (src.height / 4) as usize;
    let mut out = vec![255u8; (src.width * src.height * 4) as usize];
    for by in 0..bh {
        for bx in 0..bw {
            let off = (by * bw + bx) * 8;
            let mut block = [0u8; 8];
            block.copy_from_slice(&src.data[off..off + 8]);
            let pixels = decode_bc1_block(block);
            for py in 0..4 {
                for px in 0..4 {
                    let x = bx * 4 + px;
                    let y = by * 4 + py;
                    if x >= src.width as usize || y >= src.height as usize {
                        continue;
                    }
                    let dst = (y * src.width as usize + x) * 4;
                    out[dst..dst + 4].copy_from_slice(&pixels[py * 4 + px]);
                }
            }
        }
    }
    out
}

fn decode_bc1_block(block: [u8; 8]) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let rgb = |c: u16| -> [u8; 3] {
        let r = (c >> 11) & 31;
        let g = (c >> 5) & 63;
        let b = c & 31;
        // Widen before multiply — `u8 * 255` overflows in debug builds.
        [
            ((r * 255 + 15) / 31) as u8,
            ((g * 255 + 31) / 63) as u8,
            ((b * 255 + 15) / 31) as u8,
        ]
    };
    let a = rgb(c0);
    let b = rgb(c1);
    let mut palette = [[0u8; 4]; 4];
    palette[0] = [a[0], a[1], a[2], 255];
    palette[1] = [b[0], b[1], b[2], 255];
    if c0 > c1 {
        palette[2] = [
            ((2 * a[0] as u16 + b[0] as u16) / 3) as u8,
            ((2 * a[1] as u16 + b[1] as u16) / 3) as u8,
            ((2 * a[2] as u16 + b[2] as u16) / 3) as u8,
            255,
        ];
        palette[3] = [
            ((a[0] as u16 + 2 * b[0] as u16) / 3) as u8,
            ((a[1] as u16 + 2 * b[1] as u16) / 3) as u8,
            ((a[2] as u16 + 2 * b[2] as u16) / 3) as u8,
            255,
        ];
    } else {
        palette[2] = [
            ((a[0] as u16 + b[0] as u16) / 2) as u8,
            ((a[1] as u16 + b[1] as u16) / 2) as u8,
            ((a[2] as u16 + b[2] as u16) / 2) as u8,
            255,
        ];
        palette[3] = [0, 0, 0, 0];
    }
    let mut pixels = [[0u8; 4]; 16];
    for i in 0..16 {
        pixels[i] = palette[((bits >> (i * 2)) & 3) as usize];
    }
    pixels
}

/// Repeated half-size until both dimensions are `≤ max_dim` (at least one 4×4 block).
#[must_use]
pub fn clamp_bc1_to_max(src: &DdsBc1, max_dim: u32) -> DdsBc1 {
    let cap = max_dim.max(4);
    let mut cur = src.clone();
    while cur.width > cap || cur.height > cap {
        cur = downsample_bc1_half(&cur);
    }
    cur
}

/// Box-filter 2× downsample of tightly packed RGBA8.
#[must_use]
pub fn downsample_rgba_half(width: u32, height: u32, data: &[u8]) -> (u32, u32, Vec<u8>) {
    let dw = (width / 2).max(1);
    let dh = (height / 2).max(1);
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        for x in 0..dw {
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for oy in 0..2 {
                let sy = (y * 2 + oy).min(height - 1);
                for ox in 0..2 {
                    let sx = (x * 2 + ox).min(width - 1);
                    let i = ((sy * width + sx) * 4) as usize;
                    acc[0] += data[i] as u32;
                    acc[1] += data[i + 1] as u32;
                    acc[2] += data[i + 2] as u32;
                    acc[3] += data[i + 3] as u32;
                    n += 1;
                }
            }
            let o = ((y * dw + x) * 4) as usize;
            out[o] = (acc[0] / n) as u8;
            out[o + 1] = (acc[1] / n) as u8;
            out[o + 2] = (acc[2] / n) as u8;
            out[o + 3] = (acc[3] / n) as u8;
        }
    }
    (dw, dh, out)
}

/// Repeated half-size RGBA until both dimensions are `≤ max_dim`.
#[must_use]
pub fn clamp_rgba_to_max(
    width: u32,
    height: u32,
    data: &[u8],
    max_dim: u32,
) -> (u32, u32, Vec<u8>) {
    let cap = max_dim.max(1);
    let mut w = width;
    let mut h = height;
    let mut px = data.to_vec();
    while w > cap || h > cap {
        let (nw, nh, np) = downsample_rgba_half(w, h, &px);
        w = nw;
        h = nh;
        px = np;
    }
    (w, h, px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_bc1_4096_fits_2048_device_floor() {
        let src = DdsBc1 {
            width: 4096,
            height: 4096,
            data: vec![0u8; (4096 / 4) * (4096 / 4) * 8],
        };
        let out = clamp_bc1_to_max(&src, 2048);
        assert_eq!(out.width, 2048);
        assert_eq!(out.height, 2048);
        assert_eq!(out.data.len(), (2048 / 4) * (2048 / 4) * 8);
    }

    #[test]
    fn clamp_rgba_4096_fits_2048_device_floor() {
        let src = vec![7u8; 4096 * 4096 * 4];
        let (w, h, out) = clamp_rgba_to_max(4096, 4096, &src, 2048);
        assert_eq!((w, h), (2048, 2048));
        assert_eq!(out.len(), 2048 * 2048 * 4);
    }

    /// Build a single-mip uncompressed DDS with `bpp` bits per pixel and BGR(A) payload.
    fn uncompressed_dds(w: u32, h: u32, bpp: u32, payload: &[u8]) -> Vec<u8> {
        let mut f = vec![0u8; HEADER_LEN];
        f[0..4].copy_from_slice(b"DDS ");
        f[4..8].copy_from_slice(&124u32.to_le_bytes());
        f[12..16].copy_from_slice(&h.to_le_bytes());
        f[16..20].copy_from_slice(&w.to_le_bytes());
        f[28..32].copy_from_slice(&1u32.to_le_bytes());
        f[80..84].copy_from_slice(&0x40u32.to_le_bytes()); // DDPF_RGB, no fourCC
        f[88..92].copy_from_slice(&bpp.to_le_bytes());
        f.extend_from_slice(payload);
        f
    }

    /// `pregame/hib_clouds.dds` is 1024×512 24-bpp. Before this decoded, the miss bound the white
    /// fallback and Hibernia's character screen had a blank sky.
    #[test]
    fn uncompressed_24bpp_expands_to_opaque_rgba() {
        // Two pixels, stored B,G,R: pure red then pure blue.
        let file = uncompressed_dds(2, 1, 24, &[0, 0, 255, 255, 0, 0]);
        let tex = read_model_dds(&file).expect("24-bpp DDS must decode");
        assert_eq!((tex.width, tex.height), (2, 1));
        assert_eq!(tex.format, DdsFormat::Rgba8);
        assert_eq!(
            tex.mips[0],
            vec![255, 0, 0, 255, 0, 0, 255, 255],
            "24-bpp is BGR with an implied opaque alpha"
        );
    }

    /// The 32-bpp path must be unmoved by the 24-bpp one: same swizzle, alpha preserved.
    #[test]
    fn uncompressed_32bpp_keeps_its_alpha() {
        let file = uncompressed_dds(1, 1, 32, &[10, 20, 30, 40]);
        let tex = read_model_dds(&file).expect("32-bpp DDS must decode");
        assert_eq!(tex.mips[0], vec![30, 20, 10, 40]);
    }

    /// A depth we cannot expand still fails rather than reading garbage as pixels.
    #[test]
    fn uncompressed_16bpp_is_still_rejected() {
        let file = uncompressed_dds(2, 2, 16, &[0u8; 8]);
        assert!(read_model_dds(&file).is_err());
    }
}
