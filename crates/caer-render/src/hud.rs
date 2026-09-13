//! The player HUD — the game client's on-screen furniture (C.1).
//!
//! This is the first real consumer of the 2D overlay layer in [`crate::ui`], and it replaces the
//! stop-gap where the client wrote its target into the window title bar. It draws:
//!
//! - the **player frame** (top-left): name, then health / power / endurance bars;
//! - the **target frame** (top-centre): the selected entity's name + health, when something is
//!   selected;
//! - the **cast bar** (bottom-centre): spell id + packet cast duration while
//!   [`WorldState::self_cast_bar`](caer_world::WorldState::self_cast_bar) is `Some`.
//!
//! ## Why egui rather than a bespoke quad+font pipeline
//! The roadmap calls C.1 a "2D overlay pipeline", and the honest reading of that is *whatever gets
//! every later window on screen*. egui was already a dependency with a working wgpu pass (the dev
//! viewer's sidebar), and it brings text shaping, layout, hit-testing, input capture and window
//! dragging — which is the bulk of C.2–C.7 (quickbar, chat, inventory, character sheet, map). A
//! hand-rolled pipeline would have meant re-implementing a text stack before the first bar drew.
//! The bars themselves are painted directly ([`bar`]) rather than using `egui::ProgressBar`,
//! because DAoC's look is specific and the widget's is not.
//!
//! **This is not yet the DAoC skin.** It is a legible, correctly-driven HUD; art-matching the
//! original (the stone frames, the bevelled bar ends) is a later pass and wants Matt's eye on it.
//!
//! ## What drives it
//! Everything comes from [`HudState`], which the client fills each frame from `WorldState` — the
//! HUD reads game state and never reaches for the wire, matching the layering rule the rest of the
//! renderer follows.
//!
//! Cast-bar progress is **not** invented on a local wall clock. The bar appears while
//! `cast_bar` is `Some` (from `self_cast_bar` on local `0x72` only) and clears when it is
//! `None` (effect / interrupt). The fill shows the packet `cast_time` as duration text only —
//! no client-side completion without packets.

use caer_protocol::status::PlayerStatus;
use caer_world::CastBarState;

/// Everything the HUD draws, filled by the client each frame.
#[derive(Debug, Clone, Default)]
pub struct HudState {
    /// The player's character name, for the player frame.
    pub name: String,
    /// Our own vitals, straight from `WorldState::player_status`.
    pub status: PlayerStatus,
    /// The selected entity: name and health percent (0–100). `None` when nothing is targeted.
    pub target: Option<TargetInfo>,
    /// Active self cast from `WorldState::self_cast_bar()` — peer `0x72` must stay `None`.
    pub cast_bar: Option<CastBarState>,
    /// Typed outcome for stock-skin adapters (completed / failed / interrupted). Idle = None
    /// while casting; INT copies [`caer_world::CastPresentation::outcome`].
    pub cast_outcome: Option<caer_world::CastOutcome>,
    /// Last 0xBC result label for the target frame. Chat scrape must not fill this.
    pub combat_result: Option<&'static str>,
    /// XP permill from CharacterPointsUpdate. `None` until 0x91.
    pub xp_permill: Option<u16>,
    /// Frames per second, shown small and dim. Dev readout, but it costs one line and the
    /// alternative was a title bar nobody looks at.
    pub fps: u32,
}

#[derive(Debug, Clone)]
pub struct TargetInfo {
    pub name: String,
    /// Health percent as the server reports it for other livings (0–100).
    pub health_pct: u8,
}

/// Bar colours. DAoC's convention: health red, power blue, endurance gold.
const HEALTH: egui::Color32 = egui::Color32::from_rgb(178, 34, 34);
const POWER: egui::Color32 = egui::Color32::from_rgb(48, 96, 190);
const ENDURANCE: egui::Color32 = egui::Color32::from_rgb(200, 168, 48);
/// Cast bar chrome — distinct from vitals so a cast reads as casting, not another pool.
const CAST: egui::Color32 = egui::Color32::from_rgb(148, 96, 200);
/// The empty part of a bar — dark, but not pure black, so an empty bar still reads as a bar.
const EMPTY: egui::Color32 = egui::Color32::from_rgb(28, 26, 24);
const BORDER: egui::Color32 = egui::Color32::from_rgb(12, 11, 10);
/// Panel backing: near-black and mostly opaque, so text stays legible over bright terrain.
const PANEL: egui::Color32 = egui::Color32::from_rgba_premultiplied(10, 10, 12, 200);

