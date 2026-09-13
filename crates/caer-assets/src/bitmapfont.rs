//! Bitmap fonts: the glyph atlases the skin UI draws text with.
//!
//! The client ships these as bare `.tga` with **no metadata file of any kind** — no widths table,
//! no grid, nothing. The layout is encoded in the image itself, which took a bit of reverse
//! engineering:
//!
//! ```text
//!   ...glyph pixels...          <- a band of glyph rows
//!   #  ## ###  ####  ...        <- a MARKER row: one run per glyph, spanning its cell
//!   ...glyph pixels...          <- the next band
//!   #  ### ##  ...
//! ```
//!
//! Each band of glyphs is followed by a marker row whose runs delimit the glyphs' **cells** — not
//! their ink. Cells carry the left-side bearing, so `!` in Arial11 has ink at x=5..7 but a cell of
//! x=4..7, and using the ink extent instead would jam every letter against its neighbour.
//!
//! Two details that make the marker rows findable without knowing the font size:
//!
//!  * every marker row begins with an ISOLATED single pixel at x=0, which is not a glyph. Testing
//!    `x=0 set && x=1 clear` finds marker rows regardless of the band pitch, which varies per font
//!    (13 for Arial09, 33 for `label`). Testing x=0 alone is not enough — in `overhead_24` glyph
//!    ink reaches the left edge.
//!  * the glyph run is CONTIGUOUS from `'!'` (0x21). Space has no cell at all.
//!
//! Measured across the client's eleven bitmap fonts: ten yield exactly 223 cells, i.e. `0x21..=0xFF`
//! — the printable CP1252 range with space omitted. Arial14 yields one extra, so the charset is
//! clamped rather than shifted: one stray run must not slide every glyph by one character.

use std::collections::HashMap;

use crate::tga::TgaImage;

/// The first character with a cell in the atlas. Space (0x20) has none.
pub const FIRST_GLYPH: u32 = 0x21;
/// The last character the atlases cover — CP1252's `ÿ`.
pub const LAST_GLYPH: u32 = 0xFF;

/// One glyph's cell within the atlas, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// A parsed bitmap font: the atlas plus where every glyph lives in it.
pub struct BitmapFont {
    pub atlas: TgaImage,
    /// Distance between the tops of consecutive glyph bands — the natural line height.
    pub line_height: u32,
    /// Character → cell. Sparse: a font may not cover the whole range.
    pub glyphs: HashMap<char, Glyph>,
    /// Advance for a space, which has no cell of its own to measure.
    pub space_advance: u32,
    /// Marker runs found beyond `LAST_GLYPH`, i.e. cells we could not assign a character. Reported
    /// rather than silently dropped — a non-zero count means the marker heuristic over-split
    /// somewhere in this font (Arial14 has one).
    pub unassigned: usize,
}

impl BitmapFont {
    /// Parse a decoded font atlas.
    pub fn parse(atlas: TgaImage) -> Option<Self> {
        let (w, h) = (atlas.width, atlas.height);
        if w < 2 || h < 2 {
            return None;
        }
        let alpha = |x: u32, y: u32| atlas.pixel(x, y).map_or(0, |p| p[3]);

        // Marker rows: an isolated set pixel in column 0.
        let markers: Vec<u32> = (0..h)
            .filter(|&y| alpha(0, y) > 0 && alpha(1, y) == 0)
            .collect();
        if markers.is_empty() {
            return None;
        }

        // Band pitch, and therefore line height, from the marker spacing. A single-band font falls
        // back to the marker's own row, which is the whole band.
        let line_height = if markers.len() > 1 {
            markers[1] - markers[0]
        } else {
            markers[0] + 1
        };

        let mut glyphs = HashMap::new();
        let mut code = FIRST_GLYPH;
        let mut unassigned = 0usize;
        for &m in &markers {
            // The glyph band sits directly ABOVE its marker row.
            let band_top = (m + 1).saturating_sub(line_height);
            let band_h = m - band_top;
            for (i, (x0, x1)) in runs(&atlas, m).into_iter().enumerate() {
                // The first run of every marker row is the row marker itself, not a glyph.
                if i == 0 {
                    continue;
                }
                if code > LAST_GLYPH {
                    unassigned += 1;
                    continue;
                }
                if let Some(c) = char::from_u32(code) {
                    glyphs.insert(
                        c,
                        Glyph {
                            x: x0,
                            y: band_top,
                            w: x1 - x0 + 1,
                            h: band_h,
                        },
                    );
                }
                code += 1;
            }
        }
        if glyphs.is_empty() {
            return None;
        }

        // Space has no cell, so its advance has to come from somewhere. A quarter of the line
        // height is the usual typographic space and matches these fonts by eye; it is OURS, not
        // the client's, because the client's value is not in the file.
        let space_advance = (line_height / 4).max(2);
        Some(Self {
            atlas,
            line_height,
            glyphs,
            space_advance,
            unassigned,
        })
    }

