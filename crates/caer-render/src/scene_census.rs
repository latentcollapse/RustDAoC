//! What is actually in a pre-world realm scene.
//!
//! A census, not a verdict. It enumerates every model instance with its world box, its textures
//! and where it sits relative to the stage floor, and it can diff two censuses. That shape is
//! deliberate: without a retail reference there is no oracle for "this prop should not be here",
//! so the useful thing is a stable record that a reference capture can later be diffed against,
//! and a realm-to-realm comparison that surfaces the differences between scenes we *do* have.
//!
//! The diff is its own control: a realm against itself must show nothing, and two different realms
//! must show something. An instrument that cannot tell Albion from Midgard cannot tell anything.

use std::collections::BTreeMap;

use crate::preworld_scene::PreWorldScene;

/// Where an instance's lowest point sits relative to the stage floor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Grounding {
    /// Bottom is within `tolerance` of the floor.
    Planted,
    /// Bottom is this far above the floor.
    Floating(f32),
    /// Bottom is this far below the floor.
    Sunk(f32),
}

/// Classify an instance's bottom against the stage floor.
///
/// `ground_z` is the scene's 10th-percentile vertex Z — an approximate floor, not a surface, so a
/// prop on a rise is legitimately "floating" by this measure. **This finds candidates, not
/// defects.** Only a prop far outside the spread of its neighbours is worth looking at, which is
/// why the census reports the distribution alongside each verdict.
#[must_use]
pub fn grounding(box_min_z: f32, ground_z: f32, tolerance: f32) -> Grounding {
    let d = box_min_z - ground_z;
    if d.abs() <= tolerance {
        Grounding::Planted
    } else if d > 0.0 {
        Grounding::Floating(d)
    } else {
        Grounding::Sunk(-d)
    }
}

/// One drawable range of a scene: a texture, a blend mode, and the geometry that uses them.
///
/// **Parts, not instances.** A realm scene loads as a single merged batch with one instance, so an
/// instance-level census reports one row and says nothing. The parts are where the scene actually
/// is: Albion has 264, Midgard 20, Hibernia 756.
#[derive(Clone, Debug, PartialEq)]
pub struct ScenePart {
    pub model: usize,
    pub part: usize,
    pub texture: Option<String>,
    pub alpha: caer_assets::nif::AlphaMode,
    pub triangles: usize,
    /// World AABB of the vertices this range actually references, or `None` when it references
    /// none.
    ///
    /// The distinction is not pedantry. An earlier version reported an empty range as the box
    /// `[0,0,0]..[0,0,0]`, which reads exactly like geometry sitting at the world origin — so
    /// "this part has no vertices" and "this part is at the origin" produced the same row, and the
    /// first `a_lampglow_01` finding could not tell which it was looking at.
    pub bounds: Option<([f32; 3], [f32; 3])>,
    pub grounding: Grounding,
    /// Indices in this range that point past the end of the vertex buffer.
    pub out_of_range: OutOfRange,
}

impl ScenePart {
    #[must_use]
    pub fn box_min(&self) -> Option<[f32; 3]> {
        self.bounds.map(|(lo, _)| lo)
    }

    #[must_use]
    pub fn box_max(&self) -> Option<[f32; 3]> {
        self.bounds.map(|(_, hi)| hi)
    }

    #[must_use]
    pub fn centre(&self) -> Option<[f32; 3]> {
        self.bounds.map(|(lo, hi)| {
            [
                (lo[0] + hi[0]) * 0.5,
                (lo[1] + hi[1]) * 0.5,
                (lo[2] + hi[2]) * 0.5,
            ]
        })
    }

    /// Largest horizontal extent — a backdrop dome is enormous, a rock is not.
    #[must_use]
    pub fn span(&self) -> Option<f32> {
        self.bounds
            .map(|(lo, hi)| (hi[0] - lo[0]).max(hi[1] - lo[1]))
    }

    /// A range that references no vertex at all. It draws nothing, whatever its texture says.
    #[must_use]
    pub fn is_empty_range(&self) -> bool {
        self.bounds.is_none()
    }
}

/// Indices in a part range that point past the end of the vertex buffer.
///
/// Nothing downstream can draw these, and depending on the backend they either drop the primitive
/// or read whatever memory follows. A scene should have none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutOfRange {
    pub indices: usize,
    pub max_index: u32,
    pub vertex_count: usize,
    /// Referenced vertices whose position is not finite. NaN geometry is undefined on the GPU and
    /// silently defeats any min/max measurement, because `f32::min` returns the other operand.
    pub non_finite: usize,
}

