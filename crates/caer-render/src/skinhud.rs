//! Drive the CLIENT's own UI skin in the live client.
//!
//! The skin pipeline (skin XML → art templates → nine-slice quads → bitmap fonts → window manager →
//! adapters) has existed and worked for a while, but only the dev lens and `uilayout` ever called
//! it. `rustdaoc` drew a hand-rolled egui HUD instead — invented panel positions, invented bar
//! sizes, none of it the original's. This module is the bridge, so the live client renders the real
//! interface from `ui/<skin>/*.xml`.
//!
//! CAER is a conversion. A HUD we designed ourselves is exactly the improvisation this project is
//! meant not to contain.
//!
//! Loading is cached: the skin parse and every font atlas happen once, and texture pages upload on
//! first use. Per frame the cost is a layout pass plus one quad-buffer write, which is what the dev
//! lens already does per screenshot.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::adapters::{self, AdapterState};
use crate::gpu::Gpu;
use crate::skinui::{self, UiQuad};

/// A loaded skin plus the font atlases it references.
pub struct SkinHud {
    ui_dir: PathBuf,
    skin: caer_assets::uiskin::Skin,
    fonts: HashMap<String, caer_assets::bitmapfont::BitmapFont>,
    /// Fonts we already tried and failed to load, so a missing atlas is not retried every frame.
    failed_fonts: std::collections::HashSet<String>,
    /// Last frame's quad count, for diagnostics.
    pub last_quads: usize,
}

impl SkinHud {
    /// Load a skin by name (`"atlantis"`), or `None` if the client's `ui/` is unreadable.
    ///
    /// Also merges `pregame/login.xml` when present so the Mythic account dialog (320×146) is
    /// drawable via the same nine-slice / font path as in-game windows.
    #[must_use]
    pub fn load(skin_name: &str) -> Option<Self> {
        let root = crate::terrain::client_root();
        let ui_dir = root.join("ui");
        match caer_assets::uiskin::Skin::load(&ui_dir, skin_name) {
            Ok(mut skin) => {
                absorb_pregame(&mut skin, &root);
                inject_product_social_overlays(&mut skin);
                log::info!(
                    "skinhud: loaded skin `{skin_name}` with {} windows",
                    skin.windows.len()
                );
                Some(Self {
                    ui_dir,
                    skin,
                    fonts: HashMap::new(),
                    failed_fonts: std::collections::HashSet::new(),
                    last_quads: 0,
                })
            }
            Err(e) => {
                log::warn!("skinhud: skin `{skin_name}` failed to load: {e}");
                None
            }
        }
    }

    /// Shipped product HUD: Atlantis base + Ghost (`custom/`) override + product social overlays.
    ///
    /// INT wires this from the product loop. Leaf tests and `caer uibind` use it as the
    /// critical-window denominator. egui is not involved.
    #[must_use]
    pub fn load_shipped() -> Option<Self> {
        let root = crate::terrain::client_root();
        let ui_dir = root.join("ui");
        match caer_assets::uiskin::Skin::load_shipped(&ui_dir) {
            Ok(mut skin) => {
                absorb_pregame(&mut skin, &root);
                inject_product_social_overlays(&mut skin);
                log::info!(
                    "skinhud: loaded shipped Atlantis+Ghost with {} windows",
                    skin.windows.len()
                );
                Some(Self {
                    ui_dir,
                    skin,
                    fonts: HashMap::new(),
                    failed_fonts: std::collections::HashSet::new(),
                    last_quads: 0,
                })
            }
            Err(e) => {
                log::warn!("skinhud: shipped skin failed to load: {e}");
                None
            }
        }
    }

    /// Leaf / integration-test constructor. Not a product load path — `load` / `load_shipped` remain
    /// the rustdaoc HUD sources.
    #[must_use]
    pub fn from_skin(skin: caer_assets::uiskin::Skin) -> Self {
        Self {
            ui_dir: PathBuf::from("/tmp"),
            skin,
            fonts: HashMap::new(),
            failed_fonts: std::collections::HashSet::new(),
            last_quads: 0,
        }
    }

