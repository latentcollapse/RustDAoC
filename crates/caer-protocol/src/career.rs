//! OPEN_ORACLE identity / career tables for REQ-019 part 1 (char-create gating).
//!
//! Provenance:
//! - `eRace` / `eCharacterClass`: SoloDAoC `GameServer/GlobalConstants.cs` (~823 / ~868)
//! - Starting-class gate: `STARTING_CLASSES_DICT` (same file ~1925) — **not** `Enum.IsDefined`
//! - Eligible races / base class: `CharacterClassDB.cs` `EligibleRaces` / `BaseClassID`
//! - Gender locks: `RACE_GENDER_CONSTRAINTS_DICT` / `CLASS_GENDER_CONSTRAINTS_DICT`
//! - Race→realm: `PlayerRace.cs`
//! - Career spec keys: `career_specs.sql` `ClassXSpecialization` (career-named specs only)
//!
//! Unknown legality is [`crate::create_validity::Legality::Unestablished`], never guessed.

/// Player realm ids used on the create wire (`eRealm`).
pub const REALM_ALBION: u8 = 1;
pub const REALM_MIDGARD: u8 = 2;
pub const REALM_HIBERNIA: u8 = 3;

/// Create-wire gender: 0 male / 1 female (bit 7 of the 1124+ race/gender byte).
pub const GENDER_MALE: u8 = 0;
pub const GENDER_FEMALE: u8 = 1;

/// One playable race as recorded by OPEN_ORACLE `PlayerRace` + `eRace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RaceRow {
    pub id: u8,
    pub name: &'static str,
    pub realm: u8,
}

/// All `PlayerRace` entries (Korazh/Deifrang/Graoch share Minotaur ids 19/20/21).
pub const RACES: &[RaceRow] = &[
    RaceRow {
        id: 1,
        name: "Briton",
        realm: 1,
    },
    RaceRow {
        id: 2,
        name: "Avalonian",
        realm: 1,
    },
    RaceRow {
        id: 3,
        name: "Highlander",
        realm: 1,
    },
    RaceRow {
        id: 4,
        name: "Saracen",
        realm: 1,
    },
    RaceRow {
        id: 13,
        name: "Inconnu",
        realm: 1,
    },
    RaceRow {
        id: 16,
        name: "HalfOgre",
        realm: 1,
    },
    RaceRow {
        id: 19,
        name: "AlbionMinotaur",
        realm: 1,
    },
    RaceRow {
        id: 5,
        name: "Norseman",
        realm: 2,
    },
    RaceRow {
        id: 6,
        name: "Troll",
        realm: 2,
    },
    RaceRow {
        id: 7,
        name: "Dwarf",
        realm: 2,
    },
    RaceRow {
        id: 8,
        name: "Kobold",
        realm: 2,
    },
    RaceRow {
        id: 14,
        name: "Valkyn",
        realm: 2,
    },
    RaceRow {
        id: 17,
        name: "Frostalf",
        realm: 2,
    },
    RaceRow {
        id: 20,
        name: "MidgardMinotaur",
        realm: 2,
    },
    RaceRow {
        id: 9,
        name: "Celt",
        realm: 3,
    },
    RaceRow {
        id: 10,
        name: "Firbolg",
        realm: 3,
    },
    RaceRow {
        id: 11,
        name: "Elf",
        realm: 3,
    },
    RaceRow {
        id: 12,
        name: "Lurikeen",
        realm: 3,
    },
    RaceRow {
        id: 15,
        name: "Sylvan",
        realm: 3,
    },
    RaceRow {
        id: 18,
        name: "Shar",
        realm: 3,
    },
    RaceRow {
        id: 21,
        name: "HiberniaMinotaur",
        realm: 3,
    },
];

/// Minotaur races are male-only (`RACE_GENDER_CONSTRAINTS_DICT`).
pub const RACE_GENDER_LOCKS: &[(u8, u8)] = &[
    (19, GENDER_MALE), // AlbionMinotaur / Korazh
    (20, GENDER_MALE), // MidgardMinotaur / Deifrang
    (21, GENDER_MALE), // HiberniaMinotaur / Graoch
];

