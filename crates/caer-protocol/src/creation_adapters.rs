//! Creation-form adapter manifest — the single resolved record a class button renders *from*
//! and dispatches *with*.
//!
//! # Why this module exists
//!
//! The renderer previously carried two independent hand-written tables: a label array
//! (`preworld.rs` `CLASSES`) and a set of per-realm index→id functions (`albion_class_id` and
//! friends). Nothing tied them together, and they disagreed on five buttons — a control reading
//! `Mercenary` dispatched Theurgist, `Skald` dispatched Thane, `Berserker` dispatched Skald,
//! `Thane` dispatched Shadowblade, `Blademaster` dispatched Eldritch. That is visible semantic
//! corruption: the player creates a class other than the one named on the button they pressed.
//!
//! The structural fix is not to correct both tables — it is to delete one. Here a [`ClassAdapter`]
//! carries the label and the wire id in the same record, both resolved from
//! [`career::class_career_by_id`], so a divergence cannot be expressed. There is no second table to
//! drift against.
//!
//! # Layering
//!
//! 1. [`crate::career`] — canonical class identity, wire id, realm, base class, eligibility.
//! 2. **This module** — which classes a form context actually offers, in which adapter slot.
//! 3. Renderer + dispatch — both consume the same [`ClassAdapter`]; neither owns class truth.
//!
//! # Provenance
//!
//! * **Slot capacity** (`OWN_CAPTURE`, retail `pregame/character_creation.xml`
//!   `sha256:46d5de87dd1c4b2b3b1b1df3199556a0780ffe38d09f77ef41df7c4a9eda4a1c`):
//!   16 class adapters `class_0_text..class_15_text` (ControlId 1021..1036) and 7 race adapters
//!   `race_0_text..race_6_text` (ControlId 1014..1020).
//! * **Membership** (`OPEN_ORACLE`, SoloDAoC `STARTING_CLASSES_DICT` via [`crate::career`]):
//!   the creation form offers *advanced* classes. DoL 1.127 starts a character at its advanced
//!   class at level 1; the pre-1.93 base classes remain in the dict but are not creation choices.
//!   Counts are Albion 16, Midgard 15, Hibernia 16, each including its realm Mauler (60/61/62),
//!   which is exactly the 16-slot capacity with Midgard leaving one slot empty.
//! * **Order** — **UNPROVEN**. See [`SLOT_ORDER_PROVENANCE`]. The dict order is enum order, and
//!   deriving display order from enum order is precisely the mistake that produced the corruption
//!   above. Slots are filled in dict order *as a placeholder*, and no test in this crate asserts
//!   that the resulting order matches retail.
//!
//! The retail client also ships `charman/summary.mpk:summary.txt`, a 79-entry race×*base*-class
//! description table. It is pre-1.93 data retained in the tree and is **not** evidence about the
//! 1.127 creation form. It is the content source for the unimplemented description panes
//! (`race_desc_label_text` / `class_desc_label_text`) and nothing else.

use crate::career::{self, ClassCareerRow};

/// Authored class adapter slots on the creation form (`class_0_text..class_15_text`).
pub const CLASS_ADAPTER_SLOTS: usize = 16;

/// Authored race adapter slots on the creation form (`race_0_text..race_6_text`).
pub const RACE_ADAPTER_SLOTS: usize = 7;

/// Status of the slot ordering claim.
///
/// **Resolved 2026-08-15 (`OWN_CAPTURE`).** Live creation forms were captured for all three realms
/// (Albion/Highlander + Albion/Minotaur, Midgard/Norseman, Hibernia/Celt + Hibernia/Minotaur) and
/// the on-screen order matches `STARTING_CLASSES_DICT` order exactly, slot for slot, in every
/// realm. `class_display_order_matches_the_captured_forms` locks the observed sequence so a future
/// table edit that reorders the buttons fails.
///
/// The dict order turning out to be right does **not** retroactively justify assuming it — the
/// same assumption applied to the *labels* is what produced the B2 corruption.
pub const SLOT_ORDER_PROVENANCE: &str =
    "OWN_CAPTURE 2026-08-15: class adapter display order observed on live forms, all three realms";

