//! Skin layout: turn the client's parsed window templates into textured quads.
//!
//! This is the geometry half of the UI render pipeline, kept deliberately free of GPU types so the
//! part most likely to be subtly wrong — rect arithmetic — is testable without a device, a window
//! or a screenshot. The GPU half only has to upload what comes out of here.
//!
//! The interesting piece is the NINE-SLICE. A window frame is authored once as a small atlas region
//! and drawn at any size by holding the four corners fixed, stretching the four edges along one
//! axis each, and stretching the centre both ways. Scaling the whole image instead would smear the
//! border art, which is exactly what the nine-slice exists to prevent.

use caer_assets::uiskin::{Control, ImageArea, NineSlice, Rgba, Skin, WindowTemplate};

/// An axis-aligned rectangle in pixels. Used for both screen space and atlas space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[must_use]
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    /// Whether this rect would draw anything. A zero- or negative-sized rect is not an error — it
    /// falls out naturally when a window is squeezed below its own border width — but it must be
    /// dropped rather than sent to the GPU, where a negative extent renders as garbage.
    #[must_use]
    pub fn is_visible(&self) -> bool {
        self.w > 0.0 && self.h > 0.0
    }
}

/// One textured quad: where to draw, what to sample, and the tint to modulate by.
#[derive(Clone, Debug, PartialEq)]
pub struct UiQuad {
    /// Destination in screen pixels, top-left origin.
    pub dst: Rect,
    /// Source in atlas pixels, top-left origin, within `texture`.
    pub src: Rect,
    /// Texture page name (`page3`), resolved against `Skin::texture`.
    pub texture: String,
    pub color: Rgba,
}

pub const WHITE: Rgba = Rgba {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};

/// Expand a nine-slice frame to cover `dst`, appending the visible patches to `out`.
///
/// Corners keep their authored size, edges stretch along one axis, the centre stretches both. When
/// `dst` is too small to fit its own borders the stretchable bands collapse to zero and are dropped
/// — the corners still draw, which degrades to a squashed-but-recognisable frame instead of
/// inside-out geometry.
pub fn nine_slice_quads(ns: &NineSlice, dst: Rect, color: Rgba, out: &mut Vec<UiQuad>) {
    // Source bands are the authored sizes; destination bands stretch only in the middle.
    let src_w = [
        ns.left_width as f32,
        ns.middle_width as f32,
        ns.right_width as f32,
    ];
    let src_h = [
        ns.top_height as f32,
        ns.middle_height as f32,
        ns.bottom_height as f32,
    ];

    // The middle absorbs whatever is left after the fixed borders. `max(0)` is what stops a window
    // narrower than its own frame from producing a negative-width centre.
    let mid_w = (dst.w - src_w[0] - src_w[2]).max(0.0);
    let mid_h = (dst.h - src_h[0] - src_h[2]).max(0.0);
    let dst_w = [src_w[0], mid_w, src_w[2]];
    let dst_h = [src_h[0], mid_h, src_h[2]];

    // Running offsets, so each cell starts where the previous one ended.
    let mut dy = dst.y;
    for row in 0..3 {
        let mut dx = dst.x;
        for col in 0..3 {
            let i = row * 3 + col;
            let (sx, sy) = ns.patches[i];
            let d = Rect::new(dx, dy, dst_w[col], dst_h[row]);
            let s = Rect::new(sx as f32, sy as f32, src_w[col], src_h[row]);
            // Drop both degenerate destinations (a collapsed band) and degenerate sources (a
            // template that declares a zero-width column) — neither can draw anything.
            if d.is_visible() && s.is_visible() {
                out.push(UiQuad {
                    dst: d,
                    src: s,
                    texture: ns.texture.clone(),
                    color,
                });
            }
            dx += dst_w[col];
        }
        dy += dst_h[row];
    }
}

/// A flat atlas rect drawn at `pos`, at its authored size.
pub fn image_area_quad(area: &ImageArea, pos: (f32, f32), color: Rgba) -> Option<UiQuad> {
    let src = Rect::new(
        area.top_left.0 as f32,
        area.top_left.1 as f32,
        area.size.0 as f32,
        area.size.1 as f32,
    );
    if !src.is_visible() {
        return None;
    }
    Some(UiQuad {
        dst: Rect::new(pos.0, pos.1, src.w, src.h),
        src,
        texture: area.texture.clone(),
        color,
    })
}

/// A piece of text the window wants drawn, positioned but not yet shaped into glyphs.
///
/// Kept separate from [`UiQuad`] because text needs the font atlas to become quads, and that
/// belongs to the font layer rather than to layout.
#[derive(Clone, Debug, PartialEq)]
pub struct UiText {
    pub rect: Rect,
    pub font: Option<String>,
    pub color: Rgba,
    /// What to draw. Empty when the label is adapter-driven and the adapter has no value yet.
    pub text: String,
    pub center_horizontally: bool,
    /// The live binding this label reads, if any — the hook the game state fills in.
    pub adapter: Option<String>,
}

/// Everything one window contributes to a frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowDraw {
    pub quads: Vec<UiQuad>,
    pub texts: Vec<UiText>,
    /// Controls whose art template could not be resolved in this skin. Surfaced rather than
    /// silently skipped: a window missing its background looks like a render bug, and this says it
    /// is a missing template instead.
    pub unresolved: Vec<String>,
}

/// Turn one laid-out text control into glyph quads.
///
/// `page` is the key the font atlas was uploaded under, so text batches alongside everything else
/// through the same quad pipeline rather than needing a second one.
///
/// Vertical placement centres the line within the control's box: the skins size label boxes by hand
/// and a glyph pinned to the top of one sits noticeably high against its frame.
pub fn text_quads(
    font: &caer_assets::bitmapfont::BitmapFont,
    page: &str,
    t: &UiText,
    out: &mut Vec<UiQuad>,
) {
    if t.text.is_empty() {
        return;
    }
    let width = font.measure(&t.text) as f32;
    // `CenterHorizontally` centres within the control's own width, which is why the skins give
    // labels an explicit Width even when the text is shorter.
    let x0 = if t.center_horizontally {
        t.rect.x + (t.rect.w - width) / 2.0
    } else {
        t.rect.x
    };
    let lh = font.line_height as f32;
    let y0 = if t.rect.h > lh {
        t.rect.y + (t.rect.h - lh) / 2.0
    } else {
        t.rect.y
    };

    for (g, dx) in font.layout(&t.text) {
        out.push(UiQuad {
            dst: Rect::new(x0 + dx as f32, y0, g.w as f32, g.h as f32),
            src: Rect::new(g.x as f32, g.y as f32, g.w as f32, g.h as f32),
            texture: page.to_string(),
            // The skin's label colour tints the glyph, which is how one grey atlas serves the
            // yellow clock, the white captions and the red warnings.
            color: t.color,
        });
    }
}

