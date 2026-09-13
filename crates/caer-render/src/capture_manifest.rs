//! What a capture is a capture *of*.
//!
//! Ground rule 7 of the parity audit: a parity claim needs the same screen, realm, race, gender,
//! selection, pose, resolution and display mode on both sides. That rule was written because two
//! captures were once compared that were not of the same thing, and nothing in the files said so —
//! a PNG records pixels and forgets everything about how it was produced.
//!
//! Every `--screenshot` now drops a sibling `.manifest` beside the image, and [`mismatches`]
//! refuses a comparison whose manifests disagree on anything that changes what is drawn. A capture
//! with no manifest is not comparable either: unknown state is not matching state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The recorded state of one capture, as ordered `key = value` lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureManifest {
    fields: BTreeMap<String, String>,
}

/// Fields that must agree before two captures may be compared.
///
/// Anything that changes what is drawn belongs here. `commit` and `captured_at` deliberately do
/// not: comparing our render against a reference is the whole point, and they will always differ.
pub const COMPARABLE: &[&str] = &[
    "screen",
    "realm",
    "race",
    "gender",
    "width",
    "height",
    "anim_time",
    "class",
    "appearance",
    "surface_width",
    "surface_height",
    "scale_factor",
    "display_mode",
    "player_position",
    "heading",
    "camera_orbit",
    "camera_pitch",
    "pose_elapsed_ms",
    "scene_bearing",
    "scene_dolly",
    "scene_eye",
    "scene_focus",
    "filler_probe",
    "part_tint",
];

/// Image-changing axes every comparable capture must state, even when the truthful value is
/// `unknown`. Optional diagnostic knobs remain comparable when present but do not invalidate old
/// captures merely because neither side enabled the probe.
pub const REQUIRED_COMPARABLE: &[&str] = &[
    "screen",
    "realm",
    "race",
    "gender",
    "width",
    "height",
    "anim_time",
    "class",
    "appearance",
    "surface_width",
    "surface_height",
    "scale_factor",
    "display_mode",
    "player_position",
    "heading",
    "camera_orbit",
    "camera_pitch",
    "pose_elapsed_ms",
];

/// Fields recorded for provenance that must NOT block a comparison.
///
/// `worktree` belongs here for the same reason `commit` does: comparing our render against a
/// reference is the point, and the two will never share a build. It is recorded so a capture can
/// say whether its own binary matched its source — a dirty-tree capture is still a capture, but it
/// is not a reproducible one.
pub const PROVENANCE: &[&str] = &["commit", "captured_at", "client", "worktree"];

