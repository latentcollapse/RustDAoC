//! Pre-world skin screens — login / settings / realm / character-select / character-create.
//!
//! In-game HUD uses `ui/<skin>/*.xml`. Pre-world does not: Mythic shipped login chrome as BMPs in
//! `data/login2.mpk` and realm/char screens as TGAs in `pregame/pregame.mpk` (M3: no
//! `character_selection_window` in the atlantis skin). This module owns that catalog, maps
//! [`caer_protocol::session::SessionPhase`] onto a screen, lays out quads, and hit-tests login
//! buttons through the same [`crate::skinui::WindowManager`] z-order model used for HUD windows.
//!
//! The Options Menu is not a screen. Retail draws it as a modal overlay over whatever screen is
//! showing — see [`crate::preworld_options`]. A `Settings` screen used to exist here and drew
//! nothing, which is why opening Options replaced the character plate with black.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use caer_assets::tga::TgaImage;
use caer_assets::uiskin::Rgba;
use caer_protocol::session::SessionPhase;

use crate::gpu::Gpu;
use crate::preworld_customize::{self, CustomizerField, CustomizerWidget};
use crate::preworld_hitbox::{self, Target};
use crate::skinui::{self, Rect, UiQuad, WindowManager};

/// Synthetic 1×1 black page used as a full-viewport underlay behind letterboxed plates so the
/// world/sky clear colour cannot show through at the edges (QA defect b).
const FILL_PAGE: &str = "__preworld_fill";
/// A 1×1 white page, so a fill quad's `color` means what it says.
///
/// [`FILL_PAGE`] is black, and the UI shader multiplies texel by colour — every quad drawn on it
/// comes out black whatever colour it asks for. That is fine for letterbox bars and wrong for
/// anything tinted, so tinted fills use this one.
const WHITE_PAGE: &str = "__preworld_white";

/// Which pre-world surface is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreWorldScreen {
    Login,
    /// Intro / first-login splash (`pregame/splash.mpk` + splash loadbar) — control: EA siege art.
    Splash,
    /// Zone / enter-world / linkdead plate (`data/loading/*.mpk`).
    Loading,
    RealmSelect,
    CharSelect,
    CharCreate,
    /// Appearance customization (`character_customize.xml`) — E6/B3.
    CharCustomize,
    /// Starting-stat allocation (`character_customize_stats.xml`) — E6/B3.
    CharStats,
}

impl PreWorldScreen {
    /// Session phase → default screen. `None` once in-world (HUD takes over).
    #[must_use]
    pub fn from_phase(phase: SessionPhase) -> Option<Self> {
        match phase {
            SessionPhase::Disconnected | SessionPhase::CryptHandshake => Some(Self::Login),
            // Credentials in flight: retail intro splash (OWN_CAPTURE control 2026-08-14).
            SessionPhase::Authenticating => Some(Self::Splash),
            SessionPhase::RealmSelect => Some(Self::RealmSelect),
            SessionPhase::CharacterSelect => Some(Self::CharSelect),
            SessionPhase::EnteringWorld => Some(Self::Loading),
            SessionPhase::InWorld | SessionPhase::Closed => None,
        }
    }

    /// Does retail render a 3D realm scene behind this screen?
    ///
    /// True for every step of character creation, whose plates are a frame around a transparent
    /// middle. The splash, login, realm and loading plates are genuinely opaque 2D art, and running
    /// world passes under those would be the waste the UI-only path exists to avoid.
    #[must_use]
    pub fn has_scene_behind(self) -> bool {
        matches!(
            self,
            Self::CharSelect | Self::CharCreate | Self::CharCustomize | Self::CharStats
        )
    }

    /// Background member name inside the owning archive (if any).
    #[must_use]
    pub fn background_member(self) -> Option<&'static str> {
        match self {
            Self::Login => Some("back_tile.bmp"),
            // Retail picks `splash%d.tga` (1..8); splash1 matches the control capture Matt took.
            Self::Splash => Some("splash1.tga"),
            // Provisional zone plate until region-specific pick is wired.
            Self::Loading => Some("mid1.dds"),
            Self::RealmSelect => Some("realm_selection.tga"),
            Self::CharSelect => Some("character_selection.tga"),
            Self::CharCreate => Some("character_creation.tga"),
            // `character_customize_stats.xml` is a 440x260 modal, not a full-screen canvas of
            // its own. Retail keeps the complete customization form beneath it, so the modal
            // inherits the same source-owned plate rather than dropping the surrounding chrome.
            Self::CharCustomize | Self::CharStats => Some("character_customize.tga"),
        }
    }

    /// Archive path relative to `$CAER_CLIENT`.
    #[must_use]
    pub fn archive_rel(self) -> Option<&'static str> {
        match self {
            Self::Login => Some("data/login2.mpk"),
            Self::Splash => Some("pregame/splash.mpk"),
            Self::Loading => Some("data/loading/mid1.mpk"),
            Self::RealmSelect | Self::CharSelect | Self::CharCreate => Some("pregame/pregame.mpk"),
            // `pregame/asset.xml` assigns `character_customize_canvas` to this separate archive.
            // `pregame.mpk` does not contain the plate, so treating all creation screens as one
            // archive silently left the customization form blank.
            Self::CharCustomize | Self::CharStats => Some("pregame/pregame003.mpk"),
        }
    }

    /// Skin window overlaid on this screen (the Login account dialog, and nothing else).
    #[must_use]
    pub fn skin_window(self) -> Option<&'static str> {
        match self {
            Self::Login => Some("login"),
            _ => None,
        }
    }
}

/// Whose body stands on a character screen: `(eRace, fig3 gender)`.
///
/// Character select shows a body only after an occupied overview slot was explicitly selected.
/// Empty overview, empty slot, and no selection all return `None`. CharCreate shows the draft.
/// Never falls back to row zero, a warmup identity, or a seeded race.
#[must_use]
pub fn preview_identity(
    screen: PreWorldScreen,
    overview: Option<&caer_protocol::overview::CharacterOverview>,
    selected_slot: Option<u8>,
    draft_race: u8,
    draft_gender: u8,
) -> Option<(u8, u8)> {
    match screen {
        PreWorldScreen::CharCreate | PreWorldScreen::CharCustomize | PreWorldScreen::CharStats => {
            Some((
                draft_race,
                caer_protocol::overview::fig3_gender_from_db(draft_gender),
            ))
        }
        PreWorldScreen::CharSelect => {
            let ov = overview?;
            let slot = selected_slot?;
            let c = ov.characters.iter().find(|ch| ch.slot == slot)?;
            let (race, gender) =
                caer_protocol::overview::decode_overview_race_gender(c.race_gender);
            Some((race, caer_protocol::overview::fig3_gender_from_db(gender)))
        }
        _ => None,
    }
}

/// Which face and hair the previewed body wears, decided by exactly the same rules as
/// [`preview_identity`] so the two can never disagree about whose body is on the stage.
///
/// CharCreate shows the draft the player is building, so what stands there is what the create
/// packet will carry. CharSelect shows a saved character's stored look, straight off the overview.
/// Anywhere else there is no body, and the default is the uncustomised character DOL stores when
/// nobody touched the form.
#[must_use]
pub fn preview_appearance(
    screen: PreWorldScreen,
    overview: Option<&caer_protocol::overview::CharacterOverview>,
    selected_slot: Option<u8>,
    draft: &caer_protocol::charcreate::CharacterCreateDraft,
) -> caer_protocol::customization::AvatarAppearance {
    match screen {
        PreWorldScreen::CharCreate | PreWorldScreen::CharCustomize | PreWorldScreen::CharStats => {
            draft.appearance()
        }
        PreWorldScreen::CharSelect => overview
            .zip(selected_slot)
            .and_then(|(ov, slot)| ov.characters.iter().find(|ch| ch.slot == slot))
            .map(caer_protocol::overview::CharacterSummary::appearance)
            .unwrap_or_default(),
        _ => caer_protocol::customization::AvatarAppearance::default(),
    }
}

/// The discrete portion of [`preview_appearance`], retained for callers that only need asset
/// selection. Geometry consumers must use [`preview_appearance`] so facial morphs survive.
#[must_use]
pub fn preview_customization(
    screen: PreWorldScreen,
    overview: Option<&caer_protocol::overview::CharacterOverview>,
    selected_slot: Option<u8>,
    draft: &caer_protocol::charcreate::CharacterCreateDraft,
) -> caer_protocol::customization::Customization {
    preview_appearance(screen, overview, selected_slot, draft).customization
}

/// Clickable login chrome control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoginButton {
    Play,
    Account,
    Billing,
    Credits,
    Support,
    Exit,
}

impl LoginButton {
    const ALL: [Self; 6] = [
        Self::Play,
        Self::Account,
        Self::Billing,
        Self::Credits,
        Self::Support,
        Self::Exit,
    ];

    #[must_use]
    pub fn norm_member(self) -> &'static str {
        match self {
            Self::Play => "btn_play_norm.bmp",
            Self::Account => "btn_account_norm.bmp",
            Self::Billing => "btn_billing_norm.bmp",
            Self::Credits => "btn_credits_norm.bmp",
            Self::Support => "btn_support_norm.bmp",
            Self::Exit => "btn_exit_norm.bmp",
        }
    }

    #[must_use]
    pub fn down_member(self) -> &'static str {
        match self {
            Self::Play => "btn_play_down.bmp",
            Self::Account => "btn_account_down.bmp",
            Self::Billing => "btn_billing_down.bmp",
            Self::Credits => "btn_credits_down.bmp",
            Self::Support => "btn_support_down.bmp",
            Self::Exit => "btn_exit_down.bmp",
        }
    }
}

/// What the pointer is highlighting. Compared frame to frame to decide whether to repaint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HoverState {
    pub realm: Option<RealmButton>,
    pub action: Option<PreWorldAction>,
    pub slot: Option<u8>,
    pub options: Option<crate::preworld_options::OptionsId>,
}

/// Clickable realm crest on the realm-select screen (protocol realm ids 1/2/3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RealmButton {
    Albion = 1,
    Midgard = 2,
    Hibernia = 3,
}

impl RealmButton {
    const ALL: [Self; 3] = [Self::Albion, Self::Midgard, Self::Hibernia];

    #[must_use]
    pub fn protocol_id(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub fn member(self) -> &'static str {
        match self {
            Self::Albion => "albion_button.tga",
            Self::Midgard => "midgard_button.tga",
            Self::Hibernia => "hibernia_button.tga",
        }
    }

    /// Authored destination and normal-state atlas crop from
    /// `pregame/realm_selection.xml` + `pregame/styles.xml`.
    #[must_use]
    fn authored_rect(self, viewport: (f32, f32)) -> Rect {
        let (x, y, w, h) = match self {
            Self::Albion => (110.0, 190.0, 114.0, 150.0),
            Self::Hibernia => (430.0, 172.0, 148.0, 150.0),
            Self::Midgard => (802.0, 172.0, 122.0, 150.0),
        };
        map_pregame_rect(x, y, w, h, viewport)
    }

    #[must_use]
    fn normal_src(self) -> Rect {
        match self {
            Self::Albion => Rect::new(133.0, 8.0, 114.0, 150.0),
            Self::Hibernia => Rect::new(149.0, 1.0, 148.0, 150.0),
            Self::Midgard => Rect::new(130.0, 0.0, 122.0, 150.0),
        }
    }

    /// Retail highlighted/pressed atlas crop from `pregame/styles.xml`.
    #[must_use]
    fn highlight_src(self) -> Rect {
        match self {
            Self::Albion => Rect::new(5.0, 7.0, 114.0, 150.0),
            Self::Hibernia => Rect::new(0.0, 0.0, 148.0, 150.0),
            Self::Midgard => Rect::new(2.0, 1.0, 122.0, 150.0),
        }
    }
}

/// Authored pixel sizes from `login2.mpk` (measured).
fn login_btn_size(btn: LoginButton) -> (f32, f32) {
    match btn {
        LoginButton::Play => (84.0, 26.0),
        _ => (90.0, 34.0),
    }
}

/// Place login buttons along the bottom of `viewport` — provisional layout until we reverse the
/// client's exact placement table. Stable enough for hit-testing + MS-02 wiring.
fn login_btn_rect(btn: LoginButton, viewport: (f32, f32)) -> Rect {
    let (vw, vh) = viewport;
    let (bw, bh) = login_btn_size(btn);
    let gap = 12.0;
    let order = LoginButton::ALL;
    let total_w: f32 =
        order.iter().map(|b| login_btn_size(*b).0).sum::<f32>() + gap * (order.len() as f32 - 1.0);
    let mut x = ((vw - total_w) * 0.5).max(8.0);
    let y = vh - bh - 48.0;
    for b in order {
        let (w, h) = login_btn_size(b);
        if b == btn {
            return Rect::new(x, y, w, h);
        }
        x += w + gap;
    }
    Rect::new(x, y, bw, bh)
}

/// Realm hit boxes are the three full-height `InvisibleButtonDef`s in the shipped
/// `pregame/realm_selection.xml` (ControlIds 1014/1016/1015). The crests and the gold name
/// buttons are the visible affordance; retail lets the player click anywhere in the column.
///
/// This is the one place in the pre-world UI where the hit area is deliberately larger than the
/// art, and it is that way because the client ships it that way.
#[cfg(test)]
fn realm_btn_rect(btn: RealmButton, viewport: (f32, f32)) -> Rect {
    let column = preworld_hitbox::control_for(
        PreWorldScreen::RealmSelect,
        Target::RealmColumn(btn.protocol_id()),
    )
    .expect("realm column");
    column
        .bounds()
        .mapped(PreworldTransform::from_viewport(viewport))
}

/// Authored pregame plate. Background, controls, labels and hit-testing all share one mapping of
/// this rectangle onto the surface.
pub const PREGAME_W: f32 = 1024.0;
pub const PREGAME_H: f32 = 768.0;

/// One transform from 1024×768 authoring space into the current surface.
///
/// **The plate stretches to fill the surface; it does not letterbox.** This used to be
/// `scale = min(vw/1024, vh/768)` with centring, which on any 16:9 display left 240px of pure black
/// down both sides in *every* display mode — ledger A11, and the reason "full-screen windowed" and
/// "windowed" looked identical. Retail and Eden fill the screen, and Matt's decision (2026-08-18) is
/// that all three of CAER's modes do too: the difference between them is how the window relates to
/// the desktop, never whether the art reaches the edges.
///
/// The 4:3 plate on a 16:9 display is therefore drawn wider than tall, which is what retail does and
/// is a deliberate distortion rather than an accident.
///
/// `sx` and `sy` were always separate fields and `map_rect` always used both, so this is one
/// expression — and because the Phase 2 coordinate contract put drawing, hover and hit-testing
/// through this single transform, all three follow it without a second mapper.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreworldTransform {
    pub sx: f32,
    pub sy: f32,
    pub ox: f32,
    pub oy: f32,
}

impl PreworldTransform {
    #[must_use]
    pub fn from_viewport(viewport: (f32, f32)) -> Self {
        let (vw, vh) = viewport;
        Self {
            sx: vw / PREGAME_W,
            sy: vh / PREGAME_H,
            ox: 0.0,
            oy: 0.0,
        }
    }

    #[must_use]
    pub fn map(self, x: f32, y: f32) -> (f32, f32) {
        (self.ox + x * self.sx, self.oy + y * self.sy)
    }

    #[must_use]
    pub fn unmap(self, x: f32, y: f32) -> (f32, f32) {
        let sx = self.sx.max(f32::MIN_POSITIVE);
        let sy = self.sy.max(f32::MIN_POSITIVE);
        ((x - self.ox) / sx, (y - self.oy) / sy)
    }

    #[must_use]
    pub fn map_rect(self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        let (dx, dy) = self.map(x, y);
        Rect::new(dx, dy, w * self.sx, h * self.sy)
    }

    #[must_use]
    pub fn plate_rect(self) -> Rect {
        Rect::new(self.ox, self.oy, PREGAME_W * self.sx, PREGAME_H * self.sy)
    }
}

/// Scale a point authored for the 1024×768 pregame plate into the window.
///
/// The drawing and hit-testing paths go through [`PreworldTransform`] directly — this is the
/// point-sized convenience the transform's own tests are written against.
#[cfg(test)]
fn map_pregame(x: f32, y: f32, viewport: (f32, f32)) -> (f32, f32) {
    PreworldTransform::from_viewport(viewport).map(x, y)
}

/// Inverse of [`PreworldTransform::map`] — a viewport point back into the 1024×768 authoring
/// space.
///
/// The Options Menu is authored as one grid, so it hit-tests once in its own space rather than
/// mapping thirty rects forward per click.
fn unmap_pregame(x: f32, y: f32, viewport: (f32, f32)) -> (f32, f32) {
    PreworldTransform::from_viewport(viewport).unmap(x, y)
}

fn map_pregame_rect(x: f32, y: f32, w: f32, h: f32, viewport: (f32, f32)) -> Rect {
    PreworldTransform::from_viewport(viewport).map_rect(x, y, w, h)
}

/// The gold title each pre-world plate carries, as its own XML authors it.
///
/// Every one of these screens defines exactly one `large_gold` `LabelDef` — that is the caption.
/// Text, position and size all come from `pregame/<screen>.xml`, and the colour is `gold_color()`
/// (255/200/69), which is what those labels declare.
///
/// The client spells the text with **two spaces** between words. Searching for the natural
/// single-spaced phrase is what previously hid it and produced the conclusion that the caption was
/// painted into the plate art — it is not; the plates decode to bare frames.
struct PlateCaption {
    /// Verbatim `<Data>`, double spaces included.
    text: &'static str,
    /// `<Position>` and `<Width>`/`<Height>`, in the 1024×768 authoring space.
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// `plate_caption_matches_client_xml` checks this table against the player's own client files
/// rather than against itself, so a drifted string or moved label fails there.
const PLATE_CAPTIONS: [(PreWorldScreen, PlateCaption); 3] = [
    (
        PreWorldScreen::RealmSelect,
        PlateCaption {
            text: "Select  Your  Realm",
            x: 260.0,
            y: 662.0,
            w: 500.0,
            h: 16.0,
        },
    ),
    (
        PreWorldScreen::CharSelect,
        PlateCaption {
            text: "Select  Your  Character",
            x: 260.0,
            y: 662.0,
            w: 500.0,
            h: 16.0,
        },
    ),
    (
        PreWorldScreen::CharCreate,
        PlateCaption {
            text: "Select  Your  Race",
            x: 260.0,
            y: 662.0,
            w: 500.0,
            h: 16.0,
        },
    ),
];

/// The XML file each captioned screen is authored in — the oracle for the row above, used by
/// `plate_caption_matches_client_xml`.
#[cfg(test)]
const PLATE_CAPTION_SOURCES: [(PreWorldScreen, &str); 3] = [
    (PreWorldScreen::RealmSelect, "realm_selection.xml"),
    (PreWorldScreen::CharSelect, "character_selection.xml"),
    (PreWorldScreen::CharCreate, "character_creation.xml"),
];

fn plate_caption(screen: PreWorldScreen) -> Option<&'static PlateCaption> {
    PLATE_CAPTIONS
        .iter()
        .find(|(s, _)| *s == screen)
        .map(|(_, c)| c)
}

/// Map one authored piece of a control into the viewport.
///
/// Drawing asks for a specific piece — the round button, or the caption beneath it. Hit-testing
/// never does: it goes through [`preworld_hitbox::hit`], which tests every part, because the box
/// spanning a button and its caption is mostly empty art.
fn control_piece(
    screen: PreWorldScreen,
    target: Target,
    pick: fn(&preworld_hitbox::Control) -> preworld_hitbox::ArtRect,
    viewport: (f32, f32),
) -> Rect {
    preworld_hitbox::control_for(screen, target)
        .map(|c| pick(c).mapped(PreworldTransform::from_viewport(viewport)))
        .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0))
}

/// Where a control's caption is drawn.
fn caption_rect(screen: PreWorldScreen, target: Target, viewport: (f32, f32)) -> Rect {
    control_piece(screen, target, preworld_hitbox::Control::caption, viewport)
}

/// Where a control's button art is drawn.
fn art_rect(screen: PreWorldScreen, target: Target, viewport: (f32, f32)) -> Rect {
    control_piece(screen, target, preworld_hitbox::Control::art, viewport)
}

#[cfg(test)]
fn charcreate_cancel_rect(viewport: (f32, f32)) -> Rect {
    caption_rect(
        PreWorldScreen::CharCreate,
        Target::CharCreateCancel,
        viewport,
    )
}

/// Random-name button — ControlId 1013, `button_pregame_small` (64x21, `styles.xml:1420`).
fn charcreate_random_rect(viewport: (f32, f32)) -> Rect {
    art_rect(
        PreWorldScreen::CharCreate,
        Target::CharCreateRandomName,
        viewport,
    )
}

/// Character-select slot rows, `character_selection.xml`: ten `button_large_radio` controls at
/// (780, 125 + 45i), each with two 208x16 text lines at (815, y-10) and (815, y+10).
const CHARSELECT_SLOTS: u8 = 10;
const CHARSELECT_ROW_H: f32 = 45.0;

/// The clickable row for a slot — the radio plus its two text lines, as one target.
///
/// Draw, hover and hit-test all read this. They used to disagree: the rows were drawn at the
/// authored 45px pitch while `hit_char_slot` guessed `vw*0.62` wide, `36px` tall, starting at
/// `vh*0.22`, so what you clicked was not what you saw.
#[must_use]
pub fn charselect_slot_rect(slot: u8, viewport: (f32, f32)) -> Rect {
    control_bounds_of(PreWorldScreen::CharSelect, Target::CharSlot(slot), viewport)
}

/// Extent of a control, for drawing a highlight or reporting where it is.
fn control_bounds_of(screen: PreWorldScreen, target: Target, viewport: (f32, f32)) -> Rect {
    control_piece(screen, target, preworld_hitbox::Control::bounds, viewport)
}

/// Which character row is under a design-space point, through the authored parts rather than the
/// row's bounding box — the 4px seam between a row's two text lines belongs to neither line.
fn hit_charselect_slot(x: f32, y: f32, viewport: (f32, f32)) -> Option<u8> {
    match preworld_hitbox::hit(PreWorldScreen::CharSelect, x, y, viewport)?.target {
        Target::CharSlot(slot) => Some(slot),
        _ => None,
    }
}

/// Realm label — ControlId 1066, `64x16_no_bg` at (262,752), under the round `realm` button at
/// (272,706). Both were absent; the creation form had no way back to the realm plate.
#[cfg(test)]
fn charcreate_continue_rect(viewport: (f32, f32)) -> Rect {
    caption_rect(
        PreWorldScreen::CharCreate,
        Target::CharCreateContinue,
        viewport,
    )
}

/// Hit-test the create form.
///
/// Every rect comes from [`preworld_hitbox`], so a control responds exactly where its art is.
/// Continué, Cancel and Realm used to test their captions only, leaving the round buttons above
/// them drawn and dead.
#[must_use]
pub fn hit_charcreate(x: f32, y: f32, viewport: (f32, f32)) -> Option<PreWorldAction> {
    let hit = preworld_hitbox::hit(PreWorldScreen::CharCreate, x, y, viewport)?;
    Some(match hit.target {
        Target::CharCreateContinue => PreWorldAction::CharCreateContinue,
        Target::CharCreateCancel => PreWorldAction::CharCreateCancel,
        Target::CharCreateRealm => PreWorldAction::BackToRealm,
        Target::CharCreateRandomName => PreWorldAction::CharCreateRandomName,
        Target::CharCreateName => PreWorldAction::CharCreateFocusName,
        Target::CharCreateGender(g) => PreWorldAction::CharCreateGender(g),
        Target::CharCreateRace(i) => PreWorldAction::CharCreateRace(i),
        Target::CharCreateClass(i) => PreWorldAction::CharCreateClass(i),
        _ => return None,
    })
}

/// Hit-test the shipped appearance-customization form.
///
/// The form's source-owned geometry lives in [`preworld_hitbox`]. This arm owns only semantic
/// routing, so palette cells cannot paint at one coordinate and mutate an unrelated appearance
/// byte at another.
#[must_use]
fn hit_charcustomize(
    x: f32,
    y: f32,
    viewport: (f32, f32),
    has_tattoo: bool,
) -> Option<PreWorldAction> {
    let hit = preworld_hitbox::hit_customizer(has_tattoo, x, y, viewport)?;
    Some(match hit.target {
        Target::CustomizeCancel => PreWorldAction::CustomizeCancel,
        Target::CustomizeRealm => PreWorldAction::BackToRealm,
        Target::CustomizeBack => PreWorldAction::CustomizeBack,
        Target::CustomizeAdvance => PreWorldAction::CustomizeAdvance,
        Target::CustomizeStats => PreWorldAction::CustomizeStats,
        Target::CustomizeReset => PreWorldAction::CustomizeReset,
        Target::CustomizeRandom => PreWorldAction::CustomizeRandom,
        Target::CustomizeLock { field } => PreWorldAction::CustomizeToggleLock { field },
        Target::CustomizeAdjust { field, dir } => PreWorldAction::CustomizeAdjust { field, dir },
        Target::CustomizeSlider { field } => {
            let (design_x, _) = PreworldTransform::from_viewport(viewport).unmap(x, y);
            PreWorldAction::CustomizeSlider {
                field,
                tick: preworld_hitbox::horizontal_slider_tick(hit.art(), design_x),
            }
        }
        Target::CustomizeCamera(control) => PreWorldAction::CustomizeCamera(control),
        _ => return None,
    })
}

/// Hit-test a retail realm column.
#[must_use]
pub fn hit_realm(x: f32, y: f32, viewport: (f32, f32)) -> Option<RealmButton> {
    match preworld_hitbox::hit(PreWorldScreen::RealmSelect, x, y, viewport)?.target {
        Target::RealmColumn(1) => Some(RealmButton::Albion),
        Target::RealmColumn(2) => Some(RealmButton::Midgard),
        Target::RealmColumn(3) => Some(RealmButton::Hibernia),
        _ => None,
    }
}

/// Local pre-world navigation (offline / screenshot / before overview).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreWorldAction {
    LoginPlay,
    OpenSettings,
    LoginExit,
    ChooseRealm(u8),
    OpenCharCreate,
    /// Character-creation Cancel (XML ControlId 1067).
    CharCreateCancel,
    /// Character-creation Continué (XML ControlId 1068) — opens customization and mints
    /// nothing. Refuses while the name box is blank, because
    /// the box lives on this form.
    CharCreateContinue,
    /// Random name button (`character_creation.xml` ControlId 1013) — composes a name from the
    /// client's own `charman/names.dat` fragments for the selected race.
    CharCreateRandomName,
    /// Focus the create-form name edit box (`name_edit` / ControlId 1051).
    CharCreateFocusName,
    /// Race button index 0..6 from `character_creation.xml` (ControlId 1014–1020).
    CharCreateRace(u8),
    /// Class button index 0..4 from `character_creation.xml` (ControlId 1021–1025).
    CharCreateClass(u8),
    /// Gender: 0 male (ControlId 1090) / 1 female (ControlId 1091).
    CharCreateGender(u8),
    /// Char-select row click (`character_selection.xml` ControlIds 1030–1039, plus the two
    /// `256x16_no_bg` text buttons beside each one, which the client also makes clickable).
    ///
    /// Empty rows select too: retail fills the radio and the bottom button reads Create.
    SelectCharacterSlot(u8),
    /// Char-select Play / Enter World (`play_create_text` when slot occupied).
    EnterWorld,
    /// Char-select Delete (ControlId 1095).
    DeleteCharacter,
    /// Char-select Realm back (ControlId 1099).
    BackToRealm,
    /// Char-select Quit (ControlId 1097).
    CharSelectQuit,
    /// Char-select Options (ControlId 1093).
    CharSelectOptions,
    /// One interaction with the Options Menu overlay.
    Options(crate::preworld_options::OptionsHit),
    /// `quit_confirm.xml` Yes (1003) — leave the application.
    ///
    /// Distinct from [`Self::CharSelectQuit`], which now only raises the modal. Collapsing the two
    /// is what made Quit navigate: the command closed the session, the session's close emitted
    /// `FlowEvent::Closed`, and `Closed` routes to the login screen.
    QuitConfirmYes,
    /// `quit_confirm.xml` No (1004) — dismiss the modal and stay where we were.
    QuitConfirmNo,
    /// `delete_confirm.xml` Delete (1003) — delete the selected character.
    DeleteConfirmYes,
    /// `delete_confirm.xml` Cancel (1004) — dismiss and keep the character.
    DeleteConfirmNo,
    /// `character_customize.xml` advance (ControlId 1003, `location` template).
    ///
    /// Submits the completed draft exactly once. The source `character_customize.xml` and
    /// `character_creation.xml` both route their Continue controls through retail event `0x1F6`;
    /// the separate stats form owns Reset/Optimize only. Keeping the wire boundary here prevents
    /// its purely local Optimize button from unexpectedly creating a character.
    ///
    /// PROVENANCE GAP: which of the two bottom-row controls advances and which goes back is read
    /// off template semantics (`location` forward, `race_class` back), not a runtime capture.
    /// Flipping them if a reference shows otherwise is one line here plus one in the hitbox
    /// table, nothing else — both route through these actions either way.
    CustomizeAdvance,
    /// The `character_customize_basic.xml` Adjust Attributes icon opens the same local
    /// allocation screen, but remains its own action so the visual source and the interaction
    /// contract cannot silently collapse into the Continue button.
    CustomizeStats,
    /// `character_customize.xml` back (ControlId 1004, `race_class` template) — return to the
    /// create form. Local navigation only.
    CustomizeBack,
    /// `character_customize.xml` Cancel (ControlId 1002): return to character select. This is
    /// distinct from [`Self::CustomizeBack`], which returns one step to race/class selection.
    CustomizeCancel,
    /// `character_customize.xml` Default (ControlId 1078): restore the retail all-zero look.
    CustomizeReset,
    /// `character_customize.xml` Random (ControlId 1028): choose only unlocked,
    /// source-authored appearance values.
    CustomizeRandom,
    /// Toggle one of the live runtime Random locks.
    CustomizeToggleLock {
        field: CustomizerField,
    },
    /// One click of a textual runtime selector. `field` is the visible setting, not a raw packet
    /// byte; the product owns its corresponding protocol representation.
    CustomizeAdjust {
        field: CustomizerField,
        dir: i8,
    },
    /// Set one exact retail runtime-slider tick.  `field` is Morph 0..3, Mood, or Skin Tone;
    /// `tick` is one of the nine observed positions 0..=8.
    CustomizeSlider {
        field: CustomizerField,
        tick: u8,
    },
    /// A source-authored customizer camera button.  This changes only the local preview, never
    /// a draft field or the create packet.
    CustomizeCamera(crate::preworld_camera::CameraControl),
    /// `character_customize_stats.xml` mini arrow (ControlIds 1031–1046): stat index 0..7 in
    /// wire order, `dir` +1 right / −1 left. Enforces the DOLSharp allocation rules.
    StatsAdjust {
        stat: u8,
        dir: i8,
    },
    /// `character_customize_stats.xml` reset (ControlId 1020) — back to all race bases.
    StatsReset,
    /// `character_customize_stats.xml` Optimize (ControlId 1021). This reapplies the observed
    /// default 30-point allocation locally; it never creates or navigates.
    StatsOptimize,
    /// Dismiss the retail stats window through its generic closable-window action. The form's
    /// `CloseButton=true` returns to the still-visible customization screen without changing the
    /// draft or sending a packet.
    StatsDismiss,
}

/// `quit_confirm.xml` LabelDef 1002 `<Data>`, verbatim.
///
/// The client's wording, not ours. `quit_confirm_prompt_is_the_clients_own_words` reads it back out
/// of the player's own form.
pub const QUIT_CONFIRM_PROMPT: &str = "Are you sure you want to quit?";

/// Which action a Quit-modal control performs.
///
/// One mapping, used by both the hit test and the draw, so a button cannot highlight for one action
/// and fire another.
fn modal_action(target: Target) -> Option<PreWorldAction> {
    match target {
        Target::QuitConfirmYes => Some(PreWorldAction::QuitConfirmYes),
        Target::QuitConfirmNo => Some(PreWorldAction::QuitConfirmNo),
        Target::DeleteConfirmYes => Some(PreWorldAction::DeleteConfirmYes),
        Target::DeleteConfirmNo => Some(PreWorldAction::DeleteConfirmNo),
        _ => None,
    }
}

/// A stats-dialog control's action. One mapping so the draw's hover state and the hit arm's
/// routing cannot disagree about what a control does (the J7 defect class).
fn stats_dialog_action(target: Target) -> Option<PreWorldAction> {
    match target {
        Target::StatsAdjust { stat, dir } => Some(PreWorldAction::StatsAdjust { stat, dir }),
        Target::StatsReset => Some(PreWorldAction::StatsReset),
        Target::StatsOptimize => Some(PreWorldAction::StatsOptimize),
        _ => None,
    }
}

/// The word a delete must be confirmed with. Matched case-insensitively, so the player does not
/// have to reach for shift or notice their caps lock.
pub const DELETE_CONFIRM_WORD: &str = "YES";

/// The lines a modal displays, as `(dialog-local y, text)`.
///
/// **The authored `<Data>` strings are placeholders, and reading them literally is a defect.**
/// `delete_confirm.xml` gives LabelDefs 1002 and 1005 the *same* sentence with no adapter on either,
/// which is why CAER first drew "Are you sure you want to delete?" twice. Eden shows what the client
/// actually puts there at run time:
///
/// | | 1002 (0,25) | 1005 (0,50) | 1006 (0,75) | buttons |
/// |---|---|---|---|---|
/// | awaiting | `Deleting <name>` | `Type YES to confirm` | — | Cancel |
/// | confirmed | `Deleting <name>` | `YES confirmed` | `Press delete to complete` | Delete + Cancel |
///
/// So 1006's authored text is real and 1002/1005's are not, and the form is a two-state dialog whose
/// typed confirmation is never echoed — it only flips 1005. Provenance: `OWN_CAPTURE` of Eden
/// (DOL-derived, retail-derived) supplied 2026-08-18, both states.
#[must_use]
pub(crate) fn modal_lines(
    modal: preworld_hitbox::Modal,
    subject: Option<&str>,
    confirmed: bool,
) -> Vec<(f32, String)> {
    use preworld_hitbox::Modal;
    match modal {
        // LabelDef 1002 at (0,50), 250x16. This one form really does say what it authors.
        Modal::QuitConfirm => vec![(50.0, "Are you sure you want to quit?".to_string())],
        Modal::DeleteConfirm => {
            // No selected row should be unreachable — Delete does not route without one — but a
            // dialog that names no character is worse than one that names the slot, and inventing a
            // name would be worse than both.
            let who = subject.unwrap_or("this character");
            let mut lines = vec![
                (25.0, format!("Deleting {who}")),
                (
                    50.0,
                    if confirmed {
                        format!("{DELETE_CONFIRM_WORD} confirmed")
                    } else {
                        format!("Type {DELETE_CONFIRM_WORD} to confirm")
                    },
                ),
            ];
            if confirmed {
                lines.push((75.0, "Press delete to complete".to_string()));
            }
            lines
        }
    }
}

/// The two lines a character-select row shows, as the client writes them.
///
/// Eden: `Gwaider the Infiltrator` over `Level 1 in City of Camelot`. CAER drew the bare name and
/// put the **class** on the second line where the location belongs, so a row never said where a
/// character was — while `CharacterSummary::location` had carried it from the overview packet the
/// whole time.
///
/// A free function rather than two `format!`s inside the draw loop, so a test can reach it. That is
/// the same reason `preworld_flow::flow_event_for` was lifted out of the binary.
#[must_use]
pub fn character_row_lines(c: &caer_protocol::overview::CharacterSummary) -> (String, String) {
    let title = if c.class_name.is_empty() {
        c.name.clone()
    } else {
        format!("{} the {}", c.name, c.class_name)
    };
    // An empty location is degraded honestly rather than printed as "Level 1 in ".
    let sub = if c.location.is_empty() {
        format!("Level {}", c.level)
    } else {
        format!("Level {} in {}", c.level, c.location)
    };
    (title, sub)
}