/// Greedy word-wrap `text` to `max_width` pixels in `font`.
///
/// The creation form's description panes are authored 220 px wide (`character_creation.xml`
/// TextAreaDef 1054/1055) and hold multi-sentence prose, so the single-line [`text_quads`] cannot
/// render them. Words longer than the column are emitted on their own line rather than being split
/// mid-word or silently dropped.
#[must_use]
pub fn wrap_text(
    font: &caer_assets::bitmapfont::BitmapFont,
    text: &str,
    max_width: f32,
) -> Vec<String> {
    let mut lines = Vec::new();
    if max_width <= 0.0 {
        return lines;
    }
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_string()
            } else {
                format!("{line} {word}")
            };
            if font.measure(&candidate) as f32 <= max_width || line.is_empty() {
                line = candidate;
            } else {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines
}

/// Render wrapped prose into `rect`, top-aligned, clipped to the rect's height.
///
/// Returns the number of lines actually drawn — fewer than the wrap produced means the text
/// overflowed its authored box, which is a layout defect worth asserting on rather than letting
/// the tail vanish unnoticed.
pub fn text_block_quads(
    font: &caer_assets::bitmapfont::BitmapFont,
    page: &str,
    text: &str,
    rect: Rect,
    color: Rgba,
    out: &mut Vec<UiQuad>,
) -> usize {
    let lh = font.line_height as f32;
    if lh <= 0.0 {
        return 0;
    }
    let max_lines = (rect.h / lh).floor().max(0.0) as usize;
    let lines = wrap_text(font, text, rect.w);
    let drawn = lines.len().min(max_lines);
    for (i, line) in lines.iter().take(drawn).enumerate() {
        let t = UiText {
            text: line.clone(),
            rect: Rect::new(rect.x, rect.y + i as f32 * lh, rect.w, lh),
            color,
            center_horizontally: false,
            font: None,
            adapter: None,
        };
        text_quads(font, page, &t, out);
    }
    drawn
}

