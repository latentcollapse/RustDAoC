//! Where a part's triangles actually land on its texture sheet.
//!
//! The instrument this replaces is UV min/max bounds, which cannot answer the question: a 0..1
//! span says nothing about *where* the polygons sit. A part can report perfect bounds and still
//! have half its triangles sampling the flat filler colour an artist left between the islands —
//! which renders as a hard-edged untextured patch, and is what "unrendered triangles" looks like
//! when the geometry is fine and the texture is fully opaque.
//!
//! Two stages, both pure so the control is a test rather than a ritual:
//!
//! 1. [`detect_background`] finds the filler colour, if there is one, by looking for a large
//!    perfectly-flat region. An atlas packed edge to edge has no background and reports `None`.
//! 2. [`coverage`] rasterises each triangle in texel space and reports how much of it lands there.
//!
//! **This measures texture space, not screen space.** A triangle covering a large slice of the
//! sheet may be a few pixels on screen, and the reverse. It answers "does this part sample
//! filler", not "how much of what the player sees is wrong".

/// A sheet's texels split into art and filler.
#[derive(Clone, Debug)]
pub struct AtlasMask {
    pub width: u32,
    pub height: u32,
    /// The filler colour, when the sheet has one.
    pub background: Option<[u8; 3]>,
    /// False where the texel matches the filler colour.
    pub is_art: Vec<bool>,
    /// Share of the sheet that is filler.
    pub background_fraction: f32,
}

impl AtlasMask {
    #[must_use]
    pub fn art_at(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return true;
        }
        self.is_art[(y * self.width + x) as usize]
    }
}

/// How far a texel may drift from the filler colour and still count as filler.
///
/// Not a tuning knob to widen when a result is inconvenient. Block compression moves a flat colour
/// by a couple of levels at most, and every colour in these atlases that is *meant* to be art sits
/// far outside this — the boots sheet's filler is (128,128,128) against leather near (106,83,65),
/// a Chebyshev distance of 45. Six is compression noise; anything a designer painted is not.
const FILLER_TOLERANCE: i16 = 6;

/// Share of a sheet the border-connected region must cover before it counts as filler.
///
/// Connectivity does the real separating; this only rejects a sheet whose art happens to share a
/// colour with a few edge texels. Under-reporting is the safe direction — a false "no background"
/// makes this instrument say nothing rather than say something wrong.
const MIN_FILLER_FRACTION: f32 = 0.05;

fn chebyshev(a: [u8; 3], b: [u8; 3]) -> i16 {
    (0..3)
        .map(|i| (i16::from(a[i]) - i16::from(b[i])).abs())
        .max()
        .unwrap_or(0)
}

/// The flat filler colour an atlas is packed against, if it has one.
///
/// Two properties, and **both** are needed. Filler is flat — art has gradient and noise almost
/// everywhere — and it is *packing waste*, which means it is reachable from the sheet border
/// without crossing an island.
///
/// Flatness alone is not enough, and assuming it was is a mistake this instrument made on its
/// first run: a face sheet carries a large flat skin-toned region in the middle of the face, which
/// the modal-flat-colour test happily called filler and then counted a third of every female
/// head's triangles as landing on nothing. The colours gave it away — `[236,174,126]` is a
/// complexion, `[126,128,126]` is an empty page. Border-connectivity is the structural version of
/// that observation, and it does not need anyone to eyeball a colour.
///
/// A sheet whose islands reach every edge yields `None`. Callers must treat that as "this
/// instrument cannot speak here", not as "no triangles are on filler" — under-reporting is the
/// safe direction, because a wrong silence costs a measurement and a wrong verdict costs a fix.
#[must_use]
pub fn detect_background(width: u32, height: u32, rgba: &[u8]) -> Option<[u8; 3]> {
    border_filler(width, height, rgba).map(|(colour, _, _)| colour)
}

