//! `InventoryUpdate` (0x02) — bag / worn / vault slot contents for the local player.
//!
//! ## Provenance
//!
//! REQ-006: inventory adapters resolve only from real packet state. This packet carries item
//! **identity** (slot, model, name, extension, …) for the player's bags and worn gear.
//!
//! ## Honest protocol note (chair-relevant)
//!
//! `InventoryUpdate` does **not** carry an avatar `object_id`. Visible mesh / armour-tier changes
//! still come from `EquipmentUpdate` (0x15). Equipping is a paired server response (0x02 + 0x15);
//! asserting avatar pixels from an inventory-struct mutation alone is the REQ-020 fake this slice
//! must refuse.
//!
//! ## Wire form (1.127 = PacketLib189 header + PacketLib1124 `WriteItemData`)
//!
//! ```text
//!   u8   item count
//!   u8   unused (0 for player; NPC speed in older comments)
//!   u8   visibility: bit0 cloak invisible, bit1 helm invisible
//!   u8   bit0 hood up; bits 4..7 active quiver  (or house-vault index)
//!   u8   visible active weapon slots
//!   u8   window type (eInventoryWindowType)
//!   per item:
//!     u8   slot
//!     then either 24 zero bytes (empty slot — no name)
//!     or PacketLib1124 item body:
//!       u16 BE  unique id (0 in captures)
//!       u8      level
//!       u8      value1 / value2  (DPS/count/etc. by object type)
//!       u8      hand << 6  (or garden DPS)
//!       u8      (type_damage << 6) | object_type
//!       u8      unk (1.112)
//!       u16 BE  weight
//!       u8      condition %, durability %, quality %, bonus %
//!       u8      bonus level (1.109)
//!       u16 BE  model
//!       u8      extension (armour tier)
//!       u16 BE  emblem or color
//!       u8      flag  (bit0 new emblem, bit1 salvage, bit2 craft, bit3/4 charge spells)
//!       [optional spell icon + pascal name per flag bits]
//!       u16 BE  effect
//!       pascal  name
//! ```

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for UI / scenario asserts (REQ-021 / SCN-06).
pub const PROVENANCE: &str = "InventoryUpdate 0x02";

/// Empty-slot filler length written by `PacketLib1124.WriteItemData(null)`.
const EMPTY_ITEM_BYTES: usize = 24;

/// `eInventoryWindowType` (OPEN_ORACLE `IPacketLib.cs`). Vault/consignment pages use these codes
/// on InventoryUpdate 0x02; they are not a separate opcode.
pub mod window_type {
    pub const UPDATE: u8 = 0x00;
    pub const EQUIPMENT: u8 = 0x01;
    pub const INVENTORY: u8 = 0x02;
    pub const PLAYER_VAULT: u8 = 0x03;
    pub const HOUSE_VAULT: u8 = 0x04;
    pub const CONSIGNMENT_OWNER: u8 = 0x05;
    pub const CONSIGNMENT_VIEWER: u8 = 0x06;
    pub const HORSE_BAGS: u8 = 0x07;
}

/// One inventory slot entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryItem {
    pub slot: u8,
    /// `None` = server cleared the slot (24-byte zero fill).
    pub item: Option<ItemData>,
}

/// Decoded non-empty item body (PacketLib1124).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemData {
    pub unique_id: u16,
    pub level: u8,
    pub value1: u8,
    pub value2: u8,
    pub hand_byte: u8,
    pub object_type_byte: u8,
    pub unk_1112: u8,
    pub weight: u16,
    pub condition_pct: u8,
    pub durability_pct: u8,
    pub quality: u8,
    pub bonus: u8,
    pub bonus_level: u8,
    pub model: u16,
    pub extension: u8,
    pub color_or_emblem: u16,
    pub flag: u8,
    pub effect: u16,
    pub name: String,
}

/// A decoded InventoryUpdate (0x02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryUpdate {
    pub unused_speed: u8,
    pub cloak_invisible: bool,
    pub helm_invisible: bool,
    pub hood_up: bool,
    pub active_quiver: u8,
    pub active_weapon_slots: u8,
    pub window_type: u8,
    pub items: Vec<InventoryItem>,
}

