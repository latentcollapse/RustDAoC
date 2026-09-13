//! The Options Menu — the dialog the Options button opens on character select.
//!
//! This is **not** `options_window`. That is the in-game Game Options panel (Names, Sound
//! Settings, Spell Effect Icons) and it is what CAER was drawing here, alongside an egui Display
//! panel of its own. Retail opens a different thing: two columns of graphics / controls /
//! interface / sound settings over a Description pane, three presets, the video-card line, and
//! Cancel / Accept — drawn *over* the character screen, which stays visible behind it.
//!
//! Every label, value and template name below is a literal in the player's own `game.dll`.
//! `docs/evidence/OPTIONS_MENU_ORACLE_2026-08-15.md` records where each one came from.
//!
//! **PROVENANCE GAP — layout.** No asset carries the dialog's internal geometry; it is engine
//! code. The arrangement here is a two-parameter fit (origin + row pitch) against a retail
//! screenshot, checked at sixteen independent control positions across both columns. It reads
//! right and it is *not* claimed to be retail's geometry. The one number that is client data is
//! where the whole dialog sits — `default*.ini` `OptionsDialog=` — and even that is a saved
//! window position rather than an authored one, so this centres the dialog instead.

/// Which control. One value per row that can be interacted with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptionsId {
    Resolution,
    ClipPlaneDistance,
    Windowed,
    FullScreenWindowed,
    FullScreen,
    Monitor,
    UseAtlantisTrees,
    UseAtlantisTerrain,
    DynamicShadows,
    ShadowQuality,
    ShadowFigures,
    ClassicWater,
    ShroudedIslesWater,
    ReflectiveWater,
    ReflectionQuality,
    ReflectionUpdate,
    SleepMode,
    ConfigureFigureVersions,
    DefaultSettings,
    BestVisualQuality,
    HighestFramerate,
    ConfigureKeyboard,
    MouseMode,
    MouselookSensitivity,
    SkinChoice,
    UseClassicIcons,
    UseClassicNameFont,
    MusicVolume,
    SoundVolume,
    AmbientMusicVolume,
    AmbientSoundVolume,
    Cancel,
    Accept,
}

/// How a row draws and what clicking it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A section heading. Gold, no control.
    Section,
    /// `‹ value ›` — `button_left` / `button_right` either side of the current value.
    Cycle,
    /// `generic_check` plus a label.
    Check,
    /// `generic_check` used as a radio: one of a group is set.
    Radio,
    /// A bracketed action, e.g. `[Configure Keyboard...]`.
    Link,
    /// Text only — the video-card line, the support addresses.
    Static,
}

/// Which of the two columns a row lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    Left,
    Right,
}

/// One authored row.
#[derive(Debug, Clone, Copy)]
pub struct OptionsRow {
    /// `None` for headings and static text.
    pub id: Option<OptionsId>,
    pub kind: RowKind,
    pub col: Col,
    /// Index down the column, uniform pitch, gaps left as skipped indices.
    pub row: u8,
    /// The client's own string. `%s` rows are formatted at draw time.
    pub label: &'static str,
    /// Whether CAER has an engine behind this control.
    ///
    /// H1: a control drawn as enabled must have a complete path. Retail greys what it cannot do
    /// and says so in place — `(Dynamic Shadows are not available)` — rather than hiding the row,
    /// so an unbacked setting here is drawn disabled, never omitted.
    pub enabled: bool,
    /// Description-pane text, verbatim from `game.dll`. Empty when the client has none.
    pub help: &'static str,
}

const fn row(
    id: Option<OptionsId>,
    kind: RowKind,
    col: Col,
    row: u8,
    label: &'static str,
    enabled: bool,
    help: &'static str,
) -> OptionsRow {
    OptionsRow {
        id,
        kind,
        col,
        row,
        label,
        enabled,
        help,
    }
}