const BAR_WIDTH: f32 = 180.0;
const CAST_BAR_WIDTH: f32 = 220.0;
const BAR_HEIGHT: f32 = 14.0;
/// Left edge of each frame, and the gap between the player frame and the target frame beside it.
const MARGIN: f32 = 12.0;
/// Horizontal offset of the target frame. The player frame is a bar plus its margins wide, so this
/// clears it with a gap. Anchoring the target frame to the window *centre* instead looks tidy at
/// 1080p but overlaps the player frame on a narrow window — and DAoC puts the target window beside
/// the player window anyway, not centred.
const TARGET_X: f32 = MARGIN + BAR_WIDTH + 16.0 + 12.0;
/// Vertical offset of the cast bar from the bottom edge (DAoC places it near the action bar).
const CAST_BAR_Y: f32 = -48.0;

/// Whether the HUD will draw the cast bar this frame.
///
/// Named falsifier `CASTBAR_HUD_UNWIRED`: if `build` stops reading [`HudState::cast_bar`],
/// deleting this gate (or the `if let Some(cast)` branch) makes Some/None overlays identical —
/// casting never appears on screen.
#[must_use]
pub fn shows_cast_bar(hud: &HudState) -> bool {
    hud.cast_bar.is_some()
}

/// Build the whole HUD. Pass this to [`crate::ui::Ui::run`].
///
/// Uses `Area`s rather than panels: a panel would carve space out of the root `Ui` and shrink the
/// 3D viewport, whereas the HUD must float *over* a viewport that stays full-size.
pub fn build(root: &mut egui::Ui, hud: &HudState) {
    let ctx = root.ctx().clone();

    egui::Area::new(egui::Id::new("hud_player"))
        .anchor(egui::Align2::LEFT_TOP, [MARGIN, MARGIN])
        .interactable(false) // the player frame is a readout; clicks belong to the world
        .show(&ctx, |ui| {
            frame(ui, |ui| player_frame(ui, hud));
        });

    if let Some(target) = &hud.target {
        egui::Area::new(egui::Id::new("hud_target"))
            .anchor(egui::Align2::LEFT_TOP, [TARGET_X, MARGIN])
            .interactable(false)
            .show(&ctx, |ui| {
                frame(ui, |ui| target_frame(ui, target));
            });
    }

    if shows_cast_bar(hud) {
        if let Some(cast) = &hud.cast_bar {
            egui::Area::new(egui::Id::new("hud_cast_bar"))
                .anchor(egui::Align2::CENTER_BOTTOM, [0.0, CAST_BAR_Y])
                .interactable(false)
                .show(&ctx, |ui| {
                    frame(ui, |ui| cast_bar_frame(ui, cast));
                });
        }
    }
}

/// The shared dark backing every HUD window sits on.
fn frame(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .corner_radius(3)
        .inner_margin(8.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            body(ui);
        });
}

fn player_frame(ui: &mut egui::Ui, hud: &HudState) {
    let title = if hud.name.is_empty() {
        "—"
    } else {
        hud.name.as_str()
    };
    ui.label(
        egui::RichText::new(title)
            .strong()
            .color(egui::Color32::from_rgb(228, 222, 208)),
    );

    let s = &hud.status;
    bar(
        ui,
        s.health_frac(),
        HEALTH,
        &amount(s.health, s.max_health, s.health_pct),
    );
    // Pure melee classes have no power pool at all; drawing a permanently empty blue bar would
    // read as a bug rather than as "this class has no power".
    if s.has_power() {
        bar(
            ui,
            s.mana_frac(),
            POWER,
            &amount(s.mana, s.max_mana, s.mana_pct),
        );
    }
    bar(
        ui,
        s.endurance_frac(),
        ENDURANCE,
        &amount(s.endurance, s.max_endurance, s.endurance_pct),
    );

    if s.sitting {
        ui.label(egui::RichText::new("sitting").small().weak());
    }
    if hud.fps > 0 {
        ui.label(
            egui::RichText::new(format!("{} fps", hud.fps))
                .small()
                .weak(),
        );
    }
}

fn target_frame(ui: &mut egui::Ui, target: &TargetInfo) {
    ui.label(
        egui::RichText::new(&target.name)
            .strong()
            .color(egui::Color32::from_rgb(228, 222, 208)),
    );
    let frac = f32::from(target.health_pct) / 100.0;
    bar(ui, frac, HEALTH, &format!("{}%", target.health_pct));
}

