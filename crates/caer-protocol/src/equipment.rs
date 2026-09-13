//! `EquipmentUpdate` (0x15) — what a living thing is visibly wearing and wielding.
//!
//! Long listed as blocked on a MITM capture, which was not actually necessary: the oracle's
//! `PacketLib1124::SendLivingEquipmentUpdate` specifies the wire form exactly, and it is the same
//! 1124-era layout our session already speaks. Decoding it unblocks three visible defects at once —
//! NPC faces, NPC armour tiers, and the player's untextured white body.
//!
//! ## Wire form
//!
//! ```text
//!   u16 BE  object id
//!   u8      visible active weapon slots
//!   u8      current speed
//!   u8      visibility: bit0 cloak hidden, bit1 helm hidden
//!   u8      bit0 hood up; bits 4..7 active quiver slot
//!   u8      item count
//!   per item:
//!     u8    slot   (bit 0x80 = "new emblem", only ever set on LEFTHAND/CLOAK)
//!     u16   model, carrying THREE flags in its top bits
//!     u8    extension      — ONLY for armour slots (slot > RANGED or slot < RIGHTHAND)
//!     u16|u8 texture       — u16 if model & 0x8000, u8 if model & 0x4000, absent otherwise
//!     u16   effect         — only if model & 0x2000
//! ```
//!
//! **The per-item record is variable length, and the model word decides its own size.** Reading a
//! fixed-size record desynchronises every later item, so the flags are stripped and acted on before
//! anything else is read. `model & 0x1FFF` is the real model id; the top three bits are never part
//! of it.

use crate::codec::PacketReader;
use crate::error::Result;

/// Inventory slots, from the oracle's `GlobalConstants`. Only the ones this decoder branches on or
/// that the renderer needs to place a mesh are named.
pub mod slot {
    pub const RIGHTHAND: u8 = 10;
    pub const LEFTHAND: u8 = 11;
    pub const TWOHAND: u8 = 12;
    pub const RANGED: u8 = 13;
    pub const HELM: u8 = 21;
    pub const HANDS: u8 = 22;
    pub const FEET: u8 = 23;
    pub const TORSO: u8 = 25;
    pub const CLOAK: u8 = 26;
    pub const LEGS: u8 = 27;
    pub const ARMS: u8 = 28;
}

/// One visible worn or wielded item.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisibleItem {
    /// Slot number, with the emblem bit already stripped.
    pub slot: u8,
    /// Model id (`& 0x1FFF` — the flag bits are removed).
    pub model: u16,
    /// Armour tier / model variant. `None` for weapon slots, which do not carry one.
    pub extension: Option<u8>,
    /// Dye or emblem colour, when the item has one.
    pub texture: Option<u16>,
    /// Visual effect id (glow), when the item has one.
    pub effect: Option<u16>,
    /// The `0x80` slot bit: this item's texture is an emblem rather than a dye.
    pub new_emblem: bool,
}

/// A decoded 0x15.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EquipmentUpdate {
    pub object_id: u16,
    pub active_weapon_slots: u8,
    pub speed: u8,
    pub cloak_hidden: bool,
    pub helm_hidden: bool,
    pub hood_up: bool,
    pub active_quiver: u8,
    pub items: Vec<VisibleItem>,
}

impl EquipmentUpdate {
    /// The item in a given slot, if worn.
    #[must_use]
    pub fn item(&self, slot: u8) -> Option<&VisibleItem> {
        self.items.iter().find(|i| i.slot == slot)
    }

    /// Create-preview dress: one `objects.csv` id whose row already names Body/Arms/Legs/Boots
    /// skins. The binder expands that object across slots; we only have to put it on the torso.
    #[must_use]
    pub fn from_armor_object(model: u16) -> Self {
        Self {
            items: vec![VisibleItem {
                slot: slot::TORSO,
                model,
                extension: Some(1),
                ..VisibleItem::default()
            }],
            ..Self::default()
        }
    }
}

/// Does this slot carry an `Extension` byte?
///
/// The oracle's condition is `SlotPosition > RANGED || SlotPosition < RIGHTHAND` — i.e. everything
/// EXCEPT the four weapon slots 10..=13. Getting this backwards shifts every subsequent field.
#[must_use]
fn has_extension(slot: u8) -> bool {
    !(slot::RIGHTHAND..=slot::RANGED).contains(&slot)
}

