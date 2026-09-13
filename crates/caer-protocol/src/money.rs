//! `MoneyUpdate` (0xFA) — the player's purse.
//!
//! ## Provenance
//!
//! REQ-006 / REQ-021: displayed copper/silver/gold/mithril/platinum must come from this packet
//! (or an honest absence), never invented defaults. Adapter names on the menu bar are the Mythic
//! `merchant_*` set — same labels the stock skin uses for the player's purse.
//!
//! ## Wire form (`PacketLib168.SendUpdateMoney`, unchanged through 1.127)
//!
//! ```text
//!   u8      copper
//!   u8      silver
//!   u16 BE  gold
//!   u16 BE  mithril
//!   u16 BE  platinum
//! ```

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for UI / scenario asserts (REQ-021 / SCN-06).
pub const PROVENANCE: &str = "MoneyUpdate 0xFA";

/// A decoded MoneyUpdate (0xFA).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MoneyUpdate {
    pub copper: u8,
    pub silver: u8,
    pub gold: u16,
    pub mithril: u16,
    pub platinum: u16,
}

impl MoneyUpdate {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }
}

/// Decode a MoneyUpdate payload.
pub fn decode(payload: &[u8]) -> Result<MoneyUpdate> {
    let mut r = PacketReader::new(payload);
    Ok(MoneyUpdate {
        copper: r.u8()?,
        silver: r.u8()?,
        gold: r.u16()?,
        mithril: r.u16()?,
        platinum: r.u16()?,
    })
}

/// Provenance for ConsignmentMerchantMoney (0x1E) — house market purse, not player 0xFA.
pub const CONSIGNMENT_PROVENANCE: &str = "ConsignmentMerchantMoney 0x1E";

/// Decoded ConsignmentMerchantMoney (0x1E). Layout matches MoneyUpdate (PacketLib168).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConsignmentMerchantMoney {
    pub copper: u8,
    pub silver: u8,
    pub gold: u16,
    pub mithril: u16,
    pub platinum: u16,
}

impl ConsignmentMerchantMoney {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        CONSIGNMENT_PROVENANCE
    }
}

/// Decode ConsignmentMerchantMoney 0x1E. OPEN_ORACLE `PacketLib168.SendConsignmentMerchantMoney`.
pub fn decode_consignment(payload: &[u8]) -> Result<ConsignmentMerchantMoney> {
    let m = decode(payload)?;
    Ok(ConsignmentMerchantMoney {
        copper: m.copper,
        silver: m.silver,
        gold: m.gold,
        mithril: m.mithril,
        platinum: m.platinum,
    })
}

/// Encode matching `PacketLib168.SendConsignmentMerchantMoney`.
#[must_use]
pub fn encode_consignment(m: &ConsignmentMerchantMoney) -> Vec<u8> {
    encode(&MoneyUpdate {
        copper: m.copper,
        silver: m.silver,
        gold: m.gold,
        mithril: m.mithril,
        platinum: m.platinum,
    })
}

/// Encode matching `PacketLib168.SendUpdateMoney` (golden / SCN-06 fixtures).
#[must_use]
pub fn encode(m: &MoneyUpdate) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(8);
    w.u8(m.copper)
        .u8(m.silver)
        .u16(m.gold)
        .u16(m.mithril)
        .u16(m.platinum);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_match_oracle_sender() {
        let m = MoneyUpdate {
            copper: 20,
            silver: 27,
            gold: 612,
            mithril: 0,
            platinum: 4,
        };
        let body = encode(&m);
        assert_eq!(body, [0x14, 0x1b, 0x02, 0x64, 0x00, 0x00, 0x00, 0x04]);
        let d = decode(&body).expect("oracle-shaped 0xFA must decode");
        assert_eq!(d, m);
        assert_eq!(d.provenance(), PROVENANCE);
    }

    /// Captured sample from login golden (`0000001400000000` = 20 gold).
    #[test]
    fn decodes_captured_twenty_gold() {
        let d = decode(&[0x00, 0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00]).unwrap();
        assert_eq!(d.copper, 0);
        assert_eq!(d.silver, 0);
        assert_eq!(d.gold, 20);
        assert_eq!(d.mithril, 0);
        assert_eq!(d.platinum, 0);
    }

    #[test]
    fn consignment_money_matches_packetlib168_sender() {
        // OPEN_ORACLE PacketLib168.SendConsignmentMerchantMoney: copper, silver, gold, mithril, platinum.
        let m = ConsignmentMerchantMoney {
            copper: 5,
            silver: 6,
            gold: 7,
            mithril: 8,
            platinum: 9,
        };
        let body = encode_consignment(&m);
        assert_eq!(body, [5, 6, 0x00, 7, 0x00, 8, 0x00, 9]);
        let d = decode_consignment(&body).expect("0x1E");
        assert_eq!(d, m);
        assert_eq!(d.provenance(), CONSIGNMENT_PROVENANCE);
        assert_ne!(d.provenance(), PROVENANCE);
    }
}
