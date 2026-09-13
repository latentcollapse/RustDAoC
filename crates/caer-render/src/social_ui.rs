//! Player-facing social UI state: group invite, trade window, quest/dialog offers.
//!
//! Protocol foundation already lives in `caer-protocol` / headless `*_rt` examples
//! ([`LIVE_PROTOCOL_RT`](crate)). This module is the **product input seam**: server events become
//! visible pending state; player Accept/Refuse/Cancel becomes a typed [`crate::live::LiveCommand`].
//!
//! ## Cleanup contract
//! Pending invite/dialog and open trade clear on: explicit cancel/response, timeout, logout, or
//! region change. Unrelated keys and an open chat box must not trigger social actions.
//!
//! ## Lane A harness
//! Product-input unit tests here are self-contained. Live dual-client RT examples still use the
//! local typed command seam; integrating Lane A's shared harness (when merged) should replace
//! duplicated wait/drain helpers without inventing a second harness library.

use std::time::{Duration, Instant};

use caer_protocol::quest::QuestEntry;
use caer_protocol::session::ServerEvent;
use caer_protocol::social::{
    GroupMemberUpdate, GroupWindow, ModifyTradeAction, TradeMoney, TradeWindow,
};

use crate::live::LiveCommand;
use crate::skinhud::{PRODUCT_DIALOG_WINDOW, PRODUCT_TRADE_WINDOW};
use crate::skinui::{UiText, WindowManager};

/// Dialog Yes (`DialogResponse.response = 0x01`).
pub const DIALOG_YES: u8 = 0x01;
/// Dialog No / decline.
pub const DIALOG_NO: u8 = 0x00;
/// Group-invite dialog code (`eDialogCode`).
pub const DIALOG_GROUP_INVITE: u8 = 0x05;
/// How long a pending invite/dialog stays actionable without player input.
pub const PENDING_TIMEOUT: Duration = Duration::from_secs(60);
/// Local trade overlay timeout. Does not invent TradeWindow-closed or `trade_complete`.
pub const TRADE_TIMEOUT: Duration = Duration::from_secs(60);

/// Kind of pending Yes/No dialog the player can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    GroupInvite,
    QuestOrDialog,
}

impl PendingKind {
    #[must_use]
    pub fn from_dialog_code(code: u8) -> Self {
        if code == DIALOG_GROUP_INVITE {
            Self::GroupInvite
        } else {
            Self::QuestOrDialog
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::GroupInvite => "Group Invite",
            Self::QuestOrDialog => "Dialog",
        }
    }
}

/// One actionable server Dialog awaiting player response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDialog {
    pub kind: PendingKind,
    pub code: u8,
    pub data1: u16,
    pub data2: u16,
    pub data3: u16,
    pub data4: u16,
    pub message: String,
    pub offered_at: Instant,
}

impl PendingDialog {
    #[must_use]
    pub fn from_event(
        code: u8,
        data1: u16,
        data2: u16,
        data3: u16,
        data4: u16,
        message: String,
        now: Instant,
    ) -> Self {
        Self {
            kind: PendingKind::from_dialog_code(code),
            code,
            data1,
            data2,
            data3,
            data4,
            message,
            offered_at: now,
        }
    }

    #[must_use]
    pub fn timed_out(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.offered_at) >= PENDING_TIMEOUT
    }

    /// Build the typed accept/refuse command (exactly one DialogResponse path).
    #[must_use]
    pub fn response_command(&self, accept: bool) -> LiveCommand {
        LiveCommand::DialogResponse {
            data1: self.data1,
            data2: self.data2,
            data3: self.data3,
            message_type: self.code,
            response: if accept { DIALOG_YES } else { DIALOG_NO },
        }
    }
}

/// Open trade window presentation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeUi {
    pub window: TradeWindow,
    pub opened_at: Instant,
    /// Incremented whenever server terms change.
    pub terms_generation: u64,
    /// Set when the current generation has been rendered (or noted by tests).
    pub shown_generation: u64,
}

fn money_label(m: &TradeMoney) -> String {
    format!(
        "{}m {}p {}g {}s {}c",
        m.mithril, m.platinum, m.gold, m.silver, m.copper
    )
}

impl TradeUi {
    /// Partner identity + both money offers + own slots + partner item count.
    #[must_use]
    pub fn terms_text(&self) -> String {
        let w = &self.window;
        let partner = if w.caption.is_empty() {
            "(unknown partner)"
        } else {
            w.caption.as_str()
        };
        let own_slots: Vec<String> = w
            .own_slots
            .iter()
            .filter(|s| **s != 0)
            .map(|s| format!("slot {s} (unidentified)"))
            .collect();
        let own_line = if own_slots.is_empty() {
            format!("You offer: 0 slot(s), {}", money_label(&w.own_money))
        } else {
            format!(
                "You offer: {} — identities not in snapshot, {}",
                own_slots.join(", "),
                money_label(&w.own_money)
            )
        };
        format!(
            "Partner: {partner}\n{own_line}\nThey offer: {} item(s), {}\nYou accepted: no  They accepted: unknown",
            w.partner_item_count,
            money_label(&w.partner_money),
        )
    }

    /// Complete enough to accept: identified partner, current terms shown, no unidentified items
    /// on either side (inventory-bound identities are not in the 0xEA snapshot).
    #[must_use]
    pub fn accept_enabled(&self) -> bool {
        let w = &self.window;
        !w.closed
            && !w.caption.is_empty()
            && self.shown_generation == self.terms_generation
            && w.partner_item_count == 0
            && w.own_slots.iter().all(|s| *s == 0)
    }

    pub fn note_terms_rendered(&mut self) {
        self.shown_generation = self.terms_generation;
    }

