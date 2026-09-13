//! rustdaoc product social/UI controller.
//!
//! rustdaoc and scenario drivers share this type. Player clicks/keys become [`ProductInput`];
//! success presentation still waits on authoritative inbound. This is **not** `PLAYER_SCENARIO`
//! without a live dual-account window proof (`CAER_ACCOUNT2`).

use std::collections::HashSet;
use std::time::Instant;

use caer_assets::uiskin::Skin;
use caer_protocol::session::ServerEvent;

use crate::adapters::{AdapterState, SocialBind};
use crate::live::{Drain, LiveCommand};
use crate::product_input::ProductInput;
use crate::product_ui;
use crate::skinhud::{
    CriticalHudBind, CriticalWindowClass, SkinHud, PRODUCT_DIALOG_WINDOW, PRODUCT_INVITE_WINDOW,
    PRODUCT_TRADE_WINDOW,
};
use crate::skinui::{UiText, WindowDraw, WindowManager};
use crate::social_ui::{encode_social_command, ProductKey, SocialUiState};

/// Honest proof class for ProductController drivers. Not a scoreboard promotion.
pub const PRODUCT_LOOP_PROOF_CLASS: &str = "COMPONENT_BINDING";
/// System 6 PLAYER_SCENARIO numerator until live dual-account product proof exists.
pub const SYS6_PLAYER_SCENARIO_EARNED: usize = 0;
/// System 6 PLAYER_SCENARIO denominator (group / trade / quest).
pub const SYS6_PLAYER_SCENARIO_DENOM: usize = 3;

/// Default product invite overlay position (near target chrome).
pub const PRODUCT_INVITE_POS: (f32, f32) = (300.0, 40.0);

/// Context rustdaoc passes into [`ProductController::dispatch`] (same fields a player has).
#[derive(Debug, Clone, Copy, Default)]
pub struct ProductDispatchCtx {
    pub chat_open: bool,
    pub has_target: bool,
}

/// Result of one product-input dispatch.
#[derive(Debug, Clone, Default)]
pub struct ProductDispatch {
    pub consumed: bool,
    pub command: Option<LiveCommand>,
    pub hit: Option<(String, String)>,
}

/// Observable product UI snapshot for scenario falsifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductSocialSnapshot {
    pub in_group: bool,
    pub trade_complete: bool,
    pub quest_accepted: bool,
    pub dialog_open: bool,
    pub trade_open: bool,
    pub invite_open: bool,
    pub group_open: bool,
    pub quest_journal_open: bool,
    pub last_outbound: Option<(u8, Vec<u8>)>,
    pub last_hit: Option<(String, String)>,
    pub last_input_kind: Option<&'static str>,
}

/// Exact System 6 accounting for the freeze ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sys6FlowAccounting {
    pub player_scenario: bool,
    pub proof_class: &'static str,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sys6Accounting {
    pub group: Sys6FlowAccounting,
    pub trade: Sys6FlowAccounting,
    pub quest: Sys6FlowAccounting,
}

/// rustdaoc product social loop: state + WindowManager + ProductInput dispatch.
#[derive(Debug, Default)]
pub struct ProductController {
    pub social: SocialUiState,
    pub wm: WindowManager,
    last_input: Option<ProductInput>,
    /// Skin names the player explicitly closed. Packet gates may still be eligible; sync must
    /// not reopen these until the player toggles them open again.
    player_closed: HashSet<String>,
}

impl ProductController {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn last_input(&self) -> Option<ProductInput> {
        self.last_input
    }

    /// Reconnect / logout / region: no ghost windows or success presentation.
    pub fn reconnect_reset(&mut self) {
        self.social.reconnect_reset(&mut self.wm);
        self.wm.close(PRODUCT_INVITE_WINDOW);
        self.last_input = None;
        self.player_closed.clear();
    }

    #[must_use]
    pub fn player_closed(&self) -> &HashSet<String> {
        &self.player_closed
    }

    /// Packet-gated HUD sync that will not reopen player-closed panels.
    pub fn sync_hud(&mut self, hud: &SkinHud, bind: CriticalHudBind) {
        product_ui::sync_critical_windows_for_player(&mut self.wm, hud, bind, &self.player_closed);
    }

    pub fn observe(&mut self, ev: &ServerEvent, now: Instant) {
        self.social.observe(ev, now);
        self.social.sync_product_windows(&mut self.wm);
    }