/// Lay a window out at `origin` (its top-left on screen), resolving art through `skin`.
///
/// `values` supplies adapter text: given an adapter name it returns the current value. Labels with
/// an adapter use it; labels without one fall back to their authored `Data`, which for a
/// designer's placeholder is usually filler and for a static caption is the real text.
pub fn layout_window(
    skin: &Skin,
    win: &WindowTemplate,
    origin: (f32, f32),
    values: &dyn Fn(&str) -> Option<String>,
) -> WindowDraw {
    let mut out = WindowDraw::default();
    for control in &win.controls {
        match control {
            Control::Image(img) => {
                let Some(name) = img.template_name.as_deref() else {
                    continue;
                };
                let pos = (origin.0 + img.pos.0 as f32, origin.1 + img.pos.1 as f32);
                // An image control's own Width/Height are the size to fill; fall back to the
                // window's own extent, which is what a full-window background omits them for.
                let (w, h) = (
                    if img.width > 0 { img.width } else { win.width } as f32,
                    if img.height > 0 {
                        img.height
                    } else {
                        win.height
                    } as f32,
                );
                if let Some(ns) = skin.nine_slice(name) {
                    nine_slice_quads(ns, Rect::new(pos.0, pos.1, w, h), WHITE, &mut out.quads);
                } else if let Some(area) = skin.image_area(name) {
                    out.quads.extend(image_area_quad(area, pos, WHITE));
                } else {
                    out.unresolved.push(name.to_string());
                }
            }
            Control::Button(b) => {
                // The size is the template's, not a default. `ButtonDef` almost never authors
                // Width/Height, and the 80x24 that used to stand in for them is ledger J10's wrong
                // hit rect on every login-dialog button.
                let tmpl = b.template_name.as_deref().and_then(|n| skin.button(n));
                let (w, h) = match (b.size, tmpl) {
                    ((bw, bh), _) if bw > 0 && bh > 0 => (bw as f32, bh as f32),
                    (_, Some(t)) => (t.size.0 as f32, t.size.1 as f32),
                    // No template and no authored size: record it unresolved rather than guessing a
                    // rect. A button drawn at an invented size responds where its art is not.
                    (_, None) => {
                        if let Some(n) = b.template_name.as_deref() {
                            out.unresolved.push(n.to_string());
                        }
                        continue;
                    }
                };
                let pos = (origin.0 + b.pos.0 as f32, origin.1 + b.pos.1 as f32);
                if let Some(t) = tmpl {
                    let (sx, sy, sw, sh) = t.crop(caer_assets::uiskin::ButtonState::Normal);
                    let src = Rect::new(sx as f32, sy as f32, sw as f32, sh as f32);
                    if src.is_visible() {
                        out.quads.push(UiQuad {
                            dst: Rect::new(pos.0, pos.1, w, h),
                            src,
                            texture: t.texture.clone(),
                            color: WHITE,
                        });
                    }
                }
                if let Some(text) = b.label.as_deref().filter(|t| !t.is_empty()) {
                    out.texts.push(UiText {
                        rect: Rect::new(pos.0, pos.1, w, h),
                        font: tmpl.map(|t| t.font.clone()),
                        color: tmpl
                            .map_or(WHITE, |t| t.color(caer_assets::uiskin::ButtonState::Normal)),
                        text: text.to_string(),
                        center_horizontally: b.center_horizontally,
                        adapter: None,
                    });
                }
            }
            Control::EditBox(e) => {
                // The field's text, at the template's `TextOffset`. There is no frame to draw:
                // `generic_editbox` sets `BackgroundTemplate: none`, so retail's whole affordance is
                // the text sitting inside the authored rect (A15 / J10). An unbound field draws
                // nothing rather than its designer's placeholder.
                let Some(text) = e
                    .adapter
                    .as_deref()
                    .filter(|a| *a != "none")
                    .and_then(values)
                else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                let tmpl = e.template_name.as_deref().and_then(|n| skin.edit_box(n));
                let (dx, dy) = tmpl.map_or((0, 0), |t| t.text_offset);
                let rect = Rect::new(
                    origin.0 + (e.pos.0 + dx) as f32,
                    origin.1 + (e.pos.1 + dy) as f32,
                    (e.size.0 - dx).max(0) as f32,
                    (e.size.1 - dy).max(0) as f32,
                );
                out.texts.push(UiText {
                    rect,
                    font: tmpl.map(|t| t.font.clone()),
                    color: tmpl.map_or(WHITE, |t| t.color_normal),
                    text,
                    center_horizontally: false,
                    adapter: e.adapter.clone(),
                });
            }
            Control::Label(l) => {
                // Adapter first, authored Data second. A label bound to an adapter with no value
                // yet draws nothing rather than showing the designer's placeholder to the player.
                let text = match &l.adapter {
                    Some(a) if a != "none" => values(a).unwrap_or_default(),
                    _ => l.data.clone().unwrap_or_default(),
                };
                out.texts.push(UiText {
                    rect: Rect::new(
                        origin.0 + l.pos.0 as f32,
                        origin.1 + l.pos.1 as f32,
                        l.width as f32,
                        l.height as f32,
                    ),
                    font: l.font.clone(),
                    color: l.color,
                    text,
                    center_horizontally: l.center_horizontally,
                    adapter: l.adapter.clone(),
                });
            }
            Control::Other { tag, pos, .. } => {
                // ListBoxDef stores AdapterName in XML, but Control::Other does not yet carry it
                // (uiskin ownership). Atlantis merchant listbox is `merchant_page0` — bind it by
                // window name so a live 0x17 paints offer rows. Falsifier:
                // `merchant_listbox_binds_merchant_page0`.
                if !tag.eq_ignore_ascii_case("ListBoxDef") || win.name != "merchant" {
                    continue;
                }
                let Some(text) = values("merchant_page0") else {
                    continue;
                };
                const LINE_H: f32 = 16.0;
                for (i, line) in text.lines().enumerate() {
                    if line.is_empty() {
                        continue;
                    }
                    out.texts.push(UiText {
                        rect: Rect::new(
                            origin.0 + pos.0 as f32 + 4.0,
                            origin.1 + pos.1 as f32 + 4.0 + i as f32 * LINE_H,
                            320.0,
                            LINE_H,
                        ),
                        font: Some("arial11".into()),
                        color: caer_assets::uiskin::Rgba {
                            r: 255,
                            g: 255,
                            b: 255,
                            a: 255,
                        },
                        text: line.to_string(),
                        center_horizontally: false,
                        adapter: Some("merchant_page0".into()),
                    });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> NineSlice {
        // Mirrors `dlg_sm_title_noresize`: a 20px title band, 20px sides, 10px stretch cells.
        NineSlice {
            name: "f".into(),
            texture: "page3".into(),
            top_height: 20,
            middle_height: 10,
            bottom_height: 10,
            left_width: 20,
            middle_width: 10,
            right_width: 20,
            patches: [
                (1, 193),
                (22, 193),
                (33, 193),
                (1, 214),
                (22, 214),
                (33, 214),
                (1, 236),
                (22, 236),
                (33, 236),
            ],
        }
    }

    /// The nine patches must TILE the destination exactly: no gaps (a seam of background shows
    /// through) and no overlaps (translucent edges double-darken).
    #[test]
    fn nine_slice_tiles_the_destination_exactly() {
        let ns = frame();
        let dst = Rect::new(100.0, 50.0, 200.0, 120.0);
        let mut q = Vec::new();
        nine_slice_quads(&ns, dst, WHITE, &mut q);
        assert_eq!(q.len(), 9, "all nine patches should draw at this size");

        // The union of the destinations covers exactly the requested rect.
        let min_x = q.iter().map(|c| c.dst.x).fold(f32::MAX, f32::min);
        let min_y = q.iter().map(|c| c.dst.y).fold(f32::MAX, f32::min);
        let max_x = q.iter().map(|c| c.dst.x + c.dst.w).fold(f32::MIN, f32::max);
        let max_y = q.iter().map(|c| c.dst.y + c.dst.h).fold(f32::MIN, f32::max);
        assert_eq!((min_x, min_y), (dst.x, dst.y));
        assert_eq!((max_x, max_y), (dst.x + dst.w, dst.y + dst.h));

        // Rows and columns line up: each cell starts where its neighbour ended.
        let top_right = &q[2];
        assert_eq!(
            q[0].dst.x + q[0].dst.w,
            q[1].dst.x,
            "left edge meets the centre"
        );
        assert_eq!(
            q[1].dst.x + q[1].dst.w,
            top_right.dst.x,
            "centre meets the right edge"
        );
        assert_eq!(
            q[0].dst.y + q[0].dst.h,
            q[3].dst.y,
            "top row meets the middle row"
        );
    }

    /// Corners keep their AUTHORED size while only the middle bands stretch — that is the entire
    /// point. If corners scaled, the border art would smear.
    #[test]
    fn corners_stay_fixed_while_the_middle_stretches() {
        let ns = frame();
        let mut small = Vec::new();
        let mut large = Vec::new();
        nine_slice_quads(&ns, Rect::new(0.0, 0.0, 100.0, 100.0), WHITE, &mut small);
        nine_slice_quads(&ns, Rect::new(0.0, 0.0, 400.0, 300.0), WHITE, &mut large);

        // Corner destinations are identical at both sizes (indices 0, 2, 6, 8).
        for i in [0usize, 2, 6, 8] {
            assert_eq!(
                small[i].dst.w, large[i].dst.w,
                "corner {i} width must not scale"
            );
            assert_eq!(
                small[i].dst.h, large[i].dst.h,
                "corner {i} height must not scale"
            );
        }
        // The centre absorbs the difference.
        assert_eq!(small[4].dst.w, 100.0 - 40.0);
        assert_eq!(large[4].dst.w, 400.0 - 40.0);
        // Source rects never change with destination size.
        for i in 0..9 {
            assert_eq!(
                small[i].src, large[i].src,
                "patch {i} source must be size-independent"
            );
        }
    }

    /// Source rects must come from the template's own patch table and band sizes, so a transposed
    /// row or a swapped width would show up here rather than as subtly wrong art.
    #[test]
    fn source_rects_follow_the_patch_table() {
        let ns = frame();
        let mut q = Vec::new();
        nine_slice_quads(&ns, Rect::new(0.0, 0.0, 200.0, 200.0), WHITE, &mut q);
        assert_eq!(
            q[0].src,
            Rect::new(1.0, 193.0, 20.0, 20.0),
            "TopLeft: left_width x top_height"
        );
        assert_eq!(
            q[1].src,
            Rect::new(22.0, 193.0, 10.0, 20.0),
            "TopMiddle: middle_width x top_height"
        );
        assert_eq!(
            q[4].src,
            Rect::new(22.0, 214.0, 10.0, 10.0),
            "MiddleMiddle: both middles"
        );
        assert_eq!(
            q[8].src,
            Rect::new(33.0, 236.0, 20.0, 10.0),
            "BottomRight: right_width x bottom_height"
        );
        assert!(q.iter().all(|c| c.texture == "page3"));
    }

    /// A window squeezed below its own border width must not produce inside-out geometry. The
    /// stretch bands collapse to nothing and are dropped; the corners still draw.
    #[test]
    fn a_window_smaller_than_its_border_degrades_instead_of_inverting() {
        let ns = frame(); // needs 40px of fixed width, 30px of fixed height
        let mut q = Vec::new();
        nine_slice_quads(&ns, Rect::new(0.0, 0.0, 10.0, 10.0), WHITE, &mut q);
        assert!(!q.is_empty(), "the corners should still draw");
        for c in &q {
            assert!(
                c.dst.w > 0.0 && c.dst.h > 0.0,
                "no negative or zero quad may survive: {c:?}"
            );
        }
        // Exactly the four corners remain: every stretchable band collapsed.
        assert_eq!(q.len(), 4);
    }

    /// A template with a zero-sized band must not emit degenerate source rects.
    #[test]
    fn zero_sized_bands_are_dropped() {
        let mut ns = frame();
        ns.middle_width = 0;
        let mut q = Vec::new();
        nine_slice_quads(&ns, Rect::new(0.0, 0.0, 200.0, 200.0), WHITE, &mut q);
        // The three centre-column patches have a zero-width SOURCE and cannot draw.
        assert_eq!(q.len(), 6);
        assert!(q.iter().all(|c| c.src.is_visible()));
    }

    /// The client's own `pregame/` forms, absorbed the way the product absorbs them.
    fn pregame_skin(root: &std::path::Path) -> Skin {
        let mut skin = Skin::default();
        for f in ["styles.xml", "asset.xml", "login.xml"] {
            skin.absorb_file(root.join("pregame").join(f));
        }
        skin
    }

    /// **Ledger J10.** `login.xml`'s OK and QUIT draw their authored art and are sized by their
    /// template.
    ///
    /// Both halves in one gate because they were one cause: `ButtonDef` was flattened into a
    /// `Control::Label`, which drew a caption with no art **and** stood in a default `80x24` for the
    /// size the template declares as `64x21`. The button was therefore invisible and 16px too wide to
    /// click, from a single line.
    ///
    /// Seen red by restoring that flattening: `quads` loses both entries and the sizes go to 80x24.
    #[test]
    fn login_buttons_draw_their_authored_art_at_their_template_size() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "login_buttons_draw_their_authored_art_at_their_template_size",
        ) else {
            return;
        };
        let skin = pregame_skin(&root);
        let login = skin.windows.get("login").expect("pregame/login.xml");

        // `button_small` is the client's, and it is 64x21 — not the 80x24 that used to stand in.
        let tmpl = skin
            .button("button_small")
            .expect("styles.xml button_small");
        assert_eq!(tmpl.size, (64, 21), "button_small is authored 64x21");

        let buttons: Vec<&caer_assets::uiskin::Button> = login
            .controls
            .iter()
            .filter_map(|c| match c {
                Control::Button(b) => Some(b),
                _ => None,
            })
            .collect();
        assert_eq!(
            buttons.len(),
            2,
            "login.xml authors OK 1001 and QUIT 1002 as ButtonDefs, got {:?}",
            login.controls.len()
        );
        for b in &buttons {
            assert_eq!(
                b.template_name.as_deref(),
                Some("button_small"),
                "both login buttons are button_small"
            );
        }

        let draw = layout_window(&skin, login, (0.0, 0.0), &|_| None);
        for b in &buttons {
            let label = b.label.clone().unwrap_or_default();
            let (x, y) = (b.pos.0 as f32, b.pos.1 as f32);
            let art = draw.quads.iter().find(|q| {
                (q.dst.x - x).abs() < 0.5
                    && (q.dst.y - y).abs() < 0.5
                    && q.texture.eq_ignore_ascii_case(&tmpl.texture)
            });
            let art = art.unwrap_or_else(|| {
                panic!(
                    "{label} must draw {} art at ({x},{y}), not just its word",
                    tmpl.texture
                )
            });
            assert_eq!(
                (art.dst.w, art.dst.h),
                (64.0, 21.0),
                "{label} must be drawn at its template's size"
            );
            assert_eq!(
                (art.src.w, art.src.h),
                (64.0, 21.0),
                "{label} must crop its template's rect out of the page"
            );
            assert!(
                draw.texts.iter().any(|t| t.text == label),
                "{label}'s caption must still be drawn over the art"
            );
        }
    }

    /// **Ledger J10.** The two `generic_editbox` fields draw their bound text.
    ///
    /// They were parsed into `Control::Other` and never drawn, so the login screen had nowhere
    /// visible to type. There is deliberately no assertion about frame art: `generic_editbox` sets
    /// `BackgroundTemplate: none`, so retail draws none either (A15).
    ///
    /// Seen red by dropping the `Control::EditBox` arm from `layout_window`.
    #[test]
    fn login_fields_draw_their_bound_text_at_the_templates_offset() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "login_fields_draw_their_bound_text_at_the_templates_offset",
        ) else {
            return;
        };
        let skin = pregame_skin(&root);
        let login = skin.windows.get("login").expect("pregame/login.xml");

        let tmpl = skin
            .edit_box("generic_editbox")
            .expect("styles.xml generic_editbox");
        assert_eq!(tmpl.text_offset, (10, 5), "authored TextOffset");

        let fields: Vec<&caer_assets::uiskin::EditBox> = login
            .controls
            .iter()
            .filter_map(|c| match c {
                Control::EditBox(e) => Some(e),
                _ => None,
            })
            .collect();
        assert_eq!(fields.len(), 2, "login.xml authors two EditBoxDefs");
        let mut adapters: Vec<&str> = fields.iter().filter_map(|f| f.adapter.as_deref()).collect();
        adapters.sort_unstable();
        assert_eq!(
            adapters,
            ["password_text", "username_text"],
            "both fields must keep their AdapterName binding"
        );

        // Bound: the text lands at pos + TextOffset.
        let draw = layout_window(&skin, login, (0.0, 0.0), &|a| match a {
            "username_text" => Some("Tester".to_string()),
            "password_text" => Some("****".to_string()),
            _ => None,
        });
        for f in &fields {
            let want = f.adapter.as_deref().unwrap();
            let text = draw
                .texts
                .iter()
                .find(|t| t.adapter.as_deref() == Some(want))
                .unwrap_or_else(|| panic!("{want} must be drawn"));
            assert_eq!(
                (text.rect.x, text.rect.y),
                (
                    (f.pos.0 + tmpl.text_offset.0) as f32,
                    (f.pos.1 + tmpl.text_offset.1) as f32
                ),
                "{want} must sit at its template's TextOffset, not the rect corner"
            );
        }

        // Unbound: nothing, rather than a placeholder shown to the player.
        let empty = layout_window(&skin, login, (0.0, 0.0), &|_| None);
        assert!(
            !empty
                .texts
                .iter()
                .any(|t| t.adapter.as_deref() == Some("password_text")),
            "an unbound field must draw nothing"
        );
    }

    fn skin_with_frame() -> Skin {
        let mut s = Skin::default();
        s.nine_slices.insert(
            "bg".into(),
            NineSlice {
                name: "bg".into(),
                ..frame()
            },
        );
        s.image_areas.insert(
            "icon".into(),
            ImageArea {
                name: "icon".into(),
                texture: "page2".into(),
                size: (6, 8),
                top_left: (238, 29),
            },
        );
        s
    }

    fn window() -> WindowTemplate {
        WindowTemplate {
            name: "clock".into(),
            width: 120,
            height: 40,
            controls: vec![
                Control::Image(caer_assets::uiskin::Image {
                    control_id: Some("Background".into()),
                    template_name: Some("bg".into()),
                    width: 120,
                    height: 40,
                    pos: (0, 0),
                }),
                Control::Label(caer_assets::uiskin::Label {
                    font: Some("arial11".into()),
                    width: 120,
                    height: 16,
                    data: Some("Bizquit".into()),
                    pos: (0, 18),
                    adapter: Some("time_of_day".into()),
                    center_horizontally: true,
                    color: Rgba {
                        r: 255,
                        g: 255,
                        b: 0,
                        a: 255,
                    },
                    ..Default::default()
                }),
            ],
            ..Default::default()
        }
    }

    /// A window lays out at its screen origin: every control offset from it, not from (0,0).
    #[test]
    fn controls_are_placed_relative_to_the_window_origin() {
        let skin = skin_with_frame();
        let d = layout_window(&skin, &window(), (300.0, 200.0), &|_| None);
        assert_eq!(d.quads.len(), 9, "the background frame");
        assert_eq!(d.quads[0].dst.x, 300.0, "frame starts at the window origin");
        assert_eq!(d.quads[0].dst.y, 200.0);
        // The label sits 18px down from the window's top, not from the screen's.
        assert_eq!(d.texts.len(), 1);
        assert_eq!(d.texts[0].rect, Rect::new(300.0, 218.0, 120.0, 16.0));
        assert!(d.texts[0].center_horizontally);
        assert_eq!(
            d.texts[0].color,
            Rgba {
                r: 255,
                g: 255,
                b: 0,
                a: 255
            }
        );
    }

    /// An adapter-driven label shows the LIVE value, and shows nothing at all rather than the
    /// designer's placeholder when the adapter has no value yet.
    #[test]
    fn adapter_values_replace_the_authored_placeholder() {
        let skin = skin_with_frame();
        let d = layout_window(&skin, &window(), (0.0, 0.0), &|a| {
            (a == "time_of_day").then(|| "4:20 pm".to_string())
        });
        assert_eq!(d.texts[0].text, "4:20 pm");
        assert_eq!(d.texts[0].adapter.as_deref(), Some("time_of_day"));

        // No value yet: empty, NOT "Bizquit".
        let d = layout_window(&skin, &window(), (0.0, 0.0), &|_| None);
        assert_eq!(
            d.texts[0].text, "",
            "an unfilled adapter must not leak filler text to the player"
        );
    }

    /// A label with no adapter keeps its authored text — that is how static captions work.
    #[test]
    fn a_label_without_an_adapter_keeps_its_authored_text() {
        let mut w = window();
        let Control::Label(l) = &mut w.controls[1] else {
            panic!()
        };
        l.adapter = None;
        l.data = Some("Quest Journal".into());
        let d = layout_window(&skin_with_frame(), &w, (0.0, 0.0), &|_| None);
        assert_eq!(d.texts[0].text, "Quest Journal");

        // The literal adapter "none" that the skins use means the same as absent.
        let Control::Label(l) = &mut w.controls[1] else {
            panic!()
        };
        l.adapter = Some("none".into());
        let d = layout_window(&skin_with_frame(), &w, (0.0, 0.0), &|_| None);
        assert_eq!(
            d.texts[0].text, "Quest Journal",
            "the literal adapter `none` is not a binding"
        );
    }

    /// Falsifier `merchant_listbox_binds_merchant_page0`: ListBoxDef on the merchant window
    /// paints offer rows from `merchant_page0`; without that resolve the list stays blank.
    #[test]
    fn merchant_listbox_binds_merchant_page0() {
        let skin = Skin::default();
        let win = WindowTemplate {
            name: "merchant".into(),
            width: 350,
            height: 458,
            controls: vec![Control::Other {
                tag: "ListBoxDef".into(),
                control_id: Some("1005".into()),
                pos: (10, 21),
            }],
            ..Default::default()
        };
        let blank = layout_window(&skin, &win, (0.0, 0.0), &|_| None);
        assert!(
            blank.texts.is_empty(),
            "unbound merchant_page0 must leave the listbox blank"
        );
        let filled = layout_window(&skin, &win, (0.0, 0.0), &|a| {
            (a == "merchant_page0").then(|| "ticket\nwidget".to_string())
        });
        assert_eq!(filled.texts.len(), 2);
        assert_eq!(filled.texts[0].text, "ticket");
        assert_eq!(filled.texts[1].text, "widget");
        assert_eq!(filled.texts[0].adapter.as_deref(), Some("merchant_page0"));
    }

    /// A flat image area draws at its authored size, and an unresolvable template is REPORTED
    /// rather than silently skipped — a window missing its background otherwise looks like a
    /// renderer bug instead of a missing art template.
    #[test]
    fn flat_images_draw_and_missing_templates_are_reported() {
        let skin = skin_with_frame();
        let mut w = WindowTemplate {
            width: 50,
            height: 50,
            ..Default::default()
        };
        w.controls.push(Control::Image(caer_assets::uiskin::Image {
            template_name: Some("icon".into()),
            pos: (4, 6),
            ..Default::default()
        }));
        w.controls.push(Control::Image(caer_assets::uiskin::Image {
            template_name: Some("does_not_exist".into()),
            pos: (0, 0),
            ..Default::default()
        }));
        let d = layout_window(&skin, &w, (10.0, 20.0), &|_| None);
        assert_eq!(d.quads.len(), 1, "only the resolvable one drew");
        assert_eq!(
            d.quads[0].dst,
            Rect::new(14.0, 26.0, 6.0, 8.0),
            "authored size at the offset position"
        );
        assert_eq!(d.quads[0].src, Rect::new(238.0, 29.0, 6.0, 8.0));
        assert_eq!(d.unresolved, vec!["does_not_exist".to_string()]);
    }
}

