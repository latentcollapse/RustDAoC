//! ECO leaf: items, economy, crafting, guild/consignment correlation.
//!
//! INT must call [`EcoState::apply`] from `WorldState::apply` (or a thin `eco_apply`) and
//! [`EcoState::apply_s2c`] for ConsignmentMerchantMoney 0x1E / ObjectGuildID 0xDE until those
//! codes gain `ServerEvent` variants (caer-abi C enum is currently exhaustive — do not add
//! discriminants from this lane).
//!
//! Local intent never awards server truth. Stale generation / reject / timeout leave authority
//! unchanged.

pub mod bank;
pub mod craft;
pub mod encumberance;
pub mod inventory;
pub mod market;
pub mod merchant;
pub mod money;
pub mod trainer;

pub use bank::{BankCorr, BankIntent};
pub use craft::CraftCorr;
pub use encumberance::EncumberanceCorr;
pub use inventory::{InvIntent, InventoryCorr};
pub use market::MarketExplorerCorr;
pub use merchant::{MerchantCorr, MerchantIntent};
pub use money::{MoneyCorr, MoneyIntent};
pub use trainer::{TrainIntent, TrainerCorr};

use caer_protocol::codes;
use caer_protocol::inventory::InventoryUpdate;
use caer_protocol::session::ServerEvent;

/// Generation-gated economy store. Independent of `WorldState` columns until INT wires it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EcoState {
    pub inventory: InventoryCorr,
    pub merchant: MerchantCorr,
    pub money: MoneyCorr,
    pub craft: CraftCorr,
    pub bank: BankCorr,
    pub encumberance: EncumberanceCorr,
    pub trainer: TrainerCorr,
    pub market_explorer: MarketExplorerCorr,
    pub emblem_dialogue: bool,
    pub find_group: Option<caer_protocol::findgroup::FindGroupUpdate>,
    s2c_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EcoApplyResult {
    Applied,
    Ignored,
    Stale,
}