    /// rustdaoc live drain → controller observe. Must not mutate [`SocialUiState`] directly.
    /// Dialog/trade windows sync here (controller-only); bypassing to `social.observe` leaves them closed.
    pub fn observe_drain(&mut self, d: &Drain, now: Instant) {
        for dlg in &d.dialogs {
            self.observe(
                &ServerEvent::Dialog {
                    code: dlg.code,
                    data1: dlg.data1,
                    data2: dlg.data2,
                    data3: dlg.data3,
                    data4: dlg.data4,
                    message: dlg.message.clone(),
                },
                now,
            );
        }
        for tw in &d.trade_windows {
            self.observe(&ServerEvent::TradeWindow(tw.clone()), now);
        }
        for gw in &d.group_windows {
            self.observe(&ServerEvent::GroupWindow(gw.clone()), now);
        }
        for g in &d.group_member_updates {
            self.observe(&ServerEvent::GroupMemberUpdate(g.clone()), now);
        }
        for entry in &d.quest_entries {
            self.observe(
                &ServerEvent::QuestEntry {
                    entry: entry.clone(),
                    payload: Vec::new(),
                },
                now,
            );
        }
        if d.inventory_updated {
            self.observe(
                &ServerEvent::InventoryUpdated(caer_protocol::inventory::InventoryUpdate {
                    unused_speed: 0,
                    cloak_invisible: false,
                    helm_invisible: false,
                    hood_up: false,
                    active_quiver: 0,
                    active_weapon_slots: 0,
                    window_type: 0,
                    items: Vec::new(),
                }),
                now,
            );
        }
        if d.money_updated {
            self.observe(&ServerEvent::MoneyUpdated(Default::default()), now);
        }
    }

    pub fn tick_timeouts(&mut self, now: Instant) -> bool {
        let cleared = self.social.tick_timeouts(now);
        if cleared {
            self.social.sync_product_windows(&mut self.wm);
        }
        cleared
    }

    /// Open/close dialog/trade/invite from live state. Invite chrome follows target, not membership.
    pub fn sync_windows(&mut self, has_target: bool) {
        if has_target {
            self.wm.open(PRODUCT_INVITE_WINDOW, PRODUCT_INVITE_POS);
        } else {
            self.wm.close(PRODUCT_INVITE_WINDOW);
        }
        self.social.sync_product_windows(&mut self.wm);
    }

    /// Record a player close of an ordinary panel (not dialog/trade/invite protocol chrome).
    /// Expands to all alternate skin names for the same class so sync cannot reopen a sibling.
    fn note_player_closed_panel(&mut self, name: &str) {
        if name == PRODUCT_DIALOG_WINDOW
            || name == PRODUCT_TRADE_WINDOW
            || name == PRODUCT_INVITE_WINDOW
        {
            return;
        }
        self.player_closed.insert(name.to_string());
        for class in CriticalWindowClass::ALL {
            if class.skin_names().contains(&name) {
                for n in class.skin_names() {
                    self.player_closed.insert((*n).to_string());
                }
            }
        }
        const QUEST: &[&str] = &["new_quest_journal", "quest", "new_quest"];
        if QUEST.contains(&name) {
            for n in QUEST {
                self.player_closed.insert((*n).to_string());
            }
        }
    }

    fn note_player_toggle_panel(
        &mut self,
        skin: &Skin,
        class: CriticalWindowClass,
        preferred: &str,
        pos: (f32, f32),
    ) {
        let mut names = Vec::with_capacity(class.skin_names().len() + 1);
        names.push(preferred);
        for n in class.skin_names() {
            if !names.contains(n) {
                names.push(*n);
            }
        }
        self.note_player_toggle_names(skin, &names, pos);
    }

    /// Toggle the actually open alternate skin name, or open the first skin-present candidate.
    fn note_player_toggle_names(&mut self, skin: &Skin, candidates: &[&str], pos: (f32, f32)) {
        if let Some(open) = candidates.iter().copied().find(|n| self.wm.is_open(n)) {
            self.wm.close(open);
            for n in candidates {
                self.player_closed.insert((*n).to_string());
            }
            return;
        }
        let name = candidates
            .iter()
            .copied()
            .find(|n| skin.windows.contains_key(*n))
            .unwrap_or(candidates[0]);
        for n in candidates {
            self.player_closed.remove(*n);
        }
        self.wm.open(name, pos);
    }

    #[must_use]
    pub fn social_bind<'a>(&'a self, trade_terms: Option<&'a str>) -> SocialBind<'a> {
        SocialBind {
            group: self.social.group.as_ref(),
            group_vitals: self.social.group_vitals.as_ref(),
            quests: Some(self.social.quests.as_slice()),
            trade: self.social.trade.as_ref().map(|t| &t.window),
            dialog_message: self.social.pending.as_ref().map(|p| p.message.as_str()),
            trade_terms,
        }
    }

    /// Stock layout + generation gate. rustdaoc and drivers share this.
    #[must_use]
    pub fn draw(
        &mut self,
        skin: &Skin,
        state: &AdapterState<'_>,
        extra: &crate::adapters::ExtraBind<'_>,
        combat: Option<&caer_world::CombatPresentation<'_>>,
    ) -> WindowDraw {
        let trade_terms = self.social.trade.as_ref().map(|t| t.terms_text());
        let social = self.social_bind(trade_terms.as_deref());
        let draw = product_ui::draw_product(skin, &self.wm, state, &social, extra, combat);
        self.social.note_stock_layout(&draw.texts);
        draw
    }