/// Valkyrie and Bainshee are female-only (`CLASS_GENDER_CONSTRAINTS_DICT`).
pub const CLASS_GENDER_LOCKS: &[(u8, u8)] = &[
    (34, GENDER_FEMALE), // Valkyrie
    (39, GENDER_FEMALE), // Bainshee
];

/// `STARTING_CLASSES_DICT[Albion]` unique class ids (pre-1.93 base ∪ post-1.93 advanced).
pub const STARTING_CLASSES_ALBION: &[u8] = &[
    14, 16, 18, 15, 17, 20, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 19, 33, 60,
];

/// `STARTING_CLASSES_DICT[Midgard]` unique class ids (pre-1.93 base ∪ post-1.93 advanced).
pub const STARTING_CLASSES_MIDGARD: &[u8] = &[
    35, 36, 37, 38, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 34, 59, 61,
];

/// `STARTING_CLASSES_DICT[Hibernia]` unique class ids (pre-1.93 base ∪ post-1.93 advanced).
pub const STARTING_CLASSES_HIBERNIA: &[u8] = &[
    52, 54, 53, 51, 57, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 55, 56, 58, 62,
];

/// One `CharacterClassDB` row plus career-spec keys from `career_specs.sql`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassCareerRow {
    pub class_id: u8,
    pub name: &'static str,
    /// Realm whose `STARTING_CLASSES_DICT` lists this class.
    pub realm: u8,
    pub base_class_id: u8,
    pub eligible_races: &'static [u8],
    pub career_keys: &'static [&'static str],
}

