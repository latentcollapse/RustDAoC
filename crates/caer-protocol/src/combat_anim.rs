//! `CombatAnimation` (0xBC) — structured melee/spell swing feedback.
//!
//! ## Provenance
//!
//! REQ-021: decoded fields are asserted against the oracle sender
//! (`SoloDAoC` `PacketLib186.SendCombatAnimation`), and any displayed combat feedback from this
//! path carries [`PROVENANCE`] — never a value scraped from chat text (`Message` 0xAF).
//!
//! ## Wire form (1.127 / PacketLib186+)
//!
//! ```text
//!   u16 BE  attacker object id (0 if none)
//!   u16 BE  defender object id (0 if none)
//!   u16 BE  attacker weapon model
//!   u16 BE  defender weapon / shield model
//!   u16 LE  style / animation id   ← ReadShortLowEndian
//!   u8      stance
//!   u8      result   (client anim code — see [`CombatResult`])
//!   u8      defender health percent
//!   u8      unk (always 0 in captures)
//! ```
//!
//! The older `PacketLib168` form (style as a single byte) is not what 1.127 sends — every sample
//! in `rustdaoc_combat_20260716.bin` is exactly 14 bytes matching 186.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for displayed values that came from this packet (REQ-021 / SCN-05).
pub const PROVENANCE: &str = "CombatAnimation 0xBC";

/// Client-facing result byte as written by `GameLiving.ShowAttackAnimation` (not the raw
/// `eAttackResult` enum ordinal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CombatResult {
    Missed = 0,
    Parried = 1,
    Blocked = 2,
    Evaded = 3,
    Fumbled = 4,
    /// Unstyled hit (`resultByte = 10`).
    HitUnstyled = 10,
    /// Style hit (`resultByte = 11`).
    HitStyle = 11,
    /// Anything else the capture has seen (spell feedback often uses 0x0A / 0x14).
    Other(u8),
}

impl CombatResult {
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Missed,
            1 => Self::Parried,
            2 => Self::Blocked,
            3 => Self::Evaded,
            4 => Self::Fumbled,
            10 => Self::HitUnstyled,
            11 => Self::HitStyle,
            other => Self::Other(other),
        }
    }

    #[must_use]
    pub fn as_byte(self) -> u8 {
        match self {
            Self::Missed => 0,
            Self::Parried => 1,
            Self::Blocked => 2,
            Self::Evaded => 3,
            Self::Fumbled => 4,
            Self::HitUnstyled => 10,
            Self::HitStyle => 11,
            Self::Other(b) => b,
        }
    }

    /// Short label for floating feedback — never a damage number (0xBC does not carry one).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Missed => "MISS",
            Self::Parried => "PARRY",
            Self::Blocked => "BLOCK",
            Self::Evaded => "EVADE",
            Self::Fumbled => "FUMBLE",
            Self::HitUnstyled | Self::HitStyle => "HIT",
            Self::Other(_) => "HIT",
        }
    }

    #[must_use]
    pub fn is_hit(self) -> bool {
        match self {
            Self::HitUnstyled | Self::HitStyle => true,
            Self::Other(b) if b == 0x0A || b == 0x14 => true,
            _ => false,
        }
    }
}

/// A decoded CombatAnimation (0xBC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombatAnimation {
    pub attacker_id: u16,
    pub defender_id: u16,
    pub weapon_id: u16,
    pub shield_id: u16,
    pub style: u16,
    pub stance: u8,
    pub result: CombatResult,
    pub target_health_pct: u8,
    /// Trailing unk byte (PacketLib186 always writes 0).
    pub unk: u8,
}

impl CombatAnimation {
    /// Provenance string for UI / scenario asserts (REQ-021).
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }
}

/// Decode a 1.127 CombatAnimation payload.
pub fn decode(payload: &[u8]) -> Result<CombatAnimation> {
    let mut r = PacketReader::new(payload);
    let attacker_id = r.u16()?;
    let defender_id = r.u16()?;
    let weapon_id = r.u16()?;
    let shield_id = r.u16()?;
    let style = r.u16_le()?;
    let stance = r.u8()?;
    let result = CombatResult::from_byte(r.u8()?);
    let target_health_pct = r.u8()?;
    let unk = r.u8().unwrap_or(0);
    Ok(CombatAnimation {
        attacker_id,
        defender_id,
        weapon_id,
        shield_id,
        style,
        stance,
        result,
        target_health_pct,
        unk,
    })
}

/// Encode a payload matching `PacketLib186.SendCombatAnimation` (for golden / SCN-05 fixtures).
#[must_use]
pub fn encode_186(anim: &CombatAnimation) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(14);
    w.u16(anim.attacker_id);
    w.u16(anim.defender_id);
    w.u16(anim.weapon_id);
    w.u16(anim.shield_id);
    w.u16_le(anim.style);
    w.u8(anim.stance);
    w.u8(anim.result.as_byte());
    w.u8(anim.target_health_pct);
    w.u8(anim.unk);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// REQ-021: field values match the oracle sender layout (PacketLib186).
    #[test]
    fn fields_match_oracle_sender_186() {
        let anim = CombatAnimation {
            attacker_id: 0x4166,
            defender_id: 0x335a,
            weapon_id: 3,
            shield_id: 0,
            style: 0,
            stance: 0,
            result: CombatResult::HitUnstyled,
            target_health_pct: 94,
            unk: 0,
        };
        let body = encode_186(&anim);
        assert_eq!(body.len(), 14);
        let d = decode(&body).expect("oracle-shaped 0xBC must decode");
        assert_eq!(d, anim);
        assert_eq!(d.provenance(), PROVENANCE);
        assert_eq!(d.result.label(), "HIT");
    }

    /// Captured sample from `rustdaoc_combat_20260716.bin` (first 0xBC payload).
    #[test]
    fn decodes_captured_hit_unstyled() {
        // 0000 335a 0000 0000 0000 00 0a 5e 00
        let payload: &[u8] = &[
            0x00, 0x00, 0x33, 0x5a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x5e, 0x00,
        ];
        let d = decode(payload).unwrap();
        assert_eq!(d.attacker_id, 0);
        assert_eq!(d.defender_id, 0x335a);
        assert_eq!(d.result, CombatResult::HitUnstyled);
        assert_eq!(d.target_health_pct, 94);
        assert_eq!(d.unk, 0);
        assert_eq!(d.provenance(), "CombatAnimation 0xBC");
    }

    #[test]
    fn result_labels_are_not_damage_numbers() {
        assert_eq!(CombatResult::Missed.label(), "MISS");
        assert_eq!(CombatResult::Parried.label(), "PARRY");
        // 0xBC never carries a damage amount — labels only.
        assert!(!CombatResult::HitStyle
            .label()
            .chars()
            .any(|c| c.is_ascii_digit()));
    }
}
