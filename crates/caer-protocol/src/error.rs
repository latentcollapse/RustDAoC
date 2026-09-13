//! Protocol error types. Decoders never panic on malformed wire data — a short read, a bad
//! length, or a failed checksum is a `ProtocolError`, so a hostile or corrupt peer degrades
//! into a clean disconnect rather than a crash (a property the original C++ client famously
//! lacks).

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ProtocolError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("unexpected end of buffer: needed {needed} more byte(s) at offset {offset}")]
    UnexpectedEof { offset: usize, needed: usize },

    #[error("declared packet length {declared} exceeds the {max}-byte cap")]
    OversizedPacket { declared: usize, max: usize },

    #[error("checksum mismatch: computed 0x{computed:04X}, packet carried 0x{carried:04X}")]
    ChecksumMismatch { computed: u16, carried: u16 },

    #[error("pascal string length {len} runs past the end of the buffer")]
    BadStringLength { len: usize },

    #[error("string field was not valid: {0}")]
    BadString(&'static str),

    #[error("packet code 0x{0:02X} is not known to this build")]
    UnknownPacketCode(u8),

    #[error("received packet in the wrong session phase (have {have:?}, need {need})")]
    WrongPhase {
        have: session_phase_dbg::Phase,
        need: &'static str,
    },
}

// A tiny shim so the error enum can name a phase without a cycle back to `session`.
pub mod session_phase_dbg {
    pub type Phase = crate::session::SessionPhase;
}
