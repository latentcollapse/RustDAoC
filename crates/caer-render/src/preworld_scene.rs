//! The 3D environment behind character select and character create (B1 / H10).
//!
//! Retail does not draw a flat plate on these screens. `character_selection.tga` and
//! `character_creation.tga` are a stone frame around a **transparent void**, and the client renders
//! a modelled realm scene behind them — Camelot's courtyard, a Midgard snowfield, a Hibernian
//! grove. Without it the middle of both screens is black, which is what CAER showed.
//!
//! Each scene is one NetImmerse model in `pregame/charScreen{Alb,Mid,Hib}.npk`. Its textures come
//! from four places, because they are ordinary world art and the client resolves them across its
//! whole texture path: the loose files beside the archive in `pregame/`, the shared `items/` and
//! `effects/` art, a few members of `pregame*.mpk`, and — for the handful of effect textures that
//! ship nowhere else — the zone NIF libraries under `zones/`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use glam::Vec3;

use crate::camera::{fov_y_for_horizontal, PREWORLD_FOV_X};
use crate::terrain::ModelBatch;

/// A loaded realm scene: merged geometry plus the pages it draws with.
pub struct PreWorldScene {
    /// Realm this was built for (1 Albion, 2 Midgard, 3 Hibernia).
    pub realm: u8,
    pub models: Vec<ModelBatch>,
    /// Emitters authored into the stage: Midgard's blowing snow, Hibernia's portal motes. Albion
    /// carries none, which is the asset's own answer and not a gap.
    pub emitters: Vec<caer_assets::nif::ParticleEmitterDef>,
    pub textures: HashMap<String, caer_assets::dds::DdsTexture>,
    /// World-space bounds of the geometry, for framing the camera.
    pub bound_min: [f32; 3],
    pub bound_max: [f32; 3],
    /// Approximate stage floor: the 10th percentile of vertex Z. The scenes sit on a ground plane
    /// well below their skyline, so the mean or the centre both land in mid-air.
    pub ground_z: f32,
    /// Radius of the close stage content (10th percentile of horizontal distance from the origin).
    /// The backdrop dome is an order of magnitude further out, so this is what to frame against.
    pub content_radius: f32,
    /// Centre of the stage set — the geometry that is not the backdrop dome.
    ///
    /// Every scene is a small built stage sitting inside a huge painted sphere. Bounds and
    /// centroids over *all* vertices describe the sphere and say nothing about where the built
    /// part is, which is why framing against them put the camera in the dirt.
    pub stage_center: Vec3,
    /// 90th-percentile horizontal distance of that set from the origin — how wide the stage is.
    pub stage_radius: f32,
    /// 90th-percentile height of that set. Hibernia's canopy is 1600 units up and Midgard's keep
    /// is barely off the floor, so the eye height has to come from the scene, not a constant.
    pub stage_top: f32,
    /// Ground height directly under the scene origin — where the character stands.
    ///
    /// Not [`Self::ground_z`], which is a percentile over the whole scene including distant
    /// terrain, and so can sit well below the spot the camera is actually pointed at.
    pub origin_floor_z: f32,
    /// Where the character stands, in scene space — the authored `collidee` collision node.
    ///
    /// This is the scene's own answer, not a fit. See [`load`] for the measurement that picked it
    /// over the origin and over `flag_snapshot01`.
    pub subject_anchor: Vec3,
    /// Compass bearing the camera looks along, in degrees, measured as `atan2(dx, dy)` from the
    /// anchor. Derived from where the scene's own landmarks sit — see [`landmark_bearing`].
    pub camera_bearing_deg: f32,
    /// Textures the scene asked for and the search path could not produce, sorted.
    ///
    /// Every entry is a piece of geometry drawn with the white fallback — a blank sheet hanging
    /// in the middle of a character screen. Empty on the retail tree, so the scene proof asserts
    /// it: a miss here is a defect in our search path, not absent client data.
    pub missing_textures: Vec<String>,
    /// Parts refused at load because their vertices were not finite. Non-empty is always a finding.
    pub dropped_parts: Vec<String>,
}

/// Fallback body height, for framing a stage with nobody on it yet.
///
/// The real number comes from the assembled avatar — see [`framing_around_character`]. This is
/// only what an empty character screen composes against.
pub const CHARACTER_HEIGHT: f32 = 70.0;

/// `charScreen{Alb,Mid,Hib}.npk` — the archive per realm.
#[must_use]
pub fn scene_archive(realm: u8) -> &'static str {
    match realm {
        2 => "pregame/charScreenMid.npk",
        3 => "pregame/charScreenHib.npk",
        _ => "pregame/charScreenAlb.npk",
    }
}

/// Loose directories a character-screen scene resolves textures from first.
///
/// The NIFs reference textures by bare name and the client resolves them across its whole texture
/// path, not just the folder the model sits in: Albion's torch wants `fireshell3.dds`, which ships
/// in `items/`. `pregame` is listed last so a local copy still wins.
const LOOSE_TEXTURE_DIRS: [&str; 4] = ["items", "effects", "pregame", "pregame/textures"];

/// The shared world texture libraries, searched only when the pregame path comes up short.
///
/// These scenes are ordinary NIFs and reuse ordinary world effect art: Albion's char screen asks
/// for `e_corona_02_reverse_fade.dds`, which ships in `zones/Nifs` and nowhere near `pregame/`.
/// They are also 9,400 directory entries between them on a spinning disk, and the common case
/// needs none of it — hence the second pass rather than a wider first one.
const SHARED_TEXTURE_DIRS: [&str; 2] = ["zones/Nifs", "zones/Dnifs"];

/// Index the loose texture files under `dirs`. Later directories win.
///
/// The realm banners are **not** here — see [`archive_textures`], which is the half this
/// function's doc comment used to claim and the code never did, and is why every realm's banner
/// hung white.
fn texture_index(client_root: &Path, dirs: &[&str]) -> HashMap<String, PathBuf> {
    let mut index = HashMap::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(client_root.join(dir)) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if name.ends_with(".dds") || name.ends_with(".tga") {
                index.insert(name, e.path());
            }
        }
    }
    index
}

/// Every texture the model asks for, normalised the way [`load_model_texture`] will look it up.
///
/// [`load_model_texture`]: crate::dungeon_mesh::load_model_texture
fn wanted_texture_keys(model: &caer_assets::nif::Model) -> Vec<String> {
    let mut wanted: Vec<String> = model
        .parts
        .iter()
        .flat_map(|p| [p.texture.as_deref(), p.texture2.as_deref()])
        .flatten()
        .map(|t| {
            let base = t
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(t)
                .to_ascii_lowercase();
            // Everything resolves as `.dds`, whatever the NIF spelled.
            match base.rsplit_once('.') {
                Some((stem, ext)) if ext != "dds" => format!("{stem}.dds"),
                Some(_) => base,
                None => format!("{base}.dds"),
            }
        })
        .collect();
    wanted.sort();
    wanted.dedup();
    wanted
}

/// `pregame*.mpk` archives that carry scene textures the loose directory does not.
const TEXTURE_ARCHIVES: [&str; 3] = [
    "pregame/pregame.mpk",
    "pregame/pregame002.mpk",
    "pregame/pregame003.mpk",
];

