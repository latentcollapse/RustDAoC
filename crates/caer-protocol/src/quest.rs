//! `QuestEntry` (0x83) — quest-log slot update.
//!
//! ## Provenance
//!
//! OPEN_ORACLE SoloDAoC `PacketLib1124.SendQuestPacket` (classic `AbstractQuest`, inherited by
//! 1125–1127). Empty / null slot clears with zero name+desc lengths.
//!
//! ## Wire form (classic AbstractQuest, 1.124+)
//!
//! ```text
//!   u8       index
//!   u8       nameLen
//!   u16 LE   descLen
//!   u8       zone (0)
//!   u8       pad  (0)
//!   bytes    name   (nameLen)
//!   bytes    desc   (descLen)
//! ```
//!
//! Clear / empty step (`quest == null` or step cleared):
//!
//! ```text
//!   u8 index + five zero bytes
//! ```
//!
//! Name on the wire is often `"Important Delivery (Level 1)"`; description is
//! `"[Step #N]: …"`.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

/// Provenance tag for UI / RT asserts (REQ-021 style).
pub const PROVENANCE: &str = "QuestEntry 0x83";

/// A decoded QuestEntry (0x83) classic AbstractQuest slot.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuestEntry {
    pub index: u8,
    pub name: String,
    pub description: String,
}

impl QuestEntry {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }

    /// Empty name and description — clear / empty step.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.name.is_empty() && self.description.is_empty()
    }
}

/// Decode a QuestEntry payload (PacketLib1124 classic form).
pub fn decode(payload: &[u8]) -> Result<QuestEntry> {
    let mut r = PacketReader::new(payload);
    let index = r.u8()?;
    let name_len = r.u8()?;
    // Clear / empty step: nameLen == 0 (168: three pad zeros; 1124: five zeros after index).
    if name_len == 0 {
        while r.remaining() > 0 {
            let _ = r.u8()?;
        }
        return Ok(QuestEntry {
            index,
            name: String::new(),
            description: String::new(),
        });
    }
    let desc_len = r.u16_le()? as usize;
    let _zone = r.u8()?;
    let _pad = r.u8()?;
    let name_bytes = r.bytes(name_len as usize)?;
    let desc_bytes = r.bytes(desc_len)?;
    Ok(QuestEntry {
        index,
        name: String::from_utf8_lossy(name_bytes).into_owned(),
        description: String::from_utf8_lossy(desc_bytes).into_owned(),
    })
}

/// Encode matching `PacketLib1124.SendQuestPacket` classic non-null form (tests / fixtures).
#[must_use]
pub fn encode(e: &QuestEntry) -> Vec<u8> {
    if e.is_clear() {
        return encode_clear(e.index);
    }
    let name = e.name.as_bytes();
    let desc = e.description.as_bytes();
    let name_len = name.len().min(255) as u8;
    let desc_len = desc.len().min(u16::MAX as usize) as u16;
    let mut w = PacketWriter::with_capacity(6 + name_len as usize + desc_len as usize);
    w.u8(e.index)
        .u8(name_len)
        .u16_le(desc_len)
        .u8(0)
        .u8(0)
        .bytes(&name[..name_len as usize])
        .bytes(&desc[..desc_len as usize]);
    w.into_bytes()
}

/// Encode a clear / empty-step QuestEntry (five zero bytes after index).
#[must_use]
pub fn encode_clear(index: u8) -> Vec<u8> {
    let mut w = PacketWriter::with_capacity(6);
    w.u8(index).u8(0).u8(0).u8(0).u8(0).u8(0);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_nonempty_classic_quest_entry() {
        let e = QuestEntry {
            index: 1,
            name: "Important Delivery (Level 1)".into(),
            description: "[Step #1]: Talk to Master Frederick.".into(),
        };
        let body = encode(&e);
        // nameLen=28, descLen LE
        assert_eq!(body[0], 1);
        assert_eq!(body[1], 28);
        assert_eq!(
            u16::from_le_bytes([body[2], body[3]]),
            e.description.len() as u16
        );
        assert_eq!(&body[4..6], &[0, 0]);
        let d = decode(&body).expect("non-empty QuestEntry must decode");
        assert_eq!(d, e);
        assert!(!d.is_clear());
        assert_eq!(d.provenance(), PROVENANCE);
        assert!(d.name.contains("Important Delivery"));
    }

    #[test]
    fn decodes_clear_empty_step() {
        let body = encode_clear(3);
        assert_eq!(body, [3, 0, 0, 0, 0, 0]);
        let d = decode(&body).expect("clear QuestEntry must decode");
        assert_eq!(d.index, 3);
        assert!(d.is_clear());
        assert_eq!(d.name, "");
        assert_eq!(d.description, "");
        assert_eq!(d.provenance(), PROVENANCE);
    }

    #[test]
    fn round_trip_encode_decode() {
        let e = QuestEntry {
            index: 2,
            name: "Nuisances".into(),
            description: "[Step #2]: Find the source.".into(),
        };
        assert_eq!(decode(&encode(&e)).unwrap(), e);
        assert!(decode(&encode_clear(1)).unwrap().is_clear());
    }
}
