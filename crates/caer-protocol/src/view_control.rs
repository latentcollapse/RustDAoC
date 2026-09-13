//! RemoveObject (0xA2) / ChangeTarget (0xF6) / UDPInitReply (0x2F) — Shape-2 first-loop paths.
//!
//! OPEN_ORACLE PacketLib168. Distinct from ObjectDelete 0xE1 (view culling with unknown=1).

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

pub const REMOVE_OBJECT_PROVENANCE: &str = "RemoveObject 0xA2 PacketLib168";
pub const CHANGE_TARGET_PROVENANCE: &str = "ChangeTarget 0xF6 PacketLib168";
pub const UDP_INIT_REPLY_PROVENANCE: &str = "UDPInitReply 0x2F PacketLib168";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoveObject {
    pub object_id: u16,
    /// 0 = dead NPC, 1 = living NPC, 2 = player (oracle SendRemoveObject).
    pub object_type: u16,
}

impl RemoveObject {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        REMOVE_OBJECT_PROVENANCE
    }
}

pub fn decode_remove_object(payload: &[u8]) -> Result<RemoveObject> {
    let mut r = PacketReader::new(payload);
    Ok(RemoveObject {
        object_id: r.u16()?,
        object_type: r.u16()?,
    })
}

#[must_use]
pub fn encode_remove_object(o: &RemoveObject) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(o.object_id).u16(o.object_type);
    w.into_bytes()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangeTarget {
    /// 0 clears the server-driven target.
    pub object_id: u16,
}

impl ChangeTarget {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        CHANGE_TARGET_PROVENANCE
    }

    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.object_id == 0
    }
}

pub fn decode_change_target(payload: &[u8]) -> Result<ChangeTarget> {
    let mut r = PacketReader::new(payload);
    let object_id = r.u16()?;
    let _pad = r.u16()?;
    Ok(ChangeTarget { object_id })
}

#[must_use]
pub fn encode_change_target(oid: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u16(oid).u16(0);
    w.into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpInitReply {
    pub region_ip: String,
    pub udp_port: u16,
}

impl UdpInitReply {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        UDP_INIT_REPLY_PROVENANCE
    }
}

/// PacketLib168: FillString(ip, 22) + WriteShort(UDPPort), or 0x18 zero bytes when no region.
pub fn decode_udp_init_reply(payload: &[u8]) -> Result<UdpInitReply> {
    if payload.len() < 24 {
        return Err(crate::error::ProtocolError::UnexpectedEof {
            offset: 0,
            needed: 24usize.saturating_sub(payload.len()),
        });
    }
    if payload.iter().all(|&b| b == 0) {
        return Ok(UdpInitReply {
            region_ip: String::new(),
            udp_port: 0,
        });
    }
    let ip_bytes = &payload[..22];
    let end = ip_bytes.iter().position(|&c| c == 0).unwrap_or(22);
    let region_ip = String::from_utf8_lossy(&ip_bytes[..end]).into_owned();
    let udp_port = u16::from_be_bytes([payload[22], payload[23]]);
    Ok(UdpInitReply {
        region_ip,
        udp_port,
    })
}

#[must_use]
pub fn encode_udp_init_reply(ip: &str, port: u16) -> Vec<u8> {
    let mut w = PacketWriter::new();
    let mut buf = [0u8; 22];
    let bytes = ip.as_bytes();
    let n = bytes.len().min(22);
    buf[..n].copy_from_slice(&bytes[..n]);
    w.bytes(&buf).u16(port);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_object_round_trip() {
        let body = encode_remove_object(&RemoveObject {
            object_id: 0x1234,
            object_type: 2,
        });
        let d = decode_remove_object(&body).unwrap();
        assert_eq!(d.object_id, 0x1234);
        assert_eq!(d.object_type, 2);
    }

    #[test]
    fn change_target_clear_is_zero() {
        let body = encode_change_target(0);
        let d = decode_change_target(&body).unwrap();
        assert!(d.is_clear());
    }

    #[test]
    fn udp_init_reply_ip_and_port() {
        let body = encode_udp_init_reply("127.0.0.1", 10412);
        let d = decode_udp_init_reply(&body).unwrap();
        assert_eq!(d.region_ip, "127.0.0.1");
        assert_eq!(d.udp_port, 10412);
    }
}
