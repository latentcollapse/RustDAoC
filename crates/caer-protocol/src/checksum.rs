//! The DAoC packet checksum.
//!
//! Faithful port of `DOL.GS.PacketHandler.PacketProcessor.CalculateChecksum` (oracle:
//! `DOLSharp/GameServer/packets/Server/PacketProcessor.cs`):
//!
//! ```text
//! byte val1 = 0x7E, val2 = 0x7E;
//! for (i = dataOffset; i < dataOffset + dataSize; i++) {
//!     val1 += pak[i];   // byte arithmetic — wraps at 256
//!     val2 += val1;     // byte arithmetic — wraps at 256
//! }
//! return (ushort)(val2 - ((val1 + val2) << 8));   // the (val1+val2)<<8 term is 32-bit
//! ```
//!
//! Two subtleties the port must preserve exactly, or every client→server packet is rejected:
//! 1. `val1`/`val2` are **bytes**: they wrap mod 256 on every add (`wrapping_add`).
//! 2. The final expression is computed in **wider-than-16-bit** arithmetic and then truncated
//!    to `u16`: `(val1 + val2) << 8` must not pre-truncate. We compute in `i32` and cast.
//!
//! The checksum is appended as the last 2 bytes of a client→server packet (big-endian) and
//! covers the packet from its start up to (but not including) those 2 bytes.

/// Compute the 16-bit checksum over `data` (the exact byte range the checksum protects —
/// caller excludes the trailing 2 checksum bytes).
#[must_use]
pub fn calculate(data: &[u8]) -> u16 {
    let mut val1: u8 = 0x7E;
    let mut val2: u8 = 0x7E;
    for &b in data {
        val1 = val1.wrapping_add(b);
        val2 = val2.wrapping_add(val1);
    }
    // Widen before the shift so the high byte of the subtrahend is not lost, mirroring C#'s
    // int-promotion of `(val1 + val2) << 8`, then truncate to u16.
    let wide = i32::from(val2) - ((i32::from(val1) + i32::from(val2)) << 8);
    (wide as u32 & 0xFFFF) as u16
}

/// Append the big-endian checksum of `packet` to `packet` (the DAoC client→server trailer).
pub fn append(packet: &mut Vec<u8>) {
    let sum = calculate(packet);
    packet.push((sum >> 8) as u8);
    packet.push((sum & 0xFF) as u8);
}

/// Verify a client→server packet whose final 2 bytes are its big-endian checksum trailer.
#[must_use]
pub fn verify_trailer(packet: &[u8]) -> bool {
    if packet.len() < 2 {
        return false;
    }
    let (body, trailer) = packet.split_at(packet.len() - 2);
    let carried = (u16::from(trailer[0]) << 8) | u16::from(trailer[1]);
    calculate(body) == carried
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-computed vector locking the port to the oracle algorithm.
    /// data = [0x01]: val1 = 0x7E+1 = 0x7F; val2 = 0x7E+0x7F = 0xFD.
    /// result = 0xFD - ((0x7F+0xFD) << 8) = 253 - (380*256) = -97027 ≡ 0x84FD (mod 2^16).
    #[test]
    fn known_vector_single_byte() {
        assert_eq!(calculate(&[0x01]), 0x84FD);
    }

    /// Empty range: no bytes consumed, val1 = val2 = 0x7E.
    /// result = 0x7E - ((0x7E+0x7E) << 8) = 126 - (252*256) = -64386 ≡ 1150 = 0x047E (mod 2^16).
    #[test]
    fn known_vector_empty() {
        assert_eq!(calculate(&[]), 0x047E);
    }

    #[test]
    fn append_then_verify_roundtrips() {
        let mut pkt = vec![0x10, 0x20, 0x30, 0xAB, 0xCD];
        append(&mut pkt);
        assert!(verify_trailer(&pkt));
    }

    #[test]
    fn single_bit_flip_is_caught() {
        let mut pkt = vec![0x10, 0x20, 0x30, 0xAB, 0xCD];
        append(&mut pkt);
        pkt[2] ^= 0x01; // corrupt one body byte
        assert!(!verify_trailer(&pkt));
    }

    #[test]
    fn byte_wrap_is_respected() {
        // Inputs that overflow the running bytes many times must still land deterministically;
        // if val1/val2 were wider than u8 this would diverge from the oracle.
        let data = [0xFFu8; 300];
        let a = calculate(&data);
        let b = calculate(&data);
        assert_eq!(a, b);
        // 300 * 0xFF folded through byte-wrapping accumulators — recomputed reference.
        let mut v1: u8 = 0x7E;
        let mut v2: u8 = 0x7E;
        for _ in 0..300 {
            v1 = v1.wrapping_add(0xFF);
            v2 = v2.wrapping_add(v1);
        }
        let expect = (i32::from(v2) - ((i32::from(v1) + i32::from(v2)) << 8)) as u32 & 0xFFFF;
        assert_eq!(a, expect as u16);
    }
}