/// The dialog, in the order the client's string pool holds it.
///
/// Enabled rows are the ones with a real backing today: the display settings CAER already applies
/// and the three audio buses it already mixes. Everything else is drawn and greyed.
pub const OPTIONS_ROWS: &[OptionsRow] = &[
    // ── Left column: Graphics Options ──────────────────────────────────────────────────────
    row(
        None,
        RowKind::Section,
        Col::Left,
        0,
        "Graphics Options",
        true,
        "",
    ),
    row(
        Some(OptionsId::Resolution),
        RowKind::Cycle,
        Col::Left,
        1,
        "Resolution:",
        true,
        "Select what monitor resolution to use when playing.",
    ),
    row(
        Some(OptionsId::ClipPlaneDistance),
        RowKind::Cycle,
        Col::Left,
        2,
        "Clip Plane Distance",
        true,
        "Controls how far in the distance you can see.  On low end systems, lowering this value \
         can improve your framerate.",
    ),
    row(
        Some(OptionsId::Windowed),
        RowKind::Radio,
        Col::Left,
        3,
        "Windowed",
        true,
        "",
    ),
    row(
        Some(OptionsId::FullScreenWindowed),
        RowKind::Radio,
        Col::Left,
        4,
        "Full-screen Windowed",
        true,
        "",
    ),
    row(
        Some(OptionsId::FullScreen),
        RowKind::Radio,
        Col::Left,
        5,
        "Full Screen",
        true,
        "Takes over the display at the chosen resolution.  Not a mode retail offers; CAER adds it.",
    ),
    row(
        Some(OptionsId::Monitor),
        RowKind::Cycle,
        Col::Left,
        6,
        "Monitor:",
        true,
        "",
    ),
    row(
        Some(OptionsId::UseAtlantisTrees),
        RowKind::Check,
        Col::Left,
        7,
        "Use Atlantis Trees",
        false,
        "If checked, trees in Shrouded Isles and Classic zones will be replaced with the new \
         Trials of Atlantis-style trees.",
    ),
    row(
        Some(OptionsId::UseAtlantisTerrain),
        RowKind::Check,
        Col::Left,
        8,
        "Use Atlantis Terrain",
        false,
        "If checked, the terrain will be drawn using new higher resolution textures.",
    ),
    row(
        Some(OptionsId::DynamicShadows),
        RowKind::Check,
        Col::Left,
        9,
        "Dynamic Shadows",
        false,
        "",
    ),
    row(
        Some(OptionsId::ShadowQuality),
        RowKind::Cycle,
        Col::Left,
        10,
        "Shadow Quality:",
        false,
        "Controls the sharpness of the shadows cast by players and buildings.",
    ),
    row(
        Some(OptionsId::ShadowFigures),
        RowKind::Cycle,
        Col::Left,
        11,
        "Shadow Figures:",
        false,
        "Controls how many nearby figures to cast shadows from.",
    ),
    row(
        None,
        RowKind::Section,
        Col::Left,
        12,
        "Water Options",
        true,
        "",
    ),
    row(
        Some(OptionsId::ClassicWater),
        RowKind::Radio,
        Col::Left,
        13,
        "Classic Water",
        false,
        "When this mode is enabled, all water will be Classic-style water.",
    ),
    row(
        Some(OptionsId::ShroudedIslesWater),
        RowKind::Radio,
        Col::Left,
        14,
        "Shrouded Isles Water",
        false,
        "",
    ),
    row(
        Some(OptionsId::ReflectiveWater),
        RowKind::Radio,
        Col::Left,
        15,
        "Reflective Water",
        false,
        "",
    ),
    row(
        Some(OptionsId::ReflectionQuality),
        RowKind::Cycle,
        Col::Left,
        16,
        "Reflection Quality",
        false,
        "",
    ),
    row(
        Some(OptionsId::ReflectionUpdate),
        RowKind::Cycle,
        Col::Left,
        17,
        "Reflection Update",
        false,
        "",
    ),
    row(
        Some(OptionsId::SleepMode),
        RowKind::Cycle,
        Col::Left,
        18,
        "Sleep Mode",
        false,
        "",
    ),
    row(
        Some(OptionsId::ConfigureFigureVersions),
        RowKind::Link,
        Col::Left,
        19,
        "[Configure Figure Versions...]",
        false,
        "",
    ),
    // ── Left column: presets ───────────────────────────────────────────────────────────────
    row(
        None,
        RowKind::Section,
        Col::Left,
        20,
        "Graphics Options Presets",
        true,
        "",
    ),
    row(
        Some(OptionsId::DefaultSettings),
        RowKind::Link,
        Col::Left,
        21,
        "[Default Settings]",
        false,
        "Chooses the default settings for your hardware.",
    ),
    row(
        Some(OptionsId::BestVisualQuality),
        RowKind::Link,
        Col::Left,
        22,
        "[Best Visual Quality]",
        false,
        "Enables all graphical features that your video card supports.",
    ),
    row(
        Some(OptionsId::HighestFramerate),
        RowKind::Link,
        Col::Left,
        23,
        "[Highest Framerate]",
        false,
        "Disables all optional graphical features in favor of better framerates.",
    ),
    // ── Left column: footer ────────────────────────────────────────────────────────────────
    row(
        None,
        RowKind::Static,
        Col::Left,
        25,
        "Your Video Card: %s",
        true,
        "",
    ),
    row(
        None,
        RowKind::Static,
        Col::Left,
        26,
        "For more information visit:",
        true,
        "",
    ),
    row(
        None,
        RowKind::Static,
        Col::Left,
        27,
        "http://www.darkageofcamelot.com",
        true,
        "",
    ),
    row(
        None,
        RowKind::Static,
        Col::Left,
        28,
        "Email technical support:",
        true,
        "",
    ),
    row(
        None,
        RowKind::Static,
        Col::Left,
        29,
        "support@darkageofcamelot.com",
        true,
        "",
    ),
    // ── Right column: Controls ─────────────────────────────────────────────────────────────
    row(None, RowKind::Section, Col::Right, 0, "Controls", true, ""),
    row(
        Some(OptionsId::ConfigureKeyboard),
        RowKind::Link,
        Col::Right,
        1,
        "[Configure Keyboard...]",
        false,
        "Configure your keyboard controls and hotkeys",
    ),
    row(
        Some(OptionsId::MouseMode),
        RowKind::Cycle,
        Col::Right,
        2,
        "Mouse Mode:",
        false,
        "",
    ),
    row(
        Some(OptionsId::MouselookSensitivity),
        RowKind::Cycle,
        Col::Right,
        3,
        "Mouselook Sensitivity:",
        true,
        "",
    ),
    // ── Right column: Interface ────────────────────────────────────────────────────────────
    row(None, RowKind::Section, Col::Right, 5, "Interface", true, ""),
    row(
        Some(OptionsId::SkinChoice),
        RowKind::Cycle,
        Col::Right,
        6,
        "Skin Choice:",
        false,
        "",
    ),
    row(
        Some(OptionsId::UseClassicIcons),
        RowKind::Check,
        Col::Right,
        7,
        "Use Classic Icons",
        false,
        "",
    ),
    row(
        Some(OptionsId::UseClassicNameFont),
        RowKind::Check,
        Col::Right,
        8,
        "Use Classic Name Font",
        false,
        "",
    ),
    // ── Right column: Sound Options ────────────────────────────────────────────────────────
    row(
        None,
        RowKind::Section,
        Col::Right,
        10,
        "Sound Options",
        true,
        "",
    ),
    row(
        Some(OptionsId::MusicVolume),
        RowKind::Cycle,
        Col::Right,
        11,
        "Music Volume",
        true,
        "",
    ),
    row(
        Some(OptionsId::SoundVolume),
        RowKind::Cycle,
        Col::Right,
        12,
        "Sound Volume",
        true,
        "",
    ),
    // CAER mixes one Ambient bus; retail splits ambient music from ambient sound. Drawn and
    // greyed rather than quietly wired to the same slider as its neighbour.
    row(
        Some(OptionsId::AmbientMusicVolume),
        RowKind::Cycle,
        Col::Right,
        13,
        "Ambient Music Volume",
        false,
        "",
    ),
    row(
        Some(OptionsId::AmbientSoundVolume),
        RowKind::Cycle,
        Col::Right,
        14,
        "Ambient Sound Volume",
        true,
        "",
    ),
    row(
        None,
        RowKind::Section,
        Col::Right,
        16,
        "Description",
        true,
        "",
    ),
];