    #[must_use]
    pub fn skin(&self) -> &caer_assets::uiskin::Skin {
        &self.skin
    }

    pub fn skin_mut(&mut self) -> &mut caer_assets::uiskin::Skin {
        &mut self.skin
    }

    /// Does the skin ship this window?
    #[must_use]
    pub fn has_window(&self, name: &str) -> bool {
        self.skin.windows.contains_key(name)
    }

    /// Window names the skin ships, sorted — for `/ui` style listing and diagnostics.
    #[must_use]
    pub fn window_names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.skin.windows.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }

    /// Lay out `windows` at their positions, bind their text through the live adapters, and hand the
    /// resulting quads to the GPU.
    ///
    /// Text quads are appended AFTER every frame quad so glyphs land on top — the pipeline is
    /// painter-ordered, with no depth to sort by.
    pub fn render(
        &mut self,
        gpu: &mut Gpu,
        windows: &[(&str, (f32, f32))],
        state: &AdapterState<'_>,
    ) {
        self.render_over(gpu, &[], windows, state);
    }

    /// Like [`Self::render`], but keeps `base` quads underneath (login plate + dialog, etc.).
    pub fn render_over(
        &mut self,
        gpu: &mut Gpu,
        base: &[UiQuad],
        windows: &[(&str, (f32, f32))],
        state: &AdapterState<'_>,
    ) {
        let mut quads: Vec<UiQuad> = base.to_vec();
        let mut texts = Vec::new();

        for (name, pos) in windows {
            let Some(w) = self.skin.windows.get(*name) else {
                continue;
            };
            // Adapters resolve against LIVE state here, where the dev lens substitutes `<name>`
            // placeholders. An unbound adapter yields None and the control simply draws empty,
            // rather than the whole window failing.
            let draw = skinui::layout_window(&self.skin, w, *pos, &|a| adapters::resolve(state, a));
            quads.extend(draw.quads.iter().cloned());
            texts.extend(draw.texts);
        }

        self.upload_pages(gpu, &quads);

        for t in &texts {
            let Some(fname) = t.font.as_deref() else {
                continue;
            };
            let key = fname.to_ascii_lowercase();
            if !self.fonts.contains_key(&key) && !self.failed_fonts.contains(&key) {
                self.load_font(gpu, fname, &key);
            }
            if let Some(f) = self.fonts.get(&key) {
                skinui::text_quads(f, &key, t, &mut quads);
            }
        }

        self.last_quads = quads.len();
        gpu.set_ui_quads(&quads);
    }

    /// Composite an already-laid-out [`skinui::WindowDraw`] (product bind / WindowManager path).
    pub fn composite(&mut self, gpu: &mut Gpu, base: &[UiQuad], draw: &skinui::WindowDraw) {
        let mut quads: Vec<UiQuad> = base.to_vec();
        quads.extend(draw.quads.iter().cloned());
        for t in &draw.texts {
            let Some(fname) = t.font.as_deref() else {
                continue;
            };
            let key = fname.to_ascii_lowercase();
            if !self.fonts.contains_key(&key) && !self.failed_fonts.contains(&key) {
                self.load_font(gpu, fname, &key);
            }
            if let Some(f) = self.fonts.get(&key) {
                skinui::text_quads(f, &key, t, &mut quads);
            }
        }
        self.last_quads = quads.len();
        gpu.set_ui_quads(&quads);
    }

    /// Upload any texture page these quads reference that the GPU does not have yet.
    fn upload_pages(&self, gpu: &mut Gpu, quads: &[UiQuad]) {
        for q in quads {
            if gpu.has_ui_page(&q.texture) {
                continue;
            }
            let Some(tex) = self.skin.texture(&q.texture) else {
                continue;
            };
            // Texture `File` paths are relative to `ui/`, not the skin dir — and client paths are
            // routinely mis-cased, which has silently broken whole windows before.
            let path = caer_assets::uiskin::resolve_ignoring_case(&self.ui_dir, &tex.file);
            match std::fs::read(&path).map(|b| caer_assets::tga::decode(&b)) {
                Ok(Ok(img)) => gpu.upload_ui_page(&q.texture, &img),
                _ => log::warn!("skinhud: could not load UI page {}", tex.file),
            }
        }
    }

    /// Load one bitmap font atlas and upload it as a UI page. TrueType declarations are skipped:
    /// they are a separate path, not a failure.
    fn load_font(&mut self, gpu: &mut Gpu, fname: &str, key: &str) {
        let Some(decl) = self.skin.font(fname) else {
            self.failed_fonts.insert(key.to_string());
            return;
        };
        if decl.file.to_ascii_lowercase().ends_with(".ttf") {
            self.failed_fonts.insert(key.to_string());
            return;
        }
        let path = caer_assets::uiskin::resolve_ignoring_case(&self.ui_dir, &decl.file);
        let loaded = std::fs::read(&path)
            .ok()
            .and_then(|b| caer_assets::tga::decode(&b).ok())
            .and_then(caer_assets::bitmapfont::BitmapFont::parse);
        match loaded {
            Some(f) => {
                gpu.upload_ui_page(key, &f.atlas);
                self.fonts.insert(key.to_string(), f);
            }
            None => {
                log::warn!("skinhud: font {} did not load as a bitmap atlas", decl.file);
                self.failed_fonts.insert(key.to_string());
            }
        }
    }
}