/// Authored size of `pregame/login.xml` (`Width`/`Height`).
pub const LOGIN_DIALOG_W: f32 = 320.0;
pub const LOGIN_DIALOG_H: f32 = 146.0;

/// Centre the Mythic login dialog in the viewport (authored 320×146).
#[must_use]
pub fn login_dialog_pos(viewport: (f32, f32)) -> (f32, f32) {
    let (vw, vh) = viewport;
    ((vw - LOGIN_DIALOG_W) * 0.5, (vh - LOGIN_DIALOG_H) * 0.5)
}

/// Hit-test OK (1001) / QUIT (1002) inside the centred login dialog.
///
/// Button positions from `pregame/login.xml`; hit size matches `button_small` (~80×24).
#[must_use]
pub fn hit_login_dialog(x: f32, y: f32, viewport: (f32, f32)) -> Option<PreWorldAction> {
    let (ox, oy) = login_dialog_pos(viewport);
    let (bw, bh) = (80.0_f32, 24.0_f32);
    let ok = Rect::new(ox + 60.0, oy + 100.0, bw, bh);
    if x >= ok.x && x < ok.x + ok.w && y >= ok.y && y < ok.y + ok.h {
        return Some(PreWorldAction::LoginPlay);
    }
    let quit = Rect::new(ox + 170.0, oy + 100.0, bw, bh);
    if x >= quit.x && x < quit.x + quit.w && y >= quit.y && y < quit.y + quit.h {
        return Some(PreWorldAction::LoginExit);
    }
    None
}

/// Name edit box from `character_creation.xml` ControlId 1051.
fn charcreate_name_rect(viewport: (f32, f32)) -> Rect {
    art_rect(PreWorldScreen::CharCreate, Target::CharCreateName, viewport)
}

/// Male (1090) / Female (1091) from `character_creation.xml`.
///
/// Both are `button_pregame_medium`, 102×21. CAER approximated them as 100×22, so the drawn
/// button and the responding rect were a couple of pixels apart on every edge.
fn charcreate_gender_rect(gender: u8, viewport: (f32, f32)) -> Rect {
    art_rect(
        PreWorldScreen::CharCreate,
        Target::CharCreateGender(gender),
        viewport,
    )
}

/// Authored size of the `button_pregame_medium` template — `pregame/styles.xml:1362` `<Size>`.
///
/// `OWN_CAPTURE`. Race and class controls both use this template. CAER previously approximated it
/// as 100×22, which is 2px wide and 1px tall off the authored geometry on every one of them.
const PREGAME_MEDIUM_BUTTON: (f32, f32) = (102.0, 21.0);

/// Race button rects — ControlIds 1014–1020, `button_pregame_medium`.
fn charcreate_race_rect(index: u8, viewport: (f32, f32)) -> Option<Rect> {
    let c =
        preworld_hitbox::control_for(PreWorldScreen::CharCreate, Target::CharCreateRace(index))?;
    Some(c.art().mapped(PreworldTransform::from_viewport(viewport)))
}

/// All 16 authored class adapter slots (`class_0_text`..`class_15_text`, ControlId 1021..1036).
///
/// A two-column grid at x=795/901, y=378..553 in 25px rows. CAER once implemented only the first
/// five, which both hid eleven classes and bounded hit testing to five.
fn charcreate_class_rect(index: u8, viewport: (f32, f32)) -> Option<Rect> {
    let c =
        preworld_hitbox::control_for(PreWorldScreen::CharCreate, Target::CharCreateClass(index))?;
    Some(c.art().mapped(PreworldTransform::from_viewport(viewport)))
}

/// Resolve a race-button slot for the chosen realm to its `eRace` id.
///
/// Thin wrapper over the creation adapter manifest so the renderer and dispatch cannot disagree
/// about which race a button selects. The three per-realm id tables this replaced had Albion's
/// slots 1 and 2 transposed relative to the captured form.
#[must_use]
pub fn race_id_for_realm(realm: u8, index: u8) -> u8 {
    caer_protocol::creation_adapters::race_adapter_at(realm.max(1), index).map_or(0, |a| a.race_id)
}

/// Loaded pre-world art + WindowManager for login button z-order / hit-testing.
pub struct PreWorldHud {
    screen: PreWorldScreen,
    textures: HashMap<String, TgaImage>,
    /// Synthetic texture page names already uploaded.
    uploaded: std::collections::HashSet<String>,
    pub windows: WindowManager,
    pub last_quads: usize,
    /// Quads from the last [`Self::render`] layout (for compositing the login dialog over the plate).
    last_layout: Vec<UiQuad>,
    client_root: PathBuf,
    /// Race/class flavour text read from the user's own `game.dll` (H7). Empty on an
    /// unrecognised client build — the panes then render blank rather than invented text.
    descriptions: caer_assets::descriptions::DescriptionTable,
    /// Active splash member (`splash1.tga`..`splash8.tga`).
    splash_member: String,
    /// Active zone-loading plate (archive + member).
    loading_archive: String,
    loading_member: String,
    /// Truthful CAER version chrome (never retail 1.130).
    version_label: String,
    /// Char-select overview for slot name labels (optional).
    overview: Option<caer_protocol::overview::CharacterOverview>,
    /// Selected UI slot index on char-select (0..9), if any.
    /// Selected character's **protocol slot**, not its row in the compact list.
    ///
    /// H4: a row index is only meaningful against the overview it came from. Storing one meant an
    /// overview refresh that removed or reordered an earlier character silently retargeted Play at
    /// whoever now sat in that row.
    selected_protocol_slot: Option<u8>,
    /// Optional bitmap font for slot / chrome labels.
    label_font: Option<caer_assets::bitmapfont::BitmapFont>,
    label_font_page: String,
    /// Exact pregame gold fonts named by `pregame/asset.xml`.
    gold_font: Option<caer_assets::bitmapfont::BitmapFont>,
    gold_font_page: String,
    med_gold_font: Option<caer_assets::bitmapfont::BitmapFont>,
    med_gold_font_page: String,
    /// Retail prose loaded from `pregame/realmdesc.mpk:realmdesc.txt`.
    realm_descriptions: [String; 3],
    /// Realm column under the pointer. Retail uses the pressed crop for hover feedback.
    hovered_realm: Option<RealmButton>,
    /// Character-select row under the pointer, if any.
    hovered_slot: Option<u8>,
    /// Control under the pointer on ANY pre-world screen, as the action it would dispatch.
    ///
    /// `set_pointer` used to answer this only for the realm plate and set `None` everywhere else,
    /// which is why nothing past realm select ever lit up.
    hovered_action: Option<PreWorldAction>,
    /// Button art states from the client's own `pregame/styles.xml`, keyed by template name.
    button_templates: HashMap<String, caer_assets::uiskin::ButtonTemplate>,
    /// Resizable frame art from that same skin, keyed by template name.
    ///
    /// Keeping this beside `button_templates` matters: the stats window is a source-authored
    /// `FullResizeImageTemplate`, not a hand-painted black rectangle with a guessed border.
    nine_slices: HashMap<String, caer_assets::uiskin::NineSlice>,
    /// `asset.xml` texture page name (`breadcrumbs`) → `(archive_rel, member)`. Templates address
    /// art by page name, never by filename; keeping the indirection is what lets the round
    /// bottom-row buttons come from `breadcrumbs.tga` while the medium buttons come from
    /// `misc_pieces_new.tga` without either being hardcoded at a draw site.
    texture_pages: HashMap<String, (String, String)>,
    /// Current create-form values, used only to paint the same values the live controls mutate.
    create_draft: caer_protocol::charcreate::CharacterCreateDraft,
    /// Retail source catalogue injected by the shell once. The HUD owns no second table of option
    /// caps or labels; visual values must be the same indices product dispatch accepts.
    appearance_catalog: Option<Arc<crate::preworld_appearance::AppearanceCatalog>>,
    /// Form-local Random locks/entropy. `character_customize.xml` exposes the controls, while
    /// Size itself is encoded by the draft's 1126+ `creation_model` word.
    customizer: crate::preworld_product::CustomizerState,
    /// Is the Options Menu up?
    ///
    /// An overlay, not a screen. Retail draws it **over** character select, which stays visible
    /// behind it; CAER navigated to `PreWorldScreen::Settings`, whose layout draws nothing, so the
    /// options panel floated on black with the character screen gone.
    options_open: bool,
    /// The authored modal currently raised over the screen, if any.
    modal: Option<preworld_hitbox::Modal>,
    /// What the player has typed into the raised modal. Only `delete_confirm.xml` reads it, and it
    /// is never echoed — Eden's dialog shows confirmation by changing a label, not by drawing a
    /// field. Uppercased on the way in so caps lock cannot make a correct answer look wrong.
    modal_typed: String,
    /// Draft the dialog edits. The host syncs it in and reads it back on Accept.
    options: crate::preworld_options::OptionsDraft,
    /// Row under the pointer, for the Description pane and the highlight state.
    options_hover: Option<crate::preworld_options::OptionsId>,
}

impl PreWorldHud {
    #[must_use]
    pub fn new(client_root: PathBuf) -> Self {
        let mut windows = WindowManager::default();
        // Register login buttons as named windows so hit-test / focus share the HUD path.
        for btn in LoginButton::ALL {
            let name = format!("preworld_login_{btn:?}");
            windows.open(&name, (0.0, 0.0));
        }
        let mut textures = HashMap::new();
        // Synthetic 1×1 for letterbox underfills — world/sky must not bleed at plate edges.
        textures.insert(
            FILL_PAGE.to_string(),
            TgaImage {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
            },
        );
        textures.insert(
            WHITE_PAGE.to_string(),
            TgaImage {
                width: 1,
                height: 1,
                rgba: vec![255, 255, 255, 255],
            },
        );
        Self {
            screen: PreWorldScreen::Login,
            textures,
            uploaded: std::collections::HashSet::new(),
            windows,
            last_quads: 0,
            last_layout: Vec::new(),
            descriptions: caer_assets::descriptions::load(&client_root),
            client_root,
            splash_member: "splash1.tga".into(),
            loading_archive: crate::preworld_flow::NEUTRAL_LOADING_PLATE
                .archive_rel
                .into(),
            loading_member: crate::preworld_flow::NEUTRAL_LOADING_PLATE.member.into(),
            version_label: crate::preworld_flow::caer_version_label(),
            overview: None,
            selected_protocol_slot: None,
            label_font: None,
            label_font_page: "preworld_button_large_font".into(),
            gold_font: None,
            gold_font_page: "preworld_large_gold_font".into(),
            med_gold_font: None,
            med_gold_font_page: "preworld_med_gold_font".into(),
            realm_descriptions: std::array::from_fn(|_| String::new()),
            hovered_realm: None,
            hovered_action: None,
            hovered_slot: None,
            button_templates: HashMap::new(),
            nine_slices: HashMap::new(),
            texture_pages: HashMap::new(),
            create_draft: caer_protocol::charcreate::CharacterCreateDraft::albion_briton_stub(
                "", 0,
            ),
            appearance_catalog: None,
            customizer: crate::preworld_product::CustomizerState::default(),
            options_open: false,
            modal: None,
            modal_typed: String::new(),
            options: crate::preworld_options::OptionsDraft::default(),
            options_hover: None,
        }
    }

    /// Is the Options Menu up?
    #[must_use]
    pub fn options_open(&self) -> bool {
        self.options_open
    }

    /// The authored modal currently raised, if any.
    #[must_use]
    pub fn modal(&self) -> Option<preworld_hitbox::Modal> {
        self.modal
    }

    /// Has the raised modal's typed confirmation been satisfied?
    ///
    /// False for a modal that does not ask for one, which is what keeps `QuitConfirm`'s Yes routing
    /// normally rather than becoming unreachable.
    #[must_use]
    pub fn modal_confirmed(&self) -> bool {
        match self.modal {
            Some(preworld_hitbox::Modal::DeleteConfirm) => {
                self.modal_typed.eq_ignore_ascii_case(DELETE_CONFIRM_WORD)
            }
            _ => true,
        }
    }

    /// Feed a typed character to the raised modal. Returns whether anything changed.
    ///
    /// Uppercased, letters only, and capped at the confirmation word's length so a long run of keys
    /// cannot push the match out of reach — a player who mistypes should be able to keep going
    /// rather than having to cancel and start the dialog again.
    pub fn type_into_modal(&mut self, c: char) -> bool {
        if self.modal != Some(preworld_hitbox::Modal::DeleteConfirm) || !c.is_ascii_alphabetic() {
            return false;
        }
        if self.modal_typed.len() >= DELETE_CONFIRM_WORD.len() {
            self.modal_typed.remove(0);
        }
        self.modal_typed.push(c.to_ascii_uppercase());
        true
    }

    /// Backspace in the raised modal. Returns whether anything changed.
    pub fn backspace_modal(&mut self) -> bool {
        self.modal.is_some() && self.modal_typed.pop().is_some()
    }

    /// The character the raised modal is about — the selected row's occupant.
    #[must_use]
    pub fn modal_subject(&self) -> Option<&str> {
        let slot = self.selected_protocol_slot?;
        let ov = self.overview.as_ref()?;
        ov.characters
            .iter()
            .find(|c| c.slot == slot)
            .map(|c| c.name.as_str())
    }

    /// Is the Quit confirmation modal up?
    #[must_use]
    pub fn quit_confirm_open(&self) -> bool {
        self.modal == Some(preworld_hitbox::Modal::QuitConfirm)
    }

    /// Is the Delete confirmation modal up?
    #[must_use]
    pub fn delete_confirm_open(&self) -> bool {
        self.modal == Some(preworld_hitbox::Modal::DeleteConfirm)
    }

    /// Hand the dialog the current settings before it opens (and whenever they change under it).
    pub fn set_options_draft(&mut self, draft: crate::preworld_options::OptionsDraft) {
        self.options = draft;
    }

    /// What Accept would commit.
    #[must_use]
    pub fn options_draft(&self) -> &crate::preworld_options::OptionsDraft {
        &self.options
    }

    /// Apply one dialog interaction. Accept/Cancel close it; the host reads the draft back.
    pub fn apply_options_hit(&mut self, hit: crate::preworld_options::OptionsHit) {
        use crate::preworld_options::OptionsHit;
        match hit {
            OptionsHit::Cycle(id, side) => self.options.cycle(id, side),
            OptionsHit::Press(id) => {
                use crate::preworld_options::{OptionsId, WindowChoice};
                match id {
                    OptionsId::Windowed => self.options.window = Some(WindowChoice::Windowed),
                    OptionsId::FullScreenWindowed => {
                        self.options.window = Some(WindowChoice::FullScreenWindowed);
                    }
                    OptionsId::FullScreen => self.options.window = Some(WindowChoice::FullScreen),
                    _ => {}
                }
            }
            OptionsHit::Accept | OptionsHit::Cancel => {
                self.options_open = false;
                self.options_hover = None;
            }
        }
    }

    /// Apply splash rotation seed (session-stable).
    pub fn set_splash_seed(&mut self, seed: u64) {
        self.splash_member = crate::preworld_flow::splash_member(seed).to_string();
    }

    /// Apply loading plate from flow controller.
    pub fn set_loading_plate(&mut self, plate: crate::preworld_flow::LoadingPlate) {
        self.loading_archive = plate.archive_rel.to_string();
        self.loading_member = plate.member.to_string();
    }

    pub fn set_overview(&mut self, overview: Option<caer_protocol::overview::CharacterOverview>) {
        self.overview = overview;
    }

    pub fn set_selected_protocol_slot(&mut self, slot: Option<u8>) {
        self.selected_protocol_slot = slot;
    }

    pub fn set_create_draft(&mut self, draft: &caer_protocol::charcreate::CharacterCreateDraft) {
        self.create_draft = draft.clone();
    }

    /// Supply the one source-backed customisation catalogue shared with product dispatch.
    pub fn set_appearance_catalog(
        &mut self,
        catalog: Option<Arc<crate::preworld_appearance::AppearanceCatalog>>,
    ) {
        self.appearance_catalog = catalog;
    }

    /// Synchronize the source form's local Random-lock state from product dispatch. Size itself
    /// is read from the draft's model word.
    pub fn set_customizer_state(&mut self, state: crate::preworld_product::CustomizerState) {
        self.customizer = state;
    }

    /// Whether the selected identity has an authored decal map, and therefore a real Tattoo row.
    ///
    /// The headless geometry harness deliberately has no client catalogue; retain the complete
    /// profile in that environment so it can exercise every semantic control. A live HUD always
    /// has the catalogue and therefore follows the selected identity exactly.
    fn customizer_has_tattoo(&self) -> bool {
        self.appearance_catalog.as_ref().map_or(true, |catalog| {
            catalog.has_selector(
                self.create_draft.race,
                self.create_draft.gender,
                crate::preworld_appearance::AppearanceSelector::Tattoo,
            )
        })
    }

    /// Greyed create controls must not fire. `hit_charcreate` is geometry-only so SCN-02 can
    /// probe Continué without a draft; eligibility is applied here.
    fn filter_create_action(&self, action: PreWorldAction) -> Option<PreWorldAction> {
        match action {
            PreWorldAction::CharCreateGender(1) => {
                let male_only =
                    caer_protocol::creation_adapters::race_adapters(self.create_draft.realm)
                        .iter()
                        .find(|a| a.race_id == self.create_draft.race)
                        .is_some_and(|a| a.male_only);
                if male_only {
                    None
                } else {
                    Some(action)
                }
            }
            PreWorldAction::CharCreateClass(slot) => {
                let ok = caer_protocol::creation_adapters::class_adapters_for(
                    self.create_draft.realm,
                    self.create_draft.race,
                    self.create_draft.gender,
                )
                .iter()
                .any(|a| a.slot == slot && a.eligible);
                if ok {
                    Some(action)
                } else {
                    None
                }
            }
            other => Some(other),
        }
    }

    /// Apply the retail appearance catalogue to geometry-only customizer hits.
    ///
    /// `character_customize.xml` contains every optional row in its common layout. The client
    /// adapter makes unsupported rows absent rather than leaving a cosmetic control that only
    /// fails after a click. Keep that truth test here, alongside the corresponding renderer test,
    /// so a male Briton cannot click a tattoo arrow/palette value that dispatch will reject.
    fn filter_customize_action(&self, action: PreWorldAction) -> Option<PreWorldAction> {
        let Some(catalog) = self.appearance_catalog.as_ref() else {
            // GPU-free geometry tests intentionally have no client tree. Live rustdaoc always
            // supplies the catalog and therefore never takes this compatibility path.
            return Some(action);
        };
        let race = self.create_draft.race;
        let gender = self.create_draft.gender;
        let allowed = match action {
            PreWorldAction::CustomizeAdjust { field, .. } => match field {
                CustomizerField::Face => catalog.has_selector(
                    race,
                    gender,
                    crate::preworld_appearance::AppearanceSelector::Face,
                ),
                CustomizerField::HairStyle => catalog.has_selector(
                    race,
                    gender,
                    crate::preworld_appearance::AppearanceSelector::HairStyle,
                ),
                CustomizerField::Tattoo => catalog.has_selector(
                    race,
                    gender,
                    crate::preworld_appearance::AppearanceSelector::Tattoo,
                ),
                CustomizerField::EyeColor => catalog
                    .choices(race, gender)
                    .is_some_and(|choices| !choices.eye_colours().is_empty()),
                CustomizerField::HairColor => {
                    catalog.choices(race, gender).is_some_and(|choices| {
                        !choices
                            .hair_colours(self.create_draft.hair_style)
                            .is_empty()
                    })
                }
                CustomizerField::Size => catalog
                    .choices(race, gender)
                    .is_some_and(|choices| !choices.scale().is_empty()),
                CustomizerField::Mood => true,
                CustomizerField::Morph(_) | CustomizerField::SkinTone => false,
            },
            PreWorldAction::CustomizeSlider { field, .. } => match field {
                CustomizerField::SkinTone => catalog
                    .choices(race, gender)
                    .is_some_and(|choices| !choices.skin_tones().is_empty()),
                CustomizerField::Morph(_) | CustomizerField::Mood => true,
                _ => false,
            },
            PreWorldAction::CustomizeToggleLock { field } => {
                field != CustomizerField::Tattoo || self.customizer_has_tattoo()
            }
            PreWorldAction::CustomizeRandom => catalog.choices(race, gender).is_some(),
            _ => true,
        };
        allowed.then_some(action)
    }

    /// Lazy-load the exact bitmap fonts named by `pregame/asset.xml`.
    /// Read `pregame/styles.xml` once for its authored button and resizable-frame art.
    ///
    /// Loose on disk, like the form XML the geometry already comes from. Absent or unparsable is
    /// not fatal — `button_art` falls back to the Normal crop — because a missing styles file must
    /// cost interaction polish, never the whole screen.
    fn ensure_button_templates(&mut self) {
        if !self.button_templates.is_empty() || !self.nine_slices.is_empty() {
            return;
        }
        let path = self.client_root.join("pregame/styles.xml");
        let Ok(xml) = std::fs::read_to_string(&path) else {
            log::info!(
                "preworld: no {} — buttons draw Normal art only",
                path.display()
            );
            return;
        };
        let Ok(root) = caer_assets::uiskin::Element::parse(&xml) else {
            log::warn!("preworld: {} did not parse", path.display());
            return;
        };
        let mut skin = caer_assets::uiskin::Skin::default();
        fn absorb_all(skin: &mut caer_assets::uiskin::Skin, el: &caer_assets::uiskin::Element) {
            skin.absorb(el);
            for c in &el.children {
                absorb_all(skin, c);
            }
        }
        absorb_all(&mut skin, &root);
        log::info!(
            "preworld: {} button templates and {} nine-slice frames from styles.xml",
            skin.buttons.len(),
            skin.nine_slices.len()
        );
        self.button_templates = skin.buttons;
        self.nine_slices = skin.nine_slices;

        // `asset.xml` resolves a template's page name to an archive member.
        let apath = self.client_root.join("pregame/asset.xml");
        if let Ok(axml) = std::fs::read_to_string(&apath) {
            if let Ok(aroot) = caer_assets::uiskin::Element::parse(&axml) {
                let mut assets = caer_assets::uiskin::Skin::default();
                absorb_all(&mut assets, &aroot);
                for (name, tex) in assets.textures {
                    // Two forms: `archive://pregame/pregame.mpk:misc_pieces_new.tga`, and a bare
                    // loose path like `ui/atlantis/atlantis_01.tga`. Only the archive form used to
                    // be recorded, so `page1` — where every checkbox and radio in the skin lives —
                    // had no entry and `generic_check` silently drew nothing.
                    match tex.file.strip_prefix("archive://") {
                        Some(rest) => {
                            let Some((arch, member)) = rest.rsplit_once(':') else {
                                continue;
                            };
                            self.texture_pages
                                .insert(name, (arch.to_string(), member.to_string()));
                        }
                        // Empty archive means "loose file at this path".
                        None if tex.file.contains('/') => {
                            self.texture_pages
                                .insert(name, (String::new(), tex.file.clone()));
                        }
                        None => {}
                    }
                }
            }
        }
        log::info!(
            "preworld: {} texture pages mapped",
            self.texture_pages.len()
        );
    }

    /// Ensure a template's atlas is loaded, returning the texture key to draw with.
    fn ensure_button_page(&mut self, template: &str) -> Option<String> {
        let page = self
            .button_templates
            .get(&template.to_ascii_lowercase())?
            .texture
            .to_ascii_lowercase();
        if page.is_empty() || page == "none" {
            return None;
        }
        let (arch, member) = self.texture_pages.get(&page)?.clone();
        let key = page_texture_key(&member);
        if !self.textures.contains_key(&key) {
            let loaded = if arch.is_empty() {
                self.load_loose_tga(&member)
            } else {
                self.load_tga(&arch, &member)
            };
            if let Err(e) = loaded {
                log::warn!("preworld: {member} for template {template}: {e}");
                return None;
            }
        }
        self.textures.contains_key(&key).then_some(key)
    }

    /// Ensure the texture page backing a retail `FullResizeImageTemplate` is resident.
    ///
    /// This intentionally shares the `asset.xml` page-resolution path with buttons. The form
    /// names the frame (`dlg_background_noresize`); the skin maps that to `dialog`; and
    /// `asset.xml` says which archive member supplies that page. Hard-coding `dialog_box.tga` at
    /// a draw site would make the next source frame silently inherit the wrong asset contract.
    fn ensure_nine_slice_page(&mut self, template: &str) -> Option<String> {
        let page = self
            .nine_slices
            .get(&template.to_ascii_lowercase())?
            .texture
            .to_ascii_lowercase();
        if page.is_empty() || page == "none" {
            return None;
        }
        let (arch, member) = self.texture_pages.get(&page)?.clone();
        let key = page_texture_key(&member);
        if !self.textures.contains_key(&key) {
            let loaded = if arch.is_empty() {
                self.load_loose_tga(&member)
            } else {
                self.load_tga(&arch, &member)
            };
            if let Err(e) = loaded {
                log::warn!("preworld: {member} for frame {template}: {e}");
                return None;
            }
        }
        self.textures.contains_key(&key).then_some(key)
    }

    pub fn ensure_label_font(&mut self) {
        if self.label_font.is_none() {
            if let Some(font) = load_loose_bitmap_font(
                &self.client_root,
                &["ui/fonts/button_12.tga", "ui/atlantis/fonts/button_12.tga"],
            ) {
                self.textures
                    .insert(self.label_font_page.clone(), font.atlas.clone());
                self.label_font = Some(font);
            }
        }
        if self.gold_font.is_none() {
            if let Some(font) = load_loose_bitmap_font(
                &self.client_root,
                &["ui/fonts/label.tga", "ui/atlantis/fonts/label.tga"],
            ) {
                self.textures
                    .insert(self.gold_font_page.clone(), font.atlas.clone());
                self.gold_font = Some(font);
            }
        }
        if self.med_gold_font.is_none() {
            if let Some(font) = load_loose_bitmap_font(
                &self.client_root,
                &["ui/fonts/title_24.tga", "ui/atlantis/fonts/title_24.tga"],
            ) {
                self.textures
                    .insert(self.med_gold_font_page.clone(), font.atlas.clone());
                self.med_gold_font = Some(font);
            }
        }
    }

    fn ensure_realm_descriptions(&mut self) -> Result<(), String> {
        if self.realm_descriptions.iter().all(|text| !text.is_empty()) {
            return Ok(());
        }
        let bytes = read_mpk_member(&self.client_root, "pregame/realmdesc.mpk", "realmdesc.txt")?;
        self.realm_descriptions = parse_realm_descriptions(&bytes)?;
        Ok(())
    }

    #[must_use]
    pub fn last_layout_quads(&self) -> &[UiQuad] {
        &self.last_layout
    }

    #[must_use]
    pub fn screen(&self) -> PreWorldScreen {
        self.screen
    }

    pub fn set_screen(&mut self, screen: PreWorldScreen) {
        self.screen = screen;
        if screen != PreWorldScreen::RealmSelect {
            self.hovered_realm = None;
        }
    }

    /// Update visual pointer feedback without dispatching anything.
    ///
    /// Answers for **every** screen. It previously answered only for the realm plate and stored
    /// `None` otherwise, so every control after realm select was inert to the pointer no matter
    /// what art it had.
    pub fn set_pointer(&mut self, x: f32, y: f32, viewport: (f32, f32)) {
        self.options_hover = self
            .options_open
            .then(|| {
                let (px, py) = unmap_pregame(x, y, viewport);
                crate::preworld_options::hovered_row(px, py).and_then(|r| r.id)
            })
            .flatten();
        self.hovered_realm = if self.screen == PreWorldScreen::RealmSelect {
            hit_realm(x, y, viewport)
        } else {
            None
        };
        self.hovered_action = self.hit_action(x, y, viewport);
        self.hovered_slot = (self.screen == PreWorldScreen::CharSelect)
            .then(|| hit_charselect_slot(x, y, viewport))
            .flatten();
    }

    /// Art state for a control: pressed when it is the current selection, highlit under the
    /// pointer, disabled when the chrome flag says so, otherwise normal.
    fn button_state(
        &self,
        action: PreWorldAction,
        selected: bool,
        enabled: bool,
    ) -> caer_assets::uiskin::ButtonState {
        use caer_assets::uiskin::ButtonState;
        if !enabled {
            ButtonState::Disabled
        } else if selected {
            ButtonState::Pressed
        } else if self.hovered_action == Some(action) {
            ButtonState::Highlit
        } else {
            ButtonState::Normal
        }
    }

    /// Draw an authored button at its authored position, in the right state.
    ///
    /// Position and size both come from the client: the form XML places the control, the template
    /// sizes it. Nothing here is a measured-off-a-screenshot rect.
    fn append_template_button(
        &self,
        out: &mut Vec<UiQuad>,
        template: &str,
        (x, y): (f32, f32),
        state: caer_assets::uiskin::ButtonState,
        viewport: (f32, f32),
    ) {
        let Some(t) = self.button_templates.get(&template.to_ascii_lowercase()) else {
            return;
        };
        let page = t.texture.to_ascii_lowercase();
        let Some((_, member)) = self.texture_pages.get(&page) else {
            return;
        };
        let key = page_texture_key(member);
        if !self.textures.contains_key(&key) {
            return;
        }
        let (sx, sy, sw, sh) = t.crop(state);
        out.push(UiQuad {
            dst: map_pregame_rect(x, y, sw as f32, sh as f32, viewport),
            src: Rect::new(sx as f32, sy as f32, sw as f32, sh as f32),
            texture: key,
            color: skinui::WHITE,
        });
    }

    /// Draw one retail `FullResizeImageTemplate` as its authored nine patches.
    ///
    /// Corners retain their source dimensions, edges stretch only along their long axis, and the
    /// middle stretches in both directions. The method returns false if the source template or
    /// its page is unavailable so a caller can retain a deliberately plain fallback for damaged
    /// installs without pretending the fallback is retail artwork.
    fn append_nine_slice(
        &self,
        out: &mut Vec<UiQuad>,
        template: &str,
        rect: Rect,
        viewport: (f32, f32),
    ) -> bool {
        let Some(frame) = self.nine_slices.get(&template.to_ascii_lowercase()) else {
            return false;
        };
        let page = frame.texture.to_ascii_lowercase();
        let Some((_, member)) = self.texture_pages.get(&page) else {
            return false;
        };
        let key = page_texture_key(member);
        if !self.textures.contains_key(&key) {
            return false;
        }

        let widths = [frame.left_width, frame.middle_width, frame.right_width];
        let heights = [frame.top_height, frame.middle_height, frame.bottom_height];
        if widths.iter().any(|&value| value <= 0)
            || heights.iter().any(|&value| value <= 0)
            || rect.w < (frame.left_width + frame.right_width) as f32
            || rect.h < (frame.top_height + frame.bottom_height) as f32
        {
            return false;
        }

        let dst_widths = [
            frame.left_width as f32,
            rect.w - (frame.left_width + frame.right_width) as f32,
            frame.right_width as f32,
        ];
        let dst_heights = [
            frame.top_height as f32,
            rect.h - (frame.top_height + frame.bottom_height) as f32,
            frame.bottom_height as f32,
        ];
        let dst_x = [
            rect.x,
            rect.x + dst_widths[0],
            rect.x + rect.w - dst_widths[2],
        ];
        let dst_y = [
            rect.y,
            rect.y + dst_heights[0],
            rect.y + rect.h - dst_heights[2],
        ];

        for row in 0..3 {
            for column in 0..3 {
                let index = row * 3 + column;
                let (src_x, src_y) = frame.patches[index];
                out.push(UiQuad {
                    dst: map_pregame_rect(
                        dst_x[column],
                        dst_y[row],
                        dst_widths[column],
                        dst_heights[row],
                        viewport,
                    ),
                    src: Rect::new(
                        src_x as f32,
                        src_y as f32,
                        widths[column] as f32,
                        heights[row] as f32,
                    ),
                    texture: key.clone(),
                    color: skinui::WHITE,
                });
            }
        }
        true
    }

    /// Atlas crop + label colour for a template in a state, from `pregame/styles.xml`.
    ///
    /// Falls back to the authored `button_pregame_medium` Normal crop when the styles file is
    /// unavailable, which keeps the form drawable on a tree we cannot read rather than blanking it.
    fn button_art(
        &self,
        template: &str,
        state: caer_assets::uiskin::ButtonState,
    ) -> (Rect, Option<Rgba>) {
        match self.button_templates.get(&template.to_ascii_lowercase()) {
            Some(t) => {
                let (x, y, w, h) = t.crop(state);
                (
                    Rect::new(x as f32, y as f32, w as f32, h as f32),
                    Some(t.color(state)),
                )
            }
            None => (
                Rect::new(0.0, 106.0, PREGAME_MEDIUM_BUTTON.0, PREGAME_MEDIUM_BUTTON.1),
                None,
            ),
        }
    }

    /// Current realm crest selected by authored hit-testing. Exposed for live diagnostics so a
    /// missing visual highlight can be separated from a missing pointer event.
    #[must_use]
    pub fn hovered_realm(&self) -> Option<RealmButton> {
        self.hovered_realm
    }

    /// Everything the pointer is currently lighting up, as one comparable value.
    ///
    /// The window loop repaints when this changes. Watching only `hovered_realm` meant the
    /// character screen's chrome and rows highlighted on the next animation tick rather than on
    /// the move, which reads as a control that responds late or not at all.
    #[must_use]
    pub fn hover_state(&self) -> HoverState {
        HoverState {
            realm: self.hovered_realm,
            action: self.hovered_action,
            slot: self.hovered_slot,
            options: self.options_hover,
        }
    }