/// Every `CharacterClassDB` playable row (ids 1..=62). Missing EligibleRaces ⇒ do not invent.
pub const CLASS_CAREERS: &[ClassCareerRow] = &[
    ClassCareerRow {
        class_id: 1,
        name: "Paladin",
        realm: 1,
        base_class_id: 14,
        eligible_races: &[2, 1, 3, 4],
        career_keys: &["FighterCareer", "CharacterStyleUserCareer", "PaladinCareer"],
    },
    ClassCareerRow {
        class_id: 2,
        name: "Armsman",
        realm: 1,
        base_class_id: 14,
        eligible_races: &[19, 2, 1, 16, 3, 13, 4],
        career_keys: &[
            "FighterCareer",
            "CharacterStyleUserCareer",
            "PureTankCareer",
            "ArmsmanCareer",
        ],
    },
    ClassCareerRow {
        class_id: 3,
        name: "Scout",
        realm: 1,
        base_class_id: 17,
        eligible_races: &[1, 3, 13, 4],
        career_keys: &["RogueCareer", "CharacterStyleUserCareer", "ScoutCareer"],
    },
    ClassCareerRow {
        class_id: 4,
        name: "Minstrel",
        realm: 1,
        base_class_id: 17,
        eligible_races: &[1, 3, 4],
        career_keys: &["RogueCareer", "CharacterStyleUserCareer", "MinstrelCareer"],
    },
    ClassCareerRow {
        class_id: 5,
        name: "Theurgist",
        realm: 1,
        base_class_id: 15,
        eligible_races: &[2, 1, 16],
        career_keys: &["AlbClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 6,
        name: "Cleric",
        realm: 1,
        base_class_id: 16,
        eligible_races: &[2, 1, 3],
        career_keys: &["AcolyteCareer", "ClericCareer"],
    },
    ClassCareerRow {
        class_id: 7,
        name: "Wizard",
        realm: 1,
        base_class_id: 15,
        eligible_races: &[2, 1, 16],
        career_keys: &["AlbClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 8,
        name: "Sorcerer",
        realm: 1,
        base_class_id: 18,
        eligible_races: &[2, 1, 16, 13, 4],
        career_keys: &["AlbClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 9,
        name: "Infiltrator",
        realm: 1,
        base_class_id: 17,
        eligible_races: &[1, 3, 13, 4],
        career_keys: &[
            "RogueCareer",
            "AssassinCareer",
            "InfiltratorCareer",
            "CharacterStyleUserCareer",
        ],
    },
    ClassCareerRow {
        class_id: 10,
        name: "Friar",
        realm: 1,
        base_class_id: 16,
        eligible_races: &[2, 1, 3],
        career_keys: &["AcolyteCareer", "CharacterStyleUserCareer", "FriarCareer"],
    },
    ClassCareerRow {
        class_id: 11,
        name: "Mercenary",
        realm: 1,
        base_class_id: 14,
        eligible_races: &[19, 2, 1, 16, 3, 13, 4],
        career_keys: &[
            "FighterCareer",
            "CharacterStyleUserCareer",
            "LightTankCareer",
            "MercenaryCareer",
        ],
    },
    ClassCareerRow {
        class_id: 12,
        name: "Necromancer",
        realm: 1,
        base_class_id: 20,
        eligible_races: &[1, 13, 4],
        career_keys: &["AlbClothCasterCareer"],
    },
    ClassCareerRow {
        class_id: 13,
        name: "Cabalist",
        realm: 1,
        base_class_id: 18,
        eligible_races: &[2, 1, 16, 13, 4],
        career_keys: &["AlbClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 14,
        name: "Fighter",
        realm: 1,
        base_class_id: 14,
        eligible_races: &[19, 2, 1, 16, 3, 13, 4],
        career_keys: &["FighterCareer"],
    },
    ClassCareerRow {
        class_id: 15,
        name: "Elementalist",
        realm: 1,
        base_class_id: 15,
        eligible_races: &[2, 1, 16],
        career_keys: &["AlbClothCasterCareer"],
    },
    ClassCareerRow {
        class_id: 16,
        name: "Acolyte",
        realm: 1,
        base_class_id: 16,
        eligible_races: &[19, 2, 1, 3, 13],
        career_keys: &["AcolyteCareer"],
    },
    ClassCareerRow {
        class_id: 17,
        name: "Rogue",
        realm: 1,
        base_class_id: 17,
        eligible_races: &[1, 3, 13, 4],
        career_keys: &["RogueCareer"],
    },
    ClassCareerRow {
        class_id: 18,
        name: "Mage",
        realm: 1,
        base_class_id: 18,
        eligible_races: &[2, 1, 16, 13, 4],
        career_keys: &["AlbClothCasterCareer"],
    },
    ClassCareerRow {
        class_id: 19,
        name: "Reaver",
        realm: 1,
        base_class_id: 14,
        eligible_races: &[1, 13, 4],
        career_keys: &["FighterCareer", "CharacterStyleUserCareer", "ReaverCareer"],
    },
    ClassCareerRow {
        class_id: 20,
        name: "Disciple",
        realm: 1,
        base_class_id: 20,
        eligible_races: &[1, 13, 4],
        career_keys: &["AlbClothCasterCareer"],
    },
    ClassCareerRow {
        class_id: 21,
        name: "Thane",
        realm: 2,
        base_class_id: 35,
        eligible_races: &[7, 17, 20, 5, 6],
        career_keys: &["VikingCareer", "CharacterStyleUserCareer", "ThaneCareer"],
    },
    ClassCareerRow {
        class_id: 22,
        name: "Warrior",
        realm: 2,
        base_class_id: 35,
        eligible_races: &[7, 8, 20, 5, 6, 14],
        career_keys: &[
            "VikingCareer",
            "CharacterStyleUserCareer",
            "PureTankCareer",
            "WarriorCareer",
        ],
    },
    ClassCareerRow {
        class_id: 23,
        name: "Shadowblade",
        realm: 2,
        base_class_id: 38,
        eligible_races: &[7, 17, 8, 5, 14],
        career_keys: &[
            "MidgardRogueCareer",
            "AssassinCareer",
            "ShadowbladeCareer",
            "CharacterStyleUserCareer",
        ],
    },
    ClassCareerRow {
        class_id: 24,
        name: "Skald",
        realm: 2,
        base_class_id: 35,
        eligible_races: &[7, 8, 5, 6],
        career_keys: &["VikingCareer", "CharacterStyleUserCareer", "SkaldCareer"],
    },
    ClassCareerRow {
        class_id: 25,
        name: "Hunter",
        realm: 2,
        base_class_id: 38,
        eligible_races: &[7, 17, 8, 5, 14],
        career_keys: &[
            "MidgardRogueCareer",
            "CharacterStyleUserCareer",
            "HunterCareer",
        ],
    },
    ClassCareerRow {
        class_id: 26,
        name: "Healer",
        realm: 2,
        base_class_id: 37,
        eligible_races: &[7, 17, 5],
        career_keys: &["SeerCareer"],
    },
    ClassCareerRow {
        class_id: 27,
        name: "Spiritmaster",
        realm: 2,
        base_class_id: 36,
        eligible_races: &[17, 8, 5],
        career_keys: &["MidClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 28,
        name: "Shaman",
        realm: 2,
        base_class_id: 37,
        eligible_races: &[7, 17, 8, 6],
        career_keys: &["SeerCareer"],
    },
    ClassCareerRow {
        class_id: 29,
        name: "Runemaster",
        realm: 2,
        base_class_id: 36,
        eligible_races: &[7, 17, 8, 5],
        career_keys: &["MidClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 30,
        name: "Bonedancer",
        realm: 2,
        base_class_id: 36,
        eligible_races: &[8, 6, 14],
        career_keys: &["MidClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 31,
        name: "Berserker",
        realm: 2,
        base_class_id: 35,
        eligible_races: &[7, 20, 5, 6, 14],
        career_keys: &[
            "VikingCareer",
            "CharacterStyleUserCareer",
            "LightTankCareer",
            "BerserkerCareer",
        ],
    },
    ClassCareerRow {
        class_id: 32,
        name: "Savage",
        realm: 2,
        base_class_id: 35,
        eligible_races: &[7, 8, 5, 6, 14],
        career_keys: &[
            "VikingCareer",
            "CharacterStyleUserCareer",
            "LightTankCareer",
            "SavageCareer",
        ],
    },
    ClassCareerRow {
        class_id: 33,
        name: "Heretic",
        realm: 1,
        base_class_id: 16,
        eligible_races: &[19, 2, 1, 13, 4],
        career_keys: &[
            "AlbClothCasterCareer",
            "CharacterStyleUserCareer",
            "HereticCareer",
        ],
    },
    ClassCareerRow {
        class_id: 34,
        name: "Valkyrie",
        realm: 2,
        base_class_id: 35,
        eligible_races: &[7, 17, 5, 14],
        career_keys: &["VikingCareer", "CharacterStyleUserCareer", "ValkyrieCareer"],
    },
    ClassCareerRow {
        class_id: 35,
        name: "Viking",
        realm: 2,
        base_class_id: 35,
        eligible_races: &[7, 17, 8, 20, 5, 6, 14],
        career_keys: &["VikingCareer"],
    },
    ClassCareerRow {
        class_id: 36,
        name: "Mystic",
        realm: 2,
        base_class_id: 36,
        eligible_races: &[7, 17, 8, 5, 6, 14],
        career_keys: &["MidClothCasterCareer"],
    },
    ClassCareerRow {
        class_id: 37,
        name: "Seer",
        realm: 2,
        base_class_id: 37,
        eligible_races: &[7, 17, 8, 5, 6],
        career_keys: &["SeerCareer"],
    },
    ClassCareerRow {
        class_id: 38,
        name: "Rogue",
        realm: 2,
        base_class_id: 38,
        eligible_races: &[7, 17, 8, 5, 14],
        career_keys: &["MidgardRogueCareer"],
    },
    ClassCareerRow {
        class_id: 39,
        name: "Bainshee",
        realm: 3,
        base_class_id: 51,
        eligible_races: &[9, 11, 12],
        career_keys: &["HibClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 40,
        name: "Eldritch",
        realm: 3,
        base_class_id: 51,
        eligible_races: &[11, 12],
        career_keys: &["HibClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 41,
        name: "Enchanter",
        realm: 3,
        base_class_id: 51,
        eligible_races: &[11, 12],
        career_keys: &["HibClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 42,
        name: "Mentalist",
        realm: 3,
        base_class_id: 51,
        eligible_races: &[9, 11, 12, 18],
        career_keys: &["HibClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 43,
        name: "Blademaster",
        realm: 3,
        base_class_id: 52,
        eligible_races: &[9, 11, 10, 21, 18],
        career_keys: &[
            "GuardianCareer",
            "CharacterStyleUserCareer",
            "LightTankCareer",
            "BlademasterCareer",
        ],
    },
    ClassCareerRow {
        class_id: 44,
        name: "Hero",
        realm: 3,
        base_class_id: 52,
        eligible_races: &[9, 10, 21, 12, 18, 15],
        career_keys: &[
            "GuardianCareer",
            "CharacterStyleUserCareer",
            "PureTankCareer",
            "HeroCareer",
        ],
    },
    ClassCareerRow {
        class_id: 45,
        name: "Champion",
        realm: 3,
        base_class_id: 52,
        eligible_races: &[9, 11, 21, 12, 18],
        career_keys: &[
            "GuardianCareer",
            "CharacterStyleUserCareer",
            "ChampionCareer",
        ],
    },
    ClassCareerRow {
        class_id: 46,
        name: "Warden",
        realm: 3,
        base_class_id: 53,
        eligible_races: &[9, 10, 21, 15],
        career_keys: &[
            "NaturalistCareer",
            "CharacterStyleUserCareer",
            "WardenCareer",
        ],
    },
    ClassCareerRow {
        class_id: 47,
        name: "Druid",
        realm: 3,
        base_class_id: 53,
        eligible_races: &[9, 10, 15],
        career_keys: &["NaturalistCareer", "DruidCareer"],
    },
    ClassCareerRow {
        class_id: 48,
        name: "Bard",
        realm: 3,
        base_class_id: 53,
        eligible_races: &[9, 10],
        career_keys: &["NaturalistCareer", "CharacterStyleUserCareer", "BardCareer"],
    },
    ClassCareerRow {
        class_id: 49,
        name: "Nightshade",
        realm: 3,
        base_class_id: 54,
        eligible_races: &[9, 11, 12],
        career_keys: &[
            "StalkerCareer",
            "AssassinCareer",
            "NightshadeCareer",
            "CharacterStyleUserCareer",
        ],
    },
    ClassCareerRow {
        class_id: 50,
        name: "Ranger",
        realm: 3,
        base_class_id: 54,
        eligible_races: &[9, 11, 12, 18, 15],
        career_keys: &["StalkerCareer", "CharacterStyleUserCareer", "RangerCareer"],
    },
    ClassCareerRow {
        class_id: 51,
        name: "Magician",
        realm: 3,
        base_class_id: 51,
        eligible_races: &[9, 11, 12, 18],
        career_keys: &["HibClothCasterCareer"],
    },
    ClassCareerRow {
        class_id: 52,
        name: "Guardian",
        realm: 3,
        base_class_id: 52,
        eligible_races: &[9, 11, 10, 21, 12, 18, 15],
        career_keys: &["GuardianCareer"],
    },
    ClassCareerRow {
        class_id: 53,
        name: "Naturalist",
        realm: 3,
        base_class_id: 53,
        eligible_races: &[9, 10, 15, 21],
        career_keys: &["NaturalistCareer"],
    },
    ClassCareerRow {
        class_id: 54,
        name: "Stalker",
        realm: 3,
        base_class_id: 54,
        eligible_races: &[9, 11, 12, 18],
        career_keys: &["StalkerCareer"],
    },
    ClassCareerRow {
        class_id: 55,
        name: "Animist",
        realm: 3,
        base_class_id: 57,
        eligible_races: &[9, 10, 15],
        career_keys: &["HibClothCasterCareer", "CharacterQuickcastUserCareer"],
    },
    ClassCareerRow {
        class_id: 56,
        name: "Valewalker",
        realm: 3,
        base_class_id: 57,
        eligible_races: &[9, 10, 15],
        career_keys: &[
            "HibClothCasterCareer",
            "CharacterStyleUserCareer",
            "ValewalkerCareer",
        ],
    },
    ClassCareerRow {
        class_id: 57,
        name: "Forester",
        realm: 3,
        base_class_id: 57,
        eligible_races: &[9, 10, 15],
        career_keys: &["HibClothCasterCareer"],
    },
    ClassCareerRow {
        class_id: 58,
        name: "Vampiir",
        realm: 3,
        base_class_id: 54,
        eligible_races: &[9, 12, 18],
        career_keys: &["StalkerCareer", "CharacterStyleUserCareer", "VampiirCareer"],
    },
    ClassCareerRow {
        class_id: 59,
        name: "Warlock",
        realm: 2,
        base_class_id: 36,
        eligible_races: &[17, 8, 5],
        career_keys: &["MidClothCasterCareer"],
    },
    ClassCareerRow {
        class_id: 60,
        name: "Mauler",
        realm: 1,
        base_class_id: 14,
        eligible_races: &[19, 1, 13],
        career_keys: &[
            "MaulerCareer",
            "MaulerAlbCareer",
            "CharacterStyleUserCareer",
        ],
    },
    ClassCareerRow {
        class_id: 61,
        name: "Mauler",
        realm: 2,
        base_class_id: 35,
        eligible_races: &[8, 20, 5],
        career_keys: &[
            "MaulerCareer",
            "MaulerMidCareer",
            "CharacterStyleUserCareer",
        ],
    },
    ClassCareerRow {
        class_id: 62,
        name: "Mauler",
        realm: 3,
        base_class_id: 52,
        eligible_races: &[9, 21, 12],
        career_keys: &[
            "MaulerCareer",
            "MaulerHibCareer",
            "CharacterStyleUserCareer",
        ],
    },
];

#[must_use]
pub fn race_by_id(id: u8) -> Option<&'static RaceRow> {
    RACES.iter().find(|r| r.id == id)
}

#[must_use]
pub fn class_career_by_id(class_id: u8) -> Option<&'static ClassCareerRow> {
    CLASS_CAREERS.iter().find(|c| c.class_id == class_id)
}

#[must_use]
pub fn starting_classes_for_realm(realm: u8) -> Option<&'static [u8]> {
    match realm {
        REALM_ALBION => Some(STARTING_CLASSES_ALBION),
        REALM_MIDGARD => Some(STARTING_CLASSES_MIDGARD),
        REALM_HIBERNIA => Some(STARTING_CLASSES_HIBERNIA),
        _ => None,
    }
}

#[must_use]
pub fn race_gender_lock(race: u8) -> Option<u8> {
    RACE_GENDER_LOCKS
        .iter()
        .find(|(r, _)| *r == race)
        .map(|(_, g)| *g)
}

#[must_use]
pub fn class_gender_lock(class_id: u8) -> Option<u8> {
    CLASS_GENDER_LOCKS
        .iter()
        .find(|(c, _)| *c == class_id)
        .map(|(_, g)| *g)
}

/// `(race, male_model, female_model)` from OPEN_ORACLE SoloDAoC.
///
/// Provenance: `GameServer/gameobjects/PlayerRace.cs` lines 49–69 pair each `eRace` with its
/// `eLivingModel` per gender; numeric values are `eLivingModel` in
/// `GameServer/GlobalConstants.cs:1039`.
///
/// `None` in the female column is the oracle's own value (`eLivingModel.None`) for the three
/// Minotaur races, which are male-only. That makes the gender lock structural: there is no female
/// model to resolve, so [`race_model`] returns `None` and encoding fails closed rather than
/// shipping a male model under a female gender byte.
pub const RACE_MODELS: &[(u8, u16, Option<u16>)] = &[
    (1, 32, Some(35)),      // Briton
    (2, 61, Some(65)),      // Avalonian
    (3, 39, Some(43)),      // Highlander
    (4, 48, Some(52)),      // Saracen
    (5, 503, Some(507)),    // Norseman
    (6, 137, Some(145)),    // Troll
    (7, 185, Some(193)),    // Dwarf
    (8, 169, Some(177)),    // Kobold
    (9, 302, Some(310)),    // Celt
    (10, 286, Some(294)),   // Firbolg
    (11, 334, Some(342)),   // Elf
    (12, 318, Some(326)),   // Lurikeen
    (13, 716, Some(724)),   // Inconnu
    (14, 773, Some(781)),   // Valkyn
    (15, 700, Some(708)),   // Sylvan
    (16, 1008, Some(1020)), // HalfOgre
    (17, 1051, Some(1063)), // Frostalf
    (18, 1075, Some(1087)), // Shar
    (19, 1395, None),       // Korazh (Albion Minotaur) — male only
    (20, 1407, None),       // Deifrang (Midgard Minotaur) — male only
    (21, 1419, None),       // Graoch (Hibernia Minotaur) — male only
];

/// Resolve the creation `eLivingModel` for `(race, gender)`.
///
/// `gender` is the create-wire encoding: 0 male, 1 female. Returns `None` for an unknown race, an
/// out-of-range gender, or a race with no model for that gender (the male-only Minotaurs) — every
/// one of which must refuse to encode rather than fall back to a stale or invented model.
///
/// B4: `CharacterCreateDraft` previously carried a model set once by its realm stub constructor
/// and never updated by `set_race` or a gender change, so a Half Ogre could be created carrying
/// the Briton stub's model. Worse, the Albion stub's own value was `0x07D1` (2001), which is not
/// any race's `eLivingModel`.
#[must_use]
pub fn race_model(race: u8, gender: u8) -> Option<u16> {
    let (_, male, female) = RACE_MODELS.iter().copied().find(|(r, _, _)| *r == race)?;
    match gender {
        GENDER_MALE => Some(male),
        GENDER_FEMALE => female,
        _ => None,
    }
}

#[cfg(test)]
mod race_model_tests {
    use super::*;

    /// Every race the create form can offer must resolve a male model.
    #[test]
    fn every_playable_race_has_a_male_model() {
        for row in RACES {
            assert!(
                race_model(row.id, GENDER_MALE).is_some(),
                "{} ({}) has no male model",
                row.name,
                row.id
            );
        }
    }

    /// Minotaurs are male-only in the oracle; that must fail closed, not fall back.
    #[test]
    fn minotaur_females_resolve_to_nothing() {
        for race in [19u8, 20, 21] {
            assert!(race_model(race, GENDER_MALE).is_some());
            assert_eq!(
                race_model(race, GENDER_FEMALE),
                None,
                "race {race} is male-only; a female model must not resolve"
            );
        }
    }

    /// Non-Minotaur races must have a female model distinct from the male one — a shared value
    /// would mean the gender byte does not change the avatar.
    #[test]
    fn gendered_races_have_distinct_models() {
        for &(race, male, female) in RACE_MODELS {
            let Some(female) = female else { continue };
            assert_ne!(
                male, female,
                "race {race} male and female models are identical"
            );
        }
    }

    /// Models are unique across races, so a wrong race cannot silently render as another.
    #[test]
    fn models_are_unique_across_races_and_genders() {
        let mut seen: Vec<u16> = Vec::new();
        for &(_, male, female) in RACE_MODELS {
            for m in [Some(male), female].into_iter().flatten() {
                assert!(
                    !seen.contains(&m),
                    "model {m} is shared by two race/gender pairs"
                );
                seen.push(m);
            }
        }
    }

    /// The specific value the Albion stub used to ship is not a race model at all.
    #[test]
    fn the_old_albion_stub_model_was_never_a_race_model() {
        let stale = 0x07D1u16; // 2001
        assert!(
            !RACE_MODELS
                .iter()
                .any(|&(_, m, f)| m == stale || f == Some(stale)),
            "0x07D1 is a real race model after all; revisit the B4 diagnosis"
        );
        assert_eq!(race_model(1, GENDER_MALE), Some(32), "BritonMale is 32");
    }

    /// Unknown races and out-of-range genders refuse rather than defaulting.
    #[test]
    fn unknown_inputs_refuse() {
        assert_eq!(race_model(0, GENDER_MALE), None);
        assert_eq!(race_model(99, GENDER_MALE), None);
        assert_eq!(race_model(1, 2), None);
    }

    /// Every race in `RACE_MODELS` is a real `RaceRow`, and vice versa — neither table may drift.
    #[test]
    fn race_model_table_covers_exactly_the_race_table() {
        for &(race, _, _) in RACE_MODELS {
            assert!(
                race_by_id(race).is_some(),
                "model table has unknown race {race}"
            );
        }
        for row in RACES {
            assert!(
                RACE_MODELS.iter().any(|&(r, _, _)| r == row.id),
                "race {} ({}) has no model row",
                row.name,
                row.id
            );
        }
    }
}
