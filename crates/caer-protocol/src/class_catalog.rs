//! REQ-019 class-coverage harness — five-part per-class verification method.
//!
//! A class is **not** "mapped" until every part is [`PartStatus::Present`]. Partial evidence
//! is [`PartStatus::Provisional`]; absent work is [`PartStatus::Missing`]. There is no
//! `Complete` status on a part: completeness is only the conjunctive claim over all five
//! parts ([`ClassRow::claim_complete`]), and that claim **fails** unless every part is Present.
//!
//! Provenance rule (REQ-022): catalog facts are `OPEN_ORACLE` (SoloDAoC) or `OWN_CAPTURE` only.
//! Invented skill lists are forbidden — mark Provisional/Missing instead.

use std::fmt;

/// The five REQ-019 verification parts (spec falsifier column).
///
/// Missing any of these on a claimed class → FAIL, not partial credit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Req019Part {
    /// (1) Selectable in char-create with race/realm gating.
    CharCreateSelectable,
    /// (2) Starting stats/equipment on avatar.
    StartingAvatar,
    /// (3) Spec lines/skills from career data.
    CareerSpecs,
    /// (4) Class-specific UI surface.
    ClassUiSurface,
    /// (5) One scenario login+ability.
    ScenarioLoginAbility,
}

impl Req019Part {
    pub const ALL: [Self; 5] = [
        Self::CharCreateSelectable,
        Self::StartingAvatar,
        Self::CareerSpecs,
        Self::ClassUiSurface,
        Self::ScenarioLoginAbility,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CharCreateSelectable => "char_create_selectable",
            Self::StartingAvatar => "starting_avatar",
            Self::CareerSpecs => "career_specs",
            Self::ClassUiSurface => "class_ui_surface",
            Self::ScenarioLoginAbility => "scenario_login_ability",
        }
    }
}

impl fmt::Display for Req019Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Evidence fill for one REQ-019 part.
///
/// **No `Complete` variant.** Parts default to [`Missing`](Self::Missing). A silent
/// `Complete`/`Present` default would make the harness non-discriminating (REQ-019 / instrument
/// discipline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PartStatus {
    Present,
    Provisional,
    #[default]
    Missing,
}

impl PartStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Provisional => "provisional",
            Self::Missing => "missing",
        }
    }

    /// Whether this status may contribute to a conjunctive Complete claim.
    #[must_use]
    pub fn counts_toward_complete(self) -> bool {
        matches!(self, Self::Present)
    }
}

/// Where a filled fact came from — never CAER encoder round-trip alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvidenceProvenance {
    OpenOracle,
    OwnCapture,
    /// Used only with [`PartStatus::Missing`] / notes that no claim is made.
    None,
}

impl EvidenceProvenance {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenOracle => "OPEN_ORACLE",
            Self::OwnCapture => "OWN_CAPTURE",
            Self::None => "NONE",
        }
    }
}

/// One assessed REQ-019 part on a class row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartEvidence {
    pub part: Req019Part,
    pub status: PartStatus,
    pub provenance: EvidenceProvenance,
    /// Short citation (oracle path / capture id); empty when Missing with nothing to cite.
    pub citation: &'static str,
}

impl PartEvidence {
    #[must_use]
    pub const fn missing(part: Req019Part) -> Self {
        Self {
            part,
            status: PartStatus::Missing,
            provenance: EvidenceProvenance::None,
            citation: "",
        }
    }

    #[must_use]
    pub const fn present(
        part: Req019Part,
        provenance: EvidenceProvenance,
        citation: &'static str,
    ) -> Self {
        Self {
            part,
            status: PartStatus::Present,
            provenance,
            citation,
        }
    }

    #[must_use]
    pub const fn provisional(
        part: Req019Part,
        provenance: EvidenceProvenance,
        citation: &'static str,
    ) -> Self {
        Self {
            part,
            status: PartStatus::Provisional,
            provenance,
            citation,
        }
    }
}

/// Why [`ClassRow::claim_complete`] rejected a Complete claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompleteClaim {
    pub class_id: u8,
    pub class_name: &'static str,
    pub blockers: Vec<(Req019Part, PartStatus)>,
}

impl fmt::Display for IncompleteClaim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "REQ-019 Complete claim rejected for {} (id={}):",
            self.class_name, self.class_id
        )?;
        for (part, status) in &self.blockers {
            write!(f, " {part}={status}", status = status.as_str())?;
        }
        Ok(())
    }
}

