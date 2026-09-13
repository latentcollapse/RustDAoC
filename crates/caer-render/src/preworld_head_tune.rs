//! Live per-race head pitch for the pre-world figure, so the tilt can be dialled in by eye.
//!
//! **This is an instrument before it is a fix.** The report is that some races stand there looking
//! at the floor, and three explanations are still live: the authored clip, our compose path, or
//! the fixed stage lens looking up at a race drawn 35% taller than the camera assumes. Clip
//! analysis has already answered the first one wrong once — Firbolg male carries the *smallest*
//! authored neck angle of any male and is one of the two worst on screen — so what is missing is a
//! number measured off the rendered product, per race, by the only judge who can see the defect.
//!
//! Dialling each race until it looks right produces exactly that number, and its SHAPE is the
//! discriminator:
//!
//! - offsets that track `race_display_scale` → the lens, and the fix is geometry, not 36 nudges;
//! - offsets that cluster by clip → the authored animation;
//! - one constant across every race → our compose path, and one constant fixes all of them.
//!
//! Same contract as [`crate::preworld_camera_tune`]: per race+gender, starts empty, means "no
//! offset" until something sets it, and with nothing set this module changes no pixel.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use caer_assets::nif::{xform_to_mat4, Clip, Skeleton};

use crate::entities::SkinnedRig;

/// One press worth of tilt. A degree is already visible on a head at this framing, so half of one
/// is the finest step worth having and the coarsest that still lets a face be settled exactly.
pub const STEP_DEG: f32 = 0.5;

/// How far the head can be driven either way. A real neck does not reach 30 degrees of nod, and
/// past that a held key is bending the skull through the shoulders rather than settling a pose.
const LIMIT_DEG: f32 = 30.0;

/// Degrees of pitch per race+gender. Positive tips the chin UP, which is the same sense as the
/// `neck dX` column `avatarcover` reports, so a settled value here is comparable with it.
type Store = BTreeMap<(u8, u8), f32>;

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(
            state_path()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| parse_state(&t))
                .unwrap_or_default(),
        )
    })
}

/// Where settled head pitches survive the process. Beside the camera's own save file and for the
/// same reason: these cost a human sitting in front of the client, and losing a session's work to
/// a client restart is losing the measurement.
#[must_use]
pub fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("caer/preworld_head.txt"))
}

fn parse_state(text: &str) -> Store {
    let mut out = Store::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut f = line.split_whitespace();
        let (Some(race), Some(gender), Some(deg)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let (Ok(race), Ok(gender), Ok(deg)) =
            (race.parse::<u8>(), gender.parse::<u8>(), deg.parse::<f32>())
        else {
            continue;
        };
        if !deg.is_finite() {
            continue;
        }
        out.insert((race, gender), deg.clamp(-LIMIT_DEG, LIMIT_DEG));
    }
    out
}

fn render_state(all: &Store) -> String {
    let mut out = String::from(
        "# CAER pre-world head pitch, written by the live tuner (Ctrl+Alt+[ and Ctrl+Alt+]).\n\
         # race  gender  degrees. Positive tips the chin up. Delete this file to clear them all.\n",
    );
    for ((race, gender), deg) in all {
        out.push_str(&format!("{race} {gender} {deg:.2}\n"));
    }
    out
}

/// Write the whole store. Best-effort: a head nudge must not fail because a disk did.
fn persist(all: &Store) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, render_state(all)) {
        log::warn!("preworld head tune: cannot save {}: {e}", path.display());
    }
}

fn lock() -> std::sync::MutexGuard<'static, Store> {
    store()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The head pitch Matt settled by eye against the stage, 2026-08-24. Degrees, chin-up positive.
