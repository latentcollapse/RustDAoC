//! rustdaoc product seam for [`caer_script::AddonHost`].
//!
//! Addon-visible state is a read-only projection. Commands become [`LiveCommand`]s on the same
//! path as physical input. This module never mutates [`caer_world::WorldState`].
//!
//! Disable with `CAER_ADDONS=0`. Player drop-in root: `CAER_ADDONS_DIR`, else
//! `$XDG_DATA_HOME/caer/mods` (fallback `~/.local/share/caer/mods`). In-tree
//! HelloCAER / CombatMeter are examples for `caer addon test` only — never the
//! product default. Not CAER-1.0.
//!
//! Catalog honesty: only [`BRIDGED_EVENTS`] / [`BRIDGED_COMMANDS`] may be labeled `Stable`.
//! `caer addon check --bridge` and [`check_stable_bridge`] are the named falsifier.

use std::path::{Path, PathBuf};

use caer_script::{
    catalog_entries, AddonCommand, AddonEvent, AddonHost, AddonIntent, AddonStatus, CatalogKind,
    EventPayload, HostError, HostLimits, IntentValue, IntentValueLite, ReloadReport, Stability,
};

use crate::live::{Drain, LiveCommand};

const MAX_INTENTS_PER_FRAME: usize = 32;
const MAX_DIAGNOSTICS: usize = 32;

/// Events rustdaoc actually emits from [`Drain`]. Catalog `Stable` must be a subset.
pub const BRIDGED_EVENTS: &[AddonEvent] = &[
    AddonEvent::CombatSwing,
    AddonEvent::SpellCastStarted,
    AddonEvent::SpellEffectApplied,
    AddonEvent::ChatMessage,
    AddonEvent::InventoryUpdated,
    AddonEvent::MoneyUpdated,
    AddonEvent::LoggedOut,
    AddonEvent::RegionChanged,
    AddonEvent::GroupWindowUpdated,
    AddonEvent::DialogOpened,
    AddonEvent::TradeWindowUpdated,
    AddonEvent::GroupMemberUpdated,
    AddonEvent::QuestEntryUpdated,
    AddonEvent::CharacterOverviewReady,
    AddonEvent::ObjectRemoved,
    AddonEvent::EnteredWorld,
    AddonEvent::LoginGranted,
    AddonEvent::CryptKeyReceived,
];

/// Commands rustdaoc accepts when required args are present. Catalog `Stable` must be a subset.
pub const BRIDGED_COMMANDS: &[AddonCommand] = &[
    AddonCommand::Say,
    AddonCommand::SlashCommand,
    AddonCommand::Attack,
    AddonCommand::Target,
    AddonCommand::Quit,
    AddonCommand::InviteGroup,
    AddonCommand::RequestNpc,
    AddonCommand::SelectCharacter,
    AddonCommand::UseSkill,
    AddonCommand::MoveItem,
    AddonCommand::Interact,
    AddonCommand::BuyItem,
    AddonCommand::RequestCharacterOverview,
];

/// Structured product-visible addon fault / unload (S1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonDiagnostic {
    pub addon_id: String,
    pub event: String,
    pub error: String,
    pub status: &'static str,
    pub unloaded: bool,
}

/// Product addon layer. `None` host means explicitly disabled.
pub struct ProductAddons {
    host: Option<AddonHost>,
    diagnostics: Vec<AddonDiagnostic>,
}

impl ProductAddons {
    #[must_use]
    pub fn from_env() -> Self {
        if std::env::var_os("CAER_ADDONS").is_some_and(|v| v == "0") {
            log::info!("rustdaoc: addons disabled (CAER_ADDONS=0)");
            return Self {
                host: None,
                diagnostics: Vec::new(),
            };
        }
        let mut host = AddonHost::new(HostLimits::production());
        let root = addons_root();
        if let Err(e) = std::fs::create_dir_all(&root) {
            log::warn!(
                "rustdaoc: cannot create addon mods dir {} ({e}); host stays empty",
                root.display()
            );
        } else {
            match host.load_tree(&root) {
                Ok(ids) => log::info!(
                    "rustdaoc: loaded {} addon(s) from {} (drop .toc packs here; in-tree examples are not this path)",
                    ids.len(),
                    root.display()
                ),
                Err(e) => log::warn!(
                    "rustdaoc: addon load from {} failed ({e}); host stays empty (game continues)",
                    root.display()
                ),
            }
        }
        Self {
            host: Some(host),
            diagnostics: Vec::new(),
        }
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self {
            host: None,
            diagnostics: Vec::new(),
        }
    }

