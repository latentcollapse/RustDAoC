//! `MerchantWindow` (0x17) — NPC merchant offer list (one page per packet).
//!
//! ## Provenance
//!
//! REQ-021 / SCN-08: decoded against SoloDAoC `PacketLib1125.SendMerchantWindow` (1.127 inherits
//! via PacketLib1127 → … → 1125). OWN_CAPTURE: `captures/cap_20260714_230416_conn41042.bin`
//! carries 12× 0x17; the 7-item mythirian page and ticket pages decode cleanly under this layout.
//!
//! ## Wire form (PacketLib1125 — 1.127)
//!
//! ```text
//!   u8   entry count
//!   u8   window type (eMerchantWindowType)
//!   u8   page number
//!   // NO unused pad byte (PacketLib168 had one; 1125 dropped it)
//!   per entry:
//!     u8      slot position on page
//!     u8      level
//!     u8      value1 (DPS / pack size / … by object type)
//!     u8      SPD_ABS
//!     u8      hand<<6  (or garden DPS)
//!     u8      (type_damage<<6) | object_type
//!     u8      usable flag — 0x01 usable by class, 0x00 greyed (INVERTED vs PacketLib168)
//!     u16 LE  value2 (weight / pack×weight / …)
//!     u32 LE  currency amount (price)
//!     u16 LE  model
//!     PascalStringIntLE name (maxlen 0x30): [len+1:4 LE][bytes][NUL]
//! ```
//!
//! Honest finding: the packet does **not** carry merchant NPC object id, item template id, or
//! inventory-slot mapping for the buy — those live on the interact / buy client packets.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for UI / scenario asserts (REQ-021 / SCN-08).
pub const PROVENANCE: &str = "MerchantWindow 0x17";

/// `eMerchantWindowType` values the oracle writes (subset; housing codes exist but are unused here).
pub mod window_type {
    pub const NORMAL: u8 = 0x00;
    pub const BP: u8 = 0x01;
    pub const COUNT: u8 = 0x02;
    pub const MITHRIL: u8 = 0x0E;
}

/// One offer on a merchant page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerchantOffer {
    pub slot: u8,
    pub level: u8,
    pub value1: u8,
    pub spd_abs: u8,
    pub hand_byte: u8,
    pub object_type_byte: u8,
    /// `true` when the wire flag is 0x01 (PacketLib1125: usable by class).
    pub usable: bool,
    pub value2: u16,
    pub price: u32,
    pub model: u16,
    pub name: String,
}

/// One MerchantWindow page (one S2C packet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerchantWindow {
    pub window_type: u8,
    pub page: u8,
    pub items: Vec<MerchantOffer>,
}

impl MerchantWindow {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }

    /// Offer at page slot position, if present.
    #[must_use]
    pub fn item(&self, slot: u8) -> Option<&MerchantOffer> {
        self.items.iter().find(|i| i.slot == slot)
    }
}

/// Decode a 1.127 MerchantWindow payload (PacketLib1125).
pub fn decode(payload: &[u8]) -> Result<MerchantWindow> {
    let mut r = PacketReader::new(payload);
    let count = r.u8()? as usize;
    let window_type = r.u8()?;
    let page = r.u8()?;
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let slot = r.u8()?;
        let level = r.u8()?;
        let value1 = r.u8()?;
        let spd_abs = r.u8()?;
        let hand_byte = r.u8()?;
        let object_type_byte = r.u8()?;
        let usable = r.u8()? != 0;
        let value2 = r.u16_le()?;
        let price = r.u32_le()?;
        let model = r.u16_le()?;
        let name = r.pascal_string_int_le()?;
        items.push(MerchantOffer {
            slot,
            level,
            value1,
            spd_abs,
            hand_byte,
            object_type_byte,
            usable,
            value2,
            price,
            model,
            name,
        });
    }
    Ok(MerchantWindow {
        window_type,
        page,
        items,
    })
}

/// Encode matching `PacketLib1125.SendMerchantWindow` (one page).
#[must_use]
pub fn encode(w: &MerchantWindow) -> Vec<u8> {
    let mut pw = PacketWriter::with_capacity(64 + w.items.len() * 48);
    pw.u8(w.items.len() as u8).u8(w.window_type).u8(w.page);
    for it in &w.items {
        pw.u8(it.slot)
            .u8(it.level)
            .u8(it.value1)
            .u8(it.spd_abs)
            .u8(it.hand_byte)
            .u8(it.object_type_byte)
            .u8(if it.usable { 1 } else { 0 })
            .u16_le(it.value2)
            .u32_le(it.price)
            .u16_le(it.model)
            .pascal_string_int_le(&it.name, 0x30);
    }
    pw.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_matches_oracle_shape() {
        let w = MerchantWindow {
            window_type: window_type::NORMAL,
            page: 0,
            items: vec![MerchantOffer {
                slot: 0,
                level: 50,
                value1: 0,
                spd_abs: 0,
                hand_byte: 0,
                object_type_byte: 0x29,
                usable: true,
                value2: 0,
                price: 0,
                model: 1886,
                name: "SoloDAoC Mythirian of Readiness".into(),
            }],
        };
        let body = encode(&w);
        let got = decode(&body).expect("encode→decode");
        assert_eq!(got, w);
        assert_eq!(got.provenance(), PROVENANCE);
    }

    #[test]
    fn ticket_offer_fields_match_own_capture_sample() {
        // Second 0x17 from cap_20260714_230416_conn41042.bin ("ticket to Castle Sauvage").
        let pl = hex_compact(
            "01000000000100000001000000000000f301190000007469636b657420746f204361\
             73746c65205361757661676500",
        );
        let w = decode(&pl).expect("OWN_CAPTURE ticket 0x17");
        assert_eq!(w.items.len(), 1);
        assert_eq!(w.items[0].name, "ticket to Castle Sauvage");
        assert_eq!(w.items[0].model, 499);
        assert_eq!(w.items[0].value1, 1);
        assert!(w.items[0].usable);
        assert_eq!(w.provenance(), PROVENANCE);
    }

    #[test]
    fn short_payload_errors() {
        assert!(decode(&[0x01, 0x00]).is_err());
    }

    fn hex_compact(s: &str) -> Vec<u8> {
        let h: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..h.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
            .collect()
    }
}
