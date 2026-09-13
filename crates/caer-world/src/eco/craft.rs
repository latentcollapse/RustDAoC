//! Crafting correlation: CraftRequest 0xED is local intent.
//! Product presence is InventoryUpdate authority, never the MakeProduct encode.

use caer_protocol::inventory::InventoryUpdate;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CraftCorr {
    pending_item_id: Option<u16>,
    pending_at_inv_generation: u64,
    /// True only after an InventoryUpdate applied *after* a craft intent.
    complete: bool,
    last_craft_item_id: Option<u16>,
}

impl CraftCorr {
    #[must_use]
    pub fn pending_item_id(&self) -> Option<u16> {
        self.pending_item_id
    }

    #[must_use]
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// MakeProduct 0xED — does not invent an inventory row.
    pub fn intent_craft(&mut self, item_id: u16, inventory_generation: u64) {
        self.pending_item_id = Some(item_id);
        self.pending_at_inv_generation = inventory_generation;
        self.last_craft_item_id = Some(item_id);
        self.complete = false;
    }

    /// Server inventory after craft. Stale generation does not complete.
    pub fn observe_inventory(&mut self, _update: &InventoryUpdate, inventory_generation: u64) {
        if self.pending_item_id.is_none() {
            return;
        }
        if inventory_generation <= self.pending_at_inv_generation {
            return;
        }
        self.complete = true;
        self.pending_item_id = None;
    }

    pub fn reject_or_timeout(&mut self) {
        self.pending_item_id = None;
        self.complete = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::inventory::InventoryUpdate;
    use caer_protocol::invverb::encode_craft_request;

    fn empty_upd() -> InventoryUpdate {
        InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0,
            window_type: 0,
            items: vec![],
        }
    }

    #[test]
    fn craft_encode_is_not_completion() {
        let mut c = CraftCorr::default();
        let wire = encode_craft_request(0x42);
        assert_eq!(wire.len(), 2);
        c.intent_craft(0x42, 1);
        assert!(!c.complete());
        assert_eq!(c.pending_item_id(), Some(0x42));
        c.observe_inventory(&empty_upd(), 1);
        assert!(!c.complete(), "same inventory generation is stale");
        c.observe_inventory(&empty_upd(), 2);
        assert!(c.complete());
    }

    #[test]
    fn craft_timeout_leaves_complete_false() {
        let mut c = CraftCorr::default();
        c.intent_craft(9, 0);
        c.reject_or_timeout();
        assert!(!c.complete());
        assert!(c.pending_item_id().is_none());
    }
}