    #[must_use]
    pub fn accept_command(&self) -> LiveCommand {
        LiveCommand::ModifyTrade {
            action: ModifyTradeAction::Accept,
            repair: false,
            combine: false,
            slots: [0; 10],
            money: TradeMoney::default(),
        }
    }

    #[must_use]
    pub fn cancel_command(&self) -> LiveCommand {
        LiveCommand::ModifyTrade {
            action: ModifyTradeAction::Cancel,
            repair: false,
            combine: false,
            slots: [0; 10],
            money: TradeMoney::default(),
        }
    }
}

/// Player social overlay state (invite / trade / dialog).
#[derive(Debug, Default)]
pub struct SocialUiState {
    pub pending: Option<PendingDialog>,
    pub trade: Option<TradeUi>,
    /// Last command emitted from this UI (tests / observability).
    pub last_command: Option<LiveCommand>,
    /// Authoritative named roster. Dialog Yes never writes this.
    pub group: Option<GroupWindow>,
    pub group_vitals: Option<GroupMemberUpdate>,
    pub quests: Vec<QuestEntry>,
    /// True only after GroupWindow members or GroupMemberUpdate vitals arrive.
    pub in_group: bool,
    /// True only after a closed TradeWindow following a sent trade Accept (not any money/inv packet).
    pub trade_complete: bool,
    /// True only after a non-clear QuestEntry 0x83 **for the pending dialog's quest index**.
    pub quest_accepted: bool,
    awaiting_trade_confirm: bool,
    /// Pending dialog's quest index (data1) when the dialog is a quest offer.
    pending_quest_index: Option<u16>,
    /// Set only after stock layout actually produced the pending dialog_message text.
    dialog_shown: bool,
    /// Last encoded outbound social packet (falsifier: deleting encode makes product tests red).
    pub last_outbound: Option<(u8, Vec<u8>)>,
    /// Last skin hit `(window, control)` from [`Self::handle_product_pointer`].
    pub last_hit: Option<(String, String)>,
}

/// Product-loop pointer result for INT (`rustdaoc` must dispatch here, not call handlers raw).
#[derive(Debug, Clone, Default)]
pub struct ProductPointerResult {
    pub consumed: bool,
    pub command: Option<LiveCommand>,
    pub hit: Option<(String, String)>,
    /// Exact skin window closed by a close gadget (controller records player visibility).
    pub closed_window: Option<String>,
}

/// Product-loop key result — Escape may close a focused ordinary panel by exact name.
#[derive(Debug, Clone, Default)]
pub struct ProductKeyResult {
    pub command: Option<LiveCommand>,
    pub closed_window: Option<String>,
}

/// Product keys that may resolve social UI (not raw handler invocation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductKey {
    Accept,
    Refuse,
    Escape,
}

/// Player input that can resolve social UI (distinct typed paths).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocialInput {
    Accept,
    Refuse,
    /// Trade-only cancel (distinct from dialog refuse).
    CancelTrade,
}

