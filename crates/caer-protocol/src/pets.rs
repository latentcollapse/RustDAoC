//! Pet window S2C (0x88) and C2S command (0x8A).
//!
//! ## Provenance
//!
//! OPEN_ORACLE 1.127 inherits `PacketLib181.SendPetWindow` (no later override in PacketLib1124+).
//! Wire bytes are **not** the C# enum ordinals: `ePetWindowAction.Open` is 0 in C# but the sender
//! writes `2`. Decode the written bytes.
//!
//! **PetWindow 0x88** (`PacketLib181.SendPetWindow`):
//! ```text
//!   u16 BE  pet object id (0 if null / close)
//!   u8      unused 0
//!   u8      unused 0
//!   u8      window action  (0 close, 1 update, 2 open)
//!   u8      aggro          (0 none, 1 aggressive, 2 defensive, 3 passive)
//!   u8      walk           (0 none, 1 follow, 2 stay, 3 go-target, 4 come-here)
//!   u8      unused 0
//!   u8      pet-effect icon count (max 8)
//!   u16 BE  * count  icon ids
//! ```
//!
//! **PetWindow C2S 0x8A** (`PetWindowHandler`, no inner version gate):
//! ```text
//!   u8 aggro   (1 aggressive, 2 defensive, 3 passive; 0 ignore)
//!   u8 walk    (1 follow, 2 stay, 3 go-target, 4 come-here; 0 ignore)
//!   u8 command (1 attack, 2 release; 0 ignore)
//! ```
//!
//! One packet carries one pet oid. Necromancer shade/body and class multi-pet slots are **not**
//! in this packet — see world `cfx` remainder.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for PetWindow 0x88 (PacketLib181 / 1.127 inherit).
pub const PET_WINDOW_PROVENANCE: &str = "PetWindow 0x88 PacketLib181";

/// Max pet-effect icons the oracle writes (`icons.Count >= 8` break).
pub const MAX_PET_ICONS: usize = 8;

/// Wire window-action byte from `PacketLib181.SendPetWindow` (not C# `ePetWindowAction` ordinal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PetWindowAction {
    Close = 0,
    Update = 1,
    Open = 2,
    Other(u8),
}

impl PetWindowAction {
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Close,
            1 => Self::Update,
            2 => Self::Open,
            other => Self::Other(other),
        }
    }

    #[must_use]
    pub fn as_byte(self) -> u8 {
        match self {
            Self::Close => 0,
            Self::Update => 1,
            Self::Open => 2,
            Self::Other(b) => b,
        }
    }
}

/// Wire aggro byte (sender switch, not `eAggressionState` ordinal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PetAggro {
    None = 0,
    Aggressive = 1,
    Defensive = 2,
    Passive = 3,
    Other(u8),
}

impl PetAggro {
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::None,
            1 => Self::Aggressive,
            2 => Self::Defensive,
            3 => Self::Passive,
            other => Self::Other(other),
        }
    }

    #[must_use]
    pub fn as_byte(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Aggressive => 1,
            Self::Defensive => 2,
            Self::Passive => 3,
            Self::Other(b) => b,
        }
    }
}

/// Wire walk byte (sender switch, not `eWalkState` ordinal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PetWalk {
    None = 0,
    Follow = 1,
    Stay = 2,
    GoTarget = 3,
    ComeHere = 4,
    Other(u8),
}

impl PetWalk {
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::None,
            1 => Self::Follow,
            2 => Self::Stay,
            3 => Self::GoTarget,
            4 => Self::ComeHere,
            other => Self::Other(other),
        }
    }

    #[must_use]
    pub fn as_byte(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Follow => 1,
            Self::Stay => 2,
            Self::GoTarget => 3,
            Self::ComeHere => 4,
            Self::Other(b) => b,
        }
    }
}

/// C2S pet command byte (`PetWindowHandler`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PetCommand {
    Ignore = 0,
    Attack = 1,
    Release = 2,
    Other(u8),
}

impl PetCommand {
    #[must_use]
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Ignore,
            1 => Self::Attack,
            2 => Self::Release,
            other => Self::Other(other),
        }
    }

    #[must_use]
    pub fn as_byte(self) -> u8 {
        match self {
            Self::Ignore => 0,
            Self::Attack => 1,
            Self::Release => 2,
            Self::Other(b) => b,
        }
    }
}