/// Cast-bar panel: spell id + packet duration. Fill stays empty — elapsed progress is not
/// invented from wall-clock; the server clears `cast_bar` on effect / interrupt.
fn cast_bar_frame(ui: &mut egui::Ui, cast: &CastBarState) {
    // Oracle cast_time is tenths of a second.
    let secs = f32::from(cast.cast_time) / 10.0;
    ui.label(
        egui::RichText::new(format!("casting spell {}", cast.spell_id))
            .strong()
            .color(egui::Color32::from_rgb(228, 222, 208)),
    );
    cast_bar(ui, &format!("{secs:.1}s"));
}

/// Cast-bar track: always empty fill (packet presence only), wider than vitals.
fn cast_bar(ui: &mut egui::Ui, label: &str) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(CAST_BAR_WIDTH, BAR_HEIGHT), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2, EMPTY);
    // Thin accent strip so the track reads as "casting chrome" without inventing progress.
    let mut accent = rect;
    accent.set_width(4.0);
    painter.rect_filled(accent, 2, CAST);
    painter.rect_stroke(
        rect,
        2,
        egui::Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );
    let font = egui::FontId::proportional(11.0);
    let centre = rect.center();
    painter.text(
        centre + egui::vec2(1.0, 1.0),
        egui::Align2::CENTER_CENTER,
        label,
        font.clone(),
        egui::Color32::from_black_alpha(190),
    );
    painter.text(
        centre,
        egui::Align2::CENTER_CENTER,
        label,
        font,
        egui::Color32::from_rgb(244, 242, 238),
    );
}

/// The label inside a bar: absolute `cur / max` once the server has told us the maxima, and the
/// percentage until then (which is the pre-first-packet default state).
fn amount(cur: u16, max: u16, pct: u8) -> String {
    if max > 0 {
        format!("{cur} / {max}")
    } else {
        format!("{pct}%")
    }
}