/// Decode the textures a scene wants that are only inside `pregame*.mpk`.
///
/// `banner_albion.dds`, `banner_midgard.dds` and `banner_hibernia.dds` live there and nowhere
/// else. Without this each realm's banner fell through to the white fallback bind — a blank white
/// sheet hanging in the middle of the character screen in all three realms.
///
/// Pre-inserted into `textures` so `load_model_texture`'s own `contains_key` short-circuit finds
/// them; nothing else has to know where a texture came from.
fn archive_textures(
    client_root: &Path,
    model: &caer_assets::nif::Model,
    loose: &HashMap<String, PathBuf>,
    textures: &mut HashMap<String, caer_assets::dds::DdsTexture>,
) {
    // What does the scene ask for that the loose directory cannot answer?
    let mut wanted: Vec<String> = wanted_texture_keys(model)
        .into_iter()
        .filter(|k| !loose.contains_key(k))
        .collect();
    if wanted.is_empty() {
        return;
    }
    for arch in TEXTURE_ARCHIVES {
        if wanted.is_empty() {
            return;
        }
        let path = client_root.join(arch);
        let Ok(names) = caer_assets::list_names(&path) else {
            continue;
        };
        // Members are cased however the archive was built; the wish-list is lowered.
        for actual in names {
            let key = actual.to_ascii_lowercase();
            if !wanted.contains(&key) {
                continue;
            }
            let decoded = caer_assets::open_member(&path, &actual)
                .ok()
                .flatten()
                .and_then(|b| caer_assets::dds::read_model_dds(&b).ok());
            if let Some(tex) = decoded {
                textures.insert(key.clone(), tex);
                wanted.retain(|w| *w != key);
            }
        }
    }
    if !wanted.is_empty() {
        log::info!("preworld scene: textures not found loose or in pregame*.mpk: {wanted:?}");
    }
}

/// Decoded scenes, kept for the life of the process and keyed by realm.
///
/// Decoding one costs ~810 ms on this tree (measured, `runtime_axes::preworld_stage_timings`):
/// an NPK read, a NIF parse of up to 66k vertices, and 24-28 DDS decodes. The client already
/// avoided repeating it while sitting on one realm, but bouncing Realm -> CharSelect -> Realm paid
/// it again every time, which is the hitch Matt reported. There are exactly three of these and
/// they total 39.6 MiB decoded, so holding them is cheaper than re-reading them.
static SCENE_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<u8, std::sync::Arc<PreWorldScene>>>,
> = std::sync::OnceLock::new();

/// Load a realm's character-screen scene, decoding it at most once per process.
///
/// `None` when the archive is absent or carries no renderable geometry — the caller then draws the
/// plate over black, exactly as before, rather than failing the screen.
#[must_use]
pub fn load_cached(client_root: &Path, realm: u8) -> Option<std::sync::Arc<PreWorldScene>> {
    let cache = SCENE_CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(&realm).cloned()) {
        return Some(hit);
    }
    let scene = std::sync::Arc::new(load(client_root, realm)?);
    if let Ok(mut c) = cache.lock() {
        c.insert(realm, scene.clone());
    }
    Some(scene)
}

