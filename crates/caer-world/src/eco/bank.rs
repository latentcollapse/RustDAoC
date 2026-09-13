//! Bank / vault / consignment: InventoryUpdate window types + ConsignmentMerchantMoney 0x1E.
//! ObjectGuildID 0xDE is guild attachment, not a roster invent.

use std::collections::HashMap;

use caer_protocol::inventory::{InventoryUpdate, ItemData};
use caer_protocol::money::ConsignmentMerchantMoney;
use caer_protocol::social::ObjectGuildId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BankIntent {
    Deposit { bag_slot: u8, vault_slot: u8 },
    Withdraw { vault_slot: u8, bag_slot: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BankCorr {
    vault: HashMap<u8, Option<ItemData>>,
    vault_generation: u64,
    consignment: Option<ConsignmentMerchantMoney>,
    consignment_generation: u64,
    object_guild: HashMap<u16, u16>,
    guild_generation: u64,
    pending: Option<(BankIntent, u64)>,
}

impl BankCorr {
    #[must_use]
    pub fn vault_item(&self, slot: u8) -> Option<&ItemData> {
        self.vault.get(&slot)?.as_ref()
    }

    #[must_use]
    pub fn consignment(&self) -> Option<&ConsignmentMerchantMoney> {
        self.consignment.as_ref()
    }

    #[must_use]
    pub fn guild_id(&self, object_id: u16) -> Option<u16> {
        self.object_guild.get(&object_id).copied()
    }

    #[must_use]
    pub fn vault_generation(&self) -> u64 {
        self.vault_generation
    }

    pub fn intent(&mut self, intent: BankIntent) {
        self.pending = Some((intent, self.vault_generation));
    }

    /// Vault/consignment InventoryUpdate pages only. Backpack pages are ignored here.
    pub fn apply_vault_update(&mut self, update: &InventoryUpdate, packet_generation: u64) -> bool {
        if !update.is_vault_or_consignment() {
            return false;
        }
        if packet_generation < self.vault_generation {
            return false;
        }
        for entry in &update.items {
            self.vault.insert(entry.slot, entry.item.clone());
        }
        self.vault_generation = packet_generation;
        self.pending = None;
        true
    }

    pub fn apply_consignment_money(
        &mut self,
        money: ConsignmentMerchantMoney,
        packet_generation: u64,
    ) -> bool {
        if packet_generation < self.consignment_generation {
            return false;
        }
        self.consignment = Some(money);
        self.consignment_generation = packet_generation;
        true
    }

    pub fn apply_object_guild(&mut self, g: ObjectGuildId, packet_generation: u64) -> bool {
        if packet_generation < self.guild_generation {
            return false;
        }
        if g.has_guild() {
            self.object_guild.insert(g.object_id, g.guild_id);
        } else {
            self.object_guild.remove(&g.object_id);
        }
        self.guild_generation = packet_generation;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::inventory::{window_type, InventoryItem, ItemData};
    use caer_protocol::money::{decode_consignment, encode_consignment};
    use caer_protocol::social::{decode_object_guild_id, encode_object_guild_id};

    fn vault_item(slot: u8, name: &str) -> InventoryUpdate {
        InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0,
            window_type: window_type::PLAYER_VAULT,
            items: vec![InventoryItem {
                slot,
                item: Some(ItemData {
                    unique_id: 0,
                    level: 1,
                    value1: 0,
                    value2: 0,
                    hand_byte: 0,
                    object_type_byte: 0,
                    unk_1112: 0,
                    weight: 1,
                    condition_pct: 100,
                    durability_pct: 100,
                    quality: 100,
                    bonus: 0,
                    bonus_level: 0,
                    model: 1,
                    extension: 0,
                    color_or_emblem: 0,
                    flag: 0,
                    effect: 0,
                    name: name.into(),
                }),
            }],
        }
    }

    #[test]
    fn deposit_intent_does_not_fill_vault() {
        let mut b = BankCorr::default();
        b.intent(BankIntent::Deposit {
            bag_slot: 40,
            vault_slot: 1,
        });
        assert!(b.vault_item(1).is_none());
        assert!(b.apply_vault_update(&vault_item(1, "gem"), 1));
        assert_eq!(b.vault_item(1).unwrap().name, "gem");
    }

    #[test]
    fn backpack_window_is_not_vault_authority() {
        let mut b = BankCorr::default();
        let mut upd = vault_item(1, "gem");
        upd.window_type = window_type::INVENTORY;
        assert!(!b.apply_vault_update(&upd, 1));
        assert!(b.vault_item(1).is_none());
    }

    #[test]
    fn consignment_and_guild_typed_decode() {
        let mut b = BankCorr::default();
        let body = encode_consignment(&ConsignmentMerchantMoney {
            gold: 12,
            ..ConsignmentMerchantMoney::default()
        });
        let m = decode_consignment(&body).unwrap();
        assert!(b.apply_consignment_money(m, 1));
        b.intent(BankIntent::Withdraw {
            vault_slot: 1,
            bag_slot: 40,
        });
        assert_eq!(b.consignment().unwrap().gold, 12);

        let gbody = encode_object_guild_id(50, 7);
        let g = decode_object_guild_id(&gbody).unwrap();
        assert!(b.apply_object_guild(g, 1));
        assert_eq!(b.guild_id(50), Some(7));
        assert!(!b.apply_object_guild(g, 0), "stale guild packet");
        assert_eq!(b.guild_id(50), Some(7));
    }
}