/// Candidate filler colour, its border-connected mask, and how much of the sheet it covers.
fn border_filler(width: u32, height: u32, rgba: &[u8]) -> Option<([u8; 3], Vec<bool>, f32)> {
    if width < 3 || height < 3 || rgba.len() < (width * height * 4) as usize {
        return None;
    }
    let at = |x: u32, y: u32| -> [u8; 3] {
        let i = ((y * width + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    // Candidates are the flat colours actually present on the border — filler is what the packer
    // left over, so it is what the edge is made of.
    let mut counts: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
    let mut border: Vec<(u32, u32)> = Vec::new();
    for x in 0..width {
        border.push((x, 0));
        border.push((x, height - 1));
    }
    for y in 1..height - 1 {
        border.push((0, y));
        border.push((width - 1, y));
    }
    for &(x, y) in &border {
        *counts.entry(at(x, y)).or_default() += 1;
    }
    let (colour, _) = counts.into_iter().max_by_key(|&(_, n)| n)?;

    // Flood fill inward from every border texel of that colour. Anything the fill cannot reach is
    // enclosed by art and is art, whatever its colour.
    let idx = |x: u32, y: u32| (y * width + x) as usize;
    let mut mask = vec![false; (width * height) as usize];
    let mut stack: Vec<(u32, u32)> = border
        .into_iter()
        .filter(|&(x, y)| chebyshev(at(x, y), colour) <= FILLER_TOLERANCE)
        .collect();
    for &(x, y) in &stack {
        mask[idx(x, y)] = true;
    }
    while let Some((x, y)) = stack.pop() {
        let push = |nx: u32, ny: u32, stack: &mut Vec<(u32, u32)>, mask: &mut Vec<bool>| {
            if nx < width
                && ny < height
                && !mask[idx(nx, ny)]
                && chebyshev(at(nx, ny), colour) <= FILLER_TOLERANCE
            {
                mask[idx(nx, ny)] = true;
                stack.push((nx, ny));
            }
        };
        push(x.wrapping_sub(1), y, &mut stack, &mut mask);
        push(x + 1, y, &mut stack, &mut mask);
        push(x, y.wrapping_sub(1), &mut stack, &mut mask);
        push(x, y + 1, &mut stack, &mut mask);
    }
    let filled = mask.iter().filter(|m| **m).count() as f32;
    let fraction = filled / (width * height) as f32;
    (fraction >= MIN_FILLER_FRACTION).then_some((colour, mask, fraction))
}

/// Classify every texel on a sheet as art or filler.
#[must_use]
pub fn atlas_mask(width: u32, height: u32, rgba: &[u8]) -> AtlasMask {
    match border_filler(width, height, rgba) {
        Some((colour, filler, fraction)) => AtlasMask {
            width,
            height,
            background: Some(colour),
            is_art: filler.into_iter().map(|f| !f).collect(),
            background_fraction: fraction,
        },
        None => AtlasMask {
            width,
            height,
            background: None,
            is_art: vec![true; (width * height) as usize],
            background_fraction: 0.0,
        },
    }
}

/// What a part's triangles sample.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Coverage {
    pub triangles: usize,
    /// Triangles whose sampled texels are more than half filler.
    pub triangles_on_filler: usize,
    /// Texels covered by this part's triangles, counted once per triangle.
    pub texels: u64,
    pub texels_on_filler: u64,
    /// Triangles with a UV outside 0..1, which wrap. Reported because a wrapped triangle can land
    /// somewhere entirely unintended and this is the only place it shows up.
    pub triangles_wrapped: usize,
}

impl Coverage {
    #[must_use]
    pub fn filler_area_fraction(&self) -> f32 {
        if self.texels == 0 {
            return 0.0;
        }
        self.texels_on_filler as f32 / self.texels as f32
    }

    #[must_use]
    pub fn filler_triangle_fraction(&self) -> f32 {
        if self.triangles == 0 {
            return 0.0;
        }
        self.triangles_on_filler as f32 / self.triangles as f32
    }
}

/// Rasterise one triangle in texel space, returning `(texels, texels_on_filler)`.
///
/// Degenerate triangles — a seam weld, or a collapsed UV — cover no texels and would otherwise
/// divide by zero; they are counted as covering the single texel they sit on, so a part made
/// entirely of them still reports something rather than silently reporting nothing.
/// Visit every texel a UV triangle covers on a `width`x`height` sheet.
///
/// Extracted from [`raster_triangle`] so the filler mask and the texel sampler rasterise the *same*
/// way. Two rasterisers that disagree by a texel would make a coverage number and a colour number
/// describe slightly different triangles, which is the sort of difference that survives review.
///
/// A sub-texel triangle is sampled at its centroid rather than discarded — a part made of many tiny
/// triangles would otherwise report nothing measured.
fn raster_triangle_into(tri: [[f32; 2]; 3], width: u32, height: u32, mut f: impl FnMut(u32, u32)) {
    let (w, h) = (width as f32, height as f32);
    let px: Vec<[f32; 2]> = tri
        .iter()
        .map(|uv| [uv[0].rem_euclid(1.0) * w, uv[1].rem_euclid(1.0) * h])
        .collect();
    let clampx = |v: u32| v.min(width.saturating_sub(1));
    let clampy = |v: u32| v.min(height.saturating_sub(1));
    let minx = px
        .iter()
        .map(|p| p[0])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as u32;
    let maxx = clampx(px.iter().map(|p| p[0]).fold(f32::MIN, f32::max).ceil() as u32);
    let miny = px
        .iter()
        .map(|p| p[1])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as u32;
    let maxy = clampy(px.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil() as u32);

    let edge = |a: [f32; 2], b: [f32; 2], p: [f32; 2]| {
        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
    };
    let area = edge(px[0], px[1], px[2]);
    let centroid = |f: &mut dyn FnMut(u32, u32)| {
        let c = [
            (px[0][0] + px[1][0] + px[2][0]) / 3.0,
            (px[0][1] + px[1][1] + px[2][1]) / 3.0,
        ];
        f(clampx(c[0].max(0.0) as u32), clampy(c[1].max(0.0) as u32));
    };
    if area.abs() < 1e-6 {
        centroid(&mut f);
        return;
    }
    let mut hit = 0u64;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let (w0, w1, w2) = (
                edge(px[1], px[2], p),
                edge(px[2], px[0], p),
                edge(px[0], px[1], p),
            );
            let inside = if area > 0.0 {
                w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
            } else {
                w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0
            };
            if inside {
                hit += 1;
                f(x, y);
            }
        }
    }
    if hit == 0 {
        centroid(&mut f);
    }
}