/// Where the client puts a window at this resolution, from its own `default*.ini`.
///
/// `[Panels-WxH]` entries lead with X and Y for most windows; `ChatWindow*` leads with a name. The
/// fields are positional and per-window, so this only reads the leading position and leaves the
/// rest to whoever implements that specific window.
#[must_use]
pub fn client_window_pos(width: u32, height: u32, window: &str) -> Option<(f32, f32)> {
    let root = crate::terrain::client_root();
    for name in [format!("default{width}.ini"), "default.ini".to_string()] {
        let Ok(text) = std::fs::read_to_string(root.join(&name)) else {
            continue;
        };
        let ini = caer_assets::clientini::ClientIni::parse(&text);
        let Some(fields) = ini.panel(width, height, window) else {
            continue;
        };
        // Skip a leading non-numeric field (ChatWindow0=Main,0,537,…).
        let nums: Vec<f32> = fields
            .iter()
            .filter_map(|f| f.parse::<f32>().ok())
            .collect();
        if nums.len() >= 2 {
            return Some((nums[0], nums[1]));
        }
    }
    None
}

/// Packet-derived gates for which Atlantis windows the live HUD opens.
///
/// Vitals stay always-on; merchant / target chrome follow WorldState so we do not invent an open
/// catalogue or a stale target frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct HudBindState {
    /// `true` once any MerchantWindow 0x17 is present (`WorldState::merchant()`).
    pub merchant_open: bool,
    /// `true` when the session has a selected entity.
    pub has_target: bool,
}

/// The HUD windows to draw, with the client's own positions where its ini names them.
///
/// Only windows the loaded skin actually ships are returned, so a skin without one of these simply
/// draws fewer rather than erroring. Positions fall back to a corner offset when the ini has no
/// entry for that window — the ini positions the movable panels, not every fixed element.
///
/// Equivalent to [`hud_windows`] with default bind state (summary + target; no merchant).
#[must_use]
pub fn default_hud_windows(hud: &SkinHud) -> Vec<(String, (f32, f32))> {
    hud_windows(
        hud,
        HudBindState {
            merchant_open: false,
            has_target: true,
        },
    )
}

