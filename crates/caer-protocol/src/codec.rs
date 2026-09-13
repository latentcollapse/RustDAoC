//! Reader/writer primitives — the Rust mirror of DOLSharp `PacketIn`/`PacketOut`.
//!
//! DAoC is big-endian by default. The oracle's `ReadShort` takes the first byte as the high
//! byte; `ReadShortLowEndian` swaps. Strings are "pascal" style: a length prefix (1, 2, or 4
//! bytes depending on the variant) followed by raw bytes, sometimes NUL-padded to a fixed
//! width. Every reader is fallible and bounds-checked — malformed input returns an error, it
//! never panics or reads out of bounds.

use crate::error::{ProtocolError, Result};

/// Cursor-based reader over a borrowed packet body.
#[derive(Debug, Clone)]
pub struct PacketReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> PacketReader<'a> {
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[must_use]
    pub fn position(&self) -> usize {
        self.pos
    }

    #[must_use]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(ProtocolError::UnexpectedEof {
                offset: self.pos,
                needed: (self.pos + n) - self.buf.len(),
            });
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// Big-endian u16 (DAoC default `ReadShort`).
    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok((u16::from(b[0]) << 8) | u16::from(b[1]))
    }

    /// Little-endian u16 (`ReadShortLowEndian`) — used only where the oracle marks it.
    pub fn u16_le(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok((u16::from(b[1]) << 8) | u16::from(b[0]))
    }

    /// Big-endian u32 (`ReadInt`).
    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok((u32::from(b[0]) << 24)
            | (u32::from(b[1]) << 16)
            | (u32::from(b[2]) << 8)
            | u32::from(b[3]))
    }

    /// Little-endian u32 (`ReadIntLowEndian`) — used only where the oracle marks it.
    pub fn u32_le(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok((u32::from(b[3]) << 24)
            | (u32::from(b[2]) << 16)
            | (u32::from(b[1]) << 8)
            | u32::from(b[0]))
    }

    /// Little-endian u64 (`ReadLongLowEndian`) — PacketLib190 CharacterPointsUpdate XP fields.
    pub fn u64_le(&mut self) -> Result<u64> {
        let lo = u64::from(self.u32_le()?);
        let hi = u64::from(self.u32_le()?);
        Ok(lo | (hi << 32))
    }

    /// Little-endian f32 (`ReadFloatLowEndian`) — 1.124+ coordinates.
    pub fn f32_le(&mut self) -> Result<f32> {
        Ok(f32::from_bits(self.u32_le()?))
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }

    /// Look at the next `n` bytes without advancing. Errors if fewer than `n` remain.
    pub fn peek(&self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            return Err(ProtocolError::UnexpectedEof {
                offset: self.pos,
                needed: (self.pos + n) - self.buf.len(),
            });
        }
        Ok(&self.buf[self.pos..self.pos + n])
    }

    /// A fixed-width string field, NUL-trimmed, interpreted as Latin-1 (DAoC's on-wire
    /// encoding for names/text). Latin-1 maps 1:1 to the first 256 code points, so this is
    /// lossless and infallible.
    pub fn fixed_string(&mut self, width: usize) -> Result<String> {
        let raw = self.take(width)?;
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        Ok(raw[..end].iter().map(|&b| b as char).collect())
    }

    /// Pascal string with a 1-byte length prefix (`ReadPascalString`).
    pub fn pascal_string(&mut self) -> Result<String> {
        let len = self.u8()? as usize;
        let raw = self
            .take(len)
            .map_err(|_| ProtocolError::BadStringLength { len })?;
        Ok(raw.iter().map(|&b| b as char).collect())
    }

    /// Pascal string with a 4-byte little-endian length prefix, NUL-terminated — the mirror of
    /// the oracle's `WritePascalStringIntLE`: `[len:4 LE incl. NUL][bytes][0]`, or a bare zero
    /// u32 for the empty string. Used by 1.12x-era packets (LoginRequest, CharacterOverview).
    pub fn pascal_string_int_le(&mut self) -> Result<String> {
        let len = self.u32_le()? as usize;
        if len == 0 {
            return Ok(String::new());
        }
        let raw = self
            .take(len)
            .map_err(|_| ProtocolError::BadStringLength { len })?;
        // Trim the trailing NUL (and tolerate its absence rather than reject the packet).
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        Ok(raw[..end].iter().map(|&b| b as char).collect())
    }
}

