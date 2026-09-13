//! FindGroupUpdate (0x86) — LFG window, not GroupWindow membership.
//!
//! OPEN_ORACLE `PacketLib168.SendFindGroupWindowUpdate`: empty list is u16 0; otherwise
//! count u8 + per-player rows. This decode only surfaces count / empty — names are not
//! group roster authority.

use crate::codec::PacketReader;
use crate::error::Result;

pub const PROVENANCE: &str = "FindGroupUpdate 0x86";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindGroupUpdate {
    pub count: u8,
    pub empty_list: bool,
}

impl FindGroupUpdate {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }
}

pub fn decode(payload: &[u8]) -> Result<FindGroupUpdate> {
    if payload.len() == 2 && payload[0] == 0 && payload[1] == 0 {
        return Ok(FindGroupUpdate {
            count: 0,
            empty_list: true,
        });
    }
    let mut r = PacketReader::new(payload);
    let count = r.u8()?;
    Ok(FindGroupUpdate {
        count,
        empty_list: count == 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_is_u16_zero_not_group_window() {
        let d = decode(&[0, 0]).expect("decode");
        assert!(d.empty_list);
        assert_eq!(d.count, 0);
        assert_eq!(d.provenance(), PROVENANCE);
    }

    #[test]
    fn count_byte_is_lfg_not_roster() {
        let d = decode(&[2, 0, 50]).expect("decode");
        assert_eq!(d.count, 2);
        assert!(!d.empty_list);
    }
}
