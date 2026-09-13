//! Encumberance (0xBD) — player carry weight vs max.
//!
//! OPEN_ORACLE: `PacketLib168.SendEncumberance` writes two big-endian shorts:
//! `MaxEncumberance`, then current `Encumberance`. Not an inventory slot map.
//! Shipped Atlantis XML has no player-encumbrance AdapterName (only `mount_encumbrance`);
//! this decode is authority for EcoState, not an invented HUD label.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

pub const PROVENANCE: &str = "Encumberance 0xBD";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Encumberance {
    pub max: u16,
    pub used: u16,
}

impl Encumberance {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }
}

/// Decode Encumberance 0xBD. OPEN_ORACLE `PacketLib168.SendEncumberance`.
pub fn decode(payload: &[u8]) -> Result<Encumberance> {
    let mut r = PacketReader::new(payload);
    Ok(Encumberance {
        max: r.u16()?,
        used: r.u16()?,
    })
}

/// Encode matching `PacketLib168.SendEncumberance` (tests / fixtures).
#[must_use]
pub fn encode(e: &Encumberance) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(e.max).u16(e.used);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_is_max_then_used_be_shorts() {
        let body = encode(&Encumberance { max: 200, used: 45 });
        assert_eq!(body, [0x00, 0xc8, 0x00, 0x2d]);
        let d = decode(&body).expect("decode");
        assert_eq!(d.max, 200);
        assert_eq!(d.used, 45);
        assert_eq!(d.provenance(), PROVENANCE);
    }

    #[test]
    fn short_payload_is_error_not_invented_zero() {
        assert!(decode(&[0x00, 0x01]).is_err());
    }
}