/// One open window on screen: which template, where, and whether it is showing.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenWindow {
    pub name: String,
    pub pos: (f32, f32),
    pub visible: bool,
}

/// The open windows, in painter's order.
///
/// The vector IS the z-order — last drawn is topmost — so raising a window is a move to the end and
/// there is no separate z field to keep in sync with draw order. Hit testing walks it backwards for
/// the same reason: the window you can see is the one you should hit.
#[derive(Clone, Debug, Default)]
pub struct WindowManager {
    windows: Vec<OpenWindow>,
    /// Which window is being dragged, and the cursor offset within it, so a drag does not snap the
    /// window's corner to the pointer on the first frame.
    drag: Option<(String, f32, f32)>,
    /// Keyboard-focus owner (window name). Escape/cancel target this window first.
    focus: Option<String>,
}

impl WindowManager {
    /// Open a window (or raise and reveal it if already open).
    /// Existing windows keep their dragged position — `default_pos` is only for first open.
    pub fn open(&mut self, name: &str, pos: (f32, f32)) {
        match self.windows.iter_mut().find(|w| w.name == name) {
            Some(w) => w.visible = true,
            None => self.windows.push(OpenWindow {
                name: name.to_string(),
                pos,
                visible: true,
            }),
        }
        self.raise(name);
        self.focus = Some(name.to_string());
    }