/// Dialog frame in the 1024×768 pregame authoring space: centred, sized from the fit.
pub const DIALOG: (f32, f32, f32, f32) = (114.0, 48.0, 796.0, 500.0);
/// Y of the first row in each column — below the `Options Menu` title.
pub const FIRST_ROW_Y: f32 = 92.0;
/// Uniform row pitch. Every measured control in both columns lands on this grid.
pub const ROW_PITCH: f32 = 15.0;
/// Label X per column.
pub const COL_X: [f32; 2] = [134.0, 471.0];
/// `‹` X, `›` X and the value centre, per column.
pub const CYCLE_X: [(f32, f32, f32); 2] = [(291.0, 446.0, 368.0), (651.0, 806.0, 728.0)];
/// Row height used for hit-testing (slightly under the pitch so rows do not overlap).
pub const ROW_H: f32 = 12.0;
/// `generic_check` is 12×12; the label sits 16px right of it (`TextOffset`).
pub const CHECK_SIZE: f32 = 12.0;
/// Drawn size of `button_left` / `button_right`.
///
/// `pregame/styles.xml` declares them 16×16, and the sprite is opaque edge to edge — measured on
/// `pregame.mpk:slider.tga`, all 16 of 16 rows carry pixels, so the declaration has no padding to
/// absorb an overlap. Drawing 16 on a 15 pitch therefore collides by 1px, which is the arrow
/// overlap visible on the Options dialog.
///
/// **This is a deliberate 1px deviation from the oracle**, taken because the alternatives are
/// worse and none of them is authored: the pregame dialog is drawn from `game.dll` and ships no
/// XML, so `OPTIONS_ROWS`, `DIALOG` and `ROW_PITCH` are all our construction. The left column runs
/// to row 29; at a 16px pitch its last row lands at y=568 in a dialog that ends at 548, so the
/// pitch cannot match the art without either overflowing the frame or moving the frame into the
/// bottom chrome. 15 is the largest pitch that fits (bottom y=539), and shaving the glyph to match
/// it costs one pixel of arrow and removes the collision entirely.
pub const ARROW_SIZE: f32 = 15.0;
/// `button_large` is 64×21. Cancel then Accept, bottom right of the dialog.
pub const BUTTON_SIZE: (f32, f32) = (64.0, 21.0);