/// Decode a realm's character-screen scene. Prefer [`load_cached`] on any product path.
#[must_use]
pub fn load(client_root: &Path, realm: u8) -> Option<PreWorldScene> {
    let rel = scene_archive(realm);
    let path = client_root.join(rel);
    if !path.exists() {
        log::warn!("preworld scene: missing archive {rel} ({})", path.display());
        eprintln!("preworld scene: missing archive {rel} ({})", path.display());
        return None;
    }
    let nif = match caer_assets::open_first(&path, |n| n.to_ascii_lowercase().ends_with(".nif")) {
        Ok(Some(entry)) => entry,
        Ok(None) => {
            log::warn!("preworld scene: {rel} has no .nif member");
            eprintln!("preworld scene: {rel} has no .nif member");
            return None;
        }
        Err(e) => {
            log::warn!("preworld scene: failed to open {rel}: {e}");
            eprintln!("preworld scene: failed to open {rel}: {e}");
            return None;
        }
    };
    let mut model = match caer_assets::nif::read_model(&nif.data) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("preworld scene: {} — {e}", path.display());
            eprintln!("preworld scene: {} — {e}", path.display());
            return None;
        }
    };
    // NaN vertices are undefined behaviour on the GPU — the primitive may be dropped, or it may
    // read whatever follows in the buffer. Refuse them, and name what was refused.
    //
    // Not cosmetic tidying: a NaN coordinate defeats min/max silently, because `f32::min` returns
    // the other operand. That is why `a_lampglow_01` read first as a zero-sized box at the world
    // origin and then as an index range referencing no vertex, before anything asked whether the
    // coordinates were finite.
    //
    // **The NaN is authored, not ours.** All three scene NIFs carry a node `Box02` whose
    // translation is three `0x7fc00000` quiet NaNs, with a clean identity rotation and scale 1.0
    // after it — byte-identical in Albion, Midgard and Hibernia. `xform_apply` then propagates
    // that into every vertex of the shape hanging off it, faithfully. Refusing the part is the
    // right response to art that cannot be placed; there is nothing upstream to fix.
    let dropped_parts: Vec<String> = model
        .parts
        .iter()
        .filter(|p| p.positions.iter().any(|v| !v.iter().all(|c| c.is_finite())))
        .map(|p| {
            format!(
                "{} ({})",
                p.name,
                p.texture.as_deref().unwrap_or("<untextured>")
            )
        })
        .collect();
    if !dropped_parts.is_empty() {
        log::warn!(
            "preworld scene: {rel} — dropping {} part(s) with non-finite vertices: {dropped_parts:?}",
            dropped_parts.len()
        );
        model
            .parts
            .retain(|p| p.positions.iter().all(|v| v.iter().all(|c| c.is_finite())));
    }

    let billboards = model.parts.iter().filter(|p| p.billboard).count();
    for p in &mut model.parts {
        if p.billboard && !p.alpha.is_blended() {
            p.alpha = caer_assets::nif::AlphaMode::Blend;
        }
    }

    let mut index = texture_index(client_root, &LOOSE_TEXTURE_DIRS);
    let mut textures = HashMap::new();
    archive_textures(client_root, &model, &index, &mut textures);
    // Anything the pregame path still cannot answer is world art, not missing data. Widen the
    // search once rather than binding a white sheet and calling it a diagnostic.
    let unresolved: Vec<String> = wanted_texture_keys(&model)
        .into_iter()
        .filter(|k| {
            // Same two lookups `load_model_texture` will make: the `.dds` key, then the `.tga`
            // that shipped under it. Checking only the first sends us scanning `zones/` for art
            // sitting in `pregame/` under the other extension.
            let tga = k.strip_suffix(".dds").map(|stem| format!("{stem}.tga"));
            !textures.contains_key(k)
                && !index.contains_key(k)
                && !tga.is_some_and(|t| index.contains_key(&t))
        })
        .collect();
    if !unresolved.is_empty() {
        log::info!(
            "preworld scene: {rel} wants {} texture(s) outside pregame/ — searching {SHARED_TEXTURE_DIRS:?}: {unresolved:?}",
            unresolved.len()
        );
        for (name, path) in texture_index(client_root, &SHARED_TEXTURE_DIRS) {
            index.entry(name).or_insert(path);
        }
    }
    let mut failed = HashSet::new();
    let keys: Vec<Option<String>> = model
        .parts
        .iter()
        .map(|p| {
            p.texture.as_deref().and_then(|t| {
                crate::dungeon_mesh::load_model_texture(&index, t, &mut textures, &mut failed)
            })
        })
        .collect();

    // The stage grounds are two sheets blended by a per-vertex mask. Resolve the second layer
    // through the same index as the first, so a missing one degrades to single-layer rather than
    // to the white fallback.
    // Emitters authored into the stage. Their placement lives in the node chain above them, so
    // this is the accumulated world translation, not the node's own (which is (0,0,0) in both).
    let emitters = caer_assets::nif::read_particle_emitters(&nif.data).unwrap_or_else(|e| {
        log::warn!("preworld scene: {rel} particle emitters unreadable: {e}");
        Vec::new()
    });

    let keys2: Vec<Option<String>> = model
        .parts
        .iter()
        .map(|p| {
            p.texture2.as_deref().and_then(|t| {
                crate::dungeon_mesh::load_model_texture(&index, t, &mut textures, &mut failed)
            })
        })
        .collect();

    let mut batch = crate::dungeon_mesh::batch_from_model(&model, &keys, &keys2, None);
    if batch.vertices.is_empty() {
        log::warn!("preworld scene: {rel} decoded with no vertices");
        eprintln!("preworld scene: {rel} decoded with no vertices");
        return None;
    }
    // A batch with no instances draws nothing. The scene is authored in its own space and shown
    // whole, so it gets exactly one identity placement rather than the per-fixture list a zone has.
    batch.instances.push(crate::terrain::ModelInstance {
        pos: [0.0; 3],
        base_pos: [0.0; 3],
        yaw: 0.0,
        base_yaw: 0.0,
        rot: [0.0, 0.0, 0.0, 1.0],
        base_rot: [0.0, 0.0, 0.0, 1.0],
        scale: 1.0,
        zone_id: 0,
        fixture_id: 0,
    });
    // Percentiles rather than bounds: every scene is a small stage inside a large backdrop dome,
    // so min/max describe the dome and say nothing about where the camera belongs. Non-finite
    // vertices are excluded — each scene carries 20 of them.
    let mut zs: Vec<f32> = batch
        .vertices
        .iter()
        .map(|v| v.pos[2])
        .filter(|z| z.is_finite())
        .collect();
    let mut rs: Vec<f32> = batch
        .vertices
        .iter()
        .filter(|v| v.pos.iter().all(|c| c.is_finite()))
        .map(|v| (v.pos[0] * v.pos[0] + v.pos[1] * v.pos[1]).sqrt())
        .collect();
    zs.sort_by(f32::total_cmp);
    rs.sort_by(f32::total_cmp);
    let pct = |v: &[f32], f: f32| {
        if v.is_empty() {
            0.0
        } else {
            v[((v.len() - 1) as f32 * f) as usize]
        }
    };
    let ground_z = pct(&zs, 0.10);
    let content_radius = pct(&rs, 0.10).max(1.0);

    // The built set: geometry standing clear of the stage floor and inside the dome rather than
    // on it. This is the castle, the keep, the trees — what a camera should be pointed at.
    //
    // Two weaker definitions were tried and measured. Percentile radius from the origin gives
    // 230 / 234 / 638 across the three realms, so anything scaled by it frames Hibernia three
    // times further out than Midgard for no reason. "Everything inside the median radius" is
    // dominated by the ground plane — its radius comes out ~2000 in all three, which pushed the
    // camera back far enough to see the inside of the dome.
    let dome = batch.bound_max[0]
        .max(batch.bound_max[1])
        .max(-batch.bound_min[0])
        .max(-batch.bound_min[1]);
    let lift = ground_z + (batch.bound_max[2] - ground_z) * 0.10;
    let built: Vec<[f32; 3]> = batch
        .vertices
        .iter()
        .map(|v| v.pos)
        .filter(|p| p.iter().all(|c| c.is_finite()))
        .filter(|p| p[2] >= lift)
        .filter(|p| (p[0] * p[0] + p[1] * p[1]).sqrt() <= dome * 0.6)
        .collect();
    let (stage_center, stage_radius, stage_top) = if built.is_empty() {
        (Vec3::new(0.0, 0.0, ground_z), content_radius, ground_z)
    } else {
        let n = built.len() as f32;
        let c = built.iter().fold(Vec3::ZERO, |a, p| a + Vec3::from(*p)) / n;
        let mut r: Vec<f32> = built
            .iter()
            .map(|p| (p[0] * p[0] + p[1] * p[1]).sqrt())
            .collect();
        let mut z: Vec<f32> = built.iter().map(|p| p[2]).collect();
        r.sort_by(f32::total_cmp);
        z.sort_by(f32::total_cmp);
        // 90th percentiles, not maxima: one stray vertex must not set the whole framing.
        (c, pct(&r, 0.90).max(1.0), pct(&z, 0.90))
    };

    // The floor the character stands on: the ground right under the origin, not a percentile over
    // terrain that may be hundreds of units lower somewhere else in the scene.
    let mut near: Vec<f32> = batch
        .vertices
        .iter()
        .map(|v| v.pos)
        .filter(|p| p.iter().all(|c| c.is_finite()))
        .filter(|p| (p[0] * p[0] + p[1] * p[1]).sqrt() <= 200.0)
        .map(|p| p[2])
        .collect();
    near.sort_by(f32::total_cmp);
    let origin_floor_z = if near.is_empty() {
        ground_z
    } else {
        // Median, so a torch bracket overhead does not lift the floor and a pit does not drop it.
        pct(&near, 0.50)
    };

    // Where the character actually stands, from the scene's own collision node.
    //
    // Every char-screen NIF carries a `collisionswitch -> collidee` pair, and the first `collidee`
    // is the only candidate in the file that rests ON the floor: measured against the median
    // surface within 60u of its own XY it sits at -1 (Albion), -0 (Midgard), +2 (Hibernia). The
    // scene origin does not — it floats +45 and +40 above the ground in Albion and Midgard and
    // sinks 7 below it in Hibernia, which is the reported "Hibernians clip through the ground".
    //
    // `flag_snapshot01` is NOT this marker despite the promising name: it hangs off the `banner`
    // node and sits a constant 36 units under the local surface in all three scenes, i.e. it is
    // rigidly attached to the banner prop.
    let subject_anchor = caer_assets::nif::read_skeleton(&nif.data)
        .ok()
        .and_then(|sk| {
            sk.bones
                .iter()
                .find(|b| b.name.eq_ignore_ascii_case("collidee"))
                .map(|b| caer_assets::nif::xform_to_mat4(&b.world_bind))
        })
        .map(|m| Vec3::new(m[3][0], m[3][1], m[3][2]))
        .filter(|v| v.is_finite())
        .unwrap_or_else(|| {
            log::warn!(
                "preworld scene: realm {realm} has no `collidee` anchor — falling back to the \
                 scene origin, which is not floor-accurate"
            );
            Vec3::new(0.0, 0.0, origin_floor_z)
        });

    // CAER_SCENE_BEARING=<deg> overrides the derived aim, so the composition can be searched
    // against a retail screenshot instead of guessed. Midgard's bearing is the clamped value, not
    // a true bisector, which is why it is the one realm whose framing reads wrong.
    if let Some(v) = std::env::var("CAER_SCENE_BEARING")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
    {
        let resolved = keys.iter().filter(|k| k.is_some()).count();
        log::info!("preworld scene: realm {realm} bearing overridden to {v} ({resolved} textured)");
        return Some(PreWorldScene {
            realm,
            bound_min: batch.bound_min,
            bound_max: batch.bound_max,
            ground_z,
            content_radius,
            stage_center,
            stage_radius,
            stage_top,
            origin_floor_z,
            subject_anchor,
            camera_bearing_deg: v,
            models: vec![batch],
            textures,
            missing_textures: Vec::new(),
            dropped_parts: Vec::new(),
            emitters: emitters.clone(),
        });
    }
    let camera_bearing_deg = landmark_bearing(&nif.data, subject_anchor).unwrap_or_else(|| {
        log::warn!(
            "preworld scene: realm {realm} has no landmarks to aim between — falling back to -Y"
        );
        180.0
    });

    let resolved = keys.iter().filter(|k| k.is_some()).count();
    if !failed.is_empty() {
        log::warn!(
            "preworld scene: realm {realm} — {resolved}/{} textured, {} texture misses: {failed:?}",
            keys.len(),
            failed.len()
        );
        eprintln!("preworld scene: realm {realm} missing textures {failed:?} from {rel}");
    }
    log::info!(
        "preworld scene: realm {realm} — {} parts, {} verts, {resolved}/{} textured, {billboards} billboard, {} texture misses: {failed:?}",
        model.parts.len(),
        batch.vertices.len(),
        keys.len(),
        failed.len()
    );
    let mut missing_textures: Vec<String> = failed.into_iter().collect();
    missing_textures.sort();
    Some(PreWorldScene {
        realm,
        bound_min: batch.bound_min,
        bound_max: batch.bound_max,
        ground_z,
        content_radius,
        stage_center,
        stage_radius,
        stage_top,
        origin_floor_z,
        subject_anchor,
        camera_bearing_deg,
        models: vec![batch],
        textures,
        missing_textures,
        dropped_parts,
        emitters,
    })
}

