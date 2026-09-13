//! Client name-fragment tables (`charman/names.dat`) — the source for the creation form's
//! **Random** button (`character_creation.xml` ButtonDef ControlId 1013).
//!
//! The file is INI-shaped: 57 sections keyed `[<race>-first]`, `[<race>-second]`, `[<race>-third]`,
//! each holding name *fragments* rather than whole names. A generated name is one fragment drawn
//! from each of the three parts, concatenated.
//!
//! 19 race keys cover the 21 playable races: the three realm Minotaurs share `minotaur`, and
//! Norseman is keyed `viking` (the Midgard base-class name).
//!
//! Provenance: `OWN_CAPTURE`, client data file. Nothing here is invented — when the file is
//! missing, [`generate`] returns `None` and the Random button does nothing rather than producing a
//! name the client would never have made.

use std::collections::HashMap;
use std::path::Path;

/// Longest name the create form's edit box accepts (`character_creation.xml` EditBoxDef 1051).
pub const MAX_NAME_LEN: usize = 20;

/// Fragment pools for one race: first, second and third syllable sets.
#[derive(Debug, Clone, Default)]
pub struct RaceFragments {
    pub first: Vec<String>,
    pub second: Vec<String>,
    pub third: Vec<String>,
}

impl RaceFragments {
    /// Whether all three pools are populated — a name cannot be composed otherwise.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.first.is_empty() && !self.second.is_empty() && !self.third.is_empty()
    }
}

/// Every race's fragment pools, keyed by the section name used in `names.dat`.
#[derive(Debug, Clone, Default)]
pub struct NameTable {
    races: HashMap<String, RaceFragments>,
}

/// `eRace` id → `names.dat` section key.
///
/// `None` for an unknown race, so callers fail closed instead of defaulting to another race's
/// name style.
#[must_use]
pub fn section_for_race(race_id: u8) -> Option<&'static str> {
    Some(match race_id {
        1 => "briton",
        2 => "avalon",
        3 => "highlander",
        4 => "saracen",
        5 => "viking", // Norseman — the file keys it by the Midgard base-class name
        6 => "troll",
        7 => "dwarf",
        8 => "kobold",
        9 => "celt",
        10 => "firbolg",
        11 => "elf",
        12 => "lurikeen",
        13 => "inconnu",
        14 => "valkyn",
        15 => "sylvan",
        16 => "halfogre",
        17 => "frostalf",
        18 => "shar",
        19..=21 => "minotaur", // all three realm Minotaurs share one pool
        _ => return None,
    })
}

impl NameTable {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.races.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.races.len()
    }

    /// Fragment pools for a race id.
    #[must_use]
    pub fn for_race(&self, race_id: u8) -> Option<&RaceFragments> {
        self.races.get(section_for_race(race_id)?)
    }

    /// Compose one name for `race_id`, drawing a fragment from each pool using `pick`.
    ///
    /// `pick(n)` must return an index in `0..n`; the caller owns randomness so this stays
    /// deterministic under test. Returns `None` when the race is unknown or its pools are
    /// incomplete — never a partially-composed or invented name.
    #[must_use]
    pub fn generate(&self, race_id: u8, mut pick: impl FnMut(usize) -> usize) -> Option<String> {
        let f = self.for_race(race_id)?;
        if !f.is_complete() {
            return None;
        }
        let mut name = String::new();
        for pool in [&f.first, &f.second, &f.third] {
            let idx = pick(pool.len()).min(pool.len() - 1);
            name.push_str(&pool[idx]);
        }
        if name.is_empty() || name.len() > MAX_NAME_LEN {
            return None;
        }
        // The form's own rule: ASCII alphanumeric only.
        if !name.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        // Fragment pools are stored with mixed case (`Ab` + `ae` + `bwyn`); retail shows an
        // initial capital and lowercase tail.
        let mut chars = name.chars();
        let head = chars.next()?.to_ascii_uppercase();
        Some(
            std::iter::once(head)
                .chain(chars.flat_map(char::to_lowercase))
                .collect(),
        )
    }
}