impl InventoryUpdate {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }

    #[must_use]
    pub fn is_vault_or_consignment(&self) -> bool {
        matches!(
            self.window_type,
            window_type::PLAYER_VAULT
                | window_type::HOUSE_VAULT
                | window_type::CONSIGNMENT_OWNER
                | window_type::CONSIGNMENT_VIEWER
        )
    }

    /// Item currently occupying `slot`, if any.
    #[must_use]
    pub fn item(&self, slot: u8) -> Option<&ItemData> {
        self.items
            .iter()
            .find(|i| i.slot == slot)
            .and_then(|i| i.item.as_ref())
    }
}

/// Decode a 1.127 InventoryUpdate payload.
pub fn decode(payload: &[u8]) -> Result<InventoryUpdate> {
    let mut r = PacketReader::new(payload);
    let count = r.u8()? as usize;
    let unused_speed = r.u8()?;
    let vis = r.u8()?;
    let hood_quiver = r.u8()?;
    let active_weapon_slots = r.u8()?;
    let window_type = r.u8()?;

    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let slot = r.u8()?;
        let item = decode_item_body(&mut r)?;
        items.push(InventoryItem { slot, item });
    }

    Ok(InventoryUpdate {
        unused_speed,
        cloak_invisible: vis & 0x01 != 0,
        helm_invisible: vis & 0x02 != 0,
        hood_up: hood_quiver & 0x01 != 0,
        active_quiver: (hood_quiver >> 4) & 0x0F,
        active_weapon_slots,
        window_type,
        items,
    })
}

fn decode_item_body(r: &mut PacketReader<'_>) -> Result<Option<ItemData>> {
    // Empty slots are exactly 24 zero bytes with no trailing name.
    if r.remaining() >= EMPTY_ITEM_BYTES {
        let peek = r.peek(EMPTY_ITEM_BYTES)?;
        if peek.iter().all(|&b| b == 0) {
            let _ = r.bytes(EMPTY_ITEM_BYTES)?;
            return Ok(None);
        }
    }

    let unique_id = r.u16()?;
    let level = r.u8()?;
    let value1 = r.u8()?;
    let value2 = r.u8()?;
    let hand_byte = r.u8()?;
    let object_type_byte = r.u8()?;
    let unk_1112 = r.u8()?;
    let weight = r.u16()?;
    let condition_pct = r.u8()?;
    let durability_pct = r.u8()?;
    let quality = r.u8()?;
    let bonus = r.u8()?;
    let bonus_level = r.u8()?;
    let model = r.u16()?;
    let extension = r.u8()?;
    let color_or_emblem = r.u16()?;
    let flag = r.u8()?;
    // Charge-spell blobs — skip identity, do not invent display from them yet.
    if flag & 0x08 != 0 {
        let _icon = r.u16()?;
        let _spell_name = r.pascal_string()?;
    }
    if flag & 0x10 != 0 {
        let _icon = r.u16()?;
        let _spell_name = r.pascal_string()?;
    }
    let effect = r.u16()?;
    let name = r.pascal_string()?;

    Ok(Some(ItemData {
        unique_id,
        level,
        value1,
        value2,
        hand_byte,
        object_type_byte,
        unk_1112,
        weight,
        condition_pct,
        durability_pct,
        quality,
        bonus,
        bonus_level,
        model,
        extension,
        color_or_emblem,
        flag,
        effect,
        name,
    }))
}

