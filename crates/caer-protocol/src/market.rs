//! MarketExplorerWindow (0x1F) — housing consignment browser, not merchant 0x17.
//!
//! PacketLib1125 (1.127): count, page, maxpage, pad. Empty/close from PacketLib168 writes
//! count=255 + three zeros. Item rows are NOT decoded here (layout ≠ InventoryUpdate / 0x17).

use crate::codec::{PacketReader, PacketWriter};
use crate::error::Result;

pub const PROVENANCE: &str = "MarketExplorerWindow 0x1F PacketLib1125 header";
pub const EMPTY_COUNT: u8 = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketExplorer {
    pub count: u8,
    pub page: u8,
    pub max_page: u8,
}

impl MarketExplorer {
    #[must_use]
    pub fn provenance(&self) -> &'static str {
        PROVENANCE
    }

    #[must_use]
    pub fn is_empty_close(&self) -> bool {
        self.count == EMPTY_COUNT
    }
}

pub fn decode(payload: &[u8]) -> Result<MarketExplorer> {
    let mut r = PacketReader::new(payload);
    Ok(MarketExplorer {
        count: r.u8()?,
        page: r.u8()?,
        max_page: r.u8()?,
    })
}

#[must_use]
pub fn encode_header(m: &MarketExplorer) -> Vec<u8> {
    let mut w = PacketWriter::new();
    w.u8(m.count).u8(m.page).u8(m.max_page).u8(0);
    w.into_bytes()
}

#[must_use]
pub fn encode_empty_close() -> Vec<u8> {
    encode_header(&MarketExplorer {
        count: EMPTY_COUNT,
        page: 0,
        max_page: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_close_is_255() {
        let body = encode_empty_close();
        let d = decode(&body).expect("decode");
        assert!(d.is_empty_close());
        assert_eq!(d.provenance(), PROVENANCE);
    }

    #[test]
    fn page_header_is_not_merchant_catalogue() {
        let body = encode_header(&MarketExplorer {
            count: 3,
            page: 1,
            max_page: 4,
        });
        let d = decode(&body).expect("decode");
        assert_eq!(d.count, 3);
        assert_eq!(d.page, 1);
        assert!(!d.is_empty_close());
    }
}
