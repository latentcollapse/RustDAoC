//! Adapters: the named hooks a skin reads game state through.
//!
//! A skin's labels do not hold text — they hold an ADAPTER NAME, and the client fills the value in
//! each frame. `atlantis` names 152 distinct ones (`time_of_day`, `player_health`, `group_name0`…),
//! and together they are the entire contract between the interface and the game. That makes this
//! list the useful roadmap for the rest of the client: an adapter with nothing behind it is a game
//! system not yet ported.
//!
//! Unknown adapters return `None`, which draws an empty label rather than the designer's
//! placeholder — the skins ship filler text like `"Bizquit"` in the clock, and showing that to a
//! player would look far more broken than showing nothing.

use caer_protocol::status::PlayerStatus;

/// A read-only snapshot of everything the UI can currently bind to.
///
/// Borrowed rather than owned so building one per frame costs nothing; it is a view over state the
/// client already holds, not a copy of it.
pub struct AdapterState<'a> {
    pub player_name: &'a str,
    pub status: &'a PlayerStatus,
    /// Selected entity's name and health percent.
    pub target: Option<(&'a str, u8)>,
    /// Server-reported zone/region name, when known.
    pub zone: Option<&'a str>,
    pub fps: u32,
    /// The player's own sheet (0x16 subcode 0x03), once the server has sent it. `None` before then,
    /// which keeps level/realm-rank/title fields EMPTY rather than showing a placeholder.
    pub sheet: Option<&'a caer_protocol::charsheet::CharacterSheet>,
    /// StatsUpdate 0xFB attributes. `None` until first attribute packet.
    pub char_stats: Option<&'a caer_protocol::stats_update::CharStatsUpdate>,
    /// StatsUpdate 0xFB resists. `None` until first resist packet.
    pub char_resists: Option<&'a caer_protocol::stats_update::ResistBlock>,
    /// Player/NPC visible equipment (0x15). `None` until the server has sent an update — adapters
    /// must not invent slots (REQ-006).
    pub equipment: Option<&'a caer_protocol::equipment::EquipmentUpdate>,
    /// Local-player purse (0xFA). `None` until MoneyUpdate — adapters must not invent coin.
    pub money: Option<&'a caer_protocol::money::MoneyUpdate>,
    /// Local-player inventory slot map (0x02). `None` until InventoryUpdate.
    pub inventory:
        Option<&'a std::collections::HashMap<u8, Option<caer_protocol::inventory::ItemData>>>,
    /// Open merchant page (0x17). `None` until MerchantWindow.
    pub merchant: Option<&'a caer_protocol::merchant::MerchantWindow>,
    /// VariousUpdate 0x16:0x05 weapon/AF. `None` until first packet.
    pub weapon_armor: Option<&'a caer_protocol::weapon_armor::WeaponArmorStats>,
    /// Login dialog account field (`username_text` in `pregame/login.xml`).
    pub login_account: Option<&'a str>,
    /// Login dialog password mask (`password_text`) — never the real password.
    pub login_password_mask: Option<&'a str>,
    /// Character-create name edit (`name_edit` in `character_creation.xml`).
    pub create_name: Option<&'a str>,
    /// AttackMode 0x74. `None` until first packet.
    pub attack_mode: Option<bool>,
}