/// Growable writer that produces packet bodies. Header + checksum are applied by [`framing`],
/// not here — this only lays down field bytes.
#[derive(Debug, Default, Clone)]
pub struct PacketWriter {
    buf: Vec<u8>,
}

impl PacketWriter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(64),
        }
    }

    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            buf: Vec::with_capacity(cap),
        }
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    /// Big-endian u16 (DAoC default `WriteShort`).
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.push((v >> 8) as u8);
        self.buf.push((v & 0xFF) as u8);
        self
    }

    pub fn u16_le(&mut self, v: u16) -> &mut Self {
        self.buf.push((v & 0xFF) as u8);
        self.buf.push((v >> 8) as u8);
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf
            .extend_from_slice(&[(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8]);
        self
    }

    /// Little-endian u32 — the client→server LoginRequest uses LE length prefixes.
    pub fn u32_le(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Little-endian u64 (`WriteLongLowEndian`).
    pub fn u64_le(&mut self, v: u64) -> &mut Self {
        self.u32_le(v as u32).u32_le((v >> 32) as u32)
    }

    /// Little-endian f32 (`WriteFloatLowEndian`) — 1.124+ coordinates.
    pub fn f32_le(&mut self, v: f32) -> &mut Self {
        self.u32_le(v.to_bits())
    }

    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    /// Fixed-width Latin-1 string, NUL-padded/truncated to `width`.
    pub fn fixed_string(&mut self, s: &str, width: usize) -> &mut Self {
        let mut written = 0;
        for ch in s.chars().take(width) {
            self.buf.push(ch as u8); // Latin-1 truncation; callers pass ASCII/Latin-1 names
            written += 1;
        }
        for _ in written..width {
            self.buf.push(0);
        }
        self
    }

    /// Pascal string with a 1-byte length prefix.
    pub fn pascal_string(&mut self, s: &str) -> &mut Self {
        let bytes: Vec<u8> = s.chars().map(|c| c as u8).collect();
        let len = bytes.len().min(255);
        self.buf.push(len as u8);
        self.buf.extend_from_slice(&bytes[..len]);
        self
    }

    /// Mirror of `WritePascalStringIntLE`: `[len+1:4 LE][bytes][NUL]`, or a bare zero u32 when empty.
    /// `maxlen` caps the character bytes (oracle merchant names use `0x30`).
    pub fn pascal_string_int_le(&mut self, s: &str, maxlen: usize) -> &mut Self {
        if s.is_empty() {
            return self.u32_le(0);
        }
        let bytes: Vec<u8> = s.chars().map(|c| c as u8).collect();
        let len = bytes.len().min(maxlen.saturating_sub(1));
        self.u32_le((len + 1) as u32);
        self.buf.extend_from_slice(&bytes[..len]);
        self.buf.push(0);
        self
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u16_is_big_endian_by_default() {
        let mut w = PacketWriter::new();
        w.u16(0x1234);
        assert_eq!(w.as_slice(), &[0x12, 0x34]);
        let mut r = PacketReader::new(w.as_slice());
        assert_eq!(r.u16().unwrap(), 0x1234);
    }

    #[test]
    fn u16_le_swaps() {
        let mut w = PacketWriter::new();
        w.u16_le(0x1234);
        assert_eq!(w.as_slice(), &[0x34, 0x12]);
        let mut r = PacketReader::new(w.as_slice());
        assert_eq!(r.u16_le().unwrap(), 0x1234);
    }

    #[test]
    fn pascal_string_roundtrips() {
        let mut w = PacketWriter::new();
        w.pascal_string("Cabalist");
        let mut r = PacketReader::new(w.as_slice());
        assert_eq!(r.pascal_string().unwrap(), "Cabalist");
    }

    #[test]
    fn fixed_string_pads_and_trims() {
        let mut w = PacketWriter::new();
        w.fixed_string("Uthgard", 12);
        assert_eq!(w.len(), 12);
        let mut r = PacketReader::new(w.as_slice());
        assert_eq!(r.fixed_string(12).unwrap(), "Uthgard");
    }

    #[test]
    fn reader_reports_eof_not_panic() {
        let mut r = PacketReader::new(&[0x01]);
        assert_eq!(r.u8().unwrap(), 0x01);
        assert!(matches!(r.u32(), Err(ProtocolError::UnexpectedEof { .. })));
    }
}
