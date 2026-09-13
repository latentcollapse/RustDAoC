//! REQ-019 allowed-combination denominator — create/career identity join.
//!
//! Mirrors SoloDAoC `CharacterCreateRequestHandler.IsCharacterValid` identity axes
//! (`STARTING_CLASSES_DICT` ∩ `EligibleRaces` ∩ gender locks). Stats/points are a
//! separate joint axis in [`crate::charcreate`].
//!
//! 1124+ wire decode (ignore the stale pre-1124 comment in the oracle handler):
//! `Race = b & 0x1F`, gender = bit 7. This module classifies the *decoded* fields.
//!
//! Not Albion-only, not a race×class Cartesian product, not `Enum.IsDefined`.
//! Unknown table gaps are [`Legality::Unestablished`], never guessed Allowed.

use crate::career::{
    class_career_by_id, class_gender_lock, race_gender_lock, starting_classes_for_realm,
    ClassCareerRow, GENDER_FEMALE, GENDER_MALE, REALM_ALBION, REALM_HIBERNIA, REALM_MIDGARD,
    STARTING_CLASSES_ALBION, STARTING_CLASSES_HIBERNIA, STARTING_CLASSES_MIDGARD,
};

/// Generated character-creation denominator. This contains table rows, not retail assets.
pub const REQ019_CREATE_COMBOS_TSV: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/client_tables/req019_create_combos.tsv"
));

/// Identity-axis legality for one (realm, class, race, gender) tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Legality {
    Allowed,
    Forbidden,
    /// Starting class with no `CharacterClassDB` EligibleRaces row (or equivalent gap).
    /// Must not be treated as Allowed.
    Unestablished,
}

impl Legality {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "ALLOWED",
            Self::Forbidden => "FORBIDDEN",
            Self::Unestablished => "UNESTABLISHED",
        }
    }
}

/// One allowed create combination (denominator row).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CreateCombo {
    pub realm: u8,
    pub class_id: u8,
    pub race: u8,
    pub gender: u8,
}

/// Classify a decoded create tuple against OPEN_ORACLE tables.
#[must_use]
pub fn classify(realm: u8, class_id: u8, race: u8, gender: u8) -> Legality {
    classify_against(
        realm,
        class_id,
        race,
        gender,
        starting_classes_for_realm(realm),
        class_career_by_id(class_id),
    )
}

/// Same join as [`classify`], with injectable tables so a missing EligibleRaces row
/// is observable as [`Legality::Unestablished`] (never guessed Allowed).
#[must_use]
pub fn classify_against(
    realm: u8,
    class_id: u8,
    race: u8,
    gender: u8,
    starting: Option<&[u8]>,
    class_row: Option<&ClassCareerRow>,
) -> Legality {
    if gender > 1 {
        return Legality::Forbidden;
    }
    let Some(starting) = starting else {
        return Legality::Forbidden;
    };
    if !starting.contains(&class_id) {
        return Legality::Forbidden;
    }
    let Some(row) = class_row else {
        return Legality::Unestablished;
    };
    if row.realm != realm {
        return Legality::Forbidden;
    }
    if !row.eligible_races.contains(&race) {
        return Legality::Forbidden;
    }
    if let Some(lock) = race_gender_lock(race) {
        if gender != lock {
            return Legality::Forbidden;
        }
    }
    if let Some(lock) = class_gender_lock(class_id) {
        if gender != lock {
            return Legality::Forbidden;
        }
    }
    Legality::Allowed
}