/// Resolve one adapter name against the current state.
///
/// Returns `None` for anything not yet backed by a real system — deliberately, so a half-ported
/// system shows an empty field instead of a plausible-looking lie.
#[must_use]
pub fn resolve(state: &AdapterState<'_>, adapter: &str) -> Option<String> {
    let s = state.status;
    // The skins use the literal name "none" for a label that is decorative, not bound.
    if adapter.eq_ignore_ascii_case("none") || adapter.is_empty() {
        return None;
    }
    Some(match adapter {
        // NOTE: these are the skin's OWN names, taken from the shipped XML. An earlier version of
        // this module used invented ones (`player_health`, `target_name`) and scored 0/151 against
        // the real skin — the names have to come from the data, not from what seems reasonable.

        // Vitals, as the summary window names them. The skins show current/max together.
        // Default PlayerStatus max=0 is the pre-0xAD placeholder — stay unbound.
        "summary_player_hits" => {
            if s.max_health == 0 {
                return None;
            }
            format!("{}/{}", s.health, s.max_health)
        }
        "summary_player_power" => {
            if s.max_mana == 0 {
                return None;
            }
            format!("{}/{}", s.mana, s.max_mana)
        }
        "summary_player_end" => {
            if s.max_endurance == 0 {
                return None;
            }
            format!("{}/{}", s.endurance, s.max_endurance)
        }

        // Target, from the same window. Absent (not blank) with nothing selected, so a stale name
        // cannot linger on screen.
        "summary_target" => state.target?.0.to_string(),
        "summary_target_hits" => format!("{}%", state.target?.1),

        "player_name" => state.player_name.to_string(),

        // Pregame login / create adapters (authored names from pregame/*.xml).
        "username_text" => state.login_account?.to_string(),
        "password_text" => state.login_password_mask?.to_string(),
        "name_edit" => state.create_name?.to_string(),

        // Map chrome — zone name from region tables / RegionChanged, never invented.
        "map_title" | "map_info" | "map_info0" => state.zone?.to_string(),

        // Ground-loot interact body: targeted entity name from create + select (REQ-006).
        "interact_text" => state.target?.0.to_string(),

        // Concentration bar from 0xAD. Default placeholder max=0 stays unbound.
        "concentration" => {
            if s.max_concentration == 0 {
                return None;
            }
            format!("{}/{}", s.concentration, s.max_concentration)
        }

        // Menu-bar FPS. Zero is "not sampled yet", not a real reading.
        "framerate_meter" => {
            if state.fps == 0 {
                return None;
            }
            state.fps.to_string()
        }

        // Character sheet. These were the unbound adapters on the summary window; they resolve
        // only once the sheet has arrived, so an unloaded field stays blank instead of reading 0.
        "stats_level" => state.sheet?.level.to_string(),
        "stats_name" => state.sheet?.name.clone(),
        "realm_level" => state.sheet?.realm_level.to_string(),
        "realm_rank" => state.sheet?.realm_rank_title.clone(),
        "master_rank" => state.sheet?.master_level.to_string(),
        "summary_title" => state.sheet?.title.clone(),
        "guild_name" => state.sheet?.guild_name.clone(),
        "stats_hitpoints" => state.sheet?.max_health.to_string(),
        "stats_race" => {
            let r = &state.sheet?.race_name;
            if r.is_empty() {
                return None;
            }
            r.clone()
        }
        "stats_profession" => {
            let p = &state.sheet?.profession;
            if p.is_empty() {
                return None;
            }
            p.clone()
        }
        "stats_class" => {
            let c = &state.sheet?.class_name;
            if c.is_empty() {
                return None;
            }
            c.clone()
        }
        "stats_spec_points" => state.sheet?.realm_specialty_points.to_string(),
        "summary_champ_level" => {
            let cl = state.sheet?.champion_level;
            if cl == 0 {
                return None;
            }
            // Oracle writes ChampionLevel+1; UI shows the champion level itself.
            cl.saturating_sub(1).to_string()
        }
        "zone_name" => state.zone?.to_string(),

        // Attribute totals from StatsUpdate 0xFB (PacketLib175). Unbound until first packet.
        "stats_strength" => state.char_stats?.total.strength.to_string(),
        "stats_dexterity" => state.char_stats?.total.dexterity.to_string(),
        "stats_constitution" => state.char_stats?.total.constitution.to_string(),
        "stats_quickness" => state.char_stats?.total.quickness.to_string(),
        "stats_intelligence" => state.char_stats?.total.intelligence.to_string(),
        "stats_piety" => state.char_stats?.total.piety.to_string(),
        "stats_empathy" => state.char_stats?.total.empathy.to_string(),
        "stats_charisma" => state.char_stats?.total.charisma.to_string(),
        "stats_crush" => state.char_resists?.crush.to_string(),
        "stats_slash" => state.char_resists?.slash.to_string(),
        "stats_thrust" => state.char_resists?.thrust.to_string(),
        "stats_heat" => state.char_resists?.heat.to_string(),
        "stats_cold" => state.char_resists?.cold.to_string(),
        "stats_matter" => state.char_resists?.matter.to_string(),
        "stats_body" => state.char_resists?.body.to_string(),
        "stats_spirit" => state.char_resists?.spirit.to_string(),
        "stats_energy" => state.char_resists?.energy.to_string(),

        // Weapon / AF from VariousUpdate 0x16:0x05.
        "stats_weapon_damage" => state.weapon_armor?.weapon_damage_display(),
        "stats_weapon_skill" => state.weapon_armor?.weapon_skill.to_string(),
        "stats_armor_factor" => state.weapon_armor?.armor_factor.to_string(),

        // Attack stance from AttackMode 0x74.
        "attack_mode" => {
            let atk = state.attack_mode?;
            if atk { "Attacking" } else { "Passive" }.to_string()
        }

        // Paperdoll / equipment slots — only resolve from a real 0x15 (REQ-006 / MS-02).
        "paperdoll_slot_torso" => state
            .equipment?
            .item(caer_protocol::equipment::slot::TORSO)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_arms" => state
            .equipment?
            .item(caer_protocol::equipment::slot::ARMS)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_legs" => state
            .equipment?
            .item(caer_protocol::equipment::slot::LEGS)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_feet" => state
            .equipment?
            .item(caer_protocol::equipment::slot::FEET)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_helm" => state
            .equipment?
            .item(caer_protocol::equipment::slot::HELM)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_righthand" => state
            .equipment?
            .item(caer_protocol::equipment::slot::RIGHTHAND)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_lefthand" => state
            .equipment?
            .item(caer_protocol::equipment::slot::LEFTHAND)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_cloak" => state
            .equipment?
            .item(caer_protocol::equipment::slot::CLOAK)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_hands" => state
            .equipment?
            .item(caer_protocol::equipment::slot::HANDS)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_twohand" => state
            .equipment?
            .item(caer_protocol::equipment::slot::TWOHAND)
            .map(|i| i.model.to_string())?,
        "paperdoll_slot_ranged" => state
            .equipment?
            .item(caer_protocol::equipment::slot::RANGED)
            .map(|i| i.model.to_string())?,

        // Player purse — Mythic menu_bar / inventory chrome uses the `merchant_*` adapter names
        // for the local purse (not merchant-offer prices). Only from MoneyUpdate 0xFA (REQ-006).
        "merchant_copper" => state.money?.copper.to_string(),
        "merchant_silver" => state.money?.silver.to_string(),
        "merchant_gold" => state.money?.gold.to_string(),
        "merchant_platinum" => state.money?.platinum.to_string(),
        "merchant_mithril" => state.money?.mithril.to_string(),

        // Merchant catalogue chrome — page label from MerchantWindow 0x17 only (REQ-006).
        // `merchant_title`: NPC name is not on 0x17 — product binds selected target while merchant open.
        "merchant_title" => {
            let _ = state.merchant?;
            state.target?.0.to_string()
        }
        "merchant_page_display" => {
            // Packet carries page index only — do not invent a total page count.
            let m = state.merchant?;
            format!("Page {}", m.page + 1)
        }
        // Listbox AdapterName from Atlantis `merchant_window.xml`. Offer *names* only — never
        // invent catalogue rows (REQ-006). Falsifier: `merchant_page0_requires_merchant_window`.
        "merchant_page0" => {
            let m = state.merchant?;
            let mut rows: Vec<(u8, &str)> =
                m.items.iter().map(|i| (i.slot, i.name.as_str())).collect();
            rows.sort_by_key(|(slot, _)| *slot);
            rows.into_iter()
                .map(|(_, name)| name)
                .collect::<Vec<_>>()
                .join("\n")
        }

        // Inventory item *names* for worn slots — from InventoryUpdate 0x02 only (REQ-006).
        // Distinct from paperdoll model ids (0x15) so SCN-06 can prove both sources.
        "inventory_slot_torso" => state
            .inventory?
            .get(&caer_protocol::equipment::slot::TORSO)?
            .as_ref()?
            .name
            .clone(),
        "inventory_slot_arms" => state
            .inventory?
            .get(&caer_protocol::equipment::slot::ARMS)?
            .as_ref()?
            .name
            .clone(),
        "inventory_slot_legs" => state
            .inventory?
            .get(&caer_protocol::equipment::slot::LEGS)?
            .as_ref()?
            .name
            .clone(),
        "inventory_slot_feet" => state
            .inventory?
            .get(&caer_protocol::equipment::slot::FEET)?
            .as_ref()?
            .name
            .clone(),
        "inventory_slot_helm" => state
            .inventory?
            .get(&caer_protocol::equipment::slot::HELM)?
            .as_ref()?
            .name
            .clone(),
        "inventory_slot_cloak" => state
            .inventory?
            .get(&caer_protocol::equipment::slot::CLOAK)?
            .as_ref()?
            .name
            .clone(),
        "inventory_slot_hands" => state
            .inventory?
            .get(&caer_protocol::equipment::slot::HANDS)?
            .as_ref()?
            .name
            .clone(),
        "inventory_slot_righthand" => state
            .inventory?
            .get(&caer_protocol::equipment::slot::RIGHTHAND)?
            .as_ref()?
            .name
            .clone(),
        "inventory_slot_lefthand" => state
            .inventory?
            .get(&caer_protocol::equipment::slot::LEFTHAND)?
            .as_ref()?
            .name
            .clone(),

        // Everything else is a system not yet ported. Listing them explicitly is pointless — the
        // useful signal is the COUNT, which `adapter_coverage` reports.
        _ => return None,
    })
}