    #[must_use]
    pub fn snapshot(&self) -> ProductSocialSnapshot {
        ProductSocialSnapshot {
            in_group: self.social.in_group,
            trade_complete: self.social.trade_complete,
            quest_accepted: self.social.quest_accepted,
            dialog_open: self.wm.is_open(PRODUCT_DIALOG_WINDOW),
            trade_open: self.wm.is_open(PRODUCT_TRADE_WINDOW),
            invite_open: self.wm.is_open(PRODUCT_INVITE_WINDOW),
            group_open: self.wm.is_open("mini_group")
                || self.wm.is_open("new_group_window")
                || self.wm.is_open("stats_group"),
            quest_journal_open: self.wm.is_open("new_quest_journal")
                || self.wm.is_open("quest")
                || self.wm.is_open("new_quest"),
            last_outbound: self.social.last_outbound.clone(),
            last_hit: self.social.last_hit.clone(),
            last_input_kind: self.last_input.map(input_kind),
        }
    }

    /// Same route rustdaoc uses for overlay keys, pointer, and `/invite`.
    pub fn dispatch(
        &mut self,
        input: ProductInput,
        skin: &Skin,
        ctx: ProductDispatchCtx,
    ) -> ProductDispatch {
        self.last_input = Some(input);
        self.sync_windows(ctx.has_target);
        match input {
            ProductInput::PointerPress { x, y } => {
                let r = self
                    .social
                    .handle_product_pointer(&mut self.wm, skin, x, y, ctx.chat_open);
                if let Some(name) = r.closed_window.as_deref() {
                    self.note_player_closed_panel(name);
                }
                let invite_hit = r
                    .hit
                    .as_ref()
                    .is_some_and(|(_, id)| id.eq_ignore_ascii_case("invite"));
                if r.command.is_none() && invite_hit {
                    return self.dispatch_invite(ctx, r.consumed, r.hit);
                }
                ProductDispatch {
                    consumed: r.consumed,
                    command: r.command,
                    hit: r.hit,
                }
            }
            ProductInput::SocialAccept => self.dispatch_key(ProductKey::Accept, skin, ctx),
            ProductInput::SocialRefuse => self.dispatch_key(ProductKey::Refuse, skin, ctx),
            ProductInput::SocialEscape => self.dispatch_key(ProductKey::Escape, skin, ctx),
            ProductInput::InviteToGroup => self.dispatch_invite(ctx, true, None),
            ProductInput::ToggleInventory => {
                self.note_player_toggle_panel(
                    skin,
                    CriticalWindowClass::InventoryEquipment,
                    "stats_index",
                    (200.0, 80.0),
                );
                ProductDispatch {
                    consumed: true,
                    command: None,
                    hit: None,
                }
            }
            ProductInput::ToggleStats => {
                self.note_player_toggle_panel(
                    skin,
                    CriticalWindowClass::CharacterSheet,
                    "stats_attributes",
                    (200.0, 120.0),
                );
                ProductDispatch {
                    consumed: true,
                    command: None,
                    hit: None,
                }
            }
            ProductInput::ToggleGroup => {
                self.note_player_toggle_panel(
                    skin,
                    CriticalWindowClass::Group,
                    "mini_group",
                    (590.0, 519.0),
                );
                ProductDispatch {
                    consumed: true,
                    command: None,
                    hit: None,
                }
            }
            ProductInput::ToggleMap => {
                self.note_player_toggle_panel(
                    skin,
                    CriticalWindowClass::Map,
                    "map_window",
                    (80.0, 80.0),
                );
                ProductDispatch {
                    consumed: true,
                    command: None,
                    hit: None,
                }
            }
            ProductInput::ToggleQuest => {
                self.note_player_toggle_names(
                    skin,
                    &["new_quest_journal", "quest", "new_quest"],
                    (40.0, 200.0),
                );
                ProductDispatch {
                    consumed: true,
                    command: None,
                    hit: None,
                }
            }
            ProductInput::ToggleSkills => {
                self.note_player_toggle_panel(
                    skin,
                    CriticalWindowClass::SkillsQuickbar,
                    "stats_spec_abil",
                    (8.0, 520.0),
                );
                ProductDispatch {
                    consumed: true,
                    command: None,
                    hit: None,
                }
            }
            ProductInput::ShowCombat => {
                self.note_player_toggle_names(
                    skin,
                    &["combat", "combat_window", "new_combat"],
                    (8.0, 80.0),
                );
                ProductDispatch {
                    consumed: true,
                    command: None,
                    hit: None,
                }
            }
            ProductInput::RealmWarMap => {
                self.note_player_toggle_names(
                    skin,
                    &["realm_war_map", "rvr_map", "map_window"],
                    (80.0, 80.0),
                );
                ProductDispatch {
                    consumed: true,
                    command: None,
                    hit: None,
                }
            }
            ProductInput::CommandWindow => {
                // Chat open is rustdaoc-owned; mark consumed so dispatch does not fall through.
                ProductDispatch {
                    consumed: true,
                    command: None,
                    hit: None,
                }
            }
            ProductInput::Interact
            | ProductInput::Get
            | ProductInput::Follow
            | ProductInput::Stick
            | ProductInput::Face
            | ProductInput::Walk
            | ProductInput::Sprint
            | ProductInput::TargetFriend
            | ProductInput::TargetObject
            | ProductInput::LastAttacker
            | ProductInput::Reply
            | ProductInput::Consider
            | ProductInput::UseItem
            | ProductInput::UseItemSecondary
            | ProductInput::Sell
            | ProductInput::Runlock
            | ProductInput::GroundTarget
            | ProductInput::TargetGroup1
            | ProductInput::TargetGroup2
            | ProductInput::TargetGroup3
            | ProductInput::TargetGroup4
            | ProductInput::TargetGroup5
            | ProductInput::TargetGroup6
            | ProductInput::TargetGroup7
            | ProductInput::TargetGroup8
            | ProductInput::Craft
            | ProductInput::RightHandWeapon
            | ProductInput::TwoHandedWeapon
            | ProductInput::RangedWeapon
            | ProductInput::LookUp
            | ProductInput::LookDown
            | ProductInput::ResetCamera
            | ProductInput::ToggleNames
            | ProductInput::MouseLookToggle
            | ProductInput::Torch
            | ProductInput::PerfMeter
            | ProductInput::InformationDelve
            | ProductInput::ChatLog
            | ProductInput::PageUp
            | ProductInput::PageDown
            | ProductInput::PanCamera
            | ProductInput::Mouse => ProductDispatch {
                consumed: true,
                command: None,
                hit: None,
            },
            _ => ProductDispatch::default(),
        }
    }

