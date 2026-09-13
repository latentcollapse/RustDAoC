//! Connection crypto: the RSA→RC4 handshake and a pure-Rust RC4.
//!
//! From the oracle (`CryptLib168.cs`), the original does its crypto in a native lib exposing:
//! `EncodeMythicRSAPacket` / `DecodeMythicRSAPacket` (the initial key exchange) and
//! `EncodeMythicRC4Packet` / `DecodeMythicRC4Packet` (the per-packet stream cipher, keyed by
//! a 256-byte SBox, with a separate UDP flag). The handshake:
//!
//! 1. Client sends `CryptKeyRequest` (may be sent unencrypted).
//! 2. Server replies with its RSA-wrapped key material.
//! 3. Both sides derive the RC4 SBox; subsequent packets are RC4-streamed.
//!
//! **What's implemented here:** RC4 itself is a standard, unpatented stream cipher — the
//! `Rc4` below is a faithful, tested implementation and is the whole "Mythic RC4" once keyed
//! with the correct SBox seed. **What's deliberately stubbed:** the RSA key-exchange framing
//! and the exact SBox derivation are Mythic-specific and MUST be recovered from a golden
//! handshake capture (clean-room, from the wire — never from leaked source). The
//! [`Handshake`] trait draws that boundary so the session layer can compile and be tested
//! against a fake now, and swap in the real derivation when the capture lands.
//!
//! Many freeshards also run with encryption effectively disabled / trivial-keyed for the game
//! stream (only the login/auth packet is protected). [`NullCrypto`] models that path and is
//! enough to bring up a headless client against such a server for P1.
//!
//! **P0 FINDING (golden trace `rustdaoc_login_20260714`, 2026-07-14):** on the captured
//! server the game stream is **plaintext**. The handshake still occurs — client sends
//! `CryptKeyRequest` (`0xF4`) with a 7-byte build header and then a 256-byte RSA block, and the
//! server replies with `CryptKey` (`0x22`) carrying the version string "1.127" — but every
//! subsequent packet is unencrypted: 2057 packets decoded with zero errors, and the account
//! name is readable in cleartext in the LoginRequest. So a P1 client must *send* the handshake
//! shape to satisfy the server, but [`NullCrypto`] is correct for the stream itself. The RSA
//! key derivation is therefore NOT on the P0 critical path for this server.

use crate::codec::PacketWriter;

/// Encode the 7-byte `CryptKeyRequest` payload (packet code `0xF4`), from the golden trace:
/// `36 01 01 1b 65 7f 05`. Per the oracle `CryptKeyRequestHandler` (version ≥ 1115 branch),
/// a payload of ≤7 bytes is the version/client-type announce that triggers the server's
/// version+cryptkey reply. Fields: `[clientType:1][version:3][minorRev:1 ('e')][build:2]`.
/// These identify client build 1.127e; reproduced verbatim from the capture.
#[must_use]
pub fn encode_crypt_key_request() -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u8(0x36) // client type + addons
        .bytes(&[0x01, 0x01, 0x1b]) // version numbers (server Skip(3))
        .u8(0x65) // minor revision letter 'e'
        .bytes(&[0x7f, 0x05]); // build (server Skip(2))
    w.into_bytes()
}

/// Standard RC4 keystream cipher. `apply` is symmetric (encrypt == decrypt).
#[derive(Clone)]
pub struct Rc4 {
    s: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    /// Key-schedule an RC4 state from a raw key.
    #[must_use]
    pub fn new(key: &[u8]) -> Self {
        assert!(!key.is_empty(), "RC4 key must be non-empty");
        let mut s = [0u8; 256];
        for (i, b) in s.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut j: u8 = 0;
        for i in 0..256 {
            j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
            s.swap(i, j as usize);
        }
        Self { s, i: 0, j: 0 }
    }

    /// Construct directly from a pre-derived 256-byte SBox (the "Mythic SBox" path, once its
    /// derivation is recovered from a capture).
    #[must_use]
    pub fn from_sbox(sbox: [u8; 256]) -> Self {
        Self {
            s: sbox,
            i: 0,
            j: 0,
        }
    }

    /// XOR `data` in place with the next keystream bytes (advances state).
    pub fn apply(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.s[self.i as usize]);
            self.s.swap(self.i as usize, self.j as usize);
            let k =
                self.s[(self.s[self.i as usize].wrapping_add(self.s[self.j as usize])) as usize];
            *byte ^= k;
        }
    }
}

/// The connection-crypto boundary the session layer talks to. Lets the login/game stream be
/// null (freeshard-friendly), real RC4, or a future full Mythic handshake without the session
/// state machine caring which.
pub trait Handshake {
    /// Transform an outbound packet body just before framing (encrypt).
    fn seal(&mut self, packet: &mut Vec<u8>);
    /// Transform an inbound packet body just after de-framing (decrypt).
    fn open(&mut self, packet: &mut Vec<u8>);
}

/// No-op crypto: the game stream is plaintext. Enough for P1 against freeshards that don't
/// encrypt the game channel.
#[derive(Default)]
pub struct NullCrypto;

impl Handshake for NullCrypto {
    fn seal(&mut self, _packet: &mut Vec<u8>) {}
    fn open(&mut self, _packet: &mut Vec<u8>) {}
}

/// RC4-streamed crypto once an SBox/key is established. Two independent RC4 states (send/recv)
/// because the stream cipher is directional.
pub struct Rc4Crypto {
    send: Rc4,
    recv: Rc4,
}

impl Rc4Crypto {
    #[must_use]
    pub fn new(send_key: &[u8], recv_key: &[u8]) -> Self {
        Self {
            send: Rc4::new(send_key),
            recv: Rc4::new(recv_key),
        }
    }
}

impl Handshake for Rc4Crypto {
    fn seal(&mut self, packet: &mut Vec<u8>) {
        self.send.apply(packet);
    }
    fn open(&mut self, packet: &mut Vec<u8>) {
        self.recv.apply(packet);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6229 test vector: key "Key", the first bytes of the keystream applied to zeros.
    /// Locks the RC4 implementation to the standard.
    #[test]
    fn rc4_matches_known_vector() {
        // Key = 0x4B6579 ("Key"); keystream starts EB 9F 77 81 B7 34 CA 72 A7 19...
        let mut rc4 = Rc4::new(b"Key");
        let mut data = [0u8; 10];
        rc4.apply(&mut data);
        assert_eq!(
            &data,
            &[0xEB, 0x9F, 0x77, 0x81, 0xB7, 0x34, 0xCA, 0x72, 0xA7, 0x19]
        );
    }

    #[test]
    fn rc4_is_symmetric() {
        let msg = b"enter Camelot".to_vec();
        let mut enc = msg.clone();
        Rc4::new(b"sbox-seed").apply(&mut enc);
        assert_ne!(enc, msg);
        let mut dec = enc.clone();
        Rc4::new(b"sbox-seed").apply(&mut dec);
        assert_eq!(dec, msg);
    }

    #[test]
    fn null_crypto_is_identity() {
        let mut c = NullCrypto;
        let mut p = vec![1, 2, 3, 4];
        c.seal(&mut p);
        c.open(&mut p);
        assert_eq!(p, vec![1, 2, 3, 4]);
    }
}