/// Per-class REQ-019 checklist row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRow {
    /// `eCharacterClass` id (SoloDAoC `GlobalConstants.cs`).
    pub class_id: u8,
    pub name: &'static str,
    /// 1 Albion / 2 Midgard / 3 Hibernia.
    pub realm: u8,
    /// Base class id when advanced (`eCharacterClass::Fighter` = 14 for Armsman).
    pub base_class_id: u8,
    /// Eligible `eRace` ids for char-create gating (OPEN_ORACLE when Present).
    pub eligible_races: &'static [u8],
    /// Trainable / career specialisation key names from OPEN_ORACLE when Present.
    pub spec_key_names: &'static [&'static str],
    /// Exactly five parts — one per [`Req019Part::ALL`] order.
    pub parts: [PartEvidence; 5],
}

impl ClassRow {
    /// Lookup status for a part; panics only if the row was built without that part (harness bug).
    #[must_use]
    pub fn part(&self, want: Req019Part) -> &PartEvidence {
        self.parts
            .iter()
            .find(|p| p.part == want)
            .expect("REQ-019 row missing a required part — harness invariant broken")
    }

    /// Conjunctive Complete claim. **Fails** unless every part is [`PartStatus::Present`].
    ///
    /// Provisional and Missing both block Complete — that is the REQ-019 falsifier surface.
    pub fn claim_complete(&self) -> Result<(), IncompleteClaim> {
        let blockers: Vec<_> = self
            .parts
            .iter()
            .filter(|p| !p.status.counts_toward_complete())
            .map(|p| (p.part, p.status))
            .collect();
        if blockers.is_empty() {
            Ok(())
        } else {
            Err(IncompleteClaim {
                class_id: self.class_id,
                class_name: self.name,
                blockers,
            })
        }
    }