    fn dispatch_key(
        &mut self,
        key: ProductKey,
        _skin: &Skin,
        ctx: ProductDispatchCtx,
    ) -> ProductDispatch {
        let r = self
            .social
            .handle_product_key(&mut self.wm, key, ctx.chat_open);
        if let Some(name) = r.closed_window.as_deref() {
            self.note_player_closed_panel(name);
        }
        ProductDispatch {
            consumed: r.command.is_some() || matches!(key, ProductKey::Escape),
            command: r.command,
            hit: self.social.last_hit.clone(),
        }
    }

    fn dispatch_invite(
        &mut self,
        ctx: ProductDispatchCtx,
        consumed: bool,
        hit: Option<(String, String)>,
    ) -> ProductDispatch {
        if ctx.chat_open || !ctx.has_target {
            return ProductDispatch {
                consumed,
                command: None,
                hit,
            };
        }
        let command = LiveCommand::InviteToGroup;
        self.social.last_command = Some(command.clone());
        self.social.last_outbound = encode_social_command(&command);
        ProductDispatch {
            consumed: true,
            command: Some(command),
            hit,
        }
    }
}

#[must_use]
pub fn sys6_accounting() -> Sys6Accounting {
    let reason = "product ProductInput→outbound→inbound→UI is COMPONENT_BINDING; \
PLAYER_SCENARIO needs live dual-account rustdaoc window proof (CAER_ACCOUNT2)";
    let flow = Sys6FlowAccounting {
        player_scenario: false,
        proof_class: PRODUCT_LOOP_PROOF_CLASS,
        reason,
    };
    Sys6Accounting {
        group: flow,
        trade: flow,
        quest: flow,
    }
}

fn input_kind(input: ProductInput) -> &'static str {
    match input {
        ProductInput::PointerPress { .. } => "pointer",
        ProductInput::SocialAccept => "social_accept",
        ProductInput::SocialRefuse => "social_refuse",
        ProductInput::SocialEscape => "social_escape",
        ProductInput::InviteToGroup => "invite",
        _ => "other",
    }
}

