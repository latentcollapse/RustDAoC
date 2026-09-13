//! Animation-set resolver: creature/avatar → the `.kfa` clip for a given action (Phase A.3.5).
//!
//! A.3.4 guessed a creature's idle clip from its NIF stem (`skel01` → `skel_CIDLE`). That was a
//! heuristic and it was **wrong** — the client resolves animation through three real tables in
//! `gamedata.mpk`, and they say the skeleton's idle is the *shared humanoid* `I_hm.kfa`, not the
//! skeleton-named clip:
//!
//! ```text
//!   model id ──monsters.csv[NIF#]──▶ nif id ──monnifs.csv[Anim Set]──▶ anim set
//!   anim set ──anims.csv[<action>]───▶ anim-nif id ──animnifs.csv──▶ anims/<name>.kfa + frames/fps/type
//!   anim set ──canims.csv[<action>]──▶ (same) …for the COMBAT actions (attack/parry/combat-idle)
//! ```
//!
//! Two structural facts fall out of this, and they're why the A.3.2b bone-id table matters:
//!
//! 1. **Clips are shared and retargeted.** `I_hm.kfa` ("idle, humanoid") is the idle for the
//!    skeleton (set 4), Briton Male (set 2), and most bipeds alike. A clip keys its tracks by
//!    canonical Biped bone *id*, so one authored clip drives any mesh carrying those ids — which is
//!    exactly what [`crate::nif::CANONICAL_BIPED`] bridges.
//! 2. **The player avatar resolves the same way.** `anims.csv` rows are named per race+gender
//!    ("Briton Male" = set 2, "Troll Female" = 25, …), so an avatar's set comes from a name lookup
//!    rather than a model id — see [`AnimTables::set_for_race_gender`].
//!
//! `animnifs.csv` also carries **frames + fps + loop/clamp**, giving each clip its authored duration
//! and whether it loops — the timing A.3.5 needs to play animation rather than freeze one pose.

use std::collections::HashMap;
use std::io;
use std::path::Path;

/// `anims.csv` column indices (header: `ID, name, Walk, Back, Run, Turn Left, Turn Right, Jump,
/// Land, Fly, Idle, Death, …`). These are the locomotion/idle actions we drive today.
const ANIMS_WALK: usize = 2;
const ANIMS_BACK: usize = 3;
const ANIMS_RUN: usize = 4;
/// `Slide Left` / `Slide Right` — the STRAFE clips (DAoC's Q/E sidestep). Present in 425 of the 431
/// anim sets; `Back` is present in all 431.
const ANIMS_SLIDE_LEFT: usize = 13;
const ANIMS_SLIDE_RIGHT: usize = 14;
const ANIMS_IDLE: usize = 10;
const ANIMS_DEATH: usize = 11;

/// `canims.csv` column indices (header: `ID, name, Att Med, Att Low, Att High, Flinch, C-Idle, …`).
const CANIMS_ATTACK: usize = 2;
const CANIMS_FLINCH: usize = 5;
const CANIMS_COMBAT_IDLE: usize = 6;

/// `animnifs.csv` column indices (header: `NIFS, name, NIF Name, frames, fps, base fps, type, …`).
const NIFS_NAME: usize = 2;
const NIFS_FRAMES: usize = 3;
const NIFS_FPS: usize = 4;
const NIFS_BASE_FPS: usize = 5;
const NIFS_TYPE: usize = 6;

/// A creature/avatar action, resolved through either the base (`anims.csv`) or combat
/// (`canims.csv`) table. The split mirrors the client's own two tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Idle,
    Walk,
    Back,
    Run,
    /// Sidestep left / right — DAoC's Q/E strafe.
    SlideLeft,
    SlideRight,
    Death,
    /// Combat-ready idle (weapon out) — `canims.csv`.
    CombatIdle,
    /// Medium one-handed attack swing — `canims.csv`.
    Attack,
    /// Hit reaction — `canims.csv`.
    Flinch,
}

impl Action {
    /// Which table this action lives in, and at which column: `(is_combat_table, column)`.
    fn slot(self) -> (bool, usize) {
        match self {
            Action::Walk => (false, ANIMS_WALK),
            Action::Back => (false, ANIMS_BACK),
            Action::SlideLeft => (false, ANIMS_SLIDE_LEFT),
            Action::SlideRight => (false, ANIMS_SLIDE_RIGHT),
            Action::Run => (false, ANIMS_RUN),
            Action::Idle => (false, ANIMS_IDLE),
            Action::Death => (false, ANIMS_DEATH),
            Action::Attack => (true, CANIMS_ATTACK),
            Action::Flinch => (true, CANIMS_FLINCH),
            Action::CombatIdle => (true, CANIMS_COMBAT_IDLE),
        }
    }
}