    /// Ensure background (+ login chrome) members are decoded into `textures`.
    /// Soft-fail per member so one bad BMP does not blank the whole screen.
    pub fn ensure_loaded(&mut self) -> Result<(), String> {
        let required_background = match self.screen {
            PreWorldScreen::Splash => Some(self.splash_member.to_ascii_lowercase()),
            PreWorldScreen::Loading => Some(self.loading_member.to_ascii_lowercase()),
            screen => screen.background_member().map(str::to_ascii_lowercase),
        };
        let mut first_err: Option<String> = None;
        let mut push_err = |e: String| {
            if first_err.is_none() {
                first_err = Some(e);
            }
        };
        if self.screen == PreWorldScreen::Login {
            for btn in LoginButton::ALL {
                if let Err(e) = self.load_bmp("data/login2.mpk", btn.norm_member()) {
                    push_err(e);
                }
                if let Err(e) = self.load_bmp("data/login2.mpk", btn.down_member()) {
                    push_err(e);
                }
            }
            if let Err(e) = self.load_bmp("data/login2.mpk", "logo_daoc.bmp") {
                push_err(e);
            }
        }
        if self.screen == PreWorldScreen::Splash {
            // Loadbars live loose under data/, not in splash.mpk (game.dll string table).
            if let Err(e) = self.load_loose_tga("data/splashloadbar1.tga") {
                push_err(e);
            }
            if let Err(e) = self.load_loose_tga("data/splashloadbar2.tga") {
                push_err(e);
            }
            self.ensure_label_font();
            self.ensure_button_templates();
        }
        if self.screen == PreWorldScreen::CharSelect {
            self.ensure_label_font();
            self.ensure_button_templates();
            for tmpl in [
                "quit",
                "realm",
                "customize",
                "options",
                "delete_char",
                "play",
                "button_large_radio",
            ] {
                let _ = self.ensure_button_page(tmpl);
            }
            if let Err(e) = self.load_tgas_from(
                "pregame/pregame.mpk",
                &["character_selection.tga", "buttons_WIDE.tga"],
            ) {
                push_err(e);
            }
        }
        if self.screen == PreWorldScreen::RealmSelect {
            self.ensure_label_font();
            self.ensure_button_templates();
            if let Err(e) = self.ensure_realm_descriptions() {
                push_err(e);
            }
            // One archive read for the background + chrome + all three crests. These used to be
            // four separate `pregame.mpk` inflates on top of the background's, ~177 ms each.
            let mut members = vec!["realm_selection.tga", "buttons_WIDE.tga"];
            members.extend(RealmButton::ALL.iter().map(|b| b.member()));
            if let Err(e) = self.load_tgas_from("pregame/pregame.mpk", &members) {
                push_err(e);
            }
        }
        if self.screen == PreWorldScreen::CharCreate {
            self.ensure_label_font();
            self.ensure_button_templates();
            // The bottom row is authored as three round buttons off `breadcrumbs`, and Random
            // comes off `small_button` — neither is the medium-button atlas.
            for tmpl in ["quit", "realm", "customize", "button_pregame_small"] {
                let _ = self.ensure_button_page(tmpl);
            }
            if let Err(e) = self.load_tgas_from(
                "pregame/pregame.mpk",
                &[
                    "character_creation.tga",
                    "misc_pieces_new.tga",
                    "small_button.tga",
                ],
            ) {
                push_err(e);
            }
        }
        if matches!(
            self.screen,
            PreWorldScreen::CharCustomize | PreWorldScreen::CharStats
        ) {
            self.ensure_label_font();
            self.ensure_button_templates();
            // `character_customize.xml` reaches across the legacy pregame pages: the round
            // bottom row lives on breadcrumbs, arrows on slider, medium buttons on
            // misc_pieces_new, and the three palette strips in two archives. Resolve those
            // assets through the same template/page indirection as every other pre-world form.
            for tmpl in [
                "camera_reset",
                "camera_left",
                "camera_right",
                "camera_up",
                "camera_down",
                "camera_zoom_in",
                "camera_zoom_out",
                "button_left",
                "button_right",
                "lock",
                "button_pregame_medium",
                "quit",
                "realm",
                "race_class",
                "location",
                "assign_stats",
            ] {
                let _ = self.ensure_button_page(tmpl);
            }
            if let Err(e) = self.load_tgas_from(
                "pregame/pregame.mpk",
                &[
                    "buttons_WIDE.tga",
                    "misc_pieces_new.tga",
                    "slider.tga",
                    "skin_color_palettes.tga",
                    "eye_color_palettes.tga",
                    "color_picker_indicator.tga",
                ],
            ) {
                push_err(e);
            }
            if let Err(e) =
                self.load_tgas_from("pregame/pregame002.mpk", &["hair_colors_palettes.tga"])
            {
                push_err(e);
            }
        }
        // The Options Menu's own art, whatever screen it is over. All four templates are in
        // `pregame/styles.xml` and their pages in `pregame/asset.xml`, so this is the same
        // template → page → member path every other pre-world button already uses.
        if self.options_open {
            self.ensure_label_font();
            self.ensure_button_templates();
            for tmpl in [
                "button_left",
                "button_right",
                "generic_check",
                "button_large",
            ] {
                let _ = self.ensure_button_page(tmpl);
            }
        }
        // The Quit modal's `button_small` art, whatever screen it is over. Without this the two
        // buttons draw as bare words: `append_template_button` takes `&self`, so it cannot load a
        // page — it silently draws nothing when the atlas is absent, which looks identical to a
        // control that was never authored. That is the same shape as ledger J10, where the login
        // dialog's OK and QUIT draw as bare text off this very page.
        if self.modal.is_some() || self.screen == PreWorldScreen::CharStats {
            self.ensure_label_font();
            self.ensure_button_templates();
            let _ = self.ensure_button_page("button_small");
        }
        // The stats dialog's mini arrows are two further templates on their own page; same
        // silent-nothing failure mode as above if the atlas never loads. Its surrounding window
        // is likewise a source-authored nine-slice, resolved through `asset.xml` rather than a
        // local colour approximation.
        if self.screen == PreWorldScreen::CharStats {
            let _ = self.ensure_button_page("button_pregame_mini_right");
            let _ = self.ensure_button_page("button_pregame_mini_left");
            let _ = self.ensure_nine_slice_page("dlg_background_noresize");
        }
        // Backgrounds last: the per-screen batches above already fetched the ones that share an
        // archive with other members, so this is a cache hit for them and one read for the rest.
        // Running it first meant `pregame.mpk` was inflated for the background and then again for
        // the batch.
        if let Err(e) = self.load_member_for_screen(self.screen) {
            push_err(e);
        }
        if let Some(required) = required_background {
            if !self.textures.contains_key(&required) {
                let detail = first_err.unwrap_or_else(|| "decoder returned no texture".to_string());
                return Err(format!(
                    "required preworld background `{required}` is unavailable for {:?}: {detail}",
                    self.screen
                ));
            }
        }
        match first_err {
            Some(e) => {
                log::warn!("preworld: partial load: {e}");
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn load_member_for_screen(&mut self, screen: PreWorldScreen) -> Result<(), String> {
        match screen {
            PreWorldScreen::Splash => {
                let member = self.splash_member.clone();
                self.load_tga("pregame/splash.mpk", &member)
            }
            PreWorldScreen::Loading => {
                let arch = self.loading_archive.clone();
                let member = self.loading_member.clone();
                if member.ends_with(".dds") {
                    self.load_dds(&arch, &member)
                } else if member.ends_with(".tga") {
                    self.load_tga(&arch, &member)
                } else {
                    self.load_bmp(&arch, &member)
                }
            }
            _ => {
                let (Some(arch), Some(member)) = (screen.archive_rel(), screen.background_member())
                else {
                    return Ok(());
                };
                if member.ends_with(".tga") {
                    self.load_tga(arch, member)
                } else if member.ends_with(".dds") {
                    self.load_dds(arch, member)
                } else {
                    self.load_bmp(arch, member)
                }
            }
        }
    }

    fn load_tga(&mut self, arch_rel: &str, member: &str) -> Result<(), String> {
        self.load_tgas_from(arch_rel, &[member])
    }

    /// Decode several TGA members out of **one** archive read, inflating only those members.
    ///
    /// This used to be a full-archive inflate per member: `caer_assets::open` reads the whole
    /// `.mpk` and decompresses every member, then keeps one and drops the rest. Measured on the lab
    /// tree (release), `pregame/pregame.mpk` was ~177 ms per call. Realm select needed four members
    /// from it — background, `buttons_WIDE.tga`, and three realm crests — so it paid that four
    /// times and stalled the winit event thread for ~723 ms, roughly 43 missed frames, on the one
    /// screen the player looks at first.
    ///
    /// `caer_assets::open_named` seeks by the directory's offsets, so the cost is now the file read
    /// plus the wanted members, not the archive. That is still synchronous — the async boundary is
    /// the separate H5 repair — but there is no longer a large fixed cost to hide behind a spinner.
    ///
    /// Soft-fails per member so one bad decode cannot blank the screen, matching `load_tga`.
    fn load_tgas_from(&mut self, arch_rel: &str, members: &[&str]) -> Result<(), String> {
        let wanted: Vec<&str> = members
            .iter()
            .copied()
            .filter(|m| !self.textures.contains_key(&m.to_ascii_lowercase()))
            .collect();
        if wanted.is_empty() {
            return Ok(());
        }
        let path = self.client_root.join(arch_rel);
        let entries = caer_assets::open_named(&path, &wanted)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut first_err: Option<String> = None;
        for member in wanted {
            let Some(entry) = entries.iter().find(|e| e.name.eq_ignore_ascii_case(member)) else {
                first_err.get_or_insert(format!("{member} not in {arch_rel}"));
                continue;
            };
            match caer_assets::tga::decode(&entry.data) {
                Ok(img) => {
                    self.textures.insert(member.to_ascii_lowercase(), img);
                }
                Err(e) => {
                    first_err.get_or_insert(format!("{member}: {e}"));
                }
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    fn load_dds(&mut self, arch_rel: &str, member: &str) -> Result<(), String> {
        let key = member.to_ascii_lowercase();
        if self.textures.contains_key(&key) {
            return Ok(());
        }
        let bytes = read_mpk_member(&self.client_root, arch_rel, member)?;
        let tex = caer_assets::dds::read_model_dds(&bytes).map_err(|e| format!("{member}: {e}"))?;
        let (width, height, rgba) = tex
            .rgba8_mip0()
            .ok_or_else(|| format!("{member}: empty mip0"))?;
        self.textures.insert(
            key,
            TgaImage {
                width,
                height,
                rgba,
            },
        );
        Ok(())
    }

    fn load_bmp(&mut self, arch_rel: &str, member: &str) -> Result<(), String> {
        let key = member.to_ascii_lowercase();
        if self.textures.contains_key(&key) {
            return Ok(());
        }
        let bytes = read_mpk_member(&self.client_root, arch_rel, member)?;
        let img = caer_assets::bmp::decode(&bytes).map_err(|e| format!("{member}: {e}"))?;
        self.textures.insert(key, img);
        Ok(())
    }

    /// Texture key for a page's file, archive member or loose path alike — the basename, lowered.
    ///
    /// `append_template_button` and `ensure_button_page` must agree on this or a page loads and
    /// then fails its own `contains_key` check, which looks exactly like the art being missing.
    fn load_loose_tga(&mut self, rel: &str) -> Result<(), String> {
        let key = Path::new(rel)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(rel)
            .to_ascii_lowercase();
        if self.textures.contains_key(&key) {
            return Ok(());
        }
        let path = self.client_root.join(rel);
        let bytes = std::fs::read(&path).map_err(|e| format!("{rel}: {e}"))?;
        let img = caer_assets::tga::decode(&bytes).map_err(|e| format!("{rel}: {e}"))?;
        self.textures.insert(key, img);
        Ok(())
    }

    /// Draw a screen's authored gold caption from [`PLATE_CAPTIONS`].
    ///
    /// One place, so the three plates cannot drift apart and a literal cannot creep back in beside
    /// the table. No-op before the large gold font resolves.
    fn append_plate_caption(
        &self,
        screen: PreWorldScreen,
        viewport: (f32, f32),
        out: &mut Vec<UiQuad>,
    ) {
        let (Some(font), Some(cap)) = (self.gold_font.as_ref(), plate_caption(screen)) else {
            return;
        };
        append_text(
            font,
            &self.gold_font_page,
            cap.text,
            map_pregame_rect(cap.x, cap.y, cap.w, cap.h, viewport),
            gold_color(),
            true,
            out,
        );
    }

    /// Build screen-space quads for the active pre-world surface.
    pub fn layout(&self, viewport: (f32, f32)) -> Vec<UiQuad> {
        let mut out = Vec::new();
        let (vw, vh) = viewport;
        match self.screen {
            PreWorldScreen::Splash | PreWorldScreen::Loading => {
                out.push(UiQuad {
                    dst: Rect::new(0.0, 0.0, vw, vh),
                    src: Rect::new(0.0, 0.0, 1.0, 1.0),
                    texture: FILL_PAGE.into(),
                    color: skinui::WHITE,
                });
                let member = if self.screen == PreWorldScreen::Splash {
                    self.splash_member.as_str()
                } else {
                    self.loading_member.as_str()
                };
                if let Some(img) = self.textures.get(&member.to_ascii_lowercase()) {
                    let (iw, ih) = (img.width as f32, img.height as f32);
                    // These are opaque full-screen plates, not framed photographs.  In
                    // particular, the 2:1 link-dead/loading sheet (`spirit1.dds`) used to be
                    // aspect-fit here, leaving enormous black bands above and below it on a 4:3
                    // surface.  Retail stretches its authored plate to the consumer viewport,
                    // just like the other pre-world backgrounds.
                    let (sx, sy) = (vw / iw, vh / ih);
                    let (dw, dh, ox, oy) = (vw, vh, 0.0, 0.0);
                    out.push(UiQuad {
                        dst: Rect::new(ox, oy, dw, dh),
                        src: Rect::new(0.0, 0.0, iw, ih),
                        texture: member.into(),
                        color: skinui::WHITE,
                    });
                    if self.screen == PreWorldScreen::Splash {
                        if let Some(bar) = self.textures.get("splashloadbar1.tga") {
                            // The plate stretches, so retain the loadbar's position and size in
                            // the plate's *source* coordinate system rather than letting a
                            // uniform aspect-fit scale pull it toward a letterbox seam.
                            let bw = bar.width as f32 * sx;
                            let bh = bar.height as f32 * sy;
                            out.push(UiQuad {
                                dst: Rect::new(
                                    ox + (dw - bw) * 0.5,
                                    oy + dh * 0.88 - bh * 0.5,
                                    bw,
                                    bh,
                                ),
                                src: Rect::new(0.0, 0.0, bar.width as f32, bar.height as f32),
                                texture: "splashloadbar1.tga".into(),
                                color: skinui::WHITE,
                            });
                        }
                    }
                }
            }
            PreWorldScreen::Login => {
                if let Some(img) = self.textures.get("back_tile.bmp") {
                    // Tile the 512² backplate across the viewport.
                    let tw = img.width as f32;
                    let th = img.height as f32;
                    let mut y = 0.0;
                    while y < vh {
                        let mut x = 0.0;
                        while x < vw {
                            let w = (vw - x).min(tw);
                            let h = (vh - y).min(th);
                            out.push(UiQuad {
                                dst: Rect::new(x, y, w, h),
                                src: Rect::new(0.0, 0.0, w, h),
                                texture: "back_tile.bmp".into(),
                                color: skinui::WHITE,
                            });
                            x += tw;
                        }
                        y += th;
                    }
                }
                if let Some(logo) = self.textures.get("logo_daoc.bmp") {
                    let lw = logo.width as f32;
                    let lh = logo.height as f32;
                    out.push(UiQuad {
                        dst: Rect::new((vw - lw) * 0.5, 48.0, lw, lh),
                        src: Rect::new(0.0, 0.0, lw, lh),
                        texture: "logo_daoc.bmp".into(),
                        color: skinui::WHITE,
                    });
                }
                for btn in LoginButton::ALL {
                    let r = login_btn_rect(btn, viewport);
                    let tex = btn.norm_member();
                    if self.textures.contains_key(&tex.to_ascii_lowercase()) {
                        out.push(UiQuad {
                            dst: r,
                            src: Rect::new(0.0, 0.0, r.w, r.h),
                            texture: tex.into(),
                            color: skinui::WHITE,
                        });
                    }
                }
            }
            PreWorldScreen::RealmSelect
            | PreWorldScreen::CharSelect
            | PreWorldScreen::CharCreate
            // E6/B3: same scene plate as the create form — the subject stands through the whole
            // creation chain. Form chrome comes with the slice that draws the forms.
            | PreWorldScreen::CharCustomize
            | PreWorldScreen::CharStats => {
                let xf = PreworldTransform::from_viewport(viewport);
                let plate = xf.plate_rect();
                let mut fill = |r: Rect| {
                    if r.w > 0.5 && r.h > 0.5 {
                        out.push(UiQuad {
                            dst: r,
                            src: Rect::new(0.0, 0.0, 1.0, 1.0),
                            texture: FILL_PAGE.into(),
                            color: skinui::WHITE,
                        });
                    }
                };
                if self.screen.has_scene_behind() {
                    fill(Rect::new(0.0, 0.0, vw, plate.y));
                    fill(Rect::new(
                        0.0,
                        plate.y + plate.h,
                        vw,
                        vh - (plate.y + plate.h),
                    ));
                    fill(Rect::new(0.0, plate.y, plate.x, plate.h));
                    fill(Rect::new(
                        plate.x + plate.w,
                        plate.y,
                        vw - (plate.x + plate.w),
                        plate.h,
                    ));
                } else {
                    fill(Rect::new(0.0, 0.0, vw, vh));
                }
                if let Some(member) = self.screen.background_member() {
                    if let Some(img) = self.textures.get(&member.to_ascii_lowercase()) {
                        let (iw, ih) = (img.width as f32, img.height as f32);
                        out.push(UiQuad {
                            dst: plate,
                            src: Rect::new(0.0, 0.0, iw, ih),
                            texture: member.into(),
                            color: skinui::WHITE,
                        });
                    }
                }
                if self.screen == PreWorldScreen::RealmSelect {
                    self.append_realm_controls(&mut out, viewport);
                }
                if self.screen == PreWorldScreen::CharSelect {
                    self.append_charselect_labels(&mut out, viewport);
                }
                if self.screen == PreWorldScreen::CharCreate {
                    self.append_charcreate_controls(&mut out, viewport);
                }
                if matches!(
                    self.screen,
                    PreWorldScreen::CharCustomize | PreWorldScreen::CharStats
                ) {
                    self.append_charcustomize_controls(&mut out, viewport);
                }
                if self.screen == PreWorldScreen::CharStats {
                    self.append_stats_dialog(&mut out, viewport);
                }
            }
        }
        if self.screen == PreWorldScreen::Splash {
            self.append_version_label(&mut out, viewport);
        }
        // Last, and over whatever screen is underneath — that is what makes it the Options Menu
        // rather than a screen called Settings.
        if self.options_open {
            self.append_options_menu(&mut out, viewport);
        }
        // In front of the Options Menu, matching the hit test's order. Both being "last" is how a
        // dialog ends up clickable under another one.
        if let Some(modal) = self.modal {
            self.append_modal(modal, &mut out, viewport);
        }
        out
    }

    /// An authored confirm modal, drawn from its own form.
    ///
    /// The plate is a flat opaque fill with a hairline border for the same reason the Options
    /// dialog's is: `dlg_background_noresize` is a nine-slice and the slicer does not exist yet, so
    /// honest chrome beats a wrong stretch. Opaque matters here — a translucent confirm over the
    /// character rows would let two sets of text fight.
    fn append_modal(
        &self,
        modal: preworld_hitbox::Modal,
        out: &mut Vec<UiQuad>,
        viewport: (f32, f32),
    ) {
        let Some(font) = self.label_font.as_ref() else {
            return;
        };
        let (ox, oy) = modal.origin();
        let (dw, dh) = modal.size();
        let mut fill = |x: f32, y: f32, w: f32, h: f32, color: Rgba| {
            out.push(UiQuad {
                dst: map_pregame_rect(x, y, w, h, viewport),
                src: Rect::new(0.0, 0.0, 1.0, 1.0),
                texture: WHITE_PAGE.into(),
                color,
            });
        };
        fill(
            ox - 1.0,
            oy - 1.0,
            dw + 2.0,
            dh + 2.0,
            Rgba {
                r: 118,
                g: 96,
                b: 48,
                a: 255,
            },
        );
        fill(
            ox,
            oy,
            dw,
            dh,
            Rgba {
                r: 8,
                g: 8,
                b: 8,
                a: 255,
            },
        );
        // The form's own `LabelDef`s, at their authored y, each 250x16 and centred.
        let confirmed = self.modal_confirmed();
        for (y, line) in modal_lines(modal, self.modal_subject(), confirmed) {
            skinui::text_quads(
                font,
                &self.label_font_page,
                &skinui::UiText {
                    text: line,
                    rect: map_pregame_rect(ox, oy + y, dw, 16.0, viewport),
                    color: skinui::WHITE,
                    center_horizontally: true,
                    font: None,
                    adapter: None,
                },
                out,
            );
        }
        for c in preworld_hitbox::modal_controls(modal) {
            // Eden **hides** the Delete button until YES is typed rather than greying it, and the
            // hit test below refuses it for the same reason. Drawing a control the click cannot
            // reach is the defect A12 was about, run in reverse.
            if !confirmed && modal_action(c.target) == Some(PreWorldAction::DeleteConfirmYes) {
                continue;
            }
            let art = c.art();
            // Hover comes from `hovered_action`, the same value the click routes through, so the
            // highlit button and the button that acts cannot be different ones (J7).
            let hovered = self.hovered_action == modal_action(c.target);
            self.append_template_button(
                out,
                "button_small",
                (art.x, art.y),
                if hovered {
                    caer_assets::uiskin::ButtonState::Highlit
                } else {
                    caer_assets::uiskin::ButtonState::Normal
                },
                viewport,
            );
            skinui::text_quads(
                font,
                &self.label_font_page,
                &skinui::UiText {
                    text: c.name.into(),
                    rect: map_pregame_rect(art.x, art.y, art.w, art.h, viewport),
                    color: if hovered {
                        skinui::WHITE
                    } else {
                        pregame_gray()
                    },
                    center_horizontally: true,
                    font: None,
                    adapter: None,
                },
                out,
            );
        }
    }

    /// The Options Menu, drawn from `pregame/styles.xml` art and `game.dll` strings.
    ///
    /// See [`crate::preworld_options`] for what is client data here and what is derived.
    fn append_options_menu(&self, out: &mut Vec<UiQuad>, viewport: (f32, f32)) {
        use crate::preworld_options as opt;
        let Some(font) = self.label_font.as_ref() else {
            return;
        };
        let (dx, dy, dw, dh) = opt::DIALOG;
        // The frame. `dlg_lg_title_noresize` is a nine-slice over the `dialog` page; until the
        // slicer exists it is a flat opaque fill with a hairline border, which is honest chrome
        // rather than a wrong stretch. Opaque because retail's is: the dialog must not let the
        // character rows behind it read through, or the two sets of text fight each other.
        let border = Rgba {
            r: 118,
            g: 96,
            b: 48,
            a: 255,
        };
        let mut fill = |x: f32, y: f32, w: f32, h: f32, color: Rgba| {
            out.push(UiQuad {
                dst: map_pregame_rect(x, y, w, h, viewport),
                src: Rect::new(0.0, 0.0, 1.0, 1.0),
                texture: WHITE_PAGE.into(),
                color,
            });
        };
        fill(dx - 1.0, dy - 1.0, dw + 2.0, dh + 2.0, border);
        fill(
            dx,
            dy,
            dw,
            dh,
            Rgba {
                r: 8,
                g: 8,
                b: 8,
                a: 255,
            },
        );
        let text = |out: &mut Vec<UiQuad>, s: &str, r: Rect, color: Rgba, centered: bool| {
            skinui::text_quads(
                font,
                &self.label_font_page,
                &skinui::UiText {
                    text: s.into(),
                    rect: r,
                    color,
                    center_horizontally: centered,
                    font: None,
                    adapter: None,
                },
                out,
            );
        };
        if let Some(gold) = self.gold_font.as_ref() {
            append_text(
                gold,
                &self.gold_font_page,
                "Options Menu",
                map_pregame_rect(dx, dy + 6.0, dw, 16.0, viewport),
                gold_color(),
                true,
                out,
            );
        }

        for r in opt::OPTIONS_ROWS {
            let (lx, ly, lw, lh) = opt::row_rect(r);
            let label_rect = map_pregame_rect(lx, ly, lw, lh, viewport);
            let hovered = r.id.is_some() && self.options_hover == r.id;
            let color = match r.kind {
                opt::RowKind::Section => gold_color(),
                _ if !r.enabled => pregame_disabled(),
                _ if hovered => skinui::WHITE,
                _ => pregame_gray(),
            };
            match r.kind {
                opt::RowKind::Section | opt::RowKind::Static => {
                    let body = if r.label.contains("%s") {
                        r.label.replace("%s", &self.options.video_card)
                    } else {
                        r.label.to_string()
                    };
                    text(out, &body, label_rect, color, false);
                }
                opt::RowKind::Link => text(out, r.label, label_rect, color, false),
                opt::RowKind::Check | opt::RowKind::Radio => {
                    let set = r.id.is_some_and(|id| self.options.radio_set(id));
                    self.append_template_button(
                        out,
                        "generic_check",
                        (lx, ly),
                        self.button_state(
                            PreWorldAction::CharSelectOptions,
                            set,
                            r.enabled && !hovered,
                        ),
                        viewport,
                    );
                    // `generic_check`'s own `TextOffset` puts the label 16px right of the box.
                    let off = map_pregame_rect(lx + 16.0, ly, lw - 16.0, lh, viewport);
                    text(out, r.label, off, color, false);
                }
                opt::RowKind::Cycle => {
                    text(out, r.label, label_rect, color, false);
                    let state = if r.enabled {
                        caer_assets::uiskin::ButtonState::Normal
                    } else {
                        caer_assets::uiskin::ButtonState::Disabled
                    };
                    for (i, a) in opt::arrow_rects(r).into_iter().enumerate() {
                        let template = if i == 0 {
                            "button_left"
                        } else {
                            "button_right"
                        };
                        self.append_template_button(out, template, (a.0, a.1), state, viewport);
                    }
                    if let Some(value) = r.id.and_then(|id| self.options.value_text(id)) {
                        let (lx2, rx, _) = opt::CYCLE_X[r.col.index()];
                        let vx = lx2 + opt::ARROW_SIZE;
                        text(
                            out,
                            &value,
                            map_pregame_rect(vx, ly, rx - vx, lh, viewport),
                            color,
                            true,
                        );
                    }
                }
            }
        }

        // Description pane: the client's own help text for whatever the pointer is over.
        let (px, py, pw, ph) = opt::description_rect();
        if let Some(help) = self
            .options_hover
            .and_then(opt::row_for)
            .map(|r| r.help)
            .filter(|h| !h.is_empty())
        {
            skinui::text_block_quads(
                font,
                &self.label_font_page,
                help,
                map_pregame_rect(px, py, pw, ph, viewport),
                pregame_gray(),
                out,
            );
        }

        for (accept, label) in [(false, "Cancel"), (true, "Accept")] {
            let (bx, by, _, _) = opt::button_rect(accept);
            let hovered = false;
            self.append_template_button(
                out,
                "button_large",
                (bx, by),
                if hovered {
                    caer_assets::uiskin::ButtonState::Highlit
                } else {
                    caer_assets::uiskin::ButtonState::Normal
                },
                viewport,
            );
            let (bw, bh) = opt::BUTTON_SIZE;
            text(
                out,
                label,
                map_pregame_rect(bx, by, bw, bh, viewport),
                pregame_gray(),
                true,
            );
        }
    }

    fn append_version_label(&self, out: &mut Vec<UiQuad>, viewport: (f32, f32)) {
        let Some(font) = self.label_font.as_ref() else {
            return;
        };
        let (vw, _vh) = viewport;
        let t = skinui::UiText {
            text: self.version_label.clone(),
            rect: Rect::new(12.0, 8.0, vw * 0.5, 20.0),
            color: skinui::WHITE,
            center_horizontally: false,
            font: None,
            adapter: None,
        };
        skinui::text_quads(font, &self.label_font_page, &t, out);
    }

    fn append_charselect_labels(&self, out: &mut Vec<UiQuad>, viewport: (f32, f32)) {
        let Some(font) = self.label_font.as_ref() else {
            return;
        };
        // Play / Create, ControlId 1090. The bottom row is drawn from CHARSELECT_CHROME; this
        // used to re-draw all six here from crops hardcoded against `buttons_wide.tga`, which is
        // the same page `breadcrumbs` resolves to — so every round button was being drawn twice.
        let play_action = self.charselect_play_action();
        self.append_template_button(
            out,
            "play",
            (854.0, 575.0),
            self.button_state(play_action, false, true),
            viewport,
        );

        // The ten character slots, `character_selection.xml`: a `button_large_radio` at
        // (780, 125 + 45i) with two 208x16 text lines beside it — the name (1050+i, y+... -10) and
        // the level/class line (1070+i, +10). CAER drew one merged line and no radio at all, so a
        // selected character looked identical to an unselected one and the rows had no affordance.
        for slot in 0..CHARSELECT_SLOTS {
            let row_y = 125.0 + (slot as f32) * CHARSELECT_ROW_H;
            let occupant = self
                .overview
                .as_ref()
                .and_then(|ov| ov.characters.iter().find(|c| c.slot == slot));
            let selected = self.selected_protocol_slot == Some(slot);
            let state = if selected {
                caer_assets::uiskin::ButtonState::Pressed
            } else if self.hovered_slot == Some(slot) {
                caer_assets::uiskin::ButtonState::Highlit
            } else {
                caer_assets::uiskin::ButtonState::Normal
            };
            self.append_template_button(out, "button_large_radio", (780.0, row_y), state, viewport);
            let Some(c) = occupant else {
                continue;
            };
            let (title, sub) = character_row_lines(c);
            for (text, y) in [(title, row_y - 10.0), (sub, row_y + 10.0)] {
                let t = skinui::UiText {
                    text,
                    rect: map_pregame_rect(815.0, y, 208.0, 16.0, viewport),
                    color: skinui::WHITE,
                    center_horizontally: false,
                    font: None,
                    adapter: None,
                };
                skinui::text_quads(font, &self.label_font_page, &t, out);
            }
        }
        if let Some(gold) = self.gold_font.as_ref() {
            append_text(
                gold,
                &self.gold_font_page,
                if self.selection_is_occupied() {
                    "Play"
                } else {
                    "Create"
                },
                map_pregame_rect(810.0, 625.0, 128.0, 32.0, viewport),
                gold_color(),
                true,
                out,
            );
            self.append_plate_caption(PreWorldScreen::CharSelect, viewport, out);
        }
        if let Some(med_gold) = self.med_gold_font.as_ref() {
            append_text(
                med_gold,
                &self.med_gold_font_page,
                "Select Character",
                map_pregame_rect(755.0, 83.0, 256.0, 16.0, viewport),
                gold_color(),
                true,
                out,
            );
        }
        // H1: a visible *enabled* control must have a complete path — art, hit route, dispatch,
        // response handling, terminal outcome. Customize has no action path at all and Delete has
        // no protocol dispatch, so both render visibly disabled instead of inviting a click that
        // does nothing (Customize) or produces only a refusal line (Delete). `enabled` here is the
        // same flag `hit_action` consults, so what is drawn and what responds cannot diverge.
        for c in &CHARSELECT_CHROME {
            let (label, enabled) = (c.label, c.enabled);
            let Some(authored) = preworld_hitbox::control_for(PreWorldScreen::CharSelect, c.target)
            else {
                continue;
            };
            let action = match label {
                "Realm" => Some(PreWorldAction::BackToRealm),
                "Quit" => Some(PreWorldAction::CharSelectQuit),
                "Options" => Some(PreWorldAction::CharSelectOptions),
                "Delete" => Some(PreWorldAction::DeleteCharacter),
                _ => None,
            };
            let state = match action {
                Some(a) => self.button_state(a, false, enabled),
                None => caer_assets::uiskin::ButtonState::Disabled,
            };
            let art = authored.art();
            self.append_template_button(out, c.template, (art.x, art.y), state, viewport);
            let cap = authored.caption();
            let r = map_pregame_rect(cap.x, cap.y, cap.w, cap.h, viewport);
            let t = skinui::UiText {
                text: label.into(),
                rect: r,
                color: if enabled {
                    pregame_gray()
                } else {
                    pregame_disabled()
                },
                center_horizontally: true,
                font: None,
                adapter: None,
            };
            skinui::text_quads(font, &self.label_font_page, &t, out);
        }
    }

    fn append_realm_controls(&self, out: &mut Vec<UiQuad>, viewport: (f32, f32)) {
        for btn in RealmButton::ALL {
            if self.textures.contains_key(btn.member()) {
                out.push(UiQuad {
                    dst: btn.authored_rect(viewport),
                    src: if self.hovered_realm == Some(btn) {
                        btn.highlight_src()
                    } else {
                        btn.normal_src()
                    },
                    texture: btn.member().into(),
                    color: skinui::WHITE,
                });
            }
        }
        if self.textures.contains_key("buttons_wide.tga") {
            out.push(UiQuad {
                dst: map_pregame_rect(74.0, 706.0, 38.0, 52.0, viewport),
                src: Rect::new(109.0, 196.0, 38.0, 52.0),
                texture: "buttons_wide.tga".into(),
                color: skinui::WHITE,
            });
        }
        if let Some(gold) = self.gold_font.as_ref() {
            for (label, x) in [("Albion", 39.0), ("Hibernia", 382.0), ("Midgard", 734.0)] {
                append_text(
                    gold,
                    &self.gold_font_page,
                    label,
                    map_pregame_rect(x, 350.0, 256.0, 32.0, viewport),
                    pregame_gray(),
                    true,
                    out,
                );
            }
            self.append_plate_caption(PreWorldScreen::RealmSelect, viewport, out);
        }
        if let Some(font) = self.label_font.as_ref() {
            for (text, x) in self.realm_descriptions.iter().zip([20.0, 365.0, 705.0]) {
                append_wrapped_text(
                    font,
                    &self.label_font_page,
                    text,
                    map_pregame_rect(x + 10.0, 405.0, 280.0, 290.0, viewport),
                    pregame_gray(),
                    out,
                );
            }
            append_text(
                font,
                &self.label_font_page,
                "Quit",
                map_pregame_rect(62.0, 752.0, 67.0, 16.0, viewport),
                pregame_gray(),
                true,
                out,
            );
        }
    }

    /// `character_customize_stats.xml`, drawn from its own form.
    ///
    /// A 440x260 dialog at the retail constructor's upper-left design-space location (see
    /// [`preworld_hitbox::stats_dialog_origin`]) over the still-visible customization form. The
    /// form ships no separate full-screen plate, which is why `CharStats` reuses the customizer's
    /// canvas beneath it. Its `dlg_background_noresize` frame comes directly from the client's
    /// `pregame/styles.xml` and `dialog_box.tga` through the generic nine-slice path.
    ///
    /// Values and the points counter read [`Self::create_draft`], the same draft
    /// [`PreWorldAction::StatsAdjust`] and [`PreWorldAction::StatsOptimize`] mutate, so what the
    /// row shows and what a click does cannot disagree. The full customization form remains
    /// underneath, exactly as the retail modal composes it.
    fn append_stats_dialog(&self, out: &mut Vec<UiQuad>, viewport: (f32, f32)) {
        use caer_protocol::starting_stats;
        let Some(font) = self.label_font.as_ref() else {
            return;
        };
        let (ox, oy) = preworld_hitbox::stats_dialog_origin();
        let (dw, dh) = preworld_hitbox::STATS_DIALOG_SIZE;
        let fill = |out: &mut Vec<UiQuad>, x: f32, y: f32, w: f32, h: f32, color: Rgba| {
            out.push(UiQuad {
                dst: map_pregame_rect(x, y, w, h, viewport),
                src: Rect::new(0.0, 0.0, 1.0, 1.0),
                texture: WHITE_PAGE.into(),
                color,
            });
        };
        if !self.append_nine_slice(
            out,
            "dlg_background_noresize",
            Rect::new(ox, oy, dw, dh),
            viewport,
        ) {
            // Damaged/non-retail trees remain usable, but this is explicitly a fallback: normal
            // retail execution draws the parsed source frame above.
            fill(
                out,
                ox - 1.0,
                oy - 1.0,
                dw + 2.0,
                dh + 2.0,
                Rgba {
                    r: 118,
                    g: 96,
                    b: 48,
                    a: 255,
                },
            );
            fill(
                out,
                ox,
                oy,
                dw,
                dh,
                Rgba {
                    r: 8,
                    g: 8,
                    b: 8,
                    a: 255,
                },
            );
        }
        // The form's own text, verbatim from its LabelDefs. rgb(225,225,225) is every label's
        // authored Color.
        let text =
            |out: &mut Vec<UiQuad>, s: &str, x: f32, y: f32, w: f32, h: f32, center: bool| {
                skinui::text_quads(
                    font,
                    &self.label_font_page,
                    &skinui::UiText {
                        text: s.into(),
                        rect: map_pregame_rect(x, y, w, h, viewport),
                        color: Rgba {
                            r: 225,
                            g: 225,
                            b: 225,
                            a: 255,
                        },
                        center_horizontally: center,
                        font: None,
                        adapter: None,
                    },
                    out,
                );
            };
        text(out, "Attributes", ox + 272.0, oy + 10.0, 128.0, 16.0, true);
        text(out, "Class", ox + 20.0, oy + 15.0, 128.0, 16.0, false);
        text(
            out,
            "Points Remaining",
            ox + 298.0,
            oy + 198.0,
            120.0,
            16.0,
            false,
        );
        // The description textarea (1076, 10,31 236x146): the selected class's own description,
        // the same source the create form's class pane reads.
        fill(
            out,
            ox + 10.0,
            oy + 31.0,
            236.0,
            146.0,
            Rgba {
                r: 20,
                g: 20,
                b: 20,
                a: 255,
            },
        );
        if let Some(body) = self.descriptions.class(self.create_draft.class_id) {
            skinui::text_block_quads(
                font,
                &self.label_font_page,
                body,
                map_pregame_rect(ox + 14.0, oy + 35.0, 228.0, 138.0, viewport),
                pregame_gray(),
                out,
            );
        }
        // Eight stat rows, 20px pitch: minus, value, plus, name — the authored x order.
        let remaining = starting_stats::MAX_STARTING_BONUS_POINTS
            - starting_stats::total_spent(self.create_draft.race, &self.create_draft.stats);
        for (i, name) in starting_stats::STAT_NAMES.iter().enumerate() {
            let i = i as u8;
            let y = 34.0 + f32::from(i) * 20.0;
            text(
                out,
                &self.create_draft.stats[usize::from(i)].to_string(),
                ox + 273.0,
                oy + y,
                20.0,
                16.0,
                true,
            );
            text(out, name, ox + 318.0, oy + y, 120.0, 16.0, false);
            // PROVENANCE GAP: retail's stat_value_N_color ColorAdapter recolours a raised value;
            // the exact colour is not recoverable from the form, so raised rows keep the base
            // colour rather than an invented highlight.
            let _ = starting_stats::race_base(self.create_draft.race, usize::from(i));
        }
        text(
            out,
            &remaining.to_string(),
            ox + 273.0,
            oy + 198.0,
            20.0,
            16.0,
            true,
        );
        // The buttons, from the same table the click routes through.
        for c in preworld_hitbox::controls(PreWorldScreen::CharStats) {
            let art = c.art();
            let action = stats_dialog_action(c.target);
            let hovered = self.hovered_action == action;
            let state = if hovered {
                caer_assets::uiskin::ButtonState::Highlit
            } else {
                caer_assets::uiskin::ButtonState::Normal
            };
            let template = match c.target {
                preworld_hitbox::Target::StatsAdjust { dir: 1, .. } => "button_pregame_mini_right",
                preworld_hitbox::Target::StatsAdjust { .. } => "button_pregame_mini_left",
                _ => "button_small",
            };
            self.append_template_button(out, template, (art.x, art.y), state, viewport);
            // Only the two worded buttons carry captions; the 16 mini arrows are pure art.
            let word = match c.target {
                preworld_hitbox::Target::StatsReset => Some("Reset"),
                preworld_hitbox::Target::StatsOptimize => Some("Optimize"),
                _ => None,
            };
            if let Some(word) = word {
                skinui::text_quads(
                    font,
                    &self.label_font_page,
                    &skinui::UiText {
                        text: word.into(),
                        rect: map_pregame_rect(art.x, art.y, art.w, art.h, viewport),
                        color: if hovered {
                            skinui::WHITE
                        } else {
                            pregame_gray()
                        },
                        center_horizontally: true,
                        font: None,
                        adapter: None,
                    },
                    out,
                );
            }
        }
    }

    /// Draw the observed retail character-customizer runtime profile.
    ///
    /// The loose form still documents shared chrome, fonts, and asset templates, but the retail
    /// runtime used by the reference captures is a distinct composition: textual Eye/Hair
    /// selectors, sliders for Mood and Skin Tone, and a Tattoo row that disappears entirely for
    /// identities without decal choices.  Its row table lives in `preworld_customize`; this
    /// renderer intentionally consumes that same table as the active hit-test so art and action
    /// cannot diverge again.
    fn append_charcustomize_controls(&self, out: &mut Vec<UiQuad>, viewport: (f32, f32)) {
        let Some(font) = self.label_font.as_ref() else {
            return;
        };
        let label = |out: &mut Vec<UiQuad>, text: &str, x: f32, y: f32, w: f32, color: Rgba| {
            append_text(
                font,
                &self.label_font_page,
                text,
                map_pregame_rect(x, y, w, 16.0, viewport),
                color,
                false,
                out,
            );
        };
        let center_label = |out: &mut Vec<UiQuad>, text: &str, x: f32, y: f32, w: f32| {
            append_text(
                font,
                &self.label_font_page,
                text,
                map_pregame_rect(x, y, w, 16.0, viewport),
                skinui::WHITE,
                true,
                out,
            );
        };
        let custom_label = Rgba {
            r: 206,
            g: 169,
            b: 90,
            a: 255,
        };
        let has_tattoo = self.customizer_has_tattoo();
        let rows = preworld_customize::runtime_rows(has_tattoo);
        let controls = preworld_hitbox::customizer_controls(has_tattoo);
        let control_for = |target| controls.iter().find(|control| control.target == target);
        let source_choices = self
            .appearance_catalog
            .as_ref()
            .and_then(|catalog| catalog.choices(self.create_draft.race, self.create_draft.gender));

        // The detailed form is composed with the basic customization canvas in retail.  These
        // labels are not part of the bitmap plate, so keeping them here ensures a source page
        // cannot erase the runtime-profile affordances.
        if let Some(gold) = self.gold_font.as_ref() {
            append_text(
                gold,
                &self.gold_font_page,
                "Customize  Your  Character",
                map_pregame_rect(260.0, 662.0, 500.0, 16.0, viewport),
                gold_color(),
                true,
                out,
            );
        }
        if let Some(med_gold) = self.med_gold_font.as_ref() {
            append_text(
                med_gold,
                &self.med_gold_font_page,
                "Facial Features",
                map_pregame_rect(755.0, 83.0, 256.0, 16.0, viewport),
                skinui::WHITE,
                true,
                out,
            );
        }
        for (text, y) in [
            ("Camera Controls", 605.0),
            ("Left click and drag to rotate", 620.0),
            ("Right click and drag to zoom", 635.0),
        ] {
            center_label(out, text, 763.0, y, 250.0);
        }

        let source_selector_label = |selector: crate::preworld_appearance::AppearanceSelector,
                                     index: u8| {
            source_choices
                .and_then(|choices| {
                    choices.selector_label(selector, index).or_else(|| {
                        // The untouched all-zero wire value is shown as the first authored
                        // UI item rather than as a fictitious "Default" option.
                        (index == 0).then(|| {
                            choices
                                .selector_values(selector)
                                .first()
                                .and_then(|choice| choice.label.as_deref())
                        })?
                    })
                })
                .map_or_else(
                    || {
                        if index == 0 {
                            "Default".to_string()
                        } else {
                            index.to_string()
                        }
                    },
                    str::to_string,
                )
        };
        let roman = |ordinal: usize| match ordinal {
            1 => "I".to_string(),
            2 => "II".to_string(),
            3 => "III".to_string(),
            4 => "IV".to_string(),
            5 => "V".to_string(),
            6 => "VI".to_string(),
            7 => "VII".to_string(),
            8 => "VIII".to_string(),
            9 => "IX".to_string(),
            10 => "X".to_string(),
            11 => "XI".to_string(),
            12 => "XII".to_string(),
            13 => "XIII".to_string(),
            14 => "XIV".to_string(),
            15 => "XV".to_string(),
            16 => "XVI".to_string(),
            other => other.to_string(),
        };
        let ordinal_source_label = |prefix: &str, values: &[u8], current: u8| {
            let ordinal = values
                .iter()
                .position(|value| *value == current)
                .map_or(1, |index| index + 1);
            format!("{prefix} {}", roman(ordinal))
        };
        let source_tick = |values: &[u8], current: u8| {
            if current == 0 {
                return 0;
            }
            values
                .iter()
                .position(|value| *value == current)
                .map_or(0, |index| {
                    (index + 1).min(usize::from(preworld_customize::SLIDER_MAX_TICK))
                }) as u8
        };
        let row_value = |field: CustomizerField| -> String {
            match field {
                CustomizerField::Face => source_selector_label(
                    crate::preworld_appearance::AppearanceSelector::Face,
                    self.create_draft.face_type,
                ),
                CustomizerField::EyeColor => {
                    let values = source_choices
                        .map(crate::preworld_appearance::AppearanceChoices::eye_colours)
                        .unwrap_or_default();
                    ordinal_source_label("Eye Color", values, self.create_draft.eye_color >> 4)
                }
                CustomizerField::HairStyle => source_selector_label(
                    crate::preworld_appearance::AppearanceSelector::HairStyle,
                    self.create_draft.hair_style,
                ),
                CustomizerField::HairColor => {
                    let values = source_choices
                        .and_then(|choices| choices.palette_values(2, self.create_draft.hair_style))
                        .unwrap_or_default();
                    ordinal_source_label("Hair Color", values, self.create_draft.hair_color)
                }
                CustomizerField::Tattoo => source_selector_label(
                    crate::preworld_appearance::AppearanceSelector::Tattoo,
                    self.customizer.tattoo_index(),
                ),
                CustomizerField::Size => source_choices
                    .and_then(|choices| choices.scale_label(self.create_draft.creation_size()))
                    .unwrap_or("Average")
                    .to_string(),
                CustomizerField::Morph(_) | CustomizerField::Mood | CustomizerField::SkinTone => {
                    String::new()
                }
            }
        };
        let selector_enabled = |field: CustomizerField| match field {
            CustomizerField::Face => source_choices.map_or(true, |choices| {
                choices.has_selector(crate::preworld_appearance::AppearanceSelector::Face)
            }),
            CustomizerField::EyeColor => {
                source_choices.map_or(true, |choices| !choices.eye_colours().is_empty())
            }
            CustomizerField::HairStyle => source_choices.map_or(true, |choices| {
                choices.has_selector(crate::preworld_appearance::AppearanceSelector::HairStyle)
            }),
            CustomizerField::HairColor => source_choices.map_or(true, |choices| {
                choices
                    .palette_values(2, self.create_draft.hair_style)
                    .is_some_and(|values| !values.is_empty())
            }),
            CustomizerField::Tattoo => has_tattoo,
            CustomizerField::Size => {
                source_choices.map_or(true, |choices| !choices.scale().is_empty())
            }
            CustomizerField::Morph(_) | CustomizerField::Mood | CustomizerField::SkinTone => false,
        };
        let slider_tick = |field: CustomizerField| match field {
            CustomizerField::Morph(slot) => {
                caer_protocol::customization::FacialMorphSlot::from_source_slot(slot)
                    .map_or(0, |morph| self.create_draft.facial_morph_tick(morph))
            }
            CustomizerField::Mood => self
                .create_draft
                .mood_type
                .min(preworld_customize::SLIDER_MAX_TICK),
            CustomizerField::SkinTone => {
                let values = source_choices
                    .map(crate::preworld_appearance::AppearanceChoices::skin_tones)
                    .unwrap_or_default();
                source_tick(values, self.create_draft.eye_color & 0x0F)
            }
            _ => 0,
        };
        let append_slider =
            |out: &mut Vec<UiQuad>, art: preworld_hitbox::ArtRect, tick: u8, max_tick: u8| {
                if !self.textures.contains_key("slider.tga") {
                    return;
                }
                let texture = "slider.tga".to_string();
                out.push(UiQuad {
                    dst: map_pregame_rect(art.x, art.y, 16.0, 16.0, viewport),
                    src: Rect::new(16.0, 0.0, 16.0, 16.0),
                    texture: texture.clone(),
                    color: skinui::WHITE,
                });
                let middle_x = art.x + 16.0;
                let middle_w = (art.w - 32.0).max(0.0);
                let mut drawn = 0.0;
                while drawn < middle_w {
                    let width = (middle_w - drawn).min(5.0);
                    out.push(UiQuad {
                        dst: map_pregame_rect(middle_x + drawn, art.y + 1.0, width, 14.0, viewport),
                        src: Rect::new(7.0, 1.0, width, 14.0),
                        texture: texture.clone(),
                        color: skinui::WHITE,
                    });
                    drawn += width;
                }
                out.push(UiQuad {
                    dst: map_pregame_rect(art.x + art.w - 16.0, art.y, 16.0, 16.0, viewport),
                    src: Rect::new(16.0, 16.0, 16.0, 16.0),
                    texture: texture.clone(),
                    color: skinui::WHITE,
                });
                let travel = (art.w - 7.0).max(1.0);
                let max_tick = f32::from(max_tick.max(1));
                let indicator_x = art.x + f32::from(tick.min(max_tick as u8)) / max_tick * travel;
                out.push(UiQuad {
                    dst: map_pregame_rect(indicator_x, art.y - 1.0, 7.0, 18.0, viewport),
                    src: Rect::new(0.0, 0.0, 7.0, 18.0),
                    texture,
                    color: skinui::WHITE,
                });
            };

        for row in &rows {
            let label_text = match row.field {
                CustomizerField::Morph(slot) => source_choices
                    .and_then(|choices| choices.morph_label(slot))
                    .map_or_else(|| format!("Morph {}", slot + 1), str::to_string),
                field => field.label().unwrap_or_default().to_string(),
            };
            label(
                out,
                &label_text,
                preworld_customize::LABEL_X,
                row.y_f32() + 2.0,
                60.0,
                custom_label,
            );
            match row.widget {
                CustomizerWidget::TextSelector => {
                    let value = row_value(row.field);
                    center_label(
                        out,
                        &value,
                        preworld_customize::VALUE_X,
                        row.y_f32(),
                        preworld_customize::VALUE_WIDTH,
                    );
                    for (dir, template) in [(-1, "button_left"), (1, "button_right")] {
                        let target = Target::CustomizeAdjust {
                            field: row.field,
                            dir,
                        };
                        let Some(control) = control_for(target) else {
                            continue;
                        };
                        let art = control.art();
                        self.append_template_button(
                            out,
                            template,
                            (art.x, art.y),
                            self.button_state(
                                PreWorldAction::CustomizeAdjust {
                                    field: row.field,
                                    dir,
                                },
                                false,
                                selector_enabled(row.field),
                            ),
                            viewport,
                        );
                    }
                }
                CustomizerWidget::Slider { max_tick } => {
                    let Some(control) = control_for(Target::CustomizeSlider { field: row.field })
                    else {
                        continue;
                    };
                    append_slider(out, control.art(), slider_tick(row.field), max_tick);
                }
            }
            let Some(control) = control_for(Target::CustomizeLock { field: row.field }) else {
                continue;
            };
            let art = control.art();
            self.append_template_button(
                out,
                "lock",
                (art.x, art.y),
                self.button_state(
                    PreWorldAction::CustomizeToggleLock { field: row.field },
                    self.customizer.is_locked(row.field),
                    true,
                ),
                viewport,
            );
        }

        // The camera controls keep the source geometry and are still interactive on the observed
        // runtime profile.  They deliberately share the same dynamic table lookup as all rows.
        for (target, action, template) in [
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::Reset),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::Reset),
                "camera_reset",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::RotateLeft),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::RotateLeft),
                "camera_left",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::RotateRight),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::RotateRight),
                "camera_right",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::TiltUp),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::TiltUp),
                "camera_up",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::TiltDown),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::TiltDown),
                "camera_down",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::ZoomIn),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::ZoomIn),
                "camera_zoom_in",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::ZoomOut),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::ZoomOut),
                "camera_zoom_out",
            ),
        ] {
            let Some(control) = control_for(target) else {
                continue;
            };
            let art = control.art();
            self.append_template_button(
                out,
                template,
                (art.x, art.y),
                self.button_state(action, false, true),
                viewport,
            );
        }

        let is_default = self.create_draft.eye_size == 0
            && self.create_draft.lip_size == 0
            && self.create_draft.eye_color == 0
            && self.create_draft.hair_color == 0
            && self.create_draft.face_type == 0
            && self.create_draft.hair_style == 0
            && self.create_draft.mood_type == 0
            && self.customizer.tattoo_index() == 0
            && self.create_draft.creation_size()
                == caer_protocol::charcreate::CREATION_SIZE_AVERAGE;
        for (target, action, text) in [
            (
                Target::CustomizeReset,
                PreWorldAction::CustomizeReset,
                "Default",
            ),
            (
                Target::CustomizeRandom,
                PreWorldAction::CustomizeRandom,
                "Random",
            ),
        ] {
            let Some(control) = control_for(target) else {
                continue;
            };
            let art = control.art();
            self.append_template_button(
                out,
                "button_pregame_medium",
                (art.x, art.y),
                self.button_state(
                    action,
                    action == PreWorldAction::CustomizeReset && is_default,
                    true,
                ),
                viewport,
            );
            center_label(out, text, art.x, art.y + 2.0, art.w);
        }

        for (template, target, action, text) in [
            (
                "quit",
                Target::CustomizeCancel,
                PreWorldAction::CustomizeCancel,
                "Cancel",
            ),
            (
                "realm",
                Target::CustomizeRealm,
                PreWorldAction::BackToRealm,
                "Realm",
            ),
            (
                "race_class",
                Target::CustomizeBack,
                PreWorldAction::CustomizeBack,
                "Race/Class",
            ),
            (
                "assign_stats",
                Target::CustomizeStats,
                PreWorldAction::CustomizeStats,
                "Adjust Attributes",
            ),
            (
                "location",
                Target::CustomizeAdvance,
                PreWorldAction::CustomizeAdvance,
                "Continue",
            ),
        ] {
            let Some(control) = control_for(target) else {
                continue;
            };
            let art = control.art();
            self.append_template_button(
                out,
                template,
                (art.x, art.y),
                self.button_state(action, false, true),
                viewport,
            );
            let caption = control.caption();
            center_label(out, text, caption.x, caption.y, caption.w);
        }
    }

    /// Historical palette-form renderer retained only as source archaeology while its static
    /// XML-transcription test is replaced. The live render path above never calls it.
    #[cfg(any())]
    #[allow(dead_code)]
    fn append_charcustomize_controls_legacy(&self, out: &mut Vec<UiQuad>, viewport: (f32, f32)) {
        // The form's legacy arrow aliases are absent from retail styles.xml, so the matching
        // authored slider controls stand in for their exact art. This is intentionally disabled:
        // source archaeology only, never a live palette renderer.
        let Some(font) = self.label_font.as_ref() else {
            return;
        };
        let label = |out: &mut Vec<UiQuad>, text: &str, x: f32, y: f32, w: f32, color: Rgba| {
            append_text(
                font,
                &self.label_font_page,
                text,
                map_pregame_rect(x, y, w, 16.0, viewport),
                color,
                false,
                out,
            );
        };
        let center_label = |out: &mut Vec<UiQuad>, text: &str, x: f32, y: f32, w: f32| {
            append_text(
                font,
                &self.label_font_page,
                text,
                map_pregame_rect(x, y, w, 16.0, viewport),
                skinui::WHITE,
                true,
                out,
            );
        };
        let custom_label = Rgba {
            r: 206,
            g: 169,
            b: 90,
            a: 255,
        };
        // The common XML owns the *positions* of every optional row. Its fig3 adapters decide
        // whether a given identity actually has choices. Do that same decision before painting or
        // hit-filtering chrome: Briton has no tattoo decals, while Celt/Shar/Troll do.
        let source_choices = self
            .appearance_catalog
            .as_ref()
            .and_then(|catalog| catalog.choices(self.create_draft.race, self.create_draft.gender));
        let has_selector =
            |selector| source_choices.map_or(true, |choices| choices.has_selector(selector));
        let has_palette = |palette| {
            source_choices.map_or(true, |choices| {
                choices
                    .palette_values(palette, self.create_draft.hair_style)
                    .is_some_and(|values| !values.is_empty())
            })
        };
        let show_face = has_selector(crate::preworld_appearance::AppearanceSelector::Face);
        let show_hair_style =
            has_selector(crate::preworld_appearance::AppearanceSelector::HairStyle);
        let show_tattoo = has_selector(crate::preworld_appearance::AppearanceSelector::Tattoo);
        let show_skin = has_palette(0);
        let show_eye = has_palette(1);
        let show_hair_colour = has_palette(2);
        let show_scale = source_choices.map_or(true, |choices| !choices.scale().is_empty());

        // The detailed field form is composed with `character_customize_basic.xml` in the
        // retail client.  Its headings and camera help are separate LabelDefs, not pixels baked
        // into `character_customize.tga`; omitting them made the controls look like an unfinished
        // debug overlay even when every individual selector was wired.
        if let Some(gold) = self.gold_font.as_ref() {
            append_text(
                gold,
                &self.gold_font_page,
                "Customize  Your  Character",
                map_pregame_rect(260.0, 662.0, 500.0, 16.0, viewport),
                gold_color(),
                true,
                out,
            );
        }
        if let Some(med_gold) = self.med_gold_font.as_ref() {
            append_text(
                med_gold,
                &self.med_gold_font_page,
                "Facial Features",
                map_pregame_rect(755.0, 83.0, 256.0, 16.0, viewport),
                skinui::WHITE,
                true,
                out,
            );
        }
        for (text, y) in [
            ("Camera Controls", 605.0),
            ("Left click and drag to rotate", 620.0),
            ("Right click and drag to zoom", 635.0),
        ] {
            center_label(out, text, 763.0, y, 250.0);
        }

        // The labels are the form's own `arial9` LabelDefs. Keep the text where the XML puts it;
        // dynamic adapter values come from fig3descriptions through `appearance_catalog`, not a
        // guessed numeric range.
        for (visible, text, y) in [
            (show_face, "Face", 125.0),
            (show_skin, "Skin Color", 325.0),
            (show_eye, "Eye Color", 365.0),
            (show_hair_style, "Hair Style", 405.0),
            (show_hair_colour, "Hair Color", 445.0),
            (show_tattoo, "Tattoo", 485.0),
            (show_scale, "Size", 525.0),
        ] {
            if visible {
                label(out, text, 763.0, y, 60.0, custom_label);
            }
        }

        // `fig3descriptions.csv` owns these words.  The XML supplies four anonymous
        // `morph_N_text` adapters, while the catalogue supplies the race-specific labels:
        // Briton says Lips/Jaw, Firbolg says Jaw Length/Ears, and so on.  Never borrow the
        // previous race's label merely because the control rectangle is shared.
        for slot in 0..4u8 {
            let text = self
                .appearance_catalog
                .as_ref()
                .and_then(|catalog| {
                    catalog
                        .choices(self.create_draft.race, self.create_draft.gender)
                        .and_then(|choices| choices.morph_label(slot))
                })
                .map_or_else(|| format!("Morph {}", slot + 1), str::to_string);
            label(
                out,
                &text,
                763.0,
                165.0 + f32::from(slot) * 40.0,
                60.0,
                custom_label,
            );
        }

        // `generic_horizontal_slider` from styles.xml: 16px left cap, repeatable 5×14px middle,
        // 16px right cap, and a 7×18px indicator.  The form authors all four 155×16 tracks; the
        // source pointer occupies the 148px (155 − 7) travel span, so the visual thumb and the
        // tick sent to CharacterCreate use the exact same coordinate model.
        let append_morph_slider =
            |out: &mut Vec<UiQuad>, art: preworld_hitbox::ArtRect, tick: u8| {
                if !self.textures.contains_key("slider.tga") {
                    return;
                }
                let texture = "slider.tga".to_string();
                out.push(UiQuad {
                    dst: map_pregame_rect(art.x, art.y, 16.0, 16.0, viewport),
                    src: Rect::new(16.0, 0.0, 16.0, 16.0),
                    texture: texture.clone(),
                    color: skinui::WHITE,
                });
                let middle_x = art.x + 16.0;
                let middle_w = (art.w - 32.0).max(0.0);
                let mut drawn = 0.0;
                while drawn < middle_w {
                    let w = (middle_w - drawn).min(5.0);
                    out.push(UiQuad {
                        dst: map_pregame_rect(middle_x + drawn, art.y + 1.0, w, 14.0, viewport),
                        src: Rect::new(7.0, 1.0, w, 14.0),
                        texture: texture.clone(),
                        color: skinui::WHITE,
                    });
                    drawn += w;
                }
                out.push(UiQuad {
                    dst: map_pregame_rect(art.x + art.w - 16.0, art.y, 16.0, 16.0, viewport),
                    src: Rect::new(16.0, 16.0, 16.0, 16.0),
                    texture: texture.clone(),
                    color: skinui::WHITE,
                });
                let max_tick = f32::from(caer_protocol::customization::FACIAL_MORPH_MAX_TICK);
                let indicator_x = art.x
                    + f32::from(tick.min(caer_protocol::customization::FACIAL_MORPH_MAX_TICK))
                        / max_tick
                        * (art.w - 7.0);
                out.push(UiQuad {
                    dst: map_pregame_rect(indicator_x, art.y - 1.0, 7.0, 18.0, viewport),
                    src: Rect::new(0.0, 0.0, 7.0, 18.0),
                    texture,
                    color: skinui::WHITE,
                });
            };
        for slot in 0..4u8 {
            let Some(field) = caer_protocol::customization::FacialMorphSlot::from_source_slot(slot)
            else {
                continue;
            };
            let Some(control) = preworld_hitbox::control_for(
                PreWorldScreen::CharCustomize,
                Target::CustomizeMorph { slot },
            ) else {
                continue;
            };
            append_morph_slider(
                out,
                control.art(),
                self.create_draft.facial_morph_tick(field),
            );
        }

        // Camera chrome lives in the lower-left of the source form.  It had been omitted entirely,
        // which made the supplied retail close-ups look like a separate static camera.  Render it
        // through the same source template/page resolver as every other pre-world control, so the
        // icon, hover state and hit rectangle cannot drift apart.
        for (target, action, template) in [
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::Reset),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::Reset),
                "camera_reset",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::RotateLeft),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::RotateLeft),
                "camera_left",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::RotateRight),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::RotateRight),
                "camera_right",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::TiltUp),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::TiltUp),
                "camera_up",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::TiltDown),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::TiltDown),
                "camera_down",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::ZoomIn),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::ZoomIn),
                "camera_zoom_in",
            ),
            (
                Target::CustomizeCamera(crate::preworld_camera::CameraControl::ZoomOut),
                PreWorldAction::CustomizeCamera(crate::preworld_camera::CameraControl::ZoomOut),
                "camera_zoom_out",
            ),
        ] {
            let Some(control) = preworld_hitbox::control_for(PreWorldScreen::CharCustomize, target)
            else {
                continue;
            };
            let art = control.art();
            self.append_template_button(
                out,
                template,
                (art.x, art.y),
                self.button_state(action, false, true),
                viewport,
            );
        }

        let source_value = |selector: crate::preworld_appearance::AppearanceSelector, index: u8| {
            self.appearance_catalog
                .as_ref()
                .and_then(|catalog| {
                    catalog
                        .choices(self.create_draft.race, self.create_draft.gender)
                        .and_then(|choices| {
                            choices.selector_label(selector, index).or_else(|| {
                                // The create draft's all-zero wire default displays the first
                                // authored UI choice (Face 1, Hair 1, No Tattoo), not the word
                                // "Default". The zero remains on the wire until the player makes
                                // a selection, so rendering it as a source label must not invent
                                // a packet value.
                                (index == 0).then(|| {
                                    choices
                                        .selector_values(selector)
                                        .first()
                                        .and_then(|choice| choice.label.as_deref())
                                })?
                            })
                        })
                })
                .map_or_else(
                    || {
                        if index == 0 {
                            "Default".to_string()
                        } else {
                            index.to_string()
                        }
                    },
                    str::to_string,
                )
        };
        let face_text = source_value(
            crate::preworld_appearance::AppearanceSelector::Face,
            self.create_draft.face_type,
        );
        let hair_text = source_value(
            crate::preworld_appearance::AppearanceSelector::HairStyle,
            self.create_draft.hair_style,
        );
        let tattoo_text = source_value(
            crate::preworld_appearance::AppearanceSelector::Tattoo,
            self.create_draft.mood_type,
        );
        let scale_text = source_choices
            .and_then(|choices| choices.scale_label(self.create_draft.creation_size()))
            .unwrap_or("Average");
        for (visible, text, y) in [
            (show_face, face_text.as_str(), 123.0),
            (show_hair_style, hair_text.as_str(), 403.0),
            (show_tattoo, tattoo_text.as_str(), 483.0),
            (show_scale, scale_text, 523.0),
        ] {
            if visible {
                center_label(out, text, 837.0, y, 130.0);
            }
        }

        let can_cycle = |field: u8, dir: i8| {
            let (selector, current) = match field {
                4 => (
                    crate::preworld_appearance::AppearanceSelector::Face,
                    self.create_draft.face_type,
                ),
                5 => (
                    crate::preworld_appearance::AppearanceSelector::HairStyle,
                    self.create_draft.hair_style,
                ),
                6 => (
                    crate::preworld_appearance::AppearanceSelector::Tattoo,
                    self.create_draft.mood_type,
                ),
                _ => return false,
            };
            self.appearance_catalog.as_ref().map_or(true, |catalog| {
                catalog
                    .cycle(
                        self.create_draft.race,
                        self.create_draft.gender,
                        selector,
                        current,
                        dir,
                    )
                    .is_some_and(|next| next != current)
            })
        };
        for (target, action, template, field, dir) in [
            (
                Target::CustomizeCycle { field: 4, dir: -1 },
                PreWorldAction::CustomizeCycle { field: 4, dir: -1 },
                "button_left",
                4,
                -1,
            ),
            (
                Target::CustomizeCycle { field: 4, dir: 1 },
                PreWorldAction::CustomizeCycle { field: 4, dir: 1 },
                "button_right",
                4,
                1,
            ),
            (
                Target::CustomizeCycle { field: 5, dir: -1 },
                PreWorldAction::CustomizeCycle { field: 5, dir: -1 },
                "button_left",
                5,
                -1,
            ),
            (
                Target::CustomizeCycle { field: 5, dir: 1 },
                PreWorldAction::CustomizeCycle { field: 5, dir: 1 },
                "button_right",
                5,
                1,
            ),
            (
                Target::CustomizeCycle { field: 6, dir: -1 },
                PreWorldAction::CustomizeCycle { field: 6, dir: -1 },
                "button_left",
                6,
                -1,
            ),
            (
                Target::CustomizeCycle { field: 6, dir: 1 },
                PreWorldAction::CustomizeCycle { field: 6, dir: 1 },
                "button_right",
                6,
                1,
            ),
        ] {
            if (field == 4 && !show_face)
                || (field == 5 && !show_hair_style)
                || (field == 6 && !show_tattoo)
            {
                continue;
            }
            let Some(c) = preworld_hitbox::control_for(PreWorldScreen::CharCustomize, target)
            else {
                continue;
            };
            let art = c.art();
            self.append_template_button(
                out,
                template,
                (art.x, art.y),
                self.button_state(action, false, can_cycle(field, dir)),
                viewport,
            );
        }

        if show_scale {
            for (target, action, template) in [
                (
                    Target::CustomizeScale { dir: -1 },
                    PreWorldAction::CustomizeScale { dir: -1 },
                    "button_left",
                ),
                (
                    Target::CustomizeScale { dir: 1 },
                    PreWorldAction::CustomizeScale { dir: 1 },
                    "button_right",
                ),
            ] {
                let Some(c) = preworld_hitbox::control_for(PreWorldScreen::CharCustomize, target)
                else {
                    continue;
                };
                let art = c.art();
                self.append_template_button(
                    out,
                    template,
                    (art.x, art.y),
                    self.button_state(action, false, true),
                    viewport,
                );
            }
        }

        // `color_picker_8` and `color_picker_16` name a source crop on a source palette. Their
        // 1px border is part of the crop; the per-cell hit table starts after it, exactly as the
        // image picker template specifies. The indicator crop is its own tiny retail texture.
        let eye_index = (self.create_draft.eye_color >> 4).saturating_sub(1).min(7);
        let skin_index = (self.create_draft.eye_color & 0x0F)
            .saturating_sub(1)
            .min(7);
        let hair_index = self.create_draft.hair_color.saturating_sub(1).min(15);
        for (visible, texture, src, x, y, selected) in [
            (
                show_skin,
                "skin_color_palettes.tga",
                Rect::new(7.0, 2.0, 162.0, 22.0),
                826.0,
                320.0,
                skin_index,
            ),
            (
                show_eye,
                "eye_color_palettes.tga",
                Rect::new(7.0, 2.0, 162.0, 22.0),
                826.0,
                358.0,
                eye_index,
            ),
            (
                show_hair_colour,
                "hair_colors_palettes.tga",
                Rect::new(7.0, 2.0, 162.0, 42.0),
                826.0,
                430.0,
                hair_index,
            ),
        ] {
            if !visible {
                continue;
            }
            if self.textures.contains_key(texture) {
                out.push(UiQuad {
                    dst: map_pregame_rect(x, y, src.w, src.h, viewport),
                    src,
                    texture: texture.into(),
                    color: skinui::WHITE,
                });
            }
            if self.textures.contains_key("color_picker_indicator.tga") {
                let cell_x = x + 1.0 + f32::from(selected) * 20.0;
                let cell_y = y + 1.0 + if selected >= 8 { 20.0 } else { 0.0 };
                out.push(UiQuad {
                    dst: map_pregame_rect(cell_x, cell_y, 20.0, 20.0, viewport),
                    src: Rect::new(0.0, 0.0, 20.0, 20.0),
                    texture: "color_picker_indicator.tga".into(),
                    color: skinui::WHITE,
                });
            }
        }

        let is_default = self.create_draft.eye_size == 0
            && self.create_draft.lip_size == 0
            && self.create_draft.eye_color == 0
            && self.create_draft.hair_color == 0
            && self.create_draft.face_type == 0
            && self.create_draft.hair_style == 0
            && self.create_draft.mood_type == 0
            && self.create_draft.creation_size()
                == caer_protocol::charcreate::CREATION_SIZE_AVERAGE;
        let default_action = PreWorldAction::CustomizeReset;
        if let Some(c) =
            preworld_hitbox::control_for(PreWorldScreen::CharCustomize, Target::CustomizeReset)
        {
            let art = c.art();
            self.append_template_button(
                out,
                "button_pregame_medium",
                (art.x, art.y),
                self.button_state(default_action, is_default, true),
                viewport,
            );
            center_label(out, "Default", art.x, art.y + 2.0, art.w);
        }
        if let Some(c) =
            preworld_hitbox::control_for(PreWorldScreen::CharCustomize, Target::CustomizeRandom)
        {
            let art = c.art();
            self.append_template_button(
                out,
                "button_pregame_medium",
                (art.x, art.y),
                self.button_state(PreWorldAction::CustomizeRandom, false, true),
                viewport,
            );
            center_label(out, "Random", art.x, art.y + 2.0, art.w);
        }

        // Retail's `lock` template has distinct Normal/Pressed crops. It is not a decorative
        // checkbox: its selected state protects the adjacent source value from Random. Skip the
        // tattoo lock whenever the fig3 adapter removed that row, exactly as the client does.
        for slot in 0..11u8 {
            let visible = match slot {
                0 => show_face,
                5 => show_skin,
                6 => show_eye,
                7 => show_hair_style,
                8 => show_hair_colour,
                9 => show_tattoo,
                10 => show_scale,
                _ => true, // four source morph sliders
            };
            if !visible {
                continue;
            }
            let target = Target::CustomizeLock { slot };
            let Some(c) = preworld_hitbox::control_for(PreWorldScreen::CharCustomize, target)
            else {
                continue;
            };
            let art = c.art();
            let action = PreWorldAction::CustomizeToggleLock { slot };
            self.append_template_button(
                out,
                "lock",
                (art.x, art.y),
                self.button_state(action, self.customizer.is_locked(slot), true),
                viewport,
            );
        }

        for (template, target, action, text) in [
            (
                "quit",
                Target::CustomizeCancel,
                PreWorldAction::CustomizeCancel,
                "Cancel",
            ),
            (
                "realm",
                Target::CustomizeRealm,
                PreWorldAction::BackToRealm,
                "Realm",
            ),
            (
                "race_class",
                Target::CustomizeBack,
                PreWorldAction::CustomizeBack,
                "Race/Class",
            ),
            (
                "assign_stats",
                Target::CustomizeStats,
                PreWorldAction::CustomizeStats,
                "Adjust Attributes",
            ),
            (
                "location",
                Target::CustomizeAdvance,
                PreWorldAction::CustomizeAdvance,
                "Continue",
            ),
        ] {
            let Some(c) = preworld_hitbox::control_for(PreWorldScreen::CharCustomize, target)
            else {
                continue;
            };
            let art = c.art();
            self.append_template_button(
                out,
                template,
                (art.x, art.y),
                self.button_state(action, false, true),
                viewport,
            );
            let caption = c.caption();
            center_label(out, text, caption.x, caption.y, caption.w);
        }
    }

    fn append_charcreate_controls(&self, out: &mut Vec<UiQuad>, viewport: (f32, f32)) {
        let Some(font) = self.label_font.as_ref() else {
            return;
        };
        // Art state per control, from the client's own styles.xml: Normal, Highlit under the
        // pointer, Pressed for the current selection, Disabled for an ineligible class. Drawing
        // only Normal — which is what this did — is why clicking a race looked like nothing
        // happened even though the click dispatched.
        // A button's word carries the state, not just its plate. `styles.xml` gives every pregame
        // template four label colours — 192 grey normal, 255/0/0 under the pointer, 255/192/0
        // pressed, 128 grey disabled — and drawing all four as white is why a hovered control
        // looked inert even once the plate art was right. Plate and label are drawn together so
        // they cannot disagree about which state they are in.
        let button = |out: &mut Vec<UiQuad>,
                      template: &str,
                      rect: Rect,
                      text: &str,
                      state: caer_assets::uiskin::ButtonState| {
            let (src, authored) = self.button_art(template, state);
            if self.textures.contains_key("misc_pieces_new.tga") {
                out.push(UiQuad {
                    dst: rect,
                    src,
                    texture: "misc_pieces_new.tga".into(),
                    color: skinui::WHITE,
                });
            }
            skinui::text_quads(
                font,
                &self.label_font_page,
                &skinui::UiText {
                    text: text.into(),
                    rect,
                    color: authored.unwrap_or(skinui::WHITE),
                    center_horizontally: true,
                    font: None,
                    adapter: None,
                },
                out,
            );
        };
        let medium = |out: &mut Vec<UiQuad>,
                      rect: Rect,
                      text: &str,
                      state: caer_assets::uiskin::ButtonState| {
            button(out, "button_pregame_medium", rect, text, state);
        };
        // Random is `button_pregame_small`, a different template from the race/class grid. Drawing
        // only its word — which is what this did — left it the one control on the form with no
        // plate under it, sitting inside the name box's border as though it were part of the field.
        let small = |out: &mut Vec<UiQuad>,
                     rect: Rect,
                     text: &str,
                     state: caer_assets::uiskin::ButtonState| {
            button(out, "button_pregame_small", rect, text, state);
        };
        let label = |out: &mut Vec<UiQuad>, text: &str, rect: Rect, centered: bool| {
            skinui::text_quads(
                font,
                &self.label_font_page,
                &skinui::UiText {
                    text: text.into(),
                    rect,
                    color: skinui::WHITE,
                    center_horizontally: centered,
                    font: None,
                    adapter: None,
                },
                out,
            );
        };

        // H7 — the two authored description panes. Geometry is OWN_CAPTURE from
        // `character_creation.xml`: header LabelDef 1074 (30,85), race name LabelDef 1064 (30,115)
        // over TextAreaDef 1054 (20,134,220x280), class name LabelDef 1064 (30,415) over
        // TextAreaDef 1055 (20,434,220x260). Text comes from the player's own game.dll at runtime;
        // when that is unavailable the panes render empty rather than showing invented prose.
        self.append_plate_caption(PreWorldScreen::CharCreate, viewport, out);
        label(
            out,
            "Descriptions",
            map_pregame_rect(30.0, 85.0, 180.0, 16.0, viewport),
            false,
        );
        {
            let race_id = self.create_draft.race;
            let class_id = self.create_draft.class_id;
            let race_name = caer_protocol::career::race_by_id(race_id)
                .map(|r| caer_protocol::creation_adapters::race_button_label(r.id, r.name));
            let class_name = caer_protocol::career::class_career_by_id(class_id).map(|c| c.name);
            for (name, body, name_y, body_y, body_h) in [
                (
                    race_name,
                    self.descriptions.race(race_id),
                    115.0,
                    134.0,
                    280.0,
                ),
                (
                    class_name,
                    self.descriptions.class(class_id),
                    415.0,
                    434.0,
                    260.0,
                ),
            ] {
                if let Some(name) = name {
                    label(
                        out,
                        name,
                        map_pregame_rect(30.0, name_y, 180.0, 16.0, viewport),
                        false,
                    );
                }
                if let Some(body) = body {
                    skinui::text_block_quads(
                        font,
                        &self.label_font_page,
                        body,
                        map_pregame_rect(20.0, body_y, 220.0, body_h, viewport),
                        pregame_gray(),
                        out,
                    );
                }
            }
        }

        for (heading, x, y, w) in [
            ("Name", 808.0, 85.0, 180.0),
            ("Race", 790.0, 161.0, 112.0),
            ("Gender", 808.0, 283.0, 180.0),
            // Retail labels this section "Way"; Matt's call (2026-08-15) is to keep "Class" as
            // the clearer word. Deliberate, product-level divergence — not a parity defect.
            ("Class", 808.0, 352.0, 180.0),
        ] {
            label(
                out,
                heading,
                map_pregame_rect(x, y, w, 16.0, viewport),
                true,
            );
        }
        let random_rect = charcreate_random_rect(viewport);
        small(
            out,
            random_rect,
            "Random",
            self.button_state(PreWorldAction::CharCreateRandomName, false, true),
        );
        label(
            out,
            if self.create_draft.name.is_empty() {
                "Click to enter name"
            } else {
                &self.create_draft.name
            },
            charcreate_name_rect(viewport),
            true,
        );

        // Race controls resolve from the same manifest dispatch uses (B2 shape). The deleted
        // local array had Albion's Avalonian and Highlander transposed against the captured form.
        for adapter in caer_protocol::creation_adapters::race_adapters(self.create_draft.realm) {
            if let Some(rect) = charcreate_race_rect(adapter.slot, viewport) {
                let state = self.button_state(
                    PreWorldAction::CharCreateRace(adapter.slot),
                    self.create_draft.race == adapter.race_id,
                    true,
                );
                medium(out, rect, adapter.label, state);
            }
        }
        // Class controls render from the same resolved adapter record that dispatch uses, so a
        // button's text and its wire id cannot disagree (B2). There is deliberately no local class
        // label table here — see `caer_protocol::creation_adapters`.
        for adapter in caer_protocol::creation_adapters::class_adapters_for(
            self.create_draft.realm,
            self.create_draft.race,
            self.create_draft.gender,
        ) {
            if let Some(rect) = charcreate_class_rect(adapter.slot, viewport) {
                let state = self.button_state(
                    PreWorldAction::CharCreateClass(adapter.slot),
                    self.create_draft.class_id == adapter.class_id,
                    adapter.eligible,
                );
                medium(out, rect, adapter.label, state);
            }
        }
        for (text, gender) in [("Male", 0), ("Female", 1)] {
            let rect = charcreate_gender_rect(gender, viewport);
            let male_only =
                caer_protocol::creation_adapters::race_adapters(self.create_draft.realm)
                    .iter()
                    .find(|a| a.race_id == self.create_draft.race)
                    .is_some_and(|a| a.male_only);
            let enabled = !(gender == 1 && male_only);
            let state = self.button_state(
                PreWorldAction::CharCreateGender(gender),
                self.create_draft.gender == gender,
                enabled,
            );
            medium(out, rect, text, state);
        }
        // Bottom row, `character_creation.xml`: three round buttons at y=706 with their
        // `64x16_no_bg` labels at y=752. CAER drew two of them, as bare text with no button art —
        // the Realm button (ControlId 1053/1066) was missing entirely, so there was no way back to
        // the realm plate from the creation form.
        for (template, target, action, text) in [
            (
                "quit",
                Target::CharCreateCancel,
                PreWorldAction::CharCreateCancel,
                "Cancel",
            ),
            (
                "realm",
                Target::CharCreateRealm,
                PreWorldAction::BackToRealm,
                "Realm",
            ),
            (
                "customize",
                Target::CharCreateContinue,
                PreWorldAction::CharCreateContinue,
                "Continue",
            ),
        ] {
            let Some(authored) = preworld_hitbox::control_for(PreWorldScreen::CharCreate, target)
            else {
                continue;
            };
            let state = self.button_state(action, false, true);
            let art = authored.art();
            self.append_template_button(out, template, (art.x, art.y), state, viewport);
            label(
                out,
                text,
                caption_rect(PreWorldScreen::CharCreate, target, viewport),
                true,
            );
        }
    }

    /// Upload missing pages and submit quads. Returns skin window list for Settings.
    pub fn render(&mut self, gpu: &mut Gpu, viewport: (f32, f32)) -> Option<&'static str> {
        if let Err(e) = self.ensure_loaded() {
            log::warn!("preworld: load failed: {e}");
            return self.screen.skin_window();
        }
        // Keep WindowManager positions in sync with layout so hit-tests match pixels.
        if self.screen == PreWorldScreen::Login {
            for btn in LoginButton::ALL {
                let name = format!("preworld_login_{btn:?}");
                let r = login_btn_rect(btn, viewport);
                self.windows.set_pos(&name, (r.x, r.y));
            }
        }
        let quads = self.layout(viewport);
        for q in &quads {
            let key = q.texture.to_ascii_lowercase();
            if self.uploaded.contains(&key) {
                continue;
            }
            if let Some(img) = self.textures.get(&key) {
                gpu.upload_ui_page(&q.texture, img);
                self.uploaded.insert(key);
            }
        }
        let skin = self.screen.skin_window();
        // Login composites the account dialog via SkinHud on top of these quads — don't submit yet.
        if skin != Some("login") && !quads.is_empty() {
            let report = gpu.set_ui_quads(&quads);
            if report.requested != report.submitted {
                log::error!(
                    "preworld: refusing partial {:?} UI submission: requested={} submitted={} missing_pages={:?}",
                    self.screen,
                    report.requested,
                    report.submitted,
                    report.missing_pages
                );
            }
        }
        self.last_quads = quads.len();
        self.last_layout = quads;
        skin
    }

    /// Hit-test login chrome. Other screens return `None` until their controls are wired.
    #[must_use]
    pub fn hit_login(&self, x: f32, y: f32, viewport: (f32, f32)) -> Option<LoginButton> {
        if self.screen != PreWorldScreen::Login {
            return None;
        }
        for btn in LoginButton::ALL.into_iter().rev() {
            let r = login_btn_rect(btn, viewport);
            if x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h {
                return Some(btn);
            }
        }
        None
    }

    /// Is the currently selected row a character, rather than an empty slot?
    #[must_use]
    fn selection_is_occupied(&self) -> bool {
        self.selected_protocol_slot.is_some_and(|slot| {
            self.overview
                .as_ref()
                .is_some_and(|ov| ov.characters.iter().any(|c| c.slot == slot))
        })
    }

    /// What the big bottom-right button does right now — `play_create_text` in the client's terms.
    ///
    /// One function so the label, the art state and the click cannot disagree. They did: the draw
    /// asked whether `EnterWorld` was hovered while the hit test would have produced
    /// `OpenCharCreate`, so the button never lit up unless a character was already selected.
    #[must_use]
    pub fn charselect_play_action(&self) -> PreWorldAction {
        if self.selection_is_occupied() {
            PreWorldAction::EnterWorld
        } else {
            PreWorldAction::OpenCharCreate
        }
    }

    /// Whether a semantic caller may invoke an action in the same front-most state as a physical
    /// control. This is the non-geometric half of [`Self::hit_action`]: modals/options swallow the
    /// underlying screen, disabled character controls stay disabled, and Splash/Loading expose no
    /// action surface. Harness callers use this owner instead of maintaining a second screen map.
    #[must_use]
    pub fn semantic_action_allowed(&self, action: PreWorldAction) -> bool {
        use PreWorldAction as A;
        if let Some(modal) = self.modal {
            return matches!(
                (modal, action),
                (
                    preworld_hitbox::Modal::QuitConfirm,
                    A::QuitConfirmYes | A::QuitConfirmNo
                ) | (
                    preworld_hitbox::Modal::DeleteConfirm,
                    A::DeleteConfirmYes | A::DeleteConfirmNo
                )
            ) && (!matches!(action, A::DeleteConfirmYes) || self.modal_confirmed());
        }
        if self.options_open {
            return matches!(action, A::Options(_));
        }
        match self.screen {
            PreWorldScreen::Login => {
                matches!(action, A::LoginPlay | A::OpenSettings | A::LoginExit)
            }
            PreWorldScreen::RealmSelect => matches!(action, A::ChooseRealm(_) | A::LoginExit),
            PreWorldScreen::CharSelect => match action {
                A::OpenCharCreate | A::EnterWorld => action == self.charselect_play_action(),
                A::DeleteCharacter => charselect_control_enabled("Delete"),
                A::SelectCharacterSlot(_)
                | A::BackToRealm
                | A::CharSelectQuit
                | A::CharSelectOptions => true,
                _ => false,
            },
            PreWorldScreen::CharCreate => matches!(
                action,
                A::CharCreateCancel
                    | A::CharCreateContinue
                    | A::CharCreateRandomName
                    | A::CharCreateFocusName
                    | A::CharCreateRace(_)
                    | A::CharCreateClass(_)
                    | A::CharCreateGender(_)
                    | A::BackToRealm
            ),
            PreWorldScreen::CharCustomize => matches!(
                action,
                A::CustomizeAdvance
                    | A::CustomizeStats
                    | A::CustomizeBack
                    | A::CustomizeCancel
                    | A::CustomizeReset
                    | A::CustomizeRandom
                    | A::CustomizeToggleLock { .. }
                    | A::CustomizeAdjust { .. }
                    | A::CustomizeSlider { .. }
                    | A::CustomizeCamera(_)
                    | A::BackToRealm
            ),
            PreWorldScreen::CharStats => matches!(
                action,
                A::StatsAdjust { .. } | A::StatsReset | A::StatsOptimize | A::StatsDismiss
            ),
            PreWorldScreen::Loading | PreWorldScreen::Splash => false,
        }
    }

    /// Map a click on the active screen to a navigation action (SCN-01 / leg 16).
    #[must_use]
    pub fn hit_action(&self, x: f32, y: f32, viewport: (f32, f32)) -> Option<PreWorldAction> {
        // The Quit modal sits in front of everything, the Options Menu included, and swallows
        // every click inside the plate. `Some(..)` on a miss would fall through to the screen
        // behind — and a confirm dialog whose background is still live confirms nothing.
        if let Some(modal) = self.modal {
            let confirmed = self.modal_confirmed();
            return preworld_hitbox::hit_modal(modal, x, y, viewport)
                .and_then(|c| modal_action(c.target))
                .filter(|a| confirmed || *a != PreWorldAction::DeleteConfirmYes);
        }
        // The Options Menu is modal: while it is up, nothing behind it is clickable. Retail is the
        // same — the character screen stays drawn but stops responding.
        if self.options_open {
            let (px, py) = unmap_pregame(x, y, viewport);
            return crate::preworld_options::hit(px, py).map(PreWorldAction::Options);
        }
        match self.screen {
            PreWorldScreen::Login => {
                if let Some(a) = hit_login_dialog(x, y, viewport) {
                    return Some(a);
                }
                match self.hit_login(x, y, viewport)? {
                    LoginButton::Play => Some(PreWorldAction::LoginPlay),
                    LoginButton::Exit => Some(PreWorldAction::LoginExit),
                    LoginButton::Account
                    | LoginButton::Billing
                    | LoginButton::Credits
                    | LoginButton::Support => Some(PreWorldAction::OpenSettings),
                }
            }
            PreWorldScreen::RealmSelect => {
                match preworld_hitbox::hit(PreWorldScreen::RealmSelect, x, y, viewport)?.target {
                    Target::RealmQuit => Some(PreWorldAction::LoginExit),
                    Target::RealmColumn(realm) => Some(PreWorldAction::ChooseRealm(realm)),
                    _ => None,
                }
            }
            PreWorldScreen::CharCreate => {
                let action = hit_charcreate(x, y, viewport)?;
                self.filter_create_action(action)
            }
            // The customized form owns the real Continue/create action. The stats modal is
            // local-only Reset/Optimize, never a second creation boundary.
            PreWorldScreen::CharCustomize => {
                hit_charcustomize(x, y, viewport, self.customizer_has_tattoo())
                    .and_then(|action| self.filter_customize_action(action))
            }
            PreWorldScreen::CharStats => {
                // `character_customize_stats.xml` has no close ButtonDef, but its
                // WindowTemplate explicitly sets CloseButton=true. The generic window manager
                // owns that top-right fallback target, so test it before the form's own source
                // controls rather than fabricating a per-form ControlId.
                if preworld_hitbox::hit_stats_dialog_close(x, y, viewport) {
                    return Some(PreWorldAction::StatsDismiss);
                }
                // The source basic-customizer chrome stays visibly composed beneath this modal.
                // Pressing its already-open Adjust Attributes control is the second, discoverable
                // dismissal route; other underlying controls remain swallowed by the modal.
                if matches!(
                    hit_charcustomize(x, y, viewport, self.customizer_has_tattoo()),
                    Some(PreWorldAction::CustomizeStats)
                ) {
                    return Some(PreWorldAction::StatsDismiss);
                }
                match preworld_hitbox::hit(PreWorldScreen::CharStats, x, y, viewport)?.target {
                    preworld_hitbox::Target::StatsAdjust { stat, dir } => {
                        Some(PreWorldAction::StatsAdjust { stat, dir })
                    }
                    preworld_hitbox::Target::StatsReset => Some(PreWorldAction::StatsReset),
                    preworld_hitbox::Target::StatsOptimize => Some(PreWorldAction::StatsOptimize),
                    _ => None,
                }
            }
            PreWorldScreen::CharSelect => {
                // Every rect comes from `character_selection.xml` through `preworld_hitbox`, so a
                // control responds exactly where its art is and the empty space between a round
                // button and its caption responds to nothing.
                //
                // H1: disabled controls do not route. Re-enabling Delete means flipping its
                // `CHARSELECT_CHROME` flag *and* landing the protocol dispatch plus
                // `delete_confirm.xml`; the flag alone would make it clickable again, which is why
                // both the draw and the hit test read it.
                //
                // The ten character rows route from here too. They were drawn, hovered and
                // hit-tested by `hit_char_slot`, but nothing routed them through `hit_action` —
                // and the click handler returns the moment this says None, so every row click
                // printed "no control there" and no character could ever be selected.
                let hit = preworld_hitbox::hit(PreWorldScreen::CharSelect, x, y, viewport)?;
                match hit.target {
                    Target::CharSelectPlay => Some(self.charselect_play_action()),
                    Target::CharSelectDelete => charselect_control_enabled("Delete")
                        .then_some(PreWorldAction::DeleteCharacter),
                    // This is *existing-character* customization, a different protocol path
                    // from the new-character `CharCustomize` creation screen. It stays disabled
                    // until its selected-row request/response contract exists; crucially, it may
                    // not masquerade as the unrelated Options menu if somebody flips art state.
                    Target::CharSelectCustomize => None,
                    Target::CharSelectRealm => Some(PreWorldAction::BackToRealm),
                    Target::CharSelectQuit => Some(PreWorldAction::CharSelectQuit),
                    Target::CharSelectOptions => Some(PreWorldAction::CharSelectOptions),
                    Target::CharSlot(slot) => Some(PreWorldAction::SelectCharacterSlot(slot)),
                    _ => None,
                }
            }
            PreWorldScreen::Loading | PreWorldScreen::Splash => None,
        }
    }

    /// Apply a navigation action to the local screen state (offline / pre-overview).
    /// Apply the one navigation the protocol flow does not own: the local Settings overlay.
    ///
    /// **B5.** This used to navigate for nearly every action, which made the HUD a second
    /// navigator racing `PreWorldFlow`. The worst case was `ChooseRealm(_) => CharSelect`: it
    /// jumped to the character plate the instant the button was pressed, before any overview
    /// existed, and the next frame's `sync_preworld_screen` — projecting the flow, which correctly
    /// still said RealmSelect — snapped it straight back. Net visible effect of clicking a realm:
    /// nothing.
    ///
    /// Every other screen is now derived from `PreWorldFlow::screen()`, which advances on server
    /// replies and holds a `Pending` state in between. Settings stays here because it is a local
    /// overlay with no protocol event behind it.
    pub fn apply_action(&mut self, action: PreWorldAction) {
        match action {
            // Options is an overlay on whatever screen is showing, not a screen of its own.
            PreWorldAction::OpenSettings | PreWorldAction::CharSelectOptions => {
                self.options_open = true;
                self.options_hover = None;
            }
            PreWorldAction::Options(hit) => self.apply_options_hit(hit),
            // Quit raises the authored modal instead of quitting. Retail ships
            // `pregame/quit_confirm.xml` for exactly this and CAER used to skip straight to the
            // command.
            PreWorldAction::CharSelectQuit => {
                self.modal = Some(preworld_hitbox::Modal::QuitConfirm);
            }
            // Delete raises `delete_confirm.xml` for the same reason Quit raises its own form:
            // retail ships one, and an irreversible action with no confirmation is its own defect.
            PreWorldAction::DeleteCharacter => {
                self.modal = Some(preworld_hitbox::Modal::DeleteConfirm);
                self.modal_typed.clear();
            }
            // Every way out of a modal closes it, confirm included. Both confirms were missing here
            // at first, for different-looking reasons that are the same reason: `DeleteConfirmYes`
            // sent its packet and left the form up, still asking the question the player had just
            // answered; `QuitConfirmYes` looked harmless because the process is leaving — except the
            // exit is deferred to `about_to_wait`, so at least one more frame draws the dialog. The
            // rule is uniform because the exceptions are where the reasoning goes wrong.
            PreWorldAction::QuitConfirmNo
            | PreWorldAction::QuitConfirmYes
            | PreWorldAction::DeleteConfirmNo
            | PreWorldAction::DeleteConfirmYes => {
                self.modal = None;
                self.modal_typed.clear();
            }
            // `QuitConfirmYes` is deliberately absent: leaving the application is the shell's
            // decision, not a HUD state change, and it is routed through `preworld_product`.
            _ => {}
        }
    }

    /// Escape dismisses the raised modal, same as its Cancel button.
    ///
    /// Returns whether anything was dismissed. Checked before [`Self::close_options`] by the same
    /// front-to-back rule the hit test uses, so Escape under both dialogs closes the front one.
    pub fn close_modal(&mut self) -> bool {
        self.modal_typed.clear();
        self.modal.take().is_some()
    }

    /// Escape closes the Options Menu without committing, same as Cancel.
    ///
    /// Returns whether anything was closed, so the caller can decide whether Escape also means
    /// something else this frame.
    pub fn close_options(&mut self) -> bool {
        let was = self.options_open;
        self.options_open = false;
        self.options_hover = None;
        was
    }
}

