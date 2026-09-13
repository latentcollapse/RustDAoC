//! Stock-skin product UI path (B1). Behavioral — not `include_str!` of rustdaoc.
//!
//! Named falsifiers:
//! - terms text missing from layout → Accept stays disabled
//! - Accept before `note_stock_layout` emits no DialogResponse
//! - press → motion → release must move the window
//! - Escape closes and emits refuse/cancel
//! - outbound encode is present

use std::time::Instant;

use caer_protocol::session::ServerEvent;
use caer_protocol::social::{ModifyTradeAction, TradeMoney, TradeWindow};
use caer_protocol::status::PlayerStatus;
use caer_render::adapters::{self, AdapterState, ExtraBind, SocialBind};
use caer_render::live::LiveCommand;
use caer_render::product_ui;
use caer_render::skinhud::{self, PRODUCT_DIALOG_WINDOW};
use caer_render::skinui::WindowManager;
use caer_render::social_ui::{SocialInput, SocialUiState, DIALOG_GROUP_INVITE, DIALOG_YES};

fn adapter<'a>(status: &'a PlayerStatus) -> AdapterState<'a> {
    AdapterState {
        player_name: "",
        status,
        target: None,
        zone: None,
        fps: 0,
        sheet: None,
        char_stats: None,
        char_resists: None,
        equipment: None,
        money: None,
        inventory: None,
        merchant: None,
        weapon_armor: None,
        attack_mode: None,
        login_account: None,
        login_password_mask: None,
        create_name: None,
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

fn product_skin() -> caer_assets::uiskin::Skin {
    let mut skin = caer_assets::uiskin::Skin::default();
    skinhud::inject_product_social_overlays(&mut skin);
    skin
}

fn bind_for<'a>(ui: &'a SocialUiState, terms: Option<&'a str>) -> SocialBind<'a> {
    SocialBind {
        group: ui.group.as_ref(),
        group_vitals: ui.group_vitals.as_ref(),
        quests: Some(ui.quests.as_slice()),
        trade: ui.trade.as_ref().map(|t| &t.window),
        dialog_message: ui.pending.as_ref().map(|p| p.message.as_str()),
        trade_terms: terms,
    }
}

/// Falsifier `stock_layout_shows_dialog_message_before_accept`.
#[test]
fn stock_layout_shows_message_and_gates_accept() {
    let now = Instant::now();
    let skin = product_skin();
    let mut wm = WindowManager::default();
    let mut ui = SocialUiState::new();
    ui.observe(&invite(), now);
    ui.sync_product_windows(&mut wm);
    let status = PlayerStatus::default();
    let state = adapter(&status);
    let social = bind_for(&ui, None);
    let draw = product_ui::draw_product(&skin, &wm, &state, &social, &ExtraBind::default(), None);
    assert!(
        draw.texts
            .iter()
            .any(|t| t.adapter.as_deref() == Some("dialog_message")
                && t.text.contains("Feile has invited")),
        "stock layout must emit dialog_message text, got {:?}",
        draw.texts
            .iter()
            .map(|t| (&t.adapter, &t.text))
            .collect::<Vec<_>>()
    );

    let miss = ui.handle_product_pointer(&mut wm, &skin, 380.0, 280.0, false);
    assert!(
        miss.command.is_none(),
        "Accept before shown must emit no command: {:?}",
        miss.command
    );
    assert!(ui.pending.is_some());

    product_ui::note_stock_layout(&mut ui, &draw.texts);
    assert!(ui.dialog_was_shown());
    let hit = ui.handle_product_pointer(&mut wm, &skin, 380.0, 280.0, false);
    assert!(matches!(
        hit.command,
        Some(LiveCommand::DialogResponse {
            response: DIALOG_YES,
            ..
        })
    ));
    assert!(ui.last_outbound.is_some());
}

/// Falsifier `product_drag_press_motion_release`.
#[test]
fn press_motion_release_moves_dialog() {
    let now = Instant::now();
    let skin = product_skin();
    let mut wm = WindowManager::default();
    let mut ui = SocialUiState::new();
    ui.observe(&invite(), now);
    ui.sync_product_windows(&mut wm);
    let before = wm
        .visible()
        .find(|w| w.name == PRODUCT_DIALOG_WINDOW)
        .map(|w| w.pos)
        .expect("dialog open");
    assert!(wm.on_press(&skin, before.0 + 40.0, before.1 + 8.0));
    wm.on_motion(before.0 + 140.0, before.1 + 108.0);
    wm.on_release();
    let after = wm
        .visible()
        .find(|w| w.name == PRODUCT_DIALOG_WINDOW)
        .map(|w| w.pos)
        .expect("dialog still open");
    assert_ne!(before, after, "drag must move the window");
    assert!((after.0 - (before.0 + 100.0)).abs() < 1.0);
}

/// Falsifier `escape_closes_overlay`.
#[test]
fn escape_closes_and_refuses() {
    let now = Instant::now();
    let mut wm = WindowManager::default();
    let mut ui = SocialUiState::new();
    ui.observe(&invite(), now);
    ui.sync_product_windows(&mut wm);
    let cmd = ui.handle_product_key(&mut wm, caer_render::social_ui::ProductKey::Escape, false);
    assert!(matches!(
        cmd.command,
        Some(LiveCommand::DialogResponse { response: 0, .. })
    ));
    assert!(ui.pending.is_none());
}

/// Falsifier `trade_terms_required_for_accept`.
#[test]
fn trade_accept_requires_terms_in_layout() {
    let now = Instant::now();
    let skin = product_skin();
    let mut wm = WindowManager::default();
    let mut ui = SocialUiState::new();
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
    ui.sync_product_windows(&mut wm);
    assert!(
        !ui.trade.as_ref().unwrap().accept_enabled(),
        "unshown terms cannot accept"
    );
    let terms = ui.trade.as_ref().unwrap().terms_text();
    let status = PlayerStatus::default();
    let state = adapter(&status);
    let social = bind_for(&ui, Some(terms.as_str()));
    let draw = product_ui::draw_product(&skin, &wm, &state, &social, &ExtraBind::default(), None);
    assert!(
        draw.texts
            .iter()
            .any(|t| t.adapter.as_deref() == Some("trade_terms") && t.text.contains("Feile")),
        "layout must include trade_terms"
    );
    product_ui::note_stock_layout(&mut ui, &draw.texts);
    assert!(ui.trade.as_ref().unwrap().accept_enabled());
    let cmd = ui.handle_input(SocialInput::Accept, false);
    assert!(matches!(
        cmd,
        Some(LiveCommand::ModifyTrade {
            action: ModifyTradeAction::Accept,
            ..
        })
    ));
}

#[test]
fn resolve_product_includes_social_and_base() {
    let status = PlayerStatus::default();
    let state = adapter(&status);
    let bind = SocialBind {
        dialog_message: Some("hello"),
        ..Default::default()
    };
    assert_eq!(
        adapters::resolve_social(&bind, "dialog_message").as_deref(),
        Some("hello")
    );
    assert_eq!(
        product_ui::resolve_product(&state, &bind, &ExtraBind::default(), None, "dialog_message")
            .as_deref(),
        Some("hello")
    );
    assert!(product_ui::resolve_product(
        &state,
        &bind,
        &ExtraBind::default(),
        None,
        "bounty_points"
    )
    .is_none());
}