impl SocialUiState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn has_overlay(&self) -> bool {
        self.pending.is_some() || self.trade.as_ref().is_some_and(|t| !t.window.closed)
    }

    /// Fold a decoded server event into UI state.
    pub fn observe(&mut self, ev: &ServerEvent, now: Instant) {
        match ev {
            ServerEvent::Dialog {
                code,
                data1,
                data2,
                data3,
                data4,
                message,
            } => {
                let pending = PendingDialog::from_event(
                    *code,
                    *data1,
                    *data2,
                    *data3,
                    *data4,
                    message.clone(),
                    now,
                );
                self.dialog_shown = false;
                self.pending_quest_index = match pending.kind {
                    PendingKind::QuestOrDialog => Some(pending.data1),
                    PendingKind::GroupInvite => None,
                };
                self.pending = Some(pending);
            }
            ServerEvent::TradeWindow(tw) => {
                if tw.closed {
                    if self.awaiting_trade_confirm {
                        self.trade_complete = true;
                        self.awaiting_trade_confirm = false;
                    }
                    self.trade = None;
                } else if let Some(existing) = self.trade.as_mut() {
                    let changed = existing.window != *tw;
                    existing.window = tw.clone();
                    if changed {
                        existing.terms_generation = existing.terms_generation.saturating_add(1);
                        // Stale Accept: terms moved under us — close is no longer that Accept.
                        self.awaiting_trade_confirm = false;
                        self.trade_complete = false;
                    }
                } else {
                    self.awaiting_trade_confirm = false;
                    self.trade_complete = false;
                    self.trade = Some(TradeUi {
                        window: tw.clone(),
                        opened_at: now,
                        terms_generation: 1,
                        shown_generation: 0,
                    });
                }
            }
            ServerEvent::GroupWindow(gw) => {
                if gw.is_empty() {
                    self.group = None;
                    self.in_group = self
                        .group_vitals
                        .as_ref()
                        .is_some_and(|v| !v.members.is_empty());
                } else {
                    self.group = Some(gw.clone());
                    self.in_group = true;
                }
            }
            ServerEvent::GroupMemberUpdate(g) => {
                self.group_vitals = Some(g.clone());
                if !g.members.is_empty() {
                    self.in_group = true;
                } else if self.group.as_ref().is_none_or(|gw| gw.is_empty()) {
                    self.in_group = false;
                }
            }
            ServerEvent::QuestEntry { entry, .. } => {
                if entry.is_clear() {
                    self.quests.retain(|q| q.index != entry.index);
                    if self.pending_quest_index == Some(u16::from(entry.index)) {
                        self.quest_accepted = false;
                    }
                } else {
                    if let Some(existing) = self.quests.iter_mut().find(|q| q.index == entry.index)
                    {
                        *existing = entry.clone();
                    } else {
                        self.quests.push(entry.clone());
                    }
                    if self.pending_quest_index == Some(u16::from(entry.index)) {
                        self.quest_accepted = true;
                    }
                }
            }
            ServerEvent::LoggedOut { .. } => self.clear_all(),
            ServerEvent::RegionChanged(_) | ServerEvent::RegionHandoff { .. } => {
                self.clear_all();
            }
            _ => {}
        }
    }

    /// Drop pending/trade/presentation without sending (timeout / logout / region / reconnect).
    pub fn clear_all(&mut self) {
        self.pending = None;
        self.trade = None;
        self.group = None;
        self.group_vitals = None;
        self.quests.clear();
        self.in_group = false;
        self.trade_complete = false;
        self.quest_accepted = false;
        self.awaiting_trade_confirm = false;
        self.pending_quest_index = None;
        self.dialog_shown = false;
    }

    /// Reconnect / session rebuild: no ghost windows or success presentation.
    pub fn reconnect_reset(&mut self, wm: &mut WindowManager) {
        self.clear_all();
        self.last_command = None;
        self.last_outbound = None;
        self.last_hit = None;
        wm.close(PRODUCT_DIALOG_WINDOW);
        wm.close(PRODUCT_TRADE_WINDOW);
    }

    /// Expire timed-out pending dialogs / local trade overlay.
    /// Timeout never sets `trade_complete` / `quest_accepted` / `in_group`.
    pub fn tick_timeouts(&mut self, now: Instant) -> bool {
        let mut cleared = false;
        if self.pending.as_ref().is_some_and(|p| p.timed_out(now)) {
            self.pending = None;
            self.pending_quest_index = None;
            self.dialog_shown = false;
            cleared = true;
        }
        if let Some(trade) = self.trade.as_ref() {
            if now.saturating_duration_since(trade.opened_at) >= TRADE_TIMEOUT {
                self.trade = None;
                self.awaiting_trade_confirm = false;
                cleared = true;
            }
        }
        cleared
    }

    /// Product-input seam: chat-open or no overlay ⇒ no social command.
    ///
    /// Accept/Refuse map to dialog response when a dialog is pending; when only trade is open,
    /// Accept → ModifyTrade::Accept and Refuse/CancelTrade → ModifyTrade::Cancel.
    pub fn handle_input(&mut self, input: SocialInput, chat_open: bool) -> Option<LiveCommand> {
        if chat_open {
            return None;
        }
        let cmd = match input {
            SocialInput::Accept => {
                if let Some(p) = self.pending.take() {
                    Some(p.response_command(true))
                } else if self.trade.as_ref().is_some_and(|t| t.accept_enabled()) {
                    self.trade.as_ref().map(|t| t.accept_command())
                } else {
                    None
                }
            }
            SocialInput::Refuse => {
                if let Some(p) = self.pending.take() {
                    self.pending_quest_index = None;
                    Some(p.response_command(false))
                } else if self.trade.is_some() {
                    let cmd = self.trade.as_ref().map(|t| t.cancel_command());
                    self.trade = None;
                    self.awaiting_trade_confirm = false;
                    self.trade_complete = false;
                    cmd
                } else {
                    None
                }
            }
            SocialInput::CancelTrade => {
                if self.trade.is_some() {
                    let cmd = self.trade.as_ref().map(|t| t.cancel_command());
                    self.trade = None;
                    self.awaiting_trade_confirm = false;
                    self.trade_complete = false;
                    cmd
                } else {
                    None
                }
            }
        };
        if let Some(ref c) = cmd {
            self.last_command = Some(c.clone());
            self.last_outbound = encode_social_command(c);
            if matches!(
                c,
                LiveCommand::ModifyTrade {
                    action: ModifyTradeAction::Accept,
                    ..
                }
            ) {
                self.awaiting_trade_confirm = true;
                self.trade_complete = false;
            }
        }
        cmd
    }

    /// Open/close product overlay windows from pending/trade state. INT calls this each frame
    /// before pointer dispatch. Does not invent membership/completion.
    pub fn sync_product_windows(&self, wm: &mut WindowManager) {
        if self.pending.is_some() {
            wm.open(PRODUCT_DIALOG_WINDOW, (360.0, 200.0));
        } else {
            wm.close(PRODUCT_DIALOG_WINDOW);
        }
        if self.trade.as_ref().is_some_and(|t| !t.window.closed) {
            wm.open(PRODUCT_TRADE_WINDOW, (360.0, 280.0));
        } else {
            wm.close(PRODUCT_TRADE_WINDOW);
        }
    }

    /// Mark dialog/trade as shown only when stock layout emitted the live adapter text.
    pub fn note_stock_layout(&mut self, texts: &[UiText]) {
        if let Some(p) = self.pending.as_ref() {
            if !p.message.is_empty()
                && texts
                    .iter()
                    .any(|t| t.adapter.as_deref() == Some("dialog_message") && t.text == p.message)
            {
                self.dialog_shown = true;
            }
        }
        if let Some(trade) = self.trade.as_mut() {
            let terms = trade.terms_text();
            if !terms.is_empty()
                && texts
                    .iter()
                    .any(|t| t.adapter.as_deref() == Some("trade_terms") && t.text == terms)
            {
                trade.note_terms_rendered();
            }
        }
    }

    #[must_use]
    pub fn dialog_was_shown(&self) -> bool {
        self.dialog_shown
    }

    /// Product pointer: skin hit-testing → typed command. A miss or a raw handler call is not
    /// this path. Local Accept never flips [`Self::in_group`] / trade_complete / quest_accepted.
    pub fn handle_product_pointer(
        &mut self,
        wm: &mut WindowManager,
        skin: &caer_assets::uiskin::Skin,
        x: f32,
        y: f32,
        chat_open: bool,
    ) -> ProductPointerResult {
        self.sync_product_windows(wm);
        let hit = wm
            .hit_control(skin, x, y)
            .map(|(w, c)| (w.to_string(), c.to_string()));
        let accept_blind = hit
            .as_ref()
            .is_some_and(|(_, id)| social_input_from_control(id) == Some(SocialInput::Accept))
            && self.pending.is_some()
            && !self.dialog_shown;
        self.last_hit = hit.clone();
        let consumed = wm.on_press(skin, x, y);
        if !consumed {
            return ProductPointerResult {
                consumed: false,
                command: None,
                hit,
                closed_window: None,
            };
        }
        if let Some(closed) = wm.hit_close(skin, x, y) {
            let name = closed.to_string();
            wm.close(&name);
            let command = if name == PRODUCT_DIALOG_WINDOW {
                self.handle_input(SocialInput::Refuse, chat_open)
            } else if name == PRODUCT_TRADE_WINDOW {
                self.handle_input(SocialInput::CancelTrade, chat_open)
            } else {
                None
            };
            self.sync_product_windows(wm);
            return ProductPointerResult {
                consumed: true,
                command,
                hit,
                closed_window: Some(name),
            };
        }
        let input = hit
            .as_ref()
            .and_then(|(_, id)| social_input_from_control(id));
        let command = if chat_open || accept_blind {
            None
        } else {
            input.and_then(|i| self.handle_input(i, false))
        };
        self.sync_product_windows(wm);
        ProductPointerResult {
            consumed: true,
            command,
            hit,
            closed_window: None,
        }
    }

    /// Product keyboard: Y/N/Escape through the same typed commands as pointer Accept/Refuse.
    pub fn handle_product_key(
        &mut self,
        wm: &mut WindowManager,
        key: ProductKey,
        chat_open: bool,
    ) -> ProductKeyResult {
        self.sync_product_windows(wm);
        let mut closed_window = None;
        let command = match key {
            ProductKey::Accept if self.pending.is_some() && !self.dialog_shown => None,
            ProductKey::Accept => self.handle_input(SocialInput::Accept, chat_open),
            ProductKey::Refuse => self.handle_input(SocialInput::Refuse, chat_open),
            ProductKey::Escape => {
                closed_window = wm.on_escape();
                if self.pending.is_some() {
                    self.handle_input(SocialInput::Refuse, chat_open)
                } else if self.trade.is_some() {
                    self.handle_input(SocialInput::CancelTrade, chat_open)
                } else {
                    None
                }
            }
        };
        self.sync_product_windows(wm);
        ProductKeyResult {
            command,
            closed_window,
        }
    }

    /// egui overlay: pending invite/dialog + open trade. Returns clicked action if any.
    ///
    /// DEV-ONLY. Stock UI parity is the Atlantis/Ghost skin path (`handle_product_pointer`).
    pub fn draw(&mut self, root: &mut egui::Ui) -> Option<SocialInput> {
        let mut clicked = None;
        let ctx = root.ctx().clone();

        if let Some(p) = self.pending.as_ref() {
            let title = p.kind.label();
            let msg = p.message.clone();
            egui::Area::new(egui::Id::new("social_pending_dialog"))
                .anchor(egui::Align2::CENTER_CENTER, [0.0, -40.0])
                .show(&ctx, |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_premultiplied(18, 18, 22, 235))
                        .stroke(egui::Stroke::new(
                            1.0,
                            egui::Color32::from_rgb(180, 160, 90),
                        ))
                        .corner_radius(4)
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.set_min_width(360.0);
                            ui.label(
                                egui::RichText::new(title)
                                    .color(egui::Color32::from_rgb(220, 200, 140))
                                    .size(14.0),
                            );
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(msg).size(12.0));
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                if ui.button("Accept (Y)").clicked() {
                                    clicked = Some(SocialInput::Accept);
                                }
                                if ui.button("Refuse (N)").clicked() {
                                    clicked = Some(SocialInput::Refuse);
                                }
                            });
                        });
                });
        }

        if let Some(t) = self.trade.as_mut() {
            if !t.window.closed {
                egui::Area::new(egui::Id::new("social_trade_window"))
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 80.0])
                    .show(&ctx, |ui| {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_premultiplied(16, 20, 18, 235))
                            .stroke(egui::Stroke::new(
                                1.0,
                                egui::Color32::from_rgb(90, 140, 110),
                            ))
                            .corner_radius(4)
                            .inner_margin(12.0)
                            .show(ui, |ui| {
                                ui.set_min_width(320.0);
                                ui.label(
                                    egui::RichText::new("Trade")
                                        .color(egui::Color32::from_rgb(160, 210, 170))
                                        .size(14.0),
                                );
                                ui.label(
                                    egui::RichText::new(t.terms_text()).size(12.0),
                                );
                                if t.window.partner_item_count > 0 {
                                    ui.label(
                                        egui::RichText::new(
                                            "Accept disabled: partner item identities not in snapshot",
                                        )
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(220, 140, 90)),
                                    );
                                }
                                ui.add_space(8.0);
                                ui.horizontal(|ui| {
                                    let can_accept = t.accept_enabled();
                                    ui.add_enabled_ui(can_accept, |ui| {
                                        if ui.button("Accept Trade").clicked() {
                                            clicked = Some(SocialInput::Accept);
                                        }
                                    });
                                    if ui.button("Cancel Trade").clicked() {
                                        clicked = Some(SocialInput::CancelTrade);
                                    }
                                });
                            });
                    });
                t.note_terms_rendered();
            }
        }

        clicked
    }
}

