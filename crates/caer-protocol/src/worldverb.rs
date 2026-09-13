//! Client → server world verbs: doors + ground targeting (System 5 Stream A).
//!
//! ## DoorRequest (0x99)
//! OPEN_ORACLE: SoloDAoC `DoorRequestHandler` reads `u32 doorID` + `u8 doorState`
//! (`0x01` = open, else close). Confirm is server DoorState (also 0x99 S2C).
//!
//! ## PlayerGroundTarget (0xEC)
//! OPEN_ORACLE: `PlayerGroundTargetHandler` reads `u32 x,y,z` + `u16 flag`.
//! Bit `0x100` = ground target in view. Does **not** echo ChangeGroundTarget;
//! landing confirmation for a GT spell is SpellEffect (0x1B), never an optimistic
//! local apply from the C2S send (same reject-equip rule).

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Door open request state byte (OPEN_ORACLE `DoorRequestHandler` / `ChangeDoorAction`).
pub const DOOR_STATE_OPEN: u8 = 0x01;
/// Door close request state byte.
pub const DOOR_STATE_CLOSE: u8 = 0x00;
/// PlayerGroundTarget flag: ground target asserted in view (`PlayerGroundTargetHandler`).
pub const GROUND_TARGET_IN_VIEW: u16 = 0x100;

/// Encode DoorRequest (0x99). `door_state` is `DOOR_STATE_OPEN` / `DOOR_STATE_CLOSE`.
#[must_use]
pub fn encode_door_request(door_id: u32, door_state: u8) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32(door_id).u8(door_state);
    w.into_bytes()
}

/// Encode PlayerGroundTarget (0xEC).
#[must_use]
pub fn encode_ground_target(x: i32, y: i32, z: i32, flag: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32(x as u32).u32(y as u32).u32(z as u32).u16(flag);
    w.into_bytes()
}

/// Encode PlayerSitRequest (0xC7). OPEN_ORACLE `PlayerSitRequestHandler`: one status byte,
/// nonzero = sit. Does **not** invent a local sit pose.
#[must_use]
pub fn encode_sit_request(sit: bool) -> Vec<u8> {
    vec![u8::from(sit)]
}

/// Decoded DoorState (0x99 S2C) — OPEN_ORACLE `PacketLib168.SendDoorState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoorState {
    pub door_id: u32,
    /// `true` when open byte is `0x01`.
    pub open: bool,
    pub flag: u8,
}

/// Provenance tag for DoorState (REQ-021 style).
pub const DOOR_STATE_PROVENANCE: &str = "DoorState 0x99";

impl DoorState {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        DOOR_STATE_PROVENANCE
    }
}

/// Decode DoorState body: `u32 doorID`, open `u8`, flag `u8`, `0xFF`, `0x00`.
pub fn decode_door_state(payload: &[u8]) -> Result<DoorState> {
    let mut r = PacketReader::new(payload);
    let door_id = r.u32()?;
    let open = r.u8()? == DOOR_STATE_OPEN;
    let flag = r.u8()?;
    let _pad_ff = r.u8()?;
    let _pad_0 = r.u8()?;
    Ok(DoorState {
        door_id,
        open,
        flag,
    })
}

/// Encode matching `SendDoorState` (tests / fixtures).
#[must_use]
pub fn encode_door_state(d: &DoorState) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(8);
    w.u32(d.door_id)
        .u8(if d.open {
            DOOR_STATE_OPEN
        } else {
            DOOR_STATE_CLOSE
        })
        .u8(d.flag)
        .u8(0xFF)
        .u8(0x00);
    w.into_bytes()
}

/// Decoded ChangeGroundTarget (0xDF S2C) — OPEN_ORACLE `PacketLib168.SendChangeGroundTarget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeGroundTarget {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Provenance for server-driven GT changes (GroundAssist etc.).
pub const CHANGE_GROUND_TARGET_PROVENANCE: &str = "ChangeGroundTarget 0xDF";

