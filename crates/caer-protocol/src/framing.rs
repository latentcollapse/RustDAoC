//! Packet framing — the two header layouts, ported from the oracle.
//!
//! The two directions are **not symmetric** (a recurring source of confusion; the layouts
//! come straight from DOLSharp so we don't have to rediscover them):
//!
//! ## Client → server (`GSPacketIn`, `HDR_SIZE = 12`)
//! Big-endian. `GSPacketIn.Load`:
//! ```text
//! offset 0..2  : packet_size (payload length, excludes the 12-byte header framing)
//! offset 2..4  : sequence
//! offset 4..6  : session_id
//! offset 6..8  : parameter
//! offset 8     : (high byte of id — unused; id is effectively 1 byte)
//! offset 9     : id  (packet code)
//! offset 10..  : payload  (packet_size bytes)
//! last 2 bytes : big-endian checksum trailer (part of the 12; 10 leading + 2 trailing)
//! ```
//!
//! ## Server → client (`GSTCPPacketOut`, 3-byte lead)
//! ```text
//! offset 0..2  : size (Length - 3, i.e. payload length)  [WritePacketLength]
//! offset 2     : packet_code
//! offset 3..   : payload
//! ```
//! No checksum on the server→client TCP path. Max total 2048 bytes or the original client
//! crashes (`PacketProcessor` guard) — we enforce it so a bug here can't wedge a real client.
//!
//! NOTE (unverified against a live trace, flagged not asserted): whether the on-wire packet
//! code carries the [`codes::PACKET_CODE_XOR`](crate::codes::PACKET_CODE_XOR) obfuscation on
//! this framing path, or only in the client's internal dispatch table, must be confirmed with
//! a golden capture before P1 is called done. The header field layouts above ARE confirmed
//! from the oracle.

use crate::checksum;
use crate::error::{ProtocolError, Result};

/// Max total packet size before the original client crashes (`PacketProcessor` guard).
pub const MAX_PACKET_SIZE: usize = 2048;

/// Client→server framing overhead: 10-byte leading header + 2-byte checksum trailer.
pub const CLIENT_HDR_SIZE: usize = 12;
/// Server→client framing overhead: 2-byte size + 1-byte code.
pub const SERVER_HDR_SIZE: usize = 3;

/// A parsed client→server header (as the server sees it). We *emit* these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientPacketHeader {
    pub packet_size: u16,
    pub sequence: u16,
    pub session_id: u16,
    pub parameter: u16,
    pub id: u8,
}

impl ClientPacketHeader {
    /// Encode a full client→server packet: header + payload + checksum trailer.
    pub fn encode(&self, payload: &[u8]) -> Result<Vec<u8>> {
        let total = CLIENT_HDR_SIZE + payload.len();
        if total > MAX_PACKET_SIZE {
            return Err(ProtocolError::OversizedPacket {
                declared: total,
                max: MAX_PACKET_SIZE,
            });
        }
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes()); // 0..2 size
        out.extend_from_slice(&self.sequence.to_be_bytes()); // 2..4
        out.extend_from_slice(&self.session_id.to_be_bytes()); // 4..6
        out.extend_from_slice(&self.parameter.to_be_bytes()); // 6..8
        out.push(0x00); // 8: unused high id byte
        out.push(self.id); // 9: packet code
        out.extend_from_slice(payload); // 10..
        checksum::append(&mut out); // trailing big-endian checksum
        Ok(out)
    }

    /// Parse a client→server packet (verifying the checksum trailer). Returns header + payload.
    pub fn decode(buf: &[u8]) -> Result<(Self, &[u8])> {
        if buf.len() < CLIENT_HDR_SIZE {
            return Err(ProtocolError::UnexpectedEof {
                offset: buf.len(),
                needed: CLIENT_HDR_SIZE - buf.len(),
            });
        }
        if !checksum::verify_trailer(buf) {
            let carried = (u16::from(buf[buf.len() - 2]) << 8) | u16::from(buf[buf.len() - 1]);
            let computed = checksum::calculate(&buf[..buf.len() - 2]);
            return Err(ProtocolError::ChecksumMismatch { computed, carried });
        }
        let be = |a: usize| (u16::from(buf[a]) << 8) | u16::from(buf[a + 1]);
        let header = Self {
            packet_size: be(0),
            sequence: be(2),
            session_id: be(4),
            parameter: be(6),
            id: buf[9],
        };
        // payload sits between the 10-byte lead and the 2-byte checksum trailer
        let payload = &buf[10..buf.len() - 2];
        Ok((header, payload))
    }
}