impl CaptureManifest {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        self.fields.insert(key.to_string(), value.to_string());
        self
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    pub fn fields(&self) -> impl Iterator<Item = (&str, &str)> {
        self.fields.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// The manifest path for an image path — the image path with `.manifest` appended, so it
    /// sorts next to the image and cannot collide with another capture's.
    #[must_use]
    pub fn path_for(image: &Path) -> PathBuf {
        let mut s = image.as_os_str().to_os_string();
        s.push(".manifest");
        PathBuf::from(s)
    }

    pub fn write_beside(&self, image: &Path) -> std::io::Result<PathBuf> {
        let path = Self::path_for(image);
        let body: String = self
            .fields
            .iter()
            .map(|(k, v)| format!("{k} = {v}\n"))
            .collect();
        std::fs::write(&path, body)?;
        Ok(path)
    }

    pub fn parse(text: &str) -> Self {
        let mut fields = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                fields.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        Self { fields }
    }

    /// Read the manifest beside an image, if it has one.
    #[must_use]
    pub fn read_beside(image: &Path) -> Option<Self> {
        std::fs::read_to_string(Self::path_for(image))
            .ok()
            .map(|t| Self::parse(&t))
    }
}

/// Write a capture and its manifest, or leave neither behind.
///
/// The two writes are one act. A PNG whose manifest failed to write is precisely the undocumented
/// capture `caer audit capture` refuses — and the failure is silent at the moment it matters,
/// because the image is on disk and looks like every other capture. It surfaces later as a parity
/// claim nobody can check.
///
/// The reachable known-bad is a pre-existing `<out>.manifest` **directory**: creating the PNG
/// succeeds, the sibling write cannot, and the caller used to report a screenshot and exit 0.
/// So the PNG is removed when the manifest cannot be written, and the error is returned.
pub fn write_capture(
    out: &Path,
    rgba: &[u8],
    width: u32,
    height: u32,
    manifest: &CaptureManifest,
) -> std::io::Result<PathBuf> {
    {
        let file = std::fs::File::create(out)?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .and_then(|mut w| w.write_image_data(rgba))
            .map_err(std::io::Error::other)?;
    }
    match manifest.write_beside(out) {
        Ok(path) => Ok(path),
        Err(e) => {
            // Best effort: if the image cannot be removed either, the returned error is still the
            // manifest's, because that is the one that makes the capture unusable.
            let _ = std::fs::remove_file(out);
            Err(e)
        }
    }
}

/// Why two captures are not comparable, or an empty list when they are.
///
/// A required field missing from either side is a mismatch: two equally incomplete captures do
/// not become comparable merely by sharing the same blind spot. Optional probe fields mismatch
/// when only one side enabled them.
#[must_use]
pub fn mismatches(a: &CaptureManifest, b: &CaptureManifest) -> Vec<String> {
    let mut out = Vec::new();
    for key in COMPARABLE {
        match (a.get(key), b.get(key)) {
            (Some(x), Some(y)) if x != y => out.push(format!("{key}: {x} vs {y}")),
            (Some(x), None) => out.push(format!("{key}: {x} vs <absent>")),
            (None, Some(y)) => out.push(format!("{key}: <absent> vs {y}")),
            (None, None) if REQUIRED_COMPARABLE.contains(key) => {
                out.push(format!("{key}: <absent> vs <absent>"));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CaptureManifest {
        CaptureManifest::new()
            .with("screen", "charcreate")
            .with("realm", 1)
            .with("race", 3)
            .with("gender", 0)
            .with("width", 2048)
            .with("height", 1536)
            .with("anim_time", 0.0)
            .with("class", 2)
            .with("appearance", "default")
            .with("surface_width", 2048)
            .with("surface_height", 1536)
            .with("scale_factor", 1)
            .with("display_mode", "windowed")
            .with("player_position", "unknown")
            .with("heading", "unknown")
            .with("camera_orbit", 0)
            .with("camera_pitch", 0)
            .with("pose_elapsed_ms", 0)
    }

    #[test]
    fn identical_state_compares_and_a_changed_field_does_not() {
        let a = base();
        assert!(
            mismatches(&a, &base()).is_empty(),
            "same state must compare"
        );

        let b = base().with("race", 7);
        let m = mismatches(&a, &b);
        assert_eq!(m, vec!["race: 3 vs 7".to_string()]);
    }

    /// The case the rule exists for: character-select against character-create.
    #[test]
    fn a_select_capture_is_not_comparable_to_a_create_capture() {
        let select = base().with("screen", "charselect");
        assert!(!mismatches(&select, &base()).is_empty());
    }

    #[test]
    fn two_equally_incomplete_manifests_are_not_comparable() {
        let a = CaptureManifest::new().with("screen", "charcreate");
        let missing = mismatches(&a, &a);
        assert!(missing.iter().any(|item| item.starts_with("realm:")));
        assert!(missing.iter().any(|item| item.starts_with("display_mode:")));
        assert!(missing.iter().any(|item| item.starts_with("appearance:")));
    }

    /// A capture that could not record what it is must not survive as an image.
    ///
    /// Known-bad shape, and it is mechanical rather than remembered: `<out>.manifest` already
    /// exists as a **directory**, so the PNG write succeeds and the sibling write cannot. The
    /// screenshot path used to print the failure to stderr and return normally, leaving an
    /// undocumented capture on disk and exiting 0 — an image indistinguishable from a good one
    /// that no comparison may legally use.
    #[test]
    fn a_capture_whose_manifest_cannot_be_written_leaves_no_image() {
        let dir = std::env::temp_dir().join(format!("caer-capture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("shot.png");
        let rgba = vec![0u8; 4 * 4 * 4];

        // GREEN control: with nothing in the way, both land.
        write_capture(&out, &rgba, 4, 4, &base()).expect("clean write");
        assert!(out.exists(), "the image must be written");
        assert!(CaptureManifest::path_for(&out).exists(), "and its manifest");

        // RED control: the manifest path is occupied by a directory.
        std::fs::remove_file(&out).expect("clear image");
        std::fs::remove_file(CaptureManifest::path_for(&out)).expect("clear manifest");
        std::fs::create_dir(CaptureManifest::path_for(&out)).expect("occupy manifest path");

        let err = write_capture(&out, &rgba, 4, 4, &base());
        assert!(
            err.is_err(),
            "a capture that cannot be documented must fail"
        );
        assert!(
            !out.exists(),
            "the undocumented image must not survive the failure"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The solver's own knob must not be invisible to the instrument that gates the solve.
    ///
    /// `CAER_SCENE_EYE` and `CAER_SCENE_FOCUS` move the lens, so a capture taken with either set is
    /// not a capture of the same thing as one taken without. Both were read by the framing code and
    /// recorded in no manifest, so the pair compared as equal — and varying eye height is the whole
    /// method behind the pre-world camera work. Before `scene_eye`/`scene_focus` joined
    /// [`COMPARABLE`] this assertion found an empty mismatch list.
    #[test]
    fn a_capture_taken_at_a_different_eye_height_is_not_comparable() {
        let raised = base().with("scene_eye", 0.80);
        assert_eq!(
            mismatches(&raised, &base()),
            vec!["scene_eye: 0.8 vs <absent>".to_string()]
        );

        let aimed = base().with("scene_focus", 0.80);
        assert_eq!(
            mismatches(&aimed, &base()),
            vec!["scene_focus: 0.8 vs <absent>".to_string()]
        );

        // Two captures that agree on the override still compare: the field gates a mismatch, it
        // does not forbid solving.
        assert!(mismatches(&raised, &base().with("scene_eye", 0.80)).is_empty());
    }

    /// Resolution changes framing, so it is not a free variable.
    #[test]
    fn a_different_resolution_is_a_mismatch() {
        let small = base().with("width", 1024).with("height", 768);
        assert_eq!(mismatches(&base(), &small).len(), 2);
    }

    /// A probe capture is not a reference capture, however identical the rest of the state.
    #[test]
    fn a_probe_capture_never_compares_against_a_plain_one() {
        let probed = base().with("filler_probe", 1);
        assert_eq!(
            mismatches(&base(), &probed),
            vec!["filler_probe: <absent> vs 1".to_string()]
        );
    }

    /// Commit and timestamp differ between any two runs and must not block a comparison.
    #[test]
    fn provenance_fields_do_not_block_a_comparison() {
        let a = base()
            .with("commit", "aaaa")
            .with("captured_at", "t1")
            .with("worktree", "dirty");
        let b = base()
            .with("commit", "bbbb")
            .with("captured_at", "t2")
            .with("worktree", "clean");
        assert!(mismatches(&a, &b).is_empty());
        // And none of them may creep into the comparable set by accident.
        for k in PROVENANCE {
            assert!(
                !COMPARABLE.contains(k),
                "`{k}` is provenance and must not gate a comparison"
            );
        }
    }

    #[test]
    fn round_trips_through_text() {
        let a = base().with("scene_bearing", 60.0);
        let text: String = a.fields().map(|(k, v)| format!("{k} = {v}\n")).collect();
        assert_eq!(CaptureManifest::parse(&text), a);
    }

    #[test]
    fn manifest_path_sits_beside_the_image() {
        let p = CaptureManifest::path_for(Path::new("/tmp/shot.png"));
        assert_eq!(p, Path::new("/tmp/shot.png.manifest"));
    }
}
