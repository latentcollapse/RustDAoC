//! The player's own character sheet — `VariousUpdate` (0x16) subcode `0x03`.
//!
//! This is where level, realm rank, guild and titles come from. Without it the client's own summary
//! window has nothing true to put in `stats_level`, `realm_level`, `realm_rank`, `master_rank` or
//! `summary_title`, so those controls render empty however well the skin is wired.
//!
//! ## Layout (oracle `PacketLib175::SendUpdatePlayer`; no 1112/1124 override exists)
//!
//! The header is `[0x03 subcode][0x0e entry count][0x00 subtype][0x00 unk]`, then an ALTERNATING
//! run of single bytes and Pascal strings. The alternation is the whole trick, and it is not
//! decorative — `MaxHealth` is split into two bytes that sit on OPPOSITE SIDES of the class-name
//! string:
//!
//! ```text
//!   u8  level
//!   str name
//!   u8  maxhealth >> 8        <- high byte
//!   str salutation (class)
//!   u8  maxhealth & 0xFF      <- low byte, after a string
//!   str profession title
//!   u8  unk
//!   str class title
//!   u8  realm level
//!   str realm rank title
//!   u8  realm specialty points
//!   str base class name
//!   u8  house >> 8
//!   str guild name
//!   u8  house & 0xFF
//!   str last name
//!   u8  master level
//!   str race name
//!   u8  unk
//!   str guild rank title
//!   u8  unk
//!   str crafting skill name
//! ```
//!
//! Reading it as a flat struct — or assuming the two `MaxHealth` halves are adjacent — desynchronises
//! every field after the second string, which is the failure mode this comment exists to prevent.

use crate::codec::PacketReader;
use crate::error::Result;

/// Subcode of `VariousUpdate` that carries the character sheet.
pub const SUBCODE: u8 = 0x03;

/// The player's own sheet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CharacterSheet {
    pub level: u8,
    pub name: String,
    /// Reassembled from the two halves the packet separates with a string.
    pub max_health: u16,
    /// `Salutation` — the class name as the server words it.
    pub class_name: String,
    pub profession: String,
    /// The class's level title ("Armsman", "Veteran Armsman", …).
    pub title: String,
    pub realm_level: u8,
    pub realm_rank_title: String,
    pub realm_specialty_points: u8,
    pub base_class: String,
    pub guild_name: String,
    pub last_name: String,
    /// Master level (0 when the character has none).
    pub master_level: u8,
    pub race_name: String,
    pub guild_rank: String,
    pub crafting_skill: String,
    /// Champion level as the oracle writes it (`ChampionLevel+1`), or `0` when not a champion /
    /// when the 1.79 tail is absent. Skin adapter: `summary_champ_level`.
    pub champion_level: u8,
    /// Champion title string from the 1.79 sheet tail (empty when absent).
    pub champion_title: String,
}