fn read_mpk_member(root: &Path, arch_rel: &str, member: &str) -> Result<Vec<u8>, String> {
    let path = root.join(arch_rel);
    caer_assets::open_member(&path, member)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .ok_or_else(|| format!("{member} not in {arch_rel}"))
}

fn load_loose_bitmap_font(
    root: &Path,
    rels: &[&str],
) -> Option<caer_assets::bitmapfont::BitmapFont> {
    rels.iter().find_map(|rel| {
        let bytes = std::fs::read(root.join(rel)).ok()?;
        let atlas = caer_assets::tga::decode(&bytes).ok()?;
        caer_assets::bitmapfont::BitmapFont::parse(atlas)
    })
}

fn parse_realm_descriptions(bytes: &[u8]) -> Result<[String; 3], String> {
    let text: String = bytes.iter().map(|&byte| char::from(byte)).collect();
    let mut sections = [String::new(), String::new(), String::new()];
    let mut active = None;
    for raw in text.lines() {
        let line = raw.trim();
        active = match line.to_ascii_uppercase().as_str() {
            "[ALBION]" => Some(0),
            "[HIBERNIA]" => Some(1),
            "[MIDGARD]" => Some(2),
            _ => active,
        };
        if line.starts_with('[') && line.ends_with(']') {
            continue;
        }
        if let Some(index) = active.filter(|_| !line.is_empty()) {
            if !sections[index].is_empty() {
                sections[index].push(' ');
            }
            sections[index].push_str(line);
        }
    }
    if sections.iter().any(String::is_empty) {
        return Err("realmdesc.txt lacks one or more realm sections".into());
    }
    Ok(sections)
}