    /// Structural check: all five parts present in the row, no duplicates, Default ≠ Present.
    pub fn assert_checklist_shape(&self) {
        assert_eq!(
            self.parts.len(),
            Req019Part::ALL.len(),
            "{}: REQ-019 requires exactly five parts",
            self.name
        );
        for (i, expected) in Req019Part::ALL.iter().enumerate() {
            assert_eq!(
                self.parts[i].part, *expected,
                "{}: part slot {i} must be {expected}, got {}",
                self.name, self.parts[i].part
            );
        }
        // Present evidence must name OPEN_ORACLE or OWN_CAPTURE — never None.
        for p in &self.parts {
            if p.status == PartStatus::Present {
                assert!(
                    matches!(
                        p.provenance,
                        EvidenceProvenance::OpenOracle | EvidenceProvenance::OwnCapture
                    ),
                    "{}: Present part {} has provenance {}",
                    self.name,
                    p.part,
                    p.provenance.as_str()
                );
                assert!(
                    !p.citation.is_empty(),
                    "{}: Present part {} needs a citation",
                    self.name,
                    p.part
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Oracle constants — Armsman (eCharacterClass = 2)
// ---------------------------------------------------------------------------

/// `eCharacterClass::Armsman` — SoloDAoC `GlobalConstants.cs` ~line 890.
pub const ARMSMAN_CLASS_ID: u8 = 2;
/// `eCharacterClass::Fighter` base — same file ~line 877.
pub const FIGHTER_BASE_CLASS_ID: u8 = 14;
/// Albion realm byte used on the wire / create form.
pub const REALM_ALBION: u8 = 1;

/// Armsman EligibleRaces from SoloDAoC `CharacterClassDB.cs` Armsman row (OPEN_ORACLE).
///
/// `eRace`: Briton=1, Avalonian=2, Highlander=3, Saracen=4, Inconnu=13, HalfOgre=16,
/// Korazh/AlbionMinotaur=19. Read the live `EligibleRaces` string — do not trust stale
/// comments that omit Catacombs/LotM races.
pub const ARMSMAN_ELIGIBLE_RACES: &[u8] = &[1, 2, 3, 4, 13, 16, 19];

/// Trainable ClassXSpecialization key names for ClassID 2 from SoloDAoC `eod-public-db.sql`
/// (OPEN_ORACLE), plus career link keys from `career_specs.sql`.
///
/// Style/ability *lists* beyond these keys are not invented here.
pub const ARMSMAN_SPEC_KEYS: &[&str] = &[
    // Trainable weapon/defense specs (ClassXSpecialization ClassID=2)
    "Slash",
    "Thrust",
    "Crush",
    "Polearm",
    "Shields",
    "Two Handed",
    "Parry",
    "Crossbows",
    // Career links (career_specs.sql ClassID=2)
    "FighterCareer",
    "CharacterStyleUserCareer",
    "PureTankCareer",
    "ArmsmanCareer",
];

/// Auto-train skill names from `CharacterClassDB` Armsman `AutoTrainSkills` (OPEN_ORACLE).
pub const ARMSMAN_AUTO_TRAIN: &[&str] = &["Slash", "Thrust"];

/// Primary/secondary/tertiary stats from `CharacterClassDB` Armsman row (`eStat` STR/CON/DEX).
pub const ARMSMAN_PRIMARY_STAT_ORDER: &[&str] = &["STR", "CON", "DEX"];

/// ArmsmanTrainer promotion weapon template ids (OPEN_ORACLE `ArmsmanTrainer.cs`).
pub const ARMSMAN_PROMOTE_WEAPON_TEMPLATES: &[&str] = &[
    "slash_sword_item",
    "crush_sword_item",
    "thrust_sword_item",
    "pike_polearm_item",
];

/// Catalog entry for Albion Armsman — first class under the REQ-019 harness.
///
/// Parts 4–5 remain Missing until CAER ships a class UI surface and a login+ability scenario.
/// Part 2 is Provisional: race base stats + class primary stats are oracle-backed, but
/// avatar starting-equipment appearance is not yet verified end-to-end on CAER.
#[must_use]
pub fn armsman_row() -> ClassRow {
    ClassRow {
        class_id: ARMSMAN_CLASS_ID,
        name: "Armsman",
        realm: REALM_ALBION,
        base_class_id: FIGHTER_BASE_CLASS_ID,
        eligible_races: ARMSMAN_ELIGIBLE_RACES,
        spec_key_names: ARMSMAN_SPEC_KEYS,
        parts: [
            PartEvidence::present(
                Req019Part::CharCreateSelectable,
                EvidenceProvenance::OpenOracle,
                "SoloDAoC CharacterClassDB Armsman EligibleRaces + eCharacterClass=2; \
                 caer_protocol::charcreate::CharacterCreateDraft::albion_briton_stub",
            ),
            PartEvidence::provisional(
                Req019Part::StartingAvatar,
                EvidenceProvenance::OpenOracle,
                "STARTING_STATS_DICT race bases + CharacterClassDB STR/CON/DEX + \
                 ArmsmanTrainer promote templates — avatar equipment mesh not CAER-verified",
            ),
            PartEvidence::present(
                Req019Part::CareerSpecs,
                EvidenceProvenance::OpenOracle,
                "career_specs.sql ClassID=2 + ArmsmanCareer SpecXAbility; \
                 eod-public-db ClassXSpecialization Slash/Thrust/Crush/Polearm/Shields/\
                 Two Handed/Parry/Crossbows",
            ),
            PartEvidence::missing(Req019Part::ClassUiSurface),
            PartEvidence::missing(Req019Part::ScenarioLoginAbility),
        ],
    }
}

fn row_from_career(c: &crate::career::ClassCareerRow) -> ClassRow {
    let create = if c.eligible_races.is_empty() {
        PartEvidence::missing(Req019Part::CharCreateSelectable)
    } else {
        PartEvidence::provisional(
            Req019Part::CharCreateSelectable,
            EvidenceProvenance::OpenOracle,
            "SoloDAoC CharacterClassDB EligibleRaces + STARTING_CLASSES_DICT; \
             caer_protocol::career::CLASS_CAREERS — legal catalog, not shipped char-create proof",
        )
    };
    let specs = if c.career_keys.is_empty() {
        PartEvidence::missing(Req019Part::CareerSpecs)
    } else {
        PartEvidence::present(
            Req019Part::CareerSpecs,
            EvidenceProvenance::OpenOracle,
            "career_specs.sql ClassXSpecialization / caer_protocol::career::CLASS_CAREERS career_keys",
        )
    };
    ClassRow {
        class_id: c.class_id,
        name: c.name,
        realm: c.realm,
        base_class_id: c.base_class_id,
        eligible_races: c.eligible_races,
        spec_key_names: c.career_keys,
        parts: [
            create,
            PartEvidence::provisional(
                Req019Part::StartingAvatar,
                EvidenceProvenance::OpenOracle,
                "career/race tables exist; CAER avatar equipment mesh not verified",
            ),
            specs,
            PartEvidence::missing(Req019Part::ClassUiSurface),
            PartEvidence::missing(Req019Part::ScenarioLoginAbility),
        ],
    }
}

/// Every playable CLASS_CAREERS row. Completeness is still conjunctive Present on all five parts.
#[must_use]
pub fn catalog() -> Vec<ClassRow> {
    crate::career::CLASS_CAREERS
        .iter()
        .map(|c| {
            if c.class_id == ARMSMAN_CLASS_ID {
                armsman_row()
            } else {
                row_from_career(c)
            }
        })
        .collect()
}

/// Look up a catalog row by `eCharacterClass` id.
#[must_use]
pub fn class_by_id(class_id: u8) -> Option<ClassRow> {
    catalog().into_iter().find(|c| c.class_id == class_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_status_default_is_missing_not_present() {
        // Named invariant: Default must not silently green a part.
        assert_eq!(PartStatus::default(), PartStatus::Missing);
        assert!(!PartStatus::default().counts_toward_complete());
    }

    #[test]
    fn req019_has_exactly_five_named_parts() {
        assert_eq!(Req019Part::ALL.len(), 5);
        let names: Vec<_> = Req019Part::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            names,
            [
                "char_create_selectable",
                "starting_avatar",
                "career_specs",
                "class_ui_surface",
                "scenario_login_ability",
            ]
        );
    }

    #[test]
    fn armsman_ids_match_solodaoc_enums() {
        assert_eq!(ARMSMAN_CLASS_ID, 2);
        assert_eq!(FIGHTER_BASE_CLASS_ID, 14);
        let row = armsman_row();
        assert_eq!(row.class_id, 2);
        assert_eq!(row.realm, REALM_ALBION);
        assert_eq!(row.base_class_id, 14);
        // Briton must be eligible (classic Armsman path used by create stub).
        assert!(row.eligible_races.contains(&1));
        // OPEN_ORACLE CharacterClassDB includes LotM/Catacombs races — do not drop them.
        assert!(row.eligible_races.contains(&16)); // HalfOgre
        assert!(row.eligible_races.contains(&19)); // Korazh / AlbionMinotaur
    }

    #[test]
    fn armsman_row_covers_all_five_parts() {
        let row = armsman_row();
        row.assert_checklist_shape();
        for part in Req019Part::ALL {
            let ev = row.part(part);
            assert_eq!(ev.part, part);
            // No silent Complete: status is only Present|Provisional|Missing.
            assert!(
                matches!(
                    ev.status,
                    PartStatus::Present | PartStatus::Provisional | PartStatus::Missing
                ),
                "illegal status on {part}"
            );
        }
    }

    #[test]
    fn armsman_evidence_levels_match_oracle_fill() {
        let row = armsman_row();
        assert_eq!(
            row.part(Req019Part::CharCreateSelectable).status,
            PartStatus::Present
        );
        assert_eq!(
            row.part(Req019Part::StartingAvatar).status,
            PartStatus::Provisional
        );
        assert_eq!(
            row.part(Req019Part::CareerSpecs).status,
            PartStatus::Present
        );
        assert_eq!(
            row.part(Req019Part::ClassUiSurface).status,
            PartStatus::Missing
        );
        assert_eq!(
            row.part(Req019Part::ScenarioLoginAbility).status,
            PartStatus::Missing
        );
        assert!(row.spec_key_names.contains(&"ArmsmanCareer"));
        assert!(row.spec_key_names.contains(&"Slash"));
        assert!(ARMSMAN_AUTO_TRAIN.contains(&"Slash"));
        assert_eq!(ARMSMAN_PRIMARY_STAT_ORDER, &["STR", "CON", "DEX"]);
        assert_eq!(ARMSMAN_PROMOTE_WEAPON_TEMPLATES.len(), 4);
    }

    /// Named falsifier (REQ-019): Complete must FAIL while any part is not Present.
    ///
    /// If this ever passes for Armsman as currently filled, the instrument stopped discriminating
    /// — Provisional/Missing were treated as success.
    #[test]
    fn falsifier_armsman_complete_claim_rejected_while_parts_unfinished() {
        let row = armsman_row();
        let err = row
            .claim_complete()
            .expect_err("Armsman must not claim Complete with Provisional/Missing parts");
        assert_eq!(err.class_id, ARMSMAN_CLASS_ID);
        let blocked: Vec<_> = err.blockers.iter().map(|(p, _)| *p).collect();
        assert!(blocked.contains(&Req019Part::StartingAvatar));
        assert!(blocked.contains(&Req019Part::ClassUiSurface));
        assert!(blocked.contains(&Req019Part::ScenarioLoginAbility));
        // Present parts must not appear as blockers.
        assert!(!blocked.contains(&Req019Part::CharCreateSelectable));
        assert!(!blocked.contains(&Req019Part::CareerSpecs));
    }

    #[test]
    fn falsifier_provisional_alone_blocks_complete() {
        // Synthetic row: four Present + one Provisional still cannot claim Complete.
        let mut row = armsman_row();
        for p in &mut row.parts {
            p.status = PartStatus::Present;
            p.provenance = EvidenceProvenance::OpenOracle;
            p.citation = "synthetic";
        }
        row.parts[1].status = PartStatus::Provisional;
        let err = row
            .claim_complete()
            .expect_err("Provisional must block Complete");
        assert_eq!(err.blockers.len(), 1);
        assert_eq!(err.blockers[0].0, Req019Part::StartingAvatar);
        assert_eq!(err.blockers[0].1, PartStatus::Provisional);
    }

    #[test]
    fn catalog_covers_every_class_career_row() {
        let cat = catalog();
        assert_eq!(
            cat.len(),
            crate::career::CLASS_CAREERS.len(),
            "catalog must be generated from CLASS_CAREERS, not a hand list"
        );
        for c in crate::career::CLASS_CAREERS {
            let row = class_by_id(c.class_id).unwrap_or_else(|| panic!("missing {}", c.name));
            assert_eq!(row.name, c.name);
            assert!(
                row.claim_complete().is_err(),
                "{} must not claim Complete while UI/scenario parts are Missing",
                c.name
            );
        }
        let arms = class_by_id(ARMSMAN_CLASS_ID).expect("Armsman");
        assert_eq!(arms.name, "Armsman");
        assert_eq!(
            arms.part(Req019Part::ClassUiSurface).status,
            PartStatus::Missing
        );
        assert_eq!(
            arms.part(Req019Part::ScenarioLoginAbility).status,
            PartStatus::Missing
        );
        let paladin = class_by_id(1).expect("Paladin");
        assert_eq!(paladin.name, "Paladin");
        assert_eq!(paladin.realm, 1);
    }

    /// Falsifier H2: oracle table membership is not shipped char-create proof.
    #[test]
    fn falsifier_non_armsman_char_create_is_provisional() {
        let paladin = class_by_id(1).expect("Paladin");
        assert_eq!(
            paladin.part(Req019Part::CharCreateSelectable).status,
            PartStatus::Provisional,
            "Paladin CharCreateSelectable must stay Provisional until product-path proof"
        );
        let arms = class_by_id(ARMSMAN_CLASS_ID).expect("Armsman");
        assert_eq!(
            arms.part(Req019Part::CharCreateSelectable).status,
            PartStatus::Present
        );
        assert!(
            paladin.claim_complete().is_err(),
            "Paladin must not claim_complete"
        );
        assert!(
            arms.claim_complete().is_err(),
            "Armsman must not claim_complete"
        );
        for c in crate::career::CLASS_CAREERS {
            if c.class_id == ARMSMAN_CLASS_ID {
                continue;
            }
            let row = class_by_id(c.class_id).unwrap_or_else(|| panic!("missing {}", c.name));
            if c.eligible_races.is_empty() {
                assert_eq!(
                    row.part(Req019Part::CharCreateSelectable).status,
                    PartStatus::Missing,
                    "{} empty eligible_races must stay Missing",
                    c.name
                );
            } else {
                assert_eq!(
                    row.part(Req019Part::CharCreateSelectable).status,
                    PartStatus::Provisional,
                    "{} must not mark CharCreateSelectable Present from tables alone",
                    c.name
                );
            }
        }
    }
}