/// Decode an `EquipmentUpdate` payload.
pub fn decode(payload: &[u8]) -> Result<EquipmentUpdate> {
    let mut c = PacketReader::new(payload);
    let object_id = c.u16()?;
    let active_weapon_slots = c.u8()?;
    let speed = c.u8()?;
    let vis = c.u8()?;
    let hood_quiver = c.u8()?;
    let count = c.u8()?;

    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let raw_slot = c.u8()?;
        let new_emblem = raw_slot & 0x80 != 0;
        let slot = raw_slot & 0x7F;
        let raw_model = c.u16()?;

        // Strip the flags BEFORE reading anything else — they determine the record's own length.
        let has_wide_texture = raw_model & 0x8000 != 0;
        let has_byte_texture = raw_model & 0x4000 != 0;
        let has_effect = raw_model & 0x2000 != 0;
        let model = raw_model & 0x1FFF;

        let extension = if has_extension(slot) {
            Some(c.u8()?)
        } else {
            None
        };
        let texture = if has_wide_texture {
            Some(c.u16()?)
        } else if has_byte_texture {
            Some(u16::from(c.u8()?))
        } else {
            None
        };
        let effect = if has_effect { Some(c.u16()?) } else { None };

        items.push(VisibleItem {
            slot,
            model,
            extension,
            texture,
            effect,
            new_emblem,
        });
    }

    Ok(EquipmentUpdate {
        object_id,
        active_weapon_slots,
        speed,
        cloak_hidden: vis & 0x01 != 0,
        helm_hidden: vis & 0x02 != 0,
        hood_up: hood_quiver & 0x01 != 0,
        active_quiver: (hood_quiver >> 4) & 0x0F,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::PacketWriter;

    /// Encode exactly as the oracle does, so the test exercises the real branching.
    fn item(
        w: &mut PacketWriter,
        slot: u8,
        model: u16,
        ext: Option<u8>,
        texture: Option<u16>,
        effect: Option<u16>,
    ) {
        let mut m = model & 0x1FFF;
        let wide = texture.is_some_and(|t| t & !0xFF != 0);
        let narrow = texture.is_some_and(|t| t & !0xFF == 0 && t & 0xFF != 0);
        if wide {
            m |= 0x8000;
        } else if narrow {
            m |= 0x4000;
        }
        if effect.is_some() {
            m |= 0x2000;
        }
        w.u8(slot).u16(m);
        if let Some(e) = ext {
            w.u8(e);
        }
        if wide {
            w.u16(texture.unwrap());
        } else if narrow {
            w.u8(texture.unwrap() as u8);
        }
        if let Some(e) = effect {
            w.u16(e);
        }
    }

    #[test]
    fn decodes_header_flags() {
        let mut w = PacketWriter::new();
        w.u16(0x1234).u8(0x0A).u8(191).u8(0x03).u8(0x21).u8(0);
        let e = decode(w.as_slice()).unwrap();
        assert_eq!(e.object_id, 0x1234);
        assert_eq!(e.speed, 191);
        assert!(e.cloak_hidden && e.helm_hidden, "bit0 cloak, bit1 helm");
        assert!(e.hood_up);
        assert_eq!(e.active_quiver, 2, "quiver is bits 4..7");
    }

    /// The per-item record is variable length and the model word decides its size. A mixed run is
    /// the case that catches a fixed-size read: everything after the first item desynchronises.
    #[test]
    fn variable_length_items_stay_in_sync() {
        let mut w = PacketWriter::new();
        w.u16(1).u8(0).u8(0).u8(0).u8(0).u8(4);
        // Weapon: no extension, no texture, no effect.
        item(&mut w, slot::RIGHTHAND, 0x0123, None, None, None);
        // Armour with a byte dye.
        item(&mut w, slot::TORSO, 0x0456, Some(3), Some(0x2A), None);
        // Cloak with a wide emblem AND an effect.
        item(
            &mut w,
            slot::CLOAK,
            0x0789,
            Some(0),
            Some(0x1234),
            Some(0x00AB),
        );
        // Armour with an effect but no texture.
        item(&mut w, slot::LEGS, 0x0111, Some(7), None, Some(0x0002));

        let e = decode(w.as_slice()).unwrap();
        assert_eq!(e.items.len(), 4);

        let rh = e.item(slot::RIGHTHAND).unwrap();
        assert_eq!(rh.model, 0x0123);
        assert_eq!(rh.extension, None, "weapon slots carry no extension");

        let torso = e.item(slot::TORSO).unwrap();
        assert_eq!(
            (torso.model, torso.extension, torso.texture),
            (0x0456, Some(3), Some(0x2A))
        );

        let cloak = e.item(slot::CLOAK).unwrap();
        assert_eq!(
            (cloak.model, cloak.texture, cloak.effect),
            (0x0789, Some(0x1234), Some(0x00AB))
        );

        // The LAST item is the real proof: it is only correct if every earlier record was sized right.
        let legs = e.item(slot::LEGS).unwrap();
        assert_eq!(
            (legs.model, legs.extension, legs.texture, legs.effect),
            (0x0111, Some(7), None, Some(0x0002))
        );
    }

    /// Flag bits must not leak into the model id — 0x1FFF is the real range.
    #[test]
    fn model_flags_are_stripped() {
        let mut w = PacketWriter::new();
        w.u16(1).u8(0).u8(0).u8(0).u8(0).u8(1);
        item(&mut w, slot::HELM, 0x1FFF, Some(1), Some(0xFFFF), Some(9));
        let e = decode(w.as_slice()).unwrap();
        assert_eq!(
            e.items[0].model, 0x1FFF,
            "model must not carry 0x8000/0x4000/0x2000"
        );
    }

    /// The 0x80 slot bit marks an emblem and is not part of the slot number.
    #[test]
    fn emblem_bit_is_split_from_the_slot() {
        let mut w = PacketWriter::new();
        w.u16(1).u8(0).u8(0).u8(0).u8(0).u8(1);
        item(
            &mut w,
            slot::CLOAK | 0x80,
            0x0010,
            Some(0),
            Some(0x0301),
            None,
        );
        let e = decode(w.as_slice()).unwrap();
        assert_eq!(e.items[0].slot, slot::CLOAK);
        assert!(e.items[0].new_emblem);
    }

    #[test]
    fn a_truncated_packet_errors() {
        assert!(decode(&[0x00, 0x01, 0x00]).is_err());
    }
}
