//! Authoritative buff / debuff / CC icon list and concentration list.
//!
//! ## Provenance
//!
//! OPEN_ORACLE: `SoloDAoC` `PacketLib1110.SendUpdateIcons` (1.110+; 1.127 inherits) and
//! `PacketLib168.SendConcentrationList` (no later override found).
//!
//! **UpdateIcons 0x7F** — 1.110 layout:
//! ```text
//!   u8     entry count
//!   u8     unk
//!   u8     icons-flag (PacketLib190 `Icons`)
//!   u8     unk
//!   per entry:
//!     u8     icon index
//!     u8     list index (0xFF if not a GameSpellEffect)
//!     u8     immun / "protected by" (nonzero)
//!     u16 BE icon
//!     u16 BE remaining seconds
//!     u16 BE spell internal id (0 if not a spell effect)
//!     u8     negative (debuff / CC) flag
//!     pascal name
//!   removal: icon index + 10 zero bytes (icon id 0)
//! ```
//!
//! **ConcentrationList 0x75** — PacketLib168:
//! ```text
//!   u8 count, 3× unk 0
//!   per entry: u8 index, u8 unk, u8 concentration, u16 BE icon, pascal name, pascal owner
//! ```

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for UpdateIcons 0x7F (PacketLib1110 / 1.127).
pub const ICONS_PROVENANCE: &str = "UpdateIcons 0x7F PacketLib1110";
/// Provenance tag for ConcentrationList 0x75.
pub const CONCENTRATION_PROVENANCE: &str = "ConcentrationList 0x75 PacketLib168";

/// One live icon slot, or a server-authored clear of that index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IconSlot {
    /// Remove the icon at `index` (Fill(0,10) after the index byte).
    Cleared {
        index: u8,
    },
    Live(IconEntry),
}

/// A decoded UpdateIcons live entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconEntry {
    pub index: u8,
    pub list_index: u8,
    /// Nonzero ⇒ "protected by" (immun / disabled spell effect).
    pub immun: u8,
    pub icon: u16,
    /// Remaining duration in seconds from the packet (not a client clock).
    pub remaining_secs: u16,
    /// Spell internal id (1.110+); 0 when the effect is not a spell.
    pub spell_internal_id: u16,
    /// Packet negative flag — debuff / CC when set.
    pub negative: bool,
    pub name: String,
}

impl IconEntry {
    #[must_use]
    pub fn is_debuff_or_cc(&self) -> bool {
        self.negative
    }

    #[must_use]
    pub fn provenance(&self) -> &'static str {
        ICONS_PROVENANCE
    }
}

/// A decoded UpdateIcons (0x7F) body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateIcons {
    pub icons_flag: u8,
    pub slots: Vec<IconSlot>,
}

impl UpdateIcons {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        ICONS_PROVENANCE
    }
}

/// One concentration-list row (maintained effects).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcentrationEffect {
    pub index: u8,
    pub concentration: u8,
    pub icon: u16,
    pub name: String,
    pub owner_name: String,
}

/// A decoded ConcentrationList (0x75).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcentrationList {
    pub effects: Vec<ConcentrationEffect>,
}

impl ConcentrationList {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        CONCENTRATION_PROVENANCE
    }
}

/// Decode UpdateIcons (PacketLib1110).
pub fn decode_update_icons(payload: &[u8]) -> Result<UpdateIcons> {
    let mut r = PacketReader::new(payload);
    let count = r.u8()? as usize;
    let _unk0 = r.u8()?;
    let icons_flag = r.u8()?;
    let _unk1 = r.u8()?;
    let mut slots = Vec::with_capacity(count);
    for _ in 0..count {
        let index = r.u8()?;
        let peek = r.peek(10);
        if peek.is_ok_and(|b| b.iter().all(|&x| x == 0)) {
            let _ = r.bytes(10)?;
            slots.push(IconSlot::Cleared { index });
            continue;
        }
        let list_index = r.u8()?;
        let immun = r.u8()?;
        let icon = r.u16()?;
        let remaining_secs = r.u16()?;
        let spell_internal_id = r.u16()?;
        let negative = r.u8()? != 0;
        let name = r.pascal_string()?;
        if icon == 0 {
            slots.push(IconSlot::Cleared { index });
        } else {
            slots.push(IconSlot::Live(IconEntry {
                index,
                list_index,
                immun,
                icon,
                remaining_secs,
                spell_internal_id,
                negative,
                name,
            }));
        }
    }
    Ok(UpdateIcons { icons_flag, slots })
}

