//! `CharacterPointsUpdate` (0x91) — XP bar, realm/bounty points.
//!
//! ## Provenance
//!
//! OPEN_ORACLE: `PacketLib168.SendUpdatePoints` (16-byte form) and `PacketLib190.SendUpdatePoints`
//! (1.90+; 1.127 inherits the 190 layout with `WriteLongLowEndian` experience fields).
//!
//! Level progress is `level_permill` (0–999), not a chat scrape. Absolute experience longs are
//! present only on the 190+ body.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for CharacterPointsUpdate 0x91.
pub const POINTS_PROVENANCE: &str = "CharacterPointsUpdate 0x91";

/// Decoded XP / points snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharacterPoints {
    pub realm_points: u32,
    /// Thousandths of the current level (0–999). HUD XP bar.
    pub level_permill: u16,
    pub skill_specialty_points: u16,
    pub bounty_points: u32,
    pub realm_specialty_points: u16,
    pub champion_level_permill: u16,
    /// Absolute experience. `None` on the 16-byte PacketLib168 body.
    pub experience: Option<u64>,
    pub experience_for_next_level: Option<u64>,
}

impl CharacterPoints {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        POINTS_PROVENANCE
    }

    /// 0.0–1.0 XP-bar fraction from the server permill.
    #[must_use]
    pub fn xp_frac(self) -> f32 {
        (f32::from(self.level_permill) / 1000.0).clamp(0.0, 1.0)
    }
}

/// Decode CharacterPointsUpdate. 16-byte 168 bodies omit the experience longs.
pub fn decode_points(payload: &[u8]) -> Result<CharacterPoints> {
    let mut r = PacketReader::new(payload);
    let realm_points = r.u32()?;
    let level_permill = r.u16()?;
    let skill_specialty_points = r.u16()?;
    let bounty_points = r.u32()?;
    let realm_specialty_points = r.u16()?;
    let champion_level_permill = r.u16()?;
    let (experience, experience_for_next_level) = if r.remaining() >= 32 {
        let exp = r.u64_le()?;
        let next = r.u64_le()?;
        let _champ = r.u64_le()?;
        let _champ_next = r.u64_le()?;
        (Some(exp), Some(next))
    } else {
        (None, None)
    };
    Ok(CharacterPoints {
        realm_points,
        level_permill,
        skill_specialty_points,
        bounty_points,
        realm_specialty_points,
        champion_level_permill,
        experience,
        experience_for_next_level,
    })
}

/// Encode the PacketLib190 body (1.127).
#[must_use]
pub fn encode_points_190(p: &CharacterPoints) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(48);
    w.u32(p.realm_points)
        .u16(p.level_permill)
        .u16(p.skill_specialty_points)
        .u32(p.bounty_points)
        .u16(p.realm_specialty_points)
        .u16(p.champion_level_permill)
        .u64_le(p.experience.unwrap_or(0))
        .u64_le(p.experience_for_next_level.unwrap_or(0))
        .u64_le(0)
        .u64_le(0);
    w.into_bytes()
}

/// Encode the PacketLib168 16-byte body.
#[must_use]
pub fn encode_points_168(p: &CharacterPoints) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(16);
    w.u32(p.realm_points)
        .u16(p.level_permill)
        .u16(p.skill_specialty_points)
        .u32(p.bounty_points)
        .u16(p.realm_specialty_points)
        .u16(p.champion_level_permill);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CharacterPoints {
        CharacterPoints {
            realm_points: 0x0001_2345,
            level_permill: 420,
            skill_specialty_points: 3,
            bounty_points: 99,
            realm_specialty_points: 1,
            champion_level_permill: 0,
            experience: Some(1_000_000),
            experience_for_next_level: Some(2_000_000),
        }
    }

    #[test]
    fn points_190_roundtrip_and_xp_frac() {
        let p = sample();
        let body = encode_points_190(&p);
        assert_eq!(body.len(), 48);
        let got = decode_points(&body).expect("0x91 190");
        assert_eq!(got, p);
        assert_eq!(got.provenance(), POINTS_PROVENANCE);
        assert!((got.xp_frac() - 0.42).abs() < 1e-6);
    }

    #[test]
    fn points_168_omits_absolute_experience() {
        let mut p = sample();
        p.experience = None;
        p.experience_for_next_level = None;
        let body = encode_points_168(&p);
        assert_eq!(body.len(), 16);
        let got = decode_points(&body).expect("0x91 168");
        assert_eq!(got.level_permill, 420);
        assert!(
            got.experience.is_none(),
            "168 body must not invent exp longs"
        );
        assert_eq!(got.bounty_points, 99);
    }

    #[test]
    fn short_payload_errors() {
        assert!(decode_points(&[0u8; 8]).is_err());
    }
}