/// One control in a pre-world bottom row: a round button with a text label beneath it.
///
/// Geometry is not here. It lives in [`preworld_hitbox`], which the hit test reads too, so the
/// art cannot drift away from the rect that responds.
struct ChromeButton {
    label: &'static str,
    /// The same key the hit test resolves to.
    target: Target,
    /// False when the control has no complete path behind it; it draws Disabled and does not route.
    enabled: bool,
    /// `styles.xml` template supplying the round button's four art states.
    template: &'static str,
}

/// Char-select bottom row, every field `OWN_CAPTURE` from `character_selection.xml`.
///
/// The `64x16_no_bg` labels sit at y=752 (ControlIds 1101/1095/1099/1097/1093) with their round
/// buttons above at y~706 (1100/1094/1098/1096/1092), each naming its art template. The round
/// buttons were not drawn at all — the row was five pieces of floating text.
///
/// `enabled` is the single source for both the art state a control draws and whether `hit_action`
/// routes a click to it. Flipping one without the other is the "dishonest control" H1 is about.
/// **Customize** stays disabled specifically on the *existing-character* character-select path:
/// that needs its own selected-row customization request/response protocol. New-character
/// `character_customize.xml` is implemented in the creation chain; pointing this button at it
/// would silently edit a fresh draft rather than the selected character.
///
/// **Delete** was enabled once its path was complete end to end, not before: `delete_confirm.xml` is
/// drawn and routed, `DeleteConfirmYes` dispatches `LiveCommand::DeleteCharacter`, and that sends the
/// 0xFF the oracle specifies. Enabling it on the strength of the modal alone would have been exactly
/// the dishonesty H1 names.
const CHARSELECT_CHROME: [ChromeButton; 5] = [
    ChromeButton {
        label: "Customize",
        target: Target::CharSelectCustomize,
        enabled: false,
        template: "customize",
    },
    ChromeButton {
        label: "Delete",
        target: Target::CharSelectDelete,
        enabled: true,
        template: "delete_char",
    },
    ChromeButton {
        label: "Realm",
        target: Target::CharSelectRealm,
        enabled: true,
        template: "realm",
    },
    ChromeButton {
        label: "Quit",
        target: Target::CharSelectQuit,
        enabled: true,
        template: "quit",
    },
    ChromeButton {
        label: "Options",
        target: Target::CharSelectOptions,
        enabled: true,
        template: "options",
    },
];

/// Whether a named char-select chrome control is a complete, clickable path.
#[must_use]
fn charselect_control_enabled(label: &str) -> bool {
    CHARSELECT_CHROME
        .iter()
        .find(|c| c.label == label)
        .is_some_and(|c| c.enabled)
}

/// Disabled control text — dimmer than [`pregame_gray`], matching the greyed controls in the
/// captured forms.
fn pregame_disabled() -> Rgba {
    Rgba {
        r: 104,
        g: 104,
        b: 104,
        a: 255,
    }
}

/// `generic_textarea`'s `ColorNormal` in `pregame/styles.xml:269` — `OWN_CAPTURE`, not a pick.
fn pregame_gray() -> Rgba {
    Rgba {
        r: 192,
        g: 192,
        b: 192,
        a: 255,
    }
}

fn gold_color() -> Rgba {
    Rgba {
        r: 255,
        g: 200,
        b: 69,
        a: 255,
    }
}

fn append_text(
    font: &caer_assets::bitmapfont::BitmapFont,
    page: &str,
    text: &str,
    rect: Rect,
    color: Rgba,
    centered: bool,
    out: &mut Vec<UiQuad>,
) {
    skinui::text_quads(
        font,
        page,
        &skinui::UiText {
            text: text.into(),
            rect,
            color,
            center_horizontally: centered,
            font: None,
            adapter: None,
        },
        out,
    );
}

fn append_wrapped_text(
    font: &caer_assets::bitmapfont::BitmapFont,
    page: &str,
    text: &str,
    rect: Rect,
    color: Rgba,
    out: &mut Vec<UiQuad>,
) {
    let scale = (rect.h / 290.0).max(0.01);
    let authored_width = rect.w / scale;
    let mut line = String::new();
    let mut y = rect.y;
    let line_height = font.line_height as f32 * scale;
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.into()
        } else {
            format!("{line} {word}")
        };
        if !line.is_empty() && font.measure(&candidate) as f32 > authored_width {
            if y + line_height > rect.y + rect.h {
                return;
            }
            append_text(
                font,
                page,
                &line,
                Rect::new(rect.x, y, rect.w, line_height),
                color,
                false,
                out,
            );
            line.clear();
            line.push_str(word);
            y += line_height;
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() && y + line_height <= rect.y + rect.h {
        append_text(
            font,
            page,
            &line,
            Rect::new(rect.x, y, rect.w, line_height),
            color,
            false,
            out,
        );
    }
}

/// Texture key for a page file — the basename, lowered. Archive members are already bare names;
/// loose paths are not, and keying one by its full path while the draw site keys it by basename is
/// how a page can load successfully and still draw nothing.
fn page_texture_key(file: &str) -> String {
    Path::new(file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(file)
        .to_ascii_lowercase()
}

/// One clickable character row on the select screen (SCN-01).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CharSlotHit {
    pub slot: u8,
    pub row: usize,
}

/// Viewport used by SCN-01 PlayScenario hit-tests (matches `hit_char_slot_picks_ui_index`).
pub const SCN01_VIEWPORT: (f32, f32) = (1600.0, 1000.0);

/// Click point at the center of character list row `row` (same geometry as [`hit_char_slot`]).
#[must_use]
pub fn char_slot_click_point(slot: u8, viewport: (f32, f32)) -> (f32, f32) {
    control_click_point(PreWorldScreen::CharSelect, Target::CharSlot(slot), viewport)
}

/// Where to click to press a control: the centre of its largest visible part, mapped out.
///
/// Not the centre of its bounding box. A character row is a radio plus two lines of text, and the
/// bounding centre falls in the seam between the lines.
fn control_click_point(screen: PreWorldScreen, target: Target, viewport: (f32, f32)) -> (f32, f32) {
    let xf = PreworldTransform::from_viewport(viewport);
    preworld_hitbox::control_for(screen, target).map_or((0.0, 0.0), |c| {
        let (x, y) = c.click_point();
        xf.map(x, y)
    })
}

/// Hit-test a character row in the select list panel (right-side plate).
#[must_use]
pub fn hit_char_slot(
    overview: &caer_protocol::overview::CharacterOverview,
    x: f32,
    y: f32,
    viewport: (f32, f32),
) -> Option<CharSlotHit> {
    // Authored rows, not proportions. This used to guess: a panel at `vw*0.62` wide `vw*0.35`,
    // rows `36px` tall starting at `vh*0.22`, none of which is in the client. The form places ten
    // rows at (780, 125 + 45i), and the rows are drawn there — so clicking one selected a
    // different character than the one under the pointer, or nothing at all.
    //
    // `row` is the index into the compact overview list; `slot` is the protocol slot. H4 turns on
    // the difference, so both are reported and the caller must use `slot`.
    let slot = hit_charselect_slot(x, y, viewport)?;
    overview
        .characters
        .iter()
        .enumerate()
        .find(|(_, c)| c.slot == slot)
        .map(|(row, c)| CharSlotHit { slot: c.slot, row })
}

/// SCN-01 PlayScenario: resolve the select slot **only** via [`hit_char_slot`].
///
/// Clicks the first occupied overview row at [`SCN01_VIEWPORT`]. Returns `None` when the
/// overview is empty or hit-test misses — callers must treat that as Fail (Pass is impossible
/// without a `Some`). Observation that fails if this path is deleted/stubbed with a const:
/// SCN-01 cannot Pass, and [`scn01_goes_red_without_hit_char_slot`] goes red.
#[must_use]
pub fn scn01_slot_from_ui_hit(
    overview: &caer_protocol::overview::CharacterOverview,
) -> Option<CharSlotHit> {
    if overview.characters.is_empty() {
        return None;
    }
    // The first character's *slot*, which H4 says need not be its row index.
    let slot = overview.characters.first()?.slot;
    let (x, y) = char_slot_click_point(slot, SCN01_VIEWPORT);
    hit_char_slot(overview, x, y, SCN01_VIEWPORT)
}

/// Viewport shared by SCN-01/SCN-02 pre-world hit-tests.
pub const SCN02_VIEWPORT: (f32, f32) = SCN01_VIEWPORT;

/// Click point at the center of the character-create Continué control.
#[must_use]
pub fn charcreate_continue_click_point(viewport: (f32, f32)) -> (f32, f32) {
    control_click_point(
        PreWorldScreen::CharCreate,
        Target::CharCreateContinue,
        viewport,
    )
}

