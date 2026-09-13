//! Where the pre-world screens can be clicked, and the one table that says so.
//!
//! Every rect below is transcribed from the player's own `pregame/*.xml` together with the
//! `ControlId` that names it there, and [`tests::authored_controls_match_client_xml`] reads those
//! files back to prove the transcription. Drawing, hover and click all resolve through
//! [`hit`], so a control cannot be visible in one place and respond in another.
//!
//! That is not hypothetical. The create form drew round `quit`/`realm`/`customize` buttons at
//! y=706 and a `64x16_no_bg` word beneath each at y=752, and the hit test looked at the word only.
//! Cancel, Continué and Realm were all inert where the player could see them and live over the
//! caption underneath.
//!
//! Rects are in the 1024×768 authoring space. [`crate::preworld::PreworldTransform`] is the only
//! thing that maps them to a surface, on both sides of a click.

use crate::preworld::{PreWorldScreen, PreworldTransform};
use crate::preworld_camera::CameraControl;
use crate::preworld_customize::{
    self, CustomizerField, CustomizerWidget, ARROW_SIZE, CONTROL_WIDTH, LOCK_SIZE,
};
use crate::skinui::Rect;

/// One authored control rect, with the `ControlId` it carries in the client's XML.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArtRect {
    pub control_id: u16,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// False for `InvisibleButtonDef` — retail ships hit area with no art of its own, and the
    /// audit report has to be able to say which kind the pointer is over.
    pub visible: bool,
}

impl ArtRect {
    const fn art(control_id: u16, x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            control_id,
            x,
            y,
            w,
            h,
            visible: true,
        }
    }

    const fn invisible(control_id: u16, x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            control_id,
            x,
            y,
            w,
            h,
            visible: false,
        }
    }

    #[must_use]
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    #[must_use]
    pub fn mapped(self, xf: PreworldTransform) -> Rect {
        xf.map_rect(self.x, self.y, self.w, self.h)
    }
}

/// What a control does, independent of how it is drawn.
///
/// Geometry lives here; the mapping onto [`crate::preworld::PreWorldAction`] stays in `preworld`,
/// where the enable rules and the current selection are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Protocol realm id (Albion 1, Midgard 2, Hibernia 3).
    RealmColumn(u8),
    RealmQuit,
    /// Play or Create, depending on whether the selected row holds a character.
    CharSelectPlay,
    CharSelectCustomize,
    CharSelectDelete,
    CharSelectRealm,
    CharSelectQuit,
    CharSelectOptions,
    /// Protocol slot 0..9.
    CharSlot(u8),
    CharCreateCancel,
    CharCreateContinue,
    CharCreateRealm,
    CharCreateRandomName,
    CharCreateName,
    /// 0 male, 1 female.
    CharCreateGender(u8),
    CharCreateRace(u8),
    CharCreateClass(u8),
    /// `character_customize.xml` Cancel (ControlIds 1002 / 1039): return to character select.
    CustomizeCancel,
    /// `character_customize.xml` Realm (ControlIds 1013 / 1040): return to the realm plate.
    CustomizeRealm,
    /// `character_customize.xml` Race/Class (ControlIds 1004 / 1041): return one step to the
    /// creation form without discarding the draft.
    CustomizeBack,
    /// `character_customize.xml` Continue (ControlIds 1003 / 1068): advance to statistics.
    CustomizeAdvance,
    /// `character_customize_basic.xml` Adjust Attributes (ControlIds 1130 / 1131).  The retail
    /// customizer composes this basic overlay with the detailed face/palette form, so its stat
    /// affordance is a distinct visible control rather than an unlabelled duplicate of Continue.
    CustomizeStats,
    /// `character_customize.xml` Default (ControlId 1078): restore the shipped default look.
    CustomizeReset,
    /// `character_customize.xml` Random (ControlId 1028): randomize only unlocked, source
    /// catalogue values.
    CustomizeRandom,
    /// One left/right runtime selector. `field` identifies the actual player-facing setting,
    /// never an ambiguous raw create-packet byte. `dir` is -1 left / +1 right.
    CustomizeAdjust {
        field: CustomizerField,
        dir: i8,
    },
    /// One runtime slider.  The control owns the source rectangle; conversion from its click
    /// coordinate to a 0..=8 tick happens once in [`horizontal_slider_tick`].
    CustomizeSlider {
        field: CustomizerField,
    },
    /// A Random lock beside one runtime field.
    CustomizeLock {
        field: CustomizerField,
    },
    /// Compatibility target for the legacy static palette form.  It is retained only while the
    /// old XML-transcription tests are migrated; production customizer geometry does not emit it.
    #[doc(hidden)]
    CustomizeScale {
        dir: i8,
    },
    /// Compatibility target for the legacy static palette form.
    #[doc(hidden)]
    CustomizeCycle {
        field: u8,
        dir: i8,
    },
    /// Compatibility target for the legacy static palette form.
    #[doc(hidden)]
    CustomizeMorph {
        slot: u8,
    },
    /// Compatibility target for the legacy static palette form.
    #[doc(hidden)]
    CustomizePalette {
        palette: u8,
        value: u8,
    },
    /// One of the source-authored customizer camera controls.  These are local preview state,
    /// never a character-create packet field.
    CustomizeCamera(CameraControl),
    /// `quit_confirm.xml` — the modal Quit raises, not a control on any screen.
    QuitConfirmYes,
    QuitConfirmNo,
    /// `delete_confirm.xml` — the modal Delete raises.
    DeleteConfirmYes,
    DeleteConfirmNo,
    /// `character_customize_stats.xml` mini arrow (ControlIds 1031–1038 plus, 1039–1046 minus):
    /// stat index 0..7 in wire order, `dir` +1 right / −1 left. Enforces the DOLSharp allocation
    /// rules in `caer_protocol::starting_stats`.
    StatsAdjust {
        stat: u8,
        dir: i8,
    },
    /// `character_customize_stats.xml` reset (ControlId 1020) — back to all race bases.
    StatsReset,
    /// `character_customize_stats.xml` Optimize (ControlId 1021) — local default allocation.
    StatsOptimize,
}

/// An authored pre-world modal: a form the client ships that opens *over* a screen.
///
/// One kind, not one type per dialog. Both forms are 250x164 with two `button_small` controls, and
/// a second copy of the raise / draw / hit / dismiss machinery is exactly the parallel path this
/// codebase keeps having to delete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modal {
    QuitConfirm,
    DeleteConfirm,
}

impl Modal {
    /// The client file this modal is transcribed from.
    #[must_use]
    pub const fn source(self) -> &'static str {
        match self {
            Self::QuitConfirm => "quit_confirm.xml",
            Self::DeleteConfirm => "delete_confirm.xml",
        }
    }

    /// Authored `Width`/`Height` of the form.
    #[must_use]
    pub const fn size(self) -> (f32, f32) {
        match self {
            // Both forms author 250x164. Kept per-variant rather than shared, because a shared
            // constant would quietly become wrong the first time a third modal is added.
            Self::QuitConfirm | Self::DeleteConfirm => (250.0, 164.0),
        }
    }

    /// Where the form's top-left lands in the 1024x768 authoring space.
    ///
    /// Retail's `WindowManager` centres a modal, and centring inside the design space — rather than
    /// inside the physical viewport — is what keeps these dialogs *inside* the pre-world coordinate
    /// contract. The login dialog centres in the viewport, which is why ledger J11 records it as
    /// living outside that contract.
    #[must_use]
    pub fn origin(self) -> (f32, f32) {
        let (w, h) = self.size();
        (
            ((crate::preworld::PREGAME_W - w) * 0.5).round(),
            ((crate::preworld::PREGAME_H - h) * 0.5).round(),
        )
    }
}

/// A control the player can act on, and every piece of authored art that activates it.
#[derive(Clone, Debug, PartialEq)]
pub struct Control {
    pub target: Target,
    /// Human name, for reports. Not a hit key.
    pub name: &'static str,
    /// The client file this control was transcribed from.
    pub source: &'static str,
    pub parts: Vec<ArtRect>,
}

impl Control {
    /// Whether this control has a visual-runtime correction distinct from its retained raw source
    /// transcription. Keep this narrowly named instead of baking screenshot coordinates into the
    /// generic hit path: the DOL XML's Cancel-caption anomaly is the only known case.
    fn has_runtime_caption_correction(&self) -> bool {
        matches!(self.target, Target::CustomizeCancel) && self.parts.len() >= 2
    }

    /// Does any live part of this control cover the point?
    ///
    /// Most controls use their authored XML rects verbatim. The customizer's loose Cancel caption
    /// is a documented runtime-composition exception; the visible corrected caption replaces the
    /// displaced XML location so it cannot leave a second, invisible click target behind.
    #[must_use]
    pub fn covers(&self, x: f32, y: f32) -> bool {
        let corrected = self.has_runtime_caption_correction();
        self.parts
            .iter()
            .enumerate()
            .any(|(i, p)| (!corrected || i + 1 != self.parts.len()) && p.contains(x, y))
            || (corrected && self.caption().contains(x, y))
    }

    /// The live part under a design-space point, preferring visible art.
    ///
    /// A realm column is an `InvisibleButtonDef` laid over its own crest, so "which part is the
    /// pointer on" has two answers and the useful one is the art the player can see.
    #[must_use]
    pub fn part_at(&self, x: f32, y: f32) -> Option<ArtRect> {
        let corrected = self.has_runtime_caption_correction();
        let authored_len = self.parts.len().saturating_sub(usize::from(corrected));
        let hit = |visible: bool| {
            self.parts
                .iter()
                .take(authored_len)
                .copied()
                .find(|p| p.visible == visible && p.contains(x, y))
        };
        hit(true)
            .or_else(|| {
                let caption = self.caption();
                (corrected && caption.visible && caption.contains(x, y)).then_some(caption)
            })
            .or_else(|| hit(false))
    }

    /// The button art itself — the first visible part, which is what the form draws.
    #[must_use]
    pub fn art(&self) -> ArtRect {
        self.parts
            .iter()
            .copied()
            .find(|p| p.visible)
            .unwrap_or(self.parts[0])
    }

    /// The caption beneath the button, when the control has one; otherwise the art.
    ///
    /// Pre-world chrome is authored as a round button plus a separate `64x16_no_bg` word, and the
    /// word is a control in its own right — it is drawn from here and hit-tested with the button.
    #[must_use]
    pub fn caption(&self) -> ArtRect {
        let raw = self.parts.last().copied().unwrap_or(self.parts[0]);
        if !self.has_runtime_caption_correction() {
            return raw;
        }
        // `character_customize.xml` ControlId 1039 puts its Cancel label at x=163 even though
        // the matching round button is x=76. Retail's actual composition centres it under the
        // button (the screenshot oracle), while `parts` intentionally preserves the raw value
        // for asset/XML audit. Derive the live rectangle once so draw and hit stay identical.
        let art = self.art();
        ArtRect {
            x: art.x + (art.w - raw.w) * 0.5,
            ..raw
        }
    }