/// Encode matching `PacketLib1110.SendUpdateIcons` (tests / fixtures).
#[must_use]
pub fn encode_update_icons(u: &UpdateIcons) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(16);
    w.u8(u.slots.len() as u8).u8(0).u8(u.icons_flag).u8(0);
    for slot in &u.slots {
        match slot {
            IconSlot::Cleared { index } => {
                w.u8(*index);
                w.bytes(&[0u8; 10]);
            }
            IconSlot::Live(e) => {
                w.u8(e.index)
                    .u8(e.list_index)
                    .u8(e.immun)
                    .u16(e.icon)
                    .u16(e.remaining_secs)
                    .u16(e.spell_internal_id)
                    .u8(u8::from(e.negative))
                    .pascal_string(&e.name);
            }
        }
    }
    w.into_bytes()
}

/// Decode ConcentrationList (PacketLib168).
pub fn decode_concentration_list(payload: &[u8]) -> Result<ConcentrationList> {
    let mut r = PacketReader::new(payload);
    let count = r.u8()? as usize;
    let _ = r.u8()?;
    let _ = r.u8()?;
    let _ = r.u8()?;
    let mut effects = Vec::with_capacity(count);
    for _ in 0..count {
        let index = r.u8()?;
        let _unk = r.u8()?;
        let concentration = r.u8()?;
        let icon = r.u16()?;
        let name = r.pascal_string()?;
        let owner_name = r.pascal_string()?;
        effects.push(ConcentrationEffect {
            index,
            concentration,
            icon,
            name,
            owner_name,
        });
    }
    Ok(ConcentrationList { effects })
}

/// Encode matching `PacketLib168.SendConcentrationList`.
#[must_use]
pub fn encode_concentration_list(list: &ConcentrationList) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(16);
    w.u8(list.effects.len() as u8).u8(0).u8(0).u8(0);
    for e in &list.effects {
        w.u8(e.index)
            .u8(0)
            .u8(e.concentration)
            .u16(e.icon)
            .pascal_string(&e.name)
            .pascal_string(&e.owner_name);
    }
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_icons_roundtrip_live_and_clear() {
        let u = UpdateIcons {
            icons_flag: 1,
            slots: vec![
                IconSlot::Live(IconEntry {
                    index: 0,
                    list_index: 0,
                    immun: 0,
                    icon: 0x0122,
                    remaining_secs: 12,
                    spell_internal_id: 407,
                    negative: true,
                    name: "stun".into(),
                }),
                IconSlot::Cleared { index: 1 },
            ],
        };
        let body = encode_update_icons(&u);
        let got = decode_update_icons(&body).expect("0x7F");
        assert_eq!(got, u);
        assert_eq!(got.provenance(), ICONS_PROVENANCE);
        let IconSlot::Live(live) = &got.slots[0] else {
            panic!("expected live");
        };
        assert!(live.is_debuff_or_cc());
        assert_eq!(live.spell_internal_id, 407);
    }

    #[test]
    fn concentration_list_roundtrip() {
        let list = ConcentrationList {
            effects: vec![ConcentrationEffect {
                index: 0,
                concentration: 5,
                icon: 0x0100,
                name: "buff".into(),
                owner_name: "self".into(),
            }],
        };
        let body = encode_concentration_list(&list);
        let got = decode_concentration_list(&body).expect("0x75");
        assert_eq!(got, list);
        assert_eq!(got.provenance(), CONCENTRATION_PROVENANCE);
    }

    #[test]
    fn short_icons_payload_errors() {
        assert!(decode_update_icons(&[1, 0, 0]).is_err());
    }
}