/// What a part's triangles actually sample out of its sheet.
///
/// **Whole-sheet statistics are the wrong summary for a parity question**, and A5 is the case that
/// proved it: `cthLegs01_ss_Bri_f.dds` is 37% dark low-alpha texels and 63% light mid-alpha ones
/// *over the whole page*, but a leg only samples its own island, and predictions built on the page
/// average did not reproduce Eden's render. This measures the texels the geometry reaches.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SampledTexels {
    pub texels: u64,
    pub alpha_mean: f32,
    pub alpha_max: u8,
    pub rgb_mean: [f32; 3],
    /// The same mean taken after sRGB decode. Texel bytes are gamma-encoded, so averaging them
    /// weights bright texels far too heavily and a ratio of two such means is not a ratio of two
    /// brightnesses. Kept alongside `rgb_mean` rather than replacing it because the encoded mean is
    /// still the right thing to compare against a screenshot's own encoded pixels.
    pub rgb_mean_linear: [f32; 3],
}

impl SampledTexels {
    /// Rec.601 luma of the mean colour — the number to compare across parts, because it is what a
    /// brightness ratio in a screenshot is measuring.
    #[must_use]
    pub fn luma(&self) -> f32 {
        0.299 * self.rgb_mean[0] + 0.587 * self.rgb_mean[1] + 0.114 * self.rgb_mean[2]
    }

    /// Rec.709 luma of the decoded mean — the one that may be multiplied, divided or ratioed.
    ///
    /// Use this for "how much brighter is A than B". [`luma`] answers the different question of
    /// what a screenshot sampler would read, and the two disagree by roughly a factor of two on
    /// the Briton legs pair, which is large enough to have sent an A5 diagnosis the wrong way.
    #[must_use]
    pub fn luma_linear(&self) -> f32 {
        0.2126 * self.rgb_mean_linear[0]
            + 0.7152 * self.rgb_mean_linear[1]
            + 0.0722 * self.rgb_mean_linear[2]
    }

    /// Is this sheet usable as a base texture, or is it an overlay?
    ///
    /// A base is opaque somewhere. A sheet whose alpha never reaches opaque **under the geometry
    /// that uses it** expects something drawn beneath it, and force-opaquing it paints that
    /// something out. Judged over the sampled texels rather than the page for the reason above.
    #[must_use]
    pub fn reads_as_overlay(&self) -> bool {
        self.texels > 0 && self.alpha_max < 250
    }
}

/// Accumulate what `triangles` sample out of an RGBA sheet.
#[must_use]
pub fn sample_texels(
    triangles: &[[[f32; 2]; 3]],
    width: u32,
    height: u32,
    rgba: &[u8],
    // Same dimensions as `rgba` when present; supplies RGB while `rgba` supplies alpha.
    rgba_uploaded: Option<&[u8]>,
) -> SampledTexels {
    let mut out = SampledTexels::default();
    if width == 0 || height == 0 {
        return out;
    }
    let (mut a_sum, mut r, mut g, mut b) = (0u64, 0u64, 0u64, 0u64);
    let (mut lr, mut lg, mut lb) = (0f64, 0f64, 0f64);
    for tri in triangles {
        raster_triangle_into(*tri, width, height, |x, y| {
            let i = (y as usize * width as usize + x as usize) * 4;
            let Some(px) = rgba.get(i..i + 4) else { return };
            let col = rgba_uploaded.and_then(|u| u.get(i..i + 4)).unwrap_or(px);
            out.texels += 1;
            r += u64::from(col[0]);
            g += u64::from(col[1]);
            b += u64::from(col[2]);
            a_sum += u64::from(px[3]);
            lr += f64::from(crate::gpu::srgb_byte_to_linear(col[0]));
            lg += f64::from(crate::gpu::srgb_byte_to_linear(col[1]));
            lb += f64::from(crate::gpu::srgb_byte_to_linear(col[2]));
            out.alpha_max = out.alpha_max.max(px[3]);
        });
    }
    if out.texels > 0 {
        let n = out.texels as f32;
        out.alpha_mean = a_sum as f32 / n;
        out.rgb_mean = [r as f32 / n, g as f32 / n, b as f32 / n];
        let nd = f64::from(n);
        out.rgb_mean_linear = [(lr / nd) as f32, (lg / nd) as f32, (lb / nd) as f32];
    }
    out
}

fn raster_triangle(tri: [[f32; 2]; 3], mask: &AtlasMask) -> (u64, u64) {
    let (w, h) = (mask.width as f32, mask.height as f32);
    let px: Vec<[f32; 2]> = tri
        .iter()
        .map(|uv| [uv[0].rem_euclid(1.0) * w, uv[1].rem_euclid(1.0) * h])
        .collect();
    let minx = px
        .iter()
        .map(|p| p[0])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as u32;
    let maxx = (px.iter().map(|p| p[0]).fold(f32::MIN, f32::max).ceil() as u32).min(mask.width - 1);
    let miny = px
        .iter()
        .map(|p| p[1])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as u32;
    let maxy =
        (px.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil() as u32).min(mask.height - 1);

    let edge = |a: [f32; 2], b: [f32; 2], p: [f32; 2]| {
        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
    };
    let area = edge(px[0], px[1], px[2]);
    let (mut total, mut filler) = (0u64, 0u64);
    if area.abs() < 1e-6 {
        let (x, y) = (px[0][0] as u32, px[0][1] as u32);
        let art = mask.art_at(x.min(mask.width - 1), y.min(mask.height - 1));
        return (1, u64::from(!art));
    }
    for y in miny..=maxy {
        for x in minx..=maxx {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let (w0, w1, w2) = (
                edge(px[1], px[2], p),
                edge(px[2], px[0], p),
                edge(px[0], px[1], p),
            );
            let inside = if area > 0.0 {
                w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
            } else {
                w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0
            };
            if inside {
                total += 1;
                if !mask.art_at(x, y) {
                    filler += 1;
                }
            }
        }
    }
    if total == 0 {
        // Sub-texel triangle: sample its centroid rather than discard it.
        let c = [
            (px[0][0] + px[1][0] + px[2][0]) / 3.0,
            (px[0][1] + px[1][1] + px[2][1]) / 3.0,
        ];
        let art = mask.art_at(
            (c[0] as u32).min(mask.width - 1),
            (c[1] as u32).min(mask.height - 1),
        );
        return (1, u64::from(!art));
    }
    (total, filler)
}