/// Parse `charman/names.dat` from a client tree.
///
/// Returns an empty table when the file is absent or unreadable — the Random button then does
/// nothing, which is the honest degradation.
#[must_use]
pub fn load(client_root: &Path) -> NameTable {
    let path = client_root.join("charman").join("names.dat");
    let Ok(raw) = std::fs::read(&path) else {
        log::info!(
            "caer-assets: no names.dat at {} — random names unavailable",
            path.display()
        );
        return NameTable::default();
    };
    parse(&String::from_utf8_lossy(&raw))
}

/// Parse the INI-shaped fragment file.
#[must_use]
pub fn parse(text: &str) -> NameTable {
    let mut table = NameTable::default();
    let mut current: Option<(String, &'static str)> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = inner.rsplit_once('-').and_then(|(race, part)| {
                let part = match part {
                    "first" => "first",
                    "second" => "second",
                    "third" => "third",
                    _ => return None,
                };
                Some((race.to_ascii_lowercase(), part))
            });
            continue;
        }
        let Some((race, part)) = current.as_ref() else {
            continue;
        };
        if !line.chars().all(|c| c.is_ascii_alphanumeric()) {
            continue;
        }
        let entry = table.races.entry(race.clone()).or_default();
        match *part {
            "first" => entry.first.push(line.to_string()),
            "second" => entry.second.push(line.to_string()),
            _ => entry.third.push(line.to_string()),
        }
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "[briton-first]\nAb\nAc\n[briton-second]\nae\n[briton-third]\nbwyn\n";

    #[test]
    fn parses_sections_into_pools() {
        let t = parse(SAMPLE);
        let f = t.for_race(1).expect("briton");
        assert_eq!(f.first, ["Ab", "Ac"]);
        assert_eq!(f.second, ["ae"]);
        assert_eq!(f.third, ["bwyn"]);
        assert!(f.is_complete());
    }

    #[test]
    fn composes_first_second_third_with_retail_casing() {
        let t = parse(SAMPLE);
        assert_eq!(t.generate(1, |_| 0).as_deref(), Some("Abaebwyn"));
        assert_eq!(t.generate(1, |_| 1).as_deref(), Some("Acaebwyn"));
    }

    /// Every playable race must map to a section, or Random silently does nothing for it.
    #[test]
    fn every_race_id_maps_to_a_section() {
        for id in 1..=21u8 {
            assert!(
                section_for_race(id).is_some(),
                "race {id} has no name section"
            );
        }
        assert_eq!(section_for_race(0), None);
        assert_eq!(section_for_race(99), None);
        // The three Minotaurs deliberately share one pool.
        assert_eq!(section_for_race(19), section_for_race(20));
        assert_eq!(section_for_race(20), section_for_race(21));
        // Norseman is keyed by the Midgard base-class name in the client file.
        assert_eq!(section_for_race(5), Some("viking"));
    }

    /// Incomplete or unknown input must yield nothing rather than a partial name.
    #[test]
    fn incomplete_pools_fail_closed() {
        let t = parse("[briton-first]\nAb\n");
        assert!(!t.for_race(1).expect("briton").is_complete());
        assert_eq!(t.generate(1, |_| 0), None, "missing pools must not compose");
        assert_eq!(t.generate(99, |_| 0), None, "unknown race");
        assert_eq!(NameTable::default().generate(1, |_| 0), None, "empty table");
        assert!(load(Path::new("/nonexistent")).is_empty());
    }

    /// An out-of-range index from the caller's RNG must be clamped, not panic.
    #[test]
    fn out_of_range_pick_is_clamped() {
        let t = parse(SAMPLE);
        assert!(t.generate(1, |_| usize::MAX).is_some());
    }

    /// Non-alphanumeric junk lines are ignored rather than becoming fragments.
    #[test]
    fn junk_lines_are_skipped() {
        let t =
            parse("[briton-first]\nAb\n!!!\nwith space\n[briton-second]\nae\n[briton-third]\nb\n");
        assert_eq!(t.for_race(1).expect("briton").first, ["Ab"]);
    }
}