    /// Move an open window without changing z-order (pre-world login buttons re-layout each frame).
    pub fn set_pos(&mut self, name: &str, pos: (f32, f32)) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.name == name) {
            w.pos = pos;
        }
    }

    /// Hide a window, keeping its position for the next time it is opened — which is what players
    /// expect from a UI they have arranged.
    pub fn close(&mut self, name: &str) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.name == name) {
            w.visible = false;
        }
        if self.focus.as_deref() == Some(name) {
            self.focus = self
                .windows
                .iter()
                .rev()
                .find(|w| w.visible)
                .map(|w| w.name.clone());
        }
    }

    /// Show a hidden window or hide a shown one.
    pub fn toggle(&mut self, name: &str, default_pos: (f32, f32)) {
        match self.windows.iter().find(|w| w.name == name) {
            Some(w) if w.visible => self.close(name),
            _ => self.open(name, default_pos),
        }
    }

    #[must_use]
    pub fn is_open(&self, name: &str) -> bool {
        self.windows.iter().any(|w| w.name == name && w.visible)
    }

    /// Move a window to the top of the draw order.
    pub fn raise(&mut self, name: &str) {
        if let Some(i) = self.windows.iter().position(|w| w.name == name) {
            let w = self.windows.remove(i);
            self.windows.push(w);
        }
    }

    /// The visible windows in draw order (bottom first).
    pub fn visible(&self) -> impl Iterator<Item = &OpenWindow> {
        self.windows.iter().filter(|w| w.visible)
    }

    /// The topmost visible window containing this point, if any.
    ///
    /// Walks back to front so the window the player can SEE at that pixel is the one that answers —
    /// front-to-back would return whatever happens to be underneath.
    #[must_use]
    pub fn hit(&self, skin: &Skin, x: f32, y: f32) -> Option<&str> {
        self.windows
            .iter()
            .rev()
            .filter(|w| w.visible)
            .find(|w| {
                skin.windows.get(&w.name).is_some_and(|t| {
                    x >= w.pos.0
                        && y >= w.pos.1
                        && x < w.pos.0 + t.width as f32
                        && y < w.pos.1 + t.height as f32
                })
            })
            .map(|w| w.name.as_str())
    }

    /// Whether a point is inside a window's title band — the region a drag starts from.
    fn in_title(t: &WindowTemplate, w: &OpenWindow, x: f32, y: f32) -> bool {
        // A window with no move button cannot be dragged at all, however tall its title is.
        if !t.move_button {
            return false;
        }
        // TitleWidth/TitleHeight are the band's own size; fall back to the window width so a
        // template that omits the width is still draggable across its whole top edge.
        let tw = if t.title_width > 0 {
            t.title_width
        } else {
            t.width
        } as f32;
        let th = t.title_height as f32;
        x >= w.pos.0 && x < w.pos.0 + tw && y >= w.pos.1 && y < w.pos.1 + th
    }

    /// Topmost visible control under the pointer: `(window_name, control_id)`.
    ///
    /// Empty `control_id` means the window frame was hit but no labelled control was.
    #[must_use]
    pub fn hit_control<'a>(&'a self, skin: &'a Skin, x: f32, y: f32) -> Option<(&'a str, &'a str)> {
        let win_name = self.hit(skin, x, y)?;
        let w = self
            .windows
            .iter()
            .rev()
            .find(|w| w.visible && w.name == win_name)?;
        let t = skin.windows.get(&w.name)?;
        for c in t.controls.iter().rev() {
            let (id, pos, width, height) = match c {
                Control::Label(l) => {
                    let id = l
                        .control_id
                        .as_deref()
                        .or(l.click_event.as_deref())
                        .or(l.data.as_deref())
                        .unwrap_or("");
                    (id, l.pos, l.width.max(1), l.height.max(1))
                }
                Control::Image(i) => (
                    i.control_id.as_deref().unwrap_or(""),
                    i.pos,
                    if i.width > 0 { i.width } else { t.width },
                    if i.height > 0 { i.height } else { t.height },
                ),
                Control::Button(b) => {
                    let id = b
                        .control_id
                        .as_deref()
                        .or(b.click_event.as_deref())
                        .or(b.label.as_deref())
                        .unwrap_or("");
                    // Template size, so the responding rect is the drawn rect. J10: this arm used
                    // to be the `Control::Other` 80x24 below, against an authored 64x21.
                    let tmpl = b.template_name.as_deref().and_then(|n| skin.button(n));
                    let (bw, bh) = match (b.size, tmpl) {
                        ((w, h), _) if w > 0 && h > 0 => (w, h),
                        (_, Some(t)) => t.size,
                        (_, None) => continue,
                    };
                    (id, b.pos, bw.max(1), bh.max(1))
                }
                Control::EditBox(e) => (
                    e.control_id
                        .as_deref()
                        .or(e.adapter.as_deref())
                        .unwrap_or(""),
                    e.pos,
                    e.size.0.max(1),
                    e.size.1.max(1),
                ),
                Control::Other {
                    control_id, pos, ..
                } => (control_id.as_deref().unwrap_or(""), *pos, 80, 24),
            };
            if id.is_empty() {
                continue;
            }
            let (cx, cy) = (w.pos.0 + pos.0 as f32, w.pos.1 + pos.1 as f32);
            if x >= cx && y >= cy && x < cx + width as f32 && y < cy + height as f32 {
                return Some((win_name, id));
            }
        }
        Some((win_name, ""))
    }

    /// Title-band close gadget (top-right `title_height` square) when the template has a close button.
    #[must_use]
    pub fn hit_close(&self, skin: &Skin, x: f32, y: f32) -> Option<&str> {
        let name = self.hit(skin, x, y)?;
        let w = self
            .windows
            .iter()
            .rev()
            .find(|w| w.visible && w.name == name)?;
        let t = skin.windows.get(&w.name)?;
        if !t.close_button {
            return None;
        }
        let size = t.title_height.max(12) as f32;
        let right = w.pos.0 + t.width as f32;
        if x >= right - size && x < right && y >= w.pos.1 && y < w.pos.1 + size {
            Some(name)
        } else {
            None
        }
    }

    #[must_use]
    pub fn keyboard_focus(&self) -> Option<&str> {
        self.focus.as_deref()
    }

    /// Escape/cancel: hide the focused closeable window. Returns the closed name.
    pub fn on_escape(&mut self) -> Option<String> {
        let name = self.focus.clone()?;
        let closeable = self
            .windows
            .iter()
            .find(|w| w.name == name && w.visible)
            .is_some();
        if closeable {
            self.close(&name);
            Some(name)
        } else {
            None
        }
    }

    /// Press: raise whatever was hit, and begin a drag if it landed on the title band.
    /// Returns `true` when the UI consumed the click, so the world does not also act on it.
    pub fn on_press(&mut self, skin: &Skin, x: f32, y: f32) -> bool {
        let Some(name) = self.hit(skin, x, y).map(str::to_string) else {
            return false;
        };
        self.raise(&name);
        self.focus = Some(name.clone());
        // Look the window up AFTER raising: `raise` moves it, so an index taken before is stale.
        if let (Some(t), Some(w)) = (
            skin.windows.get(&name),
            self.windows.iter().find(|w| w.name == name),
        ) {
            if Self::in_title(t, w, x, y) {
                self.drag = Some((name, x - w.pos.0, y - w.pos.1));
            }
        }
        true
    }

    /// Motion: move the dragged window, keeping the grab offset so it does not jump.
    pub fn on_motion(&mut self, x: f32, y: f32) {
        let Some((name, ox, oy)) = self.drag.clone() else {
            return;
        };
        if let Some(w) = self.windows.iter_mut().find(|w| w.name == name) {
            w.pos = (x - ox, y - oy);
        }
    }

    /// Release: end any drag.
    pub fn on_release(&mut self) {
        self.drag = None;
    }

    #[must_use]
    pub fn dragging(&self) -> Option<&str> {
        self.drag.as_ref().map(|(n, _, _)| n.as_str())
    }

    /// Lay out every visible window, bottom to top, into one draw list.
    pub fn draw(&self, skin: &Skin, values: &dyn Fn(&str) -> Option<String>) -> WindowDraw {
        let mut out = WindowDraw::default();
        for w in self.visible() {
            let Some(t) = skin.windows.get(&w.name) else {
                continue;
            };
            let d = layout_window(skin, t, w.pos, values);
            out.quads.extend(d.quads);
            out.texts.extend(d.texts);
            out.unresolved.extend(d.unresolved);
        }
        out
    }
}