/// Project a part's triangles onto its sheet.
#[must_use]
pub fn coverage(triangles: &[[[f32; 2]; 3]], mask: &AtlasMask) -> Coverage {
    let mut c = Coverage::default();
    if mask.background.is_none() {
        // No filler to land on. Report the triangle count so a caller can tell "measured, nothing
        // to find" from "not measured".
        c.triangles = triangles.len();
        return c;
    }
    for tri in triangles {
        c.triangles += 1;
        if tri
            .iter()
            .any(|uv| uv[0] < 0.0 || uv[0] > 1.0 || uv[1] < 0.0 || uv[1] > 1.0)
        {
            c.triangles_wrapped += 1;
        }
        let (total, filler) = raster_triangle(*tri, mask);
        c.texels += total;
        c.texels_on_filler += filler;
        if filler * 2 > total {
            c.triangles_on_filler += 1;
        }
    }
    c
}

/// A connected run of probe-coloured pixels in a rendered frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillerBlob {
    pub pixels: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl FillerBlob {
    /// Thickness of the blob's thinner axis, in pixels.
    ///
    /// The number that separates the two things the probe finds. A texel of bleed at an island's
    /// edge draws a hairline around a silhouette — one or two pixels thick, however long it runs.
    /// A region whose UVs genuinely sit off the island draws a solid patch. Both are triangles
    /// sampling filler; only the second is a defect a player would report.
    #[must_use]
    pub fn thickness(&self) -> u32 {
        self.w.min(self.h)
    }

    /// Fraction of the blob's bounding box that is actually filled — a solid wedge approaches 1,
    /// a hairline tracing a curve stays low.
    #[must_use]
    pub fn density(&self) -> f32 {
        self.pixels as f32 / (self.w * self.h).max(1) as f32
    }

    /// A solid patch rather than a traced edge.
    ///
    /// Neither half is sufficient alone: a straight hairline fills its 1-pixel-wide box
    /// completely, and a diagonal one has a square box. A blob has to be thick in both axes *and*
    /// fill its box to be the kind of region a player points at.
    #[must_use]
    pub fn is_wedge(&self) -> bool {
        self.thickness() >= 3 && self.density() >= 0.5
    }
}

/// How many distinct part tints [`part_tint_colour`] can hand out before repeating.
pub const PART_TINT_COUNT: usize = 12;

/// A visually separated flat colour per part index, for `CAER_PART_TINT`.
///
/// Walks the hue circle at a stride coprime with the count, so neighbouring parts never land on
/// neighbouring hues, and pins saturation and value so scene lighting scales every part
/// identically — which is what lets [`part_tint_index`] recover the index from a lit frame.
#[must_use]
pub fn part_tint_colour(index: usize) -> [u8; 3] {
    let hue = ((index * 150) % 360) as f32;
    // Fully saturated on purpose: every tint then has a zero channel, and natural materials
    // almost never do. That single property is what keeps grass and skin out of the palette.
    let (h, s, v) = (hue / 60.0, 1.0_f32, 1.0_f32);
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    ]
}