/// Y of a row index, in pregame space.
#[must_use]
pub fn row_y(index: u8) -> f32 {
    FIRST_ROW_Y + f32::from(index) * ROW_PITCH
}

/// Where the Description pane's body sits — under its heading, filling the rest of the column.
#[must_use]
pub fn description_rect() -> (f32, f32, f32, f32) {
    let y = row_y(17);
    (
        COL_X[1],
        y,
        DIALOG.0 + DIALOG.2 - COL_X[1] - 20.0,
        DIALOG.1 + DIALOG.3 - y - 34.0,
    )
}

/// Cancel / Accept, bottom right.
#[must_use]
pub fn button_rect(accept: bool) -> (f32, f32, f32, f32) {
    let (bw, bh) = BUTTON_SIZE;
    let right = DIALOG.0 + DIALOG.2 - 12.0;
    let x = if accept {
        right - bw
    } else {
        right - bw * 2.0 - 20.0
    };
    (x, DIALOG.1 + DIALOG.3 - bh - 16.0, bw, bh)
}

/// Which arrow of a cycle control a point falls on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CycleSide {
    Left,
    Right,
}

impl Col {
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Col::Left => 0,
            Col::Right => 1,
        }
    }
}

/// Window mode.
///
/// **Retail offers two; CAER offers three, and the third is a deliberate addition** (Matt's call,
/// 2026-08-18). Retail's "Full-screen Windowed" is a borderless window sized to the monitor, and it
/// is the only full-screen it has — there is no exclusive mode and no way to ask for one. A true
/// exclusive Full Screen is a modernisation that costs a shard admin nothing and is the thing most
/// players reach for, so it gets its own row. Recorded as a §21 divergence rather than smuggled in
/// as parity.
///
/// All three fill the display. The difference between them is how, not whether — see
/// `PreworldTransform::from_viewport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowChoice {
    Windowed,
    FullScreenWindowed,
    FullScreen,
}

/// `Sounds: 1`..`Sounds: 9`, then `Sounds: Full` — ten steps, shown as the bare value.
pub const VOLUME_STEPS: u8 = 10;

