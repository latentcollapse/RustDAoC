//! `PlayerDeath` (0xAE) + `PlayerRevive` (0x89) — death / release spine.
//!
//! ## Provenance
//!
//! REQ-021 / SCN-09: decoded fields are asserted against the oracle senders
//! (`SoloDAoC` `PacketLib168.SendPlayerDied` / `SendPlayerRevive`). OWN_CAPTURE sealed in
//! `fixtures/scn09_death/` (SoloDAoC live `player kill self` / `player rez self`, 2026-08-10).
//! Encoder→decoder round-trip bytes remain rejected via `SCN09_CAER_AUTHORED_CAPTURE_SHA256`.
//!
//! ## Wire form (PacketLib168 → 1.127; no later override found)
//!
//! **PlayerDeath 0xAE** (`SendPlayerDied`):
//! ```text
//!   u16 BE  killed player / living object id
//!   u16 BE  killer object id (0 if none)
//!   4× u8   pad (always 0)
//! ```
//!
//! **PlayerRevive 0x89** (`SendPlayerRevive`):
//! ```text
//!   u16 BE  revived player object id
//!   u16 BE  0
//! ```
//!
//! Honest finding: neither packet carries cause-of-death, XP, corpse model, or release type.
//! Do not invent those fields.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for UI / scenario asserts (REQ-021 / SCN-09).
pub const DEATH_PROVENANCE: &str = "PlayerDeath 0xAE";
/// Provenance tag for revive (REQ-021 / SCN-09).
pub const REVIVE_PROVENANCE: &str = "PlayerRevive 0x89";

/// A decoded PlayerDeath (0xAE).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerDeath {
    pub object_id: u16,
    /// Killer object id, or 0 when the oracle writes none.
    pub killer_id: u16,
}

impl PlayerDeath {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        DEATH_PROVENANCE
    }
}

/// A decoded PlayerRevive (0x89).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerRevive {
    pub object_id: u16,
}

impl PlayerRevive {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        REVIVE_PROVENANCE
    }
}

/// Decode a PlayerDeath payload.
pub fn decode_death(payload: &[u8]) -> Result<PlayerDeath> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let killer_id = r.u16()?;
    // Four pad bytes — present in every oracle write; consume so a short payload errors.
    let _ = r.u8()?;
    let _ = r.u8()?;
    let _ = r.u8()?;
    let _ = r.u8()?;
    Ok(PlayerDeath {
        object_id,
        killer_id,
    })
}

/// Encode matching `PacketLib168.SendPlayerDied`.
#[must_use]
pub fn encode_death(d: &PlayerDeath) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(8);
    w.u16(d.object_id).u16(d.killer_id).u8(0).u8(0).u8(0).u8(0);
    w.into_bytes()
}

/// Decode a PlayerRevive payload.
pub fn decode_revive(payload: &[u8]) -> Result<PlayerRevive> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let _pad = r.u16()?;
    Ok(PlayerRevive { object_id })
}

/// Encode matching `PacketLib168.SendPlayerRevive`.
#[must_use]
pub fn encode_revive(v: &PlayerRevive) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(4);
    w.u16(v.object_id).u16(0);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn death_fields_match_oracle_sender() {
        let d = PlayerDeath {
            object_id: 0x1234,
            killer_id: 0x00AB,
        };
        let body = encode_death(&d);
        assert_eq!(body, [0x12, 0x34, 0x00, 0xAB, 0x00, 0x00, 0x00, 0x00]);
        let got = decode_death(&body).expect("oracle-shaped 0xAE must decode");
        assert_eq!(got, d);
        assert_eq!(got.provenance(), DEATH_PROVENANCE);
    }

    #[test]
    fn death_with_no_killer() {
        let d = PlayerDeath {
            object_id: 42,
            killer_id: 0,
        };
        let got = decode_death(&encode_death(&d)).unwrap();
        assert_eq!(got.killer_id, 0);
    }

    #[test]
    fn revive_fields_match_oracle_sender() {
        let v = PlayerRevive { object_id: 0x0042 };
        let body = encode_revive(&v);
        assert_eq!(body, [0x00, 0x42, 0x00, 0x00]);
        let got = decode_revive(&body).expect("oracle-shaped 0x89 must decode");
        assert_eq!(got, v);
        assert_eq!(got.provenance(), REVIVE_PROVENANCE);
    }

    #[test]
    fn short_death_payload_errors() {
        assert!(decode_death(&[0x00, 0x01, 0x00, 0x02]).is_err());
    }
}