/// Recover a part index from a lit pixel of a `CAER_PART_TINT` capture.
///
/// Lighting multiplies all three channels together, so the tint survives as a ratio. Normalising
/// on the brightest channel removes the lighting and leaves the hue, which is matched against the
/// palette. Returns `None` when nothing is close enough to be that part rather than scenery.
#[must_use]
pub fn part_tint_index(r: u8, g: u8, b: u8) -> Option<usize> {
    let peak = r.max(g).max(b);
    if peak < 24 {
        return None; // too dark to carry a ratio
    }
    // **Saturation first, hue second.** Every tint is fully saturated, so its darkest channel is
    // near zero; natural materials almost never are. Skipping this and matching on hue alone gave
    // a 2.5% false-positive rate on an ordinary capture — leather and skin sitting close enough to
    // the orange entry to be claimed as a part. Nearest-hue always returns *something*; this is
    // what makes "no part here" expressible.
    let floor = r.min(g).min(b);
    if u32::from(floor) * 100 > u32::from(peak) * 25 {
        return None;
    }
    let norm = |v: u8| f32::from(v) * 255.0 / f32::from(peak);
    let (nr, ng, nb) = (norm(r), norm(g), norm(b));
    let mut best: Option<(usize, f32)> = None;
    for i in 0..PART_TINT_COUNT {
        let c = part_tint_colour(i);
        let d = (nr - f32::from(c[0])).powi(2)
            + (ng - f32::from(c[1])).powi(2)
            + (nb - f32::from(c[2])).powi(2);
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    // The cutoff is half the palette's own minimum separation — derived, not chosen. A pixel
    // closer than that to one entry cannot be closer to any other, so a match is unambiguous.
    let limit = min_palette_separation() * 0.5;
    best.filter(|(_, d)| d.sqrt() < limit).map(|(i, _)| i)
}

/// Smallest distance between any two palette entries, in RGB.
fn min_palette_separation() -> f32 {
    let mut min = f32::MAX;
    for i in 0..PART_TINT_COUNT {
        for j in i + 1..PART_TINT_COUNT {
            let (a, b) = (part_tint_colour(i), part_tint_colour(j));
            let d = (0..3)
                .map(|k| (f32::from(a[k]) - f32::from(b[k])).powi(2))
                .sum::<f32>()
                .sqrt();
            min = min.min(d);
        }
    }
    min
}

/// Is this pixel the probe colour, after the scene's lighting has multiplied it?
///
/// Lighting scales all three channels together, so magenta stays "red and blue present, green far
/// below both" at any brightness. That is a property, not a tuned threshold: no DAoC material is
/// simultaneously strong in red and blue and absent in green.
#[must_use]
pub fn is_probe_pixel(r: u8, g: u8, b: u8) -> bool {
    let (r, g, b) = (i16::from(r), i16::from(g), i16::from(b));
    r > 60 && b > 60 && r - g > 40 && b - g > 40
}

/// Group a rendered frame's probe pixels into connected blobs, largest first.
///
/// `rgb` is tightly packed, 3 bytes per pixel.
#[must_use]
pub fn filler_blobs(width: u32, height: u32, rgb: &[u8]) -> Vec<FillerBlob> {
    let n = (width * height) as usize;
    if rgb.len() < n * 3 {
        return Vec::new();
    }
    let hit: Vec<bool> = (0..n)
        .map(|i| is_probe_pixel(rgb[i * 3], rgb[i * 3 + 1], rgb[i * 3 + 2]))
        .collect();
    let mut seen = vec![false; n];
    let mut out = Vec::new();
    for start in 0..n {
        if !hit[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let (mut x0, mut y0) = (u32::MAX, u32::MAX);
        let (mut x1, mut y1) = (0u32, 0u32);
        let mut pixels = 0u32;
        while let Some(i) = stack.pop() {
            let (x, y) = (i as u32 % width, i as u32 / width);
            pixels += 1;
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            let visit = |nx: u32, ny: u32, stack: &mut Vec<usize>, seen: &mut Vec<bool>| {
                if nx < width && ny < height {
                    let j = (ny * width + nx) as usize;
                    if hit[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            };
            // Eight-connected on purpose. A silhouette outline runs diagonally, and four-connected
            // labelling shatters it into one blob per pixel — which then reads as hundreds of tiny
            // findings instead of one hairline.
            for (dx, dy) in [
                (-1i32, 0i32),
                (1, 0),
                (0, -1),
                (0, 1),
                (-1, -1),
                (1, -1),
                (-1, 1),
                (1, 1),
            ] {
                visit(
                    x.wrapping_add_signed(dx),
                    y.wrapping_add_signed(dy),
                    &mut stack,
                    &mut seen,
                );
            }
        }
        out.push(FillerBlob {
            pixels,
            x: x0,
            y: y0,
            w: x1 - x0 + 1,
            h: y1 - y0 + 1,
        });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.pixels));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sheet with a flat filler border and one art island in the middle.
    fn synthetic(width: u32, height: u32, filler: [u8; 3]) -> Vec<u8> {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let island = x >= width / 2 && y >= height / 2;
                let c = if island {
                    // Deliberately noisy, the way real art is.
                    [
                        (20 + (x * 7 % 200)) as u8,
                        (40 + (y * 11 % 180)) as u8,
                        (60 + ((x + y) * 13 % 150)) as u8,
                    ]
                } else {
                    filler
                };
                rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        rgba
    }

    #[test]
    fn finds_the_filler_colour_and_ignores_a_packed_sheet() {
        let grey = [128, 128, 128];
        let rgba = synthetic(64, 64, grey);
        assert_eq!(detect_background(64, 64, &rgba), Some(grey));

        // A sheet with no flat region at all has no filler, and must say so rather than pick the
        // most common noise value.
        let mut packed = Vec::new();
        for y in 0..64u32 {
            for x in 0..64u32 {
                packed.extend_from_slice(&[
                    (x * 3 % 251) as u8,
                    (y * 5 % 241) as u8,
                    ((x ^ y) * 7 % 239) as u8,
                    255,
                ]);
            }
        }
        assert_eq!(detect_background(64, 64, &packed), None);
    }

    /// The control. A triangle placed on the island reads clean; the same triangle moved onto the
    /// filler reads as fully on filler. One measurement, both states.
    #[test]
    fn a_triangle_on_filler_is_distinguishable_from_one_on_art() {
        let mask = atlas_mask(64, 64, &synthetic(64, 64, [128, 128, 128]));
        assert_eq!(mask.background, Some([128, 128, 128]));
        assert!(
            (mask.background_fraction - 0.75).abs() < 0.01,
            "3 of 4 quadrants are filler"
        );

        // Island occupies u,v in 0.5..1.0.
        let on_art = [[[0.6, 0.6], [0.9, 0.6], [0.6, 0.9]]];
        let good = coverage(&on_art, &mask);
        assert_eq!(
            good.triangles_on_filler, 0,
            "a triangle on the island is not on filler"
        );
        assert!(good.filler_area_fraction() < 0.01, "{good:?}");

        // Same triangle translated into the empty top-left quadrant.
        let on_filler = [[[0.1, 0.1], [0.4, 0.1], [0.1, 0.4]]];
        let bad = coverage(&on_filler, &mask);
        assert_eq!(
            bad.triangles_on_filler, 1,
            "a triangle on filler must be caught"
        );
        assert!(bad.filler_area_fraction() > 0.99, "{bad:?}");

        // And the two must not be reported the same way, which is the whole point.
        assert!(bad.filler_area_fraction() - good.filler_area_fraction() > 0.9);
    }

    /// The false positive this instrument shipped with, as a test.
    ///
    /// A face sheet is one island filling the page with a big flat skin-toned area in the middle.
    /// Judging filler by flatness alone called that complexion "empty" and reported a third of
    /// every female head's triangles as landing on nothing. Border connectivity is what fixes it,
    /// and this is the state that proves the fix.
    #[test]
    fn a_flat_region_enclosed_by_art_is_not_filler() {
        let (w, h) = (64u32, 64u32);
        let skin = [236, 174, 126];
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                // Noisy art around the whole border, flat skin tone enclosed in the middle.
                let enclosed = (8..56).contains(&x) && (8..56).contains(&y);
                let c = if enclosed {
                    skin
                } else {
                    [
                        (30 + (x * 9 % 190)) as u8,
                        (50 + (y * 13 % 170)) as u8,
                        (70 + ((x * y) % 140)) as u8,
                    ]
                };
                rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        let mask = atlas_mask(w, h, &rgba);
        assert_eq!(
            mask.background, None,
            "a flat region the border cannot reach is art, not packing waste"
        );
        // And a triangle sitting squarely on it must not be reported as landing on filler.
        let c = coverage(&[[[0.3, 0.3], [0.7, 0.3], [0.3, 0.7]]], &mask);
        assert_eq!(c.triangles_on_filler, 0);
    }

    /// Filler that reaches the border is still found when art also touches the border.
    #[test]
    fn filler_is_found_even_when_islands_touch_the_edge() {
        let (w, h) = (64u32, 64u32);
        let grey = [128, 128, 128];
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                // One island bolted to the bottom-right corner; the rest is packing waste.
                let island = x >= 40 && y >= 40;
                let c = if island {
                    [(20 + (x * 7 % 200)) as u8, (40 + (y * 11 % 180)) as u8, 90]
                } else {
                    grey
                };
                rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        let mask = atlas_mask(w, h, &rgba);
        assert_eq!(mask.background, Some(grey));
        assert!(
            mask.background_fraction > 0.5,
            "{}",
            mask.background_fraction
        );
        assert_eq!(
            coverage(&[[[0.1, 0.1], [0.3, 0.1], [0.1, 0.3]]], &mask).triangles_on_filler,
            1
        );
        assert_eq!(
            coverage(&[[[0.7, 0.7], [0.95, 0.7], [0.7, 0.95]]], &mask).triangles_on_filler,
            0
        );
    }

    #[test]
    fn straddling_triangles_are_reported_by_area_not_rounded_to_a_verdict() {
        let mask = atlas_mask(64, 64, &synthetic(64, 64, [128, 128, 128]));
        // A triangle spanning the quadrant corner: part filler, part art.
        let tri = [[[0.4, 0.4], [0.9, 0.4], [0.9, 0.9]]];
        let c = coverage(&tri, &mask);
        let f = c.filler_area_fraction();
        assert!(
            f > 0.0 && f < 1.0,
            "a straddling triangle must report a partial fraction, got {f}"
        );
    }

    #[test]
    fn wrapped_uvs_are_counted_and_still_sampled() {
        let mask = atlas_mask(64, 64, &synthetic(64, 64, [128, 128, 128]));
        let tri = [[[1.6, 1.6], [1.9, 1.6], [1.6, 1.9]]];
        let c = coverage(&tri, &mask);
        assert_eq!(c.triangles_wrapped, 1);
        // 1.6 wraps to 0.6, which is the island — so it must read clean, not be discarded.
        assert_eq!(c.triangles_on_filler, 0);
        assert!(c.texels > 0);
    }

    /// A sheet with no filler yields no verdict, and the caller can tell that apart from "clean".
    #[test]
    fn a_sheet_without_filler_reports_measured_but_silent() {
        let mut packed = Vec::new();
        for y in 0..64u32 {
            for x in 0..64u32 {
                packed.extend_from_slice(&[
                    (x * 3 % 251) as u8,
                    (y * 5 % 241) as u8,
                    ((x ^ y) * 7 % 239) as u8,
                    255,
                ]);
            }
        }
        let mask = atlas_mask(64, 64, &packed);
        assert!(mask.background.is_none());
        let c = coverage(&[[[0.1, 0.1], [0.4, 0.1], [0.1, 0.4]]], &mask);
        assert_eq!(c.triangles, 1, "the triangle was seen");
        assert_eq!(c.texels, 0, "but nothing was measured about it");
    }
}

/// A part's name, its bound texture key, and its triangles in UV space.
type PartJob = (String, String, Vec<[[f32; 2]; 3]>);

/// One part's result on the real avatar path.
#[derive(Clone, Debug)]
pub struct PartCoverage {
    pub part: String,
    /// The key the binder bound, `#opaque` suffix included.
    pub texture: String,
    /// None when the key did not resolve to a sheet — a part the binder recorded and the GPU
    /// never received. That is a louder finding than any coverage number.
    pub sheet: Option<(u32, u32)>,
    pub background: Option<[u8; 3]>,
    pub background_fraction: f32,
    pub coverage: Coverage,
    /// What this part's triangles actually sample — alpha and colour, over the geometry rather than
    /// over the page. This is the reading that decides whether a bound sheet is a base or an
    /// overlay (A5).
    pub sampled: SampledTexels,
}

impl PartCoverage {
    /// Did this part get measured at all? A `false` here is not a clean result.
    #[must_use]
    pub fn measured(&self) -> bool {
        self.sheet.is_some() && self.background.is_some() && self.coverage.texels > 0
    }
}

/// Project every part of one avatar onto its bound sheet.
///
/// Goes through `ensure_avatar` so the bindings are the ones the client produces, not a
/// reconstruction — the whole point is to read what the renderer actually gave the GPU.
pub fn avatar_coverage(
    models: &mut crate::entities::EntityModels,
    gpu: &mut crate::gpu::Gpu,
    race: u8,
    gender: u8,
) -> Vec<PartCoverage> {
    let Some(model) = models.ensure_avatar(
        gpu,
        race,
        gender,
        caer_protocol::customization::Customization::default(),
        None,
    ) else {
        return Vec::new();
    };
    let bound: std::collections::HashMap<String, String> = models
        .last_avatar_stand()
        .bound_textures
        .iter()
        .map(|(p, t)| (p.to_ascii_lowercase(), t.clone()))
        .collect();
    let Some(rig) = models.skinned_rig(model) else {
        return Vec::new();
    };
    let batch = crate::terrain::skinned_batch_multi(&rig.rigs);

    // Collect the triangles first: `avatar_sheet` needs `models` mutably and the batch borrows it.
    let mut jobs: Vec<PartJob> = Vec::new();
    for part in &batch.parts {
        let Some(tex) = bound.get(&part.name.to_ascii_lowercase()) else {
            continue;
        };
        if tex == "<none>" {
            continue;
        }
        let (s, e) = (part.start as usize, part.end as usize);
        let idx = batch.indices.get(s..e).unwrap_or(&[]);
        let mut tris = Vec::with_capacity(idx.len() / 3);
        for t in idx.chunks_exact(3) {
            let uv = |i: usize| batch.vertices.get(t[i] as usize).map(|v| v.uv);
            if let (Some(a), Some(b), Some(c)) = (uv(0), uv(1), uv(2)) {
                tris.push([a, b, c]);
            }
        }
        jobs.push((part.name.clone(), tex.clone(), tris));
    }

    let mut out = Vec::with_capacity(jobs.len());
    for (name, tex, tris) in jobs {
        let decoded = models.avatar_sheet(&tex).and_then(|d| d.rgba8_mip0());
        let Some((w, h, rgba)) = decoded else {
            out.push(PartCoverage {
                part: name,
                texture: tex,
                sheet: None,
                background: None,
                background_fraction: 0.0,
                coverage: Coverage::default(),
                sampled: SampledTexels::default(),
            });
            continue;
        };
        let mask = atlas_mask(w, h, &rgba);
        // Alpha from the archive (authored — it is what makes a sheet an overlay), RGB from the
        // uploaded copy (processed — it is what the screen samples). Reading both off one texture
        // was the divergence behind ledger E11: the report described the file while being read as
        // a statement about the render.
        let uploaded = models
            .avatar_sheet_uploaded(&tex)
            .and_then(caer_assets::dds::DdsTexture::rgba8_mip0)
            .filter(|(uw, uh, _)| *uw == w && *uh == h);
        let sampled = sample_texels(
            &tris,
            w,
            h,
            &rgba,
            uploaded.as_ref().map(|(_, _, v)| &v[..]),
        );
        out.push(PartCoverage {
            part: name,
            texture: tex,
            sheet: Some((w, h)),
            background: mask.background,
            background_fraction: mask.background_fraction,
            coverage: coverage(&tris, &mask),
            sampled,
        });
    }
    out
}

#[cfg(test)]
mod blob_tests {
    use super::*;

    fn frame(width: u32, height: u32, paint: impl Fn(u32, u32) -> bool) -> Vec<u8> {
        let mut v = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                if paint(x, y) {
                    v.extend_from_slice(&[200, 20, 200]);
                } else {
                    v.extend_from_slice(&[90, 70, 55]);
                }
            }
        }
        v
    }

    /// The distinction the probe exists to make: a hairline of edge bleed against a solid patch.
    ///
    /// Both are triangles sampling the space between islands. Only the second is what a player
    /// calls an unrendered triangle, and an instrument that scores them the same sends the next
    /// session chasing every silhouette in the game.
    #[test]
    fn a_hairline_of_bleed_is_distinguishable_from_a_solid_wedge() {
        // A 1px outline tracing a diagonal — real bleed follows a silhouette, so it is curved and
        // its bounding box is mostly empty — and a solid 20x20 block.
        let f = frame(64, 64, |x, y| {
            (y < 24 && x == y + 2) || ((30..50).contains(&x) && (30..50).contains(&y))
        });
        let blobs = filler_blobs(64, 64, &f);
        assert_eq!(
            blobs.len(),
            2,
            "a diagonal outline is one feature, not one per pixel: {blobs:?}"
        );
        let wedge = blobs[0];
        let hairline = blobs[1];
        assert_eq!(wedge.thickness(), 20, "the block is thick in both axes");
        // Density is the half that survives a diagonal; thickness is the half that survives a
        // straight edge. A blob is a wedge only when BOTH say so, which is what `is_wedge` checks.
        assert!(wedge.is_wedge(), "a solid block is a wedge");
        assert!(!hairline.is_wedge(), "a traced silhouette is not");
        assert_eq!(
            hairline.thickness(),
            24,
            "a diagonal's bounding box is square — thickness alone \
                                               cannot judge it, which is why density exists"
        );
        assert!(wedge.density() > 0.99, "a solid block fills its box");
        assert!(
            hairline.density() < 0.1,
            "a hairline barely fills its box: {}",
            hairline.density()
        );
    }

    #[test]
    fn a_clean_frame_reports_nothing() {
        let f = frame(32, 32, |_, _| false);
        assert!(filler_blobs(32, 32, &f).is_empty());
    }

    /// The probe colour must survive the scene dimming it, and no ordinary material may trip it.
    #[test]
    fn probe_detection_follows_brightness_but_not_material_colour() {
        for scale in [1.0_f32, 0.75, 0.5, 0.35] {
            let s = |v: u8| (f32::from(v) * scale) as u8;
            assert!(
                is_probe_pixel(s(255), s(0), s(255)),
                "magenta at {scale} brightness must still read as the probe"
            );
        }
        // Leather, skin, grass, stone, and the atlas grey that started all this.
        for (r, g, b) in [
            (106, 83, 65),
            (236, 174, 126),
            (60, 110, 50),
            (128, 128, 128),
            (72, 56, 44),
        ] {
            assert!(
                !is_probe_pixel(r, g, b),
                "({r},{g},{b}) must not read as the probe"
            );
        }
    }
}

#[cfg(test)]
mod tint_tests {
    use super::*;

    /// Every part tint must survive the scene dimming it, and stay distinguishable from its
    /// neighbours in the palette. If two indices collapse, a blob is attributed to the wrong part.
    #[test]
    fn part_tints_survive_lighting_and_stay_distinct() {
        for i in 0..PART_TINT_COUNT {
            let c = part_tint_colour(i);
            for scale in [1.0_f32, 0.8, 0.6, 0.4, 0.25] {
                let lit = |v: u8| (f32::from(v) * scale) as u8;
                assert_eq!(
                    part_tint_index(lit(c[0]), lit(c[1]), lit(c[2])),
                    Some(i),
                    "tint {i} at {scale} brightness must still read as {i}"
                );
            }
        }
    }

    /// The palette must actually be separated, or every test below is vacuous.
    #[test]
    fn the_palette_is_separated_enough_to_identify() {
        let sep = super::min_palette_separation();
        assert!(
            sep > 100.0,
            "palette entries are only {sep} apart — indices will collide"
        );
        let mut seen = std::collections::HashSet::new();
        for i in 0..PART_TINT_COUNT {
            assert!(
                seen.insert(part_tint_colour(i)),
                "tint {i} repeats an earlier colour"
            );
        }
    }

    /// Ordinary scenery must not be attributed to a part.
    ///
    /// The first version of this listed six hand-picked colours and passed while the matcher was
    /// claiming **2.5% of an ordinary capture** as part tints — a test built from the cases its
    /// author already believed were fine. This sweeps the whole hue circle at every saturation a
    /// real material occupies, which is the population that actually matters.
    #[test]
    fn scenery_is_not_mistaken_for_a_part_tint() {
        let hsv = |h: f32, s: f32, v: f32| -> (u8, u8, u8) {
            let c = v * s;
            let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
            let (r, g, b) = match (h / 60.0) as u32 {
                0 => (c, x, 0.0),
                1 => (x, c, 0.0),
                2 => (0.0, c, x),
                3 => (0.0, x, c),
                4 => (x, 0.0, c),
                _ => (c, 0.0, x),
            };
            let m = v - c;
            (
                ((r + m) * 255.0) as u8,
                ((g + m) * 255.0) as u8,
                ((b + m) * 255.0) as u8,
            )
        };
        let mut claimed = 0usize;
        let mut total = 0usize;
        for hue in (0..360).step_by(5) {
            // Up to 0.7 saturation covers grass, skin, leather, stone, sky and snow. The tints sit
            // at 1.0, and the gap is what the matcher is entitled to rely on.
            for sat in [0.0_f32, 0.15, 0.3, 0.45, 0.6, 0.7] {
                for val in [0.25_f32, 0.5, 0.75, 1.0] {
                    let (r, g, b) = hsv(hue as f32, sat, val);
                    total += 1;
                    if part_tint_index(r, g, b).is_some() {
                        claimed += 1;
                    }
                }
            }
        }
        assert_eq!(
            claimed, 0,
            "{claimed} of {total} ordinary material colours were claimed as part tints"
        );

        // Named real samples too, so the sweep cannot drift away from actual game pixels.
        for (r, g, b) in [
            (60, 110, 50),   // grass
            (128, 128, 128), // the boots atlas filler
            (230, 233, 240), // snow
            (120, 150, 190), // sky
            (106, 83, 65),   // leather
            (236, 174, 126), // skin
            (255, 190, 150), // lit skin — the orange false positive that started this
            (72, 56, 44),    // shadowed leather
        ] {
            assert_eq!(
                part_tint_index(r, g, b),
                None,
                "({r},{g},{b}) must not name a part"
            );
        }
    }
}