/// HUD window set gated on packet-derived bind state.
///
/// Falsifiers:
/// - `hud_merchant_window_requires_merchant_open` — merchant absent from the list when
///   `merchant_open` is false.
/// - `hud_float_target_requires_has_target` — float_target absent when `has_target` is false.
#[must_use]
pub fn hud_windows(hud: &SkinHud, bind: HudBindState) -> Vec<(String, (f32, f32))> {
    let mut wanted: Vec<(&str, (f32, f32))> =
        vec![("new_summary_window", (8.0, 8.0)), ("summary", (8.0, 8.0))];
    if bind.has_target {
        wanted.push(("float_target_window", (300.0, 8.0)));
    }
    if bind.merchant_open {
        wanted.push(("merchant", (360.0, 80.0)));
    }
    let mut out = Vec::new();
    for (name, fallback) in wanted {
        if !hud.has_window(name) {
            continue;
        }
        // One summary window is enough; the skin may ship both the classic and the newer one.
        if name == "summary"
            && out
                .iter()
                .any(|(n, _): &(String, _)| n == "new_summary_window")
        {
            continue;
        }
        let pos = client_window_pos(1024, 768, name).unwrap_or(fallback);
        out.push((name.to_string(), pos));
    }
    out
}

/// Product overlay window names. Atlantis/Ghost do not ship a trade XML or a Yes/No dialog
/// template (retail chrome is engine-built). These are typed CAER overlays with hit-testable
/// Accept/Refuse/Cancel labels so the product pointer path does not fall back to egui.
pub const PRODUCT_DIALOG_WINDOW: &str = "product_dialog";
pub const PRODUCT_TRADE_WINDOW: &str = "product_trade";
/// Invite control overlay. Skin has no dedicated invite XML; hit-testable InviteToGroup 0x87.
pub const PRODUCT_INVITE_WINDOW: &str = "product_invite";

fn absorb_pregame(skin: &mut caer_assets::uiskin::Skin, root: &std::path::Path) {
    for rel in [
        "pregame/login.xml",
        "pregame/realm_selection.xml",
        "pregame/character_selection.xml",
        "pregame/character_creation.xml",
    ] {
        skin.absorb_file(root.join(rel));
    }
}

/// KILL-03 critical window classes (14). Trade/quest remain product overlays but are not
/// this denominator — §1A.3.3B names login through spell/effect status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CriticalWindowClass {
    Login,
    RealmCharSelect,
    CharCreate,
    Vitals,
    Target,
    Chat,
    InventoryEquipment,
    CharacterSheet,
    SkillsQuickbar,
    Group,
    Merchant,
    Loot,
    Map,
    CastStatus,
}