/// Extra packet-derived social bind state. Kept off [`AdapterState`] so rustdaoc's existing
/// struct literals stay valid until INT wires the product loop.
#[derive(Clone, Copy, Debug, Default)]
pub struct SocialBind<'a> {
    pub group: Option<&'a caer_protocol::social::GroupWindow>,
    pub group_vitals: Option<&'a caer_protocol::social::GroupMemberUpdate>,
    pub quests: Option<&'a [caer_protocol::quest::QuestEntry]>,
    pub trade: Option<&'a caer_protocol::social::TradeWindow>,
    /// Pending dialog body (stock `dialog_message` adapter). Empty/none → Accept must stay blind.
    pub dialog_message: Option<&'a str>,
    /// Current trade terms text (stock `trade_terms` adapter).
    pub trade_terms: Option<&'a str>,
}

/// Resolve group/quest/trade adapters from authoritative inbound packets only.
#[must_use]
pub fn resolve_social(bind: &SocialBind<'_>, adapter: &str) -> Option<String> {
    if let Some(rest) = adapter.strip_prefix("group_name") {
        let idx: usize = rest.parse().ok()?;
        return Some(bind.group?.members.get(idx)?.name.clone());
    }
    if let Some(rest) = adapter.strip_prefix("group_class") {
        // PacketLib1125 SendGroupWindowUpdate: salutation = player class / "NPC".
        let idx: usize = rest.parse().ok()?;
        let s = &bind.group?.members.get(idx)?.salutation;
        if s.is_empty() {
            return None;
        }
        return Some(s.clone());
    }
    if let Some(rest) = adapter.strip_prefix("group_level") {
        let idx: usize = rest.parse().ok()?;
        return Some(bind.group?.members.get(idx)?.level.to_string());
    }
    if let Some(rest) = adapter.strip_prefix("group_endurance") {
        let idx: usize = rest.parse().ok()?;
        let v = bind
            .group_vitals?
            .members
            .iter()
            .find(|m| m.index as usize == idx)?;
        return Some(format!("{}%", v.endurance_pct));
    }
    if let Some(rest) = adapter.strip_prefix("group_health") {
        let idx: usize = rest.parse().ok()?;
        let v = bind
            .group_vitals?
            .members
            .iter()
            .find(|m| m.index as usize == idx)?;
        return Some(format!("{}%", v.health_pct));
    }
    if let Some(rest) = adapter.strip_prefix("group_power") {
        let idx: usize = rest.parse().ok()?;
        let v = bind
            .group_vitals?
            .members
            .iter()
            .find(|m| m.index as usize == idx)?;
        return Some(format!("{}%", v.mana_pct));
    }
    match adapter {
        "quest_title" | "new_quest_title" => {
            let q = bind.quests?.iter().find(|e| !e.is_clear())?;
            Some(q.name.clone())
        }
        "interact_title" if bind.trade.is_some_and(|t| !t.closed) => {
            Some(bind.trade?.caption.clone()).filter(|s| !s.is_empty())
        }
        "dialog_message" => {
            let m = bind.dialog_message.filter(|s| !s.is_empty())?;
            Some(m.to_string())
        }
        "trade_terms" => {
            let m = bind.trade_terms.filter(|s| !s.is_empty())?;
            Some(m.to_string())
        }
        "trade_partner" => Some(bind.trade?.caption.clone()).filter(|s| !s.is_empty()),
        _ => None,
    }
}

