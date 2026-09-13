//! Player purse: MoneyUpdate 0xFA is authority. Local spend/buy/sell never mutates it.

use caer_protocol::money::MoneyUpdate;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoneyIntent {
    Spend { gold: u16 },
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MoneyCorr {
    purse: Option<MoneyUpdate>,
    generation: u64,
    pending: Option<(MoneyIntent, u64)>,
}

impl MoneyCorr {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn purse(&self) -> Option<&MoneyUpdate> {
        self.purse.as_ref()
    }

    pub fn intent(&mut self, intent: MoneyIntent) {
        self.pending = Some((intent, self.generation));
    }

    pub fn apply_update(&mut self, update: MoneyUpdate, packet_generation: u64) -> bool {
        if packet_generation < self.generation {
            return false;
        }
        self.purse = Some(update);
        self.generation = packet_generation;
        self.pending = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_spend_does_not_change_purse() {
        let mut m = MoneyCorr::default();
        assert!(m.apply_update(
            MoneyUpdate {
                gold: 20,
                ..MoneyUpdate::default()
            },
            1
        ));
        m.intent(MoneyIntent::Spend { gold: 5 });
        m.intent(MoneyIntent::Buy);
        assert_eq!(m.purse().unwrap().gold, 20);
        assert_eq!(m.generation(), 1);
    }

    #[test]
    fn stale_money_update_does_not_apply() {
        let mut m = MoneyCorr::default();
        assert!(m.apply_update(
            MoneyUpdate {
                gold: 50,
                ..MoneyUpdate::default()
            },
            4
        ));
        assert!(!m.apply_update(
            MoneyUpdate {
                gold: 1,
                ..MoneyUpdate::default()
            },
            3
        ));
        assert_eq!(m.purse().unwrap().gold, 50);
    }
}