impl CriticalWindowClass {
    pub const ALL: [Self; 14] = [
        Self::Login,
        Self::RealmCharSelect,
        Self::CharCreate,
        Self::Vitals,
        Self::Target,
        Self::Chat,
        Self::InventoryEquipment,
        Self::CharacterSheet,
        Self::SkillsQuickbar,
        Self::Group,
        Self::Merchant,
        Self::Loot,
        Self::Map,
        Self::CastStatus,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::RealmCharSelect => "realm_character_select",
            Self::CharCreate => "character_create",
            Self::Vitals => "vitals",
            Self::Target => "target",
            Self::Chat => "chat",
            Self::InventoryEquipment => "inventory_equipment",
            Self::CharacterSheet => "character_sheet",
            Self::SkillsQuickbar => "skills_quickbar",
            Self::Group => "group",
            Self::Merchant => "merchant",
            Self::Loot => "loot",
            Self::Map => "map",
            Self::CastStatus => "cast_status",
        }
    }

    /// Pre-world surfaces — not opened by the in-world HUD list.
    #[must_use]
    pub fn is_preworld(self) -> bool {
        matches!(self, Self::Login | Self::RealmCharSelect | Self::CharCreate)
    }

    /// Skin `<Name>` candidates, first present wins.
    #[must_use]
    pub fn skin_names(self) -> &'static [&'static str] {
        match self {
            Self::Login => &["login"],
            Self::RealmCharSelect => &["character_selection", "realm_selection"],
            Self::CharCreate => &["character_creation"],
            Self::Vitals => &["new_summary_window", "summary"],
            Self::Target => &["float_target_window"],
            Self::Chat => &["chat"],
            Self::InventoryEquipment => &["stats_index"],
            Self::CharacterSheet => &["stats_attributes"],
            Self::SkillsQuickbar => &["stats_spec_abil", "stats_spells", "menu_bar_window"],
            Self::Group => &["mini_group", "new_group_window", "stats_group"],
            Self::Merchant => &["merchant"],
            Self::Loot => &["interact"],
            Self::Map => &["map_window"],
            Self::CastStatus => &["concentration", "timer", "stats_combat"],
        }
    }

    /// Adapter that must resolve from packet/table state when this class is populated.
    /// Removing the matching arm in `adapters` turns `kill03_class_adapter_requires_packet` red.
    #[must_use]
    pub fn representative_adapter(self) -> &'static str {
        match self {
            Self::Login => "username_text",
            Self::RealmCharSelect => "char_slot0_name",
            Self::CharCreate => "name_edit",
            Self::Vitals => "summary_player_hits",
            Self::Target => "summary_target",
            Self::Chat => "chat_entry",
            Self::InventoryEquipment => "paperdoll_slot_torso",
            Self::CharacterSheet => "stats_name",
            Self::SkillsQuickbar => "stats_spec_points",
            Self::Group => "group_name0",
            Self::Merchant => "merchant_page0",
            Self::Loot => "interact_text",
            Self::Map => "map_title",
            Self::CastStatus => "concentration",
        }
    }

    #[must_use]
    pub fn adapter_gate(self) -> &'static str {
        match self {
            Self::Login => "pregame session fields",
            Self::RealmCharSelect => "CharacterOverview 0xFC",
            Self::CharCreate => "pregame session fields",
            Self::Vitals => "CharacterStatusUpdate 0xAD",
            Self::Target => "selected entity (Player/NPC create + StatusUpdate)",
            Self::Chat => "Message/Command 0xAF",
            Self::InventoryEquipment => "EquipmentUpdate 0x15 / InventoryUpdate 0x02",
            Self::CharacterSheet => "CharacterSheet VariousUpdate 0x16:0x03",
            Self::SkillsQuickbar => {
                "VariousUpdate 0x16 subcode 0x01 skills / sheet specialty points"
            }
            Self::Group => "GroupWindow 0x16:0x06 / GroupMemberUpdate 0x70",
            Self::Merchant => "MerchantWindow 0x17",
            Self::Loot => "targeted entity name (create + select)",
            Self::Map => "region/zone tables + RegionChanged 0xB7",
            Self::CastStatus => "CharacterStatusUpdate 0xAD concentration / ConcentrationList 0x75",
        }
    }

    /// Skin windows in the Shape-2 first continuous-loop critical set.
    ///
    /// Excludes merchant catalogue, full multi-panel group sheets, and trade/quest overlays —
    /// those are later-parity or B3-gated. See `docs/evidence/CAER_UIBIND_SHAPE2_CRITICAL_*.md`.
    #[must_use]
    pub fn shape2_skin_windows() -> &'static [&'static str] {
        &[
            "login",
            "character_selection",
            "character_creation",
            "new_summary_window",
            "summary",
            "float_target_window",
            "chat",
            "map_window",
            "menu_bar_window",
            "stats_index",
            "stats_attributes",
            "stats_spec_abil",
            "concentration",
            "timer",
            "mini_group",
        ]
    }

    #[must_use]
    pub fn is_shape2_window(name: &str) -> bool {
        Self::shape2_skin_windows().contains(&name)
    }
}

/// Packet-derived gates for the *critical* product-drawn set (`caer uibind` denominator).
///
/// Distinct from [`HudBindState`] so rustdaoc's existing two-field HUD list is not a merge
/// conflict; INT adopts this table when wiring the product loop.
#[derive(Clone, Copy, Debug, Default)]
pub struct CriticalHudBind {
    pub merchant_open: bool,
    pub has_target: bool,
    pub group_active: bool,
    pub trade_open: bool,
    pub quest_open: bool,
    pub inventory_open: bool,
    /// Character sheet — independent of [`Self::inventory_open`].
    pub stats_open: bool,
    pub skills_open: bool,
    pub cast_status: bool,
}