/// Packet-derived UI fields kept off [`AdapterState`] so rustdaoc struct literals stay valid.
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtraBind<'a> {
    pub overview: Option<&'a caer_protocol::overview::CharacterOverview>,
    pub chat_entry: Option<&'a str>,
    pub skills: Option<&'a [caer_protocol::skills::Skill]>,
    /// CharacterPointsUpdate 0x91 — realm/bounty/champ XP bar.
    pub points: Option<&'a caer_protocol::points::CharacterPoints>,
    /// TimerWindow 0xF3 open form.
    pub timer: Option<&'a caer_protocol::shape2_loop::TimerWindow>,
    /// Local merchant buy quantity (product UI; not on MerchantWindow 0x17).
    pub merchant_quantity: Option<&'a str>,
    /// Character-create class name label (`class_label_text`).
    pub create_class_label: Option<&'a str>,
    /// Character-create class description (`class_desc_label_text`).
    pub create_class_desc: Option<&'a str>,
    /// Character-create race description (`race_desc_label_text`).
    pub create_race_desc: Option<&'a str>,
}

/// Character-select / chat-entry / skill-list adapters. Unbound without the named packet.
#[must_use]
pub fn resolve_extra(bind: &ExtraBind<'_>, adapter: &str) -> Option<String> {
    if adapter == "chat_entry" {
        let t = bind.chat_entry.filter(|s| !s.is_empty())?;
        return Some(t.to_string());
    }
    if adapter == "timer_text" {
        let t = bind.timer.filter(|w| w.open)?;
        if t.title.is_empty() {
            return Some(format!("{}s", t.seconds));
        }
        return Some(format!("{} — {}s", t.title, t.seconds));
    }
    if adapter == "merchant_quantity" {
        let q = bind.merchant_quantity.filter(|s| !s.is_empty())?;
        return Some(q.to_string());
    }
    if adapter == "class_label_text" {
        let t = bind.create_class_label.filter(|s| !s.is_empty())?;
        return Some(t.to_string());
    }
    if adapter == "class_desc_label_text" {
        let t = bind.create_class_desc.filter(|s| !s.is_empty())?;
        return Some(t.to_string());
    }
    if adapter == "race_desc_label_text" {
        let t = bind.create_race_desc.filter(|s| !s.is_empty())?;
        return Some(t.to_string());
    }
    if adapter == "stats_realm_points" || adapter == "realm_points" {
        return Some(bind.points?.realm_points.to_string());
    }
    if adapter == "bounty_points" {
        return Some(bind.points?.bounty_points.to_string());
    }
    if let Some(rest) = adapter.strip_prefix("char_slot") {
        let (idx_s, field) = rest.split_once('_')?;
        let idx: usize = idx_s.parse().ok()?;
        let c = bind.overview?.characters.get(idx)?;
        return Some(match field {
            "name" => c.name.clone(),
            "level" => c.level.to_string(),
            "class" => c.class_name.clone(),
            "location" => c.location.clone(),
            _ => return None,
        })
        .filter(|s| !s.is_empty());
    }
    if adapter == "stats_specs" || adapter == "stats_abil" || adapter == "stats_spells" {
        let skills = bind.skills?;
        let kind = match adapter {
            "stats_specs" => caer_protocol::skills::SkillKind::Specialization,
            "stats_abil" => caer_protocol::skills::SkillKind::Ability,
            "stats_spells" => caer_protocol::skills::SkillKind::Spell,
            _ => return None,
        };
        let names: Vec<&str> = skills
            .iter()
            .filter(|s| s.kind == kind)
            .map(|s| s.name.as_str())
            .collect();
        if names.is_empty() {
            return None;
        }
        return Some(names.join("\n"));
    }
    None
}

