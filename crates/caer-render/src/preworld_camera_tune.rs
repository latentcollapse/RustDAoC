//! Live pre-world camera controls, so the composition can be dialled in by eye and read back.
//!
//! The four stage-camera numbers — bearing, dolly, eye height, focus height — are a declared
//! provenance gap: no `NiCamera` exists in any pre-world scene, so they cannot be recovered from
//! the asset and have to be solved against a reference capture. Solving them from the outside was
//! costing a rebuild and a relaunch per candidate value, which is slow enough that it was the
//! bottleneck rather than the question.
//!
//! These overrides sit in front of the `CAER_SCENE_*` environment variables and can be nudged
//! while the client is running, so a human can see the change immediately and then read the
//! settled numbers back out. **A tuning aid, not a second source of truth:** the values it holds
//! are per realm, start empty, and mean "no override" until something sets them. With nothing set
//! this module changes no pixel.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Which of the four stage-camera numbers a nudge applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knob {
    /// Degrees around the anchor. Which way the stage faces.
    Bearing,
    /// Multiplier on the pull-back distance. Bigger is further away.
    Dolly,
    /// Eye height, as a fraction of `CHARACTER_HEIGHT`.
    Eye,
    /// Focus height, as a fraction of `CHARACTER_HEIGHT`. Where the lens aims.
    Focus,
    /// Where the figure STANDS, sideways across the stage, in world units along the camera's own
    /// right vector. Positive walks them to screen-right. The camera keeps them centred, so this
    /// slides the scene behind them rather than moving them within the frame.
    SubjectX,
    /// Where the figure STANDS, in depth, in world units along the camera's own view vector.
    /// Positive walks them AWAY from the lens, deeper into the stage.
    ///
    /// Its own knob rather than a dolly because the two are not the same move. Dolly walks the
    /// CAMERA and changes how big the figure is drawn; this walks the FIGURE and leaves their size
    /// alone, sliding them through the backdrop instead. That is the one that matters here: the
    /// stage props are not spaced the way Eden's are, so placing the figure among them is a
    /// placement problem, not a lens problem.
    SubjectY,
    /// Straight-up camera height, in world units, added to the eye AND the aim together.
    ///
    /// A pedestal move, which none of the other knobs can make. [`Knob::Eye`] raises the lens
    /// while the aim stays put, so it TILTS; this translates the whole camera vertically and
    /// leaves the view direction untouched. That is the move for "the lens is looking up at this
    /// figure" — raise it to their eye level and the upward angle goes away without the shot
    /// swinging. World units rather than a fraction of `CHARACTER_HEIGHT` because the thing being
    /// matched is a real height on a real body, not a proportion of a constant that every race
    /// shares.
    Lift,
}

impl Knob {
    /// Every knob, in report order. One list so a new knob cannot be added to the enum and then
    /// silently missed by the report, the save file, or the loader.
    pub const ALL: [Knob; 7] = [
        Knob::Bearing,
        Knob::Dolly,
        Knob::Eye,
        Knob::Focus,
        Knob::SubjectX,
        Knob::SubjectY,
        Knob::Lift,
    ];

    /// One press worth of change. Bearing moves in degrees; the rest are fractions, so they need
    /// a much finer step to be usable.
    #[must_use]
    pub fn step(self) -> f32 {
        match self {
            Knob::Bearing => 1.0,
            Knob::Dolly => 0.02,
            Knob::Eye | Knob::Focus => 0.01,
            // World units. The figure is ~70 tall, so 5 is a visible step without overshooting.
            Knob::SubjectX | Knob::SubjectY => 5.0,
            // Finer than the walk knobs: an eye height is being matched to a head, and 5 units on
            // a 70-unit body overshoots the whole face.
            Knob::Lift => 1.0,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Knob::Bearing => "BEARING",
            Knob::Dolly => "DOLLY",
            Knob::Eye => "EYE",
            Knob::Focus => "FOCUS",
            Knob::SubjectX => "SUBJECT_X",
            Knob::SubjectY => "SUBJECT_Y",
            Knob::Lift => "LIFT",
        }
    }

    /// The environment variable this knob overrides, for reporting a settled value.
    #[must_use]
    pub fn env_name(self) -> &'static str {
        match self {
            Knob::Bearing => "CAER_SCENE_BEARING",
            Knob::Dolly => "CAER_SCENE_DOLLY",
            Knob::Eye => "CAER_SCENE_EYE",
            Knob::Focus => "CAER_SCENE_FOCUS",
            Knob::SubjectX => "CAER_SCENE_SUBJECT_X",
            Knob::SubjectY => "CAER_SCENE_SUBJECT_Y",
            Knob::Lift => "CAER_SCENE_LIFT",
        }
    }
}