impl ChangeGroundTarget {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        CHANGE_GROUND_TARGET_PROVENANCE
    }
}

/// Decode ChangeGroundTarget: three big-endian ints.
pub fn decode_change_ground_target(payload: &[u8]) -> Result<ChangeGroundTarget> {
    let mut r = PacketReader::new(payload);
    let x = r.u32()? as i32;
    let y = r.u32()? as i32;
    let z = r.u32()? as i32;
    Ok(ChangeGroundTarget { x, y, z })
}

/// Encode matching `SendChangeGroundTarget`.
#[must_use]
pub fn encode_change_ground_target(g: &ChangeGroundTarget) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(12);
    w.u32(g.x as u32).u32(g.y as u32).u32(g.z as u32);
    w.into_bytes()
}

/// Decoded CharacterJump (0x04 S2C) — OPEN_ORACLE `PacketLib168.SendPlayerJump`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharacterJump {
    pub x: i32,
    pub y: i32,
    pub z: u16,
    pub object_id: u16,
    pub heading: u16,
    pub house: u16,
}

/// Provenance for teleport / MoveTo confirm.
pub const CHARACTER_JUMP_PROVENANCE: &str = "CharacterJump 0x04";

impl CharacterJump {
    #[must_use]
    pub fn provenance(self) -> &'static str {
        CHARACTER_JUMP_PROVENANCE
    }
}

/// Decode CharacterJump: X/Y int, oid/Z/heading/house shorts (all BE).
pub fn decode_character_jump(payload: &[u8]) -> Result<CharacterJump> {
    let mut r = PacketReader::new(payload);
    let x = r.u32()? as i32;
    let y = r.u32()? as i32;
    let object_id = r.u16()?;
    let z = r.u16()?;
    let heading = r.u16()?;
    let house = r.u16().unwrap_or(0);
    Ok(CharacterJump {
        x,
        y,
        z,
        object_id,
        heading,
        house,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn door_request_wire_matches_oracle() {
        let body = encode_door_request(2184203, DOOR_STATE_OPEN);
        assert_eq!(body.len(), 5);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u32().unwrap(), 2184203);
        assert_eq!(r.u8().unwrap(), 0x01);
    }

    #[test]
    fn ground_target_wire_matches_oracle() {
        let body = encode_ground_target(100, 200, 300, GROUND_TARGET_IN_VIEW);
        assert_eq!(body.len(), 14);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u32().unwrap(), 100);
        assert_eq!(r.u32().unwrap(), 200);
        assert_eq!(r.u32().unwrap(), 300);
        assert_eq!(r.u16().unwrap(), 0x100);
    }

    #[test]
    fn sit_request_is_one_status_byte() {
        assert_eq!(encode_sit_request(true), [1]);
        assert_eq!(encode_sit_request(false), [0]);
    }

    #[test]
    fn door_state_round_trip() {
        let d = DoorState {
            door_id: 2184203,
            open: true,
            flag: 1,
        };
        let body = encode_door_state(&d);
        assert_eq!(decode_door_state(&body).unwrap(), d);
    }

    #[test]
    fn change_ground_target_round_trip() {
        let g = ChangeGroundTarget {
            x: 531000,
            y: 478100,
            z: 2500,
        };
        let body = encode_change_ground_target(&g);
        assert_eq!(decode_change_ground_target(&body).unwrap(), g);
    }

    #[test]
    fn character_jump_wire_matches_oracle() {
        let mut w = PacketWriter::new();
        w.u32(531332).u32(478446).u16(42).u16(2456).u16(1024).u16(0);
        let j = decode_character_jump(&w.into_bytes()).unwrap();
        assert_eq!(j.x, 531332);
        assert_eq!(j.y, 478446);
        assert_eq!(j.z, 2456);
        assert_eq!(j.object_id, 42);
    }
}