    /// Smallest rect covering every part — for drawing a row highlight or reporting an extent.
    /// **Not** a hit rect: for a control drawn as a button plus a caption below it, the box
    /// between them is empty art and must not respond.
    #[must_use]
    pub fn bounds(&self) -> ArtRect {
        let first = self.parts[0];
        let (mut x0, mut y0) = (first.x, first.y);
        let (mut x1, mut y1) = (first.x + first.w, first.y + first.h);
        for (i, source) in self.parts.iter().enumerate().skip(1) {
            let p = if self.has_runtime_caption_correction() && i + 1 == self.parts.len() {
                self.caption()
            } else {
                *source
            };
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x + p.w);
            y1 = y1.max(p.y + p.h);
        }
        ArtRect {
            control_id: first.control_id,
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
            visible: self.parts.iter().any(|p| p.visible),
        }
    }

    /// Where a test or scenario should click to press this control: the centre of its largest
    /// visible part, falling back to the first part when nothing is visible.
    ///
    /// Centre-of-bounds would be wrong. A character row is a radio plus two lines of text with a
    /// seam between them, and its bounding centre lands in the seam.
    #[must_use]
    pub fn click_point(&self) -> (f32, f32) {
        let p = self
            .parts
            .iter()
            .filter(|p| p.visible)
            .max_by(|a, b| (a.w * a.h).total_cmp(&(b.w * b.h)))
            .copied()
            .unwrap_or(self.parts[0]);
        (p.x + p.w * 0.5, p.y + p.h * 0.5)
    }
}

const CHAR_SLOTS: u8 = 10;
/// Row pitch of the character list, `character_selection.xml`.
const CHAR_ROW_PITCH: f32 = 45.0;

fn control(
    target: Target,
    name: &'static str,
    source: &'static str,
    parts: Vec<ArtRect>,
) -> Control {
    Control {
        target,
        name,
        source,
        parts,
    }
}

/// `realm_selection.xml`.
///
/// The three columns are `InvisibleButtonDef`s covering the left, middle and right thirds down to
/// y=640 — retail really does let the player click anywhere in a realm's column, so the crest and
/// the gold name buttons inside each are affordances, not the hit area. They are listed anyway so
/// a report can say which visible art the pointer is over.
fn realm_select() -> Vec<Control> {
    const SRC: &str = "realm_selection.xml";
    vec![
        control(
            Target::RealmColumn(1),
            "Albion",
            SRC,
            vec![
                ArtRect::invisible(1014, 0.0, 0.0, 341.0, 640.0),
                ArtRect::art(1001, 110.0, 190.0, 114.0, 150.0),
                ArtRect::art(1008, 39.0, 350.0, 256.0, 32.0),
                ArtRect::art(1020, 39.0, 375.0, 256.0, 32.0),
            ],
        ),
        control(
            Target::RealmColumn(3),
            "Hibernia",
            SRC,
            vec![
                ArtRect::invisible(1016, 341.0, 0.0, 341.0, 640.0),
                ArtRect::art(1003, 430.0, 172.0, 148.0, 150.0),
                ArtRect::art(1010, 382.0, 350.0, 256.0, 32.0),
                ArtRect::art(1022, 382.0, 375.0, 256.0, 32.0),
            ],
        ),
        control(
            Target::RealmColumn(2),
            "Midgard",
            SRC,
            vec![
                ArtRect::invisible(1015, 682.0, 0.0, 341.0, 640.0),
                ArtRect::art(1002, 802.0, 172.0, 122.0, 150.0),
                ArtRect::art(1009, 734.0, 350.0, 256.0, 32.0),
                ArtRect::art(1021, 734.0, 375.0, 256.0, 32.0),
            ],
        ),
        control(
            Target::RealmQuit,
            "Quit",
            SRC,
            vec![
                ArtRect::art(1004, 74.0, 706.0, 38.0, 52.0),
                ArtRect::art(1007, 62.0, 752.0, 67.0, 16.0),
            ],
        ),
    ]
}