/// The bearing that puts the scene's landmarks either side of the character.
///
/// Retail frames all three realms identically: realm banner on the left, character centred, tower
/// on the right. That composition is the bisector of the two flanking landmarks — aim anywhere
/// else and they crowd onto one side, which is what looking down a fixed -Y did (banner beside
/// the character, tower outside the frustum entirely).
///
/// Landmarks come from two sources because neither covers all three scenes: Albion names its
/// `tower` nodes outright, while Hibernia's is only identifiable by its `H_elftower2_*` textures.
/// Measured spans: Albion -164°..-128°, Midgard -164°..-29°, Hibernia -163°..-117°.
///
/// **PROVENANCE.** The scenes carry no `NiCamera` — verified by dumping every block type in all
/// three (21/32/… types, none a camera or a light). The dolly distance and eye height in
/// [`framing_around_character`] remain ours; this bearing is not, it is read off the geometry.
fn landmark_bearing(nif: &[u8], anchor: Vec3) -> Option<f32> {
    let is_landmark = |s: &str| {
        let s = s.to_ascii_lowercase();
        s.contains("banner") || s.contains("tower") || s.contains("portal") || s.contains("elfshop")
    };
    let bearing = |x: f32, y: f32| (x - anchor.x).atan2(y - anchor.y).to_degrees();
    let mut angles: Vec<f32> = Vec::new();

    if let Ok(sk) = caer_assets::nif::read_skeleton(nif) {
        for b in &sk.bones {
            let n = b.name.to_ascii_lowercase();
            if n.contains("tower") || n.contains("portal") {
                let w = caer_assets::nif::xform_to_mat4(&b.world_bind);
                if w[3][0].is_finite() && w[3][1].is_finite() {
                    angles.push(bearing(w[3][0], w[3][1]));
                }
            }
        }
    }
    if let Ok(model) = caer_assets::nif::read_model(nif) {
        for p in &model.parts {
            // Small parts are trim and glow sprites; they sit on top of the thing that matters
            // and would only add noise to the span.
            if p.positions.len() < 100 || !p.texture.as_deref().is_some_and(is_landmark) {
                continue;
            }
            let n = p.positions.len() as f32;
            let (cx, cy) = p
                .positions
                .iter()
                .fold((0.0, 0.0), |a, v| (a.0 + v[0] / n, a.1 + v[1] / n));
            if cx.is_finite() && cy.is_finite() {
                angles.push(bearing(cx, cy));
            }
        }
    }
    if angles.len() < 2 {
        return None;
    }
    angles.sort_by(f32::total_cmp);

    // Never aim further from the nearest landmark than half the horizontal frame.
    //
    // A plain bisector of ALL matches lets a distant prop dominate: Midgard's two gates sit 119°
    // from its banner, which pulled the aim to -97° and swung the whole composition off — the SI
    // portal ended up behind the character instead of to their left. Two things 119° apart cannot
    // share a ~92° frame, so bisecting them is meaningless.
    //
    // Clamping the swing to half the horizontal FOV reproduces the two compositions already
    // verified against retail — Albion -146° (36° span) and Hibernia -140° (46° span) — and pulls
    // Midgard to -141°, which puts its portal and banner left and the gates out of frame.
    // 60, measured against Eden. The old 23 was set to stop a distant prop dominating, but it
    // clamped Midgard's bisector from -97 to -141 — a 44 degree error, and Midgard is the one
    // realm whose framing reads wrong. Sweeping the bearing and matching the banner's screen
    // position against a retail capture puts the answer at about -104 (banner at 0.242 of screen
    // width vs Eden's 0.248; the clamped -141 sat at 0.410).
    //
    // Albion's half-span is 18 degrees and Hibernia's is 23, so both stay under this limit and
    // their bearings are unchanged at -146 and -140. Only Midgard moves.
    const MAX_SWING_DEG: f32 = 60.0;
    let near = angles[0];
    let span = angles[angles.len() - 1] - near;
    Some(near + (span * 0.5).min(MAX_SWING_DEG))
}

/// Where the camera sits to frame the scene.
///
/// **PROVENANCE GAP.** The scenes carry no `NiCamera` and no light blocks — checked all three — so
/// the viewpoint lives in the original client's code and is not recoverable from the asset. This
/// derives one from the geometry instead of inventing fixed numbers, and it is **not** claimed to
/// be retail's framing.
///
/// What it derives: the stage set (see [`PreWorldScene::stage_center`]) is the geometry that is
/// not the backdrop dome. Stand far enough back on -Y that a sphere around that set fills the
/// frame, at roughly its own height, and look at it. The distance comes from the renderer's own
/// vertical FOV, so the framing follows the camera rather than a tuned constant.
///
/// Two things it must get right, both learned the hard way. It must sit *inside* the backdrop dome
/// (radius ~2000-2400 in every realm) — outside, the screen shows the dome's back face. And it
/// must sit above the stage floor pointing at the content — the first version stood a fraction of
/// the stage radius off the ground and looked across the origin, which put the camera in the dirt
/// with a rock face filling the shot.
#[must_use]
pub fn framing(scene: &PreWorldScene, aspect: f32) -> (Vec3, Vec3) {
    framing_with_view(scene, aspect, 1.0, 0.0)
}

/// Normal stage/preview composition with a local dolly and tilt layered on top.
///
/// The default (`dolly_scale = 1`, `tilt = 0`) is byte-for-byte the full-body character-select
/// framing.  [`framing_backdrop`] is the only caller that owns the actual realm scene; a
/// non-default result is an avatar inspection lens, never permission to move that backdrop.
#[must_use]
pub fn framing_with_view(
    scene: &PreWorldScene,
    aspect: f32,
    dolly_scale: f32,
    tilt: f32,
) -> (Vec3, Vec3) {
    let (eye, focus, _) =
        framing_around_character_with_dolly(scene, CHARACTER_HEIGHT, None, aspect, dolly_scale);
    apply_preview_tilt(eye, focus, tilt)
}

/// Close avatar framing used by Character Customize and its statistics overlay.
///
/// This is deliberately *not* the realm-stage lens.  The caller renders the backdrop through
/// [`framing_backdrop`] and binds this projection only while drawing the avatar; otherwise the
/// close camera enters Midgard's backdrop dome and replaces its mountain sky with black.  Aim at
/// the upper face so the source opening frame centres eyes and shoulders rather than the chest.
/// Race model scaling is left to avatar rendering; this screen-level profile stays stable across
/// identities.
#[must_use]
pub fn framing_face_with_view(
    scene: &PreWorldScene,
    aspect: f32,
    dolly_scale: f32,
    tilt: f32,
) -> (Vec3, Vec3) {
    let (eye, focus, _) = framing_around_character_with_dolly(
        scene,
        CHARACTER_HEIGHT,
        Some(CHARACTER_HEIGHT * 0.95),
        aspect,
        dolly_scale,
    );
    let (eye, focus) = apply_preview_tilt(eye, focus, tilt);
    // The close customizer composition deliberately leaves room for the Facial Features panel.
    // Translate the complete lens carriage to camera-right while preserving its aim direction;
    // the avatar and stage therefore land left of centre, as in the retail captures, rather than
    // being centred beneath the right-side controls.
    let theta = effective_bearing_deg(scene).to_radians();
    let right = Vec3::new(theta.cos(), -theta.sin(), 0.0);
    const FACE_FRAME_RIGHT_OFFSET: f32 = 5.0;
    (
        eye + right * FACE_FRAME_RIGHT_OFFSET,
        focus + right * FACE_FRAME_RIGHT_OFFSET,
    )
}