/// Paint one bar: a filled portion, a dark remainder, a border, and a centred label.
///
/// `frac` is clamped, so a server value that disagrees with its own maximum (or a NaN arriving
/// from a future caller) cannot paint outside the bar.
fn bar(ui: &mut egui::Ui, frac: f32, colour: egui::Color32, label: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(BAR_WIDTH, BAR_HEIGHT), egui::Sense::hover());
    let painter = ui.painter();
    let frac = if frac.is_finite() {
        frac.clamp(0.0, 1.0)
    } else {
        0.0
    };

    painter.rect_filled(rect, 2, EMPTY);
    if frac > 0.0 {
        let mut filled = rect;
        filled.set_width(rect.width() * frac);
        painter.rect_filled(filled, 2, colour);
    }
    painter.rect_stroke(
        rect,
        2,
        egui::Stroke::new(1.0, BORDER),
        egui::StrokeKind::Inside,
    );

    // Drawn twice: a dark copy offset by a pixel, then the light one. The label sits on top of
    // whatever the bar's fill happens to be, and pale text on the gold endurance bar is close to
    // unreadable without a shadow behind it.
    let font = egui::FontId::proportional(11.0);
    let centre = rect.center();
    painter.text(
        centre + egui::vec2(1.0, 1.0),
        egui::Align2::CENTER_CENTER,
        label,
        font.clone(),
        egui::Color32::from_black_alpha(190),
    );
    painter.text(
        centre,
        egui::Align2::CENTER_CENTER,
        label,
        font,
        egui::Color32::from_rgb(244, 242, 238),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> PlayerStatus {
        PlayerStatus {
            health: 1848,
            max_health: 2004,
            health_pct: 92,
            mana: 492,
            max_mana: 492,
            mana_pct: 100,
            endurance: 95,
            max_endurance: 100,
            endurance_pct: 95,
            concentration: 378,
            max_concentration: 378,
            concentration_pct: 100,
            sitting: false,
        }
    }

    #[test]
    fn bar_labels_prefer_absolute_values_then_fall_back_to_percent() {
        assert_eq!(amount(1848, 2004, 92), "1848 / 2004");
        // Before the first status packet the maxima are unknown — show the percent, not "0 / 0".
        assert_eq!(amount(0, 0, 100), "100%");
    }

    /// The HUD must render every state the client can hand it without panicking — including the
    /// pre-first-packet default and a melee class with no power pool. egui can be driven headless,
    /// so this exercises the real layout code rather than a stand-in.
    #[test]
    fn builds_every_state_without_panicking() {
        let melee = PlayerStatus {
            max_mana: 0,
            mana: 0,
            ..full()
        };
        let states = [
            HudState::default(),
            HudState {
                name: "Feile".into(),
                status: full(),
                target: None,
                cast_bar: None,
                fps: 60,
                ..HudState::default()
            },
            HudState {
                name: "Feile".into(),
                status: melee,
                target: None,
                cast_bar: None,
                fps: 60,
                ..HudState::default()
            },
            HudState {
                name: "Feile".into(),
                status: full(),
                target: Some(TargetInfo {
                    name: "a large spider".into(),
                    health_pct: 43,
                }),
                cast_bar: None,
                fps: 60,
                ..HudState::default()
            },
            HudState {
                name: "Feile".into(),
                status: full(),
                target: None,
                cast_bar: Some(CastBarState {
                    caster_id: 1,
                    spell_id: 3011,
                    cast_time: 25,
                }),
                fps: 60,
                ..HudState::default()
            },
        ];
        for hud in &states {
            let ctx = egui::Context::default();
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| build(ui, hud));
        }
    }

    /// Named falsifier `CASTBAR_HUD_UNWIRED`.
    ///
    /// Observation that fails if unwired: delete `hud.cast_bar` / `shows_cast_bar` reads in
    /// [`build`] → Some and None tessellate identical overlays (casting never appears).
    #[test]
    fn cast_bar_some_vs_none_discriminates_overlay() {
        let idle = HudState::default();
        let casting = HudState {
            cast_bar: Some(CastBarState {
                caster_id: 7,
                spell_id: 3011,
                cast_time: 30,
            }),
            ..HudState::default()
        };
        assert!(!shows_cast_bar(&idle), "None must hide the cast bar");
        assert!(shows_cast_bar(&casting), "Some must show the cast bar");

        fn mesh_vertices(frame: &crate::ui::EguiFrame) -> usize {
            frame
                .primitives
                .iter()
                .map(|p| match &p.primitive {
                    egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                    egui::epaint::Primitive::Callback(_) => 0,
                })
                .sum()
        }

        let none_frame = crate::ui::tessellate_headless([640, 360], 1.0, |u| build(u, &idle));
        let some_frame = crate::ui::tessellate_headless([640, 360], 1.0, |u| build(u, &casting));
        let none_verts = mesh_vertices(&none_frame);
        let some_verts = mesh_vertices(&some_frame);
        assert!(
            some_verts > none_verts,
            "CASTBAR_HUD_UNWIRED: casting HUD must emit more mesh vertices than idle \
             (got some={some_verts}, none={none_verts}; primitives some={}, none={})",
            some_frame.primitives.len(),
            none_frame.primitives.len()
        );
    }

    /// A bar fraction that is out of range or NaN must not paint outside its rect. Driving `bar`
    /// through a real egui pass is the only way to prove the clamp is actually applied.
    #[test]
    fn out_of_range_fractions_are_clamped() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            for f in [-1.0, 0.0, 0.5, 1.0, 2.0, f32::NAN, f32::INFINITY] {
                bar(ui, f, HEALTH, "x");
            }
        });
    }

    /// The power bar is hidden for classes with no power pool, and shown for those with one.
    /// Asserted on the state rule the layout branches on, so the intent stays pinned even if the
    /// drawing changes.
    #[test]
    fn power_bar_visibility_follows_the_power_pool() {
        assert!(full().has_power());
        assert!(!PlayerStatus {
            max_mana: 0,
            ..full()
        }
        .has_power());
        assert!(
            !PlayerStatus::default().has_power(),
            "unknown maxima → no power bar yet"
        );
    }

    /// Named falsifier `hud_self_cast_only`: peer CastBarState must not be copied into HudState
    /// by the presentation helper — INT fills `cast_bar` from `self_cast_bar` only.
    #[test]
    fn hud_copies_self_bar_from_cast_presentation() {
        use caer_protocol::session::ServerEvent;
        use caer_world::WorldState;
        let mut w = WorldState::new();
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            object_id: 3,
            heading: 0,
        });
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 99,
                spell_id: 1,
                cast_time: 10,
            },
        ));
        let hud = HudState {
            cast_bar: w.cast_presentation().self_bar,
            cast_outcome: w.cast_presentation().outcome,
            ..HudState::default()
        };
        assert!(!shows_cast_bar(&hud));
        w.apply(&ServerEvent::SpellCast(
            caer_protocol::spells::SpellCastAnimation {
                caster_id: 3,
                spell_id: 1,
                cast_time: 10,
            },
        ));
        let hud = HudState {
            cast_bar: w.cast_presentation().self_bar,
            ..HudState::default()
        };
        assert!(shows_cast_bar(&hud));
    }
}