/// A resolved clip reference: which `.kfa` to load and how it's meant to play.
///
/// **The two rates are not the same thing** (RE'd 2026-07-24 from a duration cross-check that
/// initially "failed" on 97/168 clips): `base_fps` is the rate the clip was *authored* at — it's
/// what the `.kfa`'s own key timeline is expressed in, and it's 15 or 30 for 99% of rows. `fps` is
/// the rate the client *plays* it at, and differs in only ~6% of rows: the deliberate slow-downs
/// and speed-ups. `I_hm` (the shared humanoid idle) is the clearest case — 30 frames authored at
/// 15fps (a 2.0s key timeline) but played at 4fps, stretching it into a languid 7.5s idle.
///
/// So: sample the parsed clip over [`Self::authored_duration`], but advance clip time at
/// [`Self::rate_scale`] so it lasts [`Self::playback_duration`] in real seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimClipRef {
    /// Clip file stem, lowercased and without the `.kfa` extension (the `anims/` index key).
    pub stem: String,
    /// Authored frame count.
    pub frames: u32,
    /// Playback rate — the rate the client ticks this clip at.
    pub fps: f32,
    /// Authoring rate — the rate the `.kfa`'s key times are expressed in.
    pub base_fps: f32,
    /// `loop` in the table (vs `clamp`, which plays once and holds the last frame).
    pub looping: bool,
}

impl AnimClipRef {
    /// Length of the parsed `.kfa`'s key timeline in seconds (`frames / base_fps`). This is the
    /// range to sample poses over — it should match the parsed [`crate::nif::Clip::duration`].
    pub fn authored_duration(&self) -> f32 {
        if self.base_fps > 0.0 {
            self.frames as f32 / self.base_fps
        } else {
            0.0
        }
    }

    /// How long one cycle lasts on screen (`frames / fps`) — the authored length after the client's
    /// playback-rate adjustment.
    pub fn playback_duration(&self) -> f32 {
        if self.fps > 0.0 {
            self.frames as f32 / self.fps
        } else {
            0.0
        }
    }

    /// Clip-seconds to advance per real second (`fps / base_fps`); 1.0 when played as authored,
    /// <1 for the slowed idles. `0.0` if either rate is missing, which the caller reads as "hold".
    pub fn rate_scale(&self) -> f32 {
        if self.base_fps > 0.0 && self.fps > 0.0 {
            self.fps / self.base_fps
        } else {
            0.0
        }
    }
}

/// The joined animation tables: anim set + action → clip.
pub struct AnimTables {
    /// anim set id → `anims.csv` row columns.
    base: HashMap<u16, Vec<String>>,
    /// anim set id → `canims.csv` row columns.
    combat: HashMap<u16, Vec<String>>,
    /// anim-nif id → clip reference.
    clips: HashMap<u16, AnimClipRef>,
    /// Normalised `anims.csv` row name ("britonmale") → anim set id. Used for player avatars, whose
    /// set comes from race+gender rather than a creature model id.
    set_by_name: HashMap<String, u16>,
}