/// What the dialog is currently showing, and what Accept would commit.
///
/// Deliberately not a copy of `DisplaySettings` or `AudioSettings`: this is the dialog's own
/// draft, the host syncs it in before the screen opens and reads it back on Accept. Keeping the
/// renderer out of the settings types is what stops the dialog becoming a second place that
/// decides what a resolution means.
#[derive(Debug, Clone, Default)]
pub struct OptionsDraft {
    /// Live-enumerated mode labels; index selects.
    pub resolutions: Vec<String>,
    pub resolution: usize,
    pub monitors: Vec<String>,
    pub monitor: usize,
    pub window: Option<WindowChoice>,
    /// 0 Near, 1 Medium, 2 Far.
    pub clip_plane: u8,
    /// 1..=5.
    pub mouselook_sensitivity: u8,
    /// 0..=9, where 9 renders as `Full`.
    pub music: u8,
    pub sound: u8,
    pub ambient_sound: u8,
    /// Fills `Your Video Card: %s`. Empty until the host supplies the adapter name.
    pub video_card: String,
}

impl OptionsDraft {
    /// `Windowed` unless the host said otherwise, so a fresh draft draws a consistent radio group.
    #[must_use]
    pub fn window_choice(&self) -> WindowChoice {
        self.window.unwrap_or(WindowChoice::Windowed)
    }

    /// The value the client would print in the right-hand column, or `None` for rows with no value.
    #[must_use]
    pub fn value_text(&self, id: OptionsId) -> Option<String> {
        let volume = |v: u8| {
            if v + 1 >= VOLUME_STEPS {
                "Full".to_string()
            } else {
                (v + 1).to_string()
            }
        };
        Some(match id {
            OptionsId::Resolution => self.resolutions.get(self.resolution)?.clone(),
            OptionsId::Monitor => self.monitors.get(self.monitor)?.clone(),
            OptionsId::ClipPlaneDistance => match self.clip_plane {
                0 => "Near",
                1 => "Medium",
                _ => "Far",
            }
            .to_string(),
            OptionsId::MouselookSensitivity => self.mouselook_sensitivity.clamp(1, 5).to_string(),
            OptionsId::MusicVolume => volume(self.music),
            OptionsId::SoundVolume => volume(self.sound),
            OptionsId::AmbientSoundVolume => volume(self.ambient_sound),
            // Drawn but not backed: the client shows its first value rather than a blank column.
            OptionsId::ShadowQuality => "Low".to_string(),
            OptionsId::ShadowFigures => "Near 5 Figures".to_string(),
            OptionsId::ReflectionQuality => "Low Quality".to_string(),
            OptionsId::ReflectionUpdate => "Seldom".to_string(),
            OptionsId::SleepMode => "Minimized".to_string(),
            OptionsId::MouseMode => "Mouselook On".to_string(),
            OptionsId::SkinChoice => "Atlantis Skin".to_string(),
            OptionsId::AmbientMusicVolume => "Full".to_string(),
            _ => return None,
        })
    }

    /// Is this radio the set one?
    #[must_use]
    pub fn radio_set(&self, id: OptionsId) -> bool {
        match id {
            OptionsId::Windowed => self.window_choice() == WindowChoice::Windowed,
            OptionsId::FullScreenWindowed => {
                self.window_choice() == WindowChoice::FullScreenWindowed
            }
            OptionsId::FullScreen => self.window_choice() == WindowChoice::FullScreen,
            // Water mode has no engine behind it; Shrouded Isles is the client's own default and
            // the group must show exactly one set radio or it reads as broken.
            OptionsId::ShroudedIslesWater => true,
            _ => false,
        }
    }