/// Enumerate every Allowed combination from the oracle tables (the denominator).
#[must_use]
pub fn allowed_combos() -> Vec<CreateCombo> {
    let mut out = Vec::new();
    for (realm, starting) in [
        (REALM_ALBION, STARTING_CLASSES_ALBION),
        (REALM_MIDGARD, STARTING_CLASSES_MIDGARD),
        (REALM_HIBERNIA, STARTING_CLASSES_HIBERNIA),
    ] {
        for &class_id in starting {
            let Some(row) = class_career_by_id(class_id) else {
                continue;
            };
            for &race in row.eligible_races {
                for gender in [GENDER_MALE, GENDER_FEMALE] {
                    if classify_against(realm, class_id, race, gender, Some(starting), Some(row))
                        == Legality::Allowed
                    {
                        out.push(CreateCombo {
                            realm,
                            class_id,
                            race,
                            gender,
                        });
                    }
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Parse the generated TSV (comment lines starting with `#` skipped).
pub fn parse_denominator_tsv(src: &str) -> Result<Vec<CreateCombo>, String> {
    let mut rows = Vec::new();
    let mut header_seen = false;
    for (i, line) in src.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !header_seen {
            if !line.starts_with("realm_id\t") {
                return Err(format!("line {}: expected TSV header, got {line}", i + 1));
            }
            header_seen = true;
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 10 {
            return Err(format!(
                "line {}: expected ≥10 columns, got {}",
                i + 1,
                cols.len()
            ));
        }
        let realm: u8 = cols[0]
            .parse()
            .map_err(|_| format!("line {}: bad realm_id {}", i + 1, cols[0]))?;
        let class_id: u8 = cols[2]
            .parse()
            .map_err(|_| format!("line {}: bad class_id {}", i + 1, cols[2]))?;
        let race: u8 = cols[6]
            .parse()
            .map_err(|_| format!("line {}: bad race_id {}", i + 1, cols[6]))?;
        let gender: u8 = cols[8]
            .parse()
            .map_err(|_| format!("line {}: bad gender_id {}", i + 1, cols[8]))?;
        rows.push(CreateCombo {
            realm,
            class_id,
            race,
            gender,
        });
    }
    rows.sort_unstable();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::career::{class_career_by_id, CLASS_CAREERS, RACES};
    use std::collections::BTreeSet;

    fn keyset(rows: &[CreateCombo]) -> BTreeSet<CreateCombo> {
        rows.iter().copied().collect()
    }

    fn races_for(class_id: u8) -> BTreeSet<u8> {
        allowed_combos()
            .into_iter()
            .filter(|c| c.class_id == class_id)
            .map(|c| c.race)
            .collect()
    }

    fn genders_for(class_id: u8) -> BTreeSet<u8> {
        allowed_combos()
            .into_iter()
            .filter(|c| c.class_id == class_id)
            .map(|c| c.gender)
            .collect()
    }

    fn realms_for(class_id: u8) -> BTreeSet<u8> {
        allowed_combos()
            .into_iter()
            .filter(|c| c.class_id == class_id)
            .map(|c| c.realm)
            .collect()
    }

    #[test]
    fn tsv_matches_enumerated_allowed_set() {
        let enumerated = keyset(&allowed_combos());
        let tsv = keyset(&parse_denominator_tsv(REQ019_CREATE_COMBOS_TSV).expect("parse TSV"));
        let missing_from_tsv: Vec<_> = enumerated.difference(&tsv).copied().collect();
        let illegal_in_tsv: Vec<_> = tsv.difference(&enumerated).copied().collect();
        assert!(
            missing_from_tsv.is_empty(),
            "legal combo missing from TSV: {missing_from_tsv:?}"
        );
        assert!(
            illegal_in_tsv.is_empty(),
            "combination illegal in oracle marked allowed in TSV: {illegal_in_tsv:?}"
        );
        assert_eq!(enumerated.len(), 485);
    }

    #[test]
    fn not_albion_only_not_cartesian_not_enum_is_defined() {
        let rows = allowed_combos();
        assert!(rows.iter().any(|c| c.realm == REALM_ALBION));
        assert!(rows.iter().any(|c| c.realm == REALM_MIDGARD));
        assert!(rows.iter().any(|c| c.realm == REALM_HIBERNIA));

        let starting_n = STARTING_CLASSES_ALBION.len()
            + STARTING_CLASSES_MIDGARD.len()
            + STARTING_CLASSES_HIBERNIA.len();
        let cartesian = starting_n * RACES.len() * 2;
        assert!(
            rows.len() < cartesian,
            "denominator collapsed to Cartesian product ({}/{})",
            rows.len(),
            cartesian
        );

        // Paladin is a defined eCharacterClass but not a Midgard starting class.
        assert!(class_career_by_id(1).is_some());
        assert_eq!(
            classify(REALM_MIDGARD, 1, 5, GENDER_MALE),
            Legality::Forbidden,
            "Enum.IsDefined(Paladin) must not admit Midgard Paladin"
        );
        assert_eq!(
            classify(REALM_ALBION, 55, 9, GENDER_MALE),
            Legality::Forbidden,
            "Animist is Hibernia-only in STARTING_CLASSES_DICT"
        );
    }

    #[test]
    fn named_classes_have_explicit_allowed_rows() {
        // Necromancer — Briton/Inconnu/Saracen, Disciple base, Albion.
        assert_eq!(realms_for(12), BTreeSet::from([REALM_ALBION]));
        assert_eq!(races_for(12), BTreeSet::from([1, 4, 13]));
        assert_eq!(genders_for(12), BTreeSet::from([0, 1]));
        assert_eq!(classify(REALM_ALBION, 12, 1, 0), Legality::Allowed);
        assert_eq!(classify(REALM_ALBION, 12, 9, 0), Legality::Forbidden); // Celt
        assert!(class_career_by_id(12)
            .unwrap()
            .career_keys
            .contains(&"AlbClothCasterCareer"));

        // Animist — Celt/Firbolg/Sylvan, Forester base, Hibernia.
        assert_eq!(realms_for(55), BTreeSet::from([REALM_HIBERNIA]));
        assert_eq!(races_for(55), BTreeSet::from([9, 10, 15]));
        assert_eq!(classify(REALM_HIBERNIA, 55, 9, 1), Legality::Allowed);
        assert_eq!(classify(REALM_ALBION, 55, 9, 1), Legality::Forbidden);

        // Bonedancer — Kobold/Troll/Valkyn, Mystic base, Midgard.
        assert_eq!(realms_for(30), BTreeSet::from([REALM_MIDGARD]));
        assert_eq!(races_for(30), BTreeSet::from([6, 8, 14]));
        assert_eq!(classify(REALM_MIDGARD, 30, 5, 0), Legality::Forbidden); // Norseman

        // Theurgist — Avalonian/Briton/HalfOgre.
        assert_eq!(races_for(5), BTreeSet::from([1, 2, 16]));
        assert_eq!(classify(REALM_ALBION, 5, 16, 1), Legality::Allowed);

        // Minstrel / Bard / Skald (instrument hybrids, three realms).
        assert_eq!(realms_for(4), BTreeSet::from([REALM_ALBION]));
        assert_eq!(races_for(4), BTreeSet::from([1, 3, 4])); // Briton/Highlander/Saracen
        assert_eq!(realms_for(48), BTreeSet::from([REALM_HIBERNIA]));
        assert_eq!(races_for(48), BTreeSet::from([9, 10])); // Celt/Firbolg only
        assert_eq!(classify(REALM_HIBERNIA, 48, 11, 0), Legality::Forbidden); // Elf Bard
        assert_eq!(realms_for(24), BTreeSet::from([REALM_MIDGARD]));
        assert_eq!(races_for(24), BTreeSet::from([5, 6, 7, 8])); // no Valkyn
        assert!(class_career_by_id(4)
            .unwrap()
            .career_keys
            .contains(&"MinstrelCareer"));
        assert!(class_career_by_id(48)
            .unwrap()
            .career_keys
            .contains(&"BardCareer"));
        assert!(class_career_by_id(24)
            .unwrap()
            .career_keys
            .contains(&"SkaldCareer"));

        // Warlock — Frostalf/Kobold/Norseman; no dedicated WarlockCareer row in oracle SQL.
        assert_eq!(races_for(59), BTreeSet::from([5, 8, 17]));
        assert_eq!(
            class_career_by_id(59).unwrap().career_keys,
            &["MidClothCasterCareer"]
        );

        // Vampiir — Celt/Lurikeen/Shar.
        assert_eq!(races_for(58), BTreeSet::from([9, 12, 18]));
        assert!(class_career_by_id(58)
            .unwrap()
            .career_keys
            .contains(&"VampiirCareer"));

        // Valkyrie female-only; Savage both genders.
        assert_eq!(genders_for(34), BTreeSet::from([GENDER_FEMALE]));
        assert_eq!(races_for(34), BTreeSet::from([5, 7, 14, 17]));
        assert_eq!(
            classify(REALM_MIDGARD, 34, 5, GENDER_MALE),
            Legality::Forbidden
        );
        assert_eq!(
            classify(REALM_MIDGARD, 34, 5, GENDER_FEMALE),
            Legality::Allowed
        );
        assert_eq!(races_for(32), BTreeSet::from([5, 6, 7, 8, 14]));
        assert_eq!(genders_for(32), BTreeSet::from([0, 1]));

        // Friar / Heretic.
        assert_eq!(races_for(10), BTreeSet::from([1, 2, 3]));
        assert_eq!(races_for(33), BTreeSet::from([1, 2, 4, 13, 19]));
        assert_eq!(
            classify(REALM_ALBION, 33, 19, GENDER_MALE),
            Legality::Allowed
        );
        assert_eq!(
            classify(REALM_ALBION, 33, 19, GENDER_FEMALE),
            Legality::Forbidden
        );

        // Three-realm Mauler (distinct class ids 60/61/62).
        assert_eq!(realms_for(60), BTreeSet::from([REALM_ALBION]));
        assert_eq!(races_for(60), BTreeSet::from([1, 13, 19]));
        assert_eq!(realms_for(61), BTreeSet::from([REALM_MIDGARD]));
        assert_eq!(races_for(61), BTreeSet::from([5, 8, 20]));
        assert_eq!(realms_for(62), BTreeSet::from([REALM_HIBERNIA]));
        assert_eq!(races_for(62), BTreeSet::from([9, 12, 21]));
        assert_eq!(
            classify(REALM_ALBION, 60, 19, GENDER_FEMALE),
            Legality::Forbidden
        );
        assert_eq!(
            classify(REALM_MIDGARD, 61, 20, GENDER_MALE),
            Legality::Allowed
        );
        assert_eq!(
            classify(REALM_HIBERNIA, 62, 21, GENDER_MALE),
            Legality::Allowed
        );
        assert!(class_career_by_id(60)
            .unwrap()
            .career_keys
            .contains(&"MaulerAlbCareer"));
        assert!(class_career_by_id(61)
            .unwrap()
            .career_keys
            .contains(&"MaulerMidCareer"));
        assert!(class_career_by_id(62)
            .unwrap()
            .career_keys
            .contains(&"MaulerHibCareer"));
    }

    #[test]
    fn falsifier_illegal_oracle_combo_is_not_allowed() {
        // Observation that fails if the join is Cartesian / Enum.IsDefined / Albion-copied.
        let probes = [
            (REALM_ALBION, 12u8, 9u8, 0u8), // Celt Necromancer
            (REALM_HIBERNIA, 12, 1, 0),     // Hibernia Necromancer
            (REALM_MIDGARD, 2, 5, 0),       // Midgard Armsman
            (REALM_MIDGARD, 34, 5, 0),      // Male Valkyrie
            (REALM_HIBERNIA, 39, 9, 0),     // Male Bainshee
            (REALM_ALBION, 60, 19, 1),      // Female Korazh Mauler
            (REALM_ALBION, 1, 19, 0),       // Paladin Korazh (not in EligibleRaces)
        ];
        for (realm, class_id, race, gender) in probes {
            assert_eq!(
                classify(realm, class_id, race, gender),
                Legality::Forbidden,
                "illegal combo marked allowed: realm={realm} class={class_id} race={race} gender={gender}"
            );
            assert!(
                !allowed_combos().iter().any(|c| c.realm == realm
                    && c.class_id == class_id
                    && c.race == race
                    && c.gender == gender),
                "illegal combo present in denominator"
            );
        }
    }

    #[test]
    fn missing_eligible_races_is_unestablished_never_allowed() {
        // Class 2 is in Albion starting list; withhold the DB row.
        let legality = classify_against(
            REALM_ALBION,
            2,
            1,
            GENDER_MALE,
            Some(STARTING_CLASSES_ALBION),
            None,
        );
        assert_eq!(legality, Legality::Unestablished);
        assert_ne!(legality, Legality::Allowed);
    }

    #[test]
    fn catalog_covers_every_starting_class() {
        for &id in STARTING_CLASSES_ALBION
            .iter()
            .chain(STARTING_CLASSES_MIDGARD)
            .chain(STARTING_CLASSES_HIBERNIA)
        {
            assert!(
                class_career_by_id(id).is_some(),
                "starting class {id} has no CharacterClassDB row — would be UNESTABLISHED"
            );
        }
        assert_eq!(CLASS_CAREERS.len(), 62);
    }

    #[test]
    fn race_gender_byte_1124_matches_decoder_contract() {
        // Race = b & 0x1F, gender = bit 7 (CharacterCreateRequestHandler 1124+).
        let packed_female_briton = 1u8 | (1 << 7);
        assert_eq!(packed_female_briton & 0x1F, 1);
        assert_eq!(packed_female_briton >> 7, 1);
        assert_eq!(
            classify(
                REALM_ALBION,
                12,
                packed_female_briton & 0x1F,
                packed_female_briton >> 7
            ),
            Legality::Allowed
        );
    }
}
