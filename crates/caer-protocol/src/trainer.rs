//! TrainerWindow (0x7B) + train C2S. PacketLib1105 is the 1.127 S2C encoder.
//!
//! Type 0 = trainable specs, type 1 = realm abilities. Types 3/4/5 are decoded as header +
//! opaque remainder (skill-desc / name-index / RA-detail) — do not invent those layouts.
//! Local train intent never awards spec levels.

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

pub const PROVENANCE: &str = "TrainerWindow 0x7B PacketLib1105";

pub const CODE_SPEC: u8 = 0;
pub const CODE_REALM_ABILITY: u8 = 1;
pub const CODE_SKILL_DESC: u8 = 3;
pub const CODE_NAME_INDEX: u8 = 4;
pub const CODE_RA_DETAIL: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainerLine {
    pub index: u8,
    pub level: u8,
    pub cost_or_next: u8,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrainerWindow {
    pub count: u8,
    pub points: u8,
    pub code: u8,
    pub unk: u8,
    /// Populated for type 0 / 1 only.
    pub lines: Vec<TrainerLine>,
    /// Unparsed tail for types 3/4/5.
    pub rest: Vec<u8>,
}

impl TrainerWindow {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }

    #[must_use]
    pub fn is_spec_list(&self) -> bool {
        self.code == CODE_SPEC
    }
}

pub fn decode(payload: &[u8]) -> Result<TrainerWindow> {
    let mut r = PacketReader::new(payload);
    let count = r.u8()?;
    let points = r.u8()?;
    let code = r.u8()?;
    let unk = r.u8()?;
    let mut lines = Vec::new();
    let mut rest = Vec::new();
    match code {
        CODE_SPEC | CODE_REALM_ABILITY => {
            for _ in 0..count {
                let index = r.u8()?;
                let level = r.u8()?;
                let cost_or_next = r.u8()?;
                let name = r.pascal_string()?;
                lines.push(TrainerLine {
                    index,
                    level,
                    cost_or_next,
                    name,
                });
            }
        }
        _ => {
            rest.extend_from_slice(&payload[r.position()..]);
        }
    }
    Ok(TrainerWindow {
        count,
        points,
        code,
        unk,
        lines,
        rest,
    })
}

#[must_use]
pub fn encode_spec_window(points: u8, lines: &[TrainerLine]) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u8(lines.len() as u8).u8(points).u8(CODE_SPEC).u8(0);
    for line in lines {
        w.u8(line.index)
            .u8(line.level)
            .u8(line.cost_or_next)
            .pascal_string(&line.name);
    }
    w.into_bytes()
}

/// TrainWindowHandler 0x7B — oracle reads no body.
#[must_use]
pub fn encode_train_window() -> Vec<u8> {
    Vec::new()
}

/// TrainRequest 0x7C. OPEN_ORACLE `PlayerTrainRequestHandler`:
/// `u32 X, u32 Y, u8 idLine, u8 unk, u8 row, u8 skillIndex`.
#[must_use]
pub fn encode_train_request(
    player_x: u32,
    player_y: u32,
    id_line: u8,
    unk: u8,
    row: u8,
    skill_index: u8,
) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u32(player_x)
        .u32(player_y)
        .u8(id_line)
        .u8(unk)
        .u8(row)
        .u8(skill_index);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_window_roundtrip() {
        let lines = [TrainerLine {
            index: 0,
            level: 5,
            cost_or_next: 6,
            name: "Slash".into(),
        }];
        let body = encode_spec_window(12, &lines);
        let w = decode(&body).expect("decode");
        assert!(w.is_spec_list());
        assert_eq!(w.points, 12);
        assert_eq!(w.lines[0].name, "Slash");
        assert!(w.rest.is_empty());
    }

    #[test]
    fn train_request_wire() {
        let body = encode_train_request(10, 20, 0, 0, 2, 3);
        assert_eq!(body.len(), 12);
        let mut r = PacketReader::new(&body);
        assert_eq!(r.u32().unwrap(), 10);
        assert_eq!(r.u32().unwrap(), 20);
        assert_eq!(r.u8().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 0);
        assert_eq!(r.u8().unwrap(), 2);
        assert_eq!(r.u8().unwrap(), 3);
    }

    #[test]
    fn train_window_c2s_is_empty() {
        assert!(encode_train_window().is_empty());
    }

    #[test]
    fn type3_keeps_opaque_rest() {
        let body = vec![1, 8, CODE_SKILL_DESC, 0, 0xAA, 0xBB];
        let w = decode(&body).expect("decode");
        assert_eq!(w.code, CODE_SKILL_DESC);
        assert!(w.lines.is_empty());
        assert_eq!(w.rest, [0xAA, 0xBB]);
    }
}