#[cfg(test)]
mod window_tests {
    use super::*;

    fn skin() -> Skin {
        let mut s = Skin::default();
        for (name, movable) in [
            ("a", true),
            ("b", true),
            ("fixed", false),
            ("paperdoll", true),
        ] {
            s.windows.insert(
                name.into(),
                WindowTemplate {
                    name: name.into(),
                    width: 100,
                    height: 80,
                    title_width: 100,
                    title_height: 18,
                    move_button: movable,
                    close_button: name == "a",
                    ..Default::default()
                },
            );
        }
        s.windows
            .get_mut("a")
            .unwrap()
            .controls
            .push(Control::Label(caer_assets::uiskin::Label {
                control_id: Some("accept".into()),
                data: Some("Accept".into()),
                pos: (10, 40),
                width: 80,
                height: 24,
                ..Default::default()
            }));
        s
    }

    /// Paperdoll / inventory windows must participate in skin hit-testing so equipment UI can
    /// receive clicks (MS-02 step 4). Uses the same WindowManager path as every other skin window.
    #[test]
    fn paperdoll_window_hit_testing_consumes_clicks() {
        let (s, mut m) = (skin(), WindowManager::default());
        m.open("paperdoll", (40.0, 40.0));
        assert_eq!(m.hit(&s, 50.0, 50.0), Some("paperdoll"));
        assert!(
            m.on_press(&s, 50.0, 50.0),
            "paperdoll click must be consumed by the UI"
        );
        assert!(!m.on_press(&s, 900.0, 900.0), "misses still fall through");
    }