/// Everything the census records about one realm.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneCensus {
    pub realm: u8,
    pub ground_z: f32,
    pub stage_radius: f32,
    pub subject_anchor: [f32; 3],
    pub camera_bearing_deg: f32,
    pub parts: Vec<ScenePart>,
    /// Texture -> how many parts draw with it.
    pub texture_use: BTreeMap<String, usize>,
    /// Textures the scene asked for and did not get. Non-empty is always a finding.
    pub missing_textures: Vec<String>,
}

impl SceneCensus {
    #[must_use]
    pub fn part_count(&self) -> usize {
        self.parts.len()
    }

    #[must_use]
    pub fn triangles(&self) -> usize {
        self.parts.iter().map(|p| p.triangles).sum()
    }

    /// Ranges that reference no vertex at all — they draw nothing whatever their texture says.
    #[must_use]
    pub fn empty_ranges(&self) -> usize {
        self.parts.iter().filter(|p| p.is_empty_range()).count()
    }

    /// Total indices across the scene that point past the vertex buffer.
    #[must_use]
    pub fn out_of_range_indices(&self) -> usize {
        self.parts.iter().map(|p| p.out_of_range.indices).sum()
    }

    /// Parts that composite with blending rather than opaquely.
    #[must_use]
    pub fn blended(&self) -> usize {
        self.parts.iter().filter(|p| p.alpha.is_blended()).count()
    }

    /// Parts whose bottom sits furthest above the floor, worst first.
    #[must_use]
    pub fn most_floating(&self, n: usize) -> Vec<&ScenePart> {
        let mut v: Vec<&ScenePart> = self
            .parts
            .iter()
            .filter(|i| !i.is_empty_range() && matches!(i.grounding, Grounding::Floating(_)))
            .collect();
        v.sort_by(|a, b| match (a.grounding, b.grounding) {
            (Grounding::Floating(x), Grounding::Floating(y)) => y.total_cmp(&x),
            _ => std::cmp::Ordering::Equal,
        });
        v.truncate(n);
        v
    }
}

/// Tolerance for "planted", as a fraction of the stage's own radius.
///
/// Scaled to the scene rather than fixed in world units: the three realms are built at different
/// sizes, and a constant that suits Albion would call half of Midgard floating.
const PLANTED_FRACTION: f32 = 0.02;

/// Enumerate a loaded scene, part by part.
#[must_use]
pub fn census(scene: &PreWorldScene) -> SceneCensus {
    let tolerance = (scene.stage_radius * PLANTED_FRACTION).max(1.0);
    let mut parts = Vec::new();
    let mut texture_use: BTreeMap<String, usize> = BTreeMap::new();

    for (mi, batch) in scene.models.iter().enumerate() {
        // A scene batch carries one instance; anything else would place the same geometry twice
        // and the boxes below would describe only the first placement.
        let inst = batch.instances.first();
        let (off, scale) = inst.map_or(([0.0; 3], 1.0), |i| (i.pos, i.scale));
        for (pi, part) in batch.parts.iter().enumerate() {
            let (s, e) = (part.start as usize, part.end as usize);
            let idx = batch.indices.get(s..e).unwrap_or(&[]);
            let mut lo = [f32::MAX; 3];
            let mut hi = [f32::MIN; 3];
            for &vi in idx {
                let Some(v) = batch.vertices.get(vi as usize) else {
                    continue;
                };
                for k in 0..3 {
                    let w = off[k] + v.pos[k] * scale;
                    lo[k] = lo[k].min(w);
                    hi[k] = hi[k].max(w);
                }
            }
            // An empty range stays `None` rather than collapsing to a zero box at the origin.
            let bounds = (lo[0] <= hi[0]).then_some((lo, hi));
            let bad = idx
                .iter()
                .filter(|&&vi| batch.vertices.get(vi as usize).is_none())
                .count();
            let non_finite = idx
                .iter()
                .filter(|&&vi| {
                    batch
                        .vertices
                        .get(vi as usize)
                        .is_some_and(|v| !v.pos.iter().all(|c| c.is_finite()))
                })
                .count();
            let out_of_range = OutOfRange {
                indices: bad,
                max_index: idx.iter().copied().max().unwrap_or(0),
                vertex_count: batch.vertices.len(),
                non_finite,
            };
            if let Some(t) = part.texture.as_ref() {
                *texture_use.entry(t.clone()).or_default() += 1;
            }
            parts.push(ScenePart {
                model: mi,
                part: pi,
                texture: part.texture.clone(),
                alpha: part.alpha,
                triangles: idx.len() / 3,
                bounds,
                out_of_range,
                grounding: bounds.map_or(Grounding::Planted, |(lo, _)| {
                    grounding(lo[2], scene.ground_z, tolerance)
                }),
            });
        }
    }

    SceneCensus {
        realm: scene.realm,
        ground_z: scene.ground_z,
        stage_radius: scene.stage_radius,
        subject_anchor: scene.subject_anchor.into(),
        camera_bearing_deg: scene.camera_bearing_deg,
        parts,
        texture_use,
        missing_textures: scene.missing_textures.clone(),
    }
}