/// Packet/table provenance for a bound adapter. `None` means the adapter must not be reported
/// as bound — uibind will not count a value without a named source.
#[must_use]
pub fn adapter_provenance(adapter: &str) -> Option<&'static str> {
    Some(match adapter {
        "summary_player_hits" | "summary_player_power" | "summary_player_end" => {
            "CharacterStatusUpdate 0xAD"
        }
        "summary_target" | "summary_target_hits" => "selected entity + StatusUpdate",
        "player_name"
        | "stats_name"
        | "stats_level"
        | "realm_level"
        | "realm_rank"
        | "master_rank"
        | "summary_title"
        | "guild_name"
        | "stats_hitpoints"
        | "stats_race"
        | "stats_profession"
        | "stats_class"
        | "stats_spec_points"
        | "summary_champ_level" => "CharacterSheet VariousUpdate 0x16:0x03",
        "stats_strength" | "stats_dexterity" | "stats_constitution" | "stats_quickness"
        | "stats_intelligence" | "stats_piety" | "stats_empathy" | "stats_charisma"
        | "stats_crush" | "stats_slash" | "stats_thrust" | "stats_heat" | "stats_cold"
        | "stats_matter" | "stats_body" | "stats_spirit" | "stats_energy" => {
            caer_protocol::stats_update::PROVENANCE
        }
        "stats_weapon_damage" | "stats_weapon_skill" | "stats_armor_factor" => {
            caer_protocol::weapon_armor::PROVENANCE
        }
        "stats_realm_points" | "realm_points" | "bounty_points" => {
            caer_protocol::points::POINTS_PROVENANCE
        }
        "zone_name" | "map_title" | "map_info" | "map_info0" => {
            "region/zone tables + RegionChanged 0xB7"
        }
        "interact_text" => "selected entity name",
        "concentration" => "CharacterStatusUpdate 0xAD",
        "framerate_meter" => "client frame counter",
        "chat_entry" => "Message/Command 0xAF input line",
        "timer_text" => "TimerWindow 0xF3",
        "attack_mode" => "AttackMode 0x74",
        a if a.starts_with("char_slot") => "CharacterOverview 0xFC",
        "stats_specs" | "stats_abil" | "stats_spells" => "VariousUpdate 0x16 subcode 0x01 skills",
        a if a.starts_with("paperdoll_slot_") => "EquipmentUpdate 0x15",
        a if a.starts_with("inventory_slot_") => "InventoryUpdate 0x02",
        "merchant_copper" | "merchant_silver" | "merchant_gold" | "merchant_platinum"
        | "merchant_mithril" => "MoneyUpdate 0xFA",
        "merchant_page_display" | "merchant_page0" => "MerchantWindow 0x17",
        "merchant_title" => "selected merchant NPC + MerchantWindow 0x17",
        "merchant_quantity" => "product buy-quantity UI state",
        a if a.starts_with("group_name")
            || a.starts_with("group_level")
            || a.starts_with("group_class") =>
        {
            "GroupWindow VariousUpdate 0x16:0x06"
        }
        a if a.starts_with("group_endurance")
            || a.starts_with("group_health")
            || a.starts_with("group_power") =>
        {
            "GroupMemberUpdate 0x70"
        }
        "quest_title" | "new_quest_title" => "QuestEntry 0x83",
        "interact_title" | "trade_partner" | "trade_terms" => "TradeWindow 0xEA",
        "dialog_message" => "Dialog 0x81",
        "username_text" | "password_text" | "name_edit" => "pregame session fields",
        "class_label_text" | "class_desc_label_text" | "race_desc_label_text" => {
            "pregame character-create draft"
        }
        "none" | "" => return None,
        _ => return None,
    })
}

/// Whether a product resolver arm exists for this adapter name, independent of live packet state.
///
/// Provenance without an arm is `MISSING_RESOLVER` (deleting a resolve arm must not look like
/// waiting on DATA). Decorative `none` is not a resolver arm.
#[must_use]
pub fn has_resolver_arm(adapter: &str) -> bool {
    if adapter.eq_ignore_ascii_case("none") || adapter.is_empty() {
        return false;
    }
    matches!(
        adapter,
        "summary_player_hits"
            | "summary_player_power"
            | "summary_player_end"
            | "summary_target"
            | "summary_target_hits"
            | "player_name"
            | "username_text"
            | "password_text"
            | "name_edit"
            | "map_title"
            | "map_info"
            | "map_info0"
            | "interact_text"
            | "concentration"
            | "framerate_meter"
            | "stats_level"
            | "stats_name"
            | "realm_level"
            | "realm_rank"
            | "master_rank"
            | "summary_title"
            | "guild_name"
            | "stats_hitpoints"
            | "stats_race"
            | "stats_profession"
            | "stats_class"
            | "stats_spec_points"
            | "summary_champ_level"
            | "stats_strength"
            | "stats_dexterity"
            | "stats_constitution"
            | "stats_quickness"
            | "stats_intelligence"
            | "stats_piety"
            | "stats_empathy"
            | "stats_charisma"
            | "stats_crush"
            | "stats_slash"
            | "stats_thrust"
            | "stats_heat"
            | "stats_cold"
            | "stats_matter"
            | "stats_body"
            | "stats_spirit"
            | "stats_energy"
            | "stats_weapon_damage"
            | "stats_weapon_skill"
            | "stats_armor_factor"
            | "stats_realm_points"
            | "zone_name"
            | "paperdoll_slot_torso"
            | "paperdoll_slot_arms"
            | "paperdoll_slot_legs"
            | "paperdoll_slot_feet"
            | "paperdoll_slot_helm"
            | "paperdoll_slot_righthand"
            | "paperdoll_slot_lefthand"
            | "paperdoll_slot_cloak"
            | "paperdoll_slot_hands"
            | "paperdoll_slot_twohand"
            | "paperdoll_slot_ranged"
            | "merchant_copper"
            | "merchant_silver"
            | "merchant_gold"
            | "merchant_platinum"
            | "merchant_mithril"
            | "merchant_page_display"
            | "merchant_page0"
            | "merchant_title"
            | "merchant_quantity"
            | "inventory_slot_torso"
            | "inventory_slot_arms"
            | "inventory_slot_legs"
            | "inventory_slot_feet"
            | "inventory_slot_helm"
            | "inventory_slot_cloak"
            | "inventory_slot_hands"
            | "inventory_slot_righthand"
            | "inventory_slot_lefthand"
            | "quest_title"
            | "new_quest_title"
            | "interact_title"
            | "dialog_message"
            | "trade_terms"
            | "trade_partner"
            | "chat_entry"
            | "timer_text"
            | "attack_mode"
            | "class_label_text"
            | "class_desc_label_text"
            | "race_desc_label_text"
            | "stats_specs"
            | "stats_abil"
            | "stats_spells"
            | "conc_list"
            | "summary_cast_spell"
            | "summary_cast_time"
            | "cast_state"
            | "xp_percent"
            | "bounty_points"
            | "realm_points"
            | "effect_icon0"
            | "concentration_effect0"
            | "combat_result"
            | "combat_result_hp"
            | "pet_oid"
            | "summary_target_con"
    ) || adapter.starts_with("group_name")
        || adapter.starts_with("group_level")
        || adapter.starts_with("group_class")
        || adapter.starts_with("group_endurance")
        || adapter.starts_with("group_health")
        || adapter.starts_with("group_power")
        || adapter.starts_with("char_slot")
}