/// A server→client header (as the client sees it). We *parse* these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerPacketHeader {
    pub size: u16,
    pub code: u8,
}

impl ServerPacketHeader {
    /// Encode a server→client TCP packet: 2-byte size (payload len) + code + payload.
    pub fn encode(code: u8, payload: &[u8]) -> Result<Vec<u8>> {
        let total = SERVER_HDR_SIZE + payload.len();
        if total > MAX_PACKET_SIZE {
            return Err(ProtocolError::OversizedPacket {
                declared: total,
                max: MAX_PACKET_SIZE,
            });
        }
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.push(code);
        out.extend_from_slice(payload);
        Ok(out)
    }

    /// Try to parse ONE server→client packet from the front of `buf`. Returns the header, its
    /// payload, and the total bytes consumed — or `Ok(None)` if `buf` doesn't yet hold a full
    /// packet (the caller keeps buffering the TCP stream). This is the frame-splitter for the
    /// receive loop.
    pub fn decode_prefix(buf: &[u8]) -> Result<Option<(Self, &[u8], usize)>> {
        if buf.len() < SERVER_HDR_SIZE {
            return Ok(None);
        }
        let size = ((u16::from(buf[0]) << 8) | u16::from(buf[1])) as usize;
        let total = SERVER_HDR_SIZE + size;
        if total > MAX_PACKET_SIZE {
            return Err(ProtocolError::OversizedPacket {
                declared: total,
                max: MAX_PACKET_SIZE,
            });
        }
        if buf.len() < total {
            return Ok(None); // partial — wait for more stream
        }
        let header = Self {
            size: size as u16,
            code: buf[2],
        };
        Ok(Some((header, &buf[3..total], total)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_packet_roundtrips_with_checksum() {
        let hdr = ClientPacketHeader {
            packet_size: 0, // set by encode from payload
            sequence: 0x0007,
            session_id: 0x00A5,
            parameter: 0,
            id: 0xF4, // CryptKeyRequest (logical)
        };
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let wire = hdr.encode(&payload).unwrap();
        let (parsed, body) = ClientPacketHeader::decode(&wire).unwrap();
        assert_eq!(parsed.sequence, 0x0007);
        assert_eq!(parsed.session_id, 0x00A5);
        assert_eq!(parsed.id, 0xF4);
        assert_eq!(parsed.packet_size as usize, payload.len());
        assert_eq!(body, &payload);
    }

    #[test]
    fn corrupt_client_packet_fails_checksum() {
        let hdr = ClientPacketHeader {
            packet_size: 0,
            sequence: 1,
            session_id: 2,
            parameter: 0,
            id: 0x10,
        };
        let mut wire = hdr.encode(&[1, 2, 3]).unwrap();
        wire[10] ^= 0xFF; // flip a payload byte
        assert!(matches!(
            ClientPacketHeader::decode(&wire),
            Err(ProtocolError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn server_frame_splitter_handles_partial_and_multiple() {
        let p1 = ServerPacketHeader::encode(0x88, &[1, 2, 3]).unwrap();
        let p2 = ServerPacketHeader::encode(0x22, &[9]).unwrap();

        // partial: only the first 2 bytes of p1
        assert_eq!(ServerPacketHeader::decode_prefix(&p1[..2]).unwrap(), None);

        // stream of two packets — split one at a time
        let mut stream = p1.clone();
        stream.extend_from_slice(&p2);
        let (h1, body1, used1) = ServerPacketHeader::decode_prefix(&stream).unwrap().unwrap();
        assert_eq!(h1.code, 0x88);
        assert_eq!(body1, &[1, 2, 3]);
        assert_eq!(used1, p1.len());

        let (h2, body2, used2) = ServerPacketHeader::decode_prefix(&stream[used1..])
            .unwrap()
            .unwrap();
        assert_eq!(h2.code, 0x22);
        assert_eq!(body2, &[9]);
        assert_eq!(used2, p2.len());
    }

    #[test]
    fn oversized_is_rejected_both_ways() {
        let big = vec![0u8; MAX_PACKET_SIZE];
        let hdr = ClientPacketHeader {
            packet_size: 0,
            sequence: 0,
            session_id: 0,
            parameter: 0,
            id: 1,
        };
        assert!(matches!(
            hdr.encode(&big),
            Err(ProtocolError::OversizedPacket { .. })
        ));
        assert!(matches!(
            ServerPacketHeader::encode(1, &big),
            Err(ProtocolError::OversizedPacket { .. })
        ));
    }
}