/// Per-realm overrides. `None` means "not overridden" — the env var, then the shipped default.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Tuning {
    pub bearing: Option<f32>,
    pub dolly: Option<f32>,
    pub eye: Option<f32>,
    pub focus: Option<f32>,
    pub subject_x: Option<f32>,
    pub subject_y: Option<f32>,
    pub lift: Option<f32>,
}

impl Tuning {
    #[must_use]
    pub fn get(&self, knob: Knob) -> Option<f32> {
        match knob {
            Knob::Bearing => self.bearing,
            Knob::Dolly => self.dolly,
            Knob::Eye => self.eye,
            Knob::Focus => self.focus,
            Knob::SubjectX => self.subject_x,
            Knob::SubjectY => self.subject_y,
            Knob::Lift => self.lift,
        }
    }

    fn set(&mut self, knob: Knob, v: f32) {
        match knob {
            Knob::Bearing => self.bearing = Some(v),
            Knob::Dolly => self.dolly = Some(v),
            Knob::Eye => self.eye = Some(v),
            Knob::Focus => self.focus = Some(v),
            Knob::SubjectX => self.subject_x = Some(v),
            Knob::SubjectY => self.subject_y = Some(v),
            Knob::Lift => self.lift = Some(v),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The composition Matt settled by eye against the Eden bank, 2026-08-23.
///
/// Read straight off the live tuner's own report, which is the workflow this module was built for:
/// solve it on screen, print it, bake it here. **These are measurements, not choices** — nothing
/// derives them and nothing should "improve" them without a human looking at the result.
///
/// Per realm because the three stages are different rooms with their props at different distances;
/// a single set of numbers frames one of them and misframes the other two. Absent entries mean the
/// knob was never moved and keeps the generic default beside it.
///
/// Dolly supersedes the old single `SHIPPED_DOLLY`: that value existed to hold a composition
/// checked against the bank, and these are that check redone per realm with a human's eye on it.
#[must_use]
pub fn shipped(realm: u8) -> Tuning {
    match realm {
        1 => Tuning {
            bearing: Some(-144.415),
            dolly: Some(0.702),
            eye: Some(0.630),
            focus: None,
            subject_x: Some(35.0),
            subject_y: None,
            lift: None,
        },
        2 => Tuning {
            bearing: Some(-148.472),
            dolly: Some(0.742),
            eye: Some(0.680),
            focus: None,
            subject_x: Some(50.0),
            subject_y: Some(15.0),
            lift: None,
        },
        3 => Tuning {
            bearing: Some(-134.853),
            dolly: Some(0.782),
            eye: None,
            focus: None,
            subject_x: Some(0.0),
            subject_y: Some(15.0),
            lift: None,
        },
        _ => Tuning::default(),
    }
}

/// One knob's value: a live nudge first, then the shipped composition, then the caller's default.
///
/// The env layer sits between the first two and is applied by the caller, because only the caller
/// knows which variable name belongs to which knob at its own call site.
#[must_use]
pub fn settled(realm: u8, knob: Knob, default: f32) -> f32 {
    tuning(realm)
        .get(knob)
        .or_else(|| shipped(realm).get(knob))
        .unwrap_or(default)
}

/// Where a settled composition survives the process.
///
/// These four numbers per realm cost a human sitting in front of the client moving them by eye,
/// and until this file existed they lived only in process memory — closing the client threw the
/// whole session's work away, with nothing but a `Ctrl+Alt+P` printout standing between a settled
/// camera and re-solving it from scratch. Losing that once is once too many.
///
/// A cache, not a source of truth: the shipped composition still lives in the source constants and
/// `report` is still how a settled value gets baked back into them. Delete this file and the
/// client comes up on the shipped numbers.
#[must_use]
pub fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("caer/preworld_camera.txt"))
}

fn parse_state(text: &str) -> [Tuning; 4] {
    let mut out = [Tuning::default(); 4];
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut f = line.split_whitespace();
        let (Some(realm), Some(knob), Some(value)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let (Ok(realm), Ok(value)) = (realm.parse::<u8>(), value.parse::<f32>()) else {
            continue;
        };
        if !value.is_finite() {
            continue;
        }
        let Some(k) = Knob::ALL.iter().copied().find(|k| k.label() == knob) else {
            continue;
        };
        out[slot(realm)].set(k, value);
    }
    out
}

fn render_state(all: &[Tuning; 4]) -> String {
    let mut out = String::from(
        "# CAER pre-world stage camera, written by the live tuner (Ctrl+Alt+arrows).\n         # realm  knob  value. Delete this file to return to the shipped composition.\n",
    );
    for (realm, t) in all.iter().enumerate() {
        for knob in Knob::ALL {
            if let Some(v) = t.get(knob) {
                out.push_str(&format!("{realm} {} {v:.3}\n", knob.label()));
            }
        }
    }
    out
}

/// Write the whole store. Best-effort: a camera nudge must not fail because a disk did.
fn persist(all: &[Tuning; 4]) {
    let Some(path) = state_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, render_state(all)) {
        log::warn!("preworld camera tune: cannot save {}: {e}", path.display());
    }
}

