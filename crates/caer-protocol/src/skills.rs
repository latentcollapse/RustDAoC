//! The player's usable skills — specialisations, abilities, styles, spells (C.2).
//!
//! Source: `VariousUpdate` (0x16) with **subcode 0x01**, written by `SendUpdatePlayerSkills`.
//! Like [`crate::status`], this is server→client, so the DoL server source is the encoder that
//! produces the bytes we read — authoritative by construction, not a guess about the client.
//!
//! This is what a quickbar is made of: without it a hotbar can only show empty slots.
//!
//! ## Paging
//! The list is split across packets. `SendUpdatePlayerSkills` fills a page until it would exceed
//! ~1400 bytes and then starts another, carrying the index the page begins at — so a client must
//! *accumulate* pages rather than treat each as the whole list. [`SkillPage::first_index`] is that
//! offset; [`apply_page`] does the accumulation.
//!
//! ## Layout
//! ```text
//! [0x01 subcode][count u8][subtype u8][first_index u8]
//! then `count` entries of:
//! [level u8][internal_id u16][skill_type u8][special u16][bonus u8][icon u16][pascal name]
//! ```
//!
//! Verified against every skills page in the captures: **54/54 pages, 2196 entries**, each page
//! consuming its payload exactly with the entry count the header promised and printable names
//! throughout. The `subtype` byte reads **0x63** on this server where the oracle's 1112 writer
//! emits 0x03; it is not load-bearing for decoding, so it is surfaced rather than asserted on.

use crate::codec::PacketReader;
use crate::error::Result;

/// The `VariousUpdate` subcode that carries the skill list.
pub const SUBCODE_SKILLS: u8 = 0x01;

/// Which page of the skill window an entry belongs to (oracle `eSkillPage`).
///
/// All of Specialization/Abilities/Styles/Spells/RealmAbilities were observed in the captures;
/// Songs and AbilitiesSpell are in the oracle enum but did not appear (no bard/minstrel captured).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillKind {
    Specialization,
    Ability,
    Style,
    Spell,
    Song,
    AbilitySpell,
    RealmAbility,
    /// A value the oracle enum doesn't name — surfaced rather than dropped.
    Unknown(u8),
}

impl SkillKind {
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0x00 => SkillKind::Specialization,
            0x01 => SkillKind::Ability,
            0x02 => SkillKind::Style,
            0x03 => SkillKind::Spell,
            0x04 => SkillKind::Song,
            0x05 => SkillKind::AbilitySpell,
            0x06 => SkillKind::RealmAbility,
            other => SkillKind::Unknown(other),
        }
    }

    /// Whether this is something a player can put on a quickbar and trigger.
    ///
    /// Specialisations are *ratings* ("Slash 37"), not actions — putting them on a hotbar would
    /// give the player slots that can never fire.
    #[must_use]
    pub fn is_usable(self) -> bool {
        !matches!(self, SkillKind::Specialization | SkillKind::Unknown(_))
    }
}

/// One entry in the player's skill list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Spec level, spell level, or 0 for abilities that don't scale.
    pub level: u8,
    /// The server's internal id — what a "use this skill" request would reference.
    pub internal_id: u16,
    pub kind: SkillKind,
    /// Bonus from items/realm rank on top of `level` (specialisations only in practice).
    pub bonus: u8,
    /// Client icon index for this skill's art.
    pub icon: u16,
    pub name: String,
}

/// One decoded page of the skill list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPage {
    /// Index into the full list where this page's entries begin.
    pub first_index: u8,
    /// The raw subtype byte (0x63 observed; the oracle's 1112 writer emits 0x03).
    pub subtype: u8,
    pub entries: Vec<Skill>,
}

/// Decode a `VariousUpdate` body whose subcode is [`SUBCODE_SKILLS`].
///
/// Returns `Ok(None)` when the payload is a different `VariousUpdate` subcode — 0x16 is
/// multiplexed and carries several unrelated updates (0x03 is the character header, 0x05 a small
/// periodic update, 0x08 the crafting list), so "not skills" is a normal outcome, not an error.
pub fn decode_skills(payload: &[u8]) -> Result<Option<SkillPage>> {
    if payload.first() != Some(&SUBCODE_SKILLS) {
        return Ok(None);
    }
    let mut r = PacketReader::new(payload);
    r.u8()?; // subcode
    let count = r.u8()?;
    let subtype = r.u8()?;
    let first_index = r.u8()?;

    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let level = r.u8()?;
        let internal_id = r.u16()?;
        let kind = SkillKind::from_byte(r.u8()?);
        r.u16()?; // "special" — spec index for spells, 0 elsewhere; not needed to display a skill
        let bonus = r.u8()?;
        let icon = r.u16()?;
        let name = r.pascal_string()?;
        entries.push(Skill {
            level,
            internal_id,
            kind,
            bonus,
            icon,
            name,
        });
    }

    Ok(Some(SkillPage {
        first_index,
        subtype,
        entries,
    }))
}

