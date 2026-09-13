//! `RegionChanged` (0xB7) — in-world region transition.
//!
//! ## Provenance
//!
//! OPEN_ORACLE: SoloDAoC `PacketLib174.SendRegionChanged` (inherited by PacketLib1127 / 1.127;
//! no later override). PacketLib168 wrote only region+pad; 173 added zone/cause; **174** adds
//! server-id + `0xFFBF` trailer — that is the 1.127 wire.
//!
//! ## Wire form (PacketLib174)
//!
//! ```text
//!   u16 BE  region skin id
//!   u16 BE  zone skin id
//!   u16 BE  0
//!   u16 BE  cause (1 = region change)
//!   u8      server id (oracle writes 0x0C)
//!   u8      0
//!   u16 BE  0xFFBF trailer
//! ```

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for scenario / UI asserts.
pub const PROVENANCE: &str = "RegionChanged 0xB7 PacketLib174";

/// A decoded RegionChanged (0xB7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionChanged {
    pub region_id: u16,
    pub zone_skin_id: u16,
    pub cause: u16,
    pub server_id: u8,
}

impl RegionChanged {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        PROVENANCE
    }
}

/// Decode PacketLib174 / 1.127 RegionChanged.
pub fn decode(payload: &[u8]) -> Result<RegionChanged> {
    let mut r = PacketReader::new(payload);
    let region_id = r.u16()?;
    let zone_skin_id = r.u16()?;
    let _pad = r.u16()?;
    let cause = r.u16()?;
    // PacketLib173 stopped here (8B). PacketLib174 / 1.127 appends server id + trailer.
    let (server_id, _unk) = if r.remaining() >= 4 {
        let server_id = r.u8()?;
        let unk = r.u8()?;
        let trailer = r.u16()?;
        if trailer != 0xFFBF {
            return Err(crate::error::ProtocolError::BadString(
                "RegionChanged PacketLib174 trailer",
            ));
        }
        (server_id, unk)
    } else {
        (0, 0)
    };
    Ok(RegionChanged {
        region_id,
        zone_skin_id,
        cause,
        server_id,
    })
}

/// Encode matching `PacketLib174.SendRegionChanged`.
#[must_use]
pub fn encode(r: &RegionChanged) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(12);
    w.u16(r.region_id)
        .u16(r.zone_skin_id)
        .u16(0)
        .u16(r.cause)
        .u8(r.server_id)
        .u8(0)
        .u16(0xFFBF);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_packetlib174() {
        let r = RegionChanged {
            region_id: 51, // Midgard starter-ish skin in many tables; value is opaque here
            zone_skin_id: 50,
            cause: 1,
            server_id: 0x0C,
        };
        let body = encode(&r);
        assert_eq!(body.len(), 12);
        let dec = decode(&body).expect("decode");
        assert_eq!(dec, r);
        assert_eq!(dec.provenance(), PROVENANCE);
    }

    #[test]
    fn rejects_bad_trailer_on_full_body() {
        let mut body = encode(&RegionChanged {
            region_id: 1,
            zone_skin_id: 0,
            cause: 1,
            server_id: 0x0C,
        });
        body[10] = 0x00;
        body[11] = 0x00;
        assert!(decode(&body).is_err());
    }
}
