//! Merchant catalogue: MerchantWindow 0x17 is authority.
//! Local buy/sell intent does not add or remove items.

use caer_protocol::merchant::MerchantWindow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MerchantIntent {
    Buy { page_slot: u8, count: u8 },
    Sell { bag_slot: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MerchantCorr {
    catalogue: Option<MerchantWindow>,
    generation: u64,
    pending: Option<(MerchantIntent, u64)>,
}

impl MerchantCorr {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn catalogue(&self) -> Option<&MerchantWindow> {
        self.catalogue.as_ref()
    }

    #[must_use]
    pub fn pending(&self) -> Option<MerchantIntent> {
        self.pending.map(|(i, _)| i)
    }

    pub fn intent(&mut self, intent: MerchantIntent) {
        self.pending = Some((intent, self.generation));
    }

    /// Replace the open page. Stale `packet_generation` leaves the catalogue unchanged.
    pub fn apply_window(&mut self, window: MerchantWindow, packet_generation: u64) -> bool {
        if packet_generation < self.generation {
            return false;
        }
        self.catalogue = Some(window);
        self.generation = packet_generation;
        self.pending = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::merchant::{window_type, MerchantOffer};

    fn page(name: &str) -> MerchantWindow {
        MerchantWindow {
            window_type: window_type::NORMAL,
            page: 0,
            items: vec![MerchantOffer {
                slot: 0,
                level: 1,
                value1: 0,
                spd_abs: 0,
                hand_byte: 0,
                object_type_byte: 0,
                usable: true,
                value2: 0,
                price: 12,
                model: 1,
                name: name.into(),
            }],
        }
    }

    #[test]
    fn local_buy_does_not_become_catalogue_or_inventory() {
        let mut m = MerchantCorr::default();
        assert!(m.apply_window(page("ticket"), 1));
        m.intent(MerchantIntent::Buy {
            page_slot: 0,
            count: 1,
        });
        m.intent(MerchantIntent::Sell { bag_slot: 40 });
        assert_eq!(m.catalogue().unwrap().items[0].name, "ticket");
        assert_eq!(m.catalogue().unwrap().items.len(), 1);
        assert_eq!(m.generation(), 1);
    }

    #[test]
    fn stale_catalogue_does_not_replace() {
        let mut m = MerchantCorr::default();
        assert!(m.apply_window(page("new"), 3));
        assert!(!m.apply_window(page("old"), 2));
        assert_eq!(m.catalogue().unwrap().items[0].name, "new");
        assert_eq!(m.generation(), 3);
    }
}
