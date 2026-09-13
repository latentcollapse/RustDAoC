//! Group and player-to-player trade wire (System 6 Streams G + T).
//!
//! ## Provenance
//!
//! OPEN_ORACLE SoloDAoC:
//! - C2S `InviteToGroup` 0x87 — empty body; handler uses `TargetObject`
//!   (`InviteToGroupHandler`).
//! - S2C `GroupMemberUpdate` 0x70 — `PacketLib1125.WriteGroupMemberUpdate` (1.127 inherit via
//!   1126→1125): index byte `0x20|GroupIndex`, hp/mana/endu/status, optional map (`0x40|idx`),
//!   optional icons (`0x80|idx`), roster terminated by `0x00`.
//! - S2C group window clear — `VariousUpdate` 0x16 subcode `0x06` via `SendGroupWindowUpdate`
//!   (empty group → `[0x06][0x00]`).
//! - C2S `ModifyTrade` 0xEB — `PlayerModifyTradeHandler`:
//!   `[isok][repair][combine][unk][10 slots][u16][5× u16 money mithril..copper]`.
//! - S2C `TradeWindow` 0xEA — `PacketLib172.SendTradeWindow` / `SendCloseTradeWindow` (40 zero
//!   bytes closes).
//!
//! Trade open is **not** a dedicated C2S code: `PlayerMoveItem` 0xDD with
//! `to_slot = object_id + 1000` onto another player (`PlayerMoveItemRequestHandler`).

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tags for UI / scenario asserts.
pub const GROUP_MEMBER_UPDATE_PROVENANCE: &str = "GroupMemberUpdate 0x70";
pub const GROUP_WINDOW_PROVENANCE: &str = "VariousUpdate 0x16 subcode 0x06";
pub const TRADE_WINDOW_PROVENANCE: &str = "TradeWindow 0xEA";
pub const MODIFY_TRADE_PROVENANCE: &str = "ModifyTrade 0xEB";

/// `ModifyTrade.isok` values (`PlayerModifyTradeHandler`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ModifyTradeAction {
    Cancel = 0,
    Update = 1,
    Accept = 2,
}

impl ModifyTradeAction {
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Cancel),
            1 => Some(Self::Update),
            2 => Some(Self::Accept),
            _ => None,
        }
    }
}

/// One living on a GroupMemberUpdate roster (vitals only — no names on 0x70).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupMemberVitals {
    /// 0-based group index (low 3 bits of the wire index byte).
    pub index: u8,
    pub health_pct: u8,
    pub mana_pct: u8,
    pub endurance_pct: u8,
    pub status: u8,
}

/// Decoded GroupMemberUpdate (0x70) roster.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GroupMemberUpdate {
    pub members: Vec<GroupMemberVitals>,
}

impl GroupMemberUpdate {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        GROUP_MEMBER_UPDATE_PROVENANCE
    }

    #[must_use]
    pub fn member_count(&self) -> usize {
        self.members.len()
    }
}

/// One named entry from GroupWindow (`VariousUpdate` subcode 0x06, PacketLib1125).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupWindowMember {
    pub name: String,
    pub salutation: String,
    pub object_id: u16,
    pub level: u8,
}

/// Group window roster / clear (`0x16` / `0x06`). Empty `members` = left / disbanded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GroupWindow {
    pub members: Vec<GroupWindowMember>,
}

impl GroupWindow {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        GROUP_WINDOW_PROVENANCE
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

/// Purse offered on one side of a trade (mithril → copper order on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TradeMoney {
    pub mithril: u16,
    pub platinum: u16,
    pub gold: u16,
    pub silver: u16,
    pub copper: u16,
}

/// Decoded TradeWindow (0xEA). `closed` when the server sent the 40-byte zero close form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeWindow {
    pub closed: bool,
    /// Own backpack slots offered (up to 10; unused = 0).
    pub own_slots: [u8; 10],
    pub own_money: TradeMoney,
    pub partner_money: TradeMoney,
    pub partner_item_count: u8,
    pub repairing: bool,
    pub combining: bool,
    /// Partner / craft caption when present (open window).
    pub caption: String,
}

impl TradeWindow {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        TRADE_WINDOW_PROVENANCE
    }
}

/// Encode InviteToGroup (0x87). Body is empty / ignored; server uses `TargetObject`.
#[must_use]
pub fn encode_invite_to_group() -> Vec<u8> {
    Vec::new()
}