    /// Step a cycle control. No-op for controls with no backing.
    pub fn cycle(&mut self, id: OptionsId, side: CycleSide) {
        let step = |v: &mut usize, len: usize| {
            if len == 0 {
                return;
            }
            *v = match side {
                CycleSide::Left => (*v + len - 1) % len,
                CycleSide::Right => (*v + 1) % len,
            };
        };
        let step_u8 = |v: &mut u8, lo: u8, hi: u8| {
            *v = match side {
                CycleSide::Left if *v > lo => *v - 1,
                CycleSide::Left => hi,
                CycleSide::Right if *v < hi => *v + 1,
                CycleSide::Right => lo,
            };
        };
        match id {
            OptionsId::Resolution => {
                let len = self.resolutions.len();
                step(&mut self.resolution, len);
            }
            OptionsId::Monitor => {
                let len = self.monitors.len();
                step(&mut self.monitor, len);
            }
            OptionsId::ClipPlaneDistance => step_u8(&mut self.clip_plane, 0, 2),
            OptionsId::MouselookSensitivity => {
                self.mouselook_sensitivity = self.mouselook_sensitivity.clamp(1, 5);
                step_u8(&mut self.mouselook_sensitivity, 1, 5);
            }
            OptionsId::MusicVolume => step_u8(&mut self.music, 0, VOLUME_STEPS - 1),
            OptionsId::SoundVolume => step_u8(&mut self.sound, 0, VOLUME_STEPS - 1),
            OptionsId::AmbientSoundVolume => step_u8(&mut self.ambient_sound, 0, VOLUME_STEPS - 1),
            _ => {}
        }
    }
}

/// What a click on the dialog means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OptionsHit {
    /// One of the two arrows of a cycle control.
    Cycle(OptionsId, CycleSide),
    /// A check, a radio or a bracketed link — everything that is one press.
    Press(OptionsId),
    Accept,
    Cancel,
}

fn inside(rect: (f32, f32, f32, f32), x: f32, y: f32) -> bool {
    x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
}

/// The clickable box of a row's label / check / link, in pregame space.
#[must_use]
pub fn row_rect(r: &OptionsRow) -> (f32, f32, f32, f32) {
    let x = COL_X[r.col.index()];
    let (arrow_l, _, _) = CYCLE_X[r.col.index()];
    (x, row_y(r.row), arrow_l - x - 4.0, ROW_H)
}

/// Where a cycle control's two arrows are **drawn**, in pregame space.
///
/// Full 16×16 art, centred on the 12px row, exactly as the client draws it.
#[must_use]
pub fn arrow_rects(r: &OptionsRow) -> [(f32, f32, f32, f32); 2] {
    let (lx, rx, _) = CYCLE_X[r.col.index()];
    let y = row_y(r.row) - (ARROW_SIZE - ROW_H) * 0.5;
    [
        (lx, y, ARROW_SIZE, ARROW_SIZE),
        (rx, y, ARROW_SIZE, ARROW_SIZE),
    ]
}

/// Where those arrows **take a click**, in pregame space.
///
/// The drawn art is 16 tall on a 13 pitch, so two adjacent cycle rows' arrows overlap by 3px.
/// Drawn that way it is retail; hit-tested that way, a click in the overlap band would resolve to
/// whichever row [`hit`] happened to reach first — the player clicks one control and steps a
/// different one. Clipping the hit box to the row keeps every click on the row it looks like.
#[must_use]
pub fn arrow_hit_rects(r: &OptionsRow) -> [(f32, f32, f32, f32); 2] {
    let (lx, rx, _) = CYCLE_X[r.col.index()];
    let y = row_y(r.row);
    [(lx, y, ARROW_SIZE, ROW_H), (rx, y, ARROW_SIZE, ROW_H)]
}

/// Hit-test a point already converted into the 1024×768 pregame space.
///
/// Disabled controls return `None` — H1: what is drawn greyed must not respond. Returning
/// `Some(Cancel)` for a click anywhere outside the frame is deliberate: the dialog is modal, and
/// a modal that silently swallows clicks is the shape of the bug this whole screen replaces.
#[must_use]
pub fn hit(x: f32, y: f32) -> Option<OptionsHit> {
    if inside(button_rect(true), x, y) {
        return Some(OptionsHit::Accept);
    }
    if inside(button_rect(false), x, y) {
        return Some(OptionsHit::Cancel);
    }
    for r in OPTIONS_ROWS {
        let Some(id) = r.id else { continue };
        if !r.enabled {
            continue;
        }
        match r.kind {
            RowKind::Cycle => {
                for (i, rect) in arrow_hit_rects(r).into_iter().enumerate() {
                    if inside(rect, x, y) {
                        return Some(OptionsHit::Cycle(
                            id,
                            if i == 0 {
                                CycleSide::Left
                            } else {
                                CycleSide::Right
                            },
                        ));
                    }
                }
            }
            RowKind::Check | RowKind::Radio | RowKind::Link => {
                if inside(row_rect(r), x, y) {
                    return Some(OptionsHit::Press(id));
                }
            }
            RowKind::Section | RowKind::Static => {}
        }
    }
    None
}

