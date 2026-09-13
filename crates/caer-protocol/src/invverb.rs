//! Client → server inventory verbs (System 4).
//!
//! ## PlayerMoveItem (0xDD) — equip / unequip / bag shuffle
//! OPEN_ORACLE: SoloDAoC `PlayerMoveItemRequestHandler` reads
//! `u16 id, u16 to_slot, u16 from_slot, u16 count` (big-endian shorts via `ReadShort`).
//! Equipping is moving from a backpack slot (40..79) onto a worn slot (e.g. torso 25).
//!
//! ## BuyRequest (0x78) — merchant purchase
//! OPEN_ORACLE: `PlayerBuyRequestHandler` reads
//! `u32 X, u32 Y, u16 id, u16 item_slot, u8 item_count, u8 menu_id`.
//! Normal shop buy uses `menu_id = 0` (default merchant window) and requires a targeted merchant.

use crate::codec::PacketWriter;

/// First backpack slot (`eInventorySlot.FirstBackpack`).
pub const FIRST_BACKPACK: u16 = 40;
/// Last backpack slot (`eInventorySlot.LastBackpack`).
pub const LAST_BACKPACK: u16 = 79;

/// Encode PlayerMoveItem (0xDD). `id` is unused by the oracle for player↔self moves (0).
#[must_use]
pub fn encode_move_item(to_slot: u16, from_slot: u16, count: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(0) // id — player inventory move
        .u16(to_slot)
        .u16(from_slot)
        .u16(count.max(1));
    w.into_bytes()
}

/// Encode BuyRequest (0x78) for a normal NPC merchant window (`menu_id = 0`).
///
/// `player_xy` is the client's feet (oracle reads X/Y but the merchant path mainly uses
/// `TargetObject`). `merchant_id` is the targeted object's id as the client last saw it —
/// the handler re-reads `TargetObject`, so the short is informational for most DOL builds.
#[must_use]
pub fn encode_buy_request(
    player_x: u32,
    player_y: u32,
    merchant_id: u16,
    item_slot: u16,
    item_count: u8,
) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32(player_x)
        .u32(player_y)
        .u16(merchant_id)
        .u16(item_slot)
        .u8(item_count.max(1))
        .u8(0); // eMerchantWindowType normal
    w.into_bytes()
}

/// Encode ObjectInteractRequest (0x7A) — opens merchant / dialog / use object.
/// OPEN_ORACLE `ObjectInteractRequestHandler`: `u32 X, u32 Y, u16 sessionId, u16 targetOid`.
#[must_use]
pub fn encode_object_interact(
    player_x: u32,
    player_y: u32,
    session_id: u16,
    target_oid: u16,
) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32(player_x)
        .u32(player_y)
        .u16(session_id)
        .u16(target_oid);
    w.into_bytes()
}

/// Encode PickUpRequest (0xB5). OPEN_ORACLE `PlayerPickUpRequestHandler` reads
/// `u32 X, u32 Y, u16 id, u16 obj` then picks up **`TargetObject`** (wire oid is unused).
/// Same layout as ObjectInteract; call [`SessionState::target`] first.
#[must_use]
pub fn encode_pickup_request(
    player_x: u32,
    player_y: u32,
    session_id: u16,
    object_id: u16,
) -> Vec<u8> {
    encode_object_interact(player_x, player_y, session_id, object_id)
}

/// Encode DialogResponse (0x82). OPEN_ORACLE `DialogResponseHandler` reads
/// `u16 data1, u16 data2, u16 data3, u8 messageType(eDialogCode), u8 response`.
/// CustomDialog (`message_type = 0x06`): echo server `data1`/`data2`, `response = 0x01` = Yes.
#[must_use]
pub fn encode_dialog_response(
    data1: u16,
    data2: u16,
    data3: u16,
    message_type: u8,
    response: u8,
) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(data1)
        .u16(data2)
        .u16(data3)
        .u8(message_type)
        .u8(response);
    w.into_bytes()
}

/// Encode SellRequest (0x79). OPEN_ORACLE `PlayerSellRequestHandler`:
/// `u32 X, u32 Y, u16 id, u16 item_slot`. Merchant is `TargetObject`, not the wire id.
#[must_use]
pub fn encode_sell_request(
    player_x: u32,
    player_y: u32,
    merchant_id: u16,
    item_slot: u16,
) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32(player_x)
        .u32(player_y)
        .u16(merchant_id)
        .u16(item_slot);
    w.into_bytes()
}