impl AnimTables {
    /// Load and join `anims.csv` + `canims.csv` + `animnifs.csv` out of `gamedata.mpk`.
    pub fn load(gamedata_mpk: impl AsRef<Path>) -> io::Result<Self> {
        let members = crate::open(gamedata_mpk)?;
        let find = |name: &str| -> io::Result<String> {
            members
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(name))
                .map(|m| String::from_utf8_lossy(&m.data).into_owned())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("{name} not in gamedata.mpk"),
                    )
                })
        };
        Ok(Self::from_tables(
            &find("anims.csv")?,
            &find("canims.csv")?,
            &find("animnifs.csv")?,
        ))
    }

    /// Build from the three CSV bodies (split out so it's unit-testable without an mpk).
    pub fn from_tables(anims: &str, canims: &str, animnifs: &str) -> Self {
        let own = |csv: &str| -> HashMap<u16, Vec<String>> {
            numeric_rows(csv)
                .map(|(id, cols)| (id, cols.iter().map(|s| s.trim().to_string()).collect()))
                .collect()
        };

        // animnifs: anim-nif id → clip file + authored timing. Rows with no file name are dead slots.
        let mut clips = HashMap::new();
        for (id, cols) in numeric_rows(animnifs) {
            let name = cols.get(NIFS_NAME).map(|s| s.trim()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let stem = name
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(name)
                .to_ascii_lowercase();
            let stem = stem.strip_suffix(".kfa").unwrap_or(&stem).to_string();
            let frames = cols
                .get(NIFS_FRAMES)
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0);
            let fps = cols
                .get(NIFS_FPS)
                .and_then(|s| s.trim().parse::<f32>().ok())
                .unwrap_or(0.0);
            // A missing/zero base rate falls back to the playback rate (i.e. "authored as played").
            let base_fps = cols
                .get(NIFS_BASE_FPS)
                .and_then(|s| s.trim().parse::<f32>().ok())
                .filter(|v| *v > 0.0)
                .unwrap_or(fps);
            // The `type` column is `loop` or `clamp`; anything else is treated as play-once.
            let looping = cols
                .get(NIFS_TYPE)
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("loop"));
            clips.insert(
                id,
                AnimClipRef {
                    stem,
                    frames,
                    fps,
                    base_fps,
                    looping,
                },
            );
        }

        // anims.csv row names double as the player-avatar key ("Briton Male" → set 2). The naming
        // isn't uniform, so keys are registered in three passes of decreasing directness:
        //   1. the row name as written                    "Briton Male"
        //   2. slash-shared rows, split per race          "Celt/Elf Male" → Celt Male, Elf Male
        //   3. remodel rows with their era prefix dropped "Cata Avalonian Male" → Avalonian Male
        // Passes 2/3 only fill gaps (`or_insert`), so a race with a plain row always wins. Without
        // this, 11 of the 36 race+gender combos resolve to nothing and stay T-posed.
        let base = own(anims);
        let mut set_by_name: HashMap<String, u16> = HashMap::new();
        let mut rows: Vec<(&u16, &Vec<String>)> = base.iter().collect();
        rows.sort_by_key(|(id, _)| **id); // lowest id wins on any tie
        let mut register = |key: String, id: u16| {
            set_by_name.entry(key).or_insert(id);
        };
        for pass in 0..3 {
            for (id, cols) in &rows {
                let Some(name) = cols.get(1).filter(|n| !n.is_empty()) else {
                    continue;
                };
                let Some((body, gender)) = split_gender(name) else {
                    continue;
                };
                match pass {
                    0 => register(normalise(&format!("{body} {gender}")), **id),
                    1 => {
                        for part in body.split('/') {
                            register(normalise(&format!("{part} {gender}")), **id);
                        }
                    }
                    _ => {
                        let stripped = strip_era_prefix(body);
                        if stripped != body {
                            for part in stripped.split('/') {
                                register(normalise(&format!("{part} {gender}")), **id);
                            }
                        }
                    }
                }
            }
        }

        Self {
            base,
            combat: own(canims),
            clips,
            set_by_name,
        }
    }

    /// Find a clip row by its file stem. The tables are keyed by anim-nif id, so a caller holding
    /// only a name — a diagnostic override, or a clip named in a capture manifest — has no way in
    /// otherwise, and inventing a rate for it silently mis-times every sample past t=0.
    #[must_use]
    pub fn clip_by_stem(&self, stem: &str) -> Option<&AnimClipRef> {
        self.clips
            .values()
            .find(|c| c.stem.eq_ignore_ascii_case(stem))
    }

    /// Resolve `(anim set, action)` → the clip to play. `None` when the set has no entry for that
    /// action (id 0 / blank = "this creature doesn't do that"), or the id names no clip file.
    pub fn clip(&self, set: u16, action: Action) -> Option<&AnimClipRef> {
        let (is_combat, col) = action.slot();
        let row = if is_combat {
            self.combat.get(&set)?
        } else {
            self.base.get(&set)?
        };
        let id: u16 = row.get(col)?.parse().ok()?;
        if id == 0 {
            return None;
        }
        self.clips.get(&id)
    }

    /// The anim set for a player race + gender, via the `anims.csv` row name ("Briton Male").
    /// `race_name` is the display name from the race table; gender is `"Male"`/`"Female"`.
    pub fn set_for_race_gender(&self, race_name: &str, gender_word: &str) -> Option<u16> {
        self.set_by_name
            .get(&normalise(&format!("{race_name} {gender_word}")))
            .copied()
    }

    /// How many anim sets carry a base row (diagnostics).
    pub fn len(&self) -> usize {
        self.base.len()
    }

    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
    }
}

/// Split an `anims.csv` row name into `(body, gender)` when it ends in Male/Female
/// ("Celt/Elf Female" → `("Celt/Elf", "Female")`). `None` for creature rows with no gender.
fn split_gender(name: &str) -> Option<(&str, &str)> {
    for g in ["Female", "Male"] {
        if let Some(body) = name.strip_suffix(g) {
            let body = body.trim();
            if !body.is_empty() {
                return Some((body, g));
            }
        }
    }
    None
}

