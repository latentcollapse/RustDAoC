//! VariousUpdate 0x16 subcode `0x05` — weapon damage / weapon skill / effective AF.
//!
//! ## Provenance
//!
//! OPEN_ORACLE `PacketLib1126.SendUpdateWeaponAndArmorStats` (1.127 inherits via 1127→1126):
//! `[0x05][entry_count=6][0x00][0x00]` then three big-endian u16 values, each written as
//! `hi, 0x00, lo, 0x00` (six "entries" = three pairs).

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

pub const SUBCODE: u8 = 0x05;
pub const PROVENANCE: &str = "VariousUpdate 0x16:0x05 SendUpdateWeaponAndArmorStats";

/// Decoded combat-stat triple for the character sheet weapon/AF rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WeaponArmorStats {
    /// Weapon damage × 100 (oracle `WeaponDamage * 100`).
    pub weapon_damage_x100: u16,
    pub weapon_skill: u16,
    /// Effective overall armor factor.
    pub armor_factor: u16,
}

impl WeaponArmorStats {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        PROVENANCE
    }

    /// Display string for `stats_weapon_damage` (two decimal places from ×100).
    #[must_use]
    pub fn weapon_damage_display(self) -> String {
        format!(
            "{}.{:02}",
            self.weapon_damage_x100 / 100,
            self.weapon_damage_x100 % 100
        )
    }
}

/// Decode VariousUpdate body when subcode is weapon/armor (`0x05`). `Ok(None)` for other subcodes.
pub fn decode(payload: &[u8]) -> Result<Option<WeaponArmorStats>> {
    if payload.first() != Some(&SUBCODE) {
        return Ok(None);
    }
    let mut r = PacketReader::new(payload);
    let _sub = r.u8()?;
    let _count = r.u8()?;
    let _subtype = r.u8()?;
    let _unk = r.u8()?;
    let weapon_damage_x100 = read_spaced_u16(&mut r)?;
    let weapon_skill = read_spaced_u16(&mut r)?;
    let armor_factor = read_spaced_u16(&mut r)?;
    Ok(Some(WeaponArmorStats {
        weapon_damage_x100,
        weapon_skill,
        armor_factor,
    }))
}

fn read_spaced_u16(r: &mut PacketReader<'_>) -> Result<u16> {
    let hi = r.u8()?;
    let _pad0 = r.u8()?;
    let lo = r.u8()?;
    let _pad1 = r.u8()?;
    Ok((u16::from(hi) << 8) | u16::from(lo))
}

/// Encode PacketLib1126 body for tests / fixtures.
#[must_use]
pub fn encode(stats: &WeaponArmorStats) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u8(SUBCODE).u8(6).u8(0).u8(0);
    write_spaced_u16(&mut w, stats.weapon_damage_x100);
    write_spaced_u16(&mut w, stats.weapon_skill);
    write_spaced_u16(&mut w, stats.armor_factor);
    w.into_bytes()
}

fn write_spaced_u16(w: &mut PacketWriter, v: u16) {
    w.u8((v >> 8) as u8).u8(0).u8((v & 0xff) as u8).u8(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_weapon_armor_stats() {
        let s = WeaponArmorStats {
            weapon_damage_x100: 1650,
            weapon_skill: 1200,
            armor_factor: 340,
        };
        let body = encode(&s);
        let got = decode(&body).unwrap().expect("0x05");
        assert_eq!(got, s);
        assert_eq!(got.weapon_damage_display(), "16.50");
        assert_eq!(got.provenance(), PROVENANCE);
    }

    #[test]
    fn other_subcode_is_not_weapon_armor() {
        assert_eq!(decode(&[0x03, 0, 0, 0]).unwrap(), None);
    }
}