/// Encode a PacketLib189 header + PacketLib1124 item bodies (for golden / SCN-06 fixtures).
///
/// Spell charge blobs are omitted (`flag` bits 0x08/0x10 cleared) so round-trips stay fixed-size
/// unless the caller already stored names separately — captures with charges still decode.
#[must_use]
pub fn encode_1124(upd: &InventoryUpdate) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(6 + upd.items.len() * 40);
    w.u8(upd.items.len() as u8);
    w.u8(upd.unused_speed);
    let mut vis = 0u8;
    if upd.cloak_invisible {
        vis |= 0x01;
    }
    if upd.helm_invisible {
        vis |= 0x02;
    }
    w.u8(vis);
    w.u8((upd.hood_up as u8) | ((upd.active_quiver & 0x0F) << 4));
    w.u8(upd.active_weapon_slots);
    w.u8(upd.window_type);
    for entry in &upd.items {
        w.u8(entry.slot);
        match &entry.item {
            None => {
                for _ in 0..EMPTY_ITEM_BYTES {
                    w.u8(0);
                }
            }
            Some(it) => {
                // Round-trip without optional spell blobs.
                let flag = it.flag & !(0x08 | 0x10);
                w.u16(it.unique_id)
                    .u8(it.level)
                    .u8(it.value1)
                    .u8(it.value2)
                    .u8(it.hand_byte)
                    .u8(it.object_type_byte)
                    .u8(it.unk_1112)
                    .u16(it.weight)
                    .u8(it.condition_pct)
                    .u8(it.durability_pct)
                    .u8(it.quality)
                    .u8(it.bonus)
                    .u8(it.bonus_level)
                    .u16(it.model)
                    .u8(it.extension)
                    .u16(it.color_or_emblem)
                    .u8(flag)
                    .u16(it.effect)
                    .pascal_string(&it.name);
            }
        }
    }
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equipment::slot;

    #[test]
    fn practice_sword_from_login_capture() {
        // First item from rustdaoc_login_20260714.bin InventoryUpdate (slot 10).
        let payload: &[u8] = &[
            0x01, 0x00, 0x00, 0x00, 0x10, 0x01, // header, 1 item
            0x0a, // righthand
            0x00, 0x00, // unique
            0x00, // level
            0xa5, 0x19, // value1/2
            0x00, // hand
            0x83, // object type packed
            0x00, // unk
            0x00, 0x0a, // weight 10
            0x64, 0x64, 0x5a, 0x00, // con/dur/qua/bonus
            0x00, // bonus level
            0x00, 0x03, // model 3
            0x00, // ext
            0x00, 0x00, // color
            0x02, // flag salvage
            0x00, 0x00, // effect
            0x0e, b'p', b'r', b'a', b'c', b't', b'i', b'c', b'e', b' ', b's', b'w', b'o', b'r',
            b'd',
        ];
        let u = decode(payload).expect("practice sword packet");
        assert_eq!(u.provenance(), PROVENANCE);
        assert_eq!(u.active_weapon_slots, 0x10);
        let sword = u.item(slot::RIGHTHAND).expect("slot 10");
        assert_eq!(sword.model, 3);
        assert_eq!(sword.name, "practice sword");
    }

    #[test]
    fn empty_slot_is_twenty_four_zeros() {
        let mut body = vec![0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28];
        body.extend(std::iter::repeat_n(0u8, 24));
        let u = decode(&body).unwrap();
        assert!(u.items[0].item.is_none());
        assert_eq!(u.items[0].slot, 0x28);
    }

    #[test]
    fn encode_decode_roundtrip_without_spells() {
        let upd = InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0x10,
            window_type: 1,
            items: vec![InventoryItem {
                slot: slot::TORSO,
                item: Some(ItemData {
                    unique_id: 0,
                    level: 2,
                    value1: 0,
                    value2: 0,
                    hand_byte: 0,
                    object_type_byte: 0x21,
                    unk_1112: 0,
                    weight: 40,
                    condition_pct: 100,
                    durability_pct: 100,
                    quality: 90,
                    bonus: 0,
                    bonus_level: 0,
                    model: 81,
                    extension: 3,
                    color_or_emblem: 0,
                    flag: 0x02,
                    effect: 0,
                    name: "bronze studded vest".into(),
                }),
            }],
        };
        let enc = encode_1124(&upd);
        let dec = decode(&enc).unwrap();
        assert_eq!(dec.item(slot::TORSO).unwrap().model, 81);
        assert_eq!(dec.item(slot::TORSO).unwrap().extension, 3);
        assert_eq!(dec.item(slot::TORSO).unwrap().name, "bronze studded vest");
    }
}
