//! Inventory correlation: InventoryUpdate 0x02 is authority.
//! Local move/stack/use/equip is intent only.

use std::collections::HashMap;

use caer_protocol::inventory::{InventoryUpdate, ItemData};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvIntent {
    Move {
        from_slot: u8,
        to_slot: u8,
        count: u16,
    },
    Stack {
        from_slot: u8,
        to_slot: u8,
        count: u16,
    },
    Use {
        slot: u8,
    },
    Equip {
        from_slot: u8,
        to_slot: u8,
    },
    Destroy {
        slot: u8,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InventoryCorr {
    slots: HashMap<u8, Option<ItemData>>,
    /// Monotonic authority generation. Stale packets (`packet_generation < generation`) are dropped.
    generation: u64,
    pending: Option<(InvIntent, u64)>,
}

impl InventoryCorr {
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn item(&self, slot: u8) -> Option<&ItemData> {
        self.slots.get(&slot)?.as_ref()
    }

    #[must_use]
    pub fn occupied(&self) -> usize {
        self.slots.values().filter(|s| s.is_some()).count()
    }

    #[must_use]
    pub fn pending(&self) -> Option<InvIntent> {
        self.pending.map(|(i, _)| i)
    }

    /// Record a local verb. Does **not** move, stack, consume, or equip.
    pub fn intent(&mut self, intent: InvIntent) {
        self.pending = Some((intent, self.generation));
    }

    /// Apply a server InventoryUpdate. Returns false when `packet_generation` is stale.
    pub fn apply_update(&mut self, update: &InventoryUpdate, packet_generation: u64) -> bool {
        if packet_generation < self.generation {
            return false;
        }
        for entry in &update.items {
            self.slots.insert(entry.slot, entry.item.clone());
        }
        self.generation = packet_generation;
        self.pending = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::inventory::{encode_1124, InventoryItem};

    fn sword(slot: u8) -> InventoryUpdate {
        InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0x10,
            window_type: 1,
            items: vec![InventoryItem {
                slot,
                item: Some(ItemData {
                    unique_id: 0,
                    level: 1,
                    value1: 0,
                    value2: 0,
                    hand_byte: 0,
                    object_type_byte: 0x83,
                    unk_1112: 0,
                    weight: 10,
                    condition_pct: 100,
                    durability_pct: 100,
                    quality: 90,
                    bonus: 0,
                    bonus_level: 0,
                    model: 3,
                    extension: 0,
                    color_or_emblem: 0,
                    flag: 0,
                    effect: 0,
                    name: "practice sword".into(),
                }),
            }],
        }
    }

    #[test]
    fn local_move_does_not_mutate_slots() {
        let mut inv = InventoryCorr::default();
        assert!(inv.apply_update(&sword(40), 1));
        inv.intent(InvIntent::Move {
            from_slot: 40,
            to_slot: 25,
            count: 1,
        });
        inv.intent(InvIntent::Equip {
            from_slot: 40,
            to_slot: 25,
        });
        inv.intent(InvIntent::Use { slot: 40 });
        inv.intent(InvIntent::Stack {
            from_slot: 40,
            to_slot: 41,
            count: 2,
        });
        assert!(inv.item(40).is_some());
        assert!(inv.item(25).is_none());
        assert_eq!(inv.occupied(), 1);
        assert_eq!(inv.generation(), 1);
        let _ = encode_1124(&sword(40));
    }

    #[test]
    fn stale_generation_does_not_apply() {
        let mut inv = InventoryCorr::default();
        assert!(inv.apply_update(&sword(40), 2));
        let before = inv.item(40).unwrap().name.clone();
        assert!(!inv.apply_update(&sword(10), 1), "stale gen must fail");
        assert_eq!(inv.item(40).unwrap().name, before);
        assert!(inv.item(10).is_none());
        assert_eq!(inv.generation(), 2);
    }
}