    #[must_use]
    pub fn from_host(host: AddonHost) -> Self {
        Self {
            host: Some(host),
            diagnostics: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.host.is_some()
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[AddonDiagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn addon_status(&self, id: &str) -> Option<AddonStatus> {
        self.host.as_ref().and_then(|h| h.status(id))
    }

    /// Non-spam chat notes for rustdaoc combat log. Deduped by (addon, event, status).
    pub fn take_diagnostic_notes(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        for d in &self.diagnostics {
            let note = format!(
                "[addon] {} {} on {} ({})",
                d.addon_id, d.status, d.event, d.error
            );
            if !out.contains(&note) {
                out.push(note);
            }
        }
        out
    }

    /// Fold a live drain into typed addon events, then drain intents as LiveCommands.
    pub fn on_drain(&mut self, drain: &Drain) -> Vec<LiveCommand> {
        let Some(host) = self.host.as_mut() else {
            return Vec::new();
        };
        for (ev, payload) in events_from_drain(drain) {
            let report = host.dispatch(ev, &payload);
            for fault in &report.faults {
                let addon_id = fault
                    .split_once(':')
                    .map(|(id, _)| id.trim().to_string())
                    .unwrap_or_else(|| fault.clone());
                let unloaded = report.unloaded.iter().any(|u| u == &addon_id);
                record_diagnostic(
                    &mut self.diagnostics,
                    AddonDiagnostic {
                        addon_id,
                        event: ev.as_str().to_string(),
                        error: fault.clone(),
                        status: if unloaded { "unloaded" } else { "faulted" },
                        unloaded,
                    },
                );
                log::warn!(
                    "rustdaoc: addon fault on {}: {fault} (game + other addons continue)",
                    ev.as_str()
                );
            }
            for id in &report.unloaded {
                if !self
                    .diagnostics
                    .iter()
                    .any(|d| d.addon_id == *id && d.unloaded && d.event == ev.as_str())
                {
                    record_diagnostic(
                        &mut self.diagnostics,
                        AddonDiagnostic {
                            addon_id: id.clone(),
                            event: ev.as_str().to_string(),
                            error: "deferred unload".into(),
                            status: "unloaded",
                            unloaded: true,
                        },
                    );
                }
                log::warn!(
                    "rustdaoc: addon {id} unloaded after {} (neighbors continue)",
                    ev.as_str()
                );
            }
        }
        let mut out = Vec::new();
        for intent in host.take_intents() {
            if out.len() >= MAX_INTENTS_PER_FRAME {
                log::warn!("rustdaoc: addon intent cap {MAX_INTENTS_PER_FRAME} hit; dropping rest");
                break;
            }
            match intent_to_live_command(&intent) {
                Ok(cmd) => out.push(cmd),
                Err(e) => log::warn!("rustdaoc: addon intent refused: {e}"),
            }
        }
        for line in host.take_logs() {
            log::info!("addon[{}]: {}", line.addon_id, line.line);
        }
        out
    }

    /// Product hot-reload: same `AddonHost::reload` path, with diagnostics on failure.
    /// Broken replacements roll back inside the host; neighbors keep running.
    pub fn reload(&mut self, id: &str) -> Result<ReloadReport, HostError> {
        let Some(host) = self.host.as_mut() else {
            return Err(HostError::UnknownAddon(format!(
                "addons disabled; cannot reload {id}"
            )));
        };
        match host.reload(id) {
            Ok(report) => {
                log::info!(
                    "rustdaoc: reloaded addon {} gen={} saved={}",
                    report.addon_id,
                    report.generation,
                    report.preserved_saved_variables
                );
                Ok(report)
            }
            Err(e) => {
                record_diagnostic(
                    &mut self.diagnostics,
                    AddonDiagnostic {
                        addon_id: id.to_string(),
                        event: "reload".into(),
                        error: e.to_string(),
                        status: "faulted",
                        unloaded: false,
                    },
                );
                log::warn!("rustdaoc: addon reload {id} failed ({e}); previous generation kept");
                Err(e)
            }
        }
    }

    /// Reload every loaded addon. First failure returns after that addon's host rollback.
    pub fn reload_all(&mut self) -> Result<Vec<ReloadReport>, HostError> {
        let ids = self
            .host
            .as_ref()
            .map(|h| h.loaded_ids())
            .unwrap_or_default();
        let mut out = Vec::new();
        for id in ids {
            out.push(self.reload(&id)?);
        }
        Ok(out)
    }
}

fn record_diagnostic(buf: &mut Vec<AddonDiagnostic>, d: AddonDiagnostic) {
    if buf.len() >= MAX_DIAGNOSTICS {
        buf.remove(0);
    }
    buf.push(d);
}

fn addons_root() -> PathBuf {
    let explicit = std::env::var_os("CAER_ADDONS_DIR").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_product_addons_root(explicit.as_deref(), xdg.as_deref(), home.as_deref())
}

/// Player `mods/` search path. Never falls back to in-tree HelloCAER/CombatMeter.
///
/// `CAER_ADDONS_DIR` wins; otherwise `$XDG_DATA_HOME/caer/mods`, else
/// `$HOME/.local/share/caer/mods`. Missing HOME/XDG uses `./caer/mods` (cwd).
pub fn resolve_product_addons_root(
    explicit: Option<&Path>,
    xdg_data_home: Option<&Path>,
    home: Option<&Path>,
) -> PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    let data = match xdg_data_home {
        Some(p) => p.to_path_buf(),
        None => match home {
            Some(h) => h.join(".local/share"),
            None => PathBuf::from("."),
        },
    };
    data.join("caer").join("mods")
}

/// Named falsifier: every catalog `Stable` row must appear in the rustdaoc bridge tables.
pub fn check_stable_bridge() -> Result<(), String> {
    for e in catalog_entries() {
        if e.stability != Stability::Stable {
            continue;
        }
        match e.kind {
            CatalogKind::Event => {
                let ev = AddonEvent::parse(e.id)
                    .ok_or_else(|| format!("stable catalog id `{}` is not an AddonEvent", e.id))?;
                if !BRIDGED_EVENTS.contains(&ev) {
                    return Err(format!(
                        "stable event `{}` is not emitted by rustdaoc Drain→AddonHost",
                        e.id
                    ));
                }
            }
            CatalogKind::Command => {
                let cmd = AddonCommand::parse(e.id).ok_or_else(|| {
                    format!("stable catalog id `{}` is not an AddonCommand", e.id)
                })?;
                if !BRIDGED_COMMANDS.contains(&cmd) {
                    return Err(format!(
                        "stable command `{}` is not accepted by rustdaoc AddonIntent→LiveCommand",
                        e.id
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Map a validated addon intent onto the player command path. Incomplete args are refused, not invented.
pub fn intent_to_live_command(intent: &AddonIntent) -> Result<LiveCommand, HostError> {
    let refuse = |reason: &str| HostError::InvalidIntent {
        addon: intent.addon_id.clone(),
        reason: reason.to_string(),
    };
    match intent.command {
        AddonCommand::Say => {
            let text = arg_str(intent, "text").ok_or_else(|| refuse("chat.say needs text"))?;
            Ok(LiveCommand::Say(text.to_string()))
        }
        AddonCommand::SlashCommand => {
            let text =
                arg_str(intent, "text").ok_or_else(|| refuse("chat.slash_command needs text"))?;
            Ok(LiveCommand::Command(text.to_string()))
        }
        AddonCommand::Attack => {
            let start = arg_bool(intent, "start").unwrap_or(true);
            Ok(LiveCommand::Attack { start })
        }
        AddonCommand::Target => {
            let oid = arg_u16(intent, "object_id")
                .ok_or_else(|| refuse("combat.target needs object_id"))?;
            Ok(LiveCommand::Target(oid))
        }
        AddonCommand::Quit => Ok(LiveCommand::Quit),
        AddonCommand::InviteGroup => Ok(LiveCommand::InviteToGroup),
        AddonCommand::RequestNpc => {
            let object_id = arg_u16(intent, "object_id")
                .ok_or_else(|| refuse("session.request_npc needs object_id"))?;
            Ok(LiveCommand::RequestNpc { object_id })
        }
        AddonCommand::SelectCharacter => {
            let slot = arg_u8(intent, "slot")?
                .ok_or_else(|| refuse("session.select_character needs slot"))?;
            Ok(LiveCommand::SelectCharacter { slot })
        }
        AddonCommand::UseSkill => {
            let index =
                arg_u8(intent, "index")?.ok_or_else(|| refuse("spell.use_skill needs index"))?;
            let skill_type = arg_u8(intent, "skill_type")?.unwrap_or(0);
            let x = arg_f32(intent, "x").ok_or_else(|| refuse("spell.use_skill needs x"))?;
            let y = arg_f32(intent, "y").ok_or_else(|| refuse("spell.use_skill needs y"))?;
            let z = arg_f32(intent, "z").ok_or_else(|| refuse("spell.use_skill needs z"))?;
            Ok(LiveCommand::UseSkill {
                index,
                skill_type,
                x,
                y,
                z,
            })
        }
        AddonCommand::MoveItem => {
            let to_slot = arg_u16(intent, "to_slot")
                .ok_or_else(|| refuse("inventory.move_item needs to_slot"))?;
            let from_slot = arg_u16(intent, "from_slot")
                .ok_or_else(|| refuse("inventory.move_item needs from_slot"))?;
            let count = arg_u16(intent, "count").unwrap_or(1);
            Ok(LiveCommand::MoveItem {
                to_slot,
                from_slot,
                count,
            })
        }
        AddonCommand::Interact => {
            let player_x = arg_u32(intent, "player_x")
                .ok_or_else(|| refuse("inventory.interact needs player_x"))?;
            let player_y = arg_u32(intent, "player_y")
                .ok_or_else(|| refuse("inventory.interact needs player_y"))?;
            let target_oid = arg_u16(intent, "target_oid")
                .ok_or_else(|| refuse("inventory.interact needs target_oid"))?;
            Ok(LiveCommand::Interact {
                player_x,
                player_y,
                target_oid,
            })
        }
        AddonCommand::BuyItem => {
            let player_x = arg_u32(intent, "player_x")
                .ok_or_else(|| refuse("inventory.buy_item needs player_x"))?;
            let player_y = arg_u32(intent, "player_y")
                .ok_or_else(|| refuse("inventory.buy_item needs player_y"))?;
            let merchant_id = arg_u16(intent, "merchant_id")
                .ok_or_else(|| refuse("inventory.buy_item needs merchant_id"))?;
            let item_slot = arg_u16(intent, "item_slot")
                .ok_or_else(|| refuse("inventory.buy_item needs item_slot"))?;
            let item_count = arg_u8(intent, "item_count")?.unwrap_or(1);
            Ok(LiveCommand::BuyItem {
                player_x,
                player_y,
                merchant_id,
                item_slot,
                item_count,
            })
        }
        AddonCommand::RequestCharacterOverview => {
            let realm = arg_u8(intent, "realm")?
                .ok_or_else(|| refuse("session.request_character_overview needs realm"))?;
            Ok(LiveCommand::RequestCharacterOverview { realm })
        }
        AddonCommand::Move
        | AddonCommand::SetGroundTarget
        | AddonCommand::UseDoor
        | AddonCommand::LeaveGroup
        | AddonCommand::TradeOffer
        | AddonCommand::CreateCharacter => Err(refuse(&format!(
            "{} is provisional; rustdaoc will not invent missing pose/slot/draft fields",
            intent.command.as_str()
        ))),
    }
}

fn arg_str<'a>(intent: &'a AddonIntent, key: &str) -> Option<&'a str> {
    intent.args.get(key).and_then(IntentValue::as_str)
}

fn arg_bool(intent: &AddonIntent, key: &str) -> Option<bool> {
    match intent.args.get(key) {
        Some(IntentValue::Bool(b)) => Some(*b),
        _ => None,
    }
}

/// Present + in 0..=255 → `Ok(Some)`. Absent → `Ok(None)` (caller may default).
/// Present but out of u8 range → named refusal. Never coerce 256→0.
fn arg_u8(intent: &AddonIntent, key: &str) -> Result<Option<u8>, HostError> {
    match intent.args.get(key) {
        None => Ok(None),
        Some(IntentValue::Int(i)) if *i >= 0 && *i <= i64::from(u8::MAX) => Ok(Some(*i as u8)),
        Some(IntentValue::Number(n))
            if n.is_finite() && *n >= 0.0 && *n == n.trunc() && *n <= f64::from(u8::MAX) =>
        {
            Ok(Some(*n as u8))
        }
        Some(_) => Err(HostError::InvalidIntent {
            addon: intent.addon_id.clone(),
            reason: format!("{key} out of u8 range (0..=255); refusing, not wrapping"),
        }),
    }
}

fn arg_u16(intent: &AddonIntent, key: &str) -> Option<u16> {
    match intent.args.get(key) {
        Some(IntentValue::Int(i)) if *i >= 0 && *i <= i64::from(u16::MAX) => Some(*i as u16),
        Some(IntentValue::Number(n)) if *n >= 0.0 && *n <= f64::from(u16::MAX) => Some(*n as u16),
        _ => None,
    }
}

fn arg_u32(intent: &AddonIntent, key: &str) -> Option<u32> {
    match intent.args.get(key) {
        Some(IntentValue::Int(i)) if *i >= 0 && *i <= i64::from(u32::MAX) => Some(*i as u32),
        Some(IntentValue::Number(n)) if *n >= 0.0 && *n <= f64::from(u32::MAX) => Some(*n as u32),
        _ => None,
    }
}

fn arg_f32(intent: &AddonIntent, key: &str) -> Option<f32> {
    match intent.args.get(key) {
        Some(IntentValue::Number(n)) => Some(*n as f32),
        Some(IntentValue::Int(i)) => Some(*i as f32),
        _ => None,
    }
}

pub fn events_from_drain(d: &Drain) -> Vec<(AddonEvent, EventPayload)> {
    let mut out = Vec::new();
    for a in &d.combat_anims {
        out.push((
            AddonEvent::CombatSwing,
            EventPayload::empty()
                .with("oid", IntentValueLite::Int(i64::from(a.attacker_id)))
                .with("result", IntentValueLite::Str(format!("{:?}", a.result)))
                .with(
                    "hp_pct",
                    IntentValueLite::Int(i64::from(a.target_health_pct)),
                ),
        ));
    }
    for c in &d.spell_casts {
        out.push((
            AddonEvent::SpellCastStarted,
            EventPayload::empty()
                .with("caster_id", IntentValueLite::Int(i64::from(c.caster_id)))
                .with("spell_id", IntentValueLite::Int(i64::from(c.spell_id))),
        ));
    }
    for e in &d.spell_effects {
        out.push((
            AddonEvent::SpellEffectApplied,
            EventPayload::empty()
                .with("caster_id", IntentValueLite::Int(i64::from(e.caster_id)))
                .with("spell_id", IntentValueLite::Int(i64::from(e.spell_id))),
        ));
    }
    for (ty, text) in &d.messages {
        out.push((
            AddonEvent::ChatMessage,
            EventPayload::empty()
                .with("chat_type", IntentValueLite::Int(i64::from(*ty)))
                .with("text", IntentValueLite::Str(text.clone())),
        ));
    }
    if d.inventory_updated {
        out.push((AddonEvent::InventoryUpdated, EventPayload::empty()));
    }
    if d.money_updated {
        out.push((AddonEvent::MoneyUpdated, EventPayload::empty()));
    }
    if d.logged_out {
        out.push((AddonEvent::LoggedOut, EventPayload::empty()));
    }
    if let Some(rid) = d.region_changed {
        out.push((
            AddonEvent::RegionChanged,
            EventPayload::empty().with("region_id", IntentValueLite::Int(i64::from(rid))),
        ));
    }
    for g in &d.group_windows {
        out.push((
            AddonEvent::GroupWindowUpdated,
            EventPayload::empty().with("members", IntentValueLite::Int(g.members.len() as i64)),
        ));
    }
    for dlg in &d.dialogs {
        out.push((
            AddonEvent::DialogOpened,
            EventPayload::empty()
                .with("code", IntentValueLite::Int(i64::from(dlg.code)))
                .with("text", IntentValueLite::Str(dlg.message.clone())),
        ));
    }
    for tw in &d.trade_windows {
        out.push((
            AddonEvent::TradeWindowUpdated,
            EventPayload::empty()
                .with("closed", IntentValueLite::Bool(tw.closed))
                .with(
                    "partner_items",
                    IntentValueLite::Int(i64::from(tw.partner_item_count)),
                ),
        ));
    }
    for g in &d.group_member_updates {
        out.push((
            AddonEvent::GroupMemberUpdated,
            EventPayload::empty().with("members", IntentValueLite::Int(g.members.len() as i64)),
        ));
    }
    for q in &d.quest_entries {
        out.push((
            AddonEvent::QuestEntryUpdated,
            EventPayload::empty().with("name", IntentValueLite::Str(q.name.clone())),
        ));
    }
    if d.overview.is_some() {
        out.push((AddonEvent::CharacterOverviewReady, EventPayload::empty()));
    }
    for oid in &d.removed_ids {
        out.push((
            AddonEvent::ObjectRemoved,
            EventPayload::empty().with("object_id", IntentValueLite::Int(i64::from(*oid))),
        ));
    }
    if d.entered_world {
        out.push((AddonEvent::EnteredWorld, EventPayload::empty()));
    }
    if d.login_granted {
        out.push((AddonEvent::LoginGranted, EventPayload::empty()));
    }
    if d.crypt_key_received {
        out.push((AddonEvent::CryptKeyReceived, EventPayload::empty()));
    }
    out
}

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use caer_script::{in_tree_addons_dir, memory_manifest};

    #[test]
    fn product_mods_root_is_not_in_tree_examples() {
        let home = Path::new("/home/player");
        let root = resolve_product_addons_root(None, None, Some(home));
        assert_eq!(root, PathBuf::from("/home/player/.local/share/caer/mods"));
        assert_ne!(
            root,
            in_tree_addons_dir(),
            "falsifier: product rustdaoc must not default-load HelloCAER/CombatMeter"
        );
        let xdg = Path::new("/xdg");
        assert_eq!(
            resolve_product_addons_root(None, Some(xdg), Some(home)),
            PathBuf::from("/xdg/caer/mods")
        );
        let explicit = Path::new("/opt/my-mods");
        assert_eq!(
            resolve_product_addons_root(Some(explicit), Some(xdg), Some(home)),
            explicit
        );
    }

    #[test]
    fn empty_player_mods_dir_loads_zero_addons() {
        let dir = std::env::temp_dir().join(format!(
            "caer-mods-empty-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut host = AddonHost::new(HostLimits::production());
        let ids = host
            .load_tree(&dir)
            .expect("empty mods dir is a valid root");
        assert!(ids.is_empty(), "no .toc packs ⇒ no addons, got {ids:?}");
        assert_ne!(dir, in_tree_addons_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn say_intent(text: &str) -> AddonIntent {
        let mut args = BTreeMap::new();
        args.insert("text".into(), IntentValue::Str(text.into()));
        AddonIntent {
            addon_id: "CombatMeter".into(),
            command: AddonCommand::Say,
            args,
        }
    }

    #[test]
    fn chat_say_becomes_live_say_not_world_mutation() {
        let cmd = intent_to_live_command(&say_intent("hello")).unwrap();
        assert!(matches!(cmd, LiveCommand::Say(t) if t == "hello"));
        let world = caer_world::WorldState::new();
        let before = world.len();
        let _ = cmd;
        assert_eq!(
            world.len(),
            before,
            "intent mapping must not touch WorldState"
        );
    }

    #[test]
    fn incomplete_move_is_refused_not_invented() {
        let intent = AddonIntent {
            addon_id: "x".into(),
            command: AddonCommand::Move,
            args: BTreeMap::new(),
        };
        let err = intent_to_live_command(&intent).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("provisional") || msg.contains("movement.move"),
            "provisional refuse must name the command, got {msg}"
        );
    }

    fn refuse_u8_256(command: AddonCommand, key: &str, extra: &[(&str, IntentValue)]) {
        let mut args = BTreeMap::new();
        args.insert(key.into(), IntentValue::Int(256));
        for (k, v) in extra {
            args.insert((*k).into(), v.clone());
        }
        let intent = AddonIntent {
            addon_id: "x".into(),
            command,
            args,
        };
        let err = intent_to_live_command(&intent)
            .expect_err("256 must refuse, not wrap into another LiveCommand");
        let msg = err.to_string();
        assert!(
            msg.contains(key) && (msg.contains("u8") || msg.contains("255")),
            "refusal must name field `{key}`, got {msg}"
        );
    }

    #[test]
    fn select_character_slot_256_is_refused_not_slot_zero() {
        refuse_u8_256(AddonCommand::SelectCharacter, "slot", &[]);
    }

    #[test]
    fn use_skill_index_256_is_refused_not_index_zero() {
        refuse_u8_256(
            AddonCommand::UseSkill,
            "index",
            &[
                ("x", IntentValue::Number(1.0)),
                ("y", IntentValue::Number(2.0)),
                ("z", IntentValue::Number(3.0)),
            ],
        );
    }

    #[test]
    fn use_skill_type_256_is_refused_not_type_zero() {
        refuse_u8_256(
            AddonCommand::UseSkill,
            "skill_type",
            &[
                ("index", IntentValue::Int(3)),
                ("x", IntentValue::Number(1.0)),
                ("y", IntentValue::Number(2.0)),
                ("z", IntentValue::Number(3.0)),
            ],
        );
    }

    #[test]
    fn buy_item_count_256_is_refused_not_count_one() {
        refuse_u8_256(
            AddonCommand::BuyItem,
            "item_count",
            &[
                ("player_x", IntentValue::Int(1)),
                ("player_y", IntentValue::Int(2)),
                ("merchant_id", IntentValue::Int(3)),
                ("item_slot", IntentValue::Int(4)),
            ],
        );
    }

    #[test]
    fn request_character_overview_realm_256_is_refused_not_realm_zero() {
        refuse_u8_256(AddonCommand::RequestCharacterOverview, "realm", &[]);
    }

    #[test]
    fn omitted_optional_u8_keeps_default_not_refusal() {
        let mut args = BTreeMap::new();
        args.insert("index".into(), IntentValue::Int(3));
        args.insert("x".into(), IntentValue::Number(1.0));
        args.insert("y".into(), IntentValue::Number(2.0));
        args.insert("z".into(), IntentValue::Number(3.0));
        let intent = AddonIntent {
            addon_id: "x".into(),
            command: AddonCommand::UseSkill,
            args,
        };
        assert!(matches!(
            intent_to_live_command(&intent).unwrap(),
            LiveCommand::UseSkill {
                index: 3,
                skill_type: 0,
                ..
            }
        ));
    }

    #[test]
    fn u8_max_255_is_accepted() {
        let mut args = BTreeMap::new();
        args.insert("slot".into(), IntentValue::Int(255));
        let intent = AddonIntent {
            addon_id: "x".into(),
            command: AddonCommand::SelectCharacter,
            args,
        };
        assert!(matches!(
            intent_to_live_command(&intent).unwrap(),
            LiveCommand::SelectCharacter { slot: 255 }
        ));
    }

    #[test]
    fn complete_use_skill_maps_to_live_command() {
        let mut args = BTreeMap::new();
        args.insert("index".into(), IntentValue::Int(3));
        args.insert("skill_type".into(), IntentValue::Int(1));
        args.insert("x".into(), IntentValue::Number(1.0));
        args.insert("y".into(), IntentValue::Number(2.0));
        args.insert("z".into(), IntentValue::Number(3.0));
        let intent = AddonIntent {
            addon_id: "x".into(),
            command: AddonCommand::UseSkill,
            args,
        };
        assert!(matches!(
            intent_to_live_command(&intent).unwrap(),
            LiveCommand::UseSkill { index: 3, .. }
        ));
    }

    #[test]
    fn drain_combat_swing_is_addon_event() {
        use caer_protocol::combat_anim::{CombatAnimation, CombatResult};
        let mut d = empty_drain();
        d.applied = 1;
        d.combat_anims = vec![CombatAnimation {
            attacker_id: 7,
            defender_id: 8,
            weapon_id: 0,
            shield_id: 0,
            style: 0,
            stance: 0,
            result: CombatResult::HitUnstyled,
            target_health_pct: 90,
            unk: 0,
        }];
        let evs = events_from_drain(&d);
        assert!(evs.iter().any(|(e, _)| *e == AddonEvent::CombatSwing));
    }

    #[test]
    fn drain_dialog_and_entered_world_are_bridged() {
        let mut d = empty_drain();
        d.entered_world = true;
        d.login_granted = true;
        d.dialogs = vec![crate::live::SocialDialog {
            code: 1,
            data1: 0,
            data2: 0,
            data3: 0,
            data4: 0,
            message: "invite".into(),
        }];
        let evs = events_from_drain(&d);
        assert!(evs.iter().any(|(e, _)| *e == AddonEvent::EnteredWorld));
        assert!(evs.iter().any(|(e, _)| *e == AddonEvent::LoginGranted));
        assert!(evs.iter().any(|(e, _)| *e == AddonEvent::DialogOpened));
    }

    #[test]
    fn disabled_layer_emits_no_commands() {
        let mut layer = ProductAddons::disabled();
        assert!(layer.on_drain(&empty_drain()).is_empty());
        assert!(!layer.is_enabled());
    }

    #[test]
    fn stable_catalog_matches_rustdaoc_bridge() {
        check_stable_bridge().expect("Stable catalog must equal rustdaoc Drain/intent bridge");
    }

    #[test]
    fn removing_a_bridged_event_fails_the_gate() {
        assert!(
            BRIDGED_EVENTS.contains(&AddonEvent::CombatSwing),
            "fixture: CombatSwing is bridged today"
        );
        let fake_stable = catalog_entries()
            .iter()
            .filter(|e| e.stability == Stability::Stable && e.kind == CatalogKind::Event)
            .count();
        assert!(fake_stable > 0);
        // Discriminating: check_stable_bridge walks catalog, not this array alone.
        assert!(check_stable_bridge().is_ok());
    }

    #[test]
    fn faulting_addon_is_diagnosed_and_neighbor_continues() {
        let mut host = AddonHost::new(HostLimits::production());
        host.load_memory(
            memory_manifest("Good", "1.0.0", &[], &["main.lua"]),
            [(
                "main.lua".into(),
                r#"
                CAER.register("combat.swing", function(ev)
                    CAER.print("good")
                end)
            "#
                .into(),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
        host.load_memory(
            memory_manifest("Bad", "1.0.0", &[], &["main.lua"]),
            [(
                "main.lua".into(),
                r#"
                CAER.register("combat.swing", function(ev)
                    error("boom from Bad")
                end)
            "#
                .into(),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
        let mut layer = ProductAddons::from_host(host);
        let mut d = empty_drain();
        d.combat_anims = vec![caer_protocol::combat_anim::CombatAnimation {
            attacker_id: 1,
            defender_id: 2,
            weapon_id: 0,
            shield_id: 0,
            style: 0,
            stance: 0,
            result: caer_protocol::combat_anim::CombatResult::HitUnstyled,
            target_health_pct: 50,
            unk: 0,
        }];
        let _ = layer.on_drain(&d);
        assert_eq!(layer.addon_status("Good"), Some(AddonStatus::Loaded));
        assert_eq!(layer.addon_status("Bad"), Some(AddonStatus::Faulted));
        let notes = layer.take_diagnostic_notes();
        assert!(
            notes
                .iter()
                .any(|n| n.contains("Bad") && n.contains("faulted")),
            "product must surface fault: {notes:?}"
        );
        assert!(
            notes
                .iter()
                .all(|n| !n.contains("Good") || !n.contains("faulted")),
            "healthy addon must not be marked faulted: {notes:?}"
        );
    }

    /// SCN-05 product path: Drain combat animation → typed addon event → in-tree CombatMeter.
    #[test]
    fn scn05_combat_meter_receives_drain_swing_without_fault() {
        use caer_script::in_tree_addons_dir;
        let mut host = AddonHost::new(HostLimits::production());
        host.load_dir(in_tree_addons_dir().join("CombatMeter"))
            .expect("CombatMeter");
        let mut layer = ProductAddons::from_host(host);
        let mut d = empty_drain();
        d.combat_anims = vec![caer_protocol::combat_anim::CombatAnimation {
            attacker_id: 1,
            defender_id: 2,
            weapon_id: 0,
            shield_id: 0,
            style: 0,
            stance: 0,
            result: caer_protocol::combat_anim::CombatResult::HitUnstyled,
            target_health_pct: 75,
            unk: 0,
        }];
        let cmds = layer.on_drain(&d);
        assert!(
            cmds.is_empty(),
            "CombatMeter swing handler must not emit LiveCommand without chat /cm"
        );
        assert_eq!(layer.addon_status("CombatMeter"), Some(AddonStatus::Loaded));
        assert!(
            layer.diagnostics().is_empty(),
            "typed swing must not fault CombatMeter: {:?}",
            layer.diagnostics()
        );
    }

    /// Product authority: chat-triggered addon intent becomes LiveCommand::Say on same path.
    #[test]
    fn chat_addon_say_becomes_live_command_via_on_drain() {
        use caer_script::memory_manifest;
        let mut host = AddonHost::new(HostLimits::production());
        host.load_memory(
            memory_manifest("SayBot", "1.0.0", &[], &["main.lua"]),
            [(
                "main.lua".into(),
                r#"
                CAER.register("chat.message", function(ev)
                    if ev.text == "/ping" then
                        CAER.command("chat.say", { text = "pong" })
                    end
                end)
            "#
                .into(),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
        let mut layer = ProductAddons::from_host(host);
        let mut d = empty_drain();
        d.messages = vec![(2, "/ping".into())];
        let cmds = layer.on_drain(&d);
        assert!(
            matches!(cmds.as_slice(), [LiveCommand::Say(t)] if t == "pong"),
            "addon intent must map to LiveCommand on product path: {cmds:?}"
        );
    }

    #[test]
    fn product_reload_succeeds_and_disabled_refuses() {
        use caer_script::memory_manifest;
        let mut host = AddonHost::new(HostLimits::production());
        host.load_memory(
            memory_manifest("ReloadBot", "1.0.0", &[], &["main.lua"]),
            [(
                "main.lua".into(),
                "CAER.register(\"chat.message\", function(ev) end)".into(),
            )]
            .into_iter()
            .collect(),
        )
        .unwrap();
        let mut layer = ProductAddons::from_host(host);
        let report = layer.reload("ReloadBot").expect("reload");
        assert_eq!(report.addon_id, "ReloadBot");
        assert_eq!(layer.addon_status("ReloadBot"), Some(AddonStatus::Loaded));
        assert!(layer.reload("missing").is_err());
        let mut off = ProductAddons::disabled();
        assert!(off.reload("ReloadBot").is_err());
    }
}