/// The immutable realm-stage composition behind every character screen.
///
/// Source captures show the background does not dolly or orbit with the customizer controls: the
/// character gets the close inspection lens while the dome, terrain, and landmark arrangement
/// remain fixed.  Keeping that invariant in one named helper prevents a future face-camera call
/// from accidentally being handed to the stage draw again.
#[must_use]
pub fn framing_backdrop(scene: &PreWorldScene, aspect: f32) -> (Vec3, Vec3) {
    framing_with_view(scene, aspect, 1.0, 0.0)
}

/// Choose the avatar lens for one local preview controller.
///
/// The realm backdrop is separately owned by [`framing_backdrop`].
#[must_use]
pub fn framing_for_preview(
    scene: &PreWorldScene,
    aspect: f32,
    preview: crate::preworld_camera::CustomizerCamera,
) -> (Vec3, Vec3) {
    if preview.uses_face_frame() {
        framing_face_with_view(scene, aspect, preview.dolly(), preview.tilt())
    } else {
        framing_with_view(scene, aspect, preview.dolly(), preview.tilt())
    }
}

fn apply_preview_tilt(eye: Vec3, focus: Vec3, tilt: f32) -> (Vec3, Vec3) {
    // The source calls these controls Up/Down, but ships no camera handler to recover their
    // exact mechanics.  Treat them as a bounded vertical inspection offset, moving eye and aim
    // together.  A pure angular turn at a close face zoom points through the top of the backdrop
    // dome (a black ceiling), while a parallel carriage keeps the same realm composition and
    // makes the intended facial inspection usable.
    let tilt = if tilt.is_finite() {
        tilt.clamp(
            -crate::preworld_camera::CustomizerCamera::MAX_TILT,
            crate::preworld_camera::CustomizerCamera::MAX_TILT,
        )
    } else {
        0.0
    };
    let lift = Vec3::Z * (tilt * CHARACTER_HEIGHT);
    (eye + lift, focus + lift)
}

/// Stage bearing after any live tuning override.
///
/// The `CAER_SCENE_BEARING` env override is applied at scene LOAD, baking into
/// `camera_bearing_deg`, so it cannot be changed without reloading. The live override has to be
/// consulted here instead, every frame, which is what makes it adjustable while the client runs.
///
/// Public because the AVATAR needs it too. The body turns to meet the lens, so it has to read the
/// same bearing the lens does — reading `camera_bearing_deg` instead leaves the figure facing the
/// shipped bearing while a nudged camera orbits away from it.
#[must_use]
pub fn effective_bearing_deg(scene: &PreWorldScene) -> f32 {
    crate::preworld_camera_tune::tuning(scene.realm)
        .bearing
        .or_else(|| crate::preworld_camera_tune::shipped(scene.realm).bearing)
        .unwrap_or(scene.camera_bearing_deg)
}

/// Shipped pull-back multiplier.
///
/// **Solved, not derived.** The framing below now sizes the shot against the real lens, which is
/// `1/aspect` further out than the vertical-60° reading it replaced. That is the correct formula,
/// but the composition it produces is not the one checked by eye against the Eden bank — this
/// multiplier holds that checked composition while the maths underneath it is honest. It is the
/// same class of number as bearing, eye and focus: a provenance gap settled against a reference,
/// which is exactly what `CAER_SCENE_DOLLY` exists to carry.
///
/// Press Ctrl+Alt+0 then PageUp/PageDown to re-solve it; Ctrl+Alt+P prints the settled value.
pub const SHIPPED_DOLLY: f32 = 0.5625;

/// Camera, focus, and where the character stands — one decision, because they are one composition.
///
/// The character stands at the scene **origin**. That is not a guess: every realm's render root
/// (`visible`) sits within a dozen units of it, Albion's two torches straddle it symmetrically at
/// (-158, 97) and (161, 97), and the backdrop dome is centred on it. The scenes are stages built
/// around a figure at 0,0.
///
/// So the camera is composed around that figure — in front of it on -Y, at chest height, far
/// enough back to fit a [`CHARACTER_HEIGHT`] body with headroom. Three earlier attempts derived a
/// camera from vertex statistics alone and each one framed terrain instead of the stage, because
/// the statistics describe a 2500-unit backdrop dome and the subject is 220 units tall.
///
/// **PROVENANCE GAP** remains for the exact numbers: no `NiCamera` exists in any of the three
/// scenes, so the dolly distance and eye height below are chosen to frame the figure, not
/// recovered from the asset.
#[must_use]
/// `aim_height` is where the lens sits and looks, in units above the anchor — pass the subject's
/// actual head height. `None` keeps the old chest-height fractions.
///
/// Height and distance are separate parameters on purpose. `height` still drives the dolly alone,
/// so changing where the camera looks does not silently move it closer (B1 is a live row about the
/// distance and must not be absorbed by a fix aimed at the tilt).
///
/// `aspect` is the viewport's width/height. It is an input rather than a constant because the
/// stage lens is pinned horizontally, so the vertical angle — and therefore the pull-back that
/// fits a body — is different on a 4:3 screen than on a 16:9 one.
pub fn framing_around_character(
    scene: &PreWorldScene,
    height: f32,
    aim_height: Option<f32>,
    aspect: f32,
) -> (Vec3, Vec3, Vec3) {
    framing_around_character_with_dolly(scene, height, aim_height, aspect, 1.0)
}