    /// Advance width of one character, including unknown characters (which draw nothing but must
    /// still take space, or text silently shortens).
    #[must_use]
    pub fn advance(&self, c: char) -> u32 {
        if c == ' ' {
            return self.space_advance;
        }
        self.glyphs.get(&c).map_or(self.space_advance, |g| g.w)
    }

    /// Total width of a string in pixels.
    #[must_use]
    pub fn measure(&self, text: &str) -> u32 {
        text.chars().map(|c| self.advance(c)).sum()
    }

    /// Lay a string out, yielding `(glyph, x_offset)` pairs. The caller adds its own origin.
    ///
    /// Characters with no cell (space, and anything outside the atlas) advance without producing a
    /// glyph, so spacing is preserved even where art is missing.
    pub fn layout(&self, text: &str) -> Vec<(Glyph, u32)> {
        let mut out = Vec::new();
        let mut x = 0u32;
        for c in text.chars() {
            if let Some(g) = self.glyphs.get(&c) {
                out.push((*g, x));
            }
            x += self.advance(c);
        }
        out
    }
}

/// Horizontal runs of set pixels on one row.
fn runs(img: &TgaImage, y: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut start: Option<u32> = None;
    for x in 0..img.width {
        let set = img.pixel(x, y).is_some_and(|p| p[3] > 0);
        match (set, start) {
            (true, None) => start = Some(x),
            (false, Some(s)) => {
                out.push((s, x - 1));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, img.width - 1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The client's prose is CP1252 and its atlases are byte-indexed, so the punctuation above
    /// `0x7e` has to be drawable — not merely storable.
    ///
    /// Two race descriptions in `game.dll` carry byte `0x92` (a right single quote), which
    /// `descriptions::read_cstr` now preserves as `U+0092` because that is the code point this
    /// parser assigns the atlas cell at index 0x92. If that cell were missing, the fix would trade
    /// a beheaded sentence for `Midgard s armies` with a hole in it, so assert the glyph is really
    /// there and really has width.
    ///
    /// **Fails** when the retail tree is absent (REQ-025). Set `CAER_CLIENT`.
    #[test]
    fn label_font_carries_the_cp1252_quote_cells() {
        let Some(root) = crate::client_dep::require_caer_client("label_font_cp1252_cells") else {
            return;
        };
        let path = root.join("ui/fonts/button_12.tga");
        let bytes = std::fs::read(&path).expect("ui/fonts/button_12.tga");
        let font = BitmapFont::parse(crate::tga::decode(&bytes).expect("decode font atlas"))
            .expect("parse font atlas");

        for (c, what) in [
            ('\u{27}', "ASCII apostrophe"),
            ('\u{91}', "left single quote"),
            ('\u{92}', "right single quote"),
            ('\u{93}', "left double quote"),
            ('\u{94}', "right double quote"),
        ] {
            let g = font
                .glyphs
                .get(&c)
                .unwrap_or_else(|| panic!("no atlas cell for {what} ({:#04x})", c as u32));
            assert!(
                g.w > 0 && g.h > 0,
                "{what} ({:#04x}) has an empty cell {g:?}",
                c as u32
            );
        }

        // And it must survive layout, not just exist in the map.
        let with = font.measure("Midgard\u{92}s");
        let without = font.measure("Midgards");
        assert!(
            with > without,
            "the quote contributed no width: {with} vs {without}"
        );
        assert_eq!(
            font.layout("Midgard\u{92}s").len(),
            9,
            "every character including the quote must produce a glyph"
        );
    }

    /// Build a synthetic atlas: `bands` rows of glyph cells, each band followed by a marker row.
    /// `cells` gives the (start, end) x of each glyph cell per band.
    fn atlas(w: u32, pitch: u32, bands: &[Vec<(u32, u32)>]) -> TgaImage {
        let h = pitch * bands.len() as u32;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let mut set = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        };
        for (b, cells) in bands.iter().enumerate() {
            let marker_row = b as u32 * pitch + pitch - 1;
            // The row marker: one isolated pixel at x=0.
            set(0, marker_row);
            for &(x0, x1) in cells {
                for x in x0..=x1 {
                    set(x, marker_row);
                    // A dot of ink in the band above, so the glyph is not blank.
                    set(x, marker_row - 1);
                }
            }
        }
        TgaImage {
            width: w,
            height: h,
            rgba,
        }
    }

    /// Glyphs map contiguously from '!' — space has no cell, so an off-by-one here would shift the
    /// entire alphabet.
    #[test]
    fn glyphs_map_contiguously_from_exclamation() {
        let f = BitmapFont::parse(atlas(64, 8, &[vec![(4, 7), (10, 15), (18, 20)]])).unwrap();
        assert_eq!(f.glyphs.len(), 3);
        assert_eq!(
            f.glyphs[&'!'],
            Glyph {
                x: 4,
                y: 0,
                w: 4,
                h: 7
            }
        );
        assert_eq!(
            f.glyphs[&'"'],
            Glyph {
                x: 10,
                y: 0,
                w: 6,
                h: 7
            }
        );
        assert_eq!(
            f.glyphs[&'#'],
            Glyph {
                x: 18,
                y: 0,
                w: 3,
                h: 7
            }
        );
        assert!(
            !f.glyphs.contains_key(&' '),
            "space has no cell in the atlas"
        );
    }

    /// A second band continues the character sequence rather than restarting it.
    #[test]
    fn bands_continue_the_sequence_and_set_the_line_height() {
        let f = BitmapFont::parse(atlas(
            64,
            8,
            &[vec![(4, 7), (10, 15)], vec![(4, 6), (9, 12)]],
        ))
        .unwrap();
        assert_eq!(f.line_height, 8, "pitch between markers is the line height");
        assert_eq!(f.glyphs[&'!'].y, 0, "first band");
        assert_eq!(f.glyphs[&'#'].y, 8, "second band starts one pitch down");
        assert_eq!(
            f.glyphs[&'#'],
            Glyph {
                x: 4,
                y: 8,
                w: 3,
                h: 7
            }
        );
        assert_eq!(f.glyphs.len(), 4);
    }

    /// The row marker is the FIRST run of each marker row and must not be read as a glyph — doing
    /// so would shift every character by one and add a 1px glyph per band.
    #[test]
    fn the_row_marker_is_not_a_glyph() {
        let f = BitmapFont::parse(atlas(64, 8, &[vec![(4, 7)], vec![(4, 7)]])).unwrap();
        assert_eq!(f.glyphs.len(), 2, "two bands, one glyph each — not four");
        assert!(
            f.glyphs.values().all(|g| g.w > 1),
            "no 1px marker leaked in as a glyph"
        );
        assert_eq!(
            f.glyphs[&'!'].x, 4,
            "glyphs start after the marker, not at x=0"
        );
    }

    /// Cells carry the LEFT-SIDE BEARING, so measuring uses the cell width, not the ink width.
    /// Using ink would jam every letter against its neighbour.
    #[test]
    fn advances_use_the_cell_width() {
        let f = BitmapFont::parse(atlas(64, 8, &[vec![(4, 7), (10, 15)]])).unwrap();
        assert_eq!(f.advance('!'), 4);
        assert_eq!(f.advance('"'), 6);
        assert_eq!(f.measure("!\""), 10);
    }

    /// Space and unknown characters advance without drawing — otherwise text silently shortens
    /// wherever the atlas lacks a glyph.
    #[test]
    fn space_and_unknown_characters_still_advance() {
        let f = BitmapFont::parse(atlas(64, 16, &[vec![(4, 7)]])).unwrap();
        assert_eq!(f.space_advance, 4, "a quarter of the 16px line height");
        assert_eq!(f.advance(' '), 4);
        assert!(
            f.advance('Z') > 0,
            "an absent glyph must not have zero advance"
        );

        let laid = f.layout("! !");
        assert_eq!(laid.len(), 2, "the space produces no glyph");
        assert_eq!(laid[0].1, 0);
        assert_eq!(
            laid[1].1,
            4 + 4,
            "second '!' sits past the first glyph and the space"
        );
    }

    /// Runs beyond the covered range are counted, not assigned — one stray marker run (Arial14 has
    /// exactly one) must not slide every later glyph by a character.
    #[test]
    fn extra_runs_are_reported_rather_than_shifting_the_charset() {
        // One band holding more cells than the charset can absorb.
        let cells: Vec<(u32, u32)> = (0..230).map(|i| (2 + i * 3, 3 + i * 3)).collect();
        let f = BitmapFont::parse(atlas(1024, 8, &[cells])).unwrap();
        assert_eq!(f.glyphs.len(), (LAST_GLYPH - FIRST_GLYPH + 1) as usize);
        assert_eq!(f.unassigned, 230 - (LAST_GLYPH - FIRST_GLYPH + 1) as usize);
        // The last assignable character is still ÿ, not something past it.
        assert!(f.glyphs.contains_key(&'ÿ'));
    }

    /// An image with no marker rows is not a font, and must be rejected rather than yielding an
    /// empty one that silently draws nothing.
    #[test]
    fn a_non_font_image_is_rejected() {
        let blank = TgaImage {
            width: 8,
            height: 8,
            rgba: vec![0; 8 * 8 * 4],
        };
        assert!(BitmapFont::parse(blank).is_none());
        // Ink at x=0 AND x=1 is glyph art, not a marker.
        let mut solid = TgaImage {
            width: 8,
            height: 8,
            rgba: vec![255; 8 * 8 * 4],
        };
        solid
            .rgba
            .iter_mut()
            .skip(3)
            .step_by(4)
            .for_each(|a| *a = 255);
        assert!(
            BitmapFont::parse(solid).is_none(),
            "a solid image has no isolated markers"
        );
    }
}
