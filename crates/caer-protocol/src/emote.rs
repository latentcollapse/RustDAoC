//! EmoteAnimation (0xF9) — social presentation, not inventory/group authority.
//!
//! OPEN_ORACLE `PacketLib168.SendEmoteAnimation`: oid u16 BE, emote u8, pad u8.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

pub const PROVENANCE: &str = "EmoteAnimation 0xF9";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmoteAnimation {
    pub object_id: u16,
    pub emote: u8,
}

impl EmoteAnimation {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }
}

pub fn decode(payload: &[u8]) -> Result<EmoteAnimation> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let emote = r.u8()?;
    let _pad = r.u8()?;
    Ok(EmoteAnimation { object_id, emote })
}

#[must_use]
pub fn encode(e: &EmoteAnimation) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(e.object_id).u8(e.emote).u8(0);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_is_oid_emote_pad() {
        let body = encode(&EmoteAnimation {
            object_id: 0x1234,
            emote: 7,
        });
        assert_eq!(body, [0x12, 0x34, 0x07, 0x00]);
        let d = decode(&body).expect("decode");
        assert_eq!(d.object_id, 0x1234);
        assert_eq!(d.emote, 7);
        assert_eq!(d.provenance(), PROVENANCE);
    }
}
