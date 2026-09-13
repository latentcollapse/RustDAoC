//! 8-bit paletted and 24-bit BMP → RGBA8 (login chrome, older zone tiles).
//!
//! DAoC's `data/login2.mpk` mixes formats: `btn_play_*.bmp` are 8-bit paletted; the wider
//! chrome buttons are 24-bit BGR. Palette length for 8-bit is `bfOffBits - 54`, not always 256
//! entries (e.g. `logo_daoc.bmp` ships 192 colours).

use std::io;

use crate::tga::TgaImage;

/// Decode an 8- or 24-bit BMP to tightly-packed RGBA8, top-left origin.
pub fn decode(bytes: &[u8]) -> io::Result<TgaImage> {
    let err = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_string());
    if bytes.len() < 54 || &bytes[0..2] != b"BM" {
        return Err(err("not a BMP"));
    }
    let u32_at =
        |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let data_off = u32_at(10) as usize;
    let width = u32_at(18);
    let height_raw = i32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
    let bottom_up = height_raw > 0;
    let height = height_raw.unsigned_abs();
    let bpp = u16::from_le_bytes([bytes[28], bytes[29]]);
    if data_off > bytes.len() {
        return Err(err("bad BMP data offset"));
    }
    let stride = row_stride(width, bpp);
    let row_src = |y: usize| -> usize {
        let src_y = if bottom_up {
            height as usize - 1 - y
        } else {
            y
        };
        data_off + src_y * stride
    };
    let mut out = vec![0u8; (width * height * 4) as usize];
    match bpp {
        8 => {
            if data_off < 54 {
                return Err(err("bad BMP data offset"));
            }
            let palette = &bytes[54..data_off];
            if !palette.len().is_multiple_of(4) {
                return Err(err("BMP palette not BGRA-aligned"));
            }
            let ncolors = palette.len() / 4;
            if bytes.len() < data_off + stride * height as usize {
                return Err(err("truncated BMP payload"));
            }
            for y in 0..height as usize {
                let src_row = row_src(y);
                for x in 0..width as usize {
                    let idx = bytes[src_row + x] as usize;
                    if idx >= ncolors {
                        return Err(err("BMP pixel index past palette"));
                    }
                    let pi = idx * 4;
                    let dst = (y * width as usize + x) * 4;
                    out[dst] = palette[pi + 2];
                    out[dst + 1] = palette[pi + 1];
                    out[dst + 2] = palette[pi];
                    // Mythic login chrome uses magenta as the transparent key colour.
                    out[dst + 3] = if out[dst] == 255 && out[dst + 1] == 0 && out[dst + 2] == 255 {
                        0
                    } else {
                        255
                    };
                }
            }
        }
        24 => {
            if bytes.len() < data_off + stride * height as usize {
                return Err(err("truncated BMP payload"));
            }
            for y in 0..height as usize {
                let src_row = row_src(y);
                for x in 0..width as usize {
                    let s = src_row + x * 3;
                    let dst = (y * width as usize + x) * 4;
                    out[dst] = bytes[s + 2];
                    out[dst + 1] = bytes[s + 1];
                    out[dst + 2] = bytes[s];
                    out[dst + 3] = if out[dst] == 255 && out[dst + 1] == 0 && out[dst + 2] == 255 {
                        0
                    } else {
                        255
                    };
                }
            }
        }
        _ => return Err(err("unsupported BMP bit depth (need 8 or 24)")),
    }
    Ok(TgaImage {
        width,
        height,
        rgba: out,
    })
}

fn row_stride(width: u32, bpp: u16) -> usize {
    let bits = width as usize * bpp as usize;
    bits.div_ceil(32) * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login2_play_button_decodes() {
        let root = std::env::var_os("CAER_CLIENT").map(std::path::PathBuf::from);
        let Some(root) = root.filter(|p| p.is_dir()) else {
            return;
        };
        let path = root.join("data/login2.mpk");
        let Ok(members) = crate::open(&path) else {
            return;
        };
        let m = members
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case("btn_play_norm.bmp"))
            .expect("btn_play_norm.bmp");
        let img = decode(&m.data).expect("decode");
        assert_eq!((img.width, img.height), (84, 26));
        assert_eq!(img.rgba.len(), 84 * 26 * 4);
    }

    #[test]
    fn login2_account_button_24bpp_decodes() {
        let root = std::env::var_os("CAER_CLIENT").map(std::path::PathBuf::from);
        let Some(root) = root.filter(|p| p.is_dir()) else {
            return;
        };
        let path = root.join("data/login2.mpk");
        let Ok(members) = crate::open(&path) else {
            return;
        };
        let m = members
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case("btn_account_norm.bmp"))
            .expect("btn_account_norm.bmp");
        let img = decode(&m.data).expect("decode 24bpp");
        assert_eq!((img.width, img.height), (90, 34));
    }
}