/// [`framing_around_character`] with a bounded local preview dolly multiplier.
///
/// This remains separate from realm camera tuning: the latter solves the shipped backdrop once,
/// while this one answers a player's temporary customizer zoom gesture.
#[must_use]
pub fn framing_around_character_with_dolly(
    scene: &PreWorldScene,
    height: f32,
    aim_height: Option<f32>,
    aspect: f32,
    dolly_scale: f32,
) -> (Vec3, Vec3, Vec3) {
    let h = if height.is_finite() && height > 1.0 {
        height
    } else {
        CHARACTER_HEIGHT
    };
    // Where the figure stands. `SubjectX` slides them across the stage along the camera's own
    // right vector, and the camera follows because it is composed around this point — so the
    // scene moves behind the figure rather than the figure moving within the frame.
    let feet = {
        let base = scene.subject_anchor;
        // Precedence everywhere in this function: a LIVE nudge, then the environment, then the
        // realm's shipped composition. The env layer sits in the middle because it is a solving
        // aid — it must beat what shipped, and lose to what the human is moving right now.
        let solved = |knob: crate::preworld_camera_tune::Knob, var: &str| {
            crate::preworld_camera_tune::tuning(scene.realm)
                .get(knob)
                .or_else(|| {
                    std::env::var(var)
                        .ok()
                        .and_then(|v| v.parse::<f32>().ok())
                        .filter(|v| v.is_finite())
                })
                .or_else(|| crate::preworld_camera_tune::shipped(scene.realm).get(knob))
                .unwrap_or(0.0)
        };
        let across = solved(
            crate::preworld_camera_tune::Knob::SubjectX,
            "CAER_SCENE_SUBJECT_X",
        );
        let depth = solved(
            crate::preworld_camera_tune::Knob::SubjectY,
            "CAER_SCENE_SUBJECT_Y",
        );
        if across.abs() > f32::EPSILON || depth.abs() > f32::EPSILON {
            let th = effective_bearing_deg(scene).to_radians();
            // View runs from the eye toward the anchor along (sin, cos) of the bearing; camera
            // right is that rotated -90 deg about Z.
            let (vx, vy) = (th.sin(), th.cos());
            let (rx, ry) = (vy, -vx);
            Vec3::new(
                base.x + rx * across + vx * depth,
                base.y + ry * across + vy * depth,
                base.z,
            )
        } else {
            base
        }
    };
    // Frame a body plus half again, in the lens the stage is actually rendered through.
    //
    // That lens is a HORIZONTAL 60° (`PREWORLD_FOV_X`), so the vertical angle depends on the
    // viewport: 35.98° at 16:9, 45.9° at 4:3. This line used to read the 60° as vertical, which
    // put the camera `tan(30°)/tan(17.99°)` = 1.778x too close at 16:9 — the aspect ratio exactly,
    // the same signature as the lens defect itself — so the headroom it asks for was never there.
    // A NaN or zero viewport would otherwise return a NaN distance and park the camera inside the
    // subject, which renders as a black screen — the failure this module already exists to fix.
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect.clamp(0.5, 4.0)
    } else {
        16.0 / 9.0
    };
    let half_fov = fov_y_for_horizontal(PREWORLD_FOV_X, aspect) * 0.5;
    // CAER_SCENE_DOLLY scales the pull-back so the camera DISTANCE can be solved against a retail
    // capture, the same way CAER_SCENE_BEARING solves the aim. Matching one landmark fixes yaw but
    // cannot fix distance; the two together can.
    let dolly = crate::preworld_camera_tune::tuning(scene.realm)
        .dolly
        .or_else(|| {
            std::env::var("CAER_SCENE_DOLLY")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| v.is_finite() && *v > 0.05)
        })
        .or_else(|| crate::preworld_camera_tune::shipped(scene.realm).dolly)
        .unwrap_or(SHIPPED_DOLLY);
    let preview_dolly = if dolly_scale.is_finite() {
        dolly_scale.clamp(
            crate::preworld_camera::CustomizerCamera::MIN_DOLLY,
            crate::preworld_camera::CustomizerCamera::MAX_DOLLY,
        )
    } else {
        1.0
    };
    let back = h * 1.5 / half_fov.tan() * dolly * preview_dolly;
    // Dolly out on the side OPPOSITE the authored backdrop, so the landmarks end up behind the
    // character instead of behind the lens. Measured bearing of every landmark from the anchor:
    // Albion's banner -164°, Hibernia's portal cluster -162° and its tower -117°, Midgard's
    // banners -61° and -29°. Sitting at -Y (bearing 180°) put the camera among them looking away,
    // which is why the Hibernian tower and portal never appeared.
    let theta = effective_bearing_deg(scene).to_radians();
    let (dx, dy) = (theta.sin(), theta.cos());
    // Eye and aim heights as fractions of body height. Same solving pattern as CAER_SCENE_DOLLY
    // and CAER_SCENE_BEARING: these are a PROVENANCE GAP, so they have to be solvable against a
    // capture rather than argued about.
    // Precedence: a live tuning override, then the environment, then the shipped default. The
    // live layer exists because these four numbers are a provenance gap that has to be solved by
    // eye against a reference, and a rebuild per candidate value made that the slow step.
    let tune = crate::preworld_camera_tune::tuning(scene.realm);
    let frac = |var: &str| {
        std::env::var(var)
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
    };
    // `aim_height` changes vertical composition ONLY. `height` still supplies the dolly, so the
    // camera does not move closer or further; B1 is a separate open row about that distance.
    //
    // Without an anchor the eye falls back to a fraction of `height`, which is the unscaled
    // `model_height`, so the error grows with instance scale (ledger E13). Whether retail frames
    // level is not established, which is why the solver overrides below outrank the anchor.
    // An explicit `aim_height` is the face-preview contract, not a missing stage value.  It must
    // outrank the realm's settled *stage* eye/focus tuning: carrying Midgard's stage eye (0.68h)
    // into a 0.95h face focus made the avatar look up its own nose.  Without an explicit aim we
    // retain the ordinary live/env/shipped stage precedence.
    let height_or = |var: &str, live: Option<f32>, fallback: f32| {
        aim_height.unwrap_or_else(|| live.or_else(|| frac(var)).map_or(fallback, |f| h * f))
    };
    let ship = crate::preworld_camera_tune::shipped(scene.realm);
    let eye_z = height_or(
        "CAER_SCENE_EYE",
        tune.eye.or(ship.eye),
        aim_height.unwrap_or(h * 0.62),
    );
    let focus_z = height_or(
        "CAER_SCENE_FOCUS",
        tune.focus.or(ship.focus),
        aim_height.unwrap_or(h * 0.55),
    );
    // A straight-up pedestal on the whole camera, in world units. Eye and focus take the SAME
    // offset, so the view direction is untouched and only the lens height changes — the one move
    // the fraction knobs above cannot make, since raising `eye` alone tilts the shot.
    //
    // It exists because the stage lens is fixed while the figure is not: `CHARACTER_HEIGHT` is 70
    // for every race, so a Firbolg at display scale 1.35 has their head near 94 and the lens looks
    // steeply up at them. Whether that upward angle is the "head tilt" report is answerable by
    // raising the lens to their eye level and looking — which is what this knob is for.
    let lift = crate::preworld_camera_tune::tuning(scene.realm)
        .lift
        .or_else(|| {
            std::env::var("CAER_SCENE_LIFT")
                .ok()
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| v.is_finite())
        })
        .or_else(|| crate::preworld_camera_tune::shipped(scene.realm).lift)
        .unwrap_or(0.0);
    let eye = Vec3::new(
        feet.x - dx * back,
        feet.y - dy * back,
        feet.z + eye_z + lift,
    );
    let focus = Vec3::new(feet.x, feet.y, feet.z + focus_z + lift);
    (eye, focus, feet)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end material hand-off for the Hibernia backdrop: parsing the Decal 0 is not enough
    /// if scene loading drops its texture or batching turns it back into a normal vertex-mask
    /// layer. The negative blend selector is the model shader's explicit `OverlayAlpha` path.
    #[test]
    fn hibernia_panorama_reaches_the_scene_batch_as_an_alpha_overlay() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let scene = load(std::path::Path::new(&root), 3).expect("Hibernia scene loads");
        assert!(
            scene.textures.contains_key("hib_panarama01.dds"),
            "the named panorama must resolve, not silently fall back to a white page"
        );
        assert!(
            scene
                .models
                .iter()
                .flat_map(|m| &m.vertices)
                .any(|v| v.blend < 0.0),
            "the Decal 0 dome must reach the model shader's alpha-overlay path"
        );
    }

    /// The cloud dome's base map has a texture-transform controller, but its fixed-map Decal 0
    /// panorama does not. Sharing one mutable UV stream made the distant forest slide with the
    /// clouds. The model batch must retain a fixed coordinate for the overlay while its base UV
    /// actually moves — otherwise this test could go green by disabling the cloud animation.
    #[test]
    fn hibernia_panorama_stays_pinned_while_its_cloud_base_animates() {
        let Some(root) = std::env::var_os("CAER_CLIENT") else {
            eprintln!("skip: CAER_CLIENT unset");
            return;
        };
        let mut scene = load(std::path::Path::new(&root), 3).expect("Hibernia scene loads");
        let (model_index, vertex_index) = scene
            .models
            .iter()
            .enumerate()
            .find_map(|(mi, batch)| {
                batch
                    .vertices
                    .iter()
                    .position(|v| v.blend < 0.0)
                    .map(|vi| (mi, vi))
            })
            .expect("Hibernia's Decal 0 backdrop reaches a vertex");
        let batch = &mut scene.models[model_index];
        let initial_uv = batch.vertices[vertex_index].uv;
        let initial_overlay_uv = batch.vertices[vertex_index].overlay_uv;
        let duration = batch
            .uv_anims
            .iter()
            .find(|binding| {
                let first = binding.base_vertex as usize;
                (first..first + binding.rest_uv.len()).contains(&vertex_index)
            })
            .map(|binding| binding.anim.duration())
            .expect("the cloud-dome base has the authored UV controller");
        assert!(duration > 0.0, "the source controller must really animate");

        let mut base_moved = false;
        for fraction in [0.25, 0.5, 0.75] {
            batch.animate(duration * fraction);
            let v = batch.vertices[vertex_index];
            base_moved |= v.uv != initial_uv;
            assert_eq!(
                v.overlay_uv, initial_overlay_uv,
                "the panorama may not inherit the cloud base's UV transform at t={fraction}"
            );
        }
        assert!(
            base_moved,
            "red control: freezing both layers is not a repair"
        );
    }

    #[test]
    fn archive_per_realm() {
        assert!(scene_archive(1).ends_with("Alb.npk"));
        assert!(scene_archive(2).ends_with("Mid.npk"));
        assert!(scene_archive(3).ends_with("Hib.npk"));
        // An unknown realm must still name a real archive rather than an empty path.
        assert!(scene_archive(9).ends_with(".npk"));
    }

    /// Framing must look *at* the scene: the eye sits outside the bounds on -Y and above centre,
    /// and the focus is the centre. A camera inside the geometry renders as a black screen, which
    /// is indistinguishable from the bug this whole module exists to fix.
    #[test]
    fn framing_places_the_eye_outside_the_bounds() {
        let scene = PreWorldScene {
            realm: 1,
            models: Vec::new(),
            textures: HashMap::new(),
            bound_min: [-2800.0, -2577.0, -758.0],
            bound_max: [2431.0, 2653.0, 1187.0],
            ground_z: -544.0,
            content_radius: 230.0,
            stage_center: Vec3::new(0.0, 0.0, -400.0),
            stage_radius: 400.0,
            stage_top: -200.0,
            origin_floor_z: -500.0,
            subject_anchor: Vec3::new(0.0, 0.0, -500.0),
            camera_bearing_deg: 180.0,
            missing_textures: Vec::new(),
            dropped_parts: Vec::new(),
            emitters: Vec::new(),
        };
        let (eye, focus) = framing(&scene, 16.0 / 9.0);
        let dome = scene.bound_max[0].max(scene.bound_max[1]);
        let eye_radius = (eye.x * eye.x + eye.y * eye.y).sqrt();
        assert!(
            eye_radius < dome,
            "the camera must sit INSIDE the backdrop dome (r={eye_radius:.0}, dome={dome:.0}); \
             outside it the screen shows the dome's back face"
        );
        assert!(eye.z > scene.ground_z, "eye must be above the stage floor");
        let look = Vec3::new(focus.x - eye.x, focus.y - eye.y, 0.0);
        let to_stage = Vec3::new(
            scene.stage_center.x - eye.x,
            scene.stage_center.y - eye.y,
            0.0,
        );
        assert!(
            look.normalize().dot(to_stage.normalize()) > 0.9,
            "must look at the stage content, not away from it"
        );
    }

    /// Realm 0 on purpose. Realms 1..3 carry a settled composition solved by eye, so using one as
    /// a baseline makes a geometry test depend on a number a human is still free to move. Realm 0
    /// is not a stage, carries no shipped composition, and therefore isolates the maths.
    fn bare_stage() -> PreWorldScene {
        PreWorldScene {
            realm: 0,
            models: Vec::new(),
            textures: HashMap::new(),
            bound_min: [-2800.0, -2577.0, -758.0],
            bound_max: [2431.0, 2653.0, 1187.0],
            ground_z: -544.0,
            content_radius: 230.0,
            stage_center: Vec3::new(0.0, 0.0, -400.0),
            stage_radius: 400.0,
            stage_top: -200.0,
            origin_floor_z: -500.0,
            subject_anchor: Vec3::new(0.0, 0.0, -500.0),
            camera_bearing_deg: 180.0,
            missing_textures: Vec::new(),
            dropped_parts: Vec::new(),
            emitters: Vec::new(),
        }
    }

    /// The pull-back has to be solved against the lens the stage is rendered through — a
    /// **horizontal** 60°, so 35.98° vertically at 16:9. Seen red against the old line, which read
    /// the 60° as vertical: it returned 2.598 body-heights instead of 4.617, the camera sitting
    /// 1.778x too close, and the figure overflowing the headroom the framing asks for.
    #[test]
    fn the_pull_back_is_solved_against_the_stage_lens_not_a_vertical_sixty() {
        let scene = bare_stage();
        let h = CHARACTER_HEIGHT;
        let (eye, _, feet) = framing_around_character(&scene, h, None, 16.0 / 9.0);
        let back = ((eye.x - feet.x).powi(2) + (eye.y - feet.y).powi(2)).sqrt();

        // The solved multiplier is a separate decision from the geometry; divide it back out so
        // this pins the FORMULA rather than the composition. Realm 0 has no shipped dolly, so the
        // generic constant is the one in play.
        let geometric = back / SHIPPED_DOLLY;
        let want = h * 1.5 / (fov_y_for_horizontal(PREWORLD_FOV_X, 16.0 / 9.0) * 0.5).tan();
        assert!(
            (geometric - want).abs() < 0.5,
            "pull-back should be {want:.1} at 16:9, got {geometric:.1}"
        );

        // KNOWN-BAD CONTROL: reading the 60° as a vertical angle. The gap is the aspect ratio.
        let bad = h * 1.5 / (60.0_f32.to_radians() * 0.5).tan();
        assert!(
            (geometric / bad - 16.0 / 9.0).abs() < 0.01,
            "the defect this replaces sat 1.778x too close; ratio to it is {:.3}",
            geometric / bad
        );
    }

    /// The customizer's local camera must be an overlay on the shared realm composition, not a
    /// second hard-coded stage camera.  This is the red/green guard for the old temptation to
    /// make the source close-up screenshot the only static customizer framing.
    #[test]
    fn preview_dolly_and_tilt_leave_the_default_realm_frame_unchanged() {
        let scene = bare_stage();
        let aspect = 16.0 / 9.0;
        let base = framing(&scene, aspect);
        assert_eq!(base, framing_with_view(&scene, aspect, 1.0, 0.0));

        let close = framing_with_view(&scene, aspect, 0.25, 0.0);
        let base_distance = (base.1 - base.0).length();
        let close_distance = (close.1 - close.0).length();
        assert!(
            close_distance < base_distance,
            "zoom-in must reduce view distance: {close_distance:.2} < {base_distance:.2}"
        );

        let tilted = framing_with_view(&scene, aspect, 1.0, 0.2);
        assert!(
            (tilted.0.z - base.0.z - CHARACTER_HEIGHT * 0.2).abs() < 1e-4,
            "up/down must move the inspection carriage by its bounded local offset"
        );
        assert!(
            (tilted.1 - base.1).length() > 0.01,
            "known-bad zero-tilt path must not masquerade as the source up/down control"
        );
    }

    #[test]
    fn face_preview_aims_above_the_full_body_stage_focus() {
        let scene = bare_stage();
        let aspect = 16.0 / 9.0;
        let full = framing_for_preview(
            &scene,
            aspect,
            crate::preworld_camera::CustomizerCamera::default(),
        );
        let face = framing_for_preview(
            &scene,
            aspect,
            crate::preworld_camera::CustomizerCamera::face_default(),
        );
        assert!(
            face.1.z > full.1.z,
            "face frame must centre above the full-body chest aim"
        );
        assert!(
            (face.1 - face.0).length() < (full.1 - full.0).length(),
            "face frame must be closer than character-select composition"
        );
        let unoffset = framing_around_character_with_dolly(
            &scene,
            CHARACTER_HEIGHT,
            Some(CHARACTER_HEIGHT * 0.95),
            aspect,
            0.155,
        );
        let theta = effective_bearing_deg(&scene).to_radians();
        let right = Vec3::new(theta.cos(), -theta.sin(), 0.0);
        assert!(
            ((face.0 - unoffset.0).dot(right) - 5.0).abs() < 1e-4
                && ((face.1 - unoffset.1).dot(right) - 5.0).abs() < 1e-4,
            "the face frame must reserve the right panel by moving the whole lens carriage"
        );
    }

    /// The lens is pinned horizontally, so a 4:3 screen sees more vertically at a given distance
    /// and the camera has to come IN to keep the same framing. A hardcoded vertical angle — or a
    /// pull-back solved once at 16:9 — cannot express that.
    #[test]
    fn a_narrower_screen_pulls_the_camera_in() {
        let scene = bare_stage();
        let h = CHARACTER_HEIGHT;
        let dist = |aspect: f32| {
            let (eye, _, feet) = framing_around_character(&scene, h, None, aspect);
            ((eye.x - feet.x).powi(2) + (eye.y - feet.y).powi(2)).sqrt()
        };
        let wide = dist(16.0 / 9.0);
        let narrow = dist(4.0 / 3.0);
        assert!(
            narrow < wide,
            "4:3 has the taller vertical FOV, so it needs LESS pull-back: {narrow:.1} vs {wide:.1}"
        );
        // A degenerate viewport must not park the camera inside the subject.
        for aspect in [0.0, -1.0, 100.0, f32::NAN] {
            let d = dist(aspect);
            assert!(
                d.is_finite() && d > h,
                "aspect {aspect} produced a nonsense pull-back: {d}"
            );
        }
    }

    /// Each real stage ships the composition Matt settled by eye, and the renderer must actually
    /// use it. Solving these costs a human sitting in front of the client, so a regression that
    /// quietly reverts one to a generic default is expensive in exactly the way a test is cheap.
    #[test]
    fn every_stage_ships_the_composition_that_was_solved_for_it() {
        use crate::preworld_camera_tune as tune;
        let h = CHARACTER_HEIGHT;
        for realm in [1u8, 2, 3] {
            tune::reset(realm);
            let ship = tune::shipped(realm);
            let dolly = ship.dolly.expect("every stage has a solved dolly");

            let mut scene = bare_stage();
            scene.realm = realm;
            let (eye, _, feet) = framing_around_character(&scene, h, None, 16.0 / 9.0);

            // Distance: the solved dolly, not the generic one.
            let back = ((eye.x - feet.x).powi(2) + (eye.y - feet.y).powi(2)).sqrt();
            let want = h * 1.5 / (fov_y_for_horizontal(PREWORLD_FOV_X, 16.0 / 9.0) * 0.5).tan();
            assert!(
                (back - want * dolly).abs() < 0.5,
                "realm {realm}: pull-back {back:.1}, expected {:.1} at dolly {dolly}",
                want * dolly
            );
            assert!(
                (back - want * SHIPPED_DOLLY).abs() > 0.5,
                "realm {realm} fell back to the generic dolly — its solved value is not reaching \
                 the framing"
            );

            // Aim: the solved bearing, not the one derived from the geometry.
            let bearing = ship.bearing.expect("every stage has a solved bearing");
            assert!(
                (effective_bearing_deg(&scene) - bearing).abs() < 1e-3,
                "realm {realm}: bearing is not the solved one"
            );

            // Eye height, where one was solved.
            if let Some(eye_frac) = ship.eye {
                assert!(
                    ((eye.z - feet.z) / h - eye_frac).abs() < 1e-3,
                    "realm {realm}: eye height is not the solved fraction"
                );
            }
        }
    }

    /// The body turns to meet the lens, so whatever the avatar reads for "which way is the lens"
    /// has to be the angle the lens actually used. Realm 2 because the override store is global
    /// and keyed by realm — realm 1's tests must not see this.
    #[test]
    fn a_nudged_bearing_is_visible_to_whatever_turns_the_body() {
        use crate::preworld_camera_tune as tune;
        // Realm 0: no shipped bearing, so `effective_bearing_deg` starts at the derived one and
        // this test measures the override path alone.
        let scene = bare_stage();
        tune::reset(0);

        let shipped = effective_bearing_deg(&scene);
        assert!((shipped - scene.camera_bearing_deg).abs() < 1e-6);

        // Negative so the 180° shipped bearing does not wrap; the wrap is its own behaviour and
        // testing it here would only obscure what this test is about.
        tune::nudge(0, tune::Knob::Bearing, -37.0, scene.camera_bearing_deg);
        let nudged = effective_bearing_deg(&scene);
        assert!(
            (nudged - (scene.camera_bearing_deg - 37.0)).abs() < 1e-3,
            "the effective bearing must follow the nudge, got {nudged:.2}"
        );

        // The camera really does orbit to it — otherwise this accessor would be describing
        // something the lens does not do.
        let (eye, _, feet) = framing_around_character(&scene, CHARACTER_HEIGHT, None, 16.0 / 9.0);
        let orbit = (feet.x - eye.x).atan2(feet.y - eye.y).to_degrees();
        assert!(
            (orbit - nudged).abs() < 1e-2,
            "camera sits at {orbit:.2} but the accessor says {nudged:.2}"
        );

        // KNOWN-BAD CONTROL: the baked field the avatar used to read. It does NOT move, which is
        // exactly why the body kept facing the shipped bearing while the camera swung away.
        assert!(
            (scene.camera_bearing_deg - nudged).abs() > 30.0,
            "the baked bearing must be the stale one this replaces"
        );
        tune::reset(0);
    }

    /// `SubjectY` walks the figure through the backdrop without resizing them: the camera follows,
    /// so the pull-back is unchanged and only their place among the props moves.
    #[test]
    fn walking_the_figure_in_depth_keeps_the_camera_the_same_distance_away() {
        use crate::preworld_camera_tune as tune;
        // Realm 3, not 0: the override store is global and keyed by realm, and the bearing test
        // above owns realm 0. Two tests sharing a slot reset each other's nudge under `cargo
        // test`'s parallelism, which reads as a flake rather than as the collision it is.
        //
        // Realm 3 ships a subject offset, so the baseline is that shipped value and the nudge is
        // seeded from it — otherwise "how far did the figure walk" measures the gap between the
        // shipped composition and the nudge instead of the nudge itself.
        let mut scene = bare_stage();
        scene.realm = 3;
        tune::reset(3);
        let baseline = tune::shipped(3).subject_y.unwrap_or(0.0);

        let (eye0, _, feet0) = framing_around_character(&scene, CHARACTER_HEIGHT, None, 16.0 / 9.0);
        let back0 = ((eye0.x - feet0.x).powi(2) + (eye0.y - feet0.y).powi(2)).sqrt();

        tune::nudge(3, tune::Knob::SubjectY, 8.0, baseline);
        let depth = tune::tuning(3).subject_y.expect("subject_y set") - baseline;
        let (eye1, _, feet1) = framing_around_character(&scene, CHARACTER_HEIGHT, None, 16.0 / 9.0);
        let back1 = ((eye1.x - feet1.x).powi(2) + (eye1.y - feet1.y).powi(2)).sqrt();

        assert!(
            (back1 - back0).abs() < 1e-2,
            "depth must not resize the figure: {back0:.1} -> {back1:.1}"
        );
        let moved = ((feet1.x - feet0.x).powi(2) + (feet1.y - feet0.y).powi(2)).sqrt();
        assert!(
            (moved - depth.abs()).abs() < 1e-2,
            "the figure should have walked {depth:.1}, walked {moved:.1}"
        );
        // Along the VIEW axis, not across it: walking away must increase the gap to the eye's
        // own position before the camera followed.
        let th = effective_bearing_deg(&scene).to_radians();
        let along = (feet1.x - feet0.x) * th.sin() + (feet1.y - feet0.y) * th.cos();
        assert!(
            (along - depth).abs() < 1e-2,
            "depth must move along the view vector, got {along:.1} of {depth:.1}"
        );
        tune::reset(3);
    }
}
