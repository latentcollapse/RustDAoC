//! TGA decoder — the client's UI art, bitmap fonts, and a long tail of textures.
//!
//! 533 `.tga` files ship with the client and they are not one format but seven, so this decodes the
//! whole uncompressed-and-RLE TGA feature set rather than just the two variants the UI happens to
//! need. Measured across the client:
//!
//! ```text
//!   type  1  colormapped  8bpp   113   type  9  RLE colormapped  8bpp    1
//!   type  2  truecolor   16bpp     6   type 10  RLE truecolor   24bpp    3
//!   type  2  truecolor   24bpp   130   type 10  RLE truecolor   32bpp    2
//!   type  2  truecolor   32bpp   274   type  3  greyscale        8bpp    4
//! ```
//!
//! **Every one of them is bottom-left origin.** TGA stores rows bottom-to-top unless descriptor bit
//! 5 says otherwise, and not one client file sets it — so a decoder that ignores the flag produces
//! a vertically mirrored image. That is the single most likely way this goes wrong, and it is the
//! kind of wrong that looks plausible on a symmetric texture and obviously broken on a font atlas.
//!
//! Output is always tightly-packed RGBA8, top-left origin, which is what the GPU upload path wants.

use std::io;

/// A decoded image: tightly-packed RGBA8 rows, top-left origin.
#[derive(Clone, PartialEq, Eq)]
pub struct TgaImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major from the TOP row.
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for TgaImage {
    /// Deliberately does not dump the pixels — a failing assert on a 256×256 atlas would otherwise
    /// print a quarter of a megabyte.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TgaImage({}x{}, {} bytes)",
            self.width,
            self.height,
            self.rgba.len()
        )
    }
}

impl TgaImage {
    /// The RGBA of one pixel, indexed from the TOP-left.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        Some([
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ])
    }
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Image type codes from the TGA header's byte 2.
const T_COLORMAP: u8 = 1;
const T_TRUECOLOR: u8 = 2;
const T_GREY: u8 = 3;
const T_RLE_COLORMAP: u8 = 9;
const T_RLE_TRUECOLOR: u8 = 10;
const T_RLE_GREY: u8 = 11;

/// Expand one stored sample to RGBA8.
///
/// TGA channel order is **BGR(A)**, not RGB — swapping them is the second classic way to get a
/// plausible-looking but wrong image (skin tones turn blue).
fn sample_to_rgba(px: &[u8], bpp: u8) -> [u8; 4] {
    match bpp {
        32 => [px[2], px[1], px[0], px[3]],
        24 => [px[2], px[1], px[0], 255],
        // 16-bit is ARGB1555: 5 bits per colour and a single alpha BIT. Scaling 0..31 to 0..255 by
        // `<<3` alone would cap white at 248, so replicate the high bits into the low ones.
        16 => {
            let v = u16::from(px[0]) | (u16::from(px[1]) << 8);
            let expand = |c: u16| ((c << 3) | (c >> 2)) as u8;
            [
                expand((v >> 10) & 0x1F),
                expand((v >> 5) & 0x1F),
                expand(v & 0x1F),
                if v & 0x8000 != 0 { 255 } else { 0 },
            ]
        }
        // Greyscale: one channel replicated, fully opaque.
        8 => [px[0], px[0], px[0], 255],
        _ => [0, 0, 0, 255],
    }
}