/// One resolved creation-form class control.
///
/// `label` and `class_id` are read from the same [`ClassCareerRow`]. Rendering the label from one
/// field and dispatching the other is what makes them impossible to desynchronise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassAdapter {
    /// Adapter slot index, `0..CLASS_ADAPTER_SLOTS`. Maps to `class_{slot}_text` / ControlId
    /// `1021 + slot`.
    pub slot: u8,
    /// Wire id dispatched in the `0xFF` character-create payload.
    pub class_id: u8,
    /// Text drawn on the button — the canonical career name for `class_id`.
    pub label: &'static str,
    /// Whether this class accepts the currently selected race. A visible-but-ineligible control is
    /// drawn disabled rather than omitted, so the form does not reflow under the pointer.
    pub eligible: bool,
}

/// True when `row` is a base (pre-1.93) class rather than a creation choice.
///
/// A base class is its own base — `base_class_id == class_id`. Derived rather than listed so a
/// future career-table edit cannot leave a stale hand-maintained exclusion list behind.
#[must_use]
pub fn is_base_class(row: &ClassCareerRow) -> bool {
    row.base_class_id == row.class_id
}

/// Advanced classes the realm offers at creation, in server-dict order.
///
/// Order is a placeholder — see [`SLOT_ORDER_PROVENANCE`].
#[must_use]
pub fn advanced_classes_for_realm(realm: u8) -> Vec<&'static ClassCareerRow> {
    let Some(ids) = career::starting_classes_for_realm(realm) else {
        return Vec::new();
    };
    ids.iter()
        .filter_map(|&id| career::class_career_by_id(id))
        .filter(|row| !is_base_class(row))
        .collect()
}

/// Resolve the class controls for a form context.
///
/// `race` gates [`ClassAdapter::eligible`]; pass `0` when no race is chosen yet, which leaves every
/// control ineligible rather than guessing a default.
///
/// Returns at most [`CLASS_ADAPTER_SLOTS`] entries. If a realm ever exceeds capacity the surplus is
/// dropped rather than silently overflowing an authored form — [`class_adapter_overflow`] reports
/// that condition so a test can fail on it instead of it passing unnoticed.
#[must_use]
pub fn class_adapters(realm: u8, race: u8) -> Vec<ClassAdapter> {
    advanced_classes_for_realm(realm)
        .into_iter()
        .take(CLASS_ADAPTER_SLOTS)
        .enumerate()
        .map(|(slot, row)| ClassAdapter {
            slot: slot as u8,
            class_id: row.class_id,
            label: row.name,
            eligible: class_eligible(realm, row, race, None),
        })
        .collect()
}

/// Same as [`class_adapters`], with gender folded into [`ClassAdapter::eligible`].
///
/// Race-only eligibility leaves Bainshee lit for a male Celt. `classify` already forbids that.
#[must_use]
pub fn class_adapters_for(realm: u8, race: u8, gender: u8) -> Vec<ClassAdapter> {
    advanced_classes_for_realm(realm)
        .into_iter()
        .take(CLASS_ADAPTER_SLOTS)
        .enumerate()
        .map(|(slot, row)| ClassAdapter {
            slot: slot as u8,
            class_id: row.class_id,
            label: row.name,
            eligible: class_eligible(realm, row, race, Some(gender)),
        })
        .collect()
}

fn class_eligible(realm: u8, row: &ClassCareerRow, race: u8, gender: Option<u8>) -> bool {
    if race == 0 || !row.eligible_races.contains(&race) {
        return false;
    }
    let Some(g) = gender else {
        return true;
    };
    matches!(
        crate::create_validity::classify(realm, row.class_id, race, g),
        crate::create_validity::Legality::Allowed
    )
}

/// One resolved creation-form race control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RaceAdapter {
    /// Adapter slot index, `0..RACE_ADAPTER_SLOTS`. Maps to `race_{slot}_text` / ControlId
    /// `1014 + slot`. Slot 6 is the Minotaur control, authored above the grid at (901,160).
    pub slot: u8,
    /// `eRace` id sent in the create payload.
    pub race_id: u8,
    /// Text drawn on the button.
    pub label: &'static str,
    /// Male-only race (the three Minotaurs). The gender control must follow this.
    pub male_only: bool,
}