/// A decoded PetWindow (0x88).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PetWindow {
    pub pet_id: u16,
    pub action: PetWindowAction,
    pub aggro: PetAggro,
    pub walk: PetWalk,
    pub icons: Vec<u16>,
}

impl PetWindow {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PET_WINDOW_PROVENANCE
    }

    #[must_use]
    pub fn is_close(&self) -> bool {
        matches!(self.action, PetWindowAction::Close) || self.pet_id == 0
    }
}

/// Decode PetWindow (PacketLib181 / 1.127).
pub fn decode_pet_window(payload: &[u8]) -> Result<PetWindow> {
    let mut r = PacketReader::new(payload);
    let pet_id = r.u16()?;
    let _unused0 = r.u8()?;
    let _unused1 = r.u8()?;
    let action = PetWindowAction::from_byte(r.u8()?);
    let aggro = PetAggro::from_byte(r.u8()?);
    let walk = PetWalk::from_byte(r.u8()?);
    let _unused2 = r.u8()?;
    let count = r.u8()? as usize;
    let n = count.min(MAX_PET_ICONS);
    let mut icons = Vec::with_capacity(n);
    for _ in 0..n {
        icons.push(r.u16()?);
    }
    Ok(PetWindow {
        pet_id,
        action,
        aggro,
        walk,
        icons,
    })
}

/// Encode matching `PacketLib181.SendPetWindow`.
#[must_use]
pub fn encode_pet_window(w: &PetWindow) -> Vec<u8> {
    let mut p = PacketWriter::with_capacity(16);
    p.u16(w.pet_id)
        .u8(0)
        .u8(0)
        .u8(w.action.as_byte())
        .u8(w.aggro.as_byte())
        .u8(w.walk.as_byte())
        .u8(0);
    let n = w.icons.len().min(MAX_PET_ICONS);
    p.u8(n as u8);
    for icon in w.icons.iter().take(n) {
        p.u16(*icon);
    }
    p.into_bytes()
}

/// Encode C2S PetWindow 0x8A (`PetWindowHandler`).
#[must_use]
pub fn encode_pet_command(aggro: PetAggro, walk: PetWalk, command: PetCommand) -> Vec<u8> {
    let mut p = PacketWriter::with_capacity(3);
    p.u8(aggro.as_byte())
        .u8(walk.as_byte())
        .u8(command.as_byte());
    p.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_window_matches_packetlib181_sender() {
        let w = PetWindow {
            pet_id: 0x1234,
            action: PetWindowAction::Open,
            aggro: PetAggro::Aggressive,
            walk: PetWalk::Follow,
            icons: vec![0x00AA, 0x00BB],
        };
        let body = encode_pet_window(&w);
        assert_eq!(
            body,
            [0x12, 0x34, 0x00, 0x00, 0x02, 0x01, 0x01, 0x00, 0x02, 0x00, 0xAA, 0x00, 0xBB]
        );
        let got = decode_pet_window(&body).expect("0x88");
        assert_eq!(got, w);
        assert_eq!(got.provenance(), PET_WINDOW_PROVENANCE);
        assert!(!got.is_close());
    }

    #[test]
    fn close_null_pet_matches_gameplayer_release() {
        // GamePlayer.SetControlledBrain(null) → SendPetWindow(null, Close, 0, 0)
        let w = PetWindow {
            pet_id: 0,
            action: PetWindowAction::Close,
            aggro: PetAggro::None,
            walk: PetWalk::None,
            icons: vec![],
        };
        let body = encode_pet_window(&w);
        assert_eq!(body, [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        let got = decode_pet_window(&body).expect("close");
        assert!(got.is_close());
        assert_eq!(got.pet_id, 0);
    }

    #[test]
    fn csharp_enum_ordinal_is_not_the_wire() {
        // ePetWindowAction.Open == 0 in C#; the sender writes 2.
        assert_ne!(PetWindowAction::Open.as_byte(), 0);
        assert_eq!(PetWindowAction::Open.as_byte(), 2);
        assert_eq!(PetWindowAction::Close.as_byte(), 0);
    }

    #[test]
    fn c2s_command_is_three_bytes() {
        let body = encode_pet_command(PetAggro::Passive, PetWalk::Stay, PetCommand::Release);
        assert_eq!(body, [3, 2, 2]);
    }
}