/// Realm 1..3 keep separate tunings: the stage camera is fixed per realm, so one shared set of
/// numbers would mean tuning Hibernia untunes Albion.
fn store() -> &'static Mutex<[Tuning; 4]> {
    static S: OnceLock<Mutex<[Tuning; 4]>> = OnceLock::new();
    S.get_or_init(|| {
        // Loaded once, lazily, so every entry point picks it up without an init call to forget.
        let loaded = state_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| parse_state(&t))
            .unwrap_or_default();
        Mutex::new(loaded)
    })
}

fn slot(realm: u8) -> usize {
    (realm as usize).min(3)
}

/// Current overrides for a realm.
#[must_use]
pub fn tuning(realm: u8) -> Tuning {
    store().lock().map(|s| s[slot(realm)]).unwrap_or_default()
}

/// Nudge one knob by `steps` of its own step size, seeding from `current` the first time so a
/// nudge starts from what is on screen rather than from zero.
///
/// Returns the new value.
pub fn nudge(realm: u8, knob: Knob, steps: f32, current: f32) -> f32 {
    let mut guard = match store().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let t = &mut guard[slot(realm)];
    let base = t.get(knob).unwrap_or(current);
    let mut next = base + knob.step() * steps;
    // Dolly through zero flips the camera through the subject; clamp it to something sane rather
    // than letting a held key invert the shot.
    if knob == Knob::Dolly {
        next = next.clamp(0.10, 6.0);
    }
    if matches!(knob, Knob::Eye | Knob::Focus) {
        next = next.clamp(-1.0, 4.0);
    }
    if matches!(knob, Knob::SubjectX | Knob::SubjectY) {
        // A stage is a few hundred units across; past that the figure walks off it entirely.
        next = next.clamp(-400.0, 400.0);
    }
    if knob == Knob::Lift {
        // Two body heights either way. Beyond that the lens is under the floor or above the
        // backdrop dome's rim, and the shot is a held key away from being unrecoverable by eye.
        next = next.clamp(-140.0, 140.0);
    }
    if knob == Knob::Bearing {
        // Keep it in (-180, 180] so the printed value is comparable with the scene's own.
        while next <= -180.0 {
            next += 360.0;
        }
        while next > 180.0 {
            next -= 360.0;
        }
    }
    t.set(knob, next);
    let snapshot = *guard;
    drop(guard);
    persist(&snapshot);
    next
}

/// Drop every override for a realm, returning it to the shipped composition.
pub fn reset(realm: u8) {
    let snapshot = {
        let Ok(mut g) = store().lock() else {
            return;
        };
        g[slot(realm)] = Tuning::default();
        *g
    };
    persist(&snapshot);
}