/// Retail button text for a race.
///
/// `career::RACES` names the three Minotaur races by realm (`AlbionMinotaur`, …) because the
/// server enum does; every captured creation form labels all three simply `Minotaur`.
#[must_use]
pub fn race_button_label(race_id: u8, name: &'static str) -> &'static str {
    match race_id {
        19..=21 => "Minotaur",
        _ => name,
    }
}

/// Resolve the race controls for a realm, in authored slot order.
///
/// Order is `OWN_CAPTURE` — it matches `career::RACES` (ascending `eRace`) on all three realms in
/// the captured forms. The renderer previously carried its own label array which had Albion's
/// Avalonian and Highlander transposed.
#[must_use]
pub fn race_adapters(realm: u8) -> Vec<RaceAdapter> {
    career::RACES
        .iter()
        .filter(|r| r.realm == realm)
        .take(RACE_ADAPTER_SLOTS)
        .enumerate()
        .map(|(slot, r)| RaceAdapter {
            slot: slot as u8,
            race_id: r.id,
            label: race_button_label(r.id, r.name),
            male_only: career::race_model(r.id, career::GENDER_FEMALE).is_none(),
        })
        .collect()
}

/// Look up a race adapter by slot.
#[must_use]
pub fn race_adapter_at(realm: u8, slot: u8) -> Option<RaceAdapter> {
    race_adapters(realm).into_iter().find(|a| a.slot == slot)
}

/// How many advanced classes a realm has beyond the authored slot capacity (0 when it fits).
#[must_use]
pub fn class_adapter_overflow(realm: u8) -> usize {
    advanced_classes_for_realm(realm)
        .len()
        .saturating_sub(CLASS_ADAPTER_SLOTS)
}

/// Look up a resolved adapter by slot, or `None` when that slot is empty for this realm.
///
/// Dispatch goes through this so a click on an unpopulated slot cannot fall through to a default
/// class. Midgard legitimately leaves slot 15 empty.
#[must_use]
pub fn class_adapter_at(realm: u8, race: u8, slot: u8) -> Option<ClassAdapter> {
    class_adapters(realm, race)
        .into_iter()
        .find(|a| a.slot == slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **B2 falsifier.** Every visible class button's label must be the canonical career name for
    /// the id it dispatches. This is the exact property the two old tables violated on five
    /// buttons.
    #[test]
    fn every_adapter_label_matches_its_dispatched_wire_id() {
        for realm in 1..=3u8 {
            for adapter in class_adapters(realm, 0) {
                let canonical = career::class_career_by_id(adapter.class_id).unwrap_or_else(|| {
                    panic!(
                        "realm {realm} slot {} dispatches unknown class id {}",
                        adapter.slot, adapter.class_id
                    )
                });
                assert_eq!(
                    adapter.label, canonical.name,
                    "realm {realm} slot {}: button reads {:?} but dispatches class id {} ({:?})",
                    adapter.slot, adapter.label, adapter.class_id, canonical.name
                );
            }
        }
    }

    /// Proves the falsifier above can actually turn red: the historical corruption must be
    /// detected by the same comparison. Without this, a check that only ever compares a value to
    /// itself would pass vacuously.
    #[test]
    fn the_label_id_check_rejects_the_historical_corruption() {
        // The five real pre-repair pairings, as (label shown, id dispatched).
        for (shown, dispatched) in [
            ("Mercenary", 5u8),  // was Theurgist
            ("Skald", 21),       // was Thane
            ("Berserker", 24),   // was Skald
            ("Thane", 23),       // was Shadowblade
            ("Blademaster", 40), // was Eldritch
        ] {
            let canonical = career::class_career_by_id(dispatched).expect("known id");
            assert_ne!(
                shown, canonical.name,
                "corruption case ({shown} -> {dispatched}) no longer differs; \
                 the falsifier would pass vacuously"
            );
        }
    }

    /// **OWN_CAPTURE lock, 2026-08-15.** The exact on-screen button order from the captured live
    /// creation forms, all three realms. Reordering `STARTING_CLASSES_*` fails here.
    #[test]
    fn class_display_order_matches_the_captured_forms() {
        let observed: [(u8, &[&str]); 3] = [
            (
                1,
                &[
                    "Paladin",
                    "Armsman",
                    "Scout",
                    "Minstrel",
                    "Theurgist",
                    "Cleric",
                    "Wizard",
                    "Sorcerer",
                    "Infiltrator",
                    "Friar",
                    "Mercenary",
                    "Necromancer",
                    "Cabalist",
                    "Reaver",
                    "Heretic",
                    "Mauler",
                ],
            ),
            (
                2,
                &[
                    "Thane",
                    "Warrior",
                    "Shadowblade",
                    "Skald",
                    "Hunter",
                    "Healer",
                    "Spiritmaster",
                    "Shaman",
                    "Runemaster",
                    "Bonedancer",
                    "Berserker",
                    "Savage",
                    "Valkyrie",
                    "Warlock",
                    "Mauler",
                ],
            ),
            (
                3,
                &[
                    "Bainshee",
                    "Eldritch",
                    "Enchanter",
                    "Mentalist",
                    "Blademaster",
                    "Hero",
                    "Champion",
                    "Warden",
                    "Druid",
                    "Bard",
                    "Nightshade",
                    "Ranger",
                    "Animist",
                    "Valewalker",
                    "Vampiir",
                    "Mauler",
                ],
            ),
        ];
        for (realm, want) in observed {
            let got: Vec<&str> = class_adapters(realm, 0).iter().map(|a| a.label).collect();
            assert_eq!(
                got, want,
                "realm {realm} class button order drifted from the capture"
            );
        }
    }

    /// **OWN_CAPTURE lock.** Race button order, all three realms. Albion is the one that was
    /// wrong: the renderer's own array had Avalonian and Highlander transposed.
    #[test]
    fn race_display_order_matches_the_captured_forms() {
        let observed: [(u8, &[&str]); 3] = [
            (
                1,
                &[
                    "Briton",
                    "Avalonian",
                    "Highlander",
                    "Saracen",
                    "Inconnu",
                    "HalfOgre",
                    "Minotaur",
                ],
            ),
            (
                2,
                &[
                    "Norseman", "Troll", "Dwarf", "Kobold", "Valkyn", "Frostalf", "Minotaur",
                ],
            ),
            (
                3,
                &[
                    "Celt", "Firbolg", "Elf", "Lurikeen", "Sylvan", "Shar", "Minotaur",
                ],
            ),
        ];
        for (realm, want) in observed {
            let got: Vec<&str> = race_adapters(realm).iter().map(|a| a.label).collect();
            assert_eq!(
                got, want,
                "realm {realm} race button order drifted from the capture"
            );
            assert_eq!(
                got.len(),
                RACE_ADAPTER_SLOTS,
                "realm {realm} must fill all 7 race slots"
            );
        }
    }

    /// All three Minotaurs render as plain "Minotaur" and are male-only.
    #[test]
    fn minotaur_slot_is_labelled_plainly_and_is_male_only() {
        for realm in 1..=3u8 {
            let m = race_adapter_at(realm, 6).expect("slot 6 is the Minotaur control");
            assert_eq!(m.label, "Minotaur", "realm {realm} slot 6 label");
            assert!(m.male_only, "realm {realm} Minotaur must be male-only");
            assert!(matches!(m.race_id, 19..=21));
        }
        // And nothing else is male-only, or the flag is meaningless.
        for realm in 1..=3u8 {
            let others = race_adapters(realm)
                .into_iter()
                .filter(|a| a.slot != 6 && a.male_only)
                .count();
            assert_eq!(others, 0, "realm {realm} has an unexpected male-only race");
        }
    }

    /// Membership oracle: SoloDAoC advanced-class counts per realm, each including its Mauler.
    #[test]
    fn advanced_class_counts_match_the_server_oracle() {
        assert_eq!(advanced_classes_for_realm(1).len(), 16, "Albion");
        assert_eq!(advanced_classes_for_realm(2).len(), 15, "Midgard");
        assert_eq!(advanced_classes_for_realm(3).len(), 16, "Hibernia");
    }

    /// Every realm's Mauler is offered (Alb 60 / Mid 61 / Hib 62).
    #[test]
    fn each_realm_offers_its_mauler() {
        for (realm, mauler) in [(1u8, 60u8), (2, 61), (3, 62)] {
            assert!(
                class_adapters(realm, 0)
                    .iter()
                    .any(|a| a.class_id == mauler),
                "realm {realm} does not offer Mauler {mauler}"
            );
        }
    }

    /// Base classes are pre-1.93 and are not creation choices in 1.127.
    #[test]
    fn no_base_class_reaches_the_creation_form() {
        for realm in 1..=3u8 {
            for adapter in class_adapters(realm, 0) {
                let row = career::class_career_by_id(adapter.class_id).expect("known id");
                assert!(
                    !is_base_class(row),
                    "realm {realm} slot {} offers base class {:?} ({})",
                    adapter.slot,
                    row.name,
                    row.class_id
                );
            }
        }
    }

    /// The base-class filter must actually remove something, or the previous test is vacuous.
    #[test]
    fn the_base_class_filter_is_not_a_no_op() {
        for (realm, expect_removed) in [(1u8, 6usize), (2, 4), (3, 5)] {
            let all = career::starting_classes_for_realm(realm)
                .expect("realm")
                .len();
            let advanced = advanced_classes_for_realm(realm).len();
            assert_eq!(
                all - advanced,
                expect_removed,
                "realm {realm}: expected {expect_removed} base classes filtered out"
            );
        }
    }

    /// No realm may exceed the 16 authored slots.
    #[test]
    fn no_realm_overflows_the_authored_slot_capacity() {
        for realm in 1..=3u8 {
            assert_eq!(
                class_adapter_overflow(realm),
                0,
                "realm {realm} overflows 16 slots"
            );
            assert!(class_adapters(realm, 0).len() <= CLASS_ADAPTER_SLOTS);
        }
    }

    /// Slots are dense from 0 and unique, so a click index maps to exactly one class.
    #[test]
    fn slots_are_dense_and_unique() {
        for realm in 1..=3u8 {
            let adapters = class_adapters(realm, 0);
            for (i, a) in adapters.iter().enumerate() {
                assert_eq!(a.slot as usize, i, "realm {realm} slot indices not dense");
            }
            let mut ids: Vec<u8> = adapters.iter().map(|a| a.class_id).collect();
            ids.sort_unstable();
            let before = ids.len();
            ids.dedup();
            assert_eq!(
                before,
                ids.len(),
                "realm {realm} offers a duplicate class id"
            );
        }
    }

    /// Midgard has 15 advanced classes against 16 slots, so the last slot must be empty rather
    /// than wrapping to a default class.
    #[test]
    fn midgard_leaves_its_sixteenth_slot_empty() {
        assert!(class_adapter_at(2, 0, 14).is_some());
        assert!(
            class_adapter_at(2, 0, 15).is_none(),
            "Midgard slot 15 must be empty, not a fallback class"
        );
    }

    /// Eligibility is race-gated and must not be true by default.
    #[test]
    fn eligibility_requires_a_selected_race() {
        assert!(
            class_adapters(1, 0).iter().all(|a| !a.eligible),
            "no race selected must leave every class ineligible, not default-enabled"
        );
        // Briton (id 1) is the Albion generalist and is legitimately eligible for all 16.
        let briton = class_adapters(1, 1);
        assert!(
            briton.iter().any(|a| a.class_id == 2 && a.eligible),
            "Briton should be eligible for Armsman"
        );
        assert!(
            briton.iter().all(|a| a.eligible),
            "Briton is eligible for every Albion class"
        );
        // Highlander (id 3) is the restricted case — 8 of 16 — so the gate is provably not a
        // constant. Using Briton here would have made this assertion vacuous.
        let highlander = class_adapters(1, 3);
        let eligible = highlander.iter().filter(|a| a.eligible).count();
        assert_eq!(
            eligible, 8,
            "Highlander eligibility changed; expected 8 of 16 per the career table"
        );
        assert!(
            highlander
                .iter()
                .any(|a| a.label == "Theurgist" && !a.eligible),
            "Highlander must not be offered Theurgist"
        );
    }

    #[test]
    fn male_celt_bainshee_is_ineligible() {
        let male = class_adapters_for(3, 9, 0);
        let female = class_adapters_for(3, 9, 1);
        assert!(
            male.iter().any(|a| a.class_id == 39 && !a.eligible),
            "male Celt must not light Bainshee"
        );
        assert!(
            female.iter().any(|a| a.class_id == 39 && a.eligible),
            "female Celt must be offered Bainshee"
        );
    }

    /// An out-of-range slot resolves to nothing rather than a default.
    #[test]
    fn out_of_range_slot_has_no_class() {
        assert!(class_adapter_at(1, 1, 16).is_none());
        assert!(class_adapter_at(1, 1, 200).is_none());
        assert!(
            class_adapters(0, 1).is_empty(),
            "realm 0 is not a form context"
        );
        assert!(
            class_adapters(9, 1).is_empty(),
            "unknown realm offers nothing"
        );
    }
}