/// What a sheet's alpha channel actually contains.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlphaStats {
    pub width: u32,
    pub height: u32,
    /// Mean alpha over mip 0, 0..1.
    pub mean: f32,
    /// Share of texels at full opacity.
    pub fully_opaque: f32,
    /// Share of texels the skinned shader's 0.4 cutout would keep.
    pub above_cutout: f32,
}

impl AlphaStats {
    /// Does this sheet carry any transparency worth compositing?
    #[must_use]
    pub fn has_transparency(&self) -> bool {
        self.fully_opaque < 0.995
    }
}

/// How a part's declared blend mode compares with what its sheet actually holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaAgreement {
    /// Declared opaque, sheet is opaque; or declared blended, sheet has transparency.
    Consistent,
    /// Declared opaque while the sheet carries transparency — the art expects compositing and
    /// will render with hard edges, or with whatever RGB sits under its transparent texels.
    OpaqueOverTransparentArt,
    /// Declared blended while the sheet is fully opaque — nothing to composite, and it pays the
    /// sort order and overdraw of a blended pass for a solid surface.
    BlendedOverOpaqueArt,
    /// No sheet resolved, so nothing can be said.
    Unknown,
}

/// Compare one part's declared mode against its sheet.
#[must_use]
pub fn alpha_agreement(
    alpha: caer_assets::nif::AlphaMode,
    stats: Option<&AlphaStats>,
) -> AlphaAgreement {
    let Some(st) = stats else {
        return AlphaAgreement::Unknown;
    };
    // Additive is deliberately excluded: an additive pass uses RGB as intensity and does not need
    // an alpha channel at all, so "opaque sheet, additive mode" is normal rather than a finding.
    if matches!(alpha, caer_assets::nif::AlphaMode::Add) {
        return AlphaAgreement::Consistent;
    }
    match (alpha.is_blended(), st.has_transparency()) {
        (false, true) => AlphaAgreement::OpaqueOverTransparentArt,
        (true, false) => AlphaAgreement::BlendedOverOpaqueArt,
        _ => AlphaAgreement::Consistent,
    }
}

/// Decode every texture a scene resolved and measure its alpha.
#[must_use]
pub fn texture_alpha(scene: &PreWorldScene) -> BTreeMap<String, AlphaStats> {
    let mut out = BTreeMap::new();
    for (name, tex) in &scene.textures {
        let Some((w, h, rgba)) = tex.rgba8_mip0() else {
            continue;
        };
        let total = (rgba.len() / 4).max(1);
        let mut sum = 0u64;
        let mut full = 0usize;
        let mut above = 0usize;
        for px in rgba.chunks_exact(4) {
            sum += u64::from(px[3]);
            if px[3] == 255 {
                full += 1;
            }
            if px[3] >= 102 {
                above += 1;
            }
        }
        out.insert(
            name.clone(),
            AlphaStats {
                width: w,
                height: h,
                mean: sum as f32 / (total as f32 * 255.0),
                fully_opaque: full as f32 / total as f32,
                above_cutout: above as f32 / total as f32,
            },
        );
    }
    out
}