/// Encode ModifyTrade (0xEB).
///
/// `slots` is the 10 offered backpack slot positions (pad with 0). Money is only applied when
/// `action == Update`; Cancel/Accept ignore the trailing fields server-side but we still send a
/// well-formed body so the reader does not short-read on Update paths.
#[must_use]
pub fn encode_modify_trade(
    action: ModifyTradeAction,
    repair: bool,
    combine: bool,
    slots: &[u8; 10],
    money: TradeMoney,
) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u8(action as u8)
        .u8(u8::from(repair))
        .u8(u8::from(combine))
        .u8(0);
    for &s in slots {
        w.u8(s);
    }
    w.u16(0)
        .u16(money.mithril)
        .u16(money.platinum)
        .u16(money.gold)
        .u16(money.silver)
        .u16(money.copper);
    w.into_bytes()
}

/// Decode GroupMemberUpdate (0x70) under PacketLib1125 layout (1.127).
///
/// Tolerates optional per-member map (`0x40|idx` + 6 bytes) and icon blocks (`0x80|idx` +
/// `count × (u8 + u16)`). Stops at the `0x00` terminator.
pub fn decode_group_member_update(payload: &[u8]) -> Result<GroupMemberUpdate> {
    let mut r = PacketReader::new(payload);
    let mut members = Vec::new();
    loop {
        if r.remaining() == 0 {
            break;
        }
        let index_byte = r.u8()?;
        if index_byte == 0 {
            break;
        }
        // 1125: 0x20 | GroupIndex. Older libs wrote GroupIndex+1 (1..8). Either way low 3 bits
        // identify the member slot for RT counting.
        let index = index_byte & 0x07;
        let health_pct = r.u8()?;
        let mana_pct = r.u8()?;
        let endurance_pct = r.u8()?;
        let status = r.u8()?;
        members.push(GroupMemberVitals {
            index,
            health_pct,
            mana_pct,
            endurance_pct,
            status,
        });
        // Optional map / icon trailers before the next member or terminator.
        while r.remaining() > 0 {
            let peek = payload[r.position()];
            if peek == 0 {
                break;
            }
            if peek & 0x40 != 0 && peek & 0x80 == 0 {
                let _ = r.u8()?; // 0x40 | idx
                let _ = r.u16()?; // zone
                let _ = r.u16()?; // x
                let _ = r.u16()?; // y
                continue;
            }
            if peek & 0x80 != 0 {
                let _ = r.u8()?; // 0x80 | idx
                let n = r.u8()? as usize;
                for _ in 0..n {
                    let _ = r.u8()?; // pad (1125)
                    let _ = r.u16()?; // icon
                }
                continue;
            }
            // Next member index byte (0x20|idx or 1..8).
            break;
        }
    }
    Ok(GroupMemberUpdate { members })
}

/// Decode `VariousUpdate` body when subcode is group window (`0x06`). Returns `Ok(None)` for
/// other subcodes so the session multiplexer can fall through.
pub fn decode_group_window(payload: &[u8]) -> Result<Option<GroupWindow>> {
    if payload.is_empty() {
        return Ok(None);
    }
    let mut r = PacketReader::new(payload);
    let sub = r.u8()?;
    if sub != 0x06 {
        return Ok(None);
    }
    if r.remaining() == 0 {
        return Ok(Some(GroupWindow::default()));
    }
    let count = r.u8()? as usize;
    let mut members = Vec::with_capacity(count);
    for _ in 0..count {
        let name = r.pascal_string()?;
        let salutation = r.pascal_string().unwrap_or_default();
        let object_id = r.u16().unwrap_or(0);
        let level = r.u8().unwrap_or(0);
        members.push(GroupWindowMember {
            name,
            salutation,
            object_id,
            level,
        });
    }
    Ok(Some(GroupWindow { members }))
}

fn read_trade_money(r: &mut PacketReader<'_>) -> Result<TradeMoney> {
    Ok(TradeMoney {
        mithril: r.u16()?,
        platinum: r.u16()?,
        gold: r.u16()?,
        silver: r.u16()?,
        copper: r.u16()?,
    })
}