impl CriticalHudBind {
    /// Fully populated session — the uibind denominator (every critical class that a live
    /// product HUD would draw when its packet gate is true).
    #[must_use]
    pub fn product_drawn() -> Self {
        Self {
            merchant_open: true,
            has_target: true,
            group_active: true,
            trade_open: true,
            quest_open: true,
            inventory_open: true,
            stats_open: true,
            skills_open: true,
            cast_status: true,
        }
    }
}

/// Product-drawn critical windows for the current bind gates.
///
/// Vitals and chat are always on. Other classes follow packet/table-derived gates so a stale
/// merchant/group/trade frame cannot linger.
#[must_use]
pub fn critical_hud_windows(hud: &SkinHud, bind: CriticalHudBind) -> Vec<(String, (f32, f32))> {
    let mut out = Vec::new();
    let push =
        |out: &mut Vec<(String, (f32, f32))>, class: CriticalWindowClass, fallback: (f32, f32)| {
            for name in class.skin_names() {
                if hud.has_window(name) {
                    let pos = client_window_pos(1024, 768, name).unwrap_or(fallback);
                    out.push(((*name).to_string(), pos));
                    return;
                }
            }
        };
    push(&mut out, CriticalWindowClass::Vitals, (8.0, 8.0));
    if bind.has_target {
        push(&mut out, CriticalWindowClass::Target, (300.0, 8.0));
    }
    if bind.inventory_open {
        push(
            &mut out,
            CriticalWindowClass::InventoryEquipment,
            (200.0, 80.0),
        );
    }
    if bind.stats_open {
        push(
            &mut out,
            CriticalWindowClass::CharacterSheet,
            (200.0, 120.0),
        );
    }
    if bind.merchant_open {
        push(&mut out, CriticalWindowClass::Merchant, (360.0, 80.0));
    }
    if bind.group_active {
        push(&mut out, CriticalWindowClass::Group, (590.0, 519.0));
    }
    // Trade/quest are product overlays (not KILL-03 classes) but stay gated here so
    // rustdaoc's existing CriticalHudBind fields keep driving them.
    if bind.trade_open && hud.has_window(PRODUCT_TRADE_WINDOW) {
        let pos = client_window_pos(1024, 768, PRODUCT_TRADE_WINDOW).unwrap_or((360.0, 280.0));
        out.push((PRODUCT_TRADE_WINDOW.to_string(), pos));
    }
    if bind.quest_open {
        for name in [
            "new_quest_journal",
            "quest",
            "new_quest",
            PRODUCT_DIALOG_WINDOW,
        ] {
            if hud.has_window(name) {
                let pos = client_window_pos(1024, 768, name).unwrap_or((40.0, 200.0));
                out.push((name.to_string(), pos));
                break;
            }
        }
    }
    if bind.skills_open {
        push(&mut out, CriticalWindowClass::SkillsQuickbar, (8.0, 520.0));
    }
    push(&mut out, CriticalWindowClass::Chat, (0.0, 537.0));
    if bind.cast_status {
        push(&mut out, CriticalWindowClass::CastStatus, (8.0, 80.0));
    }
    out
}

fn overlay_button(id: &str, label: &str, pos: (i32, i32)) -> caer_assets::uiskin::Control {
    caer_assets::uiskin::Control::Label(caer_assets::uiskin::Label {
        control_id: Some(id.into()),
        data: Some(label.into()),
        click_event: Some(id.into()),
        pos,
        width: 80,
        height: 24,
        font: Some("arial11".into()),
        ..Default::default()
    })
}

fn overlay_bound_label(
    id: &str,
    adapter: &str,
    pos: (i32, i32),
    width: i32,
    height: i32,
) -> caer_assets::uiskin::Control {
    caer_assets::uiskin::Control::Label(caer_assets::uiskin::Label {
        control_id: Some(id.into()),
        adapter: Some(adapter.into()),
        pos,
        width,
        height,
        font: Some("arial11".into()),
        ..Default::default()
    })
}

