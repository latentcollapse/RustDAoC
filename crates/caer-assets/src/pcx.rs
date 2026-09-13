//! Minimal decoder for the 8-bit RLE PCX images DAoC uses for its per-zone maps
//! (`terrain.pcx` heightmap, `offset.pcx` coarse offsets, plus water/shade/etc.).
//!
//! All the zone maps are version-5, 8-bit, single-plane, RLE-encoded PCX. We only need the raw
//! index grid (one byte per pixel), not the palette — for `terrain`/`offset` that byte *is* the
//! datum. RLE: a byte with its top two bits set (`>= 0xC0`) is a run — the low 6 bits are the
//! count and the next byte is the value; any other byte is a single literal pixel.

use std::io;

/// A decoded 8-bit PCX: `data[y * width + x]` is the pixel/index byte.
pub struct Pcx8 {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Pcx8 {
    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.data[y * self.width + x]
    }
}

/// Decode an 8-bit RLE PCX. Errors if the header isn't the expected shape.
pub fn decode8(bytes: &[u8]) -> io::Result<Pcx8> {
    if bytes.len() < 128 || bytes[0] != 0x0A {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a PCX (bad manufacturer byte)",
        ));
    }
    let bpp = bytes[3];
    let planes = bytes[65];
    if bpp != 8 || planes != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported PCX: {bpp}bpp {planes} planes (want 8bpp/1plane)"),
        ));
    }
    let u16le = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]) as usize;
    let (xmin, ymin, xmax, ymax) = (u16le(4), u16le(6), u16le(8), u16le(10));
    let width = xmax + 1 - xmin;
    let height = ymax + 1 - ymin;
    let bytes_per_line = u16le(66); // may exceed width (row padding)

    let mut out = vec![0u8; width * height];
    let mut src = 128; // pixel data follows the fixed 128-byte header
    for row in 0..height {
        let mut col = 0usize;
        // Decode a full scanline of `bytes_per_line`, keeping only the first `width` pixels.
        while col < bytes_per_line {
            if src >= bytes.len() {
                break;
            }
            let b = bytes[src];
            src += 1;
            let (count, value) = if b >= 0xC0 {
                let v = *bytes.get(src).unwrap_or(&0);
                src += 1;
                ((b & 0x3F) as usize, v)
            } else {
                (1, b)
            };
            for _ in 0..count {
                if col < width {
                    out[row * width + col] = value;
                }
                col += 1;
            }
        }
    }
    Ok(Pcx8 {
        width,
        height,
        data: out,
    })
}
