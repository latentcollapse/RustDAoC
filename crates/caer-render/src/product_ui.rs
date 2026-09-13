//! Product stock-skin UI composition (B1).
//!
//! One bind, one [`WindowManager`], one resolve chain. `rustdaoc` composes this; it does not
//! reimplement overlay lists or skip motion/release. Acceptance is behavioral (layout text,
//! generation-gated Accept, drag lifecycle) — not `include_str!` of the binary.

use std::collections::HashSet;

use crate::adapters::{self, AdapterState, ExtraBind, SocialBind};
use crate::cfx_adapters;
use crate::skinhud::{self, CriticalHudBind, CriticalWindowClass, SkinHud};
use crate::skinui::{UiText, WindowDraw, WindowManager};
use crate::social_ui::SocialUiState;
use caer_world::CombatPresentation;

/// Resolve base + social + extra + CFX adapters. Unknown names stay unbound.
///
/// Callers that have overview / chat-entry / skills must pass them via `extra`.
/// `ExtraBind::default()` is honest empty product state, not a test-only bypass.
#[must_use]
pub fn resolve_product(
    state: &AdapterState<'_>,
    social: &SocialBind<'_>,
    extra: &ExtraBind<'_>,
    combat: Option<&CombatPresentation<'_>>,
    adapter: &str,
) -> Option<String> {
    resolve_product_extra(state, social, extra, combat, adapter)
}

/// Same chain with overview / chat-entry / skills (INT wires ExtraBind later).
#[must_use]
pub fn resolve_product_extra(
    state: &AdapterState<'_>,
    social: &SocialBind<'_>,
    extra: &ExtraBind<'_>,
    combat: Option<&CombatPresentation<'_>>,
    adapter: &str,
) -> Option<String> {
    adapters::resolve(state, adapter)
        .or_else(|| adapters::resolve_social(social, adapter))
        .or_else(|| adapters::resolve_extra(extra, adapter))
        .or_else(|| match adapter {
            "conc_list" => combat.and_then(|c| cfx_adapters::resolve(c, "concentration_effect0")),
            "stats_realm_points" => combat.and_then(|c| cfx_adapters::resolve(c, "realm_points")),
            _ => combat.and_then(|c| cfx_adapters::resolve(c, adapter)),
        })
}

/// Open/close critical windows on `wm` from packet gates. Existing positions are kept.
///
/// Packet gates are **eligibility**, not player visibility. Pass [`HashSet::new`] when no
/// player-closed set exists (scenario drivers). Player-closed names are not reopened.
pub fn sync_critical_windows(wm: &mut WindowManager, hud: &SkinHud, bind: CriticalHudBind) {
    sync_critical_windows_for_player(wm, hud, bind, &HashSet::new());
}

/// Same as [`sync_critical_windows`], honoring names the player explicitly closed.
pub fn sync_critical_windows_for_player(
    wm: &mut WindowManager,
    hud: &SkinHud,
    bind: CriticalHudBind,
    player_closed: &HashSet<String>,
) {
    let wanted = skinhud::critical_hud_windows(hud, bind);
    let wanted_names: Vec<String> = wanted.iter().map(|(n, _)| n.clone()).collect();
    for (name, pos) in &wanted {
        if panel_names_blocked(name, player_closed) {
            continue;
        }
        if panel_sibling_open(wm, name) {
            continue;
        }
        if !wm.is_open(name) {
            wm.open(name, *pos);
        }
    }
    // Only auto-close classes `critical_hud_windows` actually gates. Map/loot/preworld and
    // player-toggled sheet/skills are not packet-lifetime windows — closing them here would
    // fight an explicit open (Inventory vs Stats must stay independently toggleable).
    let mut critical: Vec<&str> = CriticalWindowClass::ALL
        .iter()
        .filter(|c| {
            !c.is_preworld()
                && !matches!(
                    c,
                    CriticalWindowClass::Loot
                        | CriticalWindowClass::Map
                        | CriticalWindowClass::CharacterSheet
                        | CriticalWindowClass::SkillsQuickbar
                )
        })
        .flat_map(|c| c.skin_names().iter().copied())
        .collect();
    critical.extend([
        skinhud::PRODUCT_TRADE_WINDOW,
        skinhud::PRODUCT_DIALOG_WINDOW,
        "new_quest_journal",
        "quest",
        "new_quest",
    ]);
    for name in critical {
        if !wm.is_open(name) || wanted_names.iter().any(|n| n == name) {
            continue;
        }
        // Dialog/trade are also synced from SocialUiState; don't close those here if gated on.
        if name == skinhud::PRODUCT_DIALOG_WINDOW || name == skinhud::PRODUCT_TRADE_WINDOW {
            continue;
        }
        // Class still eligible under an alternate skin name — keep the player's choice.
        if panel_siblings(name)
            .iter()
            .any(|s| wanted_names.iter().any(|w| w == s))
        {
            continue;
        }
        wm.close(name);
    }
}

fn panel_siblings(name: &str) -> Vec<&'static str> {
    for class in CriticalWindowClass::ALL {
        if class.skin_names().contains(&name) {
            return class.skin_names().to_vec();
        }
    }
    const QUEST: &[&str] = &["new_quest_journal", "quest", "new_quest"];
    if QUEST.contains(&name) {
        return QUEST.to_vec();
    }
    Vec::new()
}

fn panel_names_blocked(name: &str, player_closed: &HashSet<String>) -> bool {
    if player_closed.contains(name) {
        return true;
    }
    panel_siblings(name)
        .iter()
        .any(|s| player_closed.contains(*s))
}

fn panel_sibling_open(wm: &WindowManager, name: &str) -> bool {
    if wm.is_open(name) {
        return true;
    }
    panel_siblings(name).iter().any(|s| wm.is_open(s))
}

/// Lay out every visible WM window through the composed resolver.
#[must_use]
pub fn draw_product(
    skin: &caer_assets::uiskin::Skin,
    wm: &WindowManager,
    state: &AdapterState<'_>,
    social: &SocialBind<'_>,
    extra: &ExtraBind<'_>,
    combat: Option<&CombatPresentation<'_>>,
) -> WindowDraw {
    wm.draw(skin, &|a| resolve_product(state, social, extra, combat, a))
}

/// After stock layout, mark dialog/trade generations shown only when their adapter text is present.
pub fn note_stock_layout(social: &mut SocialUiState, texts: &[UiText]) {
    social.note_stock_layout(texts);
}

/// Dev-only egui HUD/chat/quickbar. Unset → stock skin is the product chrome.
#[must_use]
pub fn dev_egui_enabled() -> bool {
    match std::env::var("CAER_DEV_EGUI") {
        Ok(v) => {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("on")
        }
        Err(_) => false,
    }
}