///
/// **Measurements, not choices.** Every one cost a human sitting in front of the client moving a
/// head half a degree at a time until it looked right, and nothing derives them.
///
/// What they turned out to mean is in [`crate::preworld_head_tune`]'s header: they land on exactly
/// the bodies whose idle clip keys the HEAD bone off its bind orientation, and on no others. They
/// are therefore a **correction for a compose defect, not a per-race art choice** — when that
/// defect is found and fixed, this table should shrink to nothing rather than be carried forward.
///
/// Absent entries are bodies that already looked right and were never touched.
#[must_use]
pub fn shipped(race: u8, gender: u8) -> Option<f32> {
    let male = gender != caer_assets::figures::GENDER_FEMALE;
    Some(match (race, male) {
        (1, false) => -12.50,  // Briton female
        (5, false) => -13.00,  // Norse female
        (6, false) => -5.00,   // Troll female
        (7, false) => -7.00,   // Dwarf female
        (8, false) => -12.00,  // Kobold female
        (9, true) => -2.00,    // Celt male
        (9, false) => -16.50,  // Celt female
        (10, true) => -5.00,   // Firbolg male
        (10, false) => -14.50, // Firbolg female
        (11, false) => -11.50, // Elf female
        (12, false) => -8.50,  // Lurikeen female
        (13, false) => -2.50,  // Inconnu female
        (14, false) => 2.00,   // Valkyn female — the only body needing its chin RAISED
        (15, false) => -4.50,  // Sylvan female
        (16, false) => -15.00, // Half Ogre female
        (18, true) => -3.00,   // Shar male
        (18, false) => -9.00,  // Shar female
        _ => return None,
    })
}

/// This race+gender's settled pitch: a live nudge first, then the shipped table, then no tilt.
#[must_use]
pub fn settled(race: u8, gender: u8) -> f32 {
    lock()
        .get(&(race, gender))
        .copied()
        .or_else(|| shipped(race, gender))
        .unwrap_or(0.0)
}

/// Nudge one race+gender by `steps` of [`STEP_DEG`], returning the new value.
pub fn nudge(race: u8, gender: u8, steps: f32) -> f32 {
    let mut g = lock();
    let next = (g.get(&(race, gender)).copied().unwrap_or(0.0) + STEP_DEG * steps)
        .clamp(-LIMIT_DEG, LIMIT_DEG);
    g.insert((race, gender), next);
    let snapshot = g.clone();
    drop(g);
    persist(&snapshot);
    next
}

/// Drop this race+gender's offset, returning it to the authored pose.
pub fn reset(race: u8, gender: u8) {
    let mut g = lock();
    g.remove(&(race, gender));
    let snapshot = g.clone();
    drop(g);
    persist(&snapshot);
}

/// Drop every offset. The A/B control for "is any of this doing anything at all".
pub fn reset_all() {
    let mut g = lock();
    g.clear();
    drop(g);
    persist(&Store::new());
}

/// Every settled offset, race-major. This is the measurement the whole module exists to produce —
/// the caller pairs it with each race's display scale, and the SHAPE of that pairing is the answer.
#[must_use]
pub fn entries() -> Vec<((u8, u8), f32)> {
    lock().iter().map(|(k, v)| (*k, *v)).collect()
}

