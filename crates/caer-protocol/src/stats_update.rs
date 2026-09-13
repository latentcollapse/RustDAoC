//! StatsUpdate 0xFB — character attributes / resists (OPEN_ORACLE PacketLib175).
//!
//! Two layouts share the opcode. Byte after the RA block is `0x00` for attributes and `0xFF`
//! for resists (`SendCharStatsUpdate` / `SendCharResistsUpdate`). Attribute order is
//! STR, DEX, CON, QUI, INT, PIE, EMP, CHR.

use crate::codec::PacketReader;
use crate::error::{ProtocolError, Result};

pub const PROVENANCE: &str =
    "StatsUpdate 0xFB PacketLib175 SendCharStatsUpdate/SendCharResistsUpdate";

/// Eight primary attributes in PacketLib175 write order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AttributeBlock {
    pub strength: u16,
    pub dexterity: u16,
    pub constitution: u16,
    pub quickness: u16,
    pub intelligence: u16,
    pub piety: u16,
    pub empathy: u16,
    pub charisma: u16,
}

impl AttributeBlock {
    fn from_arr(a: [u16; 8]) -> Self {
        Self {
            strength: a[0],
            dexterity: a[1],
            constitution: a[2],
            quickness: a[3],
            intelligence: a[4],
            piety: a[5],
            empathy: a[6],
            charisma: a[7],
        }
    }
}

/// Decoded attribute StatsUpdate (flag byte 0x00).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharStatsUpdate {
    pub base: AttributeBlock,
    pub buff: AttributeBlock,
    pub item: AttributeBlock,
    /// Effective totals for UI (base + buff + item + RA), not a local invent.
    pub total: AttributeBlock,
    pub max_health: u16,
    pub constitution_lost: u8,
}

/// Nine resist channels in PacketLib175 resist write order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResistBlock {
    pub crush: u16,
    pub slash: u16,
    pub thrust: u16,
    pub heat: u16,
    pub cold: u16,
    pub matter: u16,
    pub body: u16,
    pub spirit: u16,
    pub energy: u16,
}

impl ResistBlock {
    fn from_arr(a: [u16; 9]) -> Self {
        Self {
            crush: a[0],
            slash: a[1],
            thrust: a[2],
            heat: a[3],
            cold: a[4],
            matter: a[5],
            body: a[6],
            spirit: a[7],
            energy: a[8],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatsUpdate {
    Attributes(CharStatsUpdate),
    Resists(ResistBlock),
}

fn read_u16_block8(r: &mut PacketReader<'_>) -> Result<[u16; 8]> {
    let mut a = [0u16; 8];
    for slot in &mut a {
        *slot = r.u16()?;
    }
    Ok(a)
}

fn read_u8_block8(r: &mut PacketReader<'_>) -> Result<[u8; 8]> {
    let mut a = [0u8; 8];
    for slot in &mut a {
        *slot = r.u8()?;
    }
    Ok(a)
}

/// Decode StatsUpdate 0xFB. Short / wrong-flag payloads return `Err` (malformed negative).
pub fn decode(payload: &[u8]) -> Result<StatsUpdate> {
    // Attribute layout minimum: 8*2*3 bases/buffs/items + 3 pads*2 + 8+1 caps + 8+1 RA
    // + flag + conLost + maxHealth + pad ≈ 16+2+16+2+16+2+9+9+1+1+2+2 = 78 bytes.
    if payload.len() < 40 {
        return Err(ProtocolError::UnexpectedEof {
            offset: 0,
            needed: 40usize.saturating_sub(payload.len()),
        });
    }
    let mut r = PacketReader::new(payload);
    let base = read_u16_block8(&mut r)?;
    let _pad0 = r.u16()?;
    let buff = read_u16_block8(&mut r)?;
    let _pad1 = r.u16()?;
    let item = read_u16_block8(&mut r)?;
    let _pad2 = r.u16()?;
    let _caps = read_u8_block8(&mut r)?;
    let _cap_pad = r.u8()?;
    let ra = read_u8_block8(&mut r)?;
    let _ra_pad = r.u8()?;
    let flag = r.u8()?;
    if flag == 0xFF {
        // Resist path reuses earlier shorts differently in PacketLib175; for the resist
        // packet the first blocks are racial resists. Re-parse from scratch.
        return decode_resists(payload);
    }
    if flag != 0x00 {
        return Err(ProtocolError::BadString("stats_update_flag"));
    }
    let constitution_lost = r.u8()?;
    let max_health = r.u16()?;
    let _pad3 = r.u16().unwrap_or(0);
    let mut total = [0u16; 8];
    for i in 0..8 {
        total[i] = base[i]
            .saturating_add(buff[i])
            .saturating_add(item[i])
            .saturating_add(u16::from(ra[i]));
    }
    Ok(StatsUpdate::Attributes(CharStatsUpdate {
        base: AttributeBlock::from_arr(base),
        buff: AttributeBlock::from_arr(buff),
        item: AttributeBlock::from_arr(item),
        total: AttributeBlock::from_arr(total),
        max_health,
        constitution_lost,
    }))
}

fn decode_resists(payload: &[u8]) -> Result<StatsUpdate> {
    // PacketLib175 resist write: 9 racial u16, pad, … — first nine shorts are racial totals
    // used for UI until a fuller decode is needed.
    if payload.len() < 18 {
        return Err(ProtocolError::UnexpectedEof {
            offset: 0,
            needed: 18usize.saturating_sub(payload.len()),
        });
    }
    let mut r = PacketReader::new(payload);
    let mut a = [0u16; 9];
    for slot in &mut a {
        *slot = r.u16()?;
    }
    Ok(StatsUpdate::Resists(ResistBlock::from_arr(a)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attr_payload() -> Vec<u8> {
        let mut v = Vec::new();
        // base STR..CHR
        for n in [40u16, 41, 42, 43, 44, 45, 46, 47] {
            v.extend_from_slice(&n.to_be_bytes());
        }
        v.extend_from_slice(&0u16.to_be_bytes());
        // buffs
        for _ in 0..8 {
            v.extend_from_slice(&1u16.to_be_bytes());
        }
        v.extend_from_slice(&0u16.to_be_bytes());
        // items
        for _ in 0..8 {
            v.extend_from_slice(&2u16.to_be_bytes());
        }
        v.extend_from_slice(&0u16.to_be_bytes());
        // caps + pad
        v.extend_from_slice(&[10u8; 8]);
        v.push(0);
        // RA + pad
        v.extend_from_slice(&[3u8; 8]);
        v.push(0);
        // flag, conLost, maxHealth, pad
        v.push(0x00);
        v.push(5);
        v.extend_from_slice(&500u16.to_be_bytes());
        v.extend_from_slice(&0u16.to_be_bytes());
        v
    }

    #[test]
    fn decodes_attribute_totals() {
        let s = decode(&attr_payload()).unwrap();
        let StatsUpdate::Attributes(a) = s else {
            panic!("expected attributes");
        };
        assert_eq!(a.base.strength, 40);
        assert_eq!(a.total.strength, 40 + 1 + 2 + 3);
        assert_eq!(a.max_health, 500);
        assert_eq!(a.constitution_lost, 5);
    }

    #[test]
    fn malformed_short_payload_is_err() {
        assert!(decode(&[0u8; 8]).is_err());
    }

    #[test]
    fn bad_flag_is_err() {
        let mut p = attr_payload();
        // flag sits before last 1+1+2+2 bytes
        let flag_i = p.len() - 6;
        p[flag_i] = 0x55;
        assert!(decode(&p).is_err());
    }
}