    /// The vector IS the z-order, so opening or clicking a window raises it and it draws last.
    #[test]
    fn clicking_raises_a_window_to_the_top() {
        let (s, mut m) = (skin(), WindowManager::default());
        m.open("a", (0.0, 0.0));
        m.open("b", (0.0, 0.0)); // overlapping, b on top
        assert_eq!(m.visible().last().unwrap().name, "b");
        // Both cover this point; the topmost must answer.
        assert_eq!(m.hit(&s, 50.0, 50.0), Some("b"));

        // Clicking raises `a`, which then answers the same hit.
        m.on_press(&s, 50.0, 50.0);
        m.raise("a");
        assert_eq!(m.hit(&s, 50.0, 50.0), Some("a"));
        assert_eq!(m.visible().last().unwrap().name, "a");
    }

    /// A click outside every window must NOT be consumed, or the UI would eat clicks meant for the
    /// world even where there is no window.
    #[test]
    fn clicks_outside_every_window_are_not_consumed() {
        let (s, mut m) = (skin(), WindowManager::default());
        m.open("a", (10.0, 10.0));
        assert!(m.on_press(&s, 20.0, 20.0), "inside is consumed");
        assert!(
            !m.on_press(&s, 500.0, 500.0),
            "outside must fall through to the world"
        );
        assert_eq!(m.hit(&s, 500.0, 500.0), None);
    }