/// Decode TradeWindow (0xEA). A payload of (at least) 40 zero bytes is the close form.
pub fn decode_trade_window(payload: &[u8]) -> Result<TradeWindow> {
    let closed = payload.len() >= 40 && payload.iter().take(40).all(|&b| b == 0);
    if closed {
        return Ok(TradeWindow {
            closed: true,
            own_slots: [0; 10],
            own_money: TradeMoney::default(),
            partner_money: TradeMoney::default(),
            partner_item_count: 0,
            repairing: false,
            combining: false,
            caption: String::new(),
        });
    }
    let mut r = PacketReader::new(payload);
    let mut own_slots = [0u8; 10];
    for s in &mut own_slots {
        *s = r.u8()?;
    }
    let _ = r.u16()?;
    let own_money = read_trade_money(&mut r)?;
    let _ = r.u16()?;
    let partner_money = read_trade_money(&mut r)?;
    let _ = r.u16()?;
    let partner_item_count = r.u8()?;
    let _flag = r.u8()?; // 0x01 when partner items present, else part of zero short
    let repairing = r.u8()? != 0;
    let combining = r.u8()? != 0;
    // Skip partner item blobs when present — RT only needs open/close + counts.
    if partner_item_count > 0 && _flag == 0x01 {
        for _ in 0..partner_item_count {
            let _slot = r.u8()?;
            let _level = r.u8()?;
            let _dps = r.u8()?;
            let _spd = r.u8()?;
            let _hand = r.u8()?;
            let _ot = r.u8()?;
            let _weight = r.u16()?;
            let _con = r.u8()?;
            let _dur = r.u8()?;
            let _qua = r.u8()?;
            let _bon = r.u8()?;
            let _model = r.u16()?;
            let _color = r.u16()?;
            let _effect = r.u16()?;
            let _ = r.pascal_string()?;
        }
    }
    let caption = r.pascal_string().unwrap_or_default();
    Ok(TradeWindow {
        closed: false,
        own_slots,
        own_money,
        partner_money,
        partner_item_count,
        repairing,
        combining,
        caption,
    })
}

/// Provenance for ObjectGuildID (0xDE).
pub const OBJECT_GUILD_ID_PROVENANCE: &str = "ObjectGuildID 0xDE";

/// Decoded ObjectGuildID (0xDE). `guild_id == 0` is the null-guild form (`WriteInt(0)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectGuildId {
    pub object_id: u16,
    pub guild_id: u16,
}

impl ObjectGuildId {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        OBJECT_GUILD_ID_PROVENANCE
    }

    #[must_use]
    pub fn has_guild(&self) -> bool {
        self.guild_id != 0
    }
}

/// Decode ObjectGuildID 0xDE. OPEN_ORACLE `PacketLib168.SendObjectGuildID`:
/// `u16 object_id`, then either `u32 0` (no guild) or `u16 guild.ID` twice, then unused `u16`.
pub fn decode_object_guild_id(payload: &[u8]) -> Result<ObjectGuildId> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let a = r.u16()?;
    let b = r.u16()?;
    let _unused = r.u16()?;
    let guild_id = if a == 0 && b == 0 { 0 } else { a };
    Ok(ObjectGuildId {
        object_id,
        guild_id,
    })
}

/// Encode matching `PacketLib168.SendObjectGuildID` (tests / fixtures).
#[must_use]
pub fn encode_object_guild_id(oid: u16, guild_id: u16) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(8);
    w.u16(oid);
    if guild_id == 0 {
        w.u32(0);
    } else {
        w.u16(guild_id).u16(guild_id);
    }
    w.u16(0);
    w.into_bytes()
}