/// Inject typed Accept/Refuse/Cancel overlays. Provenance: skin has no trade XML and no Yes/No
/// dialog template; these exist so product hit-testing has a real control, not egui.
pub fn inject_product_social_overlays(skin: &mut caer_assets::uiskin::Skin) {
    use caer_assets::uiskin::WindowTemplate;
    skin.windows.insert(
        PRODUCT_DIALOG_WINDOW.into(),
        WindowTemplate {
            name: PRODUCT_DIALOG_WINDOW.into(),
            width: 280,
            height: 120,
            title_width: 280,
            title_height: 18,
            close_button: true,
            move_button: true,
            controls: vec![
                overlay_bound_label("dialog_body", "dialog_message", (12, 22), 256, 52),
                overlay_button("accept", "Accept", (20, 80)),
                overlay_button("refuse", "Refuse", (160, 80)),
            ],
            ..Default::default()
        },
    );
    skin.windows.insert(
        PRODUCT_TRADE_WINDOW.into(),
        WindowTemplate {
            name: PRODUCT_TRADE_WINDOW.into(),
            width: 320,
            height: 160,
            title_width: 320,
            title_height: 18,
            close_button: true,
            move_button: true,
            controls: vec![
                overlay_bound_label("trade_body", "trade_terms", (12, 22), 296, 90),
                overlay_button("accept", "Accept", (20, 120)),
                overlay_button("cancel", "Cancel", (160, 120)),
            ],
            ..Default::default()
        },
    );
    skin.windows.insert(
        PRODUCT_INVITE_WINDOW.into(),
        WindowTemplate {
            name: PRODUCT_INVITE_WINDOW.into(),
            width: 100,
            height: 52,
            title_width: 100,
            title_height: 16,
            close_button: false,
            move_button: false,
            controls: vec![overlay_button("invite", "Invite", (8, 20))],
            ..Default::default()
        },
    );
    // Character-select XML is button chrome with no AdapterName. Bind slot 0 from
    // CharacterOverview 0xFC so the class is not an unbound KILL-03 hole.
    if let Some(win) = skin.windows.get_mut("character_selection") {
        win.controls.push(overlay_bound_label(
            "char_slot0",
            "char_slot0_name",
            (815, 115),
            208,
            16,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window whose entry leads with a name must still yield its numeric position.
    ///
    /// Exercised through `ClientIni` directly because `client_window_pos` reads the installed
    /// client; the parsing rule is what matters and it is the part that can regress.
    #[test]
    fn leading_name_field_does_not_break_position_parsing() {
        let ini = caer_assets::clientini::ClientIni::parse(
            "[Panels-1024x768]\nChatWindow0=Main,0,537,312,195\nMiniGroup=590,519,1,68,100\n",
        );
        let chat = ini.panel(1024, 768, "ChatWindow0").unwrap();
        let nums: Vec<f32> = chat.iter().filter_map(|f| f.parse::<f32>().ok()).collect();
        assert_eq!(&nums[..2], &[0.0, 537.0]);

        let mini = ini.panel(1024, 768, "MiniGroup").unwrap();
        let nums: Vec<f32> = mini.iter().filter_map(|f| f.parse::<f32>().ok()).collect();
        assert_eq!(&nums[..2], &[590.0, 519.0]);
    }

    fn stub_hud_with(windows: &[&str]) -> SkinHud {
        let mut skin = caer_assets::uiskin::Skin::default();
        for name in windows {
            skin.windows.insert(
                (*name).into(),
                caer_assets::uiskin::WindowTemplate {
                    name: (*name).into(),
                    width: 100,
                    height: 80,
                    ..Default::default()
                },
            );
        }
        SkinHud {
            ui_dir: PathBuf::from("/tmp"),
            skin,
            fonts: HashMap::new(),
            failed_fonts: std::collections::HashSet::new(),
            last_quads: 0,
        }
    }

    /// Falsifier `hud_merchant_window_requires_merchant_open`.
    #[test]
    fn hud_merchant_window_requires_merchant_open() {
        let hud = stub_hud_with(&["new_summary_window", "float_target_window", "merchant"]);
        let closed = hud_windows(
            &hud,
            HudBindState {
                merchant_open: false,
                has_target: true,
            },
        );
        assert!(
            closed.iter().all(|(n, _)| n != "merchant"),
            "merchant must stay off the HUD without a MerchantWindow packet"
        );
        let open = hud_windows(
            &hud,
            HudBindState {
                merchant_open: true,
                has_target: true,
            },
        );
        assert!(
            open.iter().any(|(n, _)| n == "merchant"),
            "merchant window must join the HUD once world.merchant is Some"
        );
    }

    /// Falsifier `hud_float_target_requires_has_target`.
    #[test]
    fn hud_float_target_requires_has_target() {
        let hud = stub_hud_with(&["new_summary_window", "float_target_window"]);
        let none = hud_windows(
            &hud,
            HudBindState {
                merchant_open: false,
                has_target: false,
            },
        );
        assert!(
            none.iter().all(|(n, _)| n != "float_target_window"),
            "float_target must not linger without a selected entity"
        );
        let some = hud_windows(
            &hud,
            HudBindState {
                merchant_open: false,
                has_target: true,
            },
        );
        assert!(some.iter().any(|(n, _)| n == "float_target_window"));
    }

    /// Falsifier `critical_hud_gates_follow_packet_state`.
    #[test]
    fn critical_hud_windows_require_packet_gates() {
        let mut hud = stub_hud_with(&[
            "new_summary_window",
            "float_target_window",
            "stats_index",
            "stats_attributes",
            "merchant",
            "mini_group",
            "new_quest_journal",
            "stats_spec_abil",
            "chat",
            "concentration",
            "interact",
            "map_window",
            "login",
        ]);
        inject_product_social_overlays(&mut hud.skin);
        let idle = critical_hud_windows(&hud, CriticalHudBind::default());
        let names: Vec<&str> = idle.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"new_summary_window"));
        assert!(names.contains(&"chat"));
        assert!(!names.contains(&"merchant"));
        assert!(!names.contains(&"mini_group"));
        assert!(!names.contains(&PRODUCT_TRADE_WINDOW));
        assert!(!names.contains(&"stats_index"));
        assert!(
            !names.contains(&"login"),
            "preworld must not join the in-world HUD"
        );
        assert!(!names.contains(&"map_window"));
        assert!(!names.contains(&"interact"));

        let live = critical_hud_windows(&hud, CriticalHudBind::product_drawn());
        let names: Vec<&str> = live.iter().map(|(n, _)| n.as_str()).collect();
        for need in [
            "new_summary_window",
            "float_target_window",
            "stats_index",
            "stats_attributes",
            "merchant",
            "mini_group",
            PRODUCT_TRADE_WINDOW,
            "new_quest_journal",
            "stats_spec_abil",
            "chat",
            "concentration",
        ] {
            assert!(
                names.contains(&need),
                "missing critical window {need}: {names:?}"
            );
        }
        assert!(!names.contains(&"login"));
    }

    /// Falsifier `kill03_fourteen_classes_named`.
    #[test]
    fn kill03_fourteen_classes_named() {
        assert_eq!(CriticalWindowClass::ALL.len(), 14);
        let names: Vec<&str> = CriticalWindowClass::ALL.iter().map(|c| c.name()).collect();
        for need in [
            "login",
            "realm_character_select",
            "character_create",
            "vitals",
            "target",
            "chat",
            "inventory_equipment",
            "character_sheet",
            "skills_quickbar",
            "group",
            "merchant",
            "loot",
            "map",
            "cast_status",
        ] {
            assert!(
                names.contains(&need),
                "KILL-03 class missing {need}: {names:?}"
            );
        }
        assert!(!names.contains(&"trade"));
        assert!(!names.contains(&"quest"));
    }
}