/// Decode a TGA file to RGBA8.
pub fn decode(bytes: &[u8]) -> io::Result<TgaImage> {
    if bytes.len() < 18 {
        return Err(err("short header"));
    }
    let id_len = bytes[0] as usize;
    let cmap_type = bytes[1];
    let image_type = bytes[2];
    let cmap_len = u16::from_le_bytes([bytes[5], bytes[6]]) as usize;
    let cmap_entry_bits = bytes[7];
    let width = u16::from_le_bytes([bytes[12], bytes[13]]) as u32;
    let height = u16::from_le_bytes([bytes[14], bytes[15]]) as u32;
    let bpp = bytes[16];
    let descriptor = bytes[17];
    // Bit 5 set = rows already run top-to-bottom. Clear (every client file) = bottom-up.
    let top_origin = descriptor & 0x20 != 0;
    // Bit 4 set = columns run right-to-left. Rare, but cheap to honour.
    let right_to_left = descriptor & 0x10 != 0;

    if width == 0 || height == 0 {
        return Err(err(format!("zero-sized image {width}x{height}")));
    }
    let rle = matches!(image_type, T_RLE_COLORMAP | T_RLE_TRUECOLOR | T_RLE_GREY);
    let indexed = matches!(image_type, T_COLORMAP | T_RLE_COLORMAP);
    if !matches!(
        image_type,
        T_COLORMAP | T_TRUECOLOR | T_GREY | T_RLE_COLORMAP | T_RLE_TRUECOLOR | T_RLE_GREY
    ) {
        return Err(err(format!("unsupported TGA image type {image_type}")));
    }
    if !matches!(bpp, 8 | 16 | 24 | 32) {
        return Err(err(format!("unsupported bpp {bpp}")));
    }

    let mut at = 18 + id_len;

    // Colour map. Present when `cmap_type` says so — which can happen even for a truecolor image,
    // where it must be SKIPPED rather than used.
    let mut palette: Vec<[u8; 4]> = Vec::new();
    if cmap_type == 1 {
        let entry_bytes = match cmap_entry_bits {
            15 | 16 => 2,
            24 => 3,
            32 => 4,
            other => return Err(err(format!("unsupported colour-map entry size {other}"))),
        };
        let need = cmap_len * entry_bytes;
        if at + need > bytes.len() {
            return Err(err("colour map runs past end of file"));
        }
        if indexed {
            palette.reserve(cmap_len);
            for i in 0..cmap_len {
                let p = &bytes[at + i * entry_bytes..];
                // A 15-bit map has no alpha bit set; treat it as opaque.
                let mut c = sample_to_rgba(
                    p,
                    if cmap_entry_bits == 15 {
                        16
                    } else {
                        cmap_entry_bits
                    },
                );
                if cmap_entry_bits == 15 {
                    c[3] = 255;
                }
                palette.push(c);
            }
        }
        at += need;
    }
    if indexed && palette.is_empty() {
        return Err(err("colormapped image with no colour map"));
    }

    let sample_bytes = (bpp / 8) as usize;
    let pixels = (width * height) as usize;
    let mut out = vec![0u8; pixels * 4];

    // Decode into a linear top-left-origin buffer, mapping each source pixel to its destination as
    // it is produced. Doing the flip during the walk avoids allocating a second full image.
    let place = |i: usize, rgba: [u8; 4], out: &mut Vec<u8>| {
        let (sx, sy) = ((i as u32) % width, (i as u32) / width);
        let dx = if right_to_left { width - 1 - sx } else { sx };
        let dy = if top_origin { sy } else { height - 1 - sy };
        let o = ((dy * width + dx) * 4) as usize;
        out[o..o + 4].copy_from_slice(&rgba);
    };

    let resolve = |px: &[u8]| -> io::Result<[u8; 4]> {
        if indexed {
            let idx = match bpp {
                8 => usize::from(px[0]),
                16 => usize::from(u16::from_le_bytes([px[0], px[1]])),
                other => return Err(err(format!("colormapped image with {other} bpp indices"))),
            };
            // An out-of-range index is corruption; a transparent pixel is a safer answer than a
            // panic, and the file still decodes.
            Ok(palette.get(idx).copied().unwrap_or([0, 0, 0, 0]))
        } else {
            Ok(sample_to_rgba(px, bpp))
        }
    };

    if rle {
        let mut i = 0usize;
        while i < pixels {
            if at >= bytes.len() {
                return Err(err("RLE data ended early"));
            }
            let packet = bytes[at];
            at += 1;
            let count = usize::from(packet & 0x7F) + 1;
            if i + count > pixels {
                return Err(err("RLE packet overruns the image"));
            }
            if packet & 0x80 != 0 {
                // Run: one sample repeated `count` times.
                if at + sample_bytes > bytes.len() {
                    return Err(err("RLE run ended early"));
                }
                let c = resolve(&bytes[at..])?;
                at += sample_bytes;
                for k in 0..count {
                    place(i + k, c, &mut out);
                }
            } else {
                // Literal: `count` distinct samples.
                if at + count * sample_bytes > bytes.len() {
                    return Err(err("RLE literal ended early"));
                }
                for k in 0..count {
                    let c = resolve(&bytes[at + k * sample_bytes..])?;
                    place(i + k, c, &mut out);
                }
                at += count * sample_bytes;
            }
            i += count;
        }
    } else {
        if at + pixels * sample_bytes > bytes.len() {
            return Err(err(format!(
                "pixel data truncated: need {} bytes, have {}",
                pixels * sample_bytes,
                bytes.len().saturating_sub(at)
            )));
        }
        for i in 0..pixels {
            let c = resolve(&bytes[at + i * sample_bytes..])?;
            place(i, c, &mut out);
        }
    }

    // A file whose alpha bit is never set ANYWHERE decodes to a wholly invisible texture, which is
    // never the intent. Measured on this client: all six 16-bit files declare one alpha bit and
    // leave it clear in every pixel — they are really RGB555 — so taken literally they vanish.
    //
    // Deliberately keyed on "no pixel uses alpha" rather than on the declared attribute-bit count.
    // Trusting the count is the tempting version and it is WRONG: plenty of 32-bit TGAs declare
    // zero attribute bits and carry real alpha anyway, and promoting those to opaque would destroy
    // information. This rule cannot — if no pixel sets alpha there is no alpha to lose.
    if matches!(bpp, 16 | 32) && out.chunks_exact(4).all(|p| p[3] == 0) {
        for p in out.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }

    Ok(TgaImage {
        width,
        height,
        rgba: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an uncompressed TGA for testing. `desc` carries the origin bits.
    fn tga(image_type: u8, bpp: u8, w: u16, h: u16, desc: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; 18];
        v[2] = image_type;
        v[12..14].copy_from_slice(&w.to_le_bytes());
        v[14..16].copy_from_slice(&h.to_le_bytes());
        v[16] = bpp;
        v[17] = desc;
        v.extend_from_slice(body);
        v
    }

    /// The bug this format invites: rows are stored BOTTOM-UP unless bit 5 says otherwise, and no
    /// client file sets it. A decoder that ignores the flag mirrors every image vertically.
    #[test]
    fn bottom_up_rows_are_flipped_and_top_down_rows_are_not() {
        // 1x2, 24bpp BGR. Stored first row is the BOTTOM one: red below, blue above.
        let body = [0, 0, 255, /* red   */ 255, 0, 0 /* blue */];
        let img = decode(&tga(T_TRUECOLOR, 24, 1, 2, 0x00, &body)).unwrap();
        assert_eq!(
            img.pixel(0, 0),
            Some([0, 0, 255, 255]),
            "top row must be the LAST row stored"
        );
        assert_eq!(
            img.pixel(0, 1),
            Some([255, 0, 0, 255]),
            "bottom row must be the FIRST stored"
        );

        // Same bytes with the top-origin bit set must come out the other way up.
        let img = decode(&tga(T_TRUECOLOR, 24, 1, 2, 0x20, &body)).unwrap();
        assert_eq!(img.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(img.pixel(0, 1), Some([0, 0, 255, 255]));
    }

    /// TGA stores BGRA, not RGBA. Swapping them yields a plausible-looking but wrong image.
    #[test]
    fn channels_are_bgra_not_rgba() {
        // One pixel: B=10 G=20 R=30 A=40.
        let img = decode(&tga(T_TRUECOLOR, 32, 1, 1, 0x20, &[10, 20, 30, 40])).unwrap();
        assert_eq!(
            img.pixel(0, 0),
            Some([30, 20, 10, 40]),
            "R and B must be swapped, A kept"
        );
        // 24-bit has no alpha channel and must come out opaque, not transparent.
        let img = decode(&tga(T_TRUECOLOR, 24, 1, 1, 0x20, &[10, 20, 30])).unwrap();
        assert_eq!(img.pixel(0, 0), Some([30, 20, 10, 255]));
    }

    /// 16-bit is ARGB1555. Scaling 5 bits with `<<3` alone caps white at 248 — the high bits must
    /// be replicated into the low ones so full-white stays 255.
    #[test]
    fn sixteen_bit_expands_to_full_range() {
        let white: u16 = 0x8000 | (31 << 10) | (31 << 5) | 31;
        let img = decode(&tga(T_TRUECOLOR, 16, 1, 1, 0x20, &white.to_le_bytes())).unwrap();
        assert_eq!(
            img.pixel(0, 0),
            Some([255, 255, 255, 255]),
            "full 5-bit channels must reach 255"
        );
        // The alpha BIT is honoured when the image actually USES it: one pixel set, one clear.
        let opaque: u16 = 0x8000 | 31;
        let clear: u16 = 31 << 10;
        let mut body = opaque.to_le_bytes().to_vec();
        body.extend_from_slice(&clear.to_le_bytes());
        // Declare 1 attribute bit (descriptor low nibble) alongside the top-origin flag.
        let img = decode(&tga(T_TRUECOLOR, 16, 2, 1, 0x21, &body)).unwrap();
        assert_eq!(img.pixel(0, 0).unwrap()[3], 255);
        assert_eq!(
            img.pixel(1, 0).unwrap()[3],
            0,
            "a used alpha bit must still mean transparent"
        );
    }

    /// A 16- or 32-bit file whose alpha is never set decodes to a wholly invisible texture if taken
    /// literally. All six of the client's 16-bit files are like this — they claim one alpha bit and
    /// leave it clear everywhere, i.e. they are really RGB555 — so this must come out opaque.
    #[test]
    fn alpha_that_is_declared_but_never_used_becomes_opaque() {
        // Two pixels, 1 attribute bit declared, top bit clear in both.
        let a: u16 = 31 << 10;
        let b: u16 = 31;
        let mut body = a.to_le_bytes().to_vec();
        body.extend_from_slice(&b.to_le_bytes());
        let img = decode(&tga(T_TRUECOLOR, 16, 2, 1, 0x21, &body)).unwrap();
        assert_eq!(
            img.pixel(0, 0).unwrap()[3],
            255,
            "an all-transparent image is never the intent"
        );
        assert_eq!(img.pixel(1, 0).unwrap()[3], 255);
        // Colour is untouched by the promotion.
        assert_eq!(img.pixel(0, 0).unwrap()[0], 255);

        // The rule is "no pixel uses alpha", NOT "the header declared none" — a 32-bit file that
        // declares zero attribute bits but carries real alpha must keep it.
        let img = decode(&tga(
            T_TRUECOLOR,
            32,
            2,
            1,
            0x20,
            &[0, 0, 0, 200, 0, 0, 0, 0],
        ))
        .unwrap();
        assert_eq!(
            img.pixel(0, 0).unwrap()[3],
            200,
            "declared 0 attr bits but alpha IS used: keep it"
        );
        assert_eq!(img.pixel(1, 0).unwrap()[3], 0);

        // …and a 32-bit file that declares and uses alpha keeps it.
        let img = decode(&tga(
            T_TRUECOLOR,
            32,
            2,
            1,
            0x28,
            &[0, 0, 0, 255, 0, 0, 0, 0],
        ))
        .unwrap();
        assert_eq!(img.pixel(0, 0).unwrap()[3], 255);
        assert_eq!(img.pixel(1, 0).unwrap()[3], 0, "real alpha must survive");
    }

    /// **A deliberately blanked sheet decodes OPAQUE, not invisible.**
    ///
    /// The promotion above reads "no pixel uses alpha" as an authoring slip and forces the image
    /// opaque. At the all-zero boundary that inverts the author's intent: a client that ships a
    /// blanked placeholder rather than deleting the member gets a solid BLACK RECTANGLE on screen.
    ///
    /// Eden does exactly this — all three realm crests in its `pregame.mpk` are RGBA `0,0,0,0` for
    /// every texel, because its realm plate paints the kings into the background instead — and the
    /// three black boxes over that artwork are this line. Pinned so the behaviour is a decision
    /// somebody can find, rather than a surprise rediscovered from a screenshot.
    #[test]
    fn an_entirely_transparent_sheet_is_promoted_to_opaque() {
        let img = decode(&tga(T_TRUECOLOR, 32, 2, 1, 0x28, &[0, 0, 0, 0, 0, 0, 0, 0])).unwrap();
        assert_eq!(
            img.pixel(0, 0).unwrap()[3],
            255,
            "every texel transparent trips the promotion, so the sheet draws as solid colour"
        );
        assert_eq!(img.pixel(1, 0).unwrap()[3], 255);
        assert_eq!(
            &img.pixel(0, 0).unwrap()[0..3],
            &[0, 0, 0],
            "and the colour it draws is whatever the blanked art carries — here, black"
        );
    }

    /// Colormapped images resolve through the palette; the palette is itself BGR.
    #[test]
    fn colormapped_images_resolve_through_the_palette() {
        let mut v = vec![0u8; 18];
        v[1] = 1; // colour map present
        v[2] = T_COLORMAP;
        v[5..7].copy_from_slice(&2u16.to_le_bytes()); // 2 entries
        v[7] = 24; // 24-bit entries
        v[12..14].copy_from_slice(&2u16.to_le_bytes());
        v[14..16].copy_from_slice(&1u16.to_le_bytes());
        v[16] = 8;
        v[17] = 0x20;
        v.extend_from_slice(&[0, 0, 255]); // entry 0: BGR red
        v.extend_from_slice(&[255, 0, 0]); // entry 1: BGR blue
        v.extend_from_slice(&[1, 0]); // pixels: blue, red
        let img = decode(&v).unwrap();
        assert_eq!(img.pixel(0, 0), Some([0, 0, 255, 255]));
        assert_eq!(img.pixel(1, 0), Some([255, 0, 0, 255]));
    }

    /// RLE: the high bit marks a repeated run, otherwise the packet is a literal batch. Both must
    /// place pixels through the same flip, or an RLE image comes out upside down while an
    /// uncompressed one doesn't.
    #[test]
    fn rle_runs_and_literals_both_decode() {
        // 4x1, 24bpp: a run of 2 red, then a literal of 2 (green, blue).
        let mut body = vec![0x81, 0, 0, 255]; // run, count 2, BGR red
        body.extend_from_slice(&[0x01, 0, 255, 0, 255, 0, 0]); // literal, count 2
        let img = decode(&tga(T_RLE_TRUECOLOR, 24, 4, 1, 0x20, &body)).unwrap();
        assert_eq!(img.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(img.pixel(1, 0), Some([255, 0, 0, 255]));
        assert_eq!(img.pixel(2, 0), Some([0, 255, 0, 255]));
        assert_eq!(img.pixel(3, 0), Some([0, 0, 255, 255]));
    }

    /// An RLE image must flip exactly like an uncompressed one.
    #[test]
    fn rle_honours_the_origin_flag_too() {
        // 1x2 bottom-up: a literal of 2 — first stored pixel is the BOTTOM row.
        let body = [0x01, 0, 0, 255, 255, 0, 0];
        let img = decode(&tga(T_RLE_TRUECOLOR, 24, 1, 2, 0x00, &body)).unwrap();
        assert_eq!(
            img.pixel(0, 1),
            Some([255, 0, 0, 255]),
            "first stored pixel is the bottom row"
        );
        assert_eq!(img.pixel(0, 0), Some([0, 0, 255, 255]));
    }

    /// Greyscale replicates its one channel and is opaque.
    #[test]
    fn greyscale_replicates_and_is_opaque() {
        let img = decode(&tga(T_GREY, 8, 2, 1, 0x20, &[0, 128])).unwrap();
        assert_eq!(img.pixel(0, 0), Some([0, 0, 0, 255]));
        assert_eq!(img.pixel(1, 0), Some([128, 128, 128, 255]));
    }

    /// The image-ID field sits between the header and the data and must be skipped, or every
    /// pixel is read `id_len` bytes early.
    #[test]
    fn the_image_id_field_is_skipped() {
        let mut v = tga(T_TRUECOLOR, 24, 1, 1, 0x20, &[]);
        v[0] = 5; // 5-byte ID field
        v.extend_from_slice(b"HELLO");
        v.extend_from_slice(&[10, 20, 30]);
        assert_eq!(decode(&v).unwrap().pixel(0, 0), Some([30, 20, 10, 255]));
    }

    /// Truncated and nonsensical files must error rather than panic or return a half-image.
    #[test]
    fn malformed_files_are_rejected_cleanly() {
        assert!(decode(&[]).is_err(), "empty");
        assert!(
            decode(&tga(T_TRUECOLOR, 24, 4, 4, 0x20, &[1, 2, 3])).is_err(),
            "truncated pixels"
        );
        assert!(
            decode(&tga(T_TRUECOLOR, 0, 1, 1, 0x20, &[0])).is_err(),
            "bad bpp"
        );
        assert!(
            decode(&tga(99, 24, 1, 1, 0x20, &[0, 0, 0])).is_err(),
            "unknown image type"
        );
        assert!(
            decode(&tga(T_TRUECOLOR, 24, 0, 1, 0x20, &[])).is_err(),
            "zero width"
        );
        // An RLE packet claiming more pixels than the image holds must not write out of bounds.
        assert!(decode(&tga(T_RLE_TRUECOLOR, 24, 1, 1, 0x20, &[0xFF, 0, 0, 0])).is_err());
    }
}