/// Rotate the head, and only the head, inside a finished skinning palette.
///
/// **Why the palette and not the pose:** a palette entry is `world_animᵦ · world_bind⁻¹ᵦ`, so
/// left-multiplying a world-space rotation `R` onto it is exactly `R · world_animᵦ` — the same
/// result as rotating the head's local transform before forward kinematics, provided every bone
/// below the head gets the same `R`. Doing it here keeps the change to one call site on the
/// pre-world path and touches nothing in the skinning chain the world render shares.
///
/// **Why `Bip01 Head` and not `Bip01 Neck`:** in a Biped rig both clavicles are children of the
/// neck, so a neck offset swings the arms with the skull. The head's only descendants are the head
/// nub and the hair bones, which is what "head tilt" means when someone points at the screen.
///
/// The pivot carries `z_offset` because the palette does: that constant is added to every palette
/// translation to foot-anchor the body, and a rotation has to happen about the head's *anchored*
/// position or the skull swings on an arm as long as the offset.
pub fn tilt_head(
    rig: &SkinnedRig,
    clip: &Clip,
    t: f32,
    degrees: f32,
    palette: &mut [[[f32; 4]; 4]],
) {
    let rad = degrees.to_radians();
    if !rad.is_finite() || rad == 0.0 {
        return;
    }
    let mut cursor = 0usize;
    for r in &rig.rigs {
        let sk = &r.skeleton;
        let stride = sk.bones.len();
        let span = r.parts.len() * stride;
        if cursor + span > palette.len() {
            return;
        }
        let head = sk
            .bones
            .iter()
            .position(|b| b.name.eq_ignore_ascii_case("Bip01 Head"));
        let Some(head) = head else {
            cursor += span;
            continue;
        };
        let posed = sk.pose(clip, t);
        let Some(hx) = posed.get(head) else {
            cursor += span;
            continue;
        };
        let m = xform_to_mat4(hx);
        let pivot = [m[3][0], m[3][1], m[3][2] + rig.z_offset];
        let rot = rotation_about(right_axis(sk), rad, pivot);
        // Bones are topologically ordered, parent before child, so one forward pass marks the
        // whole subtree: a bone is in it when it IS the head or its parent already is.
        let mut subtree = vec![false; stride];
        for i in 0..stride {
            subtree[i] = i == head || sk.bones[i].parent.is_some_and(|p| subtree[p]);
        }
        for p in 0..r.parts.len() {
            let base = cursor + p * stride;
            for (b, on) in subtree.iter().enumerate() {
                if *on {
                    palette[base + b] = mul4(&rot, &palette[base + b]);
                }
            }
        }
        cursor += span;
    }
}

/// The character's own RIGHT, in model space — the axis a nod turns about.
///
/// Measured off the rig rather than assumed from a bone's local axis convention: the clavicle
/// pair spans the shoulders, and they are named for the side they are on, so `R − L` is the right
/// vector with its sign already settled. Guessing which of a Biped head's local Y/Z is the side
/// axis, and which way round, is two coin flips that a wrong call renders as a head twisting
/// sideways instead of nodding.
///
/// The fallback is model-space `−X`, which is derived and not invented: these bodies face −Y at
/// yaw 0 and stand Z-up, and right = forward × up = (−ŷ) × ẑ = −x̂. Every playable rig ships
/// clavicles, so this is a floor under a rig that does not, not the normal path.
fn right_axis(sk: &Skeleton) -> [f32; 3] {
    let side = |tag: &str| {
        sk.bones
            .iter()
            .find(|b| {
                let n = b.name.to_ascii_lowercase();
                n.contains("clavicle") && n.contains(tag)
            })
            .map(|b| b.world_bind.2)
    };
    match (side(" l "), side(" r ")) {
        (Some(l), Some(r)) => {
            let v = [r[0] - l[0], r[1] - l[1], r[2] - l[2]];
            normalize(v).unwrap_or([-1.0, 0.0, 0.0])
        }
        _ => [-1.0, 0.0, 0.0],
    }
}

fn normalize(v: [f32; 3]) -> Option<[f32; 3]> {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (len > 1e-6).then(|| [v[0] / len, v[1] / len, v[2] / len])
}