/// Height of the scene surface directly under a point, and what was found there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceProbe {
    /// Highest triangle surface at or below `from_z`, if any.
    pub below: Option<f32>,
    /// Lowest triangle surface above `from_z`, if any — geometry the point is *inside* or under.
    pub above: Option<f32>,
    /// Triangles whose horizontal projection contains the point.
    pub hits: usize,
    /// Index of the draw range that owns the `below` surface.
    ///
    /// Reported because "is the anchor on the ground" is only answered if the thing under it is
    /// the ground. A marker quad sitting at the anchor would satisfy a height check while telling
    /// you nothing — the same circularity as measuring the subject against a camera aimed at it.
    pub below_part: Option<usize>,
}

impl SurfaceProbe {
    /// Distance from `z` down to the surface. Negative means the point is below it.
    #[must_use]
    pub fn drop_from(&self, z: f32) -> Option<f32> {
        self.below.map(|s| z - s)
    }
}

/// Where the scene's geometry is, directly under `(x, y)`.
///
/// A downward point-in-triangle test over every triangle in the scene, interpolating the plane at
/// `(x, y)`. This exists because `subject_projects_to_screen_centre` cannot answer whether the
/// subject stands in the right place: it compares the subject against a camera that was aimed at
/// the subject, so it agrees by construction and can only catch a projection-maths bug.
///
/// Whether the anchor sits ON the ground is a question the scene itself can answer, with no
/// reference capture needed.
#[must_use]
pub fn surface_under(scene: &PreWorldScene, x: f32, y: f32, from_z: f32) -> SurfaceProbe {
    let mut below: Option<f32> = None;
    let mut above: Option<f32> = None;
    let mut hits = 0usize;
    let mut below_part: Option<usize> = None;
    for batch in &scene.models {
        let inst = batch.instances.first();
        let (off, scale) = inst.map_or(([0.0; 3], 1.0), |i| (i.pos, i.scale));
        let world = |vi: u32| -> Option<[f32; 3]> {
            let v = batch.vertices.get(vi as usize)?;
            let p = [
                off[0] + v.pos[0] * scale,
                off[1] + v.pos[1] * scale,
                off[2] + v.pos[2] * scale,
            ];
            p.iter().all(|c| c.is_finite()).then_some(p)
        };
        // Which draw range a triangle belongs to, by its index offset.
        let part_of = |tri_start: usize| -> Option<usize> {
            batch
                .parts
                .iter()
                .position(|p| tri_start >= p.start as usize && tri_start < p.end as usize)
        };
        for (ti, t) in batch.indices.chunks_exact(3).enumerate() {
            let (Some(a), Some(b), Some(c)) = (world(t[0]), world(t[1]), world(t[2])) else {
                continue;
            };
            // Barycentric test in the XY plane.
            let d = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
            if d.abs() < 1e-6 {
                continue; // edge-on: contributes no surface here
            }
            let l0 = ((b[1] - c[1]) * (x - c[0]) + (c[0] - b[0]) * (y - c[1])) / d;
            let l1 = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / d;
            let l2 = 1.0 - l0 - l1;
            if l0 < 0.0 || l1 < 0.0 || l2 < 0.0 {
                continue;
            }
            hits += 1;
            let z = l0 * a[2] + l1 * b[2] + l2 * c[2];
            if z <= from_z {
                if below.is_none_or(|cur: f32| z > cur) {
                    below = Some(z);
                    below_part = part_of(ti * 3);
                }
            } else {
                above = Some(above.map_or(z, |cur: f32| cur.min(z)));
            }
        }
    }
    SurfaceProbe {
        below,
        above,
        hits,
        below_part,
    }
}

/// A difference between two censuses.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneDiff {
    /// A scalar field differs.
    Field {
        name: &'static str,
        a: String,
        b: String,
    },
    /// A texture is used by one scene and not the other.
    TextureOnlyIn {
        realm: u8,
        texture: String,
        instances: usize,
    },
    /// Both use it, different number of instances.
    TextureCount { texture: String, a: usize, b: usize },
}