/// `to_slot` for opening a player trade via PlayerMoveItem (`object_id + 1000`).
#[must_use]
pub fn trade_give_slot(partner_object_id: u16) -> u16 {
    partner_object_id.saturating_add(1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_body_is_empty() {
        assert!(encode_invite_to_group().is_empty());
    }

    #[test]
    fn modify_trade_wire_matches_oracle_reader() {
        let slots = [40, 41, 0, 0, 0, 0, 0, 0, 0, 0];
        let money = TradeMoney {
            mithril: 0,
            platinum: 0,
            gold: 1,
            silver: 2,
            copper: 3,
        };
        let body = encode_modify_trade(ModifyTradeAction::Update, false, false, &slots, money);
        assert_eq!(body.len(), 4 + 10 + 2 + 10);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u8().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 40);
        assert_eq!(r.u8().unwrap(), 41);
        for _ in 0..8 {
            assert_eq!(r.u8().unwrap(), 0);
        }
        assert_eq!(r.u16().unwrap(), 0);
        assert_eq!(r.u16().unwrap(), 0);
        assert_eq!(r.u16().unwrap(), 0);
        assert_eq!(r.u16().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), 2);
        assert_eq!(r.u16().unwrap(), 3);
        assert_eq!(
            ModifyTradeAction::from_u8(0),
            Some(ModifyTradeAction::Cancel)
        );
        assert_eq!(
            ModifyTradeAction::from_u8(2),
            Some(ModifyTradeAction::Accept)
        );
    }

    #[test]
    fn group_member_update_1125_two_members_round_trip_shape() {
        // Two members, no map/icons, terminator — synthesised OPEN_ORACLE shape.
        let body = vec![
            0x20, 100, 50, 80, 0x00, // idx0
            0x21, 90, 40, 70, 0x00, // idx1
            0x00,
        ];
        let g = decode_group_member_update(&body).expect("decode");
        assert_eq!(g.member_count(), 2);
        assert_eq!(g.members[0].index, 0);
        assert_eq!(g.members[0].health_pct, 100);
        assert_eq!(g.members[1].index, 1);
        assert_eq!(g.provenance(), GROUP_MEMBER_UPDATE_PROVENANCE);
    }

    #[test]
    fn group_member_update_skips_map_and_icons() {
        let mut body = vec![0x20, 100, 50, 80, 0x00];
        body.extend_from_slice(&[0x40, 0x00, 0x01, 0x10, 0x00, 0x20, 0x00]); // map
        body.extend_from_slice(&[0x80, 0x01, 0x00, 0x12, 0x34]); // one icon
        body.push(0x00);
        let g = decode_group_member_update(&body).expect("decode");
        assert_eq!(g.member_count(), 1);
    }

    #[test]
    fn group_window_empty_is_leave_clear() {
        let g = decode_group_window(&[0x06, 0x00])
            .unwrap()
            .expect("subcode 0x06");
        assert!(g.is_empty());
        assert!(decode_group_window(&[0x01, 0x00]).unwrap().is_none());
    }

    #[test]
    fn group_window_named_members() {
        let mut w = PacketWriter::new();
        w.u8(0x06).u8(2);
        w.pascal_string("Alice");
        w.pascal_string("Fighter");
        w.u16(1001);
        w.u8(20);
        w.pascal_string("Bob");
        w.pascal_string("Mage");
        w.u16(1002);
        w.u8(18);
        let g = decode_group_window(&w.into_bytes())
            .unwrap()
            .expect("window");
        assert_eq!(g.members.len(), 2);
        assert_eq!(g.members[0].name, "Alice");
        assert_eq!(g.members[1].object_id, 1002);
    }

    #[test]
    fn trade_window_close_is_forty_zeros() {
        let tw = decode_trade_window(&[0u8; 40]).expect("close");
        assert!(tw.closed);
        assert_eq!(tw.provenance(), TRADE_WINDOW_PROVENANCE);
    }

    #[test]
    fn trade_window_open_header() {
        let mut w = PacketWriter::new();
        for i in 0..10u8 {
            w.u8(if i == 0 { 40 } else { 0 });
        }
        w.u16(0);
        for v in [0u16, 0, 5, 0, 0] {
            w.u16(v); // own money: 5 gold
        }
        w.u16(0);
        for v in [0u16, 0, 0, 0, 0] {
            w.u16(v);
        }
        w.u16(0); // pad before partner-item block
        w.u16(0); // partner item count short = 0 (null PartnerTradeItems)
        w.u8(0).u8(0); // repair/combine
        w.pascal_string("Trading with Bob");
        let tw = decode_trade_window(&w.into_bytes()).expect("open");
        assert!(!tw.closed);
        assert_eq!(tw.own_slots[0], 40);
        assert_eq!(tw.own_money.gold, 5);
        assert!(
            tw.caption.contains("Bob"),
            "caption={:?} own_gold={} closed={}",
            tw.caption,
            tw.own_money.gold,
            tw.closed
        );
    }

    #[test]
    fn trade_give_slot_is_oid_plus_1000() {
        assert_eq!(trade_give_slot(42), 1042);
    }

    #[test]
    fn object_guild_id_matches_packetlib168() {
        let with = encode_object_guild_id(0x0102, 0x00AB);
        assert_eq!(with, [0x01, 0x02, 0x00, 0xAB, 0x00, 0xAB, 0x00, 0x00]);
        let d = decode_object_guild_id(&with).expect("guild");
        assert_eq!(d.object_id, 0x0102);
        assert_eq!(d.guild_id, 0x00AB);
        assert!(d.has_guild());
        assert_eq!(d.provenance(), OBJECT_GUILD_ID_PROVENANCE);

        let none = encode_object_guild_id(9, 0);
        assert_eq!(none, [0x00, 0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let d = decode_object_guild_id(&none).expect("no guild");
        assert!(!d.has_guild());
        assert_eq!(d.guild_id, 0);
    }
}
