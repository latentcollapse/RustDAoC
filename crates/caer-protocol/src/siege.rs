//! SiegeWeaponAnimation (0xE3) / SiegeWeaponInterface (0xF5) / C2S SiegeCommand (0xF5).
//!
//! Animation prefix is shared by PacketLib168 and PacketLib1124 (1.127 vtable). Action trailer
//! after the prefix is opaque — 1124 varies by CurrentAction; do not invent it.
//! Interface header is enough to know open vs close (`SendSiegeWeaponCloseInterface`).

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

pub const ANIM_PROVENANCE: &str = "SiegeWeaponAnimation 0xE3 PacketLib1124 prefix";
pub const INTERFACE_PROVENANCE: &str = "SiegeWeaponInterface 0xF5";
pub const COMMAND_PROVENANCE: &str = "SiegeCommandRequest 0xF5 SiegeWeaponActionHandler";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiegeWeaponAnimation {
    pub object_id: u32,
    pub aim_x: u32,
    pub aim_y: u32,
    pub aim_z: u32,
    pub target_oid: u32,
    pub effect: u16,
    pub timer: u16,
    pub action: u8,
}

impl SiegeWeaponAnimation {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        ANIM_PROVENANCE
    }
}

pub fn decode_animation(payload: &[u8]) -> Result<SiegeWeaponAnimation> {
    let mut r = PacketReader::new(payload);
    Ok(SiegeWeaponAnimation {
        object_id: r.u32()?,
        aim_x: r.u32()?,
        aim_y: r.u32()?,
        aim_z: r.u32()?,
        target_oid: r.u32()?,
        effect: r.u16()?,
        timer: r.u16()?,
        action: r.u8()?,
    })
}

#[must_use]
pub fn encode_animation_prefix(a: &SiegeWeaponAnimation) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32(a.object_id)
        .u32(a.aim_x)
        .u32(a.aim_y)
        .u32(a.aim_z)
        .u32(a.target_oid)
        .u16(a.effect)
        .u16(a.timer)
        .u8(a.action)
        .u8(0)
        .u8(0)
        .u8(0);
    w.into_bytes()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiegeWeaponInterface {
    pub flag: u16,
    pub close: u8,
}

impl SiegeWeaponInterface {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        INTERFACE_PROVENANCE
    }

    #[must_use]
    pub fn is_close(&self) -> bool {
        self.close != 0
    }
}

/// Header only. Close packet is `WriteShort(0); WriteShort(1); Fill(0,13)`.
pub fn decode_interface(payload: &[u8]) -> Result<SiegeWeaponInterface> {
    let mut r = PacketReader::new(payload);
    let flag = r.u16()?;
    let _unk = r.u8()?;
    let close = r.u8()?;
    Ok(SiegeWeaponInterface { flag, close })
}

#[must_use]
pub fn encode_interface_close() -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(0).u16(1);
    for _ in 0..13 {
        w.u8(0);
    }
    w.into_bytes()
}

/// C2S 0xF5. OPEN_ORACLE `SiegeWeaponActionHandler`: unk u16, action u8, ammo u8.
#[must_use]
pub fn encode_command(action: u8, ammo: u8) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(0).u8(action).u8(ammo);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_prefix_decodes_168_and_1124_head() {
        let a = SiegeWeaponAnimation {
            object_id: 9,
            aim_x: 1,
            aim_y: 2,
            aim_z: 3,
            target_oid: 4,
            effect: 5,
            timer: 6,
            action: 2,
        };
        let body = encode_animation_prefix(&a);
        let d = decode_animation(&body).expect("decode");
        assert_eq!(d.object_id, 9);
        assert_eq!(d.action, 2);
        assert_eq!(d.provenance(), ANIM_PROVENANCE);
    }

    #[test]
    fn close_interface_sets_close_byte() {
        let body = encode_interface_close();
        let d = decode_interface(&body).expect("decode");
        assert!(d.is_close());
        assert_eq!(d.flag, 0);
    }

    #[test]
    fn command_is_unk_short_then_action_ammo() {
        let body = encode_command(4, 1);
        assert_eq!(body, [0, 0, 4, 1]);
    }
}