    /// Dragging keeps the grab offset: the window must not snap its corner to the cursor.
    #[test]
    fn dragging_preserves_the_grab_offset() {
        let (s, mut m) = (skin(), WindowManager::default());
        m.open("a", (100.0, 100.0));
        // Grab inside the title band, 30px right and 8px down from the corner.
        m.on_press(&s, 130.0, 108.0);
        assert_eq!(m.dragging(), Some("a"));
        m.on_motion(200.0, 150.0);
        let w = m.visible().next().unwrap();
        assert_eq!(
            w.pos,
            (170.0, 142.0),
            "offset preserved, not snapped to the cursor"
        );
        m.on_release();
        assert_eq!(m.dragging(), None);
        // After release, motion no longer moves it.
        m.on_motion(400.0, 400.0);
        assert_eq!(m.visible().next().unwrap().pos, (170.0, 142.0));
    }

    /// Only the title band starts a drag; the body raises without moving.
    #[test]
    fn only_the_title_band_starts_a_drag() {
        let (s, mut m) = (skin(), WindowManager::default());
        m.open("a", (0.0, 0.0));
        m.on_press(&s, 50.0, 40.0); // below the 18px title
        assert_eq!(m.dragging(), None, "the body must not drag the window");
        m.on_motion(300.0, 300.0);
        assert_eq!(m.visible().next().unwrap().pos, (0.0, 0.0));
    }

    /// A window whose template has no move button cannot be dragged from anywhere.
    #[test]
    fn a_fixed_window_cannot_be_dragged() {
        let (s, mut m) = (skin(), WindowManager::default());
        m.open("fixed", (0.0, 0.0));
        m.on_press(&s, 50.0, 5.0); // squarely in the title band
        assert_eq!(m.dragging(), None);
    }

    /// Closing hides but REMEMBERS the position — a player who arranged their UI expects it back
    /// where they left it.
    #[test]
    fn closing_hides_but_keeps_the_position() {
        let (s, mut m) = (skin(), WindowManager::default());
        m.open("a", (10.0, 20.0));
        m.on_press(&s, 40.0, 25.0);
        m.on_motion(140.0, 125.0);
        m.on_release();
        let moved = m.visible().next().unwrap().pos;

        m.close("a");
        assert!(!m.is_open("a"));
        assert_eq!(m.visible().count(), 0);
        assert_eq!(
            m.hit(&s, 40.0, 25.0),
            None,
            "a hidden window must not take hits"
        );

        // Re-opening restores it where it was, not at the default.
        m.open("a", (999.0, 999.0));
        assert_eq!(m.visible().next().unwrap().pos, moved);
    }

    /// Toggle opens a closed window and closes an open one.
    #[test]
    fn toggle_flips_visibility() {
        let (_, mut m) = (skin(), WindowManager::default());
        m.toggle("a", (0.0, 0.0));
        assert!(m.is_open("a"));
        m.toggle("a", (0.0, 0.0));
        assert!(!m.is_open("a"));
        m.toggle("a", (0.0, 0.0));
        assert!(m.is_open("a"));
    }

    /// The combined draw list is bottom-to-top, so a raised window's art lands over its neighbour's.
    #[test]
    fn the_draw_list_follows_the_z_order() {
        let mut s = skin();
        s.nine_slices.insert(
            "bg".into(),
            NineSlice {
                name: "bg".into(),
                texture: "p".into(),
                top_height: 4,
                middle_height: 4,
                bottom_height: 4,
                left_width: 4,
                middle_width: 4,
                right_width: 4,
                patches: [(0, 0); 9],
            },
        );
        for n in ["a", "b"] {
            s.windows.get_mut(n).unwrap().controls.push(Control::Image(
                caer_assets::uiskin::Image {
                    template_name: Some("bg".into()),
                    width: 100,
                    height: 80,
                    ..Default::default()
                },
            ));
        }
        let mut m = WindowManager::default();
        m.open("a", (0.0, 0.0));
        m.open("b", (200.0, 0.0));
        let d = m.draw(&s, &|_| None);
        assert_eq!(d.quads.len(), 18, "nine patches per window");
        // `b` was opened last so its quads come last — and its x proves which is which.
        assert_eq!(d.quads[0].dst.x, 0.0);
        assert_eq!(d.quads[9].dst.x, 200.0);
        // Raising `a` puts it last instead.
        m.raise("a");
        let d = m.draw(&s, &|_| None);
        assert_eq!(d.quads[0].dst.x, 200.0, "b now draws first (underneath)");
        assert_eq!(d.quads[9].dst.x, 0.0, "a now draws last (on top)");
    }

    /// Falsifier `skin_hit_control_required_for_product_clicks`.
    #[test]
    fn hit_control_returns_labelled_button_not_the_frame() {
        let (s, mut m) = (skin(), WindowManager::default());
        m.open("a", (0.0, 0.0));
        assert_eq!(m.hit_control(&s, 20.0, 50.0), Some(("a", "accept")));
        assert_eq!(
            m.hit_control(&s, 50.0, 10.0),
            Some(("a", "")),
            "title/frame hit is not a labelled control"
        );
        assert_eq!(m.hit_control(&s, 900.0, 900.0), None);
    }

    #[test]
    fn close_gadget_and_escape_hide_the_focused_window() {
        let (s, mut m) = (skin(), WindowManager::default());
        m.open("a", (0.0, 0.0));
        assert_eq!(m.keyboard_focus(), Some("a"));
        assert_eq!(m.hit_close(&s, 95.0, 5.0), Some("a"));
        assert!(m.on_escape().as_deref() == Some("a"));
        assert!(!m.is_open("a"));
        assert_eq!(m.hit(&s, 50.0, 50.0), None, "closed window must not ghost");
    }
}