/// One line per overridden knob, in a form that can be pasted back as environment variables.
#[must_use]
pub fn report(realm: u8) -> Vec<String> {
    let t = tuning(realm);
    let mut out = Vec::new();
    for knob in Knob::ALL {
        if let Some(v) = t.get(knob) {
            out.push(format!("{}={:.3}", knob.env_name(), v));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settled camera costs a human sitting in front of the client moving numbers by eye, so
    /// what gets written has to be exactly what comes back. Round-trip through the text form.
    ///
    /// Pure on purpose: the real file is the user's own settled composition and a test must never
    /// touch it, and `XDG_STATE_HOME` is process-global so overriding it would race every other
    /// test in this binary.
    #[test]
    fn a_settled_camera_round_trips_through_the_save_file() {
        let mut all = [Tuning::default(); 4];
        all[1] = Tuning {
            bearing: Some(-146.0),
            dolly: Some(0.72),
            eye: Some(0.615),
            focus: Some(0.548),
            subject_x: Some(-15.0),
            subject_y: Some(25.0),
            lift: Some(-4.0),
        };
        all[2] = Tuning {
            bearing: Some(-104.5),
            ..Tuning::default()
        };
        let back = parse_state(&render_state(&all));
        assert_eq!(
            back, all,
            "a settled composition did not survive the round trip"
        );

        // An untouched realm must stay untouched, not become a pile of zeroes — writing defaults
        // would turn "never tuned" into "tuned to nothing" on the next load.
        assert!(
            back[3].is_empty(),
            "realm 3 was never tuned and must load empty"
        );
    }

    /// The file is hand-editable and may be hand-broken. A bad line is skipped, not fatal, and
    /// never silently becomes a number: a NaN bearing would swing the camera nowhere at all.
    #[test]
    fn a_damaged_save_file_degrades_to_the_shipped_composition() {
        let parsed = parse_state(
            "# comment\n\
             \n\
             1 BEARING -146.0\n\
             1 BEARING oops\n\
             1 DOLLY NaN\n\
             1 NOSUCHKNOB 3\n\
             notanumber EYE 1\n\
             1 FOCUS 0.5 trailing junk\n\
             1\n",
        );
        assert_eq!(
            parsed[1].bearing,
            Some(-146.0),
            "the good line must survive"
        );
        assert_eq!(parsed[1].dolly, None, "NaN must not become a dolly");
        assert_eq!(parsed[1].eye, None, "a bad realm must not land anywhere");
        assert_eq!(parsed[1].focus, Some(0.5), "trailing junk is ignorable");
    }

    /// The override store is global by design — the framing function reads it with no handle to
    /// pass one through. That makes these tests share state, so they take a lock rather than
    /// racing each other's `reset`.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        static L: OnceLock<Mutex<()>> = OnceLock::new();
        L.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn nothing_is_overridden_until_something_is_nudged() {
        let _g = guard();
        reset(1);
        assert!(tuning(1).is_empty());
        assert!(
            report(1).is_empty(),
            "an untouched realm reports no overrides"
        );
        assert_eq!(tuning(1).get(Knob::Bearing), None);
    }

    /// The first nudge starts from what is on screen, not from zero — otherwise one keypress
    /// teleports the camera instead of adjusting it.
    #[test]
    fn the_first_nudge_starts_from_the_current_value() {
        let _g = guard();
        reset(2);
        let v = nudge(2, Knob::Bearing, 1.0, -104.47);
        assert!(
            (v - (-103.47)).abs() < 1e-4,
            "expected one degree from -104.47, got {v}"
        );
        let v = nudge(2, Knob::Bearing, -3.0, -104.47);
        assert!(
            (v - (-106.47)).abs() < 1e-4,
            "second nudge continues from the first, got {v}"
        );
        reset(2);
    }

    /// Realms must not share a tuning: the stage camera is fixed per realm, so one shared set
    /// would mean dialling in Hibernia silently moved Albion.
    #[test]
    fn realms_tune_independently() {
        let _g = guard();
        reset(1);
        reset(3);
        nudge(1, Knob::Dolly, 5.0, 1.0);
        assert!(
            tuning(3).is_empty(),
            "tuning Albion must not touch Hibernia"
        );
        assert!(tuning(1).dolly.is_some());
        reset(1);
    }

    #[test]
    fn dolly_cannot_be_driven_through_the_subject() {
        let _g = guard();
        reset(1);
        let v = nudge(1, Knob::Dolly, -1000.0, 1.0);
        assert!(v >= 0.10, "dolly clamped above zero, got {v}");
        reset(1);
    }

    /// The lift is a pedestal on the whole camera, so a held key drives it somewhere unrecoverable
    /// by eye — under the stage floor, or above the backdrop dome's rim where the shot is sky.
    /// Realm 0 is the scratch slot: it is not a realm anyone plays, so a test cannot destroy a
    /// settled Albion, Midgard or Hibernia composition someone is part way through solving.
    #[test]
    fn lift_cannot_be_driven_out_of_the_stage() {
        let _g = guard();
        reset(0);
        let up = nudge(0, Knob::Lift, 1000.0, 0.0);
        assert!(up <= 140.0, "lift clamped below the dome rim, got {up}");
        let down = nudge(0, Knob::Lift, -10_000.0, 0.0);
        assert!(down >= -140.0, "lift clamped above the floor, got {down}");
        reset(0);
    }

    #[test]
    fn bearing_stays_in_a_comparable_range() {
        let _g = guard();
        reset(1);
        let v = nudge(1, Knob::Bearing, 400.0, 0.0);
        assert!(
            v > -180.0 && v <= 180.0,
            "bearing wrapped into range, got {v}"
        );
        reset(1);
    }

    #[test]
    fn the_report_is_pasteable_as_environment_variables() {
        let _g = guard();
        reset(1);
        nudge(1, Knob::Bearing, 2.0, -146.42);
        nudge(1, Knob::Eye, 1.0, 0.62);
        let r = report(1);
        assert_eq!(r.len(), 2, "only the touched knobs report: {r:?}");
        assert!(
            r.iter().any(|s| s.starts_with("CAER_SCENE_BEARING=")),
            "{r:?}"
        );
        assert!(r.iter().any(|s| s.starts_with("CAER_SCENE_EYE=")), "{r:?}");
        reset(1);
    }
}