impl EcoState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Next generation for a freshly observed S2C packet.
    fn bump(&mut self) -> u64 {
        self.s2c_generation = self.s2c_generation.saturating_add(1);
        self.s2c_generation
    }

    /// Fold already-typed session events. Unknown variants are ignored (INT owns new discriminants).
    pub fn apply(&mut self, ev: &ServerEvent) -> EcoApplyResult {
        match ev {
            ServerEvent::InventoryUpdated(u) => self.apply_inventory(u),
            ServerEvent::MerchantWindow(w) => {
                let g = self.bump();
                if self.merchant.apply_window(w.clone(), g) {
                    EcoApplyResult::Applied
                } else {
                    EcoApplyResult::Stale
                }
            }
            ServerEvent::MoneyUpdated(m) => {
                let g = self.bump();
                if self.money.apply_update(*m, g) {
                    EcoApplyResult::Applied
                } else {
                    EcoApplyResult::Stale
                }
            }
            ServerEvent::ConsignmentMerchantMoney(m) => {
                let g = self.bump();
                if self.bank.apply_consignment_money(*m, g) {
                    EcoApplyResult::Applied
                } else {
                    EcoApplyResult::Stale
                }
            }
            ServerEvent::ObjectGuildId(g) => {
                let gen = self.bump();
                if self.bank.apply_object_guild(*g, gen) {
                    EcoApplyResult::Applied
                } else {
                    EcoApplyResult::Stale
                }
            }
            ServerEvent::Encumberance(e) => {
                let g = self.bump();
                if self.encumberance.apply_update(*e, g) {
                    EcoApplyResult::Applied
                } else {
                    EcoApplyResult::Stale
                }
            }
            ServerEvent::TrainerWindow(w) => {
                let g = self.bump();
                if self.trainer.apply_window(w.clone(), g) {
                    EcoApplyResult::Applied
                } else {
                    EcoApplyResult::Stale
                }
            }
            ServerEvent::MarketExplorer(m) => {
                let g = self.bump();
                if self.market_explorer.apply(*m, g) {
                    EcoApplyResult::Applied
                } else {
                    EcoApplyResult::Stale
                }
            }
            ServerEvent::EmblemDialogue(_) => {
                self.bump();
                self.emblem_dialogue = true;
                EcoApplyResult::Applied
            }
            ServerEvent::FindGroupUpdate(f) => {
                self.bump();
                self.find_group = Some(*f);
                EcoApplyResult::Applied
            }
            ServerEvent::LoggedOut { .. } | ServerEvent::RegionChanged(_) => {
                *self = Self::default();
                EcoApplyResult::Applied
            }
            _ => EcoApplyResult::Ignored,
        }
    }

    fn apply_inventory(&mut self, u: &InventoryUpdate) -> EcoApplyResult {
        let g = self.bump();
        if u.is_vault_or_consignment() {
            let ok = self.bank.apply_vault_update(u, g);
            return if ok {
                EcoApplyResult::Applied
            } else {
                EcoApplyResult::Stale
            };
        }
        let ok = self.inventory.apply_update(u, g);
        if ok {
            self.craft.observe_inventory(u, self.inventory.generation());
        }
        if ok {
            EcoApplyResult::Applied
        } else {
            EcoApplyResult::Stale
        }
    }

    /// Typed S2C that is still `ServerEvent::Raw` on the session multiplexer.
    ///
    /// OPEN_ORACLE: `PacketLib168.SendConsignmentMerchantMoney`, `PacketLib168.SendObjectGuildID`.
    pub fn apply_s2c(&mut self, code: u8, payload: &[u8]) -> EcoApplyResult {
        match code {
            c if c == codes::server::ConsignmentMerchantMoney => {
                match caer_protocol::money::decode_consignment(payload) {
                    Ok(m) => {
                        let g = self.bump();
                        if self.bank.apply_consignment_money(m, g) {
                            EcoApplyResult::Applied
                        } else {
                            EcoApplyResult::Stale
                        }
                    }
                    Err(_) => EcoApplyResult::Ignored,
                }
            }
            c if c == codes::server::ObjectGuildID => {
                match caer_protocol::social::decode_object_guild_id(payload) {
                    Ok(g) => {
                        let gen = self.bump();
                        if self.bank.apply_object_guild(g, gen) {
                            EcoApplyResult::Applied
                        } else {
                            EcoApplyResult::Stale
                        }
                    }
                    Err(_) => EcoApplyResult::Ignored,
                }
            }
            c if c == codes::server::Encumberance => {
                match caer_protocol::encumberance::decode(payload) {
                    Ok(e) => {
                        let g = self.bump();
                        if self.encumberance.apply_update(e, g) {
                            EcoApplyResult::Applied
                        } else {
                            EcoApplyResult::Stale
                        }
                    }
                    Err(_) => EcoApplyResult::Ignored,
                }
            }
            c if c == codes::server::TrainerWindow => {
                match caer_protocol::trainer::decode(payload) {
                    Ok(w) => {
                        let g = self.bump();
                        if self.trainer.apply_window(w, g) {
                            EcoApplyResult::Applied
                        } else {
                            EcoApplyResult::Stale
                        }
                    }
                    Err(_) => EcoApplyResult::Ignored,
                }
            }
            c if c == codes::server::MarketExplorerWindow => {
                match caer_protocol::market::decode(payload) {
                    Ok(m) => {
                        let g = self.bump();
                        if self.market_explorer.apply(m, g) {
                            EcoApplyResult::Applied
                        } else {
                            EcoApplyResult::Stale
                        }
                    }
                    Err(_) => EcoApplyResult::Ignored,
                }
            }
            c if c == codes::server::EmblemDialogue => {
                match caer_protocol::emblem::decode(payload) {
                    Ok(_) => {
                        self.bump();
                        self.emblem_dialogue = true;
                        EcoApplyResult::Applied
                    }
                    Err(_) => EcoApplyResult::Ignored,
                }
            }
            c if c == codes::server::FindGroupUpdate => {
                match caer_protocol::findgroup::decode(payload) {
                    Ok(f) => {
                        self.bump();
                        self.find_group = Some(f);
                        EcoApplyResult::Applied
                    }
                    Err(_) => EcoApplyResult::Ignored,
                }
            }
            _ => EcoApplyResult::Ignored,
        }
    }

    pub fn intent_move(&mut self, from_slot: u8, to_slot: u8, count: u16) {
        self.inventory.intent(InvIntent::Move {
            from_slot,
            to_slot,
            count,
        });
    }

    pub fn intent_use(&mut self, slot: u8) {
        self.inventory.intent(InvIntent::Use { slot });
    }

    pub fn intent_equip(&mut self, from_slot: u8, to_slot: u8) {
        self.inventory
            .intent(InvIntent::Equip { from_slot, to_slot });
    }

    pub fn intent_buy(&mut self, page_slot: u8, count: u8) {
        self.merchant
            .intent(MerchantIntent::Buy { page_slot, count });
        self.money.intent(MoneyIntent::Buy);
    }

    pub fn intent_sell(&mut self, bag_slot: u16) {
        self.merchant.intent(MerchantIntent::Sell { bag_slot });
        self.money.intent(MoneyIntent::Sell);
    }

    pub fn intent_craft(&mut self, item_id: u16) {
        self.craft
            .intent_craft(item_id, self.inventory.generation());
    }

    pub fn intent_destroy(&mut self, slot: u8) {
        self.inventory.intent(InvIntent::Destroy { slot });
    }

    pub fn intent_train_window(&mut self) {
        self.trainer.intent(TrainIntent::OpenWindow);
    }

    pub fn intent_train(&mut self, id_line: u8, row: u8, skill_index: u8) {
        self.trainer.intent(TrainIntent::Train {
            id_line,
            row,
            skill_index,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::inventory::{InventoryItem, ItemData};
    use caer_protocol::invverb::{
        encode_buy_request, encode_craft_request, encode_move_item, encode_sell_request,
        encode_use_slot_1124,
    };
    use caer_protocol::merchant::{window_type, MerchantOffer, MerchantWindow};
    use caer_protocol::money::{encode_consignment, ConsignmentMerchantMoney, MoneyUpdate};
    use caer_protocol::social::encode_object_guild_id;

    fn bag_item(slot: u8, name: &str) -> InventoryUpdate {
        InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0,
            window_type: 2,
            items: vec![InventoryItem {
                slot,
                item: Some(ItemData {
                    unique_id: 0,
                    level: 1,
                    value1: 1,
                    value2: 0,
                    hand_byte: 0,
                    object_type_byte: 0,
                    unk_1112: 0,
                    weight: 1,
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
                    name: name.into(),
                }),
            }],
        }
    }

    fn shop() -> MerchantWindow {
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
                price: 5,
                model: 499,
                name: "ticket to Castle Sauvage".into(),
            }],
        }
    }

    /// Falsifier: local invverb must not award slots/purse.
    #[test]
    fn local_intent_never_awards_server_truth() {
        let mut eco = EcoState::new();
        let _ = encode_move_item(25, 40, 1);
        let _ = encode_use_slot_1124(0.0, 0.0, 0.0, 0.0, 0, 0, 40, 0);
        let _ = encode_buy_request(0, 0, 1, 0, 1);
        let _ = encode_sell_request(0, 0, 1, 40);
        let _ = encode_craft_request(0x10);

        eco.apply(&ServerEvent::InventoryUpdated(bag_item(40, "sword")));
        eco.apply(&ServerEvent::MoneyUpdated(MoneyUpdate {
            gold: 20,
            ..MoneyUpdate::default()
        }));
        eco.apply(&ServerEvent::MerchantWindow(shop()));

        eco.intent_move(40, 25, 1);
        eco.intent_equip(40, 25);
        eco.intent_use(40);
        eco.intent_buy(0, 1);
        eco.intent_sell(40);
        eco.intent_craft(0x10);
        eco.intent_destroy(40);

        assert!(eco.inventory.item(40).is_some());
        assert!(eco.inventory.item(25).is_none());
        assert_eq!(eco.money.purse().unwrap().gold, 20);
        assert_eq!(eco.merchant.catalogue().unwrap().items.len(), 1);
        assert!(!eco.craft.complete());
        assert!(eco.bank.consignment().is_none());
    }

    #[test]
    fn inventory_and_money_packets_are_authority() {
        let mut eco = EcoState::new();
        eco.apply(&ServerEvent::InventoryUpdated(bag_item(40, "sword")));
        eco.intent_equip(40, 25);
        eco.apply(&ServerEvent::InventoryUpdated(InventoryUpdate {
            unused_speed: 0,
            cloak_invisible: false,
            helm_invisible: false,
            hood_up: false,
            active_quiver: 0,
            active_weapon_slots: 0x10,
            window_type: 1,
            items: vec![
                InventoryItem {
                    slot: 40,
                    item: None,
                },
                bag_item(25, "sword").items.into_iter().next().unwrap(),
            ],
        }));
        assert!(eco.inventory.item(40).is_none());
        assert_eq!(eco.inventory.item(25).unwrap().name, "sword");

        eco.intent_buy(0, 1);
        eco.apply(&ServerEvent::MoneyUpdated(MoneyUpdate {
            gold: 15,
            ..MoneyUpdate::default()
        }));
        assert_eq!(eco.money.purse().unwrap().gold, 15);
    }

    #[test]
    fn stale_generation_leaves_authority_unchanged() {
        let mut inv = InventoryCorr::default();
        assert!(inv.apply_update(&bag_item(40, "new"), 5));
        assert!(!inv.apply_update(&bag_item(40, "old"), 4));
        assert_eq!(inv.item(40).unwrap().name, "new");

        let mut money = MoneyCorr::default();
        assert!(money.apply_update(
            MoneyUpdate {
                gold: 9,
                ..MoneyUpdate::default()
            },
            2
        ));
        assert!(!money.apply_update(MoneyUpdate::default(), 1));
        assert_eq!(money.purse().unwrap().gold, 9);
    }

    #[test]
    fn consignment_and_guild_via_apply_s2c() {
        let mut eco = EcoState::new();
        let cm = encode_consignment(&ConsignmentMerchantMoney {
            copper: 1,
            silver: 2,
            gold: 3,
            mithril: 0,
            platinum: 0,
        });
        assert_eq!(
            eco.apply_s2c(codes::server::ConsignmentMerchantMoney, &cm),
            EcoApplyResult::Applied
        );
        assert_eq!(eco.bank.consignment().unwrap().gold, 3);
        // Consignment money must not overwrite player 0xFA purse.
        assert!(eco.money.purse().is_none());

        let g = encode_object_guild_id(100, 22);
        assert_eq!(
            eco.apply_s2c(codes::server::ObjectGuildID, &g),
            EcoApplyResult::Applied
        );
        assert_eq!(eco.bank.guild_id(100), Some(22));
    }

    #[test]
    fn encumberance_via_apply_s2c_does_not_touch_inventory() {
        use caer_protocol::encumberance::{encode, Encumberance};
        let mut eco = EcoState::new();
        eco.apply(&ServerEvent::InventoryUpdated(bag_item(40, "sword")));
        let payload = encode(&Encumberance { max: 200, used: 45 });
        assert_eq!(
            eco.apply_s2c(codes::server::Encumberance, &payload),
            EcoApplyResult::Applied
        );
        assert_eq!(eco.encumberance.current().unwrap().used, 45);
        assert_eq!(eco.inventory.item(40).unwrap().name, "sword");
        eco.intent_destroy(40);
        assert_eq!(eco.inventory.item(40).unwrap().name, "sword");
    }

    #[test]
    fn trainer_market_lfg_emblem_via_apply_s2c() {
        use caer_protocol::market::encode_header;
        use caer_protocol::trainer::{encode_spec_window, TrainerLine};
        let mut eco = EcoState::new();
        let spec = encode_spec_window(
            9,
            &[TrainerLine {
                index: 0,
                level: 4,
                cost_or_next: 5,
                name: "Crush".into(),
            }],
        );
        eco.intent_train_window();
        assert_eq!(
            eco.apply_s2c(codes::server::TrainerWindow, &spec),
            EcoApplyResult::Applied
        );
        assert_eq!(eco.trainer.window().unwrap().lines[0].level, 4);
        eco.intent_train(0, 1, 1);
        assert_eq!(eco.trainer.window().unwrap().lines[0].level, 4);

        let mx = encode_header(&caer_protocol::market::MarketExplorer {
            count: 2,
            page: 0,
            max_page: 1,
        });
        assert_eq!(
            eco.apply_s2c(codes::server::MarketExplorerWindow, &mx),
            EcoApplyResult::Applied
        );
        assert_eq!(eco.market_explorer.current().unwrap().count, 2);
        assert!(eco.merchant.catalogue().is_none());

        assert_eq!(
            eco.apply_s2c(codes::server::EmblemDialogue, &[0, 0, 0, 0]),
            EcoApplyResult::Applied
        );
        assert!(eco.emblem_dialogue);

        assert_eq!(
            eco.apply_s2c(codes::server::FindGroupUpdate, &[0, 0]),
            EcoApplyResult::Applied
        );
        assert!(eco.find_group.unwrap().empty_list);
    }
}