/// Column-major 4×4 rotation of `rad` about unit `axis`, taken about the point `pivot`.
///
/// Positive is chin UP. With Z up and the axis pointing to the character's right, a small positive
/// rotation moves any forward-pointing vector `v` by `θ · (right × v)`, and right × forward = up.
fn rotation_about(axis: [f32; 3], rad: f32, pivot: [f32; 3]) -> [[f32; 4]; 4] {
    let (s, c) = rad.sin_cos();
    let t = 1.0 - c;
    let [x, y, z] = axis;
    let mut m = [
        [t * x * x + c, t * x * y + s * z, t * x * z - s * y, 0.0],
        [t * x * y - s * z, t * y * y + c, t * y * z + s * x, 0.0],
        [t * x * z + s * y, t * y * z - s * x, t * z * z + c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let rp = [
        m[0][0] * pivot[0] + m[1][0] * pivot[1] + m[2][0] * pivot[2],
        m[0][1] * pivot[0] + m[1][1] * pivot[1] + m[2][1] * pivot[2],
        m[0][2] * pivot[0] + m[1][2] * pivot[1] + m[2][2] * pivot[2],
    ];
    m[3] = [pivot[0] - rp[0], pivot[1] - rp[1], pivot[2] - rp[2], 1.0];
    m
}

/// `a · b`, both column-major.
fn mul4(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for (c, col) in out.iter_mut().enumerate() {
        for (r, cell) in col.iter_mut().enumerate() {
            *cell = a[0][r] * b[c][0] + a[1][r] * b[c][1] + a[2][r] * b[c][2] + a[3][r] * b[c][3];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_assets::nif::{Bone, RiggedModel, RiggedPart, Xform};

    /// The store loads from — and every nudge writes to — the user's own settled offsets, which
    /// are a measurement that cost them time in front of the client. Nothing here touches it:
    /// these tests exercise the text form and the geometry, and both are pure.
    /// The settled table must survive the file that produced it.
    ///
    /// These seventeen numbers are a human's afternoon. They lived only in
    /// `~/.local/state/caer/preworld_head.txt` — a cache that `reset_all` clears and a fresh
    /// checkout never had — until they were baked here.
    #[test]
    fn the_settled_head_pitches_are_the_ones_that_were_measured() {
        use caer_assets::figures::{GENDER_FEMALE, GENDER_MALE};
        assert_eq!(shipped(1, GENDER_FEMALE), Some(-12.50), "Briton female");
        assert_eq!(
            shipped(14, GENDER_FEMALE),
            Some(2.00),
            "Valkyn female is the only body whose chin was raised"
        );
        assert_eq!(shipped(10, GENDER_MALE), Some(-5.00), "Firbolg male");

        // Bodies that already looked right were never touched and must stay untouched — the
        // three `cat_i_hf` females and Frostalf female are the controls that showed this is not
        // a gender effect, so a stray entry here would erase the evidence as well as tilt a head.
        for (race, gender, who) in [
            (2, GENDER_FEMALE, "Avalonian female"),
            (3, GENDER_FEMALE, "Highlander female"),
            (4, GENDER_FEMALE, "Saracen female"),
            (17, GENDER_FEMALE, "Frostalf female"),
            (1, GENDER_MALE, "Briton male"),
        ] {
            assert_eq!(shipped(race, gender), None, "{who} was never corrected");
        }

        let n = (1..=21u8)
            .flat_map(|r| [GENDER_MALE, GENDER_FEMALE].map(move |g| (r, g)))
            .filter(|&(r, g)| shipped(r, g).is_some())
            .count();
        assert_eq!(n, 17, "the settled table is seventeen bodies");
    }

    /// The shipped table is what a body wears when nothing has been nudged; a body with no entry
    /// sits flat rather than picking up its neighbour's correction.
    #[test]
    fn the_shipped_table_is_what_an_untouched_body_wears() {
        use caer_assets::figures::GENDER_FEMALE;
        assert_eq!(settled(1, GENDER_FEMALE), -12.50);
        assert_eq!(settled(2, GENDER_FEMALE), 0.0);
    }

    #[test]
    fn settled_offsets_round_trip_through_the_save_file() {
        let mut all = Store::new();
        all.insert((10, 0), -3.5);
        all.insert((10, 1), 7.25);
        all.insert((2, 1), 0.5);
        let back = parse_state(&render_state(&all));
        assert_eq!(
            back, all,
            "a settled head pitch did not survive the round trip"
        );
    }

    /// The file is hand-editable and may be hand-broken. A bad line is skipped, never silently
    /// becomes a number, and a value beyond the limit is clamped rather than trusted — a NaN or a
    /// hand-typed 900 would otherwise fold the head through the chest on load.
    #[test]
    fn a_damaged_save_file_degrades_to_no_offset() {
        let parsed = parse_state(
            "# comment\n\
             \n\
             10 0 -3.50\n\
             10 1 oops\n\
             11 0 NaN\n\
             notarace 0 1\n\
             12 1 900\n\
             13\n",
        );
        assert_eq!(
            parsed.get(&(10, 0)),
            Some(&-3.5),
            "the good line must survive"
        );
        assert_eq!(parsed.get(&(10, 1)), None, "junk must not become a pitch");
        assert_eq!(parsed.get(&(11, 0)), None, "NaN must not become a pitch");
        assert_eq!(
            parsed.get(&(12, 1)),
            Some(&LIMIT_DEG),
            "a hand-typed 900 degrees is clamped, not obeyed"
        );
    }

    fn xf(t: [f32; 3]) -> Xform {
        ([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], 1.0, t)
    }

    /// Positive degrees must tip the chin UP, and nothing else about the convention is free: the
    /// sign is what decides whether `]` fixes a head that looks at the floor or drives it further
    /// into the ground. Rotating the model-space forward (−Y, the way these bodies face at yaw 0)
    /// about the character's right has to raise it.
    #[test]
    fn a_positive_pitch_lifts_the_chin() {
        let right = [-1.0, 0.0, 0.0];
        let m = rotation_about(right, 10f32.to_radians(), [0.0, 0.0, 0.0]);
        let forward = [0.0, -1.0, 0.0];
        let z = m[0][2] * forward[0] + m[1][2] * forward[1] + m[2][2] * forward[2];
        assert!(z > 0.0, "a positive pitch must raise the face, got z {z}");
    }

    /// A rotation about a pivot must leave the pivot exactly where it was — otherwise the head
    /// does not tilt, it swings on an arm as long as whatever the pivot got wrong.
    #[test]
    fn the_pivot_does_not_move() {
        let pivot = [3.0, -7.0, 94.0];
        let m = rotation_about([-1.0, 0.0, 0.0], 22f32.to_radians(), pivot);
        for (axis, want) in pivot.iter().enumerate() {
            let got =
                m[0][axis] * pivot[0] + m[1][axis] * pivot[1] + m[2][axis] * pivot[2] + m[3][axis];
            assert!(
                (got - want).abs() < 1e-3,
                "pivot axis {axis}: {got} vs {want}"
            );
        }
    }

    /// The shoulders decide which way a nod turns, and they are measured, not assumed. `R − L`
    /// with the clavicles a unit apart on X must come back as a unit vector along X.
    #[test]
    fn the_nod_axis_is_measured_off_the_shoulders() {
        let sk = Skeleton {
            bones: vec![
                Bone {
                    name: "Bip01 L Clavicle".into(),
                    parent: None,
                    local: xf([-4.0, 0.0, 80.0]),
                    world_bind: xf([-4.0, 0.0, 80.0]),
                    id: None,
                },
                Bone {
                    name: "Bip01 R Clavicle".into(),
                    parent: None,
                    local: xf([4.0, 0.0, 80.0]),
                    world_bind: xf([4.0, 0.0, 80.0]),
                    id: None,
                },
            ],
            name_to_index: Default::default(),
        };
        assert_eq!(right_axis(&sk), [1.0, 0.0, 0.0]);

        // No clavicles: the derived model-space right, not a panic and not a zero vector, which
        // would rotate the head about nothing and read as "the knob does not work".
        let bare = Skeleton {
            bones: Vec::new(),
            name_to_index: Default::default(),
        };
        assert_eq!(right_axis(&bare), [-1.0, 0.0, 0.0]);
    }

    /// Head bones move, the body does not. A tilt that also moved the spine would be indis-
    /// tinguishable from the whole figure leaning, which is a different defect entirely.
    #[test]
    fn only_the_head_and_what_hangs_off_it_moves() {
        let bones = vec![
            Bone {
                name: "Bip01".into(),
                parent: None,
                local: xf([0.0, 0.0, 0.0]),
                world_bind: xf([0.0, 0.0, 0.0]),
                id: None,
            },
            Bone {
                name: "Bip01 L Clavicle".into(),
                parent: Some(0),
                local: xf([-4.0, 0.0, 80.0]),
                world_bind: xf([-4.0, 0.0, 80.0]),
                id: None,
            },
            Bone {
                name: "Bip01 R Clavicle".into(),
                parent: Some(0),
                local: xf([4.0, 0.0, 80.0]),
                world_bind: xf([4.0, 0.0, 80.0]),
                id: None,
            },
            Bone {
                name: "Bip01 Head".into(),
                parent: Some(0),
                local: xf([0.0, 0.0, 90.0]),
                world_bind: xf([0.0, 0.0, 90.0]),
                id: None,
            },
            Bone {
                name: "Bip01 HeadNub".into(),
                parent: Some(3),
                local: xf([0.0, 0.0, 8.0]),
                world_bind: xf([0.0, 0.0, 98.0]),
                id: None,
            },
        ];
        let n = bones.len();
        let part = RiggedPart {
            name: "body".into(),
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            joints: Vec::new(),
            weights: Vec::new(),
            inverse_bind: vec![xf([0.0, 0.0, 0.0]); n],
            texture: None,
            morph: None,
            morph_target_ids: Vec::new(),
        };
        let rig = SkinnedRig {
            rigs: vec![RiggedModel {
                skeleton: Skeleton {
                    bones,
                    name_to_index: Default::default(),
                },
                parts: vec![part],
            }],
            clip: Clip {
                tracks: Default::default(),
                duration: 1.0,
                rate: 1.0,
            },
            walk: None,
            run: None,
            back: None,
            slide_left: None,
            slide_right: None,
            stride_walk: 0.0,
            stride_run: 0.0,
            stride_back: 0.0,
            stride_strafe: 0.0,
            z_offset: 0.0,
        };
        let clip = Clip {
            tracks: Default::default(),
            duration: 1.0,
            rate: 1.0,
        };
        let before = crate::anim_skin::build_palettes_serial(
            &rig,
            &[crate::anim_skin::UniquePaletteJob {
                loco: crate::entities::Loco::Idle,
                t: 0.0,
                blend: None,
            }],
        );
        let mut after = before.clone();
        tilt_head(&rig, &clip, 0.0, 12.0, &mut after);

        for b in [0usize, 1, 2] {
            assert_eq!(
                before[b], after[b],
                "bone {b} is not below the head and must not move"
            );
        }
        assert_ne!(before[3], after[3], "the head must move");
        assert_ne!(
            before[4], after[4],
            "the head nub hangs off the head and must follow it"
        );

        // The head's own origin is the pivot, so its translation stays put while its orientation
        // turns. The nub, 8 units above it, has to swing — and forward, since the chin went up.
        assert!(
            (after[3][3][2] - before[3][3][2]).abs() < 1e-3,
            "the head pivot moved in Z"
        );
        assert!(
            after[4][3][1] < before[4][3][1] - 1e-3,
            "a chin-up tilt must carry the crown backwards: {:?} vs {:?}",
            after[4][3],
            before[4][3]
        );

        // Zero is the identity, exactly. With nothing dialled in this module changes no pixel.
        let mut untouched = before.clone();
        tilt_head(&rig, &clip, 0.0, 0.0, &mut untouched);
        assert_eq!(before, untouched, "a zero offset must be bit-identical");
    }
}
