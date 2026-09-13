//! MarketExplorerWindow 0x1F header. Not merchant 0x17 catalogue.

use caer_protocol::market::MarketExplorer;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MarketExplorerCorr {
    explorer: Option<MarketExplorer>,
    generation: u64,
}

impl MarketExplorerCorr {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn current(&self) -> Option<MarketExplorer> {
        self.explorer
    }

    pub fn apply(&mut self, explorer: MarketExplorer, packet_generation: u64) -> bool {
        if packet_generation < self.generation {
            return false;
        }
        self.explorer = Some(explorer);
        self.generation = packet_generation;
        true
    }
}