/// How many of a skin's adapters currently resolve, given a state.
///
/// This is the honest progress metric for the UI: it counts against the skin's real demand rather
/// than against a list of what happens to be implemented.
#[must_use]
pub fn adapter_coverage(state: &AdapterState<'_>, adapters: &[String]) -> (usize, usize) {
    let bound = adapters
        .iter()
        .filter(|a| resolve(state, a).is_some())
        .count();
    (bound, adapters.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> PlayerStatus {
        PlayerStatus {
            health_pct: 62,
            mana_pct: 40,
            endurance_pct: 88,
            concentration_pct: 100,
            sitting: false,
            health: 620,
            max_health: 1000,
            mana: 200,
            max_mana: 500,
            endurance: 88,
            max_endurance: 100,
            concentration: 10,
            max_concentration: 10,
        }
    }

    fn state<'a>(st: &'a PlayerStatus, target: Option<(&'a str, u8)>) -> AdapterState<'a> {
        AdapterState {
            player_name: "Lilillyn",
            status: st,
            target,
            zone: Some("Cornwall"),
            fps: 60,
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

    #[test]
    fn vitals_and_name_resolve() {
        let st = status();
        let s = state(&st, None);
        assert_eq!(resolve(&s, "player_name").as_deref(), Some("Lilillyn"));
        // The skin's own names, not invented ones — see the note in `resolve`.
        assert_eq!(
            resolve(&s, "summary_player_hits").as_deref(),
            Some("620/1000")
        );
        assert_eq!(
            resolve(&s, "summary_player_power").as_deref(),
            Some("200/500")
        );
        assert_eq!(resolve(&s, "summary_player_end").as_deref(), Some("88/100"));
        assert_eq!(resolve(&s, "zone_name").as_deref(), Some("Cornwall"));
    }

    /// The literal adapter "none" — which the skins use 45 times — is decoration, not a binding.
    #[test]
    fn the_none_adapter_is_not_a_binding() {
        let st = status();
        let s = state(&st, None);
        assert_eq!(resolve(&s, "none"), None);
        assert_eq!(resolve(&s, "None"), None, "and it is case-insensitive");
        assert_eq!(resolve(&s, ""), None);
    }

    /// With nothing targeted the target adapters are ABSENT, not blank — a stale target name left
    /// on screen is worse than no name at all.
    #[test]
    fn target_adapters_are_absent_without_a_target() {
        let st = status();
        assert_eq!(resolve(&state(&st, None), "summary_target"), None);
        assert_eq!(resolve(&state(&st, None), "summary_target_hits"), None);

        let s = state(&st, Some(("giant skeleton", 45)));
        assert_eq!(
            resolve(&s, "summary_target").as_deref(),
            Some("giant skeleton")
        );
        assert_eq!(resolve(&s, "summary_target_hits").as_deref(), Some("45%"));
    }

    /// An adapter with no system behind it must return None so the label draws EMPTY, rather than
    /// falling through to the skin's authored placeholder text.
    #[test]
    fn unported_systems_resolve_to_nothing() {
        let st = status();
        let s = state(&st, None);
        for a in [
            "guild_name",
            "bounty_points",
            "group_name0",
            "keep_status_level",
        ] {
            assert_eq!(resolve(&s, a), None, "{a} has no system behind it yet");
        }
    }

    /// Coverage counts against the skin's real demand, so the number cannot flatter itself by
    /// only counting adapters that happen to be implemented.
    #[test]
    fn coverage_counts_against_the_skins_demand() {
        let st = status();
        let s = state(&st, None);
        let demand: Vec<String> = [
            "player_name",
            "summary_player_hits",
            "guild_name",
            "bounty_points",
        ]
        .iter()
        .map(|x| x.to_string())
        .collect();
        assert_eq!(adapter_coverage(&s, &demand), (2, 4));
    }

    /// Paperdoll adapters stay unbound without a real 0x15 (REQ-006) and resolve from equipment
    /// when one is present.
    #[test]
    fn paperdoll_adapters_require_equipment_state() {
        use caer_protocol::codec::PacketWriter;
        use caer_protocol::equipment::{decode, slot};
        let st = status();
        let empty = state(&st, None);
        assert_eq!(resolve(&empty, "paperdoll_slot_torso"), None);

        let mut w = PacketWriter::new();
        w.u16(1).u8(0).u8(0).u8(0).u8(0).u8(1);
        w.u8(slot::TORSO).u16(0x0456).u8(3);
        let eq = decode(w.as_slice()).unwrap();
        let s = AdapterState {
            player_name: "Lilillyn",
            status: &st,
            target: None,
            zone: Some("Cornwall"),
            fps: 60,
            sheet: None,
            char_stats: None,
            char_resists: None,
            equipment: Some(&eq),
            money: None,
            inventory: None,
            merchant: None,
            weapon_armor: None,
            attack_mode: None,
            login_account: None,
            login_password_mask: None,
            create_name: None,
        };
        assert_eq!(resolve(&s, "paperdoll_slot_torso").as_deref(), Some("1110"));
        // 0x0456
    }

    /// Money adapters stay unbound without 0xFA and resolve from MoneyUpdate (REQ-006).
    #[test]
    fn money_adapters_require_money_update() {
        let st = status();
        assert_eq!(resolve(&state(&st, None), "merchant_gold"), None);
        let money = caer_protocol::money::MoneyUpdate {
            copper: 1,
            silver: 2,
            gold: 20,
            mithril: 0,
            platinum: 0,
        };
        let s = AdapterState {
            player_name: "Lilillyn",
            status: &st,
            target: None,
            zone: None,
            fps: 60,
            sheet: None,
            char_stats: None,
            char_resists: None,
            equipment: None,
            money: Some(&money),
            inventory: None,
            merchant: None,
            weapon_armor: None,
            attack_mode: None,
            login_account: None,
            login_password_mask: None,
            create_name: None,
        };
        assert_eq!(resolve(&s, "merchant_gold").as_deref(), Some("20"));
        assert_eq!(resolve(&s, "merchant_copper").as_deref(), Some("1"));
    }

    /// Inventory name adapters stay unbound without 0x02 (REQ-006).
    #[test]
    fn inventory_name_adapters_require_inventory_update() {
        use caer_protocol::equipment::slot;
        use caer_protocol::inventory::ItemData;
        let st = status();
        assert_eq!(resolve(&state(&st, None), "inventory_slot_righthand"), None);
        let mut map = std::collections::HashMap::new();
        map.insert(
            slot::RIGHTHAND,
            Some(ItemData {
                unique_id: 0,
                level: 0,
                value1: 0,
                value2: 0,
                hand_byte: 0,
                object_type_byte: 0,
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
                flag: 2,
                effect: 0,
                name: "practice sword".into(),
            }),
        );
        let s = AdapterState {
            player_name: "Lilillyn",
            status: &st,
            target: None,
            zone: None,
            fps: 60,
            sheet: None,
            char_stats: None,
            char_resists: None,
            equipment: None,
            money: None,
            inventory: Some(&map),
            merchant: None,
            weapon_armor: None,
            attack_mode: None,
            login_account: None,
            login_password_mask: None,
            create_name: None,
        };
        assert_eq!(
            resolve(&s, "inventory_slot_righthand").as_deref(),
            Some("practice sword")
        );
    }

    #[test]
    fn merchant_page_display_from_merchant_window() {
        use caer_protocol::merchant::{MerchantOffer, MerchantWindow};
        let st = status();
        let mw = MerchantWindow {
            window_type: 0,
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
                price: 0,
                model: 1,
                name: "widget".into(),
            }],
        };
        let mut s = state(&st, None);
        s.merchant = Some(&mw);
        assert_eq!(
            resolve(&s, "merchant_page_display").as_deref(),
            Some("Page 1")
        );
        assert_eq!(resolve(&state(&st, None), "merchant_page_display"), None);
    }

    /// Falsifier `merchant_page0_requires_merchant_window`: without a real 0x17 the listbox
    /// adapter is absent (blank UI); with packet offers the names resolve in slot order.
    #[test]
    fn merchant_page0_requires_merchant_window() {
        use caer_protocol::merchant::{MerchantOffer, MerchantWindow};
        let st = status();
        assert_eq!(resolve(&state(&st, None), "merchant_page0"), None);

        let mw = MerchantWindow {
            window_type: 0,
            page: 0,
            items: vec![
                MerchantOffer {
                    slot: 1,
                    level: 1,
                    value1: 0,
                    spd_abs: 0,
                    hand_byte: 0,
                    object_type_byte: 0,
                    usable: true,
                    value2: 0,
                    price: 0,
                    model: 2,
                    name: "second".into(),
                },
                MerchantOffer {
                    slot: 0,
                    level: 1,
                    value1: 0,
                    spd_abs: 0,
                    hand_byte: 0,
                    object_type_byte: 0,
                    usable: true,
                    value2: 0,
                    price: 0,
                    model: 1,
                    name: "first".into(),
                },
            ],
        };
        let mut s = state(&st, None);
        s.merchant = Some(&mw);
        assert_eq!(
            resolve(&s, "merchant_page0").as_deref(),
            Some("first\nsecond"),
            "offers must follow packet slot order, not invent catalogue text"
        );
    }

    /// Falsifier `paperdoll_cloak_hands_require_equipment`: cloak/hands/twohand stay blank
    /// without 0x15 and resolve model ids only from EquipmentUpdate.
    #[test]
    fn paperdoll_cloak_hands_require_equipment() {
        use caer_protocol::codec::PacketWriter;
        use caer_protocol::equipment::{decode, slot};
        let st = status();
        let empty = state(&st, None);
        assert_eq!(resolve(&empty, "paperdoll_slot_cloak"), None);
        assert_eq!(resolve(&empty, "paperdoll_slot_hands"), None);
        assert_eq!(resolve(&empty, "paperdoll_slot_twohand"), None);

        let mut w = PacketWriter::new();
        w.u16(1).u8(0).u8(0).u8(0).u8(0).u8(2);
        w.u8(slot::CLOAK).u16(0x0100).u8(0);
        w.u8(slot::HANDS).u16(0x0200).u8(1);
        let eq = decode(w.as_slice()).unwrap();
        let s = AdapterState {
            player_name: "Lilillyn",
            status: &st,
            target: None,
            zone: None,
            fps: 60,
            sheet: None,
            char_stats: None,
            char_resists: None,
            equipment: Some(&eq),
            money: None,
            inventory: None,
            merchant: None,
            weapon_armor: None,
            attack_mode: None,
            login_account: None,
            login_password_mask: None,
            create_name: None,
        };
        assert_eq!(resolve(&s, "paperdoll_slot_cloak").as_deref(), Some("256"));
        assert_eq!(resolve(&s, "paperdoll_slot_hands").as_deref(), Some("512"));
        assert_eq!(resolve(&s, "paperdoll_slot_twohand"), None);
    }

    /// Falsifier `group_name0_requires_group_window`.
    #[test]
    fn group_name0_requires_group_window() {
        let empty = SocialBind::default();
        assert_eq!(resolve_social(&empty, "group_name0"), None);
        let gw = caer_protocol::social::GroupWindow {
            members: vec![caer_protocol::social::GroupWindowMember {
                name: "Feile".into(),
                salutation: String::new(),
                object_id: 12,
                level: 50,
            }],
        };
        let bind = SocialBind {
            group: Some(&gw),
            ..Default::default()
        };
        assert_eq!(
            resolve_social(&bind, "group_name0").as_deref(),
            Some("Feile")
        );
        assert_eq!(resolve_social(&bind, "group_name1"), None);
        assert_eq!(
            adapter_provenance("group_name0"),
            Some("GroupWindow VariousUpdate 0x16:0x06")
        );
        assert_eq!(
            resolve_social(&bind, "group_class0").as_deref(),
            None,
            "empty salutation stays unbound"
        );
        assert_eq!(
            adapter_provenance("bounty_points"),
            Some("CharacterPointsUpdate 0x91")
        );
    }

    /// Falsifier `map_title_requires_zone`.
    #[test]
    fn map_title_requires_zone() {
        let st = status();
        let mut s = state(&st, None);
        s.zone = None;
        assert_eq!(resolve(&s, "map_title"), None);
        s.zone = Some("Camelot Hills");
        assert_eq!(resolve(&s, "map_title").as_deref(), Some("Camelot Hills"));
        assert_eq!(resolve(&s, "map_info").as_deref(), Some("Camelot Hills"));
    }

    /// Falsifier `interact_text_requires_target`.
    #[test]
    fn interact_text_requires_target() {
        let st = status();
        assert_eq!(resolve(&state(&st, None), "interact_text"), None);
        let s = state(&st, Some(("practice sword", 100)));
        assert_eq!(
            resolve(&s, "interact_text").as_deref(),
            Some("practice sword")
        );
    }

    /// Falsifier `concentration_requires_status_max`.
    #[test]
    fn concentration_requires_status_max() {
        let st = PlayerStatus::default();
        assert_eq!(resolve(&state(&st, None), "concentration"), None);
        let mut live = status();
        live.concentration = 5;
        live.max_concentration = 10;
        assert_eq!(
            resolve(&state(&live, None), "concentration").as_deref(),
            Some("5/10")
        );
    }

    /// Falsifier `char_slot0_name_requires_overview`.
    #[test]
    fn char_slot0_name_requires_overview() {
        assert_eq!(
            resolve_extra(&ExtraBind::default(), "char_slot0_name"),
            None
        );
        let ov = caer_protocol::overview::CharacterOverview {
            flags: 0,
            characters: vec![caer_protocol::overview::CharacterSummary {
                slot: 0,
                level: 50,
                name: "Lilillyn".into(),
                location: "Camelot Hills".into(),
                class_name: "Armsman".into(),
                race_name: "Briton".into(),
                region: 1,
                class_id: 2,
                realm: 1,
                stats: [0; 8],
                race_gender: 0,
                ..Default::default()
            }],
        };
        let bind = ExtraBind {
            overview: Some(&ov),
            ..Default::default()
        };
        assert_eq!(
            resolve_extra(&bind, "char_slot0_name").as_deref(),
            Some("Lilillyn")
        );
        assert_eq!(resolve_extra(&bind, "char_slot1_name"), None);
        assert_eq!(
            adapter_provenance("char_slot0_name"),
            Some("CharacterOverview 0xFC")
        );
    }

    /// Falsifier `chat_entry_requires_typed_line`.
    #[test]
    fn chat_entry_requires_typed_line() {
        assert_eq!(resolve_extra(&ExtraBind::default(), "chat_entry"), None);
        let bind = ExtraBind {
            chat_entry: Some("hello"),
            ..Default::default()
        };
        assert_eq!(resolve_extra(&bind, "chat_entry").as_deref(), Some("hello"));
    }

    #[test]
    fn resolver_arm_is_independent_of_provenance() {
        assert!(has_resolver_arm("stats_level"));
        assert!(has_resolver_arm("char_slot0_name"));
        assert!(has_resolver_arm("group_name0"));
        assert!(has_resolver_arm("stats_strength"));
        assert!(!has_resolver_arm("none"));
        // Provenance without an arm must not masquerade as a resolver.
        assert!(adapter_provenance("stats_level").is_some());
        assert!(adapter_provenance("stats_strength").is_some());
    }
}