/// Adapter texts currently bound in a draw (scenario observability).
#[must_use]
pub fn bound_adapter_text<'a>(texts: &'a [UiText], adapter: &str) -> Option<&'a str> {
    texts
        .iter()
        .find(|t| t.adapter.as_deref() == Some(adapter) && !t.text.is_empty())
        .map(|t| t.text.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::quest::QuestEntry;
    use caer_protocol::social::{GroupWindow, GroupWindowMember, ModifyTradeAction, TradeWindow};
    use caer_protocol::status::PlayerStatus;
    use caer_world::WorldState;
    use std::time::Duration;

    use crate::live::{Drain, SocialDialog};
    use crate::skinhud::{self, SkinHud};
    use crate::social_ui::{DIALOG_GROUP_INVITE, DIALOG_YES};

    fn stub_hud() -> SkinHud {
        let mut skin = caer_assets::uiskin::Skin::default();
        skinhud::inject_product_social_overlays(&mut skin);
        skin.windows.insert(
            "mini_group".into(),
            caer_assets::uiskin::WindowTemplate {
                name: "mini_group".into(),
                width: 200,
                height: 80,
                controls: vec![caer_assets::uiskin::Control::Label(
                    caer_assets::uiskin::Label {
                        control_id: Some("g0".into()),
                        adapter: Some("group_name0".into()),
                        pos: (8, 20),
                        width: 180,
                        height: 16,
                        font: Some("arial11".into()),
                        ..Default::default()
                    },
                )],
                ..Default::default()
            },
        );
        skin.windows.insert(
            "new_quest_journal".into(),
            caer_assets::uiskin::WindowTemplate {
                name: "new_quest_journal".into(),
                width: 240,
                height: 80,
                controls: vec![caer_assets::uiskin::Control::Label(
                    caer_assets::uiskin::Label {
                        control_id: Some("qt".into()),
                        adapter: Some("quest_title".into()),
                        pos: (8, 20),
                        width: 220,
                        height: 20,
                        font: Some("arial11".into()),
                        ..Default::default()
                    },
                )],
                ..Default::default()
            },
        );
        for name in [
            "stats_index",
            "stats_attributes",
            "map_window",
            "stats_spec_abil",
        ] {
            skin.windows.insert(
                name.into(),
                caer_assets::uiskin::WindowTemplate {
                    name: name.into(),
                    width: 200,
                    height: 80,
                    title_height: 16,
                    close_button: true,
                    ..Default::default()
                },
            );
        }
        // Alternate group skin name — falsifier must close the actual resolved name.
        skin.windows.insert(
            "new_group_window".into(),
            caer_assets::uiskin::WindowTemplate {
                name: "new_group_window".into(),
                width: 220,
                height: 100,
                title_height: 16,
                close_button: true,
                ..Default::default()
            },
        );
        if let Some(w) = skin.windows.get_mut("mini_group") {
            w.title_height = 16;
            w.close_button = true;
            w.width = 200;
            w.height = 80;
        }
        if let Some(w) = skin.windows.get_mut("new_quest_journal") {
            w.title_height = 16;
            w.close_button = true;
            w.width = 240;
            w.height = 80;
        }
        SkinHud::from_skin(skin)
    }

    fn empty_drain() -> Drain {
        Drain {
            applied: 0,
            player: None,
            ended: false,
            self_object_id: None,
            messages: vec![],
            combat_anims: vec![],
            spell_casts: vec![],
            spell_effects: vec![],
            overview: None,
            phase: None,
            region_changed: None,
            removed_ids: vec![],
            logged_out: false,
            dialogs: vec![],
            trade_windows: vec![],
            group_windows: vec![],
            group_member_updates: vec![],
            quest_entries: vec![],
            inventory_updated: false,
            money_updated: false,
            entered_world: false,
            login_granted: false,
            crypt_key_received: false,
            login_denied: None,
            attack_mode: None,
            max_speed_percent: None,
            server_target_oid: None,
            udp_init: None,
            pending_los: None,
            name_check_bad: None,
            name_check_dup: None,
            create_reply: None,
            delve: None,
        }
    }

    fn invite() -> ServerEvent {
        ServerEvent::Dialog {
            code: DIALOG_GROUP_INVITE,
            data1: 3,
            data2: 0,
            data3: 0,
            data4: 0,
            message: "Feile has invited you to join a group".into(),
        }
    }

    fn roster() -> ServerEvent {
        ServerEvent::GroupWindow(GroupWindow {
            members: vec![GroupWindowMember {
                name: "Feile".into(),
                salutation: String::new(),
                object_id: 12,
                level: 50,
            }],
        })
    }

    fn open_trade() -> ServerEvent {
        ServerEvent::TradeWindow(TradeWindow {
            closed: false,
            own_slots: [0; 10],
            own_money: Default::default(),
            partner_money: Default::default(),
            partner_item_count: 0,
            repairing: false,
            combining: false,
            caption: "Feile".into(),
        })
    }

    fn closed_trade() -> ServerEvent {
        ServerEvent::TradeWindow(TradeWindow {
            closed: true,
            own_slots: [0; 10],
            own_money: Default::default(),
            partner_money: Default::default(),
            partner_item_count: 0,
            repairing: false,
            combining: false,
            caption: String::new(),
        })
    }

    fn quest_offer(index: u16) -> ServerEvent {
        ServerEvent::Dialog {
            code: 0x64,
            data1: index,
            data2: 0,
            data3: 0,
            data4: 0,
            message: "Accept Important Delivery?".into(),
        }
    }

    fn quest_entry(index: u8, name: &str) -> ServerEvent {
        ServerEvent::QuestEntry {
            entry: QuestEntry {
                index,
                name: name.into(),
                description: "[Step #1]: go".into(),
            },
            payload: Vec::new(),
        }
    }

    fn idle_state<'a>(status: &'a PlayerStatus, world: &'a WorldState) -> AdapterState<'a> {
        AdapterState {
            player_name: "",
            status,
            target: None,
            zone: None,
            fps: 0,
            sheet: world.character_sheet(),
            char_stats: world.char_stats(),
            char_resists: world.char_resists(),
            equipment: None,
            money: world.money(),
            inventory: world.inventory(),
            merchant: world.merchant(),
            weapon_armor: None,
            attack_mode: None,
            login_account: None,
            login_password_mask: None,
            create_name: None,
        }
    }

    /// Falsifier: observe_drain must go through observe (window sync), not social.observe alone.
    #[test]
    fn observe_drain_opens_dialog_window() {
        let now = Instant::now();
        let mut pc = ProductController::new();
        let mut d = empty_drain();
        d.dialogs.push(SocialDialog {
            code: DIALOG_GROUP_INVITE,
            data1: 3,
            data2: 0,
            data3: 0,
            data4: 0,
            message: "Feile has invited you to join a group".into(),
        });
        pc.observe_drain(&d, now);
        assert!(pc.social.pending.is_some());
        assert!(
            pc.wm.is_open(PRODUCT_DIALOG_WINDOW),
            "falsifier: social.observe bypass leaves product_dialog closed"
        );
        let mut bypass = ProductController::new();
        bypass.social.observe(&invite(), now);
        assert!(
            !bypass.wm.is_open(PRODUCT_DIALOG_WINDOW),
            "control: direct social.observe must not open the controller window"
        );
    }

    /// Falsifier: miss / no hit-test must not emit a social command.
    #[test]
    fn pointer_miss_does_not_emit_command() {
        let now = Instant::now();
        let hud = stub_hud();
        let mut pc = ProductController::new();
        pc.observe(&invite(), now);
        let d = pc.dispatch(
            ProductInput::PointerPress { x: 0.0, y: 0.0 },
            hud.skin(),
            ProductDispatchCtx::default(),
        );
        assert!(!d.consumed);
        assert!(d.command.is_none());
        assert!(!pc.social.in_group);
    }

    /// Falsifier: Dialog Yes is intent, not membership; roster event is.
    #[test]
    fn group_accept_pointer_waits_for_roster() {
        let now = Instant::now();
        let hud = stub_hud();
        let mut pc = ProductController::new();
        let world = WorldState::new();
        let status = PlayerStatus::default();
        pc.observe(&invite(), now);
        let _ = pc.draw(
            hud.skin(),
            &idle_state(&status, &world),
            &crate::adapters::ExtraBind::default(),
            None,
        );
        let d = pc.dispatch(
            ProductInput::PointerPress { x: 380.0, y: 280.0 },
            hud.skin(),
            ProductDispatchCtx::default(),
        );
        assert!(matches!(
            d.command,
            Some(LiveCommand::DialogResponse {
                response: DIALOG_YES,
                ..
            })
        ));
        assert!(pc.social.last_outbound.is_some());
        assert!(!pc.social.in_group);
        pc.observe(&roster(), now);
        product_ui::sync_critical_windows(
            &mut pc.wm,
            &hud,
            crate::skinhud::CriticalHudBind {
                group_active: pc.social.in_group,
                ..Default::default()
            },
        );
        assert!(pc.social.in_group);
        let draw = pc.draw(
            hud.skin(),
            &idle_state(&status, &world),
            &crate::adapters::ExtraBind::default(),
            None,
        );
        assert!(
            bound_adapter_text(&draw.texts, "group_name0").is_some_and(|t| t.contains("Feile")),
            "roster must populate group_name0: {:?}",
            draw.texts
                .iter()
                .map(|t| (&t.adapter, &t.text))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn invite_requires_target_and_encodes_0x87() {
        let hud = stub_hud();
        let mut pc = ProductController::new();
        let miss = pc.dispatch(
            ProductInput::InviteToGroup,
            hud.skin(),
            ProductDispatchCtx {
                chat_open: false,
                has_target: false,
            },
        );
        assert!(miss.command.is_none());
        let hit = pc.dispatch(
            ProductInput::PointerPress { x: 340.0, y: 70.0 },
            hud.skin(),
            ProductDispatchCtx {
                chat_open: false,
                has_target: true,
            },
        );
        assert!(matches!(hit.command, Some(LiveCommand::InviteToGroup)));
        let (code, body) = pc.social.last_outbound.as_ref().unwrap();
        assert_eq!(*code, caer_protocol::codes::client::InviteToGroup);
        assert!(body.is_empty());
        assert!(!pc.social.in_group, "invite send is not membership");
    }

    #[test]
    fn trade_complete_only_after_accept_then_closed_window() {
        let now = Instant::now();
        let hud = stub_hud();
        let mut pc = ProductController::new();
        let world = WorldState::new();
        let status = PlayerStatus::default();
        pc.observe(&open_trade(), now);
        let _ = pc.draw(
            hud.skin(),
            &idle_state(&status, &world),
            &crate::adapters::ExtraBind::default(),
            None,
        );
        let d = pc.dispatch(
            ProductInput::PointerPress { x: 380.0, y: 400.0 },
            hud.skin(),
            ProductDispatchCtx::default(),
        );
        assert!(matches!(
            d.command,
            Some(LiveCommand::ModifyTrade {
                action: ModifyTradeAction::Accept,
                ..
            })
        ));
        assert!(!pc.social.trade_complete);
        pc.observe(&closed_trade(), now);
        assert!(pc.social.trade_complete);
        assert!(!pc.snapshot().trade_open);
    }

    #[test]
    fn quest_refuse_and_stale_index_leave_no_accepted() {
        let now = Instant::now();
        let hud = stub_hud();
        let mut pc = ProductController::new();
        let world = WorldState::new();
        let status = PlayerStatus::default();
        pc.observe(&quest_offer(1), now);
        let _ = pc.draw(
            hud.skin(),
            &idle_state(&status, &world),
            &crate::adapters::ExtraBind::default(),
            None,
        );
        let refuse = pc.dispatch(
            ProductInput::SocialRefuse,
            hud.skin(),
            ProductDispatchCtx::default(),
        );
        assert!(matches!(
            refuse.command,
            Some(LiveCommand::DialogResponse { response: 0, .. })
        ));
        pc.observe(&quest_entry(1, "Important Delivery (Level 1)"), now);
        assert!(
            !pc.social.quest_accepted,
            "refuse + later matching index must not accept"
        );
    }

    #[test]
    fn timeout_reconnect_leave_no_ghost() {
        let now = Instant::now();
        let mut pc = ProductController::new();
        pc.observe(&invite(), now);
        pc.sync_windows(true);
        assert!(pc.snapshot().dialog_open);
        assert!(pc.snapshot().invite_open);
        let later = now + Duration::from_secs(61);
        assert!(pc.tick_timeouts(later));
        assert!(!pc.snapshot().dialog_open);
        assert!(!pc.social.in_group);
        pc.observe(&invite(), now);
        pc.observe(&open_trade(), now);
        pc.reconnect_reset();
        let snap = pc.snapshot();
        assert!(!snap.dialog_open && !snap.trade_open && !snap.invite_open);
        assert!(!snap.in_group && !snap.trade_complete && !snap.quest_accepted);
    }

    #[test]
    fn sys6_accounting_is_zero_player_scenario() {
        let acc = sys6_accounting();
        assert_eq!(SYS6_PLAYER_SCENARIO_EARNED, 0);
        assert_eq!(SYS6_PLAYER_SCENARIO_DENOM, 3);
        assert!(!acc.group.player_scenario);
        assert!(!acc.trade.player_scenario);
        assert!(!acc.quest.player_scenario);
        assert_eq!(acc.group.proof_class, PRODUCT_LOOP_PROOF_CLASS);
        assert_ne!(PRODUCT_LOOP_PROOF_CLASS, "PLAYER_SCENARIO");
    }

    /// Falsifier B1: toggle closed + intervening sync/draw with data still present stays closed.
    /// Inventory and Stats remain independently toggleable; group/quest data is not destroyed.
    #[test]
    fn falsifier_player_closed_panels_stay_closed_through_sync() {
        let hud = stub_hud();
        let mut pc = ProductController::new();
        let now = Instant::now();
        pc.observe(&roster(), now);
        pc.observe(&quest_entry(1, "Important Delivery"), now);
        assert!(pc.social.in_group, "group data must exist before toggles");
        assert!(
            !pc.social.quests.is_empty(),
            "quest data must exist before toggles"
        );

        let bind = crate::skinhud::CriticalHudBind {
            inventory_open: true,
            group_active: true,
            quest_open: true,
            skills_open: true,
            ..Default::default()
        };
        pc.sync_hud(&hud, bind);
        assert!(
            pc.wm.is_open("stats_index"),
            "inventory eligibility may auto-open"
        );
        assert!(
            !pc.wm.is_open("stats_attributes"),
            "stats must not auto-open from inventory data"
        );

        let panels = [
            (ProductInput::ToggleInventory, "stats_index"),
            (ProductInput::ToggleStats, "stats_attributes"),
            (ProductInput::ToggleGroup, "mini_group"),
            (ProductInput::ToggleMap, "map_window"),
            (ProductInput::ToggleQuest, "new_quest_journal"),
            (ProductInput::ToggleSkills, "stats_spec_abil"),
        ];
        let status = PlayerStatus::default();
        let world = WorldState::new();
        for (input, name) in panels {
            if !pc.wm.is_open(name) {
                let _ = pc.dispatch(input, hud.skin(), ProductDispatchCtx::default());
                assert!(pc.wm.is_open(name), "{name} must open on toggle");
            }
            let _ = pc.dispatch(input, hud.skin(), ProductDispatchCtx::default());
            assert!(!pc.wm.is_open(name), "{name} must close on toggle");
            pc.sync_hud(&hud, bind);
            let _ = pc.draw(
                hud.skin(),
                &idle_state(&status, &world),
                &crate::adapters::ExtraBind::default(),
                None,
            );
            assert!(
                !pc.wm.is_open(name),
                "{name} must stay closed after sync/draw with data still present"
            );
        }

        assert!(
            pc.social.in_group,
            "closing the group panel must not destroy group membership"
        );
        assert!(
            !pc.social.quests.is_empty(),
            "closing the quest panel must not destroy quest data"
        );

        let _ = pc.dispatch(
            ProductInput::ToggleStats,
            hud.skin(),
            ProductDispatchCtx::default(),
        );
        assert!(pc.wm.is_open("stats_attributes"));
        assert!(!pc.wm.is_open("stats_index"));
        pc.sync_hud(&hud, bind);
        assert!(
            pc.wm.is_open("stats_attributes"),
            "stats must stay open without inventory"
        );
        assert!(
            !pc.wm.is_open("stats_index"),
            "inventory must stay closed independently of stats"
        );
    }

    fn close_gadget_xy(wm: &WindowManager, skin: &Skin, name: &str) -> (f32, f32) {
        let w = wm
            .visible()
            .find(|w| w.name == name)
            .unwrap_or_else(|| panic!("{name} must be visible"));
        let t = skin.windows.get(name).expect("skin template");
        let size = t.title_height.max(12) as f32;
        let x = w.pos.0 + t.width as f32 - size / 2.0;
        let y = w.pos.1 + size / 2.0;
        (x, y)
    }

    /// Falsifier B1 residual: pointer close gadget records player_closed; sync cannot reopen.
    #[test]
    fn falsifier_pointer_close_stays_closed_through_sync() {
        let hud = stub_hud();
        let mut pc = ProductController::new();
        let now = Instant::now();
        pc.observe(&roster(), now);
        pc.observe(&quest_entry(1, "Important Delivery"), now);
        // Open alternate group skin name (not mini_group) so bookkeeping must use the exact name.
        pc.wm.open("new_group_window", (590.0, 519.0));
        pc.wm.open("stats_index", (200.0, 80.0));
        pc.wm.open("new_quest_journal", (40.0, 200.0));
        assert!(pc.wm.is_open("new_group_window"));
        assert!(!pc.wm.is_open("mini_group"));

        let bind = crate::skinhud::CriticalHudBind {
            inventory_open: true,
            group_active: true,
            quest_open: true,
            skills_open: true,
            ..Default::default()
        };
        let status = PlayerStatus::default();
        let world = WorldState::new();
        for name in ["stats_index", "new_group_window", "new_quest_journal"] {
            let (x, y) = close_gadget_xy(&pc.wm, hud.skin(), name);
            let d = pc.dispatch(
                ProductInput::PointerPress { x, y },
                hud.skin(),
                ProductDispatchCtx::default(),
            );
            assert!(d.consumed, "{name} close gadget must be consumed");
            assert!(!pc.wm.is_open(name), "{name} must close via pointer");
            assert!(
                pc.player_closed().contains(name),
                "controller must record exact closed name {name}"
            );
            pc.sync_hud(&hud, bind);
            let _ = pc.draw(
                hud.skin(),
                &idle_state(&status, &world),
                &crate::adapters::ExtraBind::default(),
                None,
            );
            assert!(
                !pc.wm.is_open(name),
                "{name} must stay closed after eligible sync/draw"
            );
            if name == "new_group_window" {
                assert!(
                    !pc.wm.is_open("mini_group"),
                    "closing an alternate group name must not let sync reopen mini_group"
                );
            }
        }
        assert!(pc.social.in_group);
        assert!(!pc.social.quests.is_empty());
    }

    /// Falsifier B1 residual: Escape close records player_closed; sync cannot reopen.
    #[test]
    fn falsifier_escape_close_stays_closed_through_sync() {
        let hud = stub_hud();
        let mut pc = ProductController::new();
        let now = Instant::now();
        pc.observe(&roster(), now);
        pc.wm.open("new_group_window", (590.0, 519.0));
        // Focus the alternate group window so Escape closes that exact name.
        pc.wm.raise("new_group_window");
        // WindowManager focus is set by raise? raise doesn't set focus — open does.
        // Re-open to set focus.
        pc.wm.open("new_group_window", (590.0, 519.0));
        assert_eq!(pc.wm.keyboard_focus(), Some("new_group_window"));

        let bind = crate::skinhud::CriticalHudBind {
            group_active: true,
            inventory_open: true,
            ..Default::default()
        };
        let _ = pc.dispatch(
            ProductInput::SocialEscape,
            hud.skin(),
            ProductDispatchCtx::default(),
        );
        assert!(!pc.wm.is_open("new_group_window"));
        assert!(pc.player_closed().contains("new_group_window"));
        pc.sync_hud(&hud, bind);
        let status = PlayerStatus::default();
        let world = WorldState::new();
        let _ = pc.draw(
            hud.skin(),
            &idle_state(&status, &world),
            &crate::adapters::ExtraBind::default(),
            None,
        );
        assert!(!pc.wm.is_open("new_group_window"));
        assert!(
            !pc.wm.is_open("mini_group"),
            "Escape on alternate must suppress the whole group class"
        );
        assert!(
            pc.social.in_group,
            "Escape close must not destroy group membership"
        );
    }
}