/// Compare two censuses.
///
/// Intended for a realm against a reference of the same realm. Run across two *different* realms
/// it reports the (large) genuine differences, which is what makes it its own control.
#[must_use]
pub fn diff(a: &SceneCensus, b: &SceneCensus) -> Vec<SceneDiff> {
    let mut out = Vec::new();
    let mut field = |name: &'static str, x: String, y: String| {
        if x != y {
            out.push(SceneDiff::Field { name, a: x, b: y });
        }
    };
    field(
        "parts",
        a.part_count().to_string(),
        b.part_count().to_string(),
    );
    field(
        "triangles",
        a.triangles().to_string(),
        b.triangles().to_string(),
    );
    field(
        "blended_parts",
        a.blended().to_string(),
        b.blended().to_string(),
    );
    field(
        "empty_ranges",
        a.empty_ranges().to_string(),
        b.empty_ranges().to_string(),
    );
    field(
        "camera_bearing_deg",
        format!("{:.3}", a.camera_bearing_deg),
        format!("{:.3}", b.camera_bearing_deg),
    );
    field(
        "ground_z",
        format!("{:.2}", a.ground_z),
        format!("{:.2}", b.ground_z),
    );
    field(
        "stage_radius",
        format!("{:.2}", a.stage_radius),
        format!("{:.2}", b.stage_radius),
    );

    for (tex, count) in &a.texture_use {
        match b.texture_use.get(tex) {
            None => out.push(SceneDiff::TextureOnlyIn {
                realm: a.realm,
                texture: tex.clone(),
                instances: *count,
            }),
            Some(other) if other != count => out.push(SceneDiff::TextureCount {
                texture: tex.clone(),
                a: *count,
                b: *other,
            }),
            _ => {}
        }
    }
    for (tex, count) in &b.texture_use {
        if !a.texture_use.contains_key(tex) {
            out.push(SceneDiff::TextureOnlyIn {
                realm: b.realm,
                texture: tex.clone(),
                instances: *count,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grounding_separates_planted_from_floating_and_sunk() {
        assert_eq!(grounding(100.0, 100.0, 2.0), Grounding::Planted);
        assert_eq!(grounding(101.5, 100.0, 2.0), Grounding::Planted);
        // The boundary belongs to the planted case, as it does for entity ground snapping.
        assert_eq!(grounding(102.0, 100.0, 2.0), Grounding::Planted);
        assert_eq!(grounding(110.0, 100.0, 2.0), Grounding::Floating(10.0));
        assert_eq!(grounding(90.0, 100.0, 2.0), Grounding::Sunk(10.0));
    }

    fn sample(realm: u8, textures: &[(&str, usize)]) -> SceneCensus {
        SceneCensus {
            realm,
            ground_z: 0.0,
            stage_radius: 100.0,
            subject_anchor: [0.0; 3],
            camera_bearing_deg: 60.0,
            parts: Vec::new(),
            texture_use: textures
                .iter()
                .map(|(t, n)| ((*t).to_string(), *n))
                .collect(),
            missing_textures: Vec::new(),
        }
    }

    /// The diff's own control: a census against itself is silent, and against a different scene
    /// it is not. An instrument that reports nothing in both cases is reporting nothing.
    #[test]
    fn a_census_matches_itself_and_differs_from_another_scene() {
        let alb = sample(1, &[("alb_rock01.dds", 12), ("alb_keep.dds", 1)]);
        assert!(diff(&alb, &alb).is_empty(), "a scene must match itself");

        let mid = sample(2, &[("mid_snow.dds", 30), ("alb_keep.dds", 1)]);
        let d = diff(&alb, &mid);
        assert!(
            d.iter().any(|x| matches!(x, SceneDiff::TextureOnlyIn { texture, .. } if texture == "alb_rock01.dds")),
            "{d:?}"
        );
        assert!(
            d.iter().any(|x| matches!(x, SceneDiff::TextureOnlyIn { texture, .. } if texture == "mid_snow.dds")),
            "{d:?}"
        );
    }

    #[test]
    fn a_changed_instance_count_for_a_shared_texture_is_reported() {
        let a = sample(1, &[("tree.dds", 12)]);
        let b = sample(1, &[("tree.dds", 9)]);
        assert_eq!(
            diff(&a, &b),
            vec![SceneDiff::TextureCount {
                texture: "tree.dds".into(),
                a: 12,
                b: 9
            }]
        );
    }

    #[test]
    fn a_changed_bearing_is_reported_as_a_field() {
        let a = sample(1, &[]);
        let mut b = sample(1, &[]);
        b.camera_bearing_deg = 23.0;
        assert_eq!(
            diff(&a, &b),
            vec![SceneDiff::Field {
                name: "camera_bearing_deg",
                a: "60.000".into(),
                b: "23.000".into()
            }]
        );
    }
}