/// Which row the pointer is over, for the Description pane. Disabled rows still describe
/// themselves — retail explains a setting you cannot use as readily as one you can.
#[must_use]
pub fn hovered_row(x: f32, y: f32) -> Option<&'static OptionsRow> {
    OPTIONS_ROWS.iter().find(|r| {
        r.id.is_some()
            && (inside(row_rect(r), x, y)
                || (r.kind == RowKind::Cycle
                    && arrow_hit_rects(r).into_iter().any(|a| inside(a, x, y))))
    })
}

/// Look up a row by its control id.
#[must_use]
pub fn row_for(id: OptionsId) -> Option<&'static OptionsRow> {
    OPTIONS_ROWS.iter().find(|r| r.id == Some(id))
}

/// Is this control both present and backed?
#[must_use]
pub fn is_enabled(id: OptionsId) -> bool {
    row_for(id).is_some_and(|r| r.enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two controls must never share a grid cell — one of them would be unreachable.
    #[test]
    fn no_two_rows_share_a_cell() {
        let mut seen = Vec::new();
        for r in OPTIONS_ROWS {
            let cell = (r.col, r.row);
            assert!(
                !seen.contains(&cell),
                "{:?} collides with an earlier row at {cell:?}",
                r.label
            );
            seen.push(cell);
        }
    }

    /// Every interactive row has an id, and every id appears at most once.
    #[test]
    fn interactive_rows_are_identified_exactly_once() {
        let mut ids = Vec::new();
        for r in OPTIONS_ROWS {
            match r.kind {
                RowKind::Section | RowKind::Static => {
                    assert!(
                        r.id.is_none(),
                        "{:?} is not interactive but carries an id",
                        r.label
                    );
                }
                _ => {
                    let id = r.id.unwrap_or_else(|| panic!("{:?} has no id", r.label));
                    assert!(!ids.contains(&id), "{id:?} is placed twice");
                    ids.push(id);
                }
            }
        }
        assert!(ids.len() > 25, "the dialog lost most of its controls");
    }

    /// Every row draws inside the dialog frame. A control outside it is invisible and unclickable.
    #[test]
    fn every_row_stays_inside_the_dialog() {
        let (dx, dy, dw, dh) = DIALOG;
        for r in OPTIONS_ROWS {
            let y = row_y(r.row);
            let x = COL_X[r.col.index()];
            assert!(
                x >= dx && y >= dy && y + ROW_H <= dy + dh,
                "{:?} at ({x},{y}) leaves the dialog {DIALOG:?}",
                r.label
            );
            if r.kind == RowKind::Cycle {
                let (lx, rx, _) = CYCLE_X[r.col.index()];
                assert!(
                    rx + ARROW_SIZE <= dx + dw && lx > x,
                    "{:?}: cycle arrows fall outside the dialog",
                    r.label
                );
            }
        }
        for accept in [false, true] {
            let (bx, by, bw, bh) = button_rect(accept);
            assert!(bx >= dx && by >= dy && bx + bw <= dx + dw && by + bh <= dy + dh);
        }
        let (px, py, pw, ph) = description_rect();
        assert!(pw > 100.0 && ph > 40.0, "the Description pane collapsed");
        assert!(px + pw <= dx + dw && py + ph <= dy + dh);
    }

    /// The arrow is drawn one pixel under its authored size, and that is deliberate.
    ///
    /// `styles.xml` declares `button_left` 16×16 and the sprite is opaque edge to edge — measured
    /// on `pregame.mpk:slider.tga`, all 16 of 16 rows carry pixels. But the pregame dialog has no
    /// authored geometry at all (it is drawn from `game.dll`), so `OPTIONS_ROWS`, `DIALOG` and
    /// `ROW_PITCH` are ours. The left column runs to row 29, and at a 16px pitch its last row
    /// lands at y=568 in a dialog ending at 548. 15 is the largest pitch that fits, so the glyph
    /// is shaved to match rather than left colliding with the row below.
    ///
    /// `generic_check` keeps its authored 12×12 — nothing forces a deviation there.
    #[test]
    fn arrow_art_is_the_authored_size_and_only_the_hit_box_is_clipped() {
        assert!(
            (ARROW_SIZE - ROW_PITCH).abs() < f32::EPSILON,
            "the arrow must exactly fill its row pitch, or adjacent rows collide again"
        );
        assert!(
            (CHECK_SIZE - 12.0).abs() < f32::EPSILON,
            "styles.xml: 12×12"
        );
        let cycle = OPTIONS_ROWS
            .iter()
            .find(|r| r.kind == RowKind::Cycle)
            .expect("a cycle row");
        let [drawn, _] = arrow_rects(cycle);
        let [hit_box, _] = arrow_hit_rects(cycle);
        assert!(
            (drawn.3 - ARROW_SIZE).abs() < 0.01,
            "drawn at full art height"
        );
        assert!((hit_box.3 - ROW_H).abs() < 0.01, "hit clipped to the row");
        assert!(
            drawn.1 < hit_box.1 && drawn.1 + drawn.3 > hit_box.1 + hit_box.3,
            "the art is centred on the row it belongs to"
        );
        assert!((drawn.0 - hit_box.0).abs() < 0.01 && (drawn.2 - hit_box.2).abs() < 0.01);
    }

    #[test]
    fn cancel_and_accept_do_not_overlap() {
        let (cx, cy, cw, ch) = button_rect(false);
        let (ax, ay, aw, ah) = button_rect(true);
        let cancel_right = cx + cw;
        assert!(
            ax >= cancel_right + 8.0,
            "Cancel ({cx}..{cancel_right}) kisses Accept ({ax}..{})",
            ax + aw
        );
        assert!((cy - ay).abs() < 0.1 && (ch - ah).abs() < 0.1);
    }

    /// A click must land on the row it looks like it landed on.
    ///
    /// The **drawn** arrows are 16px on a 13px pitch and therefore do overlap between adjacent
    /// rows — that is the client's own art and is not a defect to design away. The **hit** boxes
    /// are the ones that must be disjoint, or a click in the 3px overlap steps whichever control
    /// `hit` reaches first.
    #[test]
    fn cycle_arrow_hit_boxes_do_not_overlap() {
        for r in OPTIONS_ROWS.iter().filter(|r| r.kind == RowKind::Cycle) {
            let [l, ri] = arrow_hit_rects(r);
            assert!(
                l.0 + l.2 + 4.0 <= ri.0,
                "{:?}: left arrow kisses right",
                r.label
            );
        }
        let cycles: Vec<_> = OPTIONS_ROWS
            .iter()
            .filter(|r| r.kind == RowKind::Cycle)
            .collect();
        for pair in cycles.windows(2) {
            if pair[0].col != pair[1].col {
                continue;
            }
            if pair[1].row != pair[0].row + 1 {
                continue;
            }
            let [_, a] = arrow_hit_rects(pair[0]);
            let [b, _] = arrow_hit_rects(pair[1]);
            assert!(
                a.1 + a.3 <= b.1 + 0.01,
                "{} / {} cycle arrow hit boxes overlap vertically",
                pair[0].label,
                pair[1].label
            );
        }
    }

    /// The enabled set is the set CAER can actually honour. If a control gains a backing this
    /// list changes deliberately, not by accident.
    #[test]
    fn only_backed_controls_are_enabled() {
        let enabled: Vec<OptionsId> = OPTIONS_ROWS
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| r.id)
            .collect();
        assert_eq!(
            enabled,
            vec![
                OptionsId::Resolution,
                OptionsId::ClipPlaneDistance,
                OptionsId::Windowed,
                OptionsId::FullScreenWindowed,
                OptionsId::FullScreen,
                OptionsId::Monitor,
                OptionsId::MouselookSensitivity,
                OptionsId::MusicVolume,
                OptionsId::SoundVolume,
                OptionsId::AmbientSoundVolume,
            ]
        );
    }
}