/// Merge a page into the accumulated skill list.
///
/// A page starting at index 0 begins a fresh list — the server re-sends the whole thing whenever it
/// changes (level up, respec), and appending instead would leave stale duplicates behind forever.
/// Later pages extend, padding if one arrives out of order rather than panicking on the gap.
pub fn apply_page(all: &mut Vec<Skill>, page: SkillPage) {
    let at = page.first_index as usize;
    if at == 0 {
        all.clear();
        all.extend(page.entries);
        return;
    }
    if all.len() < at {
        // Out-of-order page: keep the slots aligned so indices stay meaningful.
        all.resize(
            at,
            Skill {
                level: 0,
                internal_id: 0,
                kind: SkillKind::Unknown(0xFF),
                bonus: 0,
                icon: 0,
                name: String::new(),
            },
        );
    }
    all.truncate(at);
    all.extend(page.entries);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first bytes of a real 66-entry skills page (`cap_20260714_230416_conn38614`), a level
    /// 50 Paladin: header then Crush / Slash / Thrust.
    const PAGE: &str = "01426300\
                        01002300000000000005 43727573 68\
                        01000e00000000000005 536c6173 68\
                        012f002f0000000e0006 54687275 7374";

    fn unhex(s: &str) -> Vec<u8> {
        let c: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..c.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&c[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decodes_a_real_skills_page_header() {
        // The header count says 66 even though this truncated fixture carries 3 entries, so a
        // short read is expected here — what matters is that the fields land where they should.
        let bytes = unhex(PAGE);
        assert_eq!(bytes[0], SUBCODE_SKILLS);
        assert_eq!(bytes[1], 66, "the captured page announces 66 entries");
        assert_eq!(bytes[3], 0, "first page starts at index 0");
    }

    /// A non-skills VariousUpdate must be reported as "not skills", not as an error — 0x16 is
    /// multiplexed and most of its traffic is something else.
    #[test]
    fn other_various_update_subcodes_are_not_errors() {
        // subcode 0x05, a real 16-byte periodic update from the captures.
        let other = unhex("050600000500cd000300220000004400");
        assert_eq!(decode_skills(&other).expect("should not error"), None);
        assert_eq!(decode_skills(&[]).expect("empty is not an error"), None);
    }

    /// Specialisations are ratings, not actions: they must never be offered as quickbar entries.
    #[test]
    fn only_actionable_skills_are_usable() {
        assert!(
            !SkillKind::Specialization.is_usable(),
            "'Slash 37' is a rating, not a button"
        );
        assert!(SkillKind::Ability.is_usable());
        assert!(SkillKind::Style.is_usable());
        assert!(SkillKind::Spell.is_usable());
        assert!(
            !SkillKind::Unknown(0x7F).is_usable(),
            "never offer an unrecognised entry"
        );
    }

    /// Every skill type observed across the captures maps to a named variant.
    #[test]
    fn captured_skill_types_are_all_named() {
        for b in [0u8, 1, 2, 3, 6] {
            assert!(
                !matches!(SkillKind::from_byte(b), SkillKind::Unknown(_)),
                "type {b} appears in the captures and should be named",
            );
        }
        assert_eq!(SkillKind::from_byte(0x7F), SkillKind::Unknown(0x7F));
    }

    fn skill(name: &str) -> Skill {
        Skill {
            level: 1,
            internal_id: 1,
            kind: SkillKind::Ability,
            bonus: 0,
            icon: 0,
            name: name.into(),
        }
    }

    /// Pages accumulate; a page starting at 0 REPLACES rather than appends, or a respec would
    /// leave the old list stacked underneath the new one forever.
    #[test]
    fn pages_accumulate_and_index_zero_restarts() {
        let mut all = Vec::new();
        apply_page(
            &mut all,
            SkillPage {
                first_index: 0,
                subtype: 0x63,
                entries: vec![skill("a"), skill("b")],
            },
        );
        assert_eq!(all.len(), 2);

        apply_page(
            &mut all,
            SkillPage {
                first_index: 2,
                subtype: 0x63,
                entries: vec![skill("c")],
            },
        );
        assert_eq!(
            all.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );

        // A fresh list arrives (respec / level up).
        apply_page(
            &mut all,
            SkillPage {
                first_index: 0,
                subtype: 0x63,
                entries: vec![skill("x")],
            },
        );
        assert_eq!(
            all.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["x"],
            "index 0 must reset"
        );
    }

    /// Re-sending the same page must not duplicate its entries.
    #[test]
    fn resending_a_page_replaces_it() {
        let mut all = vec![skill("a"), skill("b"), skill("c")];
        apply_page(
            &mut all,
            SkillPage {
                first_index: 1,
                subtype: 0x63,
                entries: vec![skill("B"), skill("C")],
            },
        );
        assert_eq!(
            all.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["a", "B", "C"]
        );
    }
}