/// Drop the leading era/variant words the client uses for remodel rows, so
/// "Cata Avalonian" → "Avalonian" and "Classic Vamp Lurikeen" → "Lurikeen". Only used to fill gaps
/// for races that have no plain row of their own.
fn strip_era_prefix(body: &str) -> &str {
    let mut out = body;
    loop {
        let trimmed = ["Cata", "New", "Classic", "Vamp"]
            .iter()
            .find_map(|p| out.strip_prefix(p).map(str::trim_start))
            .filter(|s| !s.is_empty());
        match trimmed {
            Some(t) => out = t,
            None => return out,
        }
    }
}

/// Case/space-insensitive key ("Briton Male" == "britonmale").
fn normalise(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Iterate a CSV's data rows whose first column is a `u16`, yielding `(id, columns)`. Header rows
/// (non-numeric first column) are skipped; fields are split on commas (DAoC's tables don't quote).
fn numeric_rows(csv: &str) -> impl Iterator<Item = (u16, Vec<&str>)> {
    csv.lines().filter_map(|line| {
        let cols: Vec<&str> = line.split(',').collect();
        let id = cols.first()?.trim().parse::<u16>().ok()?;
        Some((id, cols))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Stand-ins shaped like the real tables (two header rows + data), using the real skeleton
    // (set 4) and Briton Male (set 2) values verified against the shipped client.
    const ANIMS: &str = ",,0,1,2,3,4,5,6,7,8,9\n\
        ID,name,Walk,Back,Run,Turn Left,Turn Right,Jump,Land,Fly,Idle,Death\n\
        2,Briton Male,380,332,381,336,337,340,36,340,329,386\n\
        4,Skeleton,96,96,97,336,337,340,36,340,329,224\n\
        6,Briton Female,380,332,381,336,337,340,36,340,329,386\n";
    const CANIMS: &str = ",,0,1,2,3,4\n\
        ID,name,Att Med,Att Low,Att High,Flinch,C-Idle\n\
        2,Briton Male,384,378,379,385,383\n\
        4,Skeleton,384,378,379,225,214\n";
    const ANIMNIFS: &str = "Anim,,,,,base, \n\
        NIFS,name,NIF Name,frames,fps, fps,type\n\
        96,skel walk,skel_walk.kfa,18,15,15,loop\n\
        97,skel run,skel_run.kfa,15,15,15,loop\n\
        214,rd cidle,RD_cidle.kfa,30,15,15,loop\n\
        224,rd death,RD_death.kfa,14,15,15,clamp\n\
        329,idle hm,I_hm.kfa,30,4,15,loop\n\
        380,walk hm,W_hm.kfa,18,15,15,loop\n\
        999,dead slot,,0,0,0,\n";

    fn tables() -> AnimTables {
        AnimTables::from_tables(ANIMS, CANIMS, ANIMNIFS)
    }

    #[test]
    fn resolves_the_shared_humanoid_idle_for_both_skeleton_and_briton() {
        let t = tables();
        // The headline finding: BOTH resolve idle to the same shared clip, retargeted by bone id.
        // This is what falsifies the A.3.4 "<stem>_cidle" name heuristic.
        let skel = t.clip(4, Action::Idle).unwrap();
        let briton = t.clip(2, Action::Idle).unwrap();
        assert_eq!(skel.stem, "i_hm");
        assert_eq!(briton.stem, "i_hm");
        assert!(skel.looping);
        assert_eq!(skel.frames, 30);
    }

    #[test]
    fn authored_and_playback_rates_are_distinguished() {
        let t = tables();
        // I_hm: 30 frames authored at 15fps (a 2.0s key timeline) but PLAYED at 4fps → 7.5s on
        // screen. Conflating the two is what made the first duration cross-check fail; the parsed
        // .kfa timeline matches the authored rate, not the playback rate.
        let idle = t.clip(4, Action::Idle).unwrap();
        assert!((idle.authored_duration() - 2.0).abs() < 1e-6);
        assert!((idle.playback_duration() - 7.5).abs() < 1e-6);
        assert!((idle.rate_scale() - 4.0 / 15.0).abs() < 1e-6);
        // A clip authored and played at the same rate scales 1:1.
        let walk = t.clip(4, Action::Walk).unwrap();
        assert!((walk.rate_scale() - 1.0).abs() < 1e-6);
        assert!((walk.authored_duration() - walk.playback_duration()).abs() < 1e-6);
    }

    #[test]
    fn combat_and_base_tables_are_kept_separate() {
        let t = tables();
        // Skeleton's combat idle is RD_cidle — a different clip from its base idle.
        assert_eq!(t.clip(4, Action::CombatIdle).unwrap().stem, "rd_cidle");
        assert_eq!(t.clip(4, Action::Idle).unwrap().stem, "i_hm");
        // Creature-specific locomotion still resolves per-set.
        assert_eq!(t.clip(4, Action::Walk).unwrap().stem, "skel_walk");
        assert_eq!(t.clip(4, Action::Run).unwrap().stem, "skel_run");
        assert_eq!(t.clip(2, Action::Walk).unwrap().stem, "w_hm");
    }

    #[test]
    fn clamp_clips_are_not_marked_looping() {
        let t = tables();
        let death = t.clip(4, Action::Death).unwrap();
        assert_eq!(death.stem, "rd_death");
        assert!(!death.looping, "a `clamp` clip plays once and holds");
    }

    #[test]
    fn missing_sets_actions_and_dead_slots_resolve_to_none() {
        let t = tables();
        assert!(t.clip(999, Action::Idle).is_none(), "unknown anim set");
        assert!(
            t.clip(6, Action::CombatIdle).is_none(),
            "set with no canims row"
        );
        // An anim-nif row with an empty file name is a dead slot, not a clip.
        assert!(!t.clips.contains_key(&999));
    }

    #[test]
    fn shared_and_remodel_rows_fill_gaps_without_beating_plain_rows() {
        // The real table's three naming shapes: a plain row, a slash-shared row (Celt/Elf share one
        // anim set), and remodel rows with an era prefix. 11 of 36 race+genders resolve ONLY via the
        // latter two, so all three must register — but a plain row must always win.
        let anims = ",,0,1,2,3,4,5,6,7,8,9\n\
            ID,name,Walk,Back,Run,Turn Left,Turn Right,Jump,Land,Fly,Idle,Death\n\
            2,Briton Male,380,332,381,336,337,340,36,340,329,386\n\
            48,Celt/Elf Male,380,332,381,336,337,340,36,340,329,386\n\
            241,Cata Avalonian Male,380,332,381,336,337,340,36,340,329,386\n\
            280,Classic Vamp Briton Male,380,332,381,336,337,340,36,340,329,386\n";
        let t = AnimTables::from_tables(anims, CANIMS, ANIMNIFS);
        assert_eq!(
            t.set_for_race_gender("Briton", "Male"),
            Some(2),
            "plain row"
        );
        // Both halves of a slash-shared row resolve to it.
        assert_eq!(t.set_for_race_gender("Celt", "Male"), Some(48));
        assert_eq!(t.set_for_race_gender("Elf", "Male"), Some(48));
        // A race with only a remodel row resolves through the stripped prefix.
        assert_eq!(t.set_for_race_gender("Avalonian", "Male"), Some(241));
        // ...but the prefixed row must NOT displace Briton's plain row 2.
        assert_eq!(t.set_for_race_gender("Briton", "Male"), Some(2));
        assert_eq!(t.set_for_race_gender("Nonesuch", "Male"), None);
    }

    #[test]
    fn gender_split_and_era_prefix_stripping() {
        assert_eq!(
            split_gender("Celt/Elf Female"),
            Some(("Celt/Elf", "Female"))
        );
        assert_eq!(split_gender("Briton Male"), Some(("Briton", "Male")));
        assert_eq!(
            split_gender("Skeleton"),
            None,
            "creature rows carry no gender"
        );
        assert_eq!(
            split_gender("Female"),
            None,
            "a bare gender is not a race row"
        );
        assert_eq!(strip_era_prefix("Cata Avalonian"), "Avalonian");
        assert_eq!(strip_era_prefix("Classic Vamp Lurikeen"), "Lurikeen");
        assert_eq!(
            strip_era_prefix("Briton"),
            "Briton",
            "unprefixed is unchanged"
        );
    }

    #[test]
    fn player_avatar_set_resolves_by_race_and_gender_name() {
        let t = tables();
        assert_eq!(t.set_for_race_gender("Briton", "Male"), Some(2));
        assert_eq!(t.set_for_race_gender("Briton", "Female"), Some(6));
        // Space/case-insensitive, so "Half Ogre" style names match.
        assert_eq!(t.set_for_race_gender("briton", "male"), Some(2));
        assert_eq!(t.set_for_race_gender("Nonesuch", "Male"), None);
    }
}