/// SCN-02 PlayScenario: Continué must resolve **only** via [`hit_charcreate`].
///
/// Returns `None` when the hit-test misses. Observation that fails if Continué is bypassed with a
/// const `CharCreateContinue`: [`scn02_goes_red_without_hit_charcreate`] and the SCN-02 binding
/// probe both go red (detail requires `hit_charcreate`).
#[must_use]
pub fn scn02_continue_from_ui_hit() -> Option<PreWorldAction> {
    let (x, y) = charcreate_continue_click_point(SCN02_VIEWPORT);
    hit_charcreate(x, y, SCN02_VIEWPORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realm_description_sections_are_not_invented() {
        let parsed = parse_realm_descriptions(
            b"[ALBION]\r\nAlbion prose.\r\nSecond line.\r\n[HIBERNIA]\r\nHibernia prose.\r\n[MIDGARD]\r\nMidgard prose.\r\n",
        )
        .unwrap();
        assert_eq!(parsed[0], "Albion prose. Second line.");
        assert_eq!(parsed[1], "Hibernia prose.");
        assert_eq!(parsed[2], "Midgard prose.");
    }

    /// **Ledger A11.** The plate fills the surface at every aspect; it does not letterbox.
    ///
    /// This test previously asserted the opposite — `assert!(xf.ox > 0.0, "16:9 must pillarbox a 4:3
    /// plate")` — and it was *correct about the code* the whole time. That is the point worth
    /// keeping: a green test can pin a defect in place as firmly as it pins a fix, and this one
    /// guarded 240px of black down both sides of every 16:9 display for as long as it passed.
    ///
    /// Matt's decision (2026-08-18): all three window modes fill the display, matching retail and
    /// Eden. The 4:3 plate is therefore stretched on a 16:9 screen, deliberately.
    #[test]
    fn pregame_stretches_to_fill_the_surface_at_every_aspect() {
        for vp in [
            (1024.0, 768.0),  // authored, 4:3 — a 1:1 map
            (1920.0, 1080.0), // 16:9, the case that used to pillarbox
            (2560.0, 1080.0), // 21:9
            (1280.0, 1600.0), // taller than wide
        ] {
            let xf = PreworldTransform::from_viewport(vp);
            assert_eq!((xf.ox, xf.oy), (0.0, 0.0), "{vp:?}: no bars, so no offset");

            // The plate's corners ARE the surface's corners.
            let (x0, y0) = xf.map(0.0, 0.0);
            let (x1, y1) = xf.map(PREGAME_W, PREGAME_H);
            assert!((x0).abs() < 0.01 && (y0).abs() < 0.01, "{vp:?}: top-left");
            assert!(
                (x1 - vp.0).abs() < 0.01 && (y1 - vp.1).abs() < 0.01,
                "{vp:?}: bottom-right must reach the surface edge, got ({x1},{y1})"
            );

            // `plate_rect` agrees with `map`, and round-tripping is exact.
            let plate = xf.plate_rect();
            assert!((plate.x).abs() < 0.01 && (plate.y).abs() < 0.01);
            assert!((plate.w - vp.0).abs() < 0.01 && (plate.h - vp.1).abs() < 0.01);
            let (u, v) = xf.unmap(vp.0 * 0.5, vp.1 * 0.5);
            assert!(
                (u - PREGAME_W * 0.5).abs() < 1.0 && (v - PREGAME_H * 0.5).abs() < 1.0,
                "{vp:?}: the surface centre is the plate centre"
            );
        }

        // 4:3 is still a uniform map; everything else is not, and that asymmetry is the feature.
        let square = PreworldTransform::from_viewport((1024.0, 768.0));
        assert!(
            (square.sx - square.sy).abs() < 1e-6,
            "the authored aspect is undistorted"
        );
        let wide = PreworldTransform::from_viewport((1920.0, 1080.0));
        assert!(wide.sx > wide.sy, "16:9 stretches horizontally, by design");
    }

    /// Splash and loading art are opaque plates too.  Their source dimensions differ sharply
    /// (`splash*.tga` is 4:3; `spirit1.dds` is a 4:1 loading banner), but that is not permission
    /// to expose a black letterbox around either one.  The consumer surface is the full display,
    /// just as it is for every other pre-world plate.
    ///
    /// The loading half is the discriminating control: the old aspect-fit branch drew a
    /// 2048×512 `spirit1.dds` band on a 2048×1536 capture, leaving two giant black bars.  This
    /// assertion is therefore red on that implementation while the ordinary 4:3 splash can hide
    /// the same defect at an authored-aspect viewport.
    #[test]
    fn opaque_splash_and_loading_plates_fill_every_viewport() {
        let viewport = (2048.0, 1536.0);
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));

        for (screen, member, width, height) in [
            (PreWorldScreen::Splash, "splash1.tga", 1024, 768),
            (PreWorldScreen::Loading, "spirit1.dds", 1024, 256),
        ] {
            hud.set_screen(screen);
            hud.textures.insert(
                member.to_string(),
                TgaImage {
                    width,
                    height,
                    rgba: Vec::new(),
                },
            );
            let plate = hud
                .layout(viewport)
                .into_iter()
                .find(|q| q.texture.eq_ignore_ascii_case(member))
                .unwrap_or_else(|| panic!("missing {screen:?} plate {member}"));
            assert_eq!(
                plate.dst,
                Rect::new(0.0, 0.0, viewport.0, viewport.1),
                "{screen:?} must cover the opaque pre-world surface"
            );
        }
    }

    /// The creation chain owns one continuous 3D preview; only the surrounding retail form
    /// changes.  The old predicate admitted CharCreate and then dropped CharCustomize/CharStats,
    /// producing a black screen even with a valid draft.  Separately, the custom form is the one
    /// pre-world canvas stored in `pregame003.mpk` (`pregame/asset.xml`), not `pregame.mpk`.
    ///
    /// The two explicit known-bad values below are the former implementation.  Either one would
    /// make this gate red while the rest of a character-create screenshot could still look sane.
    #[test]
    fn creation_subscreens_keep_the_stage_and_use_their_authored_archive() {
        for screen in [
            PreWorldScreen::CharCreate,
            PreWorldScreen::CharCustomize,
            PreWorldScreen::CharStats,
        ] {
            assert!(
                screen.has_scene_behind(),
                "{screen:?} lost its 3D preview stage"
            );
        }
        assert_ne!(
            PreWorldScreen::CharCustomize.archive_rel(),
            Some("pregame/pregame.mpk"),
            "known-bad shared archive: character_customize.tga only ships in pregame003.mpk"
        );
        assert_eq!(
            PreWorldScreen::CharCustomize.archive_rel(),
            Some("pregame/pregame003.mpk")
        );
        assert_eq!(
            PreWorldScreen::CharStats.archive_rel(),
            Some("pregame/pregame003.mpk"),
            "the stats dialog is over the customizer, not a bare pregame.mpk scene"
        );
        assert_eq!(
            PreWorldScreen::CharStats.background_member(),
            Some("character_customize.tga"),
            "the source modal keeps the customizer canvas visible underneath"
        );
    }

    #[test]
    fn pregame_identity_map_on_authored_size() {
        let vp = (1024.0, 768.0);
        assert_eq!(map_pregame(0.0, 0.0, vp), (0.0, 0.0));
        let (x, y) = map_pregame(1024.0, 768.0, vp);
        assert!((x - 1024.0).abs() < 0.01 && (y - 768.0).abs() < 0.01);
    }

    #[test]
    fn bottom_icon_regions_are_live_controls() {
        let vp = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::RealmSelect);
        assert_eq!(
            hud.hit_action(90.0, 720.0, vp),
            Some(PreWorldAction::LoginExit)
        );

        hud.set_screen(PreWorldScreen::CharSelect);
        assert_eq!(
            hud.hit_action(873.0, 600.0, vp),
            Some(PreWorldAction::OpenCharCreate)
        );
        assert_eq!(
            hud.hit_action(298.0, 720.0, vp),
            Some(PreWorldAction::BackToRealm)
        );
        assert_eq!(
            hud.hit_action(100.0, 720.0, vp),
            Some(PreWorldAction::CharSelectQuit)
        );
        assert_eq!(
            hud.hit_action(513.0, 720.0, vp),
            Some(PreWorldAction::CharSelectOptions)
        );
    }

    #[test]
    fn pregame_hit_at_unequal_aspect_agrees_with_draw_rect() {
        let vp = (1920.0, 1080.0);
        let xf = PreworldTransform::from_viewport(vp);
        let plate = xf.plate_rect();
        assert!(
            (plate.x - xf.map(0.0, 0.0).0).abs() < 0.01,
            "background plate origin must equal map(0,0)"
        );
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharCreate);
        let r = charcreate_continue_rect(vp);
        let cx = r.x + r.w * 0.5;
        let cy = r.y + r.h * 0.5;
        assert_eq!(
            hud.hit_action(cx, cy, vp),
            Some(PreWorldAction::CharCreateContinue)
        );
        assert_ne!(
            hud.hit_action(cx - r.w, cy, vp),
            Some(PreWorldAction::CharCreateContinue),
            "a miss offset by one control width must not still hit Continue"
        );

        hud.set_screen(PreWorldScreen::CharSelect);
        let play = map_pregame_rect(810.0, 575.0, 128.0, 82.0, vp);
        let px = play.x + play.w * 0.5;
        let py = play.y + play.h * 0.5;
        assert_eq!(
            hud.hit_action(px, py, vp),
            Some(PreWorldAction::OpenCharCreate),
            "Play/Create draw rect must hit at 16:9"
        );

        hud.set_screen(PreWorldScreen::RealmSelect);
        let alb = realm_btn_rect(RealmButton::Albion, vp);
        assert_eq!(
            hit_realm(alb.x + alb.w * 0.5, alb.y + alb.h * 0.5, vp),
            Some(RealmButton::Albion)
        );
        // There is no pillarbox to test any more (A11) — the plate reaches the surface edge, so
        // x=0 is legitimately inside Albion's column. What still has to hold is that hit and draw
        // agree, which is what the rest of this test measures. The replacement negative: a point
        // *outside* the surface is still nothing.
        assert_eq!(
            hit_realm(-1.0, alb.y + alb.h * 0.5, vp),
            None,
            "a point off the surface must not be a realm hit"
        );
    }

    /// Whose body it is and what face it wears are decided by the same screen, so they cannot
    /// disagree. CharCreate shows the draft — which is what the create packet will carry, so the
    /// created character matches the figure the player accepted. CharSelect shows the saved
    /// character's stored bytes. Nowhere else has a body, and the default is the uncustomised
    /// character DOL stores when nobody touched the form.
    #[test]
    fn the_previewed_body_wears_the_look_that_screen_owns() {
        use caer_protocol::customization::Customization;
        use caer_protocol::overview::{CharacterOverview, CharacterSummary};

        let stored = Customization {
            eye_color: 2,
            hair_color: 3,
            face_type: 4,
            hair_style: 5,
            mood_type: 6,
        };
        let overview = CharacterOverview {
            flags: 0,
            characters: vec![CharacterSummary {
                slot: 3,
                level: 1,
                name: "Hero".into(),
                race_name: "Briton".into(),
                race_gender: 1,
                custom: stored,
                ..Default::default()
            }],
        };
        let mut draft = caer_protocol::charcreate::CharacterCreateDraft::albion_briton_stub("D", 0);
        let drafted = Customization {
            eye_color: 1,
            hair_color: 1,
            face_type: 7,
            hair_style: 2,
            mood_type: 0,
        };
        draft.set_customization(drafted);

        assert_eq!(
            preview_customization(PreWorldScreen::CharCreate, None, None, &draft),
            drafted,
            "CharCreate shows the draft the player is building"
        );
        assert_eq!(
            preview_customization(PreWorldScreen::CharSelect, Some(&overview), Some(3), &draft),
            stored,
            "CharSelect shows the saved character's own look, not the draft's"
        );
        assert!(
            preview_customization(PreWorldScreen::CharSelect, Some(&overview), Some(0), &draft)
                .is_default(),
            "an empty slot has no look to show"
        );
        assert!(
            preview_customization(
                PreWorldScreen::RealmSelect,
                Some(&overview),
                Some(3),
                &draft
            )
            .is_default(),
            "a screen with no body has no look"
        );
    }

    #[test]
    fn preview_identity_requires_an_occupied_selected_slot() {
        use caer_protocol::overview::{CharacterOverview, CharacterSummary};

        let occupied = CharacterSummary {
            slot: 3,
            level: 1,
            name: "Hero".into(),
            race_name: "Briton".into(),
            race_gender: 1,
            ..Default::default()
        };
        let overview = CharacterOverview {
            flags: 0,
            characters: vec![occupied],
        };

        assert_eq!(
            preview_identity(PreWorldScreen::CharSelect, None, None, 1, 0),
            None,
            "empty overview must not show a body"
        );
        assert_eq!(
            preview_identity(PreWorldScreen::CharSelect, Some(&overview), None, 1, 0),
            None,
            "unselected occupied overview must not show a body"
        );
        assert_eq!(
            preview_identity(PreWorldScreen::CharSelect, Some(&overview), Some(0), 1, 0),
            None,
            "empty slot 0 must not fall back to a seeded race"
        );
        let selected =
            preview_identity(PreWorldScreen::CharSelect, Some(&overview), Some(3), 99, 1);
        assert_eq!(
            selected,
            Some((1, 1)),
            "occupied slot 3 is Briton male fig3"
        );

        let create = preview_identity(PreWorldScreen::CharCreate, None, None, 3, 1);
        assert_eq!(
            create,
            Some((3, 2)),
            "CharCreate shows the draft (Highlander female)"
        );
        assert_eq!(
            preview_identity(PreWorldScreen::RealmSelect, Some(&overview), Some(3), 1, 0),
            None
        );
    }

    #[test]
    fn retail_realm_surface_loads_prose_fonts_and_quit_art() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "retail_realm_surface_loads_prose_fonts_and_quit_art",
        ) else {
            return;
        };
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::RealmSelect);
        hud.ensure_loaded().unwrap();
        assert!(hud.realm_descriptions.iter().all(|text| text.len() > 40));
        assert!(hud.label_font.is_some());
        assert!(hud.gold_font.is_some());
        assert!(hud.textures.contains_key("buttons_wide.tga"));
        let quads = hud.layout((1024.0, 768.0));
        assert!(quads.iter().any(|quad| quad.texture == "buttons_wide.tga"));
        assert!(quads.iter().any(|quad| quad.texture == hud.gold_font_page));
        assert!(quads.iter().any(|quad| quad.texture == hud.label_font_page));
    }

    #[test]
    fn phase_maps_login_and_charselect() {
        assert_eq!(
            PreWorldScreen::from_phase(SessionPhase::Disconnected),
            Some(PreWorldScreen::Login)
        );
        assert_eq!(
            PreWorldScreen::from_phase(SessionPhase::Authenticating),
            Some(PreWorldScreen::Splash),
            "auth in flight must show the intro EA splash (control capture)"
        );
        assert_eq!(
            PreWorldScreen::from_phase(SessionPhase::RealmSelect),
            Some(PreWorldScreen::RealmSelect)
        );
        assert_eq!(
            PreWorldScreen::from_phase(SessionPhase::CharacterSelect),
            Some(PreWorldScreen::CharSelect)
        );
        assert_eq!(
            PreWorldScreen::from_phase(SessionPhase::EnteringWorld),
            Some(PreWorldScreen::Loading)
        );
        assert_eq!(PreWorldScreen::from_phase(SessionPhase::InWorld), None);
    }

    #[test]
    fn loads_splash_plate_when_client_present() {
        let Some(root) =
            caer_assets::client_dep::require_caer_client("loads_splash_plate_when_client_present")
        else {
            return;
        };
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::Splash);
        hud.ensure_loaded().expect("load splash plate");
        let quads = hud.layout((1024.0, 768.0));
        assert!(
            quads
                .iter()
                .any(|q| q.texture.eq_ignore_ascii_case("splash1.tga")),
            "expected splash1.tga intro plate"
        );
        assert!(
            quads
                .iter()
                .any(|q| q.texture.eq_ignore_ascii_case("splashloadbar1.tga")),
            "expected splash loadbar overlay"
        );
    }

    #[test]
    fn loads_loading_plate_when_client_present() {
        let Some(root) =
            caer_assets::client_dep::require_caer_client("loads_loading_plate_when_client_present")
        else {
            return;
        };
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::Loading);
        hud.ensure_loaded().expect("load loading plate");
        let quads = hud.layout((1024.0, 768.0));
        // Whatever plate the flow has configured must actually reach the screen. This named
        // `mid1.dds` outright and went stale the moment the neutral fallback moved to `spirit1`
        // — and nobody noticed, because without `$CAER_CLIENT` the test returned early and cargo
        // counted that as a pass.
        let want = crate::preworld_flow::NEUTRAL_LOADING_PLATE.member;
        assert!(
            quads.iter().any(|q| q.texture.eq_ignore_ascii_case(want)),
            "expected the configured loading plate {want:?}; got {:?}",
            quads.iter().map(|q| &q.texture).collect::<Vec<_>>()
        );
    }

    #[test]
    fn realm_select_owns_three_authored_crest_quads() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "realm_select_owns_three_authored_crest_quads",
        ) else {
            return;
        };
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::RealmSelect);
        hud.ensure_loaded().expect("load realm-select chrome");
        let quads = hud.layout((1024.0, 768.0));

        for button in RealmButton::ALL {
            let matching: Vec<_> = quads
                .iter()
                .filter(|quad| quad.texture.eq_ignore_ascii_case(button.member()))
                .collect();
            assert_eq!(matching.len(), 1, "{:?} must own one crest quad", button);
            assert_eq!(matching[0].dst, button.authored_rect((1024.0, 768.0)));
            assert_eq!(matching[0].src, button.normal_src());
        }
    }

    #[test]
    fn realm_hover_uses_retail_highlight_crop_without_changing_hit_target() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "realm_hover_uses_retail_highlight_crop_without_changing_hit_target",
        ) else {
            return;
        };
        let viewport = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::RealmSelect);
        hud.ensure_loaded().expect("load realm-select chrome");

        let column = realm_btn_rect(RealmButton::Hibernia, viewport);
        hud.set_pointer(column.x + column.w * 0.5, column.y + 100.0, viewport);
        let quads = hud.layout(viewport);
        let crest = quads
            .iter()
            .find(|q| {
                q.texture
                    .eq_ignore_ascii_case(RealmButton::Hibernia.member())
            })
            .expect("Hibernia crest");
        assert_eq!(crest.src, RealmButton::Hibernia.highlight_src());
        assert_eq!(
            hit_realm(column.x + column.w * 0.5, column.y + 100.0, viewport),
            Some(RealmButton::Hibernia)
        );
    }

    /// A hovered control has to *look* hovered. `styles.xml` gives every pregame button template
    /// four label colours — 192 grey normal, 255/0/0 highlit, 255/192/0 pressed, 128 disabled —
    /// and the draw path passed `skinui::WHITE` for all of them, so a pointer changed only the
    /// plate behind the word.
    ///
    /// This reads the **drawn glyph quads**, not the template table. An earlier version of this
    /// test asserted that `button_art` returns the authored colours, which it always did — the
    /// defect was at the call site that threw the second half of the tuple away, so that version
    /// passed against the known-bad and proved nothing.
    ///
    /// Seen red by restoring `color: skinui::WHITE` in the `button` closure.
    #[test]
    fn button_labels_carry_their_authored_state_colour() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "button_labels_carry_their_authored_state_colour",
        ) else {
            return;
        };
        let viewport = (1024.0, 768.0);
        let xf = PreworldTransform::from_viewport(viewport);
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::CharCreate);
        hud.ensure_loaded().expect("load charcreate chrome");

        // Slot 0 is the draft's current race, so it draws Pressed; slot 1 is the untouched one.
        let race0 = charcreate_race_rect(0, viewport).expect("race slot 0 is authored");
        let race1 = charcreate_race_rect(1, viewport).expect("race slot 1 is authored");
        // Glyph quads are the ones on the font atlas page; the plate is on the button page.
        let label_colour = |hud: &PreWorldHud, race0: Rect| -> Vec<(u8, u8, u8)> {
            let mut seen: Vec<(u8, u8, u8)> = hud
                .layout(viewport)
                .into_iter()
                .filter(|q| q.texture.eq_ignore_ascii_case(&hud.label_font_page))
                .filter(|q| {
                    q.dst.x >= race0.x - 1.0
                        && q.dst.x + q.dst.w <= race0.x + race0.w + 1.0
                        && q.dst.y >= race0.y - 1.0
                        && q.dst.y + q.dst.h <= race0.y + race0.h + 1.0
                })
                .map(|q| (q.color.r, q.color.g, q.color.b))
                .collect();
            seen.sort_unstable();
            seen.dedup();
            seen
        };

        let _ = xf;
        assert_eq!(
            label_colour(&hud, race1),
            vec![(192, 192, 192)],
            "an untouched race button draws its authored normal colour, not white"
        );
        assert_eq!(
            label_colour(&hud, race0),
            vec![(255, 192, 0)],
            "the selected race draws the authored pressed colour"
        );

        hud.set_pointer(race1.x + race1.w * 0.5, race1.y + race1.h * 0.5, viewport);
        assert_eq!(
            label_colour(&hud, race1),
            vec![(255, 0, 0)],
            "under the pointer the word turns the authored highlit red"
        );
    }

    /// **Ledger A13.** The Quit modal's authored art reaches the frame, not just its words.
    ///
    /// `append_template_button` takes `&self` and cannot load a page, so it draws **nothing** when
    /// the atlas is missing — indistinguishable from a control that was never authored. That is how
    /// the first working version of this modal shipped Yes and No as bare text, and it is J10's
    /// mechanism on the login dialog too. Counting quads at the authored rects is what separates
    /// "the template resolved" from "the player can see a button".
    ///
    /// Seen red by removing the `quit_confirm_open` page pre-load from `ensure_loaded`: the two
    /// plate quads disappear and the words remain.
    #[test]
    fn the_quit_modal_draws_its_authored_button_art() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "the_quit_modal_draws_its_authored_button_art",
        ) else {
            return;
        };
        let viewport = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::CharSelect);
        hud.apply_action(PreWorldAction::CharSelectQuit);
        assert!(hud.quit_confirm_open());
        hud.ensure_loaded()
            .expect("load char-select + modal chrome");

        // Resolve the page's texture key **without** loading it. The first version of this test
        // called `ensure_button_page`, which loads the atlas as a side effect — so it passed with
        // the fix removed, and was not a gate at all. This is the exact defect class the ledger
        // exists for, committed inside the test written to prove the fix.
        let page = {
            let name = hud
                .button_templates
                .get("button_small")
                .map(|t| t.texture.to_ascii_lowercase())
                .expect("styles.xml declares button_small");
            let (_, member) = hud
                .texture_pages
                .get(&name)
                .expect("asset.xml maps the button_small page");
            page_texture_key(member)
        };
        assert!(
            hud.textures.contains_key(&page),
            "{page} must be loaded while the modal is up"
        );
        let quads = hud.layout(viewport);
        let xf = PreworldTransform::from_viewport(viewport);
        for c in preworld_hitbox::modal_controls(preworld_hitbox::Modal::QuitConfirm) {
            let art = c.art().mapped(xf);
            assert!(
                quads.iter().any(|q| {
                    q.texture.eq_ignore_ascii_case(&page)
                        && (q.dst.x - art.x).abs() < 0.5
                        && (q.dst.y - art.y).abs() < 0.5
                }),
                "{} must draw {page} at its authored rect {:?}, not just its word",
                c.name,
                (art.x, art.y)
            );
        }
        // And the client's own prompt is in the frame. A modal that asks nothing is not a
        // confirmation.
        assert!(
            !QUIT_CONFIRM_PROMPT.is_empty() && quads.len() > 2,
            "the modal drew {} quads",
            quads.len()
        );
    }

    #[test]
    fn clean_retail_font_and_create_controls_are_visible() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "clean_retail_font_and_create_controls_are_visible",
        ) else {
            return;
        };
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::CharCreate);
        hud.ensure_loaded().expect("load character-create chrome");

        assert!(
            hud.label_font.is_some(),
            "clean retail ui/fonts/Arial11b.tga must supply pre-world labels"
        );
        let viewport = (1024.0, 768.0);
        let quads = hud.layout(viewport);
        let button_quads = quads
            .iter()
            .filter(|quad| quad.texture.eq_ignore_ascii_case("misc_pieces_new.tga"))
            .count();

        // This asserted a flat `14` — "seven races, five classes, and two genders" — which was the
        // pre-B2 form. The create screen authors sixteen class slots; eleven of them were
        // unreachable. The hardcoded count would have caught that regression if it had ever run,
        // but without `$CAER_CLIENT` the test returned early and counted as a pass.
        //
        // Derived from the same adapter manifests the layout draws from, and paired with the two
        // `is_some` checks below, which are the part that bites: an authored slot with no rect is a
        // button the player cannot click.
        let races = caer_protocol::creation_adapters::race_adapters(hud.create_draft.realm);
        let classes = caer_protocol::creation_adapters::class_adapters(
            hud.create_draft.realm,
            hud.create_draft.race,
        );
        assert!(
            races
                .iter()
                .all(|a| charcreate_race_rect(a.slot, viewport).is_some()),
            "every authored race slot needs a rect; one without is an unreachable button"
        );
        assert!(
            classes
                .iter()
                .all(|a| charcreate_class_rect(a.slot, viewport).is_some()),
            "every authored class slot needs a rect; one without is an unreachable button"
        );
        assert!(
            classes.len() >= 15,
            "the form authors sixteen class slots; {} means the five-class regression is back",
            classes.len()
        );
        // races + classes + two genders + Random. Random is counted separately because it is the
        // one control here on `button_pregame_small`, and it drew as a bare word for long enough
        // to look like part of the name field's border.
        assert_eq!(
            button_quads,
            races.len() + classes.len() + 3,
            "every race, class, gender and the Random control needs visible button chrome"
        );
        let random = charcreate_random_rect(viewport);
        assert!(
            quads.iter().any(|q| {
                q.texture.eq_ignore_ascii_case("misc_pieces_new.tga")
                    && (q.dst.x - random.x).abs() < 0.5
                    && (q.dst.y - random.y).abs() < 0.5
            }),
            "Random must draw a plate at its authored rect, not just its word"
        );
        assert!(
            quads.iter().any(|quad| quad.texture == hud.label_font_page),
            "create controls must paint labels through the retail bitmap font"
        );
    }

    #[test]
    fn login_buttons_do_not_overlap() {
        let vp = (1600.0, 1000.0);
        let rects: Vec<_> = LoginButton::ALL
            .into_iter()
            .map(|b| (b, login_btn_rect(b, vp)))
            .collect();
        for (i, (a, ra)) in rects.iter().enumerate() {
            for (b, rb) in rects.iter().skip(i + 1) {
                let overlap = ra.x < rb.x + rb.w
                    && ra.x + ra.w > rb.x
                    && ra.y < rb.y + rb.h
                    && ra.y + ra.h > rb.y;
                assert!(!overlap, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn hit_test_finds_play() {
        let hud = PreWorldHud::new(PathBuf::from("."));
        let vp = (1600.0, 1000.0);
        let r = login_btn_rect(LoginButton::Play, vp);
        assert_eq!(
            hud.hit_login(r.x + 1.0, r.y + 1.0, vp),
            Some(LoginButton::Play)
        );
        assert_eq!(hud.hit_login(0.0, 0.0, vp), None);
    }

    #[test]
    fn hit_realm_returns_protocol_ids() {
        let vp = (1600.0, 1000.0);
        let alb = realm_btn_rect(RealmButton::Albion, vp);
        assert_eq!(
            hit_realm(alb.x + 1.0, alb.y + 1.0, vp),
            Some(RealmButton::Albion)
        );
        assert_eq!(RealmButton::Albion.protocol_id(), 1);
        assert_eq!(RealmButton::Midgard.protocol_id(), 2);
        assert_eq!(RealmButton::Hibernia.protocol_id(), 3);
    }

    #[test]
    fn entire_retail_realm_columns_are_clickable() {
        let vp = (1024.0, 768.0);
        assert_eq!(hit_realm(20.0, 500.0, vp), Some(RealmButton::Albion));
        assert_eq!(hit_realm(500.0, 500.0, vp), Some(RealmButton::Hibernia));
        assert_eq!(hit_realm(900.0, 500.0, vp), Some(RealmButton::Midgard));
        assert_eq!(hit_realm(500.0, 700.0, vp), None);
    }

    #[test]
    fn hit_charcreate_continue_and_cancel() {
        let vp = (1600.0, 1000.0);
        let cont = charcreate_continue_rect(vp);
        assert_eq!(
            hit_charcreate(cont.x + 1.0, cont.y + 1.0, vp),
            Some(PreWorldAction::CharCreateContinue)
        );
        let cancel = charcreate_cancel_rect(vp);
        assert_eq!(
            hit_charcreate(cancel.x + 1.0, cancel.y + 1.0, vp),
            Some(PreWorldAction::CharCreateCancel)
        );
    }

    /// Every live customize control must route and hover at its own authored art. This is the
    /// direct guard against the old state where `character_customize.xml` drew a transparent frame
    /// over a valid 3D stage but `hit_action` returned `None` everywhere.
    #[test]
    fn every_customizer_control_routes_at_its_authored_pixel() {
        let vp = (1600.0, 1000.0);
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharCustomize);
        let table = preworld_hitbox::controls(PreWorldScreen::CharCustomize);
        assert_eq!(
            table.len(),
            44,
            "6 text-selector arrows + 6 sliders + 12 locks + 7 camera + 2 action + 5 chrome controls"
        );
        for c in table {
            let want = match c.target {
                Target::CustomizeCancel => PreWorldAction::CustomizeCancel,
                Target::CustomizeRealm => PreWorldAction::BackToRealm,
                Target::CustomizeBack => PreWorldAction::CustomizeBack,
                Target::CustomizeAdvance => PreWorldAction::CustomizeAdvance,
                Target::CustomizeStats => PreWorldAction::CustomizeStats,
                Target::CustomizeReset => PreWorldAction::CustomizeReset,
                Target::CustomizeRandom => PreWorldAction::CustomizeRandom,
                Target::CustomizeLock { field } => PreWorldAction::CustomizeToggleLock { field },
                Target::CustomizeAdjust { field, dir } => {
                    PreWorldAction::CustomizeAdjust { field, dir }
                }
                Target::CustomizeSlider { field } => {
                    PreWorldAction::CustomizeSlider { field, tick: 4 }
                }
                Target::CustomizeCamera(control) => PreWorldAction::CustomizeCamera(control),
                other => panic!("customizer table contains non-custom target {other:?}"),
            };
            let (x, y) = control_click_point(PreWorldScreen::CharCustomize, c.target, vp);
            assert_eq!(
                hud.hit_action(x, y, vp),
                Some(want),
                "{:?} is drawn but does not route",
                c.target
            );
            hud.set_pointer(x, y, vp);
            assert_eq!(
                hud.hovered_action,
                Some(want),
                "{:?} routes but does not light",
                c.target
            );
        }
    }

    /// The retail common form owns Tattoo geometry, but its fig3 adapter suppresses it for
    /// identities with no decal map. A visible-but-refusing Briton Tattoo row was the exact
    /// player-facing regression reported in the first native creation run.
    #[test]
    fn source_catalogue_hides_briton_tattoo_controls_but_keeps_celt_tattoo_controls() {
        let root = caer_assets::client_dep::required_caer_client_root("customizer_tattoo_gate");
        let catalog = Arc::new(
            crate::preworld_appearance::AppearanceCatalog::load(&root)
                .expect("load retail fig3 appearance catalogue"),
        );
        assert!(
            !catalog.has_selector(1, 0, crate::preworld_appearance::AppearanceSelector::Tattoo),
            "retail Briton male has no tattoo choices"
        );
        assert!(
            catalog.has_selector(9, 0, crate::preworld_appearance::AppearanceSelector::Tattoo),
            "retail Celt male has tattoo choices"
        );

        let vp = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(root);
        hud.set_screen(PreWorldScreen::CharCustomize);
        hud.set_appearance_catalog(Some(catalog));
        let mut briton =
            caer_protocol::charcreate::CharacterCreateDraft::albion_briton_stub("Tester", 0);
        briton.set_race(1);
        briton.set_gender(0);
        hud.set_create_draft(&briton);

        assert!(
            preworld_hitbox::customizer_control_for(
                false,
                Target::CustomizeAdjust {
                    field: CustomizerField::Tattoo,
                    dir: -1,
                },
            )
            .is_none(),
            "Briton profile has no hidden Tattoo control"
        );
        let size = preworld_hitbox::customizer_control_for(
            false,
            Target::CustomizeAdjust {
                field: CustomizerField::Size,
                dir: -1,
            },
        )
        .expect("Briton Size left geometry");
        let (x, y) = (
            size.art().mapped(PreworldTransform::from_viewport(vp)).x + 1.0,
            size.art().mapped(PreworldTransform::from_viewport(vp)).y + 1.0,
        );
        assert_eq!(
            hud.hit_action(x, y, vp),
            Some(PreWorldAction::CustomizeAdjust {
                field: CustomizerField::Size,
                dir: -1,
            }),
            "Size must close the Tattoo gap instead of leaving a dead-looking row"
        );
        assert!(
            !preworld_hitbox::customizer_controls(false)
                .iter()
                .any(|control| {
                    matches!(
                        control.target,
                        Target::CustomizeAdjust {
                            field: CustomizerField::Tattoo,
                            ..
                        } | Target::CustomizeLock {
                            field: CustomizerField::Tattoo
                        }
                    )
                }),
            "Briton customizer must not draw or hit-test Tattoo chrome"
        );
    }

    /// The live profile must draw source slider/chrome pages and reject every palette-only page.
    ///
    /// This is the known-bad control for the previous implementation: the old form passes a
    /// "palette pages exist" assertion while visually failing every supplied reference capture.
    #[test]
    fn customization_screen_draws_runtime_controls_not_palette_pages() {
        let root = caer_assets::client_dep::required_caer_client_root("customization_screen");
        let mut hud = PreWorldHud::new(root);
        hud.set_screen(PreWorldScreen::CharCustomize);
        hud.ensure_loaded()
            .expect("load customization source assets");
        let quads = hud.layout((1024.0, 768.0));
        for texture in ["character_customize.tga", "buttons_wide.tga", "slider.tga"] {
            assert!(
                quads
                    .iter()
                    .any(|q| q.texture.eq_ignore_ascii_case(texture)),
                "customization layout omitted {texture}"
            );
        }
        for obsolete_palette in [
            "skin_color_palettes.tga",
            "eye_color_palettes.tga",
            "hair_colors_palettes.tga",
            "color_picker_indicator.tga",
        ] {
            assert!(
                !quads
                    .iter()
                    .any(|q| q.texture.eq_ignore_ascii_case(obsolete_palette)),
                "runtime customizer must reject legacy {obsolete_palette}"
            );
        }
        let xf = PreworldTransform::from_viewport((1024.0, 768.0));
        for row in preworld_customize::runtime_rows(true) {
            let art = preworld_hitbox::customizer_control_for(
                true,
                Target::CustomizeLock { field: row.field },
            )
            .expect("runtime lock")
            .art()
            .mapped(xf);
            assert!(
                quads.iter().any(|q| {
                    q.texture.eq_ignore_ascii_case("misc_pieces_new.tga")
                        && (q.dst.x - art.x).abs() < 0.5
                        && (q.dst.y - art.y).abs() < 0.5
                }),
                "runtime lock {:?} needs visible chrome at ({}, {})",
                row.field,
                art.x,
                art.y
            );
        }
    }

    /// The player-facing customizer seam: a pixel on every currently visible row must become
    /// the semantic action for that row and mutate only that row's product owner.  This is more
    /// discriminating than a layout snapshot: the former palette UI could still draw recognisable
    /// chrome while every click either mutated the wrong packed byte or was silently refused.
    #[test]
    fn source_customizer_pixels_reach_every_visible_product_owner() {
        use crate::preworld_appearance::AppearanceCatalog;
        use crate::preworld_product::{dispatch_preworld_action, PreWorldProductState};
        use std::sync::Arc;

        let root = caer_assets::client_dep::required_caer_client_root(
            "source_customizer_pixels_reach_every_visible_product_owner",
        );
        let catalog =
            Arc::new(AppearanceCatalog::load(&root).expect("load the retail appearance catalogue"));
        // Celt male is the supplied Hibernia reference identity and has the optional Tattoo row,
        // so it exercises the complete runtime table rather than a convenient subset.
        let mut state = PreWorldProductState {
            appearance_catalog: Some(Arc::clone(&catalog)),
            ..PreWorldProductState::default()
        };
        state.create_draft.set_race(9);
        state.create_draft.set_gender(0);
        let choices = catalog
            .choices(9, 0)
            .expect("Celt male retail appearance choices")
            .clone();
        assert!(
            !choices
                .selector_values(crate::preworld_appearance::AppearanceSelector::Tattoo)
                .is_empty(),
            "the full-row regression subject must actually have Tattoo choices"
        );

        let viewport = (1024.0, 768.0);
        let xf = PreworldTransform::from_viewport(viewport);
        let mut hud = PreWorldHud::new(root);
        hud.set_screen(PreWorldScreen::CharCustomize);
        hud.set_appearance_catalog(Some(Arc::clone(&catalog)));
        let sync_hud = |hud: &mut PreWorldHud, state: &PreWorldProductState| {
            hud.set_create_draft(&state.create_draft);
            hud.set_customizer_state(state.customizer);
        };
        sync_hud(&mut hud, &state);

        let hit = |hud: &mut PreWorldHud, target: Target, design_x: f32| {
            let control = preworld_hitbox::customizer_control_for(true, target)
                .expect("visible runtime control");
            let art = control.art();
            let (_, y) = xf.map(art.x + art.w * 0.5, art.y + art.h * 0.5);
            let (x, _) = xf.map(design_x, art.y + art.h * 0.5);
            hud.hit_action(x, y, viewport)
                .unwrap_or_else(|| panic!("{target:?} went dead at ({x}, {y})"))
        };

        for row in preworld_customize::runtime_rows(true) {
            let action = match row.widget {
                CustomizerWidget::TextSelector => {
                    let target = Target::CustomizeAdjust {
                        field: row.field,
                        dir: 1,
                    };
                    let art = preworld_hitbox::customizer_control_for(true, target)
                        .expect("runtime selector")
                        .art();
                    let action = hit(&mut hud, target, art.x + art.w * 0.5);
                    assert_eq!(
                        action,
                        PreWorldAction::CustomizeAdjust {
                            field: row.field,
                            dir: 1
                        },
                        "selector {:?} must keep its own semantic action",
                        row.field
                    );
                    action
                }
                CustomizerWidget::Slider { .. } => {
                    let target = Target::CustomizeSlider { field: row.field };
                    let art = preworld_hitbox::customizer_control_for(true, target)
                        .expect("runtime slider")
                        .art();
                    // Facial/Mood sliders have all nine positions. Skin Tone has source-owned
                    // values after the neutral tick, so choose its highest authored tick rather
                    // than inventing a colour cell that may not exist for this identity.
                    let tick = match row.field {
                        CustomizerField::SkinTone => {
                            u8::try_from(choices.skin_tones().len().min(8))
                                .expect("skin palette fits the retail slider")
                                .max(1)
                        }
                        _ => preworld_customize::SLIDER_MAX_TICK,
                    };
                    let travel = art.w - 7.0;
                    let design_x = art.x
                        + travel * f32::from(tick) / f32::from(preworld_customize::SLIDER_MAX_TICK);
                    let action = hit(&mut hud, target, design_x);
                    assert_eq!(
                        action,
                        PreWorldAction::CustomizeSlider {
                            field: row.field,
                            tick
                        },
                        "slider {:?} must preserve its clicked tick",
                        row.field
                    );
                    action
                }
            };
            let result = dispatch_preworld_action(&mut state, action);
            assert!(
                result.refused.is_none(),
                "visible {:?} action {action:?} refused: {:?}",
                row.field,
                result.refused
            );
            sync_hud(&mut hud, &state);

            let lock_target = Target::CustomizeLock { field: row.field };
            let lock_art = preworld_hitbox::customizer_control_for(true, lock_target)
                .expect("runtime lock")
                .art();
            let lock_action = hit(&mut hud, lock_target, lock_art.x + lock_art.w * 0.5);
            assert_eq!(
                lock_action,
                PreWorldAction::CustomizeToggleLock { field: row.field },
                "lock {:?} must not alias an adjacent row",
                row.field
            );
            let result = dispatch_preworld_action(&mut state, lock_action);
            assert!(
                result.refused.is_none(),
                "visible {:?} lock refused: {:?}",
                row.field,
                result.refused
            );
            assert!(
                state.customizer.is_locked(row.field),
                "lock {:?} did not persist in product state",
                row.field
            );
            sync_hud(&mut hud, &state);
        }

        // The four morph controls, Mood, and Skin Tone own distinct storage.  These exact values
        // falsify the historic Tattoo→Mood alias and the old anonymous-slider no-op path.
        assert_eq!(
            state.create_draft.eye_size, 0x88,
            "Nose/Eyes reach their nibbles"
        );
        assert_eq!(
            state.create_draft.lip_size, 0x88,
            "Lips/Jaw reach their nibbles"
        );
        assert_eq!(
            state.create_draft.mood_type,
            preworld_customize::SLIDER_MAX_TICK,
            "Mood reaches its independent byte"
        );
        assert!(
            choices
                .skin_tones()
                .contains(&(state.create_draft.eye_color & 0x0F)),
            "Skin Tone reaches an authored low-nibble value"
        );
        assert!(
            choices
                .selector_values(crate::preworld_appearance::AppearanceSelector::Face)
                .iter()
                .any(|choice| choice.index == state.create_draft.face_type),
            "Face selector reaches an authored source value"
        );
        assert!(
            choices
                .selector_values(crate::preworld_appearance::AppearanceSelector::HairStyle)
                .iter()
                .any(|choice| choice.index == state.create_draft.hair_style),
            "Hair Style selector reaches an authored source value"
        );
        assert!(
            choices
                .selector_values(crate::preworld_appearance::AppearanceSelector::Tattoo)
                .iter()
                .any(|choice| choice.index == state.customizer.tattoo_index()),
            "Tattoo remains a distinct, source-backed local selection"
        );

        // Bottom controls use the same hit/action/product seam. Default must clear visible
        // source-owned values rather than only repainting the labels.
        let default_target = Target::CustomizeReset;
        let default_art = preworld_hitbox::customizer_control_for(true, default_target)
            .expect("Default button")
            .art();
        let default_action = hit(
            &mut hud,
            default_target,
            default_art.x + default_art.w * 0.5,
        );
        assert_eq!(default_action, PreWorldAction::CustomizeReset);
        let result = dispatch_preworld_action(&mut state, default_action);
        assert!(
            result.refused.is_none(),
            "Default refused: {:?}",
            result.refused
        );
        assert_eq!(state.create_draft.customization(), Default::default());
        assert_eq!(state.customizer.tattoo_index(), 0);
    }

    /// Falsifier: SCN-02 Continué binding must come from [`hit_charcreate`], not a const.
    #[test]
    fn scn02_goes_red_without_hit_charcreate() {
        assert_eq!(
            scn02_continue_from_ui_hit(),
            Some(PreWorldAction::CharCreateContinue),
            "Continué center must hit CharCreateContinue"
        );
        // Off-control click must miss — a deleted/stubbed hit path that always returns Continue
        // would also pass the center click, so miss coords are the discriminating half.
        assert!(
            hit_charcreate(1.0, 1.0, SCN02_VIEWPORT).is_none(),
            "corner miss must not invent Continué"
        );
        let (cx, cy) = charcreate_continue_click_point(SCN02_VIEWPORT);
        assert_eq!(
            hit_charcreate(cx, cy, SCN02_VIEWPORT),
            Some(PreWorldAction::CharCreateContinue)
        );
    }

    /// **Ledger A13.** Quit raises the authored modal; it does not navigate and it does not quit.
    ///
    /// Seen red by deleting the `CharSelectQuit` arm of `apply_action`, which leaves the modal shut
    /// and puts the click straight back on the screen behind.
    #[test]
    fn quit_raises_the_authored_modal_and_the_modal_is_modal() {
        let vp = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(PathBuf::from("."));
        hud.set_screen(PreWorldScreen::CharSelect);
        assert!(!hud.quit_confirm_open(), "the modal starts shut");

        // Pressing Quit where its art is.
        let quit = preworld_hitbox::control_for(PreWorldScreen::CharSelect, Target::CharSelectQuit)
            .expect("char-select Quit");
        let (qx, qy) = quit.click_point();
        let xf = PreworldTransform::from_viewport(vp);
        let (sx, sy) = xf.map(qx, qy);
        assert_eq!(
            hud.hit_action(sx, sy, vp),
            Some(PreWorldAction::CharSelectQuit)
        );
        hud.apply_action(PreWorldAction::CharSelectQuit);
        assert!(hud.quit_confirm_open(), "Quit must raise quit_confirm.xml");
        let modal = preworld_hitbox::Modal::QuitConfirm;

        // While it is up, the character rows and the rest of the chrome behind it are dead. This is
        // what "modal" has to mean: a confirmation whose background still responds confirms nothing.
        for target in [
            Target::CharSelectPlay,
            Target::CharSelectRealm,
            Target::CharSelectOptions,
            Target::CharSlot(0),
        ] {
            let c = preworld_hitbox::control_for(PreWorldScreen::CharSelect, target)
                .expect("char-select control");
            let (cx, cy) = c.click_point();
            let (bx, by) = xf.map(cx, cy);
            assert_eq!(
                hud.hit_action(bx, by, vp),
                None,
                "{target:?} must be dead behind the Quit modal"
            );
        }

        // Its own two buttons answer where their art is, and the plate between them does not.
        for (target, action) in [
            (Target::QuitConfirmYes, PreWorldAction::QuitConfirmYes),
            (Target::QuitConfirmNo, PreWorldAction::QuitConfirmNo),
        ] {
            let c = preworld_hitbox::modal_controls(modal)
                .iter()
                .find(|c| c.target == target)
                .expect("modal control");
            let (cx, cy) = c.click_point();
            let (bx, by) = xf.map(cx, cy);
            assert_eq!(
                hud.hit_action(bx, by, vp),
                Some(action),
                "{target:?} centre"
            );

            // One pixel outside the art on each side is dead.
            let art = c.art();
            for (px, py) in [
                (art.x - 1.0, art.y + art.h * 0.5),
                (art.x + art.w, art.y + art.h * 0.5),
                (art.x + art.w * 0.5, art.y - 1.0),
                (art.x + art.w * 0.5, art.y + art.h),
            ] {
                let (ox2, oy2) = xf.map(px, py);
                assert_eq!(
                    hud.hit_action(ox2, oy2, vp),
                    None,
                    "{target:?}: ({px},{py}) is outside the art and must not act"
                );
            }
        }

        // No dismisses it and leaves us where we were.
        hud.apply_action(PreWorldAction::QuitConfirmNo);
        assert!(!hud.quit_confirm_open(), "No must dismiss the modal");
        assert_eq!(
            hud.hit_action(sx, sy, vp),
            Some(PreWorldAction::CharSelectQuit),
            "and the screen behind is live again"
        );
    }

    /// **Ledger B5.** Delete raises `delete_confirm.xml`, and the modal is modal.
    ///
    /// Delete is now an enabled control, which under the H1 rule means its whole path exists. This
    /// covers the UI half: the button routes, the form comes up, the screen behind it goes dead, and
    /// Cancel puts it back. `delete_confirm_sends_the_oracles_operation_three` covers the wire half.
    ///
    /// Seen red by setting Delete's `CHARSELECT_CHROME` entry back to `enabled: false`, which is the
    /// state it was in before this slice — `hit_action` then returns `None` and nothing is raised.
    #[test]
    fn delete_raises_the_authored_modal_and_the_modal_is_modal() {
        let vp = (1024.0, 768.0);
        let modal = preworld_hitbox::Modal::DeleteConfirm;
        let mut hud = PreWorldHud::new(PathBuf::from("."));
        hud.set_screen(PreWorldScreen::CharSelect);

        let del =
            preworld_hitbox::control_for(PreWorldScreen::CharSelect, Target::CharSelectDelete)
                .expect("char-select Delete");
        let (dx, dy) = del.click_point();
        let xf = PreworldTransform::from_viewport(vp);
        let (sx, sy) = xf.map(dx, dy);
        assert_eq!(
            hud.hit_action(sx, sy, vp),
            Some(PreWorldAction::DeleteCharacter),
            "Delete must route now that its path is complete"
        );
        hud.apply_action(PreWorldAction::DeleteCharacter);
        assert!(
            hud.delete_confirm_open(),
            "Delete must raise delete_confirm.xml"
        );
        assert!(!hud.quit_confirm_open(), "and not the other modal");

        // The screen behind is dead, including Delete itself.
        for target in [
            Target::CharSelectPlay,
            Target::CharSelectDelete,
            Target::CharSlot(0),
        ] {
            let c = preworld_hitbox::control_for(PreWorldScreen::CharSelect, target)
                .expect("char-select control");
            let (cx, cy) = c.click_point();
            let (bx, by) = xf.map(cx, cy);
            assert_eq!(
                hud.hit_action(bx, by, vp),
                None,
                "{target:?} must be dead behind the Delete modal"
            );
        }

        // Satisfy the typed confirmation first: this test is about the modal being *modal*, and
        // `delete_is_unreachable_until_the_confirmation_word_is_typed` owns the YES gate. Without
        // this the Delete control is legitimately dead and the probes below would be measuring the
        // wrong rule.
        for c in DELETE_CONFIRM_WORD.chars() {
            hud.type_into_modal(c);
        }
        assert!(hud.modal_confirmed());

        // Its own buttons answer where their art is, and one pixel out is dead.
        for (target, action) in [
            (Target::DeleteConfirmYes, PreWorldAction::DeleteConfirmYes),
            (Target::DeleteConfirmNo, PreWorldAction::DeleteConfirmNo),
        ] {
            let c = preworld_hitbox::modal_controls(modal)
                .iter()
                .find(|c| c.target == target)
                .expect("modal control");
            let (cx, cy) = c.click_point();
            let (bx, by) = xf.map(cx, cy);
            assert_eq!(
                hud.hit_action(bx, by, vp),
                Some(action),
                "{target:?} centre"
            );
            let art = c.art();
            for (px, py) in [
                (art.x - 1.0, art.y + art.h * 0.5),
                (art.x + art.w, art.y + art.h * 0.5),
                (art.x + art.w * 0.5, art.y - 1.0),
                (art.x + art.w * 0.5, art.y + art.h),
            ] {
                let (ox2, oy2) = xf.map(px, py);
                assert_eq!(
                    hud.hit_action(ox2, oy2, vp),
                    None,
                    "{target:?}: ({px},{py}) is outside the art and must not act"
                );
            }
        }

        // Cancel puts the screen back.
        hud.apply_action(PreWorldAction::DeleteConfirmNo);
        assert!(!hud.delete_confirm_open());
        assert_eq!(
            hud.hit_action(sx, sy, vp),
            Some(PreWorldAction::DeleteCharacter)
        );
    }

    /// **Ledger B5.** Confirming closes the modal — every way out of a dialog puts it away.
    ///
    /// This was missing from `apply_action` in the first working version: `DeleteConfirmYes` fell
    /// through to `_ => {}`, so the delete packet went out and the form stayed on screen, still
    /// asking the question the player had just answered. Nothing in the UI would have looked broken
    /// to a test that only checked the packet.
    ///
    /// Seen red by removing `DeleteConfirmYes` from that arm.
    #[test]
    fn confirming_or_cancelling_always_puts_the_modal_away() {
        let vp = (1024.0, 768.0);
        for (open, confirm, cancel) in [
            (
                PreWorldAction::CharSelectQuit,
                PreWorldAction::QuitConfirmYes,
                PreWorldAction::QuitConfirmNo,
            ),
            (
                PreWorldAction::DeleteCharacter,
                PreWorldAction::DeleteConfirmYes,
                PreWorldAction::DeleteConfirmNo,
            ),
        ] {
            for out in [confirm, cancel] {
                let mut hud = PreWorldHud::new(PathBuf::from("."));
                hud.set_screen(PreWorldScreen::CharSelect);
                hud.apply_action(open);
                assert!(hud.modal().is_some(), "{open:?} must raise a modal");
                hud.apply_action(out);
                assert!(
                    hud.modal().is_none(),
                    "{out:?} must put the modal away, not leave it asking again"
                );
                // And the screen behind is live again.
                let c = preworld_hitbox::control_for(
                    PreWorldScreen::CharSelect,
                    Target::CharSelectRealm,
                )
                .expect("Realm");
                let (cx, cy) = c.click_point();
                let (sx, sy) = PreworldTransform::from_viewport(vp).map(cx, cy);
                assert_eq!(
                    hud.hit_action(sx, sy, vp),
                    Some(PreWorldAction::BackToRealm),
                    "the screen behind must respond again after {out:?}"
                );
            }
        }
    }

    /// **Ledger B5.** Delete cannot be pressed until the confirmation word is typed.
    ///
    /// Eden hides the Delete button in the awaiting state rather than greying it, so this asserts
    /// both halves of the same rule: no quad at its rect, and no action from its centre. A control
    /// that draws but does not respond — or responds but does not draw — is the class A12 was about.
    ///
    /// Seen red by dropping the `filter` in `hit_action`, which lets the centre route while the
    /// button is still invisible.
    #[test]
    fn delete_is_unreachable_until_the_confirmation_word_is_typed() {
        let vp = (1024.0, 768.0);
        let modal = preworld_hitbox::Modal::DeleteConfirm;
        let xf = PreworldTransform::from_viewport(vp);
        let mut hud = PreWorldHud::new(PathBuf::from("."));
        hud.set_screen(PreWorldScreen::CharSelect);
        hud.apply_action(PreWorldAction::DeleteCharacter);

        let yes = preworld_hitbox::modal_controls(modal)
            .iter()
            .find(|c| c.target == Target::DeleteConfirmYes)
            .expect("Delete control");
        let no = preworld_hitbox::modal_controls(modal)
            .iter()
            .find(|c| c.target == Target::DeleteConfirmNo)
            .expect("Cancel control");
        let (yx, yy) = yes.click_point();
        let (yx, yy) = xf.map(yx, yy);
        let (nx, ny) = no.click_point();
        let (nx, ny) = xf.map(nx, ny);

        assert!(
            !hud.modal_confirmed(),
            "a fresh delete modal is unconfirmed"
        );
        assert_eq!(
            hud.hit_action(yx, yy, vp),
            None,
            "Delete is dead before YES"
        );
        assert_eq!(
            hud.hit_action(nx, ny, vp),
            Some(PreWorldAction::DeleteConfirmNo),
            "Cancel is live throughout"
        );

        // Typed in lower case, because caps lock must not decide whether a delete is possible.
        for c in "yes".chars() {
            assert!(hud.type_into_modal(c));
        }
        assert!(hud.modal_confirmed(), "lower-case yes must confirm");
        assert_eq!(
            hud.hit_action(yx, yy, vp),
            Some(PreWorldAction::DeleteConfirmYes),
            "Delete is live once confirmed"
        );

        // Backspace takes it away again.
        assert!(hud.backspace_modal());
        assert!(!hud.modal_confirmed());
        assert_eq!(hud.hit_action(yx, yy, vp), None);

        // A wrong word never confirms, however much is typed — and overtyping keeps the tail, so a
        // player who fumbles a key can carry on rather than having to cancel the dialog.
        for c in "NOPE".chars() {
            hud.type_into_modal(c);
        }
        assert!(!hud.modal_confirmed(), "NOPE is not YES");
        for c in "YES".chars() {
            hud.type_into_modal(c);
        }
        assert!(
            hud.modal_confirmed(),
            "typing YES after a fumble must still work"
        );

        // Non-letters are ignored rather than poisoning the buffer.
        assert!(!hud.type_into_modal('7'));
        assert!(hud.modal_confirmed());

        // Quit's confirm is unaffected — it asks for no word, so gating it would make Yes dead.
        let mut q = PreWorldHud::new(PathBuf::from("."));
        q.set_screen(PreWorldScreen::CharSelect);
        q.apply_action(PreWorldAction::CharSelectQuit);
        assert!(q.modal_confirmed(), "QuitConfirm asks for no typed word");
    }

    /// The delete dialog draws two lines before confirmation and three after, and names the
    /// character rather than repeating the question.
    #[test]
    fn the_delete_dialog_has_two_states() {
        let modal = preworld_hitbox::Modal::DeleteConfirm;
        let a = modal_lines(modal, Some("Gwaider"), false);
        let b = modal_lines(modal, Some("Gwaider"), true);
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 3);
        assert_eq!(a[0].1, "Deleting Gwaider");
        assert_eq!(b[0].1, "Deleting Gwaider");
        // The defect this replaced: the same sentence twice.
        assert_ne!(a[0].1, a[1].1, "no line may repeat another");
        assert_ne!(b[0].1, b[1].1);
        assert_ne!(b[1].1, b[2].1);
        // No selection is still a sentence, not a panic and not an invented name.
        let n = modal_lines(modal, None, false);
        assert!(!n[0].1.contains("Gwaider") && n[0].1.starts_with("Deleting"));
    }

    /// **Ledger B5.** The hidden Delete button is hidden in the *frame*, not only in the hit test.
    ///
    /// The companion to `delete_is_unreachable_until_the_confirmation_word_is_typed`, which covers
    /// routing. Eden hides rather than greys, so a drawn-but-dead button would be a different defect
    /// from the one that was fixed. Counting plate quads at the two authored rects is what separates
    /// "we skipped the draw" from "the template failed to resolve".
    ///
    /// Seen red by removing the `continue` that skips the unconfirmed Delete in `append_modal`.
    #[test]
    fn the_delete_button_is_not_drawn_until_confirmed() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "the_delete_button_is_not_drawn_until_confirmed",
        ) else {
            return;
        };
        let viewport = (1024.0, 768.0);
        let modal = preworld_hitbox::Modal::DeleteConfirm;
        let xf = PreworldTransform::from_viewport(viewport);
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::CharSelect);
        hud.apply_action(PreWorldAction::DeleteCharacter);
        hud.ensure_loaded()
            .expect("load char-select + modal chrome");

        let page = {
            let name = hud
                .button_templates
                .get("button_small")
                .map(|t| t.texture.to_ascii_lowercase())
                .expect("styles.xml declares button_small");
            let (_, member) = hud
                .texture_pages
                .get(&name)
                .expect("asset.xml maps the button_small page");
            page_texture_key(member)
        };
        let plate_at = |quads: &[UiQuad], target: Target| {
            let art = preworld_hitbox::modal_controls(modal)
                .iter()
                .find(|c| c.target == target)
                .expect("modal control")
                .art()
                .mapped(xf);
            quads.iter().any(|q| {
                q.texture.eq_ignore_ascii_case(&page)
                    && (q.dst.x - art.x).abs() < 0.5
                    && (q.dst.y - art.y).abs() < 0.5
            })
        };

        let before = hud.layout(viewport);
        assert!(
            !plate_at(&before, Target::DeleteConfirmYes),
            "Delete must not be drawn before the confirmation word is typed"
        );
        assert!(
            plate_at(&before, Target::DeleteConfirmNo),
            "Cancel is drawn throughout — without this the test passes on a blank dialog"
        );

        for c in DELETE_CONFIRM_WORD.chars() {
            hud.type_into_modal(c);
        }
        let after = hud.layout(viewport);
        assert!(
            plate_at(&after, Target::DeleteConfirmYes),
            "Delete must appear once confirmed"
        );
        assert!(plate_at(&after, Target::DeleteConfirmNo));
    }

    /// **Ledger B5.** A character row reads the way the client's does.
    ///
    /// Eden: `Gwaider the Infiltrator` / `Level 1 in City of Camelot`. CAER drew the bare name and
    /// put the **class** where the location belongs, so a row never said where a character was —
    /// while `CharacterSummary::location` had carried it from the overview packet all along.
    ///
    /// Seen red by restoring `format!("Level {} {}", level, class_name)`.
    #[test]
    fn character_rows_read_like_the_clients() {
        use caer_protocol::overview::CharacterSummary;
        let mut c = CharacterSummary {
            slot: 0,
            level: 24,
            name: "Krivette".into(),
            class_name: "Occultist".into(),
            location: "Campacorentin Forest".into(),
            ..Default::default()
        };
        let (title, sub) = character_row_lines(&c);
        assert_eq!(title, "Krivette the Occultist");
        assert_eq!(sub, "Level 24 in Campacorentin Forest");
        assert!(
            !sub.contains("Occultist"),
            "the class belongs on line 1; line 2 is where the character IS"
        );

        // Degraded honestly rather than printed with a dangling preposition or an invented place.
        c.location.clear();
        c.class_name.clear();
        let (title, sub) = character_row_lines(&c);
        assert_eq!(title, "Krivette");
        assert_eq!(sub, "Level 24");
    }

    /// The two confirm modals cannot be confused for one another.
    ///
    /// Both forms use ControlIds 1003 and 1004 and both are 250x164, so "the right rect from the
    /// wrong form" is a live failure mode here rather than a hypothetical one. The buttons sit 15px
    /// apart vertically, which is the only thing distinguishing them geometrically.
    #[test]
    fn the_two_modals_do_not_share_geometry_or_actions() {
        use preworld_hitbox::Modal;
        let quit: Vec<_> = preworld_hitbox::modal_controls(Modal::QuitConfirm)
            .iter()
            .map(|c| (c.target, c.art().y))
            .collect();
        let del: Vec<_> = preworld_hitbox::modal_controls(Modal::DeleteConfirm)
            .iter()
            .map(|c| (c.target, c.art().y))
            .collect();
        assert_ne!(
            quit, del,
            "the two forms must not resolve to the same table"
        );
        assert_eq!(
            del[0].1 - quit[0].1,
            15.0,
            "delete_confirm authors its buttons 15px below quit_confirm's"
        );
        for (t, _) in &quit {
            assert!(
                matches!(t, Target::QuitConfirmYes | Target::QuitConfirmNo),
                "{t:?} is not a Quit-modal target"
            );
        }
        for (t, _) in &del {
            assert!(
                matches!(t, Target::DeleteConfirmYes | Target::DeleteConfirmNo),
                "{t:?} is not a Delete-modal target"
            );
        }
    }

    /// Escape dismisses the front-most dialog only.
    #[test]
    fn escape_dismisses_the_quit_modal_before_the_options_menu() {
        let mut hud = PreWorldHud::new(PathBuf::from("."));
        hud.set_screen(PreWorldScreen::CharSelect);
        hud.apply_action(PreWorldAction::CharSelectOptions);
        hud.apply_action(PreWorldAction::CharSelectQuit);
        assert!(hud.options_open() && hud.quit_confirm_open());

        // This is the `||` the shell uses, in the same order.
        assert!(hud.close_modal() || hud.close_options());
        assert!(!hud.quit_confirm_open(), "the Quit modal went first");
        assert!(
            hud.options_open(),
            "the Options Menu behind it must survive one Escape"
        );
        assert!(hud.close_modal() || hud.close_options());
        assert!(!hud.options_open(), "the second Escape closes Options");
    }

    #[test]
    fn hit_login_dialog_ok_and_quit() {
        let vp = (1600.0, 1000.0);
        let (ox, oy) = login_dialog_pos(vp);
        assert_eq!(
            hit_login_dialog(ox + 60.0 + 1.0, oy + 100.0 + 1.0, vp),
            Some(PreWorldAction::LoginPlay)
        );
        assert_eq!(
            hit_login_dialog(ox + 170.0 + 1.0, oy + 100.0 + 1.0, vp),
            Some(PreWorldAction::LoginExit)
        );
    }

    #[test]
    fn hit_charcreate_name_and_race() {
        let vp = (1024.0, 768.0);
        let name = charcreate_name_rect(vp);
        assert_eq!(
            hit_charcreate(name.x + 1.0, name.y + 1.0, vp),
            Some(PreWorldAction::CharCreateFocusName)
        );
        let race0 = charcreate_race_rect(0, vp).unwrap();
        assert_eq!(
            hit_charcreate(race0.x + 1.0, race0.y + 1.0, vp),
            Some(PreWorldAction::CharCreateRace(0))
        );
        assert_eq!(race_id_for_realm(1, 0), 1);
        let male = charcreate_gender_rect(0, vp);
        assert_eq!(
            hit_charcreate(male.x + 1.0, male.y + 1.0, vp),
            Some(PreWorldAction::CharCreateGender(0))
        );
        let female = charcreate_gender_rect(1, vp);
        assert_eq!(
            hit_charcreate(female.x + 1.0, female.y + 1.0, vp),
            Some(PreWorldAction::CharCreateGender(1))
        );
    }

    #[test]
    fn minotaur_female_hit_is_dropped() {
        let vp = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharCreate);
        let mut draft = hud.create_draft.clone();
        draft.realm = 1;
        draft.set_race(19);
        hud.set_create_draft(&draft);
        let female = charcreate_gender_rect(1, vp);
        assert_eq!(
            hud.hit_action(female.x + 1.0, female.y + 1.0, vp),
            None,
            "Minotaur Female must not take a click"
        );
        let male = charcreate_gender_rect(0, vp);
        assert_eq!(
            hud.hit_action(male.x + 1.0, male.y + 1.0, vp),
            Some(PreWorldAction::CharCreateGender(0))
        );
    }

    #[test]
    fn male_celt_bainshee_hit_is_dropped() {
        let vp = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharCreate);
        let mut draft = hud.create_draft.clone();
        draft.realm = 3;
        draft.set_race(9);
        draft.set_gender(0);
        hud.set_create_draft(&draft);
        let bainshee = caer_protocol::creation_adapters::class_adapters_for(3, 9, 0)
            .into_iter()
            .find(|a| a.class_id == 39)
            .expect("Bainshee");
        let rect = charcreate_class_rect(bainshee.slot, vp).expect("class rect");
        assert_eq!(
            hud.hit_action(rect.x + 1.0, rect.y + 1.0, vp),
            None,
            "male Celt must not click Bainshee"
        );
    }

    /// **B5.** `apply_action` is not a navigator. The protocol flow owns every screen; the one
    /// thing the HUD decides for itself is whether the Options Menu overlay is up.
    ///
    /// Replaces `login_play_navigates_to_realm_then_charselect`,
    /// `continue_returns_to_charselect`, `launcher_auth_contract_login_play_is_local_navigation_only`
    /// and `preworld_navigation_covers_required_screen_transitions`, all of which asserted the
    /// second navigator. The screen sequence they described is now owned by `PreWorldFlow` and
    /// falsified there (see `realm_chosen_survives_repeated_realm_select_phase_observation`).
    #[test]
    fn hud_navigates_nowhere_options_is_an_overlay_not_a_screen() {
        for opener in [
            PreWorldAction::OpenSettings,
            PreWorldAction::CharSelectOptions,
        ] {
            let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
            hud.set_screen(PreWorldScreen::CharSelect);
            hud.apply_action(opener);
            assert!(hud.options_open(), "{opener:?} must open the Options Menu");
            // Retail keeps the character screen drawn behind the dialog. Navigating away from it
            // is what left the options panel floating on black.
            assert_eq!(
                hud.screen(),
                PreWorldScreen::CharSelect,
                "{opener:?} must not navigate — the screen underneath stays"
            );
            hud.apply_action(PreWorldAction::Options(
                crate::preworld_options::OptionsHit::Cancel,
            ));
            assert!(!hud.options_open(), "Cancel must close the dialog");
            assert_eq!(hud.screen(), PreWorldScreen::CharSelect);
        }

        // Everything the flow owns must leave the HUD exactly where it was. `ChooseRealm` is the
        // one that mattered: it used to jump to CharSelect before any overview existed, and the
        // next frame's projection snapped it back, so the click read as dead.
        for action in [
            PreWorldAction::LoginPlay,
            PreWorldAction::ChooseRealm(1),
            PreWorldAction::ChooseRealm(2),
            PreWorldAction::ChooseRealm(3),
            PreWorldAction::OpenCharCreate,
            PreWorldAction::CharCreateCancel,
            PreWorldAction::CharCreateContinue,
            PreWorldAction::BackToRealm,
            PreWorldAction::EnterWorld,
            PreWorldAction::DeleteCharacter,
        ] {
            let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
            let before = hud.screen();
            hud.apply_action(action);
            assert_eq!(
                hud.screen(),
                before,
                "{action:?} must not navigate; PreWorldFlow owns that screen"
            );
        }
    }

    /// Collect every `LabelDef` in a parsed pregame form, at any depth.
    fn label_defs(el: &caer_assets::uiskin::Element, out: &mut Vec<caer_assets::uiskin::Element>) {
        if el.tag.eq_ignore_ascii_case("LabelDef") {
            out.push(el.clone());
        }
        for c in &el.children {
            label_defs(c, out);
        }
    }

    /// **Oracle check.** [`PLATE_CAPTIONS`] must match the player's own `pregame/*.xml`: the exact
    /// string (double spaces included), the position, the size and the colour.
    ///
    /// The predecessor of this test scanned *this source file* for the literals it expected to
    /// find, which proves only that someone typed the same thing twice. This reads the client.
    ///
    /// Picking the caption out of a form needs care: `character_creation.xml`'s **first** LabelDef
    /// is "Race" at (30,115) in `med_gold`, not the title. The caption is the screen's unique
    /// `large_gold` label, and the test asserts that uniqueness rather than assuming it.
    ///
    /// **Fails** when the retail tree is absent (REQ-025) — a check that did not run must not
    /// report `pass`. Set `CAER_CLIENT` to point at the client tree.
    #[test]
    fn plate_caption_matches_client_xml() {
        let root =
            caer_assets::client_dep::required_caer_client_root("plate_caption_matches_client_xml");
        assert!(
            root.join("pregame").is_dir(),
            "CAER_CLIENT has no pregame directory: {} — plate captions can only be checked against \
             the client's own XML. REQ-025: a test that cannot run must not report pass.",
            root.display()
        );

        for (screen, file) in PLATE_CAPTION_SOURCES {
            let path = root.join("pregame").join(file);
            let xml = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let root_el =
                caer_assets::uiskin::Element::parse(&xml).unwrap_or_else(|e| panic!("{file}: {e}"));

            let mut labels = Vec::new();
            label_defs(&root_el, &mut labels);
            let gold: Vec<_> = labels
                .iter()
                .filter(|l| l.text_of("FontName").map(str::trim) == Some("large_gold"))
                .collect();
            assert_eq!(
                gold.len(),
                1,
                "{file}: expected exactly one large_gold label (the caption), found {}",
                gold.len()
            );
            let authored = gold[0];
            let cap = plate_caption(screen)
                .unwrap_or_else(|| panic!("{screen:?} has no caption in PLATE_CAPTIONS"));

            assert_eq!(
                authored.text_of("Data"),
                Some(cap.text),
                "{file}: caption text differs from the client's"
            );
            let (x, y) = authored.position();
            assert_eq!(
                (x as f32, y as f32),
                (cap.x, cap.y),
                "{file}: caption position differs from the client's"
            );
            assert_eq!(
                (
                    authored.int_of("Width").unwrap_or(0) as f32,
                    authored.int_of("Height").unwrap_or(0) as f32
                ),
                (cap.w, cap.h),
                "{file}: caption size differs from the client's"
            );
            let c = authored.color();
            let want = gold_color();
            assert_eq!(
                (c.r, c.g, c.b),
                (want.r, want.g, want.b),
                "{file}: caption colour differs from the client's"
            );
        }
    }

    /// The caption strings live in [`PLATE_CAPTIONS`] and nowhere else, so the oracle check above
    /// covers every one that gets drawn. A literal beside the table would escape it.
    #[test]
    fn no_caption_literal_lives_outside_the_table() {
        let src = include_str!("preworld.rs");
        // Derived, not spelled: a literal here would match itself in this very file.
        let stem: String = ["Select", "Your"].join("  ");
        let occurrences = src.matches(&stem).count();
        assert_eq!(
            occurrences,
            PLATE_CAPTIONS.len(),
            "expected the {} captions to appear only in PLATE_CAPTIONS, found {occurrences} \
             occurrences of {stem:?}",
            PLATE_CAPTIONS.len()
        );
    }

    /// The Random button overlaps the name edit box by 2px (authored: box 790..940, button
    /// 938..1002). The button must win that strip — it is drawn on top — and the rest of the box
    /// must still focus the name field.
    #[test]
    fn random_button_wins_its_overlap_with_the_name_box() {
        let vp = (1024.0, 768.0);
        let random = charcreate_random_rect(vp);
        let name = charcreate_name_rect(vp);
        assert!(
            random.x < name.x + name.w,
            "expected the authored overlap; geometry changed"
        );
        // Inside the overlap strip -> Random.
        assert_eq!(
            hit_charcreate(random.x + 0.5, random.y + 2.0, vp),
            Some(PreWorldAction::CharCreateRandomName),
            "overlap strip must belong to the button drawn over it"
        );
        // Left of the button -> the name field still focuses.
        assert_eq!(
            hit_charcreate(name.x + 2.0, name.y + 2.0, vp),
            Some(PreWorldAction::CharCreateFocusName),
            "the rest of the edit box must still focus"
        );
    }

    /// **H1 falsifier.** A control drawn as enabled must route, and one drawn as disabled must
    /// not. The defect this guards is a control that looks live but has no complete path —
    /// Customize was drawn with no hit route at all, and Delete routed to a refusal.
    #[test]
    fn disabled_charselect_controls_do_not_route() {
        let vp = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharSelect);

        // Existing-character Customize remains disabled until its selected-row protocol path is
        // implemented. The new-character customization form is live, but this button must not
        // borrow it and mutate a fresh draft. Delete used to be the subject of this test and was
        // enabled by B5 once its whole path existed — the H1 rule cuts both ways. The click lands
        // on Customize's own art; a point that missed would return None too and prove nothing.
        assert!(!charselect_control_enabled("Customize"));
        let (dx, dy) = charselect_click(Target::CharSelectCustomize, vp);
        assert!(
            preworld_hitbox::hit(PreWorldScreen::CharSelect, dx, dy, vp)
                .is_some_and(|c| c.target == Target::CharSelectCustomize),
            "the probe must be on Customize's art, or the None below is just a miss"
        );
        assert_eq!(
            hud.hit_action(dx, dy, vp),
            None,
            "a disabled control must not route a click"
        );

        // And Delete, now that it has one, routes to its confirmation rather than deleting.
        assert!(charselect_control_enabled("Delete"));
        let (delx, dely) = charselect_click(Target::CharSelectDelete, vp);
        assert_eq!(
            hud.hit_action(delx, dely, vp),
            Some(PreWorldAction::DeleteCharacter)
        );

        // Enabled neighbours still work, so the gate is not simply swallowing everything.
        assert!(charselect_control_enabled("Realm"));
        let (rx, ry) = charselect_click(Target::CharSelectRealm, vp);
        assert_eq!(
            hud.hit_action(rx, ry, vp),
            Some(PreWorldAction::BackToRealm)
        );
        assert!(charselect_control_enabled("Quit"));
        let (qx, qy) = charselect_click(Target::CharSelectQuit, vp);
        assert_eq!(
            hud.hit_action(qx, qy, vp),
            Some(PreWorldAction::CharSelectQuit)
        );
    }

    /// Where to press a char-select control, from the authored table.
    fn charselect_click(target: Target, vp: (f32, f32)) -> (f32, f32) {
        control_click_point(PreWorldScreen::CharSelect, target, vp)
    }

    /// Clicking a character row must produce an action.
    ///
    /// This is the defect the playtest hit: the rows were drawn at their authored positions and
    /// `hit_char_slot` agreed with them, but `hit_action` — the only thing the click handler
    /// consults before it gives up and returns — never tested them. Every row click printed
    /// "no control there" and no character could be selected on the live client.
    #[test]
    fn every_character_row_routes_a_selection() {
        let vp = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharSelect);
        for slot in 0..CHARSELECT_SLOTS {
            let (x, y) = char_slot_click_point(slot, vp);
            assert_eq!(
                hud.hit_action(x, y, vp),
                Some(PreWorldAction::SelectCharacterSlot(slot)),
                "row {slot} is drawn but does not route"
            );
        }
        // And hover answers the same question the click does, so a row that responds also lights.
        let (x, y) = char_slot_click_point(3, vp);
        hud.set_pointer(x, y, vp);
        assert_eq!(hud.hovered_slot, Some(3));
        assert_eq!(
            hud.hovered_action,
            Some(PreWorldAction::SelectCharacterSlot(3))
        );
    }

    /// The bottom-right button's label, its art state and its click must be one decision.
    ///
    /// With nothing selected it is Create; with an occupied row selected it is Play. The draw used
    /// to compare hover against `EnterWorld` unconditionally, so on a fresh screen the button
    /// could never highlight — it was asking about an action the hit test would not have produced.
    #[test]
    fn play_create_button_agrees_with_itself() {
        let vp = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharSelect);
        let (px, py) = charselect_click(Target::CharSelectPlay, vp);

        assert_eq!(
            hud.charselect_play_action(),
            PreWorldAction::OpenCharCreate,
            "nothing selected ⇒ Create"
        );
        assert_eq!(
            hud.hit_action(px, py, vp),
            Some(hud.charselect_play_action())
        );
        hud.set_pointer(px, py, vp);
        assert_eq!(hud.hovered_action, Some(hud.charselect_play_action()));

        hud.set_overview(Some(caer_protocol::overview::CharacterOverview {
            flags: 0,
            characters: vec![caer_protocol::overview::CharacterSummary {
                slot: 4,
                level: 1,
                name: "Cadellin".into(),
                location: "Camelot Hills".into(),
                class_name: "Wizard".into(),
                race_name: "Avalonian".into(),
                region: 1,
                class_id: 1,
                realm: 1,
                stats: [0; 8],
                race_gender: 0x02,
                ..Default::default()
            }],
        }));
        // An empty row stays Create even when selected; the occupied one becomes Play.
        hud.set_selected_protocol_slot(Some(0));
        assert_eq!(hud.charselect_play_action(), PreWorldAction::OpenCharCreate);
        hud.set_selected_protocol_slot(Some(4));
        assert_eq!(hud.charselect_play_action(), PreWorldAction::EnterWorld);
        assert_eq!(hud.hit_action(px, py, vp), Some(PreWorldAction::EnterWorld));
    }

    /// The Options Menu is modal and its backed controls respond where they are drawn.
    ///
    /// The bug: Options navigated to `PreWorldScreen::Settings`, whose layout draws nothing, so
    /// the character screen vanished and every click on the panel printed "no control there"
    /// because `hit_action` returned `None` for that screen unconditionally.
    #[test]
    fn options_menu_is_modal_and_its_live_controls_route() {
        use crate::preworld_options as opt;
        let vp = (1024.0, 768.0);
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharSelect);

        // Closed: the character screen answers normally.
        let (qx, qy) = charselect_click(Target::CharSelectQuit, vp);
        assert_eq!(
            hud.hit_action(qx, qy, vp),
            Some(PreWorldAction::CharSelectQuit)
        );

        hud.apply_action(PreWorldAction::CharSelectOptions);
        // Open: the screen behind it is inert.
        assert_eq!(
            hud.hit_action(qx, qy, vp),
            None,
            "the dialog is modal; Quit behind it must not fire"
        );

        // Every backed control routes at the point it is drawn, and every greyed one does not.
        for r in opt::OPTIONS_ROWS {
            let Some(id) = r.id else { continue };
            let (rx, ry, _, _) = opt::row_rect(r);
            let (x, y) = map_pregame(rx + 2.0, ry + 2.0, vp);
            let got = hud.hit_action(x, y, vp);
            match (r.kind, r.enabled) {
                (opt::RowKind::Cycle, _) => {
                    let [(ax, ay, _, _), (bx, by, _, _)] = opt::arrow_rects(r);
                    for (px, py, side) in [
                        (ax + 2.0, ay + 8.0, opt::CycleSide::Left),
                        (bx + 2.0, by + 8.0, opt::CycleSide::Right),
                    ] {
                        let (sx, sy) = map_pregame(px, py, vp);
                        let want = r
                            .enabled
                            .then_some(PreWorldAction::Options(opt::OptionsHit::Cycle(id, side)));
                        assert_eq!(hud.hit_action(sx, sy, vp), want, "{:?} {side:?}", r.label);
                    }
                }
                (opt::RowKind::Check | opt::RowKind::Radio | opt::RowKind::Link, true) => {
                    assert_eq!(
                        got,
                        Some(PreWorldAction::Options(opt::OptionsHit::Press(id))),
                        "{:?} is drawn live but does not route",
                        r.label
                    );
                }
                (opt::RowKind::Check | opt::RowKind::Radio | opt::RowKind::Link, false) => {
                    assert_eq!(got, None, "{:?} is drawn greyed but routes", r.label);
                }
                _ => {}
            }
        }

        for (accept, want) in [
            (true, opt::OptionsHit::Accept),
            (false, opt::OptionsHit::Cancel),
        ] {
            let (bx, by, _, _) = opt::button_rect(accept);
            let (x, y) = map_pregame(bx + 2.0, by + 2.0, vp);
            assert_eq!(
                hud.hit_action(x, y, vp),
                Some(PreWorldAction::Options(want))
            );
        }
    }

    /// The window-mode radios behave as a group, and the value column tracks what was clicked.
    #[test]
    fn options_radios_and_cycles_change_the_draft() {
        use crate::preworld_options::{OptionsDraft, OptionsHit, OptionsId, WindowChoice};
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_options_draft(OptionsDraft {
            resolutions: vec!["800x600".into(), "1024x768".into(), "1920x1080".into()],
            resolution: 1,
            mouselook_sensitivity: 3,
            music: 9,
            ..OptionsDraft::default()
        });
        assert_eq!(
            hud.options_draft()
                .value_text(OptionsId::Resolution)
                .as_deref(),
            Some("1024x768")
        );
        assert_eq!(
            hud.options_draft()
                .value_text(OptionsId::MusicVolume)
                .as_deref(),
            Some("Full")
        );

        hud.apply_options_hit(OptionsHit::Cycle(
            OptionsId::Resolution,
            crate::preworld_options::CycleSide::Right,
        ));
        assert_eq!(
            hud.options_draft()
                .value_text(OptionsId::Resolution)
                .as_deref(),
            Some("1920x1080")
        );
        // Wraps rather than sticking at the end.
        hud.apply_options_hit(OptionsHit::Cycle(
            OptionsId::Resolution,
            crate::preworld_options::CycleSide::Right,
        ));
        assert_eq!(
            hud.options_draft()
                .value_text(OptionsId::Resolution)
                .as_deref(),
            Some("800x600")
        );

        // Exactly one window-mode radio is ever set.
        assert!(hud.options_draft().radio_set(OptionsId::Windowed));
        assert!(!hud.options_draft().radio_set(OptionsId::FullScreenWindowed));
        hud.apply_options_hit(OptionsHit::Press(OptionsId::FullScreenWindowed));
        assert_eq!(
            hud.options_draft().window_choice(),
            WindowChoice::FullScreenWindowed
        );
        assert!(!hud.options_draft().radio_set(OptionsId::Windowed));

        // A greyed control cannot be moved even if a hit somehow reaches it.
        let before = hud.options_draft().value_text(OptionsId::ShadowQuality);
        hud.apply_options_hit(OptionsHit::Cycle(
            OptionsId::ShadowQuality,
            crate::preworld_options::CycleSide::Right,
        ));
        assert_eq!(
            hud.options_draft().value_text(OptionsId::ShadowQuality),
            before
        );
    }

    /// Draw state and hit state read the same flag, so a control cannot look live while being
    /// inert (or the reverse).
    #[test]
    fn charselect_chrome_draw_and_hit_share_one_enabled_flag() {
        for c in &CHARSELECT_CHROME {
            assert_eq!(
                charselect_control_enabled(c.label),
                c.enabled,
                "{}: lookup disagrees with the table it reads",
                c.label
            );
        }
        // Unknown controls are not enabled by default.
        assert!(!charselect_control_enabled("NoSuchControl"));
        // The table must actually contain both states, or these tests prove nothing.
        assert!(CHARSELECT_CHROME.iter().any(|c| c.enabled));
        assert!(CHARSELECT_CHROME.iter().any(|c| !c.enabled));
    }

    /// Legality is `create_validity::classify`'s job, not the renderer's.
    ///
    /// This replaces `albion_eligible_races_match_oracle_advanced_classes`, which asserted the
    /// renderer's own Albion table — including `!albion_class_allows_race(2, 16)` ("Armsman
    /// HalfOgre commented out"). The canonical career row for Armsman lists race 16 as eligible,
    /// so that test was green while encoding the opposite of the oracle: it proved an internal
    /// table agreed with itself. The table is deleted; this asserts the authority answers instead.
    #[test]
    fn renderer_holds_no_legality_table_of_its_own() {
        use caer_protocol::create_validity::{classify, Legality};
        // The case the old renderer table got wrong.
        assert_eq!(
            classify(1, 2, 16, 0),
            Legality::Allowed,
            "HalfOgre Armsman is legal"
        );
        assert_eq!(
            classify(1, 5, 3, 0),
            Legality::Forbidden,
            "Highlander Theurgist is not"
        );
        // And the module no longer exports any legality helper to drift against.
        // Needles are assembled at runtime so this list cannot match itself in the source text.
        let src = include_str!("preworld.rs");
        for name in [
            "albion_class_allows_race",
            "first_albion_class_index_for_race",
            "first_albion_race_index_for_class",
            "albion_class_id",
            "midgard_class_id",
            "hibernia_class_id",
            "class_id_for_realm",
        ] {
            let definition = format!("pub fn {name}(");
            assert!(
                !src.contains(&definition),
                "{name} came back; legality and class identity have exactly one authority"
            );
        }
    }

    /// Every Albion race button must resolve to an Albion `eRace` (OPEN_ORACLE:
    /// SoloDAoC `GameServer/GlobalConstants.cs:823`).
    ///
    /// **OWN_CAPTURE lock.** Race buttons resolve to the right `eRace`, in the captured order.
    ///
    /// This replaces `albion_race_buttons_are_all_albion_races` and
    /// `midgard_and_hibernia_race_buttons_are_partition_members`, which pinned
    /// `[1, 3, 2, 4, 13, 16, 19]` for Albion — Avalonian and Highlander transposed. Both were
    /// green while encoding the wrong button order, because they only ever compared the renderer's
    /// table to itself. Order now comes from the captured live forms via the adapter manifest.
    #[test]
    fn race_buttons_resolve_to_the_captured_race_order() {
        let expect: [(u8, [u8; 7]); 3] = [
            (1, [1, 2, 3, 4, 13, 16, 19]), // Briton Avalonian Highlander Saracen Inconnu HalfOgre Minotaur
            (2, [5, 6, 7, 8, 14, 17, 20]), // Norseman Troll Dwarf Kobold Valkyn Frostalf Minotaur
            (3, [9, 10, 11, 12, 15, 18, 21]), // Celt Firbolg Elf Lurikeen Sylvan Shar Minotaur
        ];
        for (realm, want) in expect {
            for (index, &id) in want.iter().enumerate() {
                assert_eq!(
                    race_id_for_realm(realm, u8::try_from(index).unwrap()),
                    id,
                    "realm {realm} race button {index}"
                );
            }
        }
        // The three realm sets must stay disjoint — the original defect in this area was
        // cross-realm ids on an Albion form.
        let (alb, mid, hib) = (expect[0].1, expect[1].1, expect[2].1);
        for a in alb {
            assert!(
                !mid.contains(&a) && !hib.contains(&a),
                "albion id {a} leaked cross-realm"
            );
        }
        // Race is a 5-bit field in the 1124+ create packet (`Race = b & 0x1F`).
        for (realm, ids) in expect {
            for id in ids {
                assert_eq!(
                    id & 0x1F,
                    id,
                    "realm {realm} race id {id} overflows the 5-bit field"
                );
            }
        }
        // An out-of-range slot resolves to nothing, not to a default race.
        assert_eq!(race_id_for_realm(1, 7), 0);
    }

    #[test]
    fn open_create_for_midgard_uses_norseman_not_briton() {
        assert_eq!(race_id_for_realm(2, 0), 5);
        assert_ne!(race_id_for_realm(2, 0), race_id_for_realm(1, 0));
    }

    /// All 16 authored class slots are laid out, distinct, and hit-testable.
    ///
    /// Replaces `albion_class_buttons_are_all_startable_albion_classes`, which asserted a
    /// five-entry renderer-local id table that no longer exists. Class identity now lives in
    /// `caer_protocol::creation_adapters` and is falsified there; what the renderer still owns is
    /// geometry, so that is what this tests.
    #[test]
    fn all_sixteen_authored_class_slots_are_laid_out_and_distinct() {
        let vp = (1024.0, 768.0);
        let mut seen: Vec<(i32, i32)> = Vec::new();
        for slot in 0..caer_protocol::creation_adapters::CLASS_ADAPTER_SLOTS as u8 {
            let r = charcreate_class_rect(slot, vp)
                .unwrap_or_else(|| panic!("class slot {slot} has no authored rect"));
            let key = (r.x.round() as i32, r.y.round() as i32);
            assert!(
                !seen.contains(&key),
                "class slot {slot} overlaps an earlier slot at {key:?}"
            );
            seen.push(key);
            // Every slot must be reachable by the pointer, or the control is decorative.
            assert_eq!(
                hit_charcreate(r.x + 1.0, r.y + 1.0, vp),
                Some(PreWorldAction::CharCreateClass(slot)),
                "class slot {slot} is drawn but not hit-testable"
            );
        }
        assert!(
            charcreate_class_rect(16, vp).is_none(),
            "there are exactly 16 authored class slots"
        );
    }

    /// Login is the only pre-world screen that borrows a skin window. Options used to be the
    /// other one; it is drawn from `pregame` art now and is an overlay, not a screen.
    #[test]
    fn login_is_the_only_screen_that_borrows_a_skin_window() {
        assert_eq!(PreWorldScreen::Login.skin_window(), Some("login"));
        for s in [
            PreWorldScreen::Splash,
            PreWorldScreen::Loading,
            PreWorldScreen::RealmSelect,
            PreWorldScreen::CharSelect,
            PreWorldScreen::CharCreate,
        ] {
            assert_eq!(s.skin_window(), None, "{s:?} must not borrow a skin window");
        }
    }

    #[test]
    fn hit_char_slot_picks_ui_index() {
        let ov = caer_protocol::overview::CharacterOverview {
            flags: 0,
            characters: vec![
                caer_protocol::overview::CharacterSummary {
                    slot: 2,
                    level: 50,
                    name: "Alpha".into(),
                    location: "Camelot Hills".into(),
                    class_name: "Wizard".into(),
                    race_name: "Avalonian".into(),
                    region: 1,
                    class_id: 1,
                    realm: 1,
                    stats: [0; 8],
                    race_gender: 0x02, // Avalonian male
                    ..Default::default()
                },
                caer_protocol::overview::CharacterSummary {
                    slot: 5,
                    level: 40,
                    name: "Beta".into(),
                    location: "Camelot Hills".into(),
                    class_name: "Armsman".into(),
                    race_name: "Briton".into(),
                    region: 1,
                    class_id: 2,
                    realm: 1,
                    stats: [0; 8],
                    race_gender: 0x01, // Briton male
                    ..Default::default()
                },
            ],
        };
        let vp = SCN01_VIEWPORT;
        // Click the authored rows for slots 2 and 5 — the characters live there, and the rows are
        // drawn there. The old points were proportional guesses that matched neither.
        let (x2, y2) = char_slot_click_point(2, vp);
        let hit = hit_char_slot(&ov, x2, y2, vp).expect("slot 2 row");
        assert_eq!(
            hit.slot, 2,
            "the row for slot 2 must report slot 2, not a row index"
        );
        assert_eq!(hit.row, 0, "slot 2 is the first entry in the compact list");
        let (x5, y5) = char_slot_click_point(5, vp);
        let hit = hit_char_slot(&ov, x5, y5, vp).expect("slot 5 row");
        assert_eq!(hit.slot, 5);
        assert_eq!(hit.row, 1);
        // An empty authored row hits nothing rather than the nearest character.
        let (x0, y0) = char_slot_click_point(0, vp);
        assert!(
            hit_char_slot(&ov, x0, y0, vp).is_none(),
            "slot 0 is empty; its row must not select someone else"
        );
        let via = scn01_slot_from_ui_hit(&ov).expect("SCN-01 helper must hit row 0");
        assert_eq!(via.slot, 2);
        assert_eq!(via.row, 0);
    }

    /// Falsifier leg (render): empty overview / miss coords ⇒ no slot.
    /// If `hit_char_slot` is deleted or always returns `None`, SCN-01 cannot Pass
    /// (`scn01_slot_from_ui_hit` is the only slot source for PlayScenario).
    #[test]
    fn scn01_goes_red_without_hit_char_slot() {
        let empty = caer_protocol::overview::CharacterOverview {
            flags: 0,
            characters: vec![],
        };
        assert!(
            scn01_slot_from_ui_hit(&empty).is_none(),
            "empty overview must not produce a UI slot (Pass would be fake)"
        );
        let ov = caer_protocol::overview::CharacterOverview {
            flags: 0,
            characters: vec![caer_protocol::overview::CharacterSummary {
                slot: 3,
                level: 1,
                name: "Probe".into(),
                location: String::new(),
                class_name: String::new(),
                race_name: String::new(),
                region: 1,
                class_id: 1,
                realm: 1,
                stats: [0; 8],
                race_gender: 0x01,
                ..Default::default()
            }],
        };
        let (vw, vh) = SCN01_VIEWPORT;
        // Left of the list panel — same miss a deleted hit-test path would force.
        assert!(
            hit_char_slot(&ov, vw * 0.10, vh * 0.50, SCN01_VIEWPORT).is_none(),
            "miss coords must not invent a slot"
        );
        let hit = scn01_slot_from_ui_hit(&ov).expect("occupied row must hit");
        assert_eq!(
            hit.slot, 3,
            "slot must come from overview via hit_char_slot, not row index"
        );
    }

    #[test]
    fn loads_charselect_background_when_client_present() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "loads_charselect_background_when_client_present",
        ) else {
            return;
        };
        // Keep `root` (the ClientDep guard) alive for the whole body — its Drop is the
        // completion marker — and hand the HUD an owned copy of the path.
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::CharSelect);
        hud.ensure_loaded().expect("load charselect art");
        let quads = hud.layout((1600.0, 1000.0));
        assert!(
            !quads.is_empty(),
            "charselect must produce background quads"
        );
        assert!(
            quads
                .iter()
                .any(|q| q.texture.eq_ignore_ascii_case("character_selection.tga")),
            "expected character_selection.tga quad"
        );
    }

    // --- E6/B3 slice 2: the stats dialog's authored pixels ------------------------------------

    const STATS_VP: (f32, f32) = (1024.0, 768.0);

    /// The stats window is a real skin frame, not a black fill that happens to carry the right
    /// labels. This reaches from `styles.xml` through `asset.xml` and checks every one of the
    /// nine source crops in the submitted layout.
    ///
    /// Seen red by removing either `ensure_nine_slice_page` (the page never reaches the HUD) or
    /// the `append_nine_slice` call (the page loads but no source patch is submitted).
    #[test]
    fn stats_dialog_draws_its_retail_nine_slice_frame() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "stats_dialog_draws_its_retail_nine_slice_frame",
        ) else {
            return;
        };
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::CharStats);
        hud.ensure_loaded()
            .expect("load customizer, stats chrome, and dialog frame");

        let frame = hud
            .nine_slices
            .get("dlg_background_noresize")
            .cloned()
            .expect("retail styles.xml declares the stats frame");
        let page = {
            let (_, member) = hud
                .texture_pages
                .get(&frame.texture.to_ascii_lowercase())
                .expect("retail asset.xml maps the dialog page");
            page_texture_key(member)
        };
        assert!(
            hud.textures.contains_key(&page),
            "{page} must be resident before the frame can be drawn"
        );

        let quads = hud.layout(STATS_VP);
        let (ox, oy) = preworld_hitbox::stats_dialog_origin();
        let (dw, dh) = preworld_hitbox::STATS_DIALOG_SIZE;
        let widths = [frame.left_width as f32, 0.0, frame.right_width as f32];
        let heights = [frame.top_height as f32, 0.0, frame.bottom_height as f32];
        let dst_widths = [
            widths[0],
            dw - frame.left_width as f32 - frame.right_width as f32,
            widths[2],
        ];
        let dst_heights = [
            heights[0],
            dh - frame.top_height as f32 - frame.bottom_height as f32,
            heights[2],
        ];
        let dst_x = [ox, ox + dst_widths[0], ox + dw - dst_widths[2]];
        let dst_y = [oy, oy + dst_heights[0], oy + dh - dst_heights[2]];

        for row in 0..3 {
            for column in 0..3 {
                let index = row * 3 + column;
                let (src_x, src_y) = frame.patches[index];
                let source = Rect::new(
                    src_x as f32,
                    src_y as f32,
                    [frame.left_width, frame.middle_width, frame.right_width][column] as f32,
                    [frame.top_height, frame.middle_height, frame.bottom_height][row] as f32,
                );
                let destination = map_pregame_rect(
                    dst_x[column],
                    dst_y[row],
                    dst_widths[column],
                    dst_heights[row],
                    STATS_VP,
                );
                assert!(
                    quads.iter().any(|q| {
                        q.texture.eq_ignore_ascii_case(&page)
                            && q.src == source
                            && q.dst == destination
                    }),
                    "missing frame patch {index} src={source:?} dst={destination:?}"
                );
            }
        }
    }

    /// Every authored stats control responds at its own pixels, and hover agrees with click.
    ///
    /// The historical failure shape (ledger A12, run forward) is chrome that is visible in one
    /// place and live in another. One mapping — [`stats_dialog_action`] — feeds both the draw's
    /// hover and the hit arm, so the assertion here is that the mapping is total over the table.
    #[test]
    fn every_stats_dialog_control_routes_at_its_authored_pixel() {
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharStats);
        let table = preworld_hitbox::controls(PreWorldScreen::CharStats);
        assert_eq!(table.len(), 18, "8 rows x 2 arrows + reset + optimize");
        for c in table {
            let want = stats_dialog_action(c.target);
            assert!(want.is_some(), "{:?} has no action", c.target);
            let (x, y) = control_click_point(PreWorldScreen::CharStats, c.target, STATS_VP);
            assert_eq!(
                hud.hit_action(x, y, STATS_VP),
                want,
                "{:?} is transcribed but does not route",
                c.target
            );
            hud.set_pointer(x, y, STATS_VP);
            assert_eq!(
                hud.hovered_action, want,
                "{:?} routes but does not light",
                c.target
            );
        }
    }

    /// `character_customize_stats.xml` declares `<CloseButton>true</CloseButton>`, so its
    /// generic window close affordance must be live even though it has no per-control `ControlId`.
    /// The visible Adjust Attributes chrome beneath the modal is the player's other safe return
    /// route; it closes the modal, while the underlying Continue remains blocked.
    ///
    /// This is deliberately driven through [`PreWorldHud::hit_action`] rather than calling the
    /// flow directly.  The historical defect was exactly that the flow knew how to dismiss the
    /// modal while the real pointer path had no way to produce `StatsDismiss`.
    #[test]
    fn stats_modal_exposes_source_close_and_visible_return_route() {
        use crate::preworld_flow::{flow_event_for, FlowEvent, PreWorldFlow};
        use caer_protocol::session::SessionPhase;

        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharStats);
        let xf = PreworldTransform::from_viewport(STATS_VP);
        let (ox, oy) = preworld_hitbox::stats_dialog_origin();
        let (dw, _) = preworld_hitbox::STATS_DIALOG_SIZE;
        // Generic WindowManager semantics: CloseButton=true, no title bar => its 12px fallback
        // square at the top-right of the source form.
        let close = xf.map(ox + dw - 6.0, oy + 6.0);
        assert_eq!(
            hud.hit_action(close.0, close.1, STATS_VP),
            Some(PreWorldAction::StatsDismiss),
            "the source-declared close affordance must not leave the modal trapped"
        );

        let return_to_customize = control_click_point(
            PreWorldScreen::CharCustomize,
            Target::CustomizeStats,
            STATS_VP,
        );
        let action = hud.hit_action(return_to_customize.0, return_to_customize.1, STATS_VP);
        assert_eq!(
            action,
            Some(PreWorldAction::StatsDismiss),
            "the visible Adjust Attributes control must close its already-open modal"
        );

        let blocked_continue = control_click_point(
            PreWorldScreen::CharCustomize,
            Target::CustomizeAdvance,
            STATS_VP,
        );
        assert_eq!(
            hud.hit_action(blocked_continue.0, blocked_continue.1, STATS_VP),
            None,
            "the modal must not leak the underlying Continue action"
        );

        let mut flow = PreWorldFlow::from_observed_phase(0, SessionPhase::CharacterSelect);
        assert!(flow.on_event(FlowEvent::OpenCreate));
        assert!(flow.on_event(FlowEvent::CustomizeOpened));
        assert!(flow.on_event(FlowEvent::StatsOpened));
        assert_eq!(
            flow_event_for(action.expect("visible return route")),
            Some(FlowEvent::StatsDismissed)
        );
        assert!(flow.on_event(FlowEvent::StatsDismissed));
        assert_eq!(flow.screen(), Some(PreWorldScreen::CharCustomize));
    }

    /// Labels are not hit areas (the A12 guard): a click on a stat name, a value or the
    /// description pane resolves to nothing, and the dead space between the rows stays dead.
    #[test]
    fn stats_dialog_labels_and_gaps_route_nothing() {
        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharStats);
        let (ox, oy) = preworld_hitbox::stats_dialog_origin();
        let xf = PreworldTransform::from_viewport(STATS_VP);
        let at = |x: f32, y: f32| {
            let (sx, sy) = xf.map(x, y);
            hud.hit_action(sx, sy, STATS_VP)
        };
        // Stat-name label of row 0 (318..438, y 34..50) — drawn text, no hit area.
        assert_eq!(at(ox + 360.0, oy + 40.0), None, "stat name must not route");
        // The description textarea.
        assert_eq!(at(ox + 100.0, oy + 100.0), None, "textarea must not route");
        // The gap between row 0's arrows (y 34..52) and row 1's (y 54..72).
        assert_eq!(
            at(ox + 300.0, oy + 53.0),
            None,
            "inter-row gap must not route"
        );
    }

    /// The full seam, clicked: authored pixels → action → draft → customizer Continue packet.
    ///
    /// Clicks STR's plus arrow through [`PreWorldHud::hit_action`] and
    /// [`crate::preworld_product::dispatch_preworld_action`] — the exact path a live click
    /// takes. The escalation costs (10 points at 1, then 2, then 3) mean 18 clicks spend 29 of
    /// 30: the 19th must be refused by the pool, not silently clamped.
    #[test]
    fn stats_dialog_clicks_move_the_draft_and_customizer_continue_mints_one_character() {
        use crate::live::LiveCommand;
        use crate::preworld_flow::{FlowEvent, PreWorldFlow};
        use crate::preworld_product::{
            apply_create_name_input, dispatch_preworld_action, PreWorldProductState,
        };
        use caer_protocol::session::SessionPhase;
        use caer_protocol::starting_stats;

        let mut hud = PreWorldHud::new(PathBuf::from("/nonexistent"));
        hud.set_screen(PreWorldScreen::CharStats);
        let mut state = PreWorldProductState::default();
        let mut flow = PreWorldFlow::from_observed_phase(0, SessionPhase::RealmSelect);

        // Walk to the stats modal the legal way: realm → overview → create → name → customize →
        // Adjust Attributes.
        dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(1));
        flow.on_event(FlowEvent::RealmChosen(1));
        state.overview = None;
        flow.on_event(FlowEvent::OverviewReady);
        dispatch_preworld_action(&mut state, PreWorldAction::OpenCharCreate);
        flow.on_event(FlowEvent::OpenCreate);
        dispatch_preworld_action(&mut state, PreWorldAction::CharCreateRace(0));
        dispatch_preworld_action(&mut state, PreWorldAction::CharCreateClass(0));
        dispatch_preworld_action(&mut state, PreWorldAction::CharCreateGender(0));
        apply_create_name_input(&mut state, "Walker");
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateContinue);
        assert!(r.refused.is_none());
        flow.on_event(FlowEvent::CustomizeOpened);
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeStats);
        assert!(r.refused.is_none());
        flow.on_event(FlowEvent::StatsOpened);
        assert_eq!(flow.screen(), Some(PreWorldScreen::CharStats));

        let base = starting_stats::race_base(state.create_draft.race, 0);
        let plus0 = control_click_point(
            PreWorldScreen::CharStats,
            Target::StatsAdjust { stat: 0, dir: 1 },
            STATS_VP,
        );
        let minus0 = control_click_point(
            PreWorldScreen::CharStats,
            Target::StatsAdjust { stat: 0, dir: -1 },
            STATS_VP,
        );
        let (sx, sy) = plus0;
        let (mx, my) = minus0;

        // The form opens with all 30 points pre-allocated (the stub draft must satisfy the
        // exactly-30 validation rule). Drain STR to its race base through the minus arrow.
        for click in 0..40 {
            if state.create_draft.stats[0] == base {
                break;
            }
            let Some(action) = hud.hit_action(mx, my, STATS_VP) else {
                panic!("minus arrow went dead on click {click}");
            };
            let r = dispatch_preworld_action(&mut state, action);
            assert!(r.refused.is_none(), "drain click {click} refused");
        }
        assert_eq!(
            state.create_draft.stats[0], base,
            "minus must stop at the race base"
        );

        // Ten plus clicks: the freed 10 points each cost 1 below +10, so all ten apply.
        for click in 0..10 {
            let Some(action) = hud.hit_action(sx, sy, STATS_VP) else {
                panic!("plus arrow went dead on click {click}");
            };
            assert_eq!(action, PreWorldAction::StatsAdjust { stat: 0, dir: 1 });
            let r = dispatch_preworld_action(&mut state, action);
            assert!(
                r.refused.is_none(),
                "click {click} refused: {:?}",
                r.refused
            );
        }
        assert_eq!(
            state.create_draft.stats[0],
            base + 10,
            "ten clicks must land on the draft"
        );
        assert_eq!(
            starting_stats::total_spent(state.create_draft.race, &state.create_draft.stats),
            starting_stats::MAX_STARTING_BONUS_POINTS,
            "the pool is exactly full again"
        );
        // KNOWN-BAD CONTROL: the old fail-closed hit arm (CharStats => None) would return None
        // here, silently dropping every click. The pool refusal must come from the allocation
        // rules, not from a dead control. The 11th point would cost 2 and the pool is full.
        let r =
            dispatch_preworld_action(&mut state, PreWorldAction::StatsAdjust { stat: 0, dir: 1 });
        assert!(
            r.refused.is_some(),
            "the full pool must refuse an eleventh raise"
        );
        assert_eq!(
            state.create_draft.stats[0],
            base + 10,
            "refusal mutated nothing"
        );

        // Reset restores every base through the same pixels.
        let reset = control_click_point(PreWorldScreen::CharStats, Target::StatsReset, STATS_VP);
        let (rx, ry) = reset;
        let action = hud.hit_action(rx, ry, STATS_VP).expect("reset must route");
        let r = dispatch_preworld_action(&mut state, action);
        assert!(r.refused.is_none());
        assert_eq!(
            state.create_draft.stats,
            {
                let mut bases = [0u8; 8];
                for (i, s) in bases.iter_mut().enumerate() {
                    *s = starting_stats::race_base(state.create_draft.race, i);
                }
                bases
            },
            "reset must restore every race base"
        );

        // Optimize is source control 1021, a distinct local event. It repopulates the observed
        // +10 STR/CON/DEX allocation and must never place a character-create packet on the wire.
        let optimize =
            control_click_point(PreWorldScreen::CharStats, Target::StatsOptimize, STATS_VP);
        let action = hud
            .hit_action(optimize.0, optimize.1, STATS_VP)
            .expect("Optimize must route");
        assert_eq!(action, PreWorldAction::StatsOptimize);
        let r = dispatch_preworld_action(&mut state, action);
        assert!(r.refused.is_none(), "Optimize refused: {:?}", r.refused);
        assert!(r.commands.is_empty(), "Optimize must stay local");
        assert!(
            starting_stats::validate_distribution(
                state.create_draft.race,
                &state.create_draft.stats
            )
            .is_ok(),
            "Optimize must return the draft to the DOL-valid 30-point distribution"
        );

        // Closing the source modal returns to the underlying customizer. Its Continue is the one
        // creation boundary, then the latch refuses a second Continue.
        flow.on_event(FlowEvent::StatsDismissed);
        assert_eq!(flow.screen(), Some(PreWorldScreen::CharCustomize));
        state.overview = Some(caer_protocol::overview::CharacterOverview {
            flags: 0,
            characters: Vec::new(),
        });
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(r.refused.is_none(), "Continue refused: {:?}", r.refused);
        let draft = match r.commands.as_slice() {
            [LiveCommand::CreateCharacter { draft }] => draft.clone(),
            other => panic!("expected exactly one CreateCharacter, got {other:?}"),
        };
        assert_eq!(draft.name, "Walker");
        assert_eq!(
            draft.stats, state.create_draft.stats,
            "the wire must carry what the dialog last showed"
        );
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(
            r.refused.is_some(),
            "a second Continue must be refused by the creation latch"
        );
        flow.on_event(FlowEvent::CreateAccepted);
        assert_eq!(flow.screen(), Some(PreWorldScreen::CharSelect));
    }

    /// Native first-run regression: clicking Optimize repeatedly redrew the exact same stats
    /// dialog, but the old renderer created a fresh native GPU buffer for every painter run on
    /// every redraw. NVIDIA eventually terminated the client with SIGSEGV before Rust could emit
    /// a panic. Exercise the retail hit pixel, product dispatch, HUD layout, upload, and GPU pass
    /// together so the actual path stays warm after its first frame.
    #[test]
    fn repeated_stats_optimize_redraws_reuse_the_native_ui_buffers() {
        let Some(root) = caer_assets::client_dep::require_caer_client(
            "repeated_stats_optimize_redraws_reuse_the_native_ui_buffers",
        ) else {
            return;
        };
        let _gpu_lock = if crate::gpu_init::gpu_test_serialize_enabled() {
            None
        } else {
            Some(crate::gpu_init::lock_gpu_device().expect("test device lock"))
        };
        let mut gpu = pollster::block_on(crate::gpu::Gpu::new_headless(256, 256, 1_000.0))
            .expect("headless GPU init");
        let mut hud = PreWorldHud::new(root.to_path_buf());
        hud.set_screen(PreWorldScreen::CharStats);
        hud.ensure_loaded().expect("load retail stats form");

        let mut state = crate::preworld_product::PreWorldProductState::default();
        state.create_draft.set_race(9); // Celt, the supplied Hibernia reference subject.
        state.create_draft.reset_stats_to_race_bases();
        let optimize =
            control_click_point(PreWorldScreen::CharStats, Target::StatsOptimize, STATS_VP);
        // The reported desktop failure arrived after only a handful of Optimize presses.  Keep
        // this substantially above that observed threshold so a future pool regression cannot
        // hide behind a one-frame or ten-frame smoke test.
        const STRESS_REDRAWS: usize = 64;
        let mut warm_allocations = None;
        for redraw in 0..STRESS_REDRAWS {
            let action = hud
                .hit_action(optimize.0, optimize.1, STATS_VP)
                .expect("retail Optimize pixel must keep routing");
            assert_eq!(action, PreWorldAction::StatsOptimize);
            let result = crate::preworld_product::dispatch_preworld_action(&mut state, action);
            assert!(result.refused.is_none(), "Optimize {redraw}: {result:?}");
            assert!(result.commands.is_empty(), "Optimize stays local");

            hud.set_create_draft(&state.create_draft);
            hud.render(&mut gpu, STATS_VP);
            let frame = gpu
                .render_to_rgba_pass(0, crate::gpu::FramePass::PreWorldUi)
                .expect("stats redraw must complete a native GPU pass");
            assert_eq!(frame.len(), 256 * 256 * 4);
            let allocations = gpu.ui_buffer_allocation_count();
            match warm_allocations {
                Some(warm) => assert_eq!(
                    allocations, warm,
                    "Optimize redraw {redraw} recreated UI buffers instead of reusing the warm pool"
                ),
                None => warm_allocations = Some(allocations),
            }
        }
        assert!(
            warm_allocations.unwrap_or_default() > 0,
            "stats form must submit UI runs"
        );
    }
}
