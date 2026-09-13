//! EmblemDialogue (0xE2) — guild emblem picker presence.
//! OPEN_ORACLE `PacketLib168.SendEmblemDialogue`: Fill(0x00, 4). No invented emblem pixels.

use crate::error::Result;

pub const PROVENANCE: &str = "EmblemDialogue 0xE2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmblemDialogue;

impl EmblemDialogue {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }
}

pub fn decode(payload: &[u8]) -> Result<EmblemDialogue> {
    if payload.len() < 4 {
        return Err(crate::error::ProtocolError::UnexpectedEof {
            offset: 0,
            needed: 4usize.saturating_sub(payload.len()),
        });
    }
    Ok(EmblemDialogue)
}

#[must_use]
pub fn encode() -> Vec<u8> {
    vec![0, 0, 0, 0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_zero_bytes() {
        let body = encode();
        assert_eq!(body, [0, 0, 0, 0]);
        decode(&body).expect("decode");
    }

    #[test]
    fn short_payload_errors() {
        assert!(decode(&[0, 0]).is_err());
    }
}