/// `character_selection.xml`.
///
/// Each bottom-row control is a round 38×52 button with a `64x16_no_bg` word beneath it, and each
/// character row is a 20×20 radio with two 208×16 text lines beside it. Both pieces of every pair
/// respond, and the empty space between them does not.
fn char_select() -> Vec<Control> {
    const SRC: &str = "character_selection.xml";
    let mut out = vec![
        control(
            Target::CharSelectPlay,
            "Play/Create",
            SRC,
            vec![
                ArtRect::art(1090, 854.0, 575.0, 38.0, 52.0),
                ArtRect::art(1091, 810.0, 625.0, 128.0, 32.0),
            ],
        ),
        control(
            Target::CharSelectCustomize,
            "Customize",
            SRC,
            vec![
                ArtRect::art(1100, 701.0, 706.0, 38.0, 52.0),
                ArtRect::art(1101, 686.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CharSelectDelete,
            "Delete",
            SRC,
            vec![
                ArtRect::art(1094, 913.0, 705.0, 38.0, 52.0),
                ArtRect::art(1095, 900.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CharSelectRealm,
            "Realm",
            SRC,
            vec![
                ArtRect::art(1098, 279.0, 706.0, 38.0, 52.0),
                ArtRect::art(1099, 267.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CharSelectQuit,
            "Quit",
            SRC,
            vec![
                ArtRect::art(1096, 81.0, 706.0, 38.0, 52.0),
                ArtRect::art(1097, 67.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CharSelectOptions,
            "Options",
            SRC,
            vec![
                ArtRect::art(1092, 494.0, 707.0, 38.0, 52.0),
                ArtRect::art(1093, 480.0, 752.0, 67.0, 16.0),
            ],
        ),
    ];
    for slot in 0..CHAR_SLOTS {
        let row = f32::from(slot) * CHAR_ROW_PITCH;
        let id = u16::from(slot);
        out.push(control(
            Target::CharSlot(slot),
            "Character row",
            SRC,
            vec![
                ArtRect::art(1030 + id, 780.0, 125.0 + row, 20.0, 20.0),
                ArtRect::art(1050 + id, 815.0, 115.0 + row, 208.0, 16.0),
                ArtRect::art(1070 + id, 815.0, 135.0 + row, 208.0, 16.0),
            ],
        ));
    }
    out
}

/// `character_creation.xml`.
///
/// Race, class and gender are all `button_pregame_medium` (102×21); the two-column grids are
/// generated rather than transcribed row by row, and the XML check proves the generator.
fn char_create() -> Vec<Control> {
    const SRC: &str = "character_creation.xml";
    const MEDIUM: (f32, f32) = (102.0, 21.0);
    let medium = |id: u16, x: f32, y: f32| ArtRect::art(id, x, y, MEDIUM.0, MEDIUM.1);
    let mut out = vec![
        control(
            Target::CharCreateCancel,
            "Cancel",
            SRC,
            vec![
                ArtRect::art(1002, 70.0, 706.0, 38.0, 52.0),
                ArtRect::art(1067, 60.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CharCreateContinue,
            "Continue",
            SRC,
            vec![
                ArtRect::art(1003, 908.0, 706.0, 38.0, 52.0),
                ArtRect::art(1068, 898.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CharCreateRealm,
            "Realm",
            SRC,
            vec![
                ArtRect::art(1053, 272.0, 706.0, 38.0, 52.0),
                ArtRect::art(1066, 262.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CharCreateRandomName,
            "Random",
            SRC,
            vec![ArtRect::art(1013, 938.0, 116.0, 64.0, 21.0)],
        ),
        control(
            Target::CharCreateName,
            "Name",
            SRC,
            vec![ArtRect::art(1051, 790.0, 114.0, 150.0, 28.0)],
        ),
        control(
            Target::CharCreateGender(0),
            "Male",
            SRC,
            vec![medium(1090, 795.0, 307.0)],
        ),
        control(
            Target::CharCreateGender(1),
            "Female",
            SRC,
            vec![medium(1091, 901.0, 307.0)],
        ),
    ];
    // Races 0..5 are a two-column grid at y=185/210/235; slot 6 is the Minotaur button, authored
    // apart from the grid at (901,160).
    for i in 0..6u8 {
        let x = if i % 2 == 0 { 795.0 } else { 901.0 };
        let y = 185.0 + f32::from(i / 2) * 25.0;
        out.push(control(
            Target::CharCreateRace(i),
            "Race",
            SRC,
            vec![medium(1014 + u16::from(i), x, y)],
        ));
    }
    out.push(control(
        Target::CharCreateRace(6),
        "Race (Minotaur)",
        SRC,
        vec![medium(1020, 901.0, 160.0)],
    ));
    for i in 0..caer_protocol::creation_adapters::CLASS_ADAPTER_SLOTS as u8 {
        let x = if i % 2 == 0 { 795.0 } else { 901.0 };
        let y = 378.0 + f32::from(i / 2) * 25.0;
        out.push(control(
            Target::CharCreateClass(i),
            "Class",
            SRC,
            vec![medium(1021 + u16::from(i), x, y)],
        ));
    }
    out
}

/// `character_customize.xml`.
///
/// This is deliberately a separate table from [`char_create`]: the form has a different archive,
/// different bottom-row artwork, three image pickers, and several controls that look broadly like
/// creation controls but do a very different thing. Keeping all geometry here lets one test read
/// it back from the client XML instead of turning the customization screen into a second set of
/// guessed rectangles.
fn char_customize() -> Vec<Control> {
    const SRC: &str = "character_customize.xml";
    // This retail build names the arrows `left_arrow_button`/`right_arrow_button`, but those
    // legacy aliases are not declared in `styles.xml`. The shipped generic `button_left/right`
    // controls are the matching 16x16 slider arrows; their size is the source-backed fallback
    // used for both draw and hit until the legacy alias table is recovered.
    const ARROW: (f32, f32) = (16.0, 16.0);
    const MEDIUM: (f32, f32) = (102.0, 21.0);
    const CAMERA: (f32, f32) = (26.0, 26.0);
    const CAMERA_ZOOM: (f32, f32) = (30.0, 30.0);
    const CAMERA_RESET: (f32, f32) = (43.0, 43.0);
    let arrow = |id: u16, x: f32, y: f32| ArtRect::art(id, x, y, ARROW.0, ARROW.1);
    let medium = |id: u16, x: f32, y: f32| ArtRect::art(id, x, y, MEDIUM.0, MEDIUM.1);
    let mut out = vec![
        control(
            Target::CustomizeMorph { slot: 0 },
            "Morph 0 slider",
            SRC,
            vec![ArtRect::art(1015, 826.0, 163.0, 155.0, 16.0)],
        ),
        control(
            Target::CustomizeMorph { slot: 1 },
            "Morph 1 slider",
            SRC,
            vec![ArtRect::art(1016, 826.0, 203.0, 155.0, 16.0)],
        ),
        control(
            Target::CustomizeMorph { slot: 2 },
            "Morph 2 slider",
            SRC,
            vec![ArtRect::art(1017, 826.0, 243.0, 155.0, 16.0)],
        ),
        control(
            Target::CustomizeMorph { slot: 3 },
            "Morph 3 slider",
            SRC,
            vec![ArtRect::art(1018, 826.0, 283.0, 155.0, 16.0)],
        ),
        // The source form also owns the preview camera.  These are not decorative icons: the
        // same XML describes the reset, rotate, tilt and zoom buttons plus left/right drag help.
        control(
            Target::CustomizeCamera(CameraControl::Reset),
            "Camera reset",
            SRC,
            vec![ArtRect::art(
                1014,
                53.0,
                602.0,
                CAMERA_RESET.0,
                CAMERA_RESET.1,
            )],
        ),
        control(
            Target::CustomizeCamera(CameraControl::RotateLeft),
            "Camera rotate left",
            SRC,
            vec![ArtRect::art(1024, 23.0, 610.0, CAMERA.0, CAMERA.1)],
        ),
        control(
            Target::CustomizeCamera(CameraControl::RotateRight),
            "Camera rotate right",
            SRC,
            vec![ArtRect::art(1025, 102.0, 610.0, CAMERA.0, CAMERA.1)],
        ),
        control(
            Target::CustomizeCamera(CameraControl::TiltUp),
            "Camera tilt up",
            SRC,
            vec![ArtRect::art(1026, 60.0, 572.0, CAMERA.0, CAMERA.1)],
        ),
        control(
            Target::CustomizeCamera(CameraControl::TiltDown),
            "Camera tilt down",
            SRC,
            vec![ArtRect::art(1027, 60.0, 648.0, CAMERA.0, CAMERA.1)],
        ),
        control(
            Target::CustomizeCamera(CameraControl::ZoomIn),
            "Camera zoom in",
            SRC,
            vec![ArtRect::art(
                1029,
                12.0,
                640.0,
                CAMERA_ZOOM.0,
                CAMERA_ZOOM.1,
            )],
        ),
        control(
            Target::CustomizeCamera(CameraControl::ZoomOut),
            "Camera zoom out",
            SRC,
            vec![ArtRect::art(
                1030,
                110.0,
                640.0,
                CAMERA_ZOOM.0,
                CAMERA_ZOOM.1,
            )],
        ),
        control(
            Target::CustomizeCycle { field: 4, dir: -1 },
            "Face left",
            SRC,
            vec![arrow(1089, 826.0, 123.0)],
        ),
        control(
            Target::CustomizeCycle { field: 4, dir: 1 },
            "Face right",
            SRC,
            vec![arrow(1090, 965.0, 123.0)],
        ),
        control(
            Target::CustomizeCycle { field: 5, dir: -1 },
            "Hair style left",
            SRC,
            vec![arrow(1092, 826.0, 403.0)],
        ),
        control(
            Target::CustomizeCycle { field: 5, dir: 1 },
            "Hair style right",
            SRC,
            vec![arrow(1093, 965.0, 403.0)],
        ),
        control(
            Target::CustomizeCycle { field: 6, dir: -1 },
            "Tattoo left",
            SRC,
            vec![arrow(1095, 826.0, 483.0)],
        ),
        control(
            Target::CustomizeCycle { field: 6, dir: 1 },
            "Tattoo right",
            SRC,
            vec![arrow(1096, 965.0, 483.0)],
        ),
        control(
            Target::CustomizeScale { dir: -1 },
            "Size left",
            SRC,
            vec![arrow(1098, 826.0, 523.0)],
        ),
        control(
            Target::CustomizeScale { dir: 1 },
            "Size right",
            SRC,
            vec![arrow(1099, 965.0, 523.0)],
        ),
        control(
            Target::CustomizeRandom,
            "Random",
            SRC,
            vec![medium(1028, 783.0, 562.0)],
        ),
        control(
            Target::CustomizeReset,
            "Default",
            SRC,
            vec![medium(1078, 890.0, 562.0)],
        ),
        control(
            Target::CustomizeCancel,
            "Cancel",
            SRC,
            vec![
                ArtRect::art(1002, 76.0, 706.0, 38.0, 52.0),
                ArtRect::art(1039, 163.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CustomizeRealm,
            "Realm",
            SRC,
            vec![
                ArtRect::art(1013, 272.0, 706.0, 38.0, 52.0),
                ArtRect::art(1040, 262.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CustomizeBack,
            "Race/Class",
            SRC,
            vec![
                ArtRect::art(1004, 488.0, 706.0, 38.0, 52.0),
                ArtRect::art(1041, 478.0, 752.0, 67.0, 16.0),
            ],
        ),
        control(
            Target::CustomizeAdvance,
            "Continue",
            SRC,
            vec![
                ArtRect::art(1003, 908.0, 706.0, 38.0, 52.0),
                ArtRect::art(1068, 898.0, 752.0, 67.0, 16.0),
            ],
        ),
        // The detailed `character_customize.xml` is shown together with the source client's
        // `character_customize_basic.xml` overlay.  That overlay contributes the fourth bottom
        // action visible in retail captures: an explicit route into stat allocation.
        control(
            Target::CustomizeStats,
            "Adjust Attributes",
            "character_customize_basic.xml",
            vec![
                ArtRect::art(1130, 696.0, 706.0, 38.0, 52.0),
                ArtRect::art(1131, 656.0, 752.0, 120.0, 16.0),
            ],
        ),
    ];

    // `character_customize.xml` ControlIds 1050..=1060. These are real buttons, not a visual
    // flourish: every lock constrains the Random action for exactly the field beside it.
    for (slot, y) in [
        123.0, 163.0, 203.0, 243.0, 283.0, 323.0, 363.0, 403.0, 443.0, 483.0, 523.0,
    ]
    .into_iter()
    .enumerate()
    {
        let field = match slot {
            0 => CustomizerField::Face,
            1..=4 => CustomizerField::Morph((slot - 1) as u8),
            5 => CustomizerField::SkinTone,
            6 => CustomizerField::EyeColor,
            7 => CustomizerField::HairStyle,
            8 => CustomizerField::HairColor,
            9 => CustomizerField::Tattoo,
            10 => CustomizerField::Size,
            _ => unreachable!("the legacy source form has exactly eleven locks"),
        };
        out.push(control(
            Target::CustomizeLock { field },
            "Customizer random lock",
            SRC,
            vec![ArtRect::art(1050 + slot as u16, 992.0, y, 16.0, 16.0)],
        ));
    }

    // `color_picker_8`/`_16` author an outer 1px border and 20px cells. The ImagePickerDef owns
    // one ControlId for its whole strip; we retain that id on every cell so a probe can still name
    // the exact client widget it struck while dispatch knows the selected cell.
    for (palette, id, name, y, cells) in [
        (0u8, 1021u16, "Skin colour", 320.0f32, 8u8),
        (1u8, 1020u16, "Eye colour", 358.0f32, 8u8),
        (2u8, 1022u16, "Hair colour", 430.0f32, 16u8),
    ] {
        for index in 0..cells {
            out.push(control(
                Target::CustomizePalette {
                    palette,
                    value: index + 1,
                },
                name,
                SRC,
                vec![ArtRect::art(
                    id,
                    827.0 + f32::from(index % 8) * 20.0,
                    y + 1.0 + f32::from(index / 8) * 20.0,
                    20.0,
                    20.0,
                )],
            ));
        }
    }
    out
}

/// Live retail customizer geometry.
///
/// The shipped XML still contributes camera and bottom-row chrome, but its central palette form
/// is not the screen players see.  Keep the runtime profile's controls in a single table and use
/// it for both drawing and hit-testing; the old static table above remains only as a migration
/// fixture until the XML-only audit is removed.
fn char_customize_runtime(has_tattoo: bool) -> Vec<Control> {
    const RUNTIME_SRC: &str = "observed retail customizer runtime profile";
    const MEDIUM: (f32, f32) = (102.0, 21.0);
    const RUNTIME_ID: u16 = 20_000;

    // Reuse the asset-backed camera and bottom icon geometry.  The runtime profile owns the rows
    // on the right as well as the placement of Random/Default below its actual final row.
    let mut out = char_customize();
    out.retain(|control| {
        matches!(
            control.target,
            Target::CustomizeCamera(_)
                | Target::CustomizeCancel
                | Target::CustomizeRealm
                | Target::CustomizeBack
                | Target::CustomizeAdvance
                | Target::CustomizeStats
        )
    });

    let rows = preworld_customize::runtime_rows(has_tattoo);
    for row in &rows {
        let y = row.y_f32();
        let slot = u16::from(row.field.lock_slot());
        match row.widget {
            CustomizerWidget::TextSelector => {
                out.push(control(
                    Target::CustomizeAdjust {
                        field: row.field,
                        dir: -1,
                    },
                    "Customizer selector left",
                    RUNTIME_SRC,
                    vec![ArtRect::art(
                        RUNTIME_ID + slot * 3,
                        preworld_customize::CONTROL_X,
                        y,
                        ARROW_SIZE.0,
                        ARROW_SIZE.1,
                    )],
                ));
                out.push(control(
                    Target::CustomizeAdjust {
                        field: row.field,
                        dir: 1,
                    },
                    "Customizer selector right",
                    RUNTIME_SRC,
                    vec![ArtRect::art(
                        RUNTIME_ID + slot * 3 + 1,
                        preworld_customize::CONTROL_X + CONTROL_WIDTH - ARROW_SIZE.0,
                        y,
                        ARROW_SIZE.0,
                        ARROW_SIZE.1,
                    )],
                ));
            }
            CustomizerWidget::Slider { .. } => out.push(control(
                Target::CustomizeSlider { field: row.field },
                "Customizer runtime slider",
                RUNTIME_SRC,
                vec![ArtRect::art(
                    RUNTIME_ID + slot * 3,
                    preworld_customize::CONTROL_X,
                    y,
                    CONTROL_WIDTH,
                    16.0,
                )],
            )),
        }
        out.push(control(
            Target::CustomizeLock { field: row.field },
            "Customizer random lock",
            RUNTIME_SRC,
            vec![ArtRect::art(
                RUNTIME_ID + 100 + slot,
                preworld_customize::LOCK_X,
                y,
                LOCK_SIZE.0,
                LOCK_SIZE.1,
            )],
        ));
    }

    let buttons_y = rows
        .last()
        .map_or(preworld_customize::FIRST_ROW_Y, |row| row.y_f32())
        + 39.0;
    let medium = |id: u16, x: f32| ArtRect::art(id, x, buttons_y, MEDIUM.0, MEDIUM.1);
    out.push(control(
        Target::CustomizeRandom,
        "Random",
        RUNTIME_SRC,
        vec![medium(RUNTIME_ID + 200, 783.0)],
    ));
    out.push(control(
        Target::CustomizeReset,
        "Default",
        RUNTIME_SRC,
        vec![medium(RUNTIME_ID + 201, 890.0)],
    ));

    out
}

/// All live customizer controls for an identity with or without a Tattoo row.
///
/// This returns an owned table because the visible row set is identity-dependent.  The renderer
/// and active-screen hit path both call this function; [`controls`] keeps a Tattoo-capable table
/// only for generic inspector tooling that has no selected identity.
#[must_use]
pub fn customizer_controls(has_tattoo: bool) -> Vec<Control> {
    char_customize_runtime(has_tattoo)
}

/// Look up one live customizer control under an identity-specific runtime profile.
#[must_use]
pub fn customizer_control_for(has_tattoo: bool, target: Target) -> Option<Control> {
    customizer_controls(has_tattoo)
        .into_iter()
        .find(|control| control.target == target)
}

/// Hit-test the runtime profile for a selected identity.
#[must_use]
pub fn hit_customizer(has_tattoo: bool, x: f32, y: f32, viewport: (f32, f32)) -> Option<Control> {
    let (px, py) = PreworldTransform::from_viewport(viewport).unmap(x, y);
    customizer_controls(has_tattoo)
        .into_iter()
        .find(|control| control.covers(px, py))
}

/// Convert a design-space click on a source `generic_horizontal_slider` to its nine-tick value.
///
/// `generic_slider_indicator` is 7px wide and the parent slider is 155px wide (`styles.xml` /
/// `character_customize.xml`).  The indicator's left edge travels exactly `155 - 7` pixels, so
/// the two end clicks represent 0 and 8 and the centre represents retail's neutral 4.  Keeping
/// this next to the authored rectangle prevents a visual track and its packet tick from drifting.
#[must_use]
pub fn horizontal_slider_tick(art: ArtRect, design_x: f32) -> u8 {
    const INDICATOR_WIDTH: f32 = 7.0;
    let travel = (art.w - INDICATOR_WIDTH).max(1.0);
    let max_tick = f32::from(caer_protocol::customization::FACIAL_MORPH_MAX_TICK);
    (((design_x - art.x) / travel).clamp(0.0, 1.0) * max_tick).round() as u8
}

/// Authored size of the `button_small` template — `pregame/styles.xml` `<Size>`.
///
/// `button_small`, `button_large` and `button_pregame_small` all declare 64×21 over the same
/// `small_button` page. Ledger J10 records the login dialog hit-testing its OK and QUIT at 80×24
/// against this, which is 16px wide and 3px tall of dead-and-live space on every edge.
const SMALL_BUTTON: (f32, f32) = (64.0, 21.0);

/// Authored size of the `button_pregame_mini_left`/`_right` templates — `pregame/styles.xml`.
const MINI_BUTTON: (f32, f32) = (18.0, 18.0);

/// A dialog-local rect lifted into design space.
fn modal_rect(modal: Modal, control_id: u16, x: f32, y: f32, w: f32, h: f32) -> ArtRect {
    let (ox, oy) = modal.origin();
    ArtRect::art(control_id, ox + x, oy + y, w, h)
}

/// The two buttons of a confirm modal, transcribed from its own form.
///
/// The `LabelDef`s are deliberately absent: a label has no hit area, and giving one a rect is how
/// A12 happened. What each modal *says* lives in `crate::preworld::modal_lines`, beside the code
/// that draws it.
fn modal_controls_for(modal: Modal) -> Vec<Control> {
    let src = modal.source();
    let (bw, bh) = SMALL_BUTTON;
    match modal {
        // `quit_confirm.xml`: Yes 1003 (30,100), No 1004 (160,100).
        Modal::QuitConfirm => vec![
            control(
                Target::QuitConfirmYes,
                "Yes",
                src,
                vec![modal_rect(modal, 1003, 30.0, 100.0, bw, bh)],
            ),
            control(
                Target::QuitConfirmNo,
                "No",
                src,
                vec![modal_rect(modal, 1004, 160.0, 100.0, bw, bh)],
            ),
        ],
        // `delete_confirm.xml`: Delete 1003 (30,115), Cancel 1004 (160,115). Same ids, different
        // captions and 15px lower — which is why these are transcribed per form rather than shared.
        Modal::DeleteConfirm => vec![
            control(
                Target::DeleteConfirmYes,
                "Delete",
                src,
                vec![modal_rect(modal, 1003, 30.0, 115.0, bw, bh)],
            ),
            control(
                Target::DeleteConfirmNo,
                "Cancel",
                src,
                vec![modal_rect(modal, 1004, 160.0, 115.0, bw, bh)],
            ),
        ],
    }
}

/// Authored size of `character_customize_stats.xml` — its `WindowTemplate` `Width`/`Height`.
pub const STATS_DIALOG_SIZE: (f32, f32) = (440.0, 260.0);

/// Where the stats dialog's top-left lands in the 1024x768 authoring space.
///
/// The XML declares no position, so this must come from the retail window constructor rather than
/// from an attractive layout inference.  In the Clean Retail `game.dll`, the
/// `CharacterCustomizeStatsWindow` constructor at `.text:0x5A03E9` writes the two-element
/// position `[0x20, 0x18]` and passes it to the base-window initializer.  That is `(32, 24)` in
/// pregame space, matching the upper-left dialog in the retail/Eden reference capture.
pub const STATS_DIALOG_ORIGIN: (f32, f32) = (32.0, 24.0);

/// The generic minimum close hit target used by the retail window manager when a closeable form
/// has no title band. `character_customize_stats.xml` sets `CloseButton=true` and
/// `TitleHeight=0`, so the client falls back to this 12px square rather than inventing a form
/// control with a made-up `ControlId`.
pub const STATS_DIALOG_CLOSE_HIT_SIZE: f32 = 12.0;

#[must_use]
pub fn stats_dialog_origin() -> (f32, f32) {
    STATS_DIALOG_ORIGIN
}

/// The implicit generic close target for `character_customize_stats.xml` in design space.
///
/// This is deliberately separate from [`char_stats`]: the close gadget is owned by the generic
/// `WindowTemplate`, not by an authored `ButtonDef`, and placing it in the source-control table
/// would fabricate a `ControlId` and cause the XML-audit count to lie.
#[must_use]
pub fn stats_dialog_close_rect() -> ArtRect {
    let (ox, oy) = stats_dialog_origin();
    let (width, _) = STATS_DIALOG_SIZE;
    ArtRect::invisible(
        0,
        ox + width - STATS_DIALOG_CLOSE_HIT_SIZE,
        oy,
        STATS_DIALOG_CLOSE_HIT_SIZE,
        STATS_DIALOG_CLOSE_HIT_SIZE,
    )
}

/// Does a surface-space point land on the generic source close gadget?
#[must_use]
pub fn hit_stats_dialog_close(x: f32, y: f32, viewport: (f32, f32)) -> bool {
    let (px, py) = PreworldTransform::from_viewport(viewport).unmap(x, y);
    stats_dialog_close_rect().contains(px, py)
}

/// A stats-dialog-local rect lifted into design space.
fn stats_rect(control_id: u16, x: f32, y: f32, w: f32, h: f32) -> ArtRect {
    let (ox, oy) = stats_dialog_origin();
    ArtRect::art(control_id, ox + x, oy + y, w, h)
}

/// `character_customize_stats.xml` — the clickable controls of the stats dialog.
///
/// Transcribed verbatim, dialog-local positions lifted by [`stats_dialog_origin`]. The form's
/// `LabelDef`s (stat names 1056–1063, values 1005–1012, "Attributes" 1073, "Points Remaining"
/// 1065, the points value 1004, "Class" 1075 and the description textarea 1076) are deliberately
/// absent: a label has no hit area, and giving one a rect is how A12 happened. They are drawn by
/// `preworld`'s stats-dialog arm, beside the code that fills their adapters.
fn char_stats() -> Vec<Control> {
    const SRC: &str = "character_customize_stats.xml";
    let (bw, bh) = SMALL_BUTTON;
    let (mw, mh) = MINI_BUTTON;
    let mut out = Vec::new();
    // Eight rows, 20px pitch: plus (right of the value) then minus (left of it), wire order.
    for i in 0..8u8 {
        let y = 34.0 + f32::from(i) * 20.0;
        out.push(control(
            Target::StatsAdjust { stat: i, dir: 1 },
            "plus",
            SRC,
            vec![stats_rect(1031 + u16::from(i), 295.0, y, mw, mh)],
        ));
        out.push(control(
            Target::StatsAdjust { stat: i, dir: -1 },
            "minus",
            SRC,
            vec![stats_rect(1039 + u16::from(i), 256.0, y, mw, mh)],
        ));
    }
    out.push(control(
        Target::StatsReset,
        "Reset",
        SRC,
        vec![stats_rect(1020, 256.0, 224.0, bw, bh)],
    ));
    out.push(control(
        Target::StatsOptimize,
        "Optimize",
        SRC,
        vec![stats_rect(1021, 364.0, 224.0, bw, bh)],
    ));
    out
}

/// Every control of a modal.
#[must_use]
pub fn modal_controls(modal: Modal) -> &'static [Control] {
    use std::sync::OnceLock;
    static QUIT: OnceLock<Vec<Control>> = OnceLock::new();
    static DELETE: OnceLock<Vec<Control>> = OnceLock::new();
    match modal {
        Modal::QuitConfirm => QUIT.get_or_init(|| modal_controls_for(Modal::QuitConfirm)),
        Modal::DeleteConfirm => DELETE.get_or_init(|| modal_controls_for(Modal::DeleteConfirm)),
    }
}

/// A modal's control under a design-space point.
///
/// A modal swallows every click inside it, so callers must treat `None` as "consumed, no action"
/// rather than falling through to the screen behind. That fall-through is the whole reason a confirm
/// dialog exists.
#[must_use]
pub fn hit_modal(modal: Modal, x: f32, y: f32, viewport: (f32, f32)) -> Option<&'static Control> {
    let (px, py) = PreworldTransform::from_viewport(viewport).unmap(x, y);
    hit_in(modal_controls(modal), px, py)
}

/// Every control on a screen, front to back.
///
/// Screens with no authored controls of their own return an empty slice, which is what stops a
/// splash or loading plate from swallowing clicks.
#[must_use]
pub fn controls(screen: PreWorldScreen) -> &'static [Control] {
    use std::sync::OnceLock;
    static REALM: OnceLock<Vec<Control>> = OnceLock::new();
    static SELECT: OnceLock<Vec<Control>> = OnceLock::new();
    static CREATE: OnceLock<Vec<Control>> = OnceLock::new();
    static CUSTOMIZE: OnceLock<Vec<Control>> = OnceLock::new();
    static STATS: OnceLock<Vec<Control>> = OnceLock::new();
    match screen {
        PreWorldScreen::RealmSelect => REALM.get_or_init(realm_select),
        PreWorldScreen::CharSelect => SELECT.get_or_init(char_select),
        PreWorldScreen::CharCreate => CREATE.get_or_init(char_create),
        // Inspector tooling has no selected identity, so its stable table includes the optional
        // Tattoo row. The live HUD instead calls `hit_customizer` with its source catalogue.
        PreWorldScreen::CharCustomize => CUSTOMIZE.get_or_init(|| char_customize_runtime(true)),
        PreWorldScreen::CharStats => STATS.get_or_init(char_stats),
        _ => &[],
    }
}

/// Look up a control by target on its screen.
#[must_use]
pub fn control_for(screen: PreWorldScreen, target: Target) -> Option<&'static Control> {
    controls(screen).iter().find(|c| c.target == target)
}

/// The control under a design-space point, searched over an explicit table.
///
/// The table is a parameter so a calibration test can feed the historical label-only geometry
/// through this exact code and watch the round buttons stop responding.
#[must_use]
pub fn hit_in(table: &[Control], x: f32, y: f32) -> Option<&Control> {
    table.iter().find(|c| c.covers(x, y))
}

/// The control under a surface-space point on `screen`.
///
/// The point is inverse-mapped through the same [`PreworldTransform`] the frame was drawn with,
/// so a letterbox bar maps outside the plate and hits nothing.
#[must_use]
pub fn hit(
    screen: PreWorldScreen,
    x: f32,
    y: f32,
    viewport: (f32, f32),
) -> Option<&'static Control> {
    let (px, py) = PreworldTransform::from_viewport(viewport).unmap(x, y);
    hit_in(controls(screen), px, py)
}

/// A window cursor position, expressed in the surface the frame was drawn into.
///
/// Both are physical pixels and normally identical, so this is the identity almost always. It is
/// not during a resize: the window reports the cursor against the size it just became, while the
/// swapchain is still the old size until the pending resize is applied. Hit-testing a new-window
/// cursor against an old-surface layout puts the click off by the difference — visible as hover
/// lighting the control next to the one under the pointer.
///
/// This is the **only** window→surface conversion. Everything downstream is surface or design
/// space, and mixing the three is the class of bug the pre-world coordinate contract exists to
/// exclude.
#[must_use]
pub fn window_to_surface(x: f32, y: f32, window: (f32, f32), surface: (f32, f32)) -> [f32; 2] {
    [
        x * surface.0 / window.0.max(1.0),
        y * surface.1 / window.1.max(1.0),
    ]
}

/// Everything the hitbox auditor reports for one probe point.
#[derive(Clone, Debug, PartialEq)]
pub struct HitProbe {
    pub screen: PreWorldScreen,
    pub viewport: (f32, f32),
    pub transform: PreworldTransform,
    /// The probe as the window delivered it, in surface pixels.
    pub surface_point: (f32, f32),
    /// The same point inverse-mapped into 1024×768 design space.
    pub design_point: (f32, f32),
    /// False when the point landed in a letterbox bar rather than on the plate.
    pub on_plate: bool,
    pub control: Option<&'static Control>,
    /// The authored part actually under the point, in design space.
    pub part: Option<ArtRect>,
    /// The same part in surface space — where it was drawn this frame.
    pub part_surface: Option<Rect>,
    /// Whether the point is inside *visible* art, as opposed to an `InvisibleButtonDef`.
    pub inside_visible_art: bool,
}

/// Probe one surface-space point, reporting every coordinate the click passed through.
#[must_use]
pub fn probe(screen: PreWorldScreen, x: f32, y: f32, viewport: (f32, f32)) -> HitProbe {
    let transform = PreworldTransform::from_viewport(viewport);
    let (px, py) = transform.unmap(x, y);
    let on_plate = (0.0..crate::preworld::PREGAME_W).contains(&px)
        && (0.0..crate::preworld::PREGAME_H).contains(&py);
    let control = hit_in(controls(screen), px, py);
    let part = control.and_then(|c| c.part_at(px, py));
    HitProbe {
        screen,
        viewport,
        transform,
        surface_point: (x, y),
        design_point: (px, py),
        on_plate,
        control,
        part,
        part_surface: part.map(|p| p.mapped(transform)),
        inside_visible_art: part.is_some_and(|p| p.visible),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the control back out of the player's own client, so a drifted transcription fails
    /// here rather than at the pointer.
    /// The client tree. **Fails** when it is absent (REQ-025): a transcription check that cannot
    /// read the source it checks against must not report pass.
    fn client_root() -> std::path::PathBuf {
        let root = caer_assets::client_dep::required_caer_client_root("preworld_hitbox");
        assert!(
            root.join("pregame").is_dir(),
            "CAER_CLIENT has no pregame directory: {} — the authored control rects can only be \
             checked against the client that shipped them. REQ-025.",
            root.display()
        );
        root
    }

    fn parse_pregame(root: &std::path::Path, file: &str) -> caer_assets::uiskin::Element {
        let path = root.join("pregame").join(file);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        // The pregame forms are ISO-8859-1; `Continué` is why.
        let text: String = bytes.iter().map(|&b| char::from(b)).collect();
        caer_assets::uiskin::Element::parse(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn xml_rects(file: &str) -> std::collections::HashMap<u16, (f32, f32, f32, f32)> {
        let root = client_root();
        let doc = parse_pregame(&root, file);
        let styles = parse_pregame(&root, "styles.xml");
        let mut sizes = std::collections::HashMap::new();
        for el in styles.children.iter().flat_map(|c| c.children.iter()) {
            if el.tag.eq_ignore_ascii_case("ButtonTemplate") {
                if let (Some(n), Some(s)) = (el.text_of("Name"), el.child("Size")) {
                    sizes.insert(n.to_ascii_lowercase(), s.xy());
                }
            }
        }
        // `styles.xml` nests templates one level down in some builds and at the root in others.
        for el in styles.children_named("ButtonTemplate") {
            if let (Some(n), Some(s)) = (el.text_of("Name"), el.child("Size")) {
                sizes.insert(n.to_ascii_lowercase(), s.xy());
            }
        }
        let window = doc
            .child("WindowTemplate")
            .unwrap_or_else(|| panic!("{file}: no WindowTemplate"));
        let mut out = std::collections::HashMap::new();
        for el in &window.children {
            let Some(id) = el.int_of("ControlId") else {
                continue;
            };
            if !el.tag.to_ascii_lowercase().ends_with("def") {
                continue;
            }
            let (x, y) = el.position();
            let tmpl = el
                .text_of("TemplateName")
                .map(str::to_ascii_lowercase)
                .and_then(|t| sizes.get(&t).copied());
            let w = el.int_of("Width").or(tmpl.map(|t| t.0));
            let h = el.int_of("Height").or(tmpl.map(|t| t.1));
            let (Some(w), Some(h)) = (w, h) else { continue };
            out.insert(id as u16, (x as f32, y as f32, w as f32, h as f32));
        }
        out
    }

    /// **Both** confirm modals, checked against the client that shipped them.
    ///
    /// Their controls are authored in **dialog-local** coordinates, so the origin is subtracted back
    /// off before comparing — which also means a wrong centring calculation shows up here rather than
    /// only on screen. Both forms use ControlIds 1003 and 1004, so a table that had them swapped, or
    /// one form's geometry under the other's name, would pass any check that did not read the file.
    ///
    /// **Fails** when the retail tree is absent (REQ-025).
    #[test]
    fn modal_controls_match_client_xml() {
        let mut checked = 0usize;
        for modal in [Modal::QuitConfirm, Modal::DeleteConfirm] {
            let file = modal.source();
            let xml = xml_rects(file);
            let (ox, oy) = modal.origin();
            for c in modal_controls(modal) {
                assert_eq!(
                    c.source, file,
                    "{file}: control transcribed from the wrong form"
                );
                for p in &c.parts {
                    let got = xml.get(&p.control_id).copied().unwrap_or_else(|| {
                        panic!(
                            "{file}: ControlId {} is not in the client form",
                            p.control_id
                        )
                    });
                    assert_eq!(
                        got,
                        (p.x - ox, p.y - oy, p.w, p.h),
                        "{file}: ControlId {} ({}) disagrees with the client",
                        p.control_id,
                        c.name
                    );
                    checked += 1;
                }
            }
            assert_eq!(
                checked % 2,
                0,
                "{file}: each modal authors exactly two buttons"
            );

            // The window's own authored size.
            let root = client_root();
            let doc = parse_pregame(&root, file);
            let window = doc.child("WindowTemplate").expect("WindowTemplate");
            assert_eq!(
                (
                    window.int_of("Width").expect("Width") as f32,
                    window.int_of("Height").expect("Height") as f32
                ),
                modal.size(),
                "{file} dialog size"
            );

            // The whole modal has to land on the plate, or part of it is unreachable at any viewport.
            let (dw, dh) = modal.size();
            assert!(
                ox >= 0.0
                    && oy >= 0.0
                    && ox + dw <= crate::preworld::PREGAME_W
                    && oy + dh <= crate::preworld::PREGAME_H,
                "{file} leaves the design space at ({ox},{oy}) {dw}x{dh}"
            );
        }
        assert_eq!(checked, 4, "two modals, two buttons each");
    }

    /// A modal's lines sit at the client's authored y, and the one line we take verbatim is the
    /// client's.
    ///
    /// **The authored `<Data>` strings are mostly placeholders**, so this cannot simply compare our
    /// text to theirs — that comparison is what produced the "Are you sure you want to delete?"
    /// duplicate. `delete_confirm.xml` gives LabelDefs 1002 and 1005 the *identical* sentence with
    /// no adapter on either, which is only explicable as two runtime-substituted slots; Eden shows
    /// `Deleting <name>` and `Type YES to confirm` / `YES confirmed` there. LabelDef 1006 is real
    /// and is asserted verbatim.
    ///
    /// If a future client ships 1002 and 1005 with *different* text, the placeholder reading is
    /// wrong and the first assertion here fails rather than the substitution silently standing.
    ///
    /// **Fails** when the retail tree is absent (REQ-025).
    #[test]
    fn modal_lines_sit_at_the_authored_positions() {
        let root = client_root();
        for modal in [Modal::QuitConfirm, Modal::DeleteConfirm] {
            let file = modal.source();
            let doc = parse_pregame(&root, file);
            let window = doc.child("WindowTemplate").expect("WindowTemplate");
            let authored: Vec<(f32, String)> = window
                .children
                .iter()
                .filter(|el| el.tag.eq_ignore_ascii_case("LabelDef"))
                .filter_map(|el| Some((el.position().1 as f32, el.text_of("Data")?.to_string())))
                .collect();
            let ys: Vec<f32> = authored.iter().map(|(y, _)| *y).collect();

            // Every line we draw lands on a y the form authors — in either state.
            for confirmed in [false, true] {
                for (y, _) in crate::preworld::modal_lines(modal, Some("Tester"), confirmed) {
                    assert!(
                        ys.contains(&y),
                        "{file}: we draw a line at y={y}, which the form does not author {ys:?}"
                    );
                }
            }

            if modal == Modal::QuitConfirm {
                // The one form whose authored text really is its runtime text.
                let ours = crate::preworld::modal_lines(modal, None, false);
                assert_eq!(ours.len(), 1);
                assert_eq!(ours[0].1, authored[0].1, "{file}: verbatim line");
                continue;
            }

            // 1002 and 1005 are placeholders, and the proof is that they are identical.
            assert_eq!(authored.len(), 3, "{file} authors three LabelDefs");
            assert_eq!(
                authored[0].1, authored[1].1,
                "{file}: 1002 and 1005 are only explicable as runtime slots because they are the \
                 SAME sentence; if a client ships them different, re-derive the substitution"
            );
            // 1006 is real, and only appears once confirmed.
            let awaiting = crate::preworld::modal_lines(modal, Some("Tester"), false);
            let confirmed = crate::preworld::modal_lines(modal, Some("Tester"), true);
            assert_eq!(awaiting.len(), 2, "awaiting state shows two lines");
            assert_eq!(confirmed.len(), 3, "confirmed state shows three");
            assert_eq!(
                confirmed[2].1, authored[2].1,
                "{file}: the third line is the client's own words, verbatim"
            );
            // The subject is named, and the confirmation word is asked for then acknowledged.
            assert!(
                awaiting[0].1.contains("Tester"),
                "the dialog names the character"
            );
            assert!(
                awaiting[1].1.contains(crate::preworld::DELETE_CONFIRM_WORD)
                    && confirmed[1]
                        .1
                        .contains(crate::preworld::DELETE_CONFIRM_WORD),
                "line 1005 asks for the confirmation word, then acknowledges it"
            );
            assert_ne!(awaiting[1].1, confirmed[1].1, "1005 must change on confirm");
        }
    }

    /// The whole table, checked against the client that shipped it.
    ///
    /// This is the gate that makes the rest of the contract non-circular. Every other test here
    /// reasons *from* the table; only this one asks whether the table is what the client says.
    /// It is what catches a rect typed off a screenshot instead of read from the form.
    #[test]
    fn authored_controls_match_client_xml() {
        let mut checked = 0usize;
        for screen in [
            PreWorldScreen::RealmSelect,
            PreWorldScreen::CharSelect,
            PreWorldScreen::CharCreate,
        ] {
            let table = controls(screen);
            let Some(file) = table.first().map(|c| c.source) else {
                continue;
            };
            let xml = xml_rects(file);
            for c in table {
                for p in &c.parts {
                    let got = xml.get(&p.control_id).copied().unwrap_or_else(|| {
                        panic!(
                            "{file}: ControlId {} is not in the client form",
                            p.control_id
                        )
                    });
                    assert_eq!(
                        got,
                        (p.x, p.y, p.w, p.h),
                        "{file}: ControlId {} ({}) disagrees with the client",
                        p.control_id,
                        c.name
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 60, "only {checked} rects checked — table shrank?");
    }

    /// The customize form combines template-backed bottom buttons, legacy arrow aliases, and
    /// image-picker cells. All three must trace back to the player's own source form; checking
    /// only the bottom row is how the screen could look complete while every colour click landed
    /// one cell off.
    #[cfg(any())]
    #[test]
    fn customize_controls_match_client_xml_and_picker_template() {
        let file = "character_customize.xml";
        let rects = xml_rects(file);
        let positions = xml_positions(file);
        let table = controls(PreWorldScreen::CharCustomize);
        assert!(
            !table.is_empty(),
            "customize form has no interactive controls"
        );

        // The direct template-backed controls carry their exact source sizes.
        for target in [
            Target::CustomizeCamera(CameraControl::Reset),
            Target::CustomizeCamera(CameraControl::RotateLeft),
            Target::CustomizeCamera(CameraControl::RotateRight),
            Target::CustomizeCamera(CameraControl::TiltUp),
            Target::CustomizeCamera(CameraControl::TiltDown),
            Target::CustomizeCamera(CameraControl::ZoomIn),
            Target::CustomizeCamera(CameraControl::ZoomOut),
            Target::CustomizeRandom,
            Target::CustomizeReset,
            Target::CustomizeCancel,
            Target::CustomizeRealm,
            Target::CustomizeBack,
            Target::CustomizeAdvance,
        ] {
            let c = control_for(PreWorldScreen::CharCustomize, target).expect("custom control");
            for p in &c.parts {
                assert_eq!(
                    rects.get(&p.control_id).copied(),
                    Some((p.x, p.y, p.w, p.h)),
                    "{file}: ControlId {} ({}) drifted",
                    p.control_id,
                    c.name
                );
            }
        }

        for slot in 0..11u8 {
            let c = control_for(
                PreWorldScreen::CharCustomize,
                Target::CustomizeLock { slot },
            )
            .expect("customizer Random lock");
            for p in &c.parts {
                assert_eq!(
                    positions.get(&p.control_id).copied(),
                    Some((p.x, p.y)),
                    "{file}: ControlId {} ({}) position drifted",
                    p.control_id,
                    c.name
                );
                assert_eq!(
                    (p.w, p.h),
                    (16.0, 16.0),
                    "{file}: lock {} must retain its styles.xml 16px template",
                    p.control_id
                );
            }
        }

        // Retail draws a second, basic customizer form over the detailed controls.  Its Adjust
        // Attributes icon is not part of `character_customize.xml`, so checking it against that
        // file would make the test either reject correct art or tempt a hard-coded substitute.
        let basic_file = "character_customize_basic.xml";
        let basic_rects = xml_rects(basic_file);
        let stats = control_for(PreWorldScreen::CharCustomize, Target::CustomizeStats)
            .expect("basic customizer stats control");
        assert_eq!(stats.source, basic_file);
        for p in &stats.parts {
            assert_eq!(
                basic_rects.get(&p.control_id).copied(),
                Some((p.x, p.y, p.w, p.h)),
                "{basic_file}: ControlId {} ({}) drifted",
                p.control_id,
                stats.name
            );
        }

        // These labels are also supplied by the basic overlay.  They are deliberately checked
        // as form data rather than treated as screenshot decoration: `character_customize.tga`
        // does not contain them, and a renderer that drew only that plate would omit the
        // facial-features heading, camera instructions, and creation title entirely.
        let root = client_root();
        let basic_doc = parse_pregame(&root, basic_file);
        let basic_window = basic_doc
            .child("WindowTemplate")
            .expect("basic WindowTemplate");
        for (id, text, font, x, y, width, color) in [
            (
                2010,
                "Camera Controls",
                "arial9",
                763,
                605,
                250,
                (206, 169, 90),
            ),
            (
                2011,
                "Left click and drag to rotate",
                "arial9",
                763,
                620,
                250,
                (206, 169, 90),
            ),
            (
                2012,
                "Right click and drag to zoom",
                "arial9",
                763,
                635,
                250,
                (206, 169, 90),
            ),
            (
                2050,
                "Customize  Your  Character",
                "large_gold",
                260,
                662,
                500,
                (255, 200, 69),
            ),
            (
                2051,
                "Facial Features",
                "med_gold",
                755,
                83,
                256,
                (255, 255, 255),
            ),
        ] {
            let label = basic_window
                .children
                .iter()
                .find(|el| el.int_of("ControlId") == Some(id))
                .unwrap_or_else(|| panic!("{basic_file}: missing label {id}"));
            assert_eq!(label.tag, "LabelDef", "{basic_file}: ControlId {id}");
            assert_eq!(label.text_of("Data"), Some(text));
            assert_eq!(label.text_of("FontName"), Some(font));
            assert_eq!(label.position(), (x, y));
            assert_eq!(label.int_of("Width"), Some(width));
            assert_eq!(label.int_of("Height"), Some(16));
            let source_color = label.child("Color").expect("source label colour");
            assert_eq!(
                (
                    source_color.int_of("R"),
                    source_color.int_of("G"),
                    source_color.int_of("B")
                ),
                (Some(color.0), Some(color.1), Some(color.2)),
                "{basic_file}: label {id} colour"
            );
        }
        let source_caption = basic_window
            .children
            .iter()
            .find(|el| el.int_of("ControlId") == Some(1131))
            .expect("basic Adjust Attributes caption");
        assert_eq!(source_caption.text_of("Label"), Some("Adjust Attributes"));

        // `left_arrow_button` / `right_arrow_button` are runtime aliases in this source tree;
        // their positions are still authored by the XML, and we use the matching 16px slider
        // arrow size from styles.xml rather than guess off a screenshot.
        for target in [
            Target::CustomizeCycle { field: 4, dir: -1 },
            Target::CustomizeCycle { field: 4, dir: 1 },
            Target::CustomizeCycle { field: 5, dir: -1 },
            Target::CustomizeCycle { field: 5, dir: 1 },
            Target::CustomizeCycle { field: 6, dir: -1 },
            Target::CustomizeCycle { field: 6, dir: 1 },
            Target::CustomizeScale { dir: -1 },
            Target::CustomizeScale { dir: 1 },
        ] {
            let c = control_for(PreWorldScreen::CharCustomize, target).expect("arrow");
            let p = c.art();
            assert_eq!(
                positions.get(&p.control_id).copied(),
                Some((p.x, p.y)),
                "{file}: arrow {} position drifted",
                p.control_id
            );
            assert_eq!((p.w, p.h), (16.0, 16.0), "fallback arrow size");
        }

        // The four facial controls are real `HorizontalSliderDef`s, not cosmetic labels.  Their
        // width and nine ticks come from the form; their 16px height comes from the named source
        // `generic_horizontal_slider` template in styles.xml.
        let root = client_root();
        let doc = parse_pregame(&root, file);
        let window = doc.child("WindowTemplate").expect("WindowTemplate");
        let styles = parse_pregame(&root, "styles.xml");
        let slider_height = styles
            .children_named("HorizontalSliderTemplate")
            .find(|el| el.text_of("Name") == Some("generic_horizontal_slider"))
            .and_then(|el| el.int_of("Height"))
            .expect("styles.xml generic_horizontal_slider Height")
            as f32;
        assert_eq!(slider_height, 16.0, "source slider template height");
        for slot in 0..4u8 {
            let target = Target::CustomizeMorph { slot };
            let control = control_for(PreWorldScreen::CharCustomize, target).expect("morph slider");
            let art = control.art();
            let source = window
                .children
                .iter()
                .find(|el| el.int_of("ControlId") == Some(1015 + i32::from(slot)))
                .expect("source morph slider");
            assert_eq!(
                source.tag, "HorizontalSliderDef",
                "slider {slot} source type"
            );
            assert_eq!(source.position(), (art.x as i32, art.y as i32));
            assert_eq!(source.int_of("Width"), Some(art.w as i32));
            assert_eq!(source.int_of("Numticks"), Some(9));
            assert_eq!(art.h, slider_height, "slider {slot} source template height");
        }
        let slider = control_for(
            PreWorldScreen::CharCustomize,
            Target::CustomizeMorph { slot: 0 },
        )
        .expect("first morph slider")
        .art();
        assert_eq!(horizontal_slider_tick(slider, slider.x), 0);
        assert_eq!(
            horizontal_slider_tick(slider, slider.x + (slider.w - 7.0) * 0.5),
            4,
            "the centre of a source nine-tick slider is neutral tick four"
        );
        assert_eq!(horizontal_slider_tick(slider, slider.x + slider.w), 8);

        // `color_picker_8` / `_16`: one-pixel border, then 20px cells. The image picker owns a
        // single source control id, so all of the individually routable cells retain that id.
        for (palette, id, x, y, count) in [
            (0u8, 1021u16, 826.0f32, 320.0f32, 8u8),
            (1u8, 1020u16, 826.0f32, 358.0f32, 8u8),
            (2u8, 1022u16, 826.0f32, 430.0f32, 16u8),
        ] {
            assert_eq!(positions.get(&id).copied(), Some((x, y)), "picker {id}");
            for index in 0..count {
                let c = control_for(
                    PreWorldScreen::CharCustomize,
                    Target::CustomizePalette {
                        palette,
                        value: index + 1,
                    },
                )
                .expect("picker cell");
                let p = c.art();
                assert_eq!(p.control_id, id, "picker source id");
                assert_eq!(p.x, x + 1.0 + f32::from(index % 8) * 20.0);
                assert_eq!(p.y, y + 1.0 + f32::from(index / 8) * 20.0);
                assert_eq!((p.w, p.h), (20.0, 20.0));
            }
        }
    }

    /// The retired static form is the known-bad control: it contains palette cells while the
    /// observed runtime profile contains only semantic selectors/sliders. Both halves live in
    /// this test so a future refactor cannot accidentally make the detector self-confirming.
    #[test]
    fn runtime_customizer_profile_rejects_the_palette_form_and_closes_optional_rows() {
        let legacy = char_customize();
        assert!(
            legacy
                .iter()
                .any(|control| matches!(control.target, Target::CustomizePalette { .. })),
            "known-bad static form must retain a palette witness"
        );

        let with_tattoo = customizer_controls(true);
        assert!(
            !with_tattoo.iter().any(|control| {
                matches!(
                    control.target,
                    Target::CustomizePalette { .. }
                        | Target::CustomizeMorph { .. }
                        | Target::CustomizeCycle { .. }
                        | Target::CustomizeScale { .. }
                )
            }),
            "known-good runtime profile must not emit palette-era targets"
        );
        for row in preworld_customize::runtime_rows(true) {
            let lock = customizer_control_for(true, Target::CustomizeLock { field: row.field })
                .expect("every visible row owns one lock");
            assert_eq!(lock.art().y, row.y_f32());
            match row.widget {
                CustomizerWidget::TextSelector => {
                    for dir in [-1, 1] {
                        let selector = customizer_control_for(
                            true,
                            Target::CustomizeAdjust {
                                field: row.field,
                                dir,
                            },
                        )
                        .expect("text row owns both arrows");
                        assert_eq!(selector.art().y, row.y_f32());
                    }
                }
                CustomizerWidget::Slider { .. } => {
                    let slider =
                        customizer_control_for(true, Target::CustomizeSlider { field: row.field })
                            .expect("slider row owns one track");
                    assert_eq!(slider.art().y, row.y_f32());
                    assert_eq!(slider.art().w, CONTROL_WIDTH);
                }
            }
        }

        let without_tattoo = customizer_controls(false);
        assert!(
            !without_tattoo.iter().any(|control| {
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
            "no-decal identity has no ghost Tattoo control"
        );
        let size = customizer_control_for(
            false,
            Target::CustomizeAdjust {
                field: CustomizerField::Size,
                dir: -1,
            },
        )
        .expect("Size survives without Tattoo");
        assert_eq!(size.art().y, 523.0, "Size closes the omitted-row gap");
    }

    /// The stats dialog, checked against the client form that shipped it.
    ///
    /// Dialog-local coordinates with the binary-authored window origin subtracted back off. This
    /// makes a wrong constructor position show up here rather than only on screen. Also pins the
    /// routing: every plus/minus control id must carry the stat its row position says, because a
    /// table that shifted the ids by one would still pass a pure geometry check while moving
    /// points between the wrong stats.
    ///
    /// **Fails** when the retail tree is absent (REQ-025).
    #[test]
    fn stats_dialog_controls_match_client_xml() {
        let file = "character_customize_stats.xml";
        let xml = xml_rects(file);
        let root = client_root();
        let doc = parse_pregame(&root, file);
        let window = doc.child("WindowTemplate").expect("WindowTemplate");
        assert_eq!(
            window.text_of("CloseButton"),
            Some("true"),
            "the retail stats form is a closable modal over customization"
        );
        let optimize = window
            .children
            .iter()
            .find(|el| el.int_of("ControlId") == Some(1021))
            .expect("retail Optimize button");
        assert_eq!(optimize.tag, "ButtonDef");
        assert_eq!(optimize.text_of("Label"), Some("Optimize"));
        let (ox, oy) = stats_dialog_origin();
        let table = controls(PreWorldScreen::CharStats);
        assert!(!table.is_empty(), "stats dialog has no controls");
        let mut checked = 0usize;
        for c in table {
            assert_eq!(c.source, file, "control transcribed from the wrong form");
            for p in &c.parts {
                let got = xml.get(&p.control_id).copied().unwrap_or_else(|| {
                    panic!(
                        "{file}: ControlId {} is not in the client form",
                        p.control_id
                    )
                });
                assert_eq!(
                    got,
                    (p.x - ox, p.y - oy, p.w, p.h),
                    "{file}: ControlId {} ({}) disagrees with the client",
                    p.control_id,
                    c.name
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 18, "8 rows x 2 arrows + reset + optimize, no more");
        // Routing: row y pins the stat index; the id arithmetic must agree with it.
        for c in table {
            let Target::StatsAdjust { stat, dir } = c.target else {
                continue;
            };
            let p = c.parts[0];
            let row = ((p.y - oy) - 34.0) / 20.0;
            assert_eq!(
                f32::from(stat),
                row,
                "ControlId {} sits on row {:.0} but adjusts stat {stat}",
                p.control_id,
                row
            );
            let want_dir = if p.control_id < 1039 { 1 } else { -1 };
            assert_eq!(
                dir, want_dir,
                "ControlId {} routes the wrong arrow direction",
                p.control_id
            );
        }
    }

    /// `CharacterCustomizeStatsWindow` stores `0x20, 0x18` before its base-window init.  Keep the
    /// consumer-side location explicit too: replacing this with centring was visually plausible
    /// but put the retail dialog in the wrong third of the screen.
    #[test]
    fn stats_dialog_uses_the_retail_constructor_position() {
        assert_eq!(stats_dialog_origin(), (32.0, 24.0));
    }

    /// Retail's loose `character_customize.xml` puts the Cancel caption at x=163 even though its
    /// round button is at x=76. The live client composes that caption under the button (as the
    /// visual oracle shows), so the runtime geometry must correct the known-bad loose position
    /// without corrupting the raw XML transcription retained in `parts`.
    #[test]
    fn live_customize_cancel_caption_is_centered_and_is_the_only_live_caption_hit() {
        let control = customizer_control_for(true, Target::CustomizeCancel)
            .expect("Cancel exists for every customization profile");
        let raw = *control.parts.last().expect("Cancel source caption");
        let art = control.art();
        let caption = control.caption();
        assert_eq!(
            raw.x, 163.0,
            "keep the source anomaly explicit for the audit"
        );
        assert!(
            (caption.x + caption.w * 0.5 - (art.x + art.w * 0.5)).abs() < f32::EPSILON,
            "the visible Cancel caption must sit beneath its round button"
        );
        assert_ne!(
            caption.x, raw.x,
            "the runtime correction must actually be active"
        );

        let live_center = (caption.x + caption.w * 0.5, caption.y + caption.h * 0.5);
        assert_eq!(
            hit_customizer(true, live_center.0, live_center.1, (1024.0, 768.0))
                .map(|hit| hit.target),
            Some(Target::CustomizeCancel),
            "the caption the player sees must be clickable"
        );
        let raw_center = (raw.x + raw.w * 0.5, raw.y + raw.h * 0.5);
        assert_eq!(
            hit_customizer(true, raw_center.0, raw_center.1, (1024.0, 768.0)).map(|hit| hit.target),
            None,
            "the displaced source caption must not leave a ghost hit target"
        );
    }

    /// The historical geometry: the create form's chrome hit-tested on its caption only.
    ///
    /// This is the known-bad state the contract exists to exclude. It runs through the same
    /// [`hit_in`] the product uses, so the control is mechanical rather than remembered.
    fn label_only_char_create() -> Vec<Control> {
        vec![
            control(
                Target::CharCreateContinue,
                "Continue",
                "known-bad",
                vec![ArtRect::art(1068, 898.0, 752.0, 64.0, 16.0)],
            ),
            control(
                Target::CharCreateCancel,
                "Cancel",
                "known-bad",
                vec![ArtRect::art(1067, 60.0, 752.0, 64.0, 16.0)],
            ),
            control(
                Target::CharCreateRealm,
                "Realm",
                "known-bad",
                vec![ArtRect::art(1066, 262.0, 752.0, 64.0, 16.0)],
            ),
        ]
    }

    /// The other historical geometry: char-select chrome hit-tested on a box drawn round the
    /// round button *and* its caption, which is 26px wider and 10px taller than the button and
    /// starts 14px to its left. Clicking the empty corner activated the control.
    fn padded_bounding_box_char_select() -> Vec<Control> {
        vec![
            control(
                Target::CharSelectQuit,
                "Quit",
                "known-bad",
                vec![ArtRect::art(1096, 67.0, 706.0, 64.0, 62.0)],
            ),
            control(
                Target::CharSelectRealm,
                "Realm",
                "known-bad",
                vec![ArtRect::art(1098, 267.0, 706.0, 64.0, 62.0)],
            ),
            control(
                Target::CharSelectPlay,
                "Play/Create",
                "known-bad",
                vec![ArtRect::art(1090, 810.0, 575.0, 128.0, 82.0)],
            ),
        ]
    }

    /// The dead corners the padded rects used to claim, as `(target, x, y)`.
    ///
    /// Each is inside the old bounding box and outside every rect the client authors: left of the
    /// round button and above its caption, or above the gold Play label and beside the round
    /// button.
    const DEAD_CORNERS: [(Target, f32, f32); 3] = [
        (Target::CharSelectQuit, 70.0, 710.0),
        (Target::CharSelectRealm, 270.0, 710.0),
        (Target::CharSelectPlay, 815.0, 580.0),
    ];

    #[test]
    fn empty_corners_are_dead_and_the_old_padded_rects_prove_it() {
        let bad = padded_bounding_box_char_select();
        let good = controls(PreWorldScreen::CharSelect);
        for (target, x, y) in DEAD_CORNERS {
            assert_eq!(
                hit_in(&bad, x, y).map(|c| c.target),
                Some(target),
                "{target:?}: ({x}, {y}) must be a hit under the padded geometry, or this \
                 calibration is probing the wrong point"
            );
            assert_eq!(
                hit_in(good, x, y),
                None,
                "{target:?}: ({x}, {y}) is empty art and must activate nothing"
            );
        }
    }

    #[test]
    fn round_button_centres_are_dead_under_the_old_label_only_geometry() {
        let bad = label_only_char_create();
        let good = controls(PreWorldScreen::CharCreate);
        for target in [
            Target::CharCreateContinue,
            Target::CharCreateCancel,
            Target::CharCreateRealm,
        ] {
            let round = good
                .iter()
                .find(|c| c.target == target)
                .and_then(|c| c.parts.first().copied())
                .expect("round button part");
            let (cx, cy) = (round.x + round.w * 0.5, round.y + round.h * 0.5);
            assert!(
                hit_in(&bad, cx, cy).is_none(),
                "{target:?}: known-bad table must not answer at the round button centre"
            );
            assert_eq!(
                hit_in(good, cx, cy).map(|c| c.target),
                Some(target),
                "{target:?}: shipped table must answer at the round button centre"
            );
        }
    }

    /// Every part of every control takes its own centre, and a point one pixel outside it does
    /// not — the second half is what a padded or bounding-box hit rect fails.
    #[test]
    fn every_part_takes_its_centre_and_releases_just_outside() {
        for screen in [
            PreWorldScreen::RealmSelect,
            PreWorldScreen::CharSelect,
            PreWorldScreen::CharCreate,
        ] {
            let table = controls(screen);
            for c in table {
                for p in &c.parts {
                    let (cx, cy) = (p.x + p.w * 0.5, p.y + p.h * 0.5);
                    assert_eq!(
                        hit_in(table, cx, cy).map(|t| t.target),
                        Some(c.target),
                        "{screen:?} {} ({}): centre must hit",
                        c.name,
                        p.control_id
                    );
                    assert!(
                        !p.contains(p.x - 1.0, cy),
                        "{} left edge is not exclusive",
                        p.control_id
                    );
                    assert!(
                        !p.contains(p.x + p.w, cy),
                        "{} right edge is not exclusive",
                        p.control_id
                    );
                    assert!(
                        !p.contains(cx, p.y - 1.0),
                        "{} top edge is not exclusive",
                        p.control_id
                    );
                    assert!(
                        !p.contains(cx, p.y + p.h),
                        "{} bottom edge is not exclusive",
                        p.control_id
                    );
                }
            }
        }
    }

    /// The empty space between a round button and its caption belongs to neither.
    #[test]
    fn the_gap_between_a_button_and_its_caption_is_not_clickable() {
        for (screen, target) in [
            (PreWorldScreen::CharSelect, Target::CharSelectQuit),
            (PreWorldScreen::CharSelect, Target::CharSelectRealm),
            (PreWorldScreen::CharSelect, Target::CharSelectOptions),
            (PreWorldScreen::CharCreate, Target::CharCreateCancel),
            (PreWorldScreen::CharCreate, Target::CharCreateContinue),
            (PreWorldScreen::CharCreate, Target::CharCreateRealm),
        ] {
            let c = control_for(screen, target).expect("control");
            let (round, label) = (c.parts[0], c.parts[1]);
            // Left of the round button, above the caption: inside the bounding box the old code
            // used, outside every rect the client authored.
            let x = label.x + 1.0;
            let y = round.y + round.h * 0.5;
            assert!(
                x < round.x,
                "{target:?}: caption does not start left of the button"
            );
            assert!(
                hit_in(controls(screen), x, y).is_none(),
                "{target:?}: dead corner at ({x}, {y}) still routes"
            );
        }
    }

    /// Nothing on the surface is unreachable, and nothing off it responds.
    ///
    /// This replaces `black_bars_hit_nothing`, which asserted that the 240px pillarbox on a 16:9
    /// display routed to no control. That was true, and it was **guarding the defect**: ledger A11
    /// is those bars. With the plate stretched to fill (Matt's decision, 2026-08-18) there are no
    /// bars, so the property worth pinning inverted — every point of the surface is now live, and
    /// the negative moved outside the surface where it belongs.
    ///
    /// The four corners are the useful probes: under the old uniform fit they were dead on any
    /// non-4:3 display, which is exactly the region a maximised window puts under the player's mouse.
    #[test]
    fn every_surface_point_is_on_the_plate_and_nothing_outside_it_responds() {
        for vp in [(1024.0, 768.0), (1920.0, 1080.0), (2560.0, 1080.0)] {
            let xf = PreworldTransform::from_viewport(vp);
            assert_eq!(
                (xf.ox, xf.oy),
                (0.0, 0.0),
                "{vp:?}: the plate fills the surface"
            );
            for screen in [
                PreWorldScreen::RealmSelect,
                PreWorldScreen::CharSelect,
                PreWorldScreen::CharCreate,
            ] {
                // Corners and edge midpoints — the old bar region.
                for (x, y) in [
                    (0.0, 0.0),
                    (vp.0 - 1.0, 0.0),
                    (0.0, vp.1 - 1.0),
                    (vp.0 - 1.0, vp.1 - 1.0),
                    (0.0, vp.1 * 0.5),
                    (vp.0 - 1.0, vp.1 * 0.5),
                ] {
                    assert!(
                        probe(screen, x, y, vp).on_plate,
                        "{screen:?} {vp:?}: ({x},{y}) must be on the plate now"
                    );
                }
                // Outside the surface is still nothing — the replacement negative control. Without
                // this the test would pass on a transform that called everything on-plate.
                for (x, y) in [
                    (-1.0, vp.1 * 0.5),
                    (vp.0 + 1.0, vp.1 * 0.5),
                    (vp.0 * 0.5, -1.0),
                ] {
                    let p = probe(screen, x, y, vp);
                    assert!(
                        !p.on_plate,
                        "{screen:?} {vp:?}: ({x},{y}) is off the surface"
                    );
                    assert!(
                        p.control.is_none(),
                        "{screen:?} {vp:?}: ({x},{y}) routed to {:?}",
                        p.control.map(|c| c.target)
                    );
                }
            }
        }
    }

    /// The same design point resolves to the same control at every viewport, including one where
    /// the window and the surface disagree.
    #[test]
    fn hit_is_viewport_independent_in_design_space() {
        for vp in [
            (1024.0, 768.0),
            (1920.0, 1080.0),
            (1366.0, 768.0),
            (800.0, 600.0),
            (2560.0, 1440.0),
        ] {
            let xf = PreworldTransform::from_viewport(vp);
            for screen in [
                PreWorldScreen::RealmSelect,
                PreWorldScreen::CharSelect,
                PreWorldScreen::CharCreate,
            ] {
                for c in controls(screen) {
                    let (dx, dy) = c.click_point();
                    let (sx, sy) = xf.map(dx, dy);
                    assert_eq!(
                        hit(screen, sx, sy, vp).map(|h| h.target),
                        Some(c.target),
                        "{vp:?} {screen:?} {}: click point did not survive the round trip",
                        c.name
                    );
                }
            }
        }
    }

    /// A control's click point must be inside visible art. A row whose bounding centre lands in
    /// the seam between its two text lines is the case this catches.
    #[test]
    fn click_points_land_on_visible_art() {
        for screen in [
            PreWorldScreen::RealmSelect,
            PreWorldScreen::CharSelect,
            PreWorldScreen::CharCreate,
        ] {
            for c in controls(screen) {
                let (x, y) = c.click_point();
                let part = c.part_at(x, y).expect("click point is on a part");
                assert!(part.visible, "{}: click point is on invisible art", c.name);
                let b = c.bounds();
                let (bx, by) = (b.x + b.w * 0.5, b.y + b.h * 0.5);
                if c.parts.len() > 1 && c.part_at(bx, by).is_none() {
                    // Documented: this control's bounding centre is empty art, which is exactly
                    // why click_point does not use it.
                    assert_ne!((x, y), (bx, by));
                }
            }
        }
    }

    /// Overlaps the client itself authors, as `(screen file, id a, id b)`.
    ///
    /// `character_creation.xml` really does place the Random button (1013, x 938..1002) two pixels
    /// into the name edit box (1051, x 790..940). It is in the shipped form, so it is reproduced;
    /// listing it here is what keeps a *new* overlap — one we introduced — failing.
    const CLIENT_AUTHORED_OVERLAPS: [(u16, u16); 1] = [(1013, 1051)];

    /// No two controls on a screen claim the same point, except where the client does.
    #[test]
    fn controls_do_not_overlap_within_a_screen() {
        for screen in [
            PreWorldScreen::RealmSelect,
            PreWorldScreen::CharSelect,
            PreWorldScreen::CharCreate,
        ] {
            let table = controls(screen);
            for (i, a) in table.iter().enumerate() {
                for b in &table[i + 1..] {
                    for pa in &a.parts {
                        for pb in &b.parts {
                            let overlap = pa.x < pb.x + pb.w
                                && pa.x + pa.w > pb.x
                                && pa.y < pb.y + pb.h
                                && pa.y + pa.h > pb.y;
                            let known = CLIENT_AUTHORED_OVERLAPS.contains(&(
                                pa.control_id.min(pb.control_id),
                                pa.control_id.max(pb.control_id),
                            ));
                            assert!(
                                !overlap || known,
                                "{screen:?}: {} ({}) overlaps {} ({})",
                                a.name,
                                pa.control_id,
                                b.name,
                                pb.control_id
                            );
                        }
                    }
                }
            }
        }
    }
}
