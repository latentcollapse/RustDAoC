//! LoginRequest encoding — the client→server auth packet, from real captured bytes.
//!
//! P0 finding (golden trace `rustdaoc_login_20260714`): the LoginRequest payload (packet code
//! [`codes::client::LoginRequest`] = `0xA7`) is:
//!
//! ```text
//! [len:4 LE][account bytes][0x00]   len = account.len() + 1  (NUL included)
//! [len:4 LE][password bytes][0x00]  len = password.len() + 1
//! [trailer]                         client-type byte + length-prefixed client id (below)
//! ```
//!
//! In the capture: `09 00 00 00  "rustdaoc" 00   09 00 00 00  "rustdaoc" 00   2A  08 00 00 00
//! 30 72 4A 62 F1 8A 0C 85`. The trailer decodes as a `0x2A` client-type/version byte, then a
//! 4-byte-LE length (`8`) and an 8-byte machine-specific client id. Account/password are
//! **plaintext** — the game stream is not encrypted on this server (see [`crate::crypto`]).
//!
//! Strings here use a **4-byte little-endian** length prefix (NUL included) — distinct from the
//! server→client 1-byte pascal strings. Latin-1 on the wire.

use crate::codec::PacketWriter;

/// The client-type byte seen leading the LoginRequest trailer.
pub const LOGIN_CLIENT_TYPE: u8 = 0x2A;

/// Write a 4-byte-LE length-prefixed, NUL-terminated Latin-1 string (the LoginRequest string
/// form: the length COUNTS the NUL).
fn write_le_cstr(w: &mut PacketWriter, s: &str) {
    let bytes: Vec<u8> = s.chars().map(|c| c as u8).collect();
    w.u32_le((bytes.len() + 1) as u32);
    w.bytes(&bytes);
    w.u8(0);
}

/// Encode a LoginRequest **payload** (the bytes after the packet header) for `account`/
/// `password`. `client_id` is the machine-specific 8-byte trailer id captured from the real
/// client; `LOGIN_CLIENT_TYPE` + its 4-byte-LE length are emitted around it.
#[must_use]
pub fn encode_login_request(account: &str, password: &str, client_id: &[u8]) -> Vec<u8> {
    let mut w = PacketWriter::new();
    write_le_cstr(&mut w, account);
    write_le_cstr(&mut w, password);
    w.u8(LOGIN_CLIENT_TYPE);
    w.u32_le(client_id.len() as u32);
    w.bytes(client_id);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduce the exact LoginRequest payload from the golden trace, byte-for-byte. This is
    /// the P1 direction proven in miniature: our encoder emits the real client's bytes.
    #[test]
    fn reproduces_captured_login_payload() {
        // captured client id (the 8-byte trailer value)
        let client_id = [0x30, 0x72, 0x4A, 0x62, 0xF1, 0x8A, 0x0C, 0x85];
        let payload = encode_login_request("rustdaoc", "rustdaoc", &client_id);
        let expected =
            hex("090000007275737464616f6300090000007275737464616f63002a0800000030724a62f18a0c85");
        assert_eq!(
            payload, expected,
            "must match the captured LoginRequest byte-for-byte"
        );
    }

    #[test]
    fn length_prefix_counts_the_nul() {
        let p = encode_login_request("ab", "c", &[]);
        // "ab" -> 4-byte-LE len 3 (a,b,NUL), then a,b,00 ; "c" -> len 2, c,00 ; 0x2a ; len 0
        assert_eq!(&p[..4], &[0x03, 0x00, 0x00, 0x00]); // length is 4 bytes LE
        assert_eq!(p[4], b'a'); // string starts after the 4-byte length
        assert_eq!(p[5], b'b');
        assert_eq!(p[6], 0x00); // NUL
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