/// Encode UseSlot (0x71) for client version ≥ 1.124 (1.127 route).
/// OPEN_ORACLE `UseSlotHandler`: four LE floats (x,y,z,speed), heading u16, then
/// `u16 flagSpeedData, u8 slot, u8 type`. Pre-1124 omits the float/heading prefix — do not send that.
#[must_use]
pub fn encode_use_slot_1124(
    x: f32,
    y: f32,
    z: f32,
    speed: f32,
    heading: u16,
    flag_speed_data: u16,
    slot: u8,
    use_type: u8,
) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.f32_le(x)
        .f32_le(y)
        .f32_le(z)
        .f32_le(speed)
        .u16(heading)
        .u16(flag_speed_data)
        .u8(slot)
        .u8(use_type);
    w.into_bytes()
}

/// Encode CraftRequest / MakeProduct (0xED). OPEN_ORACLE `MakeProductHandler`: `u16 ItemID`.
/// Local encode is intent only — inventory/money change is InventoryUpdate / MoneyUpdate.
#[must_use]
pub fn encode_craft_request(item_id: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(item_id);
    w.into_bytes()
}

/// Encode DestroyItemRequest (0x80). OPEN_ORACLE `DestroyItemRequestHandler`: Skip(4) then
/// `ReadShort` slot. The four skipped bytes are unread padding — encode as zeros, do not invent
/// a session-id meaning. Does **not** remove the item locally.
#[must_use]
pub fn encode_destroy_item(slot: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32(0).u16(slot);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::PacketReader;

    #[test]
    fn move_item_wire_matches_oracle_layout() {
        let body = encode_move_item(25, 40, 1); // torso ← backpack0
        assert_eq!(body.len(), 8);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u16().unwrap(), 0);
        assert_eq!(r.u16().unwrap(), 25);
        assert_eq!(r.u16().unwrap(), 40);
        assert_eq!(r.u16().unwrap(), 1);
    }

    #[test]
    fn buy_request_wire_matches_oracle_layout() {
        let body = encode_buy_request(100, 200, 7, 3, 1);
        assert_eq!(body.len(), 14);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u32().unwrap(), 100);
        assert_eq!(r.u32().unwrap(), 200);
        assert_eq!(r.u16().unwrap(), 7);
        assert_eq!(r.u16().unwrap(), 3);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u8().unwrap(), 0);
    }

    #[test]
    fn object_interact_wire_matches_oracle_layout() {
        let body = encode_object_interact(1, 2, 3, 4);
        assert_eq!(body.len(), 12);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u32().unwrap(), 1);
        assert_eq!(r.u32().unwrap(), 2);
        assert_eq!(r.u16().unwrap(), 3);
        assert_eq!(r.u16().unwrap(), 4);
    }

    #[test]
    fn sell_request_wire_matches_oracle_layout() {
        let body = encode_sell_request(10, 20, 7, 40);
        assert_eq!(body.len(), 12);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u32().unwrap(), 10);
        assert_eq!(r.u32().unwrap(), 20);
        assert_eq!(r.u16().unwrap(), 7);
        assert_eq!(r.u16().unwrap(), 40);
    }

    #[test]
    fn destroy_item_wire_is_four_pad_bytes_then_slot() {
        let body = encode_destroy_item(40);
        assert_eq!(body.len(), 6);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u32().unwrap(), 0);
        assert_eq!(r.u16().unwrap(), 40);
    }

    #[test]
    fn use_slot_1124_wire_matches_oracle_layout() {
        let body = encode_use_slot_1124(1.0, 2.0, 3.0, 0.0, 0x100, 0, 40, 0);
        assert_eq!(body.len(), 16 + 2 + 2 + 1 + 1);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.f32_le().unwrap(), 1.0);
        assert_eq!(r.f32_le().unwrap(), 2.0);
        assert_eq!(r.f32_le().unwrap(), 3.0);
        assert_eq!(r.f32_le().unwrap(), 0.0);
        assert_eq!(r.u16().unwrap(), 0x100);
        assert_eq!(r.u16().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 40);
        assert_eq!(r.u8().unwrap(), 0);
    }

    #[test]
    fn craft_request_wire_matches_make_product_handler() {
        let body = encode_craft_request(0x1234);
        assert_eq!(body, [0x12, 0x34]);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u16().unwrap(), 0x1234);
    }
}