/// Decode a `VariousUpdate` payload as a character sheet.
///
/// `Ok(None)` when the payload is a different subcode — 0x16 is multiplexed, and other subcodes are
/// legitimately not-a-sheet rather than a decode failure.
pub fn decode(payload: &[u8]) -> Result<Option<CharacterSheet>> {
    if payload.first() != Some(&SUBCODE) {
        return Ok(None);
    }
    let mut c = PacketReader::new(payload);
    c.u8()?; // subcode
    c.u8()?; // entry count
    c.u8()?; // subtype
    c.u8()?; // unk

    let level = c.u8()?;
    let name = c.pascal_string()?;
    let health_hi = c.u8()?;
    let class_name = c.pascal_string()?;
    let health_lo = c.u8()?;
    let profession = c.pascal_string()?;
    c.u8()?; // unk
    let title = c.pascal_string()?;
    let realm_level = c.u8()?;
    let realm_rank_title = c.pascal_string()?;
    let realm_specialty_points = c.u8()?;
    let base_class = c.pascal_string()?;
    c.u8()?; // house number, high byte
    let guild_name = c.pascal_string()?;
    c.u8()?; // house number, low byte
    let last_name = c.pascal_string()?;
    let master_level = c.u8()?;
    let race_name = c.pascal_string()?;
    c.u8()?; // unk
    let guild_rank = c.pascal_string()?;
    c.u8()?; // unk
             // The tail (crafting skill onward) is optional in practice: older/!crafting characters have
             // been seen to end the packet here, so a missing string is not a failure.
    let crafting_skill = c.pascal_string().unwrap_or_default();

    // PacketLib175+ optional tail: craft title, ML title, custom title; PacketLib179 adds CL.
    // Older truncations stop after crafting_skill — never invent champion fields.
    let mut champion_level = 0u8;
    let mut champion_title = String::new();
    if c.remaining() > 0 {
        let _ = c.u8(); // unk before craft title
        let _craft_title = c.pascal_string().unwrap_or_default();
        if c.remaining() > 0 {
            let _ = c.u8();
            let _ml_title = c.pascal_string().unwrap_or_default();
        }
        if c.remaining() > 0 {
            let _ = c.u8();
            let _custom_title = c.pascal_string().unwrap_or_default();
        }
        if c.remaining() > 0 {
            champion_level = c.u8().unwrap_or(0);
            champion_title = c.pascal_string().unwrap_or_default();
        }
    }

    Ok(Some(CharacterSheet {
        level,
        name,
        max_health: (u16::from(health_hi) << 8) | u16::from(health_lo),
        class_name,
        profession,
        title,
        realm_level,
        realm_rank_title,
        realm_specialty_points,
        base_class,
        guild_name,
        last_name,
        master_level,
        race_name,
        guild_rank,
        crafting_skill,
        champion_level,
        champion_title,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::PacketWriter;

    /// Build a sheet packet the way the oracle writes one.
    fn encode(level: u8, name: &str, max_health: u16, realm_level: u8, ml: u8) -> Vec<u8> {
        let mut w = PacketWriter::new();
        w.u8(SUBCODE).u8(0x0e).u8(0x00).u8(0x00);
        w.u8(level).pascal_string(name);
        w.u8((max_health >> 8) as u8).pascal_string("Armsman");
        w.u8((max_health & 0xFF) as u8)
            .pascal_string("Defenders of Albion");
        w.u8(0).pascal_string("Veteran Armsman");
        w.u8(realm_level).pascal_string("Guardian");
        w.u8(3).pascal_string("Fighter");
        w.u8(0).pascal_string("Knights of Cotswold");
        w.u8(0).pascal_string("Ironhand");
        w.u8(ml).pascal_string("Highlander");
        w.u8(0).pascal_string("Master");
        w.u8(0).pascal_string("Weaponcraft");
        w.as_slice().to_vec()
    }

    #[test]
    fn decodes_the_fields_the_summary_window_needs() {
        let pkt = encode(42, "Lilillyn", 1337, 5, 2);
        let s = decode(&pkt).unwrap().expect("should decode");
        assert_eq!(s.level, 42);
        assert_eq!(s.name, "Lilillyn");
        assert_eq!(s.realm_level, 5);
        assert_eq!(s.master_level, 2);
        assert_eq!(s.realm_rank_title, "Guardian");
        assert_eq!(s.title, "Veteran Armsman");
        assert_eq!(s.race_name, "Highlander");
        assert_eq!(s.guild_name, "Knights of Cotswold");
    }

    /// The two MaxHealth halves sit on opposite sides of a string. Reassembling them wrongly — or
    /// assuming they are adjacent — desynchronises every later field, so this is asserted directly.
    #[test]
    fn max_health_reassembles_across_the_intervening_string() {
        let s = decode(&encode(1, "x", 0x0539, 0, 0)).unwrap().unwrap();
        assert_eq!(
            s.max_health, 0x0539,
            "high and low bytes are split by the class name"
        );
        // And the fields AFTER the split are still aligned.
        assert_eq!(s.profession, "Defenders of Albion");
    }

    /// 0x16 is multiplexed; another subcode is not a failure.
    #[test]
    fn a_different_subcode_is_not_a_sheet() {
        assert_eq!(decode(&[0x01, 0, 0, 0]).unwrap(), None);
    }

    #[test]
    fn a_truncated_packet_errors_rather_than_inventing_fields() {
        assert!(decode(&[SUBCODE, 0x0e, 0x00]).is_err());
    }
}