fn social_input_from_control(id: &str) -> Option<SocialInput> {
    match id.to_ascii_lowercase().as_str() {
        "accept" | "yes" | "ok" => Some(SocialInput::Accept),
        "refuse" | "no" | "decline" => Some(SocialInput::Refuse),
        "cancel" => Some(SocialInput::CancelTrade),
        _ => None,
    }
}

/// Encode the wire body a [`LiveCommand`] social variant would send (for falsifier tests).
#[must_use]
pub fn encode_social_command(cmd: &LiveCommand) -> Option<(u8, Vec<u8>)> {
    match cmd {
        LiveCommand::DialogResponse {
            data1,
            data2,
            data3,
            message_type,
            response,
        } => Some((
            caer_protocol::codes::client::DialogResponse,
            caer_protocol::invverb::encode_dialog_response(
                *data1,
                *data2,
                *data3,
                *message_type,
                *response,
            ),
        )),
        LiveCommand::ModifyTrade {
            action,
            repair,
            combine,
            slots,
            money,
        } => Some((
            caer_protocol::codes::client::ModifyTrade,
            caer_protocol::social::encode_modify_trade(*action, *repair, *combine, slots, *money),
        )),
        LiveCommand::InviteToGroup => Some((
            caer_protocol::codes::client::InviteToGroup,
            caer_protocol::social::encode_invite_to_group(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use caer_protocol::social::TradeMoney;

    fn invite_event() -> ServerEvent {
        ServerEvent::Dialog {
            code: DIALOG_GROUP_INVITE,
            data1: 3,
            data2: 0,
            data3: 0,
            data4: 0,
            message: "Feile has invited you to join a group".into(),
        }
    }

    fn quest_event() -> ServerEvent {
        ServerEvent::Dialog {
            code: 0x64,
            data1: 1,
            data2: 2,
            data3: 0,
            data4: 0,
            message: "Accept Important Delivery?".into(),
        }
    }

    fn open_trade() -> ServerEvent {
        trade_offer("Feile", TradeMoney::default(), 0)
    }

    fn trade_offer(
        caption: &str,
        partner_money: TradeMoney,
        partner_item_count: u8,
    ) -> ServerEvent {
        ServerEvent::TradeWindow(TradeWindow {
            closed: false,
            own_slots: [0; 10],
            own_money: TradeMoney::default(),
            partner_money,
            partner_item_count,
            repairing: false,
            combining: false,
            caption: caption.into(),
        })
    }

    /// Accept and refuse take distinct typed paths and encode the intended packet once.
    #[test]
    fn accept_refuse_cancel_are_distinct_typed_packets() {
        let now = Instant::now();
        let mut ui = SocialUiState::new();
        ui.observe(&invite_event(), now);
        let accept = ui.handle_input(SocialInput::Accept, false).unwrap();
        let (code, body) = encode_social_command(&accept).unwrap();
        assert_eq!(code, caer_protocol::codes::client::DialogResponse);
        assert_eq!(body, &[0x00, 0x03, 0, 0, 0, 0, 0x05, 0x01]);

        ui.observe(&invite_event(), now);
        let refuse = ui.handle_input(SocialInput::Refuse, false).unwrap();
        let (code, body) = encode_social_command(&refuse).unwrap();
        assert_eq!(code, caer_protocol::codes::client::DialogResponse);
        assert_eq!(body, &[0x00, 0x03, 0, 0, 0, 0, 0x05, 0x00]);
        assert_ne!(
            encode_social_command(&accept),
            encode_social_command(&refuse)
        );

        ui.observe(&open_trade(), now);
        ui.trade.as_mut().unwrap().note_terms_rendered();
        let trade_accept = ui.handle_input(SocialInput::Accept, false).unwrap();
        let (code, body) = encode_social_command(&trade_accept).unwrap();
        assert_eq!(code, caer_protocol::codes::client::ModifyTrade);
        assert_eq!(body[0], ModifyTradeAction::Accept as u8);

        ui.observe(&open_trade(), now);
        let trade_cancel = ui.handle_input(SocialInput::CancelTrade, false).unwrap();
        let (code, body) = encode_social_command(&trade_cancel).unwrap();
        assert_eq!(code, caer_protocol::codes::client::ModifyTrade);
        assert_eq!(body[0], ModifyTradeAction::Cancel as u8);
        assert_ne!(
            encode_social_command(&trade_accept),
            encode_social_command(&trade_cancel)
        );
    }

    /// Chat-open and idle keys must not fire social actions (Codex social falsifier).
    #[test]
    fn chat_open_and_unrelated_input_do_not_trigger_social() {
        let now = Instant::now();
        let mut ui = SocialUiState::new();
        ui.observe(&invite_event(), now);
        assert!(
            ui.handle_input(SocialInput::Accept, true).is_none(),
            "chat-open must block Accept"
        );
        assert!(
            ui.pending.is_some(),
            "pending invite remains while chat open"
        );

        let mut idle = SocialUiState::new();
        assert!(
            idle.handle_input(SocialInput::Accept, false).is_none(),
            "no overlay ⇒ no command"
        );
        assert!(idle.handle_input(SocialInput::Refuse, false).is_none());
        assert!(idle.handle_input(SocialInput::CancelTrade, false).is_none());
    }

    /// Timeout, logout, and region change clear pending UI state.
    #[test]
    fn timeout_logout_region_clear_pending() {
        let now = Instant::now();
        let mut ui = SocialUiState::new();
        ui.observe(&invite_event(), now);
        ui.observe(&open_trade(), now);
        assert!(ui.has_overlay());

        assert!(ui.tick_timeouts(now + PENDING_TIMEOUT + Duration::from_millis(1)));
        assert!(ui.pending.is_none());
        assert!(ui.trade.is_none(), "local trade overlay times out");
        assert!(!ui.trade_complete);
        ui.observe(&open_trade(), now);
        ui.observe(
            &ServerEvent::RegionChanged(caer_protocol::region::RegionChanged {
                region_id: 51,
                zone_skin_id: 51,
                cause: 0,
                server_id: 0,
            }),
            now,
        );
        assert!(!ui.has_overlay());

        ui.observe(&quest_event(), now);
        ui.observe(
            &ServerEvent::LoggedOut {
                total_out: false,
                level: 1,
            },
            now,
        );
        assert!(ui.pending.is_none());
    }

    /// Quest accept/decline also go through DialogResponse, never Say.
    #[test]
    fn quest_dialog_accept_decline_typed() {
        let now = Instant::now();
        let mut ui = SocialUiState::new();
        ui.observe(&quest_event(), now);
        let accept = ui.handle_input(SocialInput::Accept, false).unwrap();
        match accept {
            LiveCommand::DialogResponse {
                message_type,
                response,
                ..
            } => {
                assert_eq!(message_type, 0x64);
                assert_eq!(response, DIALOG_YES);
            }
            other => panic!("expected DialogResponse, got {other:?}"),
        }
        ui.observe(&quest_event(), now);
        let decline = ui.handle_input(SocialInput::Refuse, false).unwrap();
        match decline {
            LiveCommand::DialogResponse { response, .. } => assert_eq!(response, DIALOG_NO),
            other => panic!("expected DialogResponse, got {other:?}"),
        }
    }

    /// Direct LiveSession-style observation alone is LIVE_PROTOCOL_RT evidence — product UI
    /// requires [`SocialUiState::handle_input`]. This test documents the gate.
    #[test]
    fn live_session_observe_alone_is_not_player_scenario() {
        let now = Instant::now();
        let mut ui = SocialUiState::new();
        ui.observe(&invite_event(), now);
        // Protocol state visible, but no LiveCommand until product input.
        assert!(ui.last_command.is_none());
        assert!(ui.pending.is_some());
        // PLAYER_SCENARIO requires handle_input (or egui click) → typed command.
        let _ = ui.handle_input(SocialInput::Accept, false).unwrap();
        assert!(matches!(
            ui.last_command,
            Some(LiveCommand::DialogResponse { .. })
        ));
    }

    /// W1-03: last-moment money/item change must update displayed terms and disable Accept
    /// until the new snapshot is shown; partner items without identity never enable Accept.
    #[test]
    fn trade_accept_requires_shown_complete_current_terms() {
        let now = Instant::now();
        let mut ui = SocialUiState::new();
        ui.observe(
            &trade_offer(
                "Feile",
                TradeMoney {
                    gold: 1,
                    ..TradeMoney::default()
                },
                0,
            ),
            now,
        );
        let first = ui.trade.as_ref().unwrap().terms_text();
        assert!(first.contains("Feile") && first.contains("1g"), "{first}");
        assert!(
            !ui.trade.as_ref().unwrap().accept_enabled(),
            "must not accept before terms are shown"
        );
        assert!(ui.handle_input(SocialInput::Accept, false).is_none());

        ui.trade.as_mut().unwrap().note_terms_rendered();
        assert!(ui.trade.as_ref().unwrap().accept_enabled());

        ui.observe(
            &trade_offer(
                "Feile",
                TradeMoney {
                    gold: 99,
                    ..TradeMoney::default()
                },
                0,
            ),
            now,
        );
        let second = ui.trade.as_ref().unwrap().terms_text();
        assert!(
            second.contains("99g"),
            "must display current money: {second}"
        );
        assert!(!second.contains("1g") || second.contains("99g"));
        assert!(
            !ui.trade.as_ref().unwrap().accept_enabled(),
            "terms change must reset Accept until re-shown"
        );
        assert!(ui.handle_input(SocialInput::Accept, false).is_none());

        ui.observe(&trade_offer("Feile", TradeMoney::default(), 3), now);
        let with_items = ui.trade.as_ref().unwrap().terms_text();
        assert!(with_items.contains("3 item"), "{with_items}");
        ui.trade.as_mut().unwrap().note_terms_rendered();
        assert!(
            !ui.trade.as_ref().unwrap().accept_enabled(),
            "partner items without identity must keep Accept disabled"
        );
        assert!(ui.handle_input(SocialInput::Accept, false).is_none());

        let mut slots = [0u8; 10];
        slots[0] = 12;
        ui.observe(
            &ServerEvent::TradeWindow(TradeWindow {
                closed: false,
                own_slots: slots,
                own_money: TradeMoney::default(),
                partner_money: TradeMoney::default(),
                partner_item_count: 0,
                repairing: false,
                combining: false,
                caption: "Feile".into(),
            }),
            now,
        );
        let own = ui.trade.as_ref().unwrap().terms_text();
        assert!(
            own.contains("slot 12") && own.contains("unidentified"),
            "own offered slots must be named as unidentified: {own}"
        );
        ui.trade.as_mut().unwrap().note_terms_rendered();
        assert!(
            !ui.trade.as_ref().unwrap().accept_enabled(),
            "nonzero own_slots without item identity must keep Accept disabled"
        );
        assert!(ui.handle_input(SocialInput::Accept, false).is_none());

        ui.observe(
            &ServerEvent::TradeWindow(TradeWindow {
                closed: false,
                own_slots: [0; 10],
                own_money: TradeMoney::default(),
                partner_money: TradeMoney::default(),
                partner_item_count: 0,
                repairing: false,
                combining: false,
                caption: "Feile".into(),
            }),
            now,
        );
        assert!(
            !ui.trade.as_ref().unwrap().accept_enabled(),
            "last-moment own-slot clear still requires re-show"
        );
        ui.trade.as_mut().unwrap().note_terms_rendered();
        assert!(
            ui.trade.as_ref().unwrap().accept_enabled(),
            "money-only identified snapshot after re-show may accept"
        );
    }

    fn product_skin() -> caer_assets::uiskin::Skin {
        let mut skin = caer_assets::uiskin::Skin::default();
        crate::skinhud::inject_product_social_overlays(&mut skin);
        skin
    }

    fn show_dialog(ui: &mut SocialUiState) {
        let msg = ui
            .pending
            .as_ref()
            .map(|p| p.message.clone())
            .unwrap_or_default();
        ui.note_stock_layout(&[UiText {
            rect: crate::skinui::Rect::new(0.0, 0.0, 1.0, 1.0),
            font: Some("arial11".into()),
            color: caer_assets::uiskin::Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            text: msg,
            center_horizontally: false,
            adapter: Some("dialog_message".into()),
        }]);
    }

    fn roster_event() -> ServerEvent {
        ServerEvent::GroupWindow(GroupWindow {
            members: vec![caer_protocol::social::GroupWindowMember {
                name: "Feile".into(),
                salutation: String::new(),
                object_id: 12,
                level: 50,
            }],
        })
    }

    /// Falsifier `local_accept_without_roster_is_not_membership`.
    #[test]
    fn local_button_without_authoritative_response_cannot_show_success() {
        let now = Instant::now();
        let skin = product_skin();
        let mut wm = WindowManager::default();
        let mut ui = SocialUiState::new();
        ui.observe(&invite_event(), now);
        show_dialog(&mut ui);
        let hit = ui.handle_product_pointer(&mut wm, &skin, 380.0, 280.0, false);
        assert!(hit.consumed);
        assert!(matches!(
            hit.command,
            Some(LiveCommand::DialogResponse {
                response: DIALOG_YES,
                ..
            })
        ));
        assert!(
            ui.last_outbound.is_some(),
            "falsifier: deleting outbound encode makes this red"
        );
        assert!(!ui.in_group, "Dialog Yes is local intent, not membership");
        assert!(!ui.quest_accepted);
        assert!(!ui.trade_complete);

        ui.observe(&roster_event(), now);
        assert!(ui.in_group, "membership comes from GroupWindow");

        let mut trade_ui = SocialUiState::new();
        trade_ui.observe(&open_trade(), now);
        trade_ui.trade.as_mut().unwrap().note_terms_rendered();
        let t = trade_ui.handle_product_pointer(&mut wm, &skin, 380.0, 400.0, false);
        assert!(matches!(
            t.command,
            Some(LiveCommand::ModifyTrade {
                action: ModifyTradeAction::Accept,
                ..
            })
        ));
        assert!(!trade_ui.trade_complete);
        trade_ui.observe(
            &ServerEvent::MoneyUpdated(caer_protocol::money::MoneyUpdate::default()),
            now,
        );
        assert!(
            !trade_ui.trade_complete,
            "unrelated MoneyUpdate is not trade completion"
        );
        trade_ui.observe(
            &ServerEvent::TradeWindow(TradeWindow {
                closed: true,
                own_slots: [0; 10],
                own_money: TradeMoney::default(),
                partner_money: TradeMoney::default(),
                partner_item_count: 0,
                repairing: false,
                combining: false,
                caption: String::new(),
            }),
            now,
        );
        assert!(trade_ui.trade_complete);

        let mut quest_ui = SocialUiState::new();
        quest_ui.observe(&quest_event(), now);
        show_dialog(&mut quest_ui);
        let _ = quest_ui.handle_product_pointer(&mut wm, &skin, 380.0, 280.0, false);
        assert!(!quest_ui.quest_accepted);
        quest_ui.observe(
            &ServerEvent::QuestEntry {
                entry: QuestEntry {
                    index: 99,
                    name: "Unrelated".into(),
                    description: "no".into(),
                },
                payload: vec![],
            },
            now,
        );
        assert!(
            !quest_ui.quest_accepted,
            "unrelated QuestEntry must not complete the offered dialog"
        );
        quest_ui.observe(
            &ServerEvent::QuestEntry {
                entry: QuestEntry {
                    index: 1,
                    name: "Important Delivery (Level 1)".into(),
                    description: "[Step #1]: go".into(),
                },
                payload: vec![],
            },
            now,
        );
        assert!(quest_ui.quest_accepted);
    }

    /// Falsifier: partner reject / local timeout / stale terms must not set trade_complete
    /// or quest_accepted.
    #[test]
    fn trade_reject_timeout_stale_generation_leave_authority_unchanged() {
        let now = Instant::now();
        let closed = ServerEvent::TradeWindow(TradeWindow {
            closed: true,
            own_slots: [0; 10],
            own_money: TradeMoney::default(),
            partner_money: TradeMoney::default(),
            partner_item_count: 0,
            repairing: false,
            combining: false,
            caption: String::new(),
        });

        let mut reject = SocialUiState::new();
        reject.observe(&open_trade(), now);
        reject.trade.as_mut().unwrap().note_terms_rendered();
        reject.observe(&closed, now);
        assert!(!reject.trade_complete, "close without Accept is reject");
        assert!(reject.trade.is_none());

        let mut timeout = SocialUiState::new();
        timeout.observe(&open_trade(), now);
        timeout.trade.as_mut().unwrap().note_terms_rendered();
        assert!(timeout.tick_timeouts(now + TRADE_TIMEOUT + Duration::from_millis(1)));
        assert!(!timeout.trade_complete);
        assert!(timeout.trade.is_none());

        let mut stale = SocialUiState::new();
        stale.observe(
            &trade_offer(
                "Feile",
                TradeMoney {
                    gold: 1,
                    ..TradeMoney::default()
                },
                0,
            ),
            now,
        );
        stale.trade.as_mut().unwrap().note_terms_rendered();
        let gen_shown = stale.trade.as_ref().unwrap().shown_generation;
        stale.observe(
            &trade_offer(
                "Feile",
                TradeMoney {
                    gold: 99,
                    ..TradeMoney::default()
                },
                0,
            ),
            now,
        );
        assert!(stale.trade.as_ref().unwrap().terms_generation > gen_shown);
        assert!(!stale.trade.as_ref().unwrap().accept_enabled());
        assert!(stale.handle_input(SocialInput::Accept, false).is_none());
        assert!(!stale.trade_complete);
        stale.observe(&closed, now);
        assert!(
            !stale.trade_complete,
            "stale Accept was never sent; close is not completion"
        );

        let mut decline = SocialUiState::new();
        decline.observe(&quest_event(), now);
        let _ = decline.handle_input(SocialInput::Refuse, false);
        decline.observe(
            &ServerEvent::QuestEntry {
                entry: QuestEntry {
                    index: 1,
                    name: "Important Delivery (Level 1)".into(),
                    description: "[Step #1]: go".into(),
                },
                payload: vec![],
            },
            now,
        );
        assert!(
            !decline.quest_accepted,
            "decline must not latch quest_accepted on a later matching index"
        );
    }

    /// Falsifier `product_hit_testing_required`.
    #[test]
    fn deleting_hit_testing_makes_product_path_red() {
        let now = Instant::now();
        let skin = product_skin();
        let mut wm = WindowManager::default();
        let mut ui = SocialUiState::new();
        ui.observe(&invite_event(), now);
        let miss = ui.handle_product_pointer(&mut wm, &skin, 0.0, 0.0, false);
        assert!(!miss.consumed);
        assert!(miss.command.is_none());
        assert!(ui.pending.is_some());
        show_dialog(&mut ui);
        let hit = ui.handle_product_pointer(&mut wm, &skin, 380.0, 280.0, false);
        assert_eq!(
            hit.hit.as_ref().map(|h| h.1.as_str()),
            Some("accept"),
            "product path must go through WindowManager::hit_control"
        );
        assert!(hit.command.is_some());
    }

    /// Falsifier `direct_handler_is_not_player_scenario` — documented gate, not a promotion.
    #[test]
    fn handle_input_alone_is_not_the_product_pointer_path() {
        let now = Instant::now();
        let mut ui = SocialUiState::new();
        ui.observe(&invite_event(), now);
        let _ = ui.handle_input(SocialInput::Accept, false);
        assert!(
            ui.last_hit.is_none(),
            "raw handler never recorded a skin hit"
        );
    }

    #[test]
    fn accept_and_refuse_and_escape_cover_both_arms() {
        let now = Instant::now();
        let skin = product_skin();
        let mut wm = WindowManager::default();
        let mut ui = SocialUiState::new();
        ui.observe(&invite_event(), now);
        let refuse = ui.handle_product_pointer(&mut wm, &skin, 520.0, 280.0, false);
        match refuse.command {
            Some(LiveCommand::DialogResponse { response, .. }) => assert_eq!(response, DIALOG_NO),
            other => panic!("{other:?}"),
        }

        ui.observe(&open_trade(), now);
        ui.trade.as_mut().unwrap().note_terms_rendered();
        let cancel = ui.handle_product_key(&mut wm, ProductKey::Escape, false);
        assert!(matches!(
            cancel.command,
            Some(LiveCommand::ModifyTrade {
                action: ModifyTradeAction::Cancel,
                ..
            })
        ));
        assert!(ui.trade.is_none());
    }

    /// Falsifier `stale_ids_timeout_reconnect_leave_no_ghost`.
    #[test]
    fn stale_timeout_cancel_logout_reconnect_leave_no_ghost() {
        let now = Instant::now();
        let skin = product_skin();
        let mut wm = WindowManager::default();
        let mut ui = SocialUiState::new();
        ui.observe(&invite_event(), now);
        ui.observe(
            &ServerEvent::Dialog {
                code: DIALOG_GROUP_INVITE,
                data1: 99,
                data2: 0,
                data3: 0,
                data4: 0,
                message: "Other has invited you".into(),
            },
            now,
        );
        show_dialog(&mut ui);
        let accept = ui.handle_product_pointer(&mut wm, &skin, 380.0, 280.0, false);
        match accept.command {
            Some(LiveCommand::DialogResponse { data1, .. }) => {
                assert_eq!(data1, 99, "stale identity 3 must not be sent")
            }
            other => panic!("{other:?}"),
        }

        ui.observe(&invite_event(), now);
        ui.observe(&open_trade(), now);
        ui.observe(&roster_event(), now);
        assert!(ui.in_group);
        ui.tick_timeouts(now + PENDING_TIMEOUT + Duration::from_millis(1));
        assert!(ui.pending.is_none());

        ui.reconnect_reset(&mut wm);
        assert!(!ui.has_overlay());
        assert!(!ui.in_group);
        assert!(!ui.trade_complete);
        assert!(!ui.quest_accepted);
        assert!(!wm.is_open(PRODUCT_DIALOG_WINDOW));
        assert!(!wm.is_open(PRODUCT_TRADE_WINDOW));
        assert_eq!(wm.hit(&skin, 380.0, 280.0), None);
    }

    #[test]
    fn duplicate_group_window_does_not_ghost_extra_members() {
        let now = Instant::now();
        let mut ui = SocialUiState::new();
        ui.observe(&roster_event(), now);
        ui.observe(&roster_event(), now);
        assert_eq!(ui.group.as_ref().unwrap().members.len(), 1);
        ui.observe(
            &ServerEvent::GroupWindow(GroupWindow { members: vec![] }),
            now,
        );
        assert!(!ui.in_group);
        assert!(ui.group.is_none());
    }
}
