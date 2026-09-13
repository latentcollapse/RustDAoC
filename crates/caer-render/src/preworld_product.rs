//! GPU-free pre-world product dispatch — the LiveCommand / transition effects rustdaoc sends
//! on realm pick, Continué, slot select, and Quit.
//!
//! [`crate::preworld::PreWorldHud::apply_action`] is navigation-only (screen transitions). This
//! module is the discriminating product denominator: deleting a command here must turn a named
//! test red.

use caer_protocol::charcreate::CharacterCreateDraft;
use caer_protocol::overview::{CharacterOverview, CharacterSummary};
use caer_protocol::transition::WorldTransition;
use std::sync::Arc;

use crate::live::LiveCommand;
use crate::preworld::{race_id_for_realm, PreWorldAction};
use crate::preworld_appearance::{AppearanceCatalog, AppearanceChoice, AppearanceSelector};
use crate::preworld_customize::CustomizerField;

/// Local state owned by the runtime customizer rather than the create packet.
///
/// Locks constrain Random.  The currently selected Tattoo has a dedicated local slot until its
/// actual wire byte is proven by a controlled reference capture; it must never be silently
/// aliased to DOLSharp's independent MoodType byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomizerState {
    /// Bit ownership is [`CustomizerField::lock_slot`], not a renderer-specific row ordinal.
    locks: u16,
    /// Deterministic local entropy for the Random button. It is deliberately not a protocol field.
    random_state: u64,
    /// Source decal index currently selected in the live form.
    ///
    /// The DOLSharp create handler reads MoodType as a distinct byte.  Until capture identifies
    /// which of the skipped customization bytes owns decals, this remains local instead of
    /// corrupting MoodType just to make a label move.
    tattoo_index: u8,
}

impl Default for CustomizerState {
    fn default() -> Self {
        Self {
            locks: 0,
            // A non-zero xorshift seed. Reproducible in a test, varied on every Random press.
            random_state: 0xCAEC_AFE0_DA0C_1126,
            tattoo_index: 0,
        }
    }
}

impl CustomizerState {
    /// Whether this form-local Random lock is set.
    #[must_use]
    pub const fn is_locked(self, field: CustomizerField) -> bool {
        let slot = field.lock_slot();
        field.is_runtime_field() && slot < u16::BITS as u8 && (self.locks & (1u16 << slot)) != 0
    }

    fn toggle_lock(&mut self, field: CustomizerField) -> bool {
        let slot = field.lock_slot();
        if !field.is_runtime_field() || slot >= u16::BITS as u8 {
            return false;
        }
        self.locks ^= 1u16 << slot;
        true
    }

    /// The visible tattoo selection, which remains local pending packet ownership evidence.
    #[must_use]
    pub const fn tattoo_index(self) -> u8 {
        self.tattoo_index
    }

    fn set_tattoo_index(&mut self, index: u8) {
        self.tattoo_index = index;
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64*: a tiny deterministic UI-only generator. Avoids a new runtime dependency
        // and has a non-zero seed by construction.
        let mut x = self.random_state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.random_state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Mutable product state the pre-world path owns (create draft + transition + overview).
#[derive(Debug, Clone)]
pub struct PreWorldProductState {
    pub transition: WorldTransition,
    pub create_draft: CharacterCreateDraft,
    /// Realm last chosen on the realm plate (feeds create draft on Continué / OpenCharCreate).
    pub create_realm: u8,
    pub overview: Option<CharacterOverview>,
    pub pending_create_name: Option<String>,
    pub name_edit_focus: bool,
    /// Selected character's **protocol slot** — stable across overview refreshes, unlike a row
    /// index into the compact list (H4).
    pub selected_protocol_slot: Option<u8>,
    pub ui_select_sent: bool,
    /// Set when the customizer's Continue has encoded and sent a create packet this session. A
    /// second Continue refuses — back-navigation through the creation chain may never double-create
    /// (acceptance criterion 3). Cleared only by starting a fresh draft (`OpenCharCreate`).
    pub creation_sent: bool,
    /// Client name fragments for the Random button. Empty by default: Random then does nothing
    /// rather than inventing a name the client would never produce.
    pub names: caer_assets::names::NameTable,
    /// Retail fig3 choice catalogue loaded once by the shell.  `None` is the explicit
    /// dependency-free test fallback; a live client always supplies it and therefore never
    /// navigates a guessed `1..=N` appearance range.
    pub appearance_catalog: Option<Arc<AppearanceCatalog>>,
    /// UI-only Random locks / entropy from `character_customize.xml`. Size lives in
    /// `create_draft.creation_model`, its real 1126+ wire owner.
    pub customizer: CustomizerState,
}

impl Default for PreWorldProductState {
    fn default() -> Self {
        // Empty editable name — Continué must refuse until the player types one. Never invent
        // "Newchar" or any other fallback.
        let mut create_draft = CharacterCreateDraft::albion_briton_stub("", 0);
        create_draft.name.clear();
        Self {
            transition: WorldTransition::new(),
            create_draft,
            create_realm: 1,
            overview: None,
            pending_create_name: None,
            name_edit_focus: false,
            selected_protocol_slot: None,
            ui_select_sent: false,
            creation_sent: false,
            names: caer_assets::names::NameTable::default(),
            appearance_catalog: None,
            customizer: CustomizerState::default(),
        }
    }
}

/// Result of one product dispatch step.
#[derive(Debug, Clone)]
pub struct PreWorldProductResult {
    /// Typed commands the live session must receive (empty ⇒ local-only).
    pub commands: Vec<LiveCommand>,
    /// Human-readable refusal (no commands emitted).
    pub refused: Option<String>,
    /// Whether the caller should also advance [`crate::preworld::PreWorldHud`] via `apply_action`.
    pub apply_hud_navigation: bool,
}

impl PreWorldProductResult {
    fn local(apply_hud: bool) -> Self {
        Self {
            commands: Vec::new(),
            refused: None,
            apply_hud_navigation: apply_hud,
        }
    }

    fn cmds(commands: Vec<LiveCommand>, apply_hud: bool) -> Self {
        Self {
            commands,
            refused: None,
            apply_hud_navigation: apply_hud,
        }
    }

    fn refuse(msg: impl Into<String>) -> Self {
        Self {
            commands: Vec::new(),
            refused: Some(msg.into()),
            apply_hud_navigation: false,
        }
    }
}

/// Max length of the create-form name edit (`character_creation.xml`, `name_edit`
/// `<MaxCharacters>20</MaxCharacters>`).
pub const CREATE_NAME_MAX_LEN: usize = 20;

/// Shortest name the client accepts.
///
/// PROVENANCE GAP: the client carries the *message* ("Your name is too short!", and the rename
/// window's "That name is too short.") but the threshold itself lives in code, not in an asset.
/// Three is DAoC's documented minimum and the server rejects shorter; it is not read from the
/// client tree, and it is recorded here rather than buried at a call site.
pub const CREATE_NAME_MIN_LEN: usize = 3;

/// Is `ch` a character a DAoC name may contain?
///
/// Letters only. The client says so in its own words — `game.dll` carries
/// **"Names can only contain characters A-z."** alongside "Your name contains invalid
/// characters!". CAER accepted `is_ascii_alphanumeric`, so `Ca33435` was a legal character name.
#[must_use]
pub fn create_name_char_allowed(ch: char) -> bool {
    ch.is_ascii_alphabetic()
}

/// Shared create-name text mutation: letters only, capped at [`CREATE_NAME_MAX_LEN`].
///
/// Both the live `rustdaoc` keyboard branch and the GPU-free product path must call this exact
/// function — deleting the live call must make the source-structure guard red.
///
/// PROVENANCE GAP: whether retail swallows the invalid keystroke or accepts it and refuses at
/// Continué is not established — the refusal string's existence suggests the latter. The outcome
/// is identical either way (no name with a digit in it can ever be submitted); only the feel while
/// typing may differ. [`validate_create_name`] enforces the same rule at submit time regardless.
pub fn append_create_name_chars(name: &mut String, text: &str) {
    for ch in text.chars() {
        if create_name_char_allowed(ch) && name.len() < CREATE_NAME_MAX_LEN {
            name.push(ch);
        }
    }
}

/// Check a finished name the way the client does, in the client's own words.
///
/// Every message here is a verbatim `game.dll` string. Returns `None` when the name is acceptable.
#[must_use]
pub fn validate_create_name(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("You must enter a name before continuing.");
    }
    if !name.chars().all(create_name_char_allowed) {
        return Some("Names can only contain characters A-z.");
    }
    if name.len() < CREATE_NAME_MIN_LEN {
        return Some("Your name is too short!");
    }
    if name.len() > CREATE_NAME_MAX_LEN {
        return Some("That name is too long.");
    }
    None
}

/// GPU-free name entry matching rustdaoc create-form keyboard rules.
pub fn apply_create_name_input(state: &mut PreWorldProductState, text: &str) {
    state.name_edit_focus = true;
    append_create_name_chars(&mut state.create_draft.name, text);
}

/// Apply one of the four real source facial sliders, preserving the other three packed nibbles.
///
/// The UI receives a zero-based `morph_N` slot from `character_customize.xml`; packet ownership
/// lives in `FacialMorphSlot` so a visual row cannot be accidentally wired to its neighbour.
fn set_facial_morph(
    draft: &mut CharacterCreateDraft,
    source_slot: u8,
    tick: u8,
) -> PreWorldProductResult {
    let Some(slot) = caer_protocol::customization::FacialMorphSlot::from_source_slot(source_slot)
    else {
        return PreWorldProductResult::refuse(format!(
            "facial morph slot {source_slot} is outside character_customize.xml's four controls"
        ));
    };
    if tick > caer_protocol::customization::FACIAL_MORPH_MAX_TICK {
        return PreWorldProductResult::refuse(format!(
            "facial morph tick {tick} is outside the source nine-position slider"
        ));
    }
    draft.set_facial_morph_tick(slot, tick);
    PreWorldProductResult::local(false)
}

/// Return the adjacent author-provided value without compacting source holes.
///
/// Zero is the untouched retail default. Left from the first real value restores zero; right
/// from zero picks the first real value. This is the scalar equivalent of the catalogue cycle
/// for eye, hair-colour, and skin-tone tables.
fn cycle_source_values(values: &[u8], current: u8, direction: i8) -> Option<u8> {
    if values.is_empty() || direction == 0 {
        return None;
    }
    if direction < 0 {
        values
            .iter()
            .rev()
            .find(|value| **value < current)
            .copied()
            .or(Some(0))
    } else {
        values
            .iter()
            .find(|value| **value > current)
            .copied()
            .or_else(|| values.last().copied())
    }
}

/// Apply one visible text-selector click from the retail runtime profile.
///
/// This deliberately names UI semantics instead of static XML control ids. The old form mapped
/// Tattoo onto MoodType; that made two independently visible controls overwrite each other.
/// Mood is now edited only by its slider and Tattoo stays local until a reference packet proves
/// its real owner.
fn adjust_customizer(
    state: &mut PreWorldProductState,
    field: CustomizerField,
    direction: i8,
) -> PreWorldProductResult {
    if direction == 0 {
        return PreWorldProductResult::refuse("a zero selector direction changes nothing");
    }
    let catalog = state.appearance_catalog.clone();
    match field {
        CustomizerField::Face | CustomizerField::HairStyle => {
            let selector = if field == CustomizerField::Face {
                AppearanceSelector::Face
            } else {
                AppearanceSelector::HairStyle
            };
            let current = if field == CustomizerField::Face {
                state.create_draft.face_type
            } else {
                state.create_draft.hair_style
            };
            let Some(catalog) = catalog else {
                return PreWorldProductResult::refuse(
                    "retail appearance catalogue unavailable; selector cannot invent a range",
                );
            };
            let Some(next) = catalog.cycle(
                state.create_draft.race,
                state.create_draft.gender,
                selector,
                current,
                direction,
            ) else {
                return PreWorldProductResult::refuse(format!(
                    "retail appearance catalogue has no {:?} choices for race {} gender {}",
                    selector, state.create_draft.race, state.create_draft.gender
                ));
            };
            if field == CustomizerField::Face {
                state.create_draft.face_type = next;
            } else {
                state.create_draft.hair_style = next;
            }
            state.create_draft.enable_customization();
            // Hair colour, eye colour, and skin tone tables are keyed by the selected source
            // hair style.  A valid style change can therefore make the old palette value
            // impossible; normalise immediately rather than carrying an invisible stale byte
            // until race/gender changes happen to touch it later.
            if field == CustomizerField::HairStyle {
                normalize_source_appearance(state);
            }
            PreWorldProductResult::local(false)
        }
        CustomizerField::EyeColor | CustomizerField::HairColor => {
            let Some(choices) = catalog.as_ref().and_then(|catalog| {
                catalog.choices(state.create_draft.race, state.create_draft.gender)
            }) else {
                return PreWorldProductResult::refuse(
                    "retail appearance catalogue unavailable; colour selector cannot invent a range",
                );
            };
            let (values, current) = match field {
                CustomizerField::EyeColor => {
                    (choices.eye_colours(), state.create_draft.eye_color >> 4)
                }
                CustomizerField::HairColor => (
                    choices
                        .palette_values(2, state.create_draft.hair_style)
                        .unwrap_or_default(),
                    state.create_draft.hair_color,
                ),
                _ => unreachable!("field was checked above"),
            };
            let Some(next) = cycle_source_values(values, current, direction) else {
                return PreWorldProductResult::refuse(format!(
                    "retail appearance catalogue has no {field:?} choices for race {} gender {}",
                    state.create_draft.race, state.create_draft.gender
                ));
            };
            if field == CustomizerField::EyeColor {
                state.create_draft.eye_color = (state.create_draft.eye_color & 0x0F) | (next << 4);
            } else {
                state.create_draft.hair_color = next;
            }
            state.create_draft.enable_customization();
            PreWorldProductResult::local(false)
        }
        CustomizerField::Tattoo => {
            let Some(catalog) = catalog else {
                return PreWorldProductResult::refuse(
                    "retail appearance catalogue unavailable; Tattoo cannot invent a range",
                );
            };
            let current = state.customizer.tattoo_index();
            let Some(next) = catalog.cycle(
                state.create_draft.race,
                state.create_draft.gender,
                AppearanceSelector::Tattoo,
                current,
                direction,
            ) else {
                return PreWorldProductResult::refuse(format!(
                    "retail appearance catalogue has no Tattoo choices for race {} gender {}",
                    state.create_draft.race, state.create_draft.gender
                ));
            };
            state.customizer.set_tattoo_index(next);
            PreWorldProductResult::local(false)
        }
        CustomizerField::Size => cycle_scale(state, direction),
        CustomizerField::Morph(_) | CustomizerField::Mood | CustomizerField::SkinTone => {
            PreWorldProductResult::refuse(format!(
                "{field:?} is a slider, not a textual runtime selector"
            ))
        }
    }
}

/// Apply one live runtime-slider click.
fn set_customizer_slider(
    state: &mut PreWorldProductState,
    field: CustomizerField,
    tick: u8,
) -> PreWorldProductResult {
    if tick > crate::preworld_customize::SLIDER_MAX_TICK {
        return PreWorldProductResult::refuse(format!(
            "customizer slider tick {tick} is outside the observed nine-position range"
        ));
    }
    match field {
        CustomizerField::Morph(source_slot) => {
            set_facial_morph(&mut state.create_draft, source_slot, tick)
        }
        CustomizerField::Mood => {
            // DOLSharp reads this byte after the three unknown create fields. It is independent
            // from the visible Tattoo selector and is therefore owned by Mood alone.
            state.create_draft.mood_type = tick;
            state.create_draft.enable_customization();
            PreWorldProductResult::local(false)
        }
        CustomizerField::SkinTone => {
            let Some(choices) = state.appearance_catalog.as_ref().and_then(|catalog| {
                catalog.choices(state.create_draft.race, state.create_draft.gender)
            }) else {
                return PreWorldProductResult::refuse(
                    "retail appearance catalogue unavailable; Skin Tone cannot invent a range",
                );
            };
            // Tick zero is the untouched wire/default value. The eight source palette entries
            // occupy ticks one through eight, which is why the observed nine-position slider
            // must not index directly into the palette vector.
            let value = if tick == 0 {
                0
            } else {
                let Some(value) = choices
                    .skin_tones()
                    .get(usize::from(tick.saturating_sub(1)))
                    .copied()
                else {
                    return PreWorldProductResult::refuse(format!(
                        "Skin Tone slider tick {tick} has no authored choice for race {} gender {}",
                        state.create_draft.race, state.create_draft.gender
                    ));
                };
                value
            };
            state.create_draft.eye_color = (state.create_draft.eye_color & 0xF0) | value;
            state.create_draft.enable_customization();
            PreWorldProductResult::local(false)
        }
        CustomizerField::Face
        | CustomizerField::EyeColor
        | CustomizerField::HairStyle
        | CustomizerField::HairColor
        | CustomizerField::Tattoo
        | CustomizerField::Size => PreWorldProductResult::refuse(format!(
            "{field:?} is a textual selector, not a runtime slider"
        )),
    }
}

/// Set the exact all-zero look retail uses for its Default button.
fn reset_appearance(state: &mut PreWorldProductState) -> PreWorldProductResult {
    let draft = &mut state.create_draft;
    draft.custom_mode = 0;
    draft.eye_size = 0;
    draft.lip_size = 0;
    draft.eye_color = 0;
    draft.hair_color = 0;
    draft.face_type = 0;
    draft.hair_style = 0;
    draft.mood_type = 0;
    draft.reset_creation_size();
    state.customizer.set_tattoo_index(0);
    PreWorldProductResult::local(false)
}

/// Remove values that the newly selected source identity cannot represent.
///
/// Race and gender buttons do not open a second draft; they change the identity of the one that
/// will be encoded. Without this normalization a colour/style index that is legal only for the
/// previous body could remain in invisible packet fields after selecting a Briton. The source
/// catalogue is therefore the one authority for both what the HUD draws and what the retained
/// draft is allowed to carry.
fn normalize_source_appearance(state: &mut PreWorldProductState) {
    let Some(catalog) = &state.appearance_catalog else {
        return;
    };
    let Some(choices) = catalog.choices(state.create_draft.race, state.create_draft.gender) else {
        return;
    };

    let mut appearance = state.create_draft.customization();
    let original = appearance;
    let selector_is_valid = |selector, value| {
        value == 0
            || choices
                .selector_values(selector)
                .iter()
                .any(|choice| choice.index == value)
    };
    if !selector_is_valid(AppearanceSelector::Face, appearance.face_type) {
        appearance.face_type = 0;
    }
    if !selector_is_valid(AppearanceSelector::HairStyle, appearance.hair_style) {
        appearance.hair_style = 0;
    }
    if !selector_is_valid(AppearanceSelector::Tattoo, state.customizer.tattoo_index()) {
        // Tattoos are source catalogue selections, but their packet byte has not yet been
        // measured. Keep the local selection legal when identity changes without touching the
        // independent MoodType packet field.
        state.customizer.set_tattoo_index(0);
    }
    // MoodType is a real DOLSharp packet byte and has its own observed nine-position slider.
    // It is not validated against fig3 decal rows; doing so was the old Tattoo/Mood alias bug.
    appearance.mood_type = appearance
        .mood_type
        .min(crate::preworld_customize::SLIDER_MAX_TICK);

    let skin = appearance.eye_color & 0x0F;
    if skin != 0 && !choices.allows_palette(0, skin, appearance.hair_style) {
        appearance.eye_color &= 0xF0;
    }
    let eye = appearance.eye_color >> 4;
    if eye != 0 && !choices.allows_palette(1, eye, appearance.hair_style) {
        appearance.eye_color &= 0x0F;
    }
    if appearance.hair_color != 0
        && !choices.allows_palette(2, appearance.hair_color, appearance.hair_style)
    {
        appearance.hair_color = 0;
    }
    if appearance != original {
        state.create_draft.set_customization(appearance);
    }

    if !choices.scale().is_empty()
        && !choices
            .scale()
            .iter()
            .any(|choice| choice.index == state.create_draft.creation_size())
    {
        if let Some(choice) = choices
            .scale()
            .iter()
            .find(|choice| choice.index == caer_protocol::charcreate::CREATION_SIZE_AVERAGE)
            .or_else(|| choices.scale().first())
        {
            // `AppearanceCatalog` retains the source index verbatim.  The packet only has the
            // three DOLSharp model-bit encodings, so a malformed future table must collapse to
            // the neutral retail default instead of leaking an unencodable index into UI state.
            if !state.create_draft.set_creation_size(choice.index) {
                state.create_draft.reset_creation_size();
            }
        }
    }
}

/// Cycle retail's source-backed `height_text` adapter through the model bits that DOLSharp owns.
fn cycle_scale(state: &mut PreWorldProductState, dir: i8) -> PreWorldProductResult {
    let Some(catalog) = &state.appearance_catalog else {
        return PreWorldProductResult::refuse(
            "retail appearance catalogue unavailable; Size cannot use an invented range",
        );
    };
    let Some(next) = catalog.cycle_scale(
        state.create_draft.race,
        state.create_draft.gender,
        state.create_draft.creation_size(),
        dir,
    ) else {
        return PreWorldProductResult::refuse(format!(
            "retail appearance catalogue has no Size choices for race {} gender {}",
            state.create_draft.race, state.create_draft.gender
        ));
    };
    if !state.create_draft.set_creation_size(next) {
        return PreWorldProductResult::refuse(format!(
            "retail Size index {next} cannot be represented by the 1126+ creation model"
        ));
    }
    PreWorldProductResult::local(false)
}

/// Toggle one authored Random lock by the field the player can actually see.
fn toggle_customizer_lock(
    state: &mut PreWorldProductState,
    field: CustomizerField,
) -> PreWorldProductResult {
    if state.customizer.toggle_lock(field) {
        PreWorldProductResult::local(false)
    } else {
        PreWorldProductResult::refuse(format!(
            "customizer lock {field:?} is outside the runtime profile"
        ))
    }
}

fn random_index(customizer: &mut CustomizerState, len: usize) -> Option<usize> {
    if len == 0 {
        None
    } else {
        Some((customizer.next_random() % len as u64) as usize)
    }
}

fn random_choice(customizer: &mut CustomizerState, values: &[AppearanceChoice]) -> Option<u8> {
    values
        .get(random_index(customizer, values.len())?)
        .map(|choice| choice.index)
}

fn random_value(customizer: &mut CustomizerState, values: &[u8]) -> Option<u8> {
    values.get(random_index(customizer, values.len())?).copied()
}

/// Randomize precisely the fields the retail form exposes, respecting its semantic locks.
///
/// Unlike the old disabled button, this never guesses a count: all choices come from the same
/// `fig3` maps that populate visible labels and the avatar binder. Size is randomized through
/// the creation-model bits that DOLSharp receives on the wire.
fn randomize_appearance(state: &mut PreWorldProductState) -> PreWorldProductResult {
    let Some(catalog) = state.appearance_catalog.clone() else {
        return PreWorldProductResult::refuse(
            "retail appearance catalogue unavailable; Random cannot invent appearance choices",
        );
    };
    let Some(choices) = catalog
        .choices(state.create_draft.race, state.create_draft.gender)
        .cloned()
    else {
        return PreWorldProductResult::refuse(format!(
            "retail appearance catalogue has no choices for race {} gender {}",
            state.create_draft.race, state.create_draft.gender
        ));
    };

    let customizer = &mut state.customizer;
    let mut appearance = state.create_draft.customization();
    if !customizer.is_locked(CustomizerField::Face) {
        if let Some(value) = random_choice(
            customizer,
            choices.selector_values(AppearanceSelector::Face),
        ) {
            appearance.face_type = value;
        }
    }
    // Hair colour maps are keyed by the *selected* hairstyle.  Choose the style before its
    // colour so Random can never serialize a colour from the former head onto the newly selected
    // mesh.  `palette_values` deliberately has a first-style fallback for drawing the untouched
    // retail form; that fallback is not valid for a randomized, explicit hairstyle.
    if !customizer.is_locked(CustomizerField::HairStyle) {
        if let Some(value) = random_choice(
            customizer,
            choices.selector_values(AppearanceSelector::HairStyle),
        ) {
            appearance.hair_style = value;
        }
    }
    if !customizer.is_locked(CustomizerField::SkinTone) {
        if let Some(value) = random_value(
            customizer,
            choices
                .palette_values(0, appearance.hair_style)
                .unwrap_or_default(),
        ) {
            appearance.eye_color = (appearance.eye_color & 0xF0) | value;
        }
    }
    if !customizer.is_locked(CustomizerField::EyeColor) {
        if let Some(value) = random_value(
            customizer,
            choices
                .palette_values(1, appearance.hair_style)
                .unwrap_or_default(),
        ) {
            appearance.eye_color = (appearance.eye_color & 0x0F) | (value << 4);
        }
    }
    if !customizer.is_locked(CustomizerField::HairColor) {
        if let Some(value) = random_value(customizer, choices.hair_colours(appearance.hair_style)) {
            appearance.hair_color = value;
        } else {
            // A source hairstyle with no colour map is not permission to borrow a colour from
            // another hairstyle. Zero is the uncoloured/default wire value (for example a bald
            // or map-less source row) and renders honestly rather than making a mismatched mesh.
            appearance.hair_color = 0;
        }
    }
    if !customizer.is_locked(CustomizerField::Tattoo) {
        if let Some(value) = random_choice(
            customizer,
            choices.selector_values(AppearanceSelector::Tattoo),
        ) {
            customizer.set_tattoo_index(value);
        }
    }
    state.create_draft.set_customization(appearance);

    for slot in 0..4u8 {
        if customizer.is_locked(CustomizerField::Morph(slot)) {
            continue;
        }
        let tick = (customizer.next_random() % 9) as u8;
        let Some(morph) = caer_protocol::customization::FacialMorphSlot::from_source_slot(slot)
        else {
            continue;
        };
        state.create_draft.set_facial_morph_tick(morph, tick);
    }
    if !customizer.is_locked(CustomizerField::Mood) {
        state.create_draft.mood_type = (customizer.next_random()
            % (u64::from(crate::preworld_customize::SLIDER_MAX_TICK) + 1))
            as u8;
        state.create_draft.enable_customization();
    }
    if !customizer.is_locked(CustomizerField::Size) {
        if let Some(value) = random_choice(customizer, choices.scale()) {
            // The trusted source tables have only Short/Average/Tall, but retain the previous
            // model rather than manufacturing a fourth size if a future client changes them.
            let _ = state.create_draft.set_creation_size(value);
        }
    }
    PreWorldProductResult::local(false)
}

/// One stats-screen arrow click, under the DOLSharp allocation rules
/// (`IsCustomPointsDistributionValid` at level 1): no stat below its race base, and the
/// escalating point costs (1/2/3) charged against a pool of exactly 30.
fn adjust_starting_stat(
    state: &mut PreWorldProductState,
    stat: u8,
    dir: i8,
) -> PreWorldProductResult {
    if stat > 7 {
        return PreWorldProductResult::refuse(format!(
            "stat index {stat} is outside the wire's eight"
        ));
    }
    let base = caer_protocol::starting_stats::race_base(state.create_draft.race, stat as usize);
    let current = state.create_draft.stats[stat as usize];
    if dir < 0 {
        if current <= base {
            return PreWorldProductResult::refuse("Your base statistics cannot be lowered.");
        }
        state.create_draft.stats[stat as usize] = current - 1;
        return PreWorldProductResult::local(false);
    }
    if dir > 0 {
        let above = u32::from(current - base);
        let cost = caer_protocol::starting_stats::marginal_cost(above);
        let spent = caer_protocol::starting_stats::total_spent(
            state.create_draft.race,
            &state.create_draft.stats,
        );
        if spent + cost > caer_protocol::starting_stats::MAX_STARTING_BONUS_POINTS {
            return PreWorldProductResult::refuse("You have no bonus points left to spend.");
        }
        state.create_draft.stats[stat as usize] = current.saturating_add(1);
        return PreWorldProductResult::local(false);
    }
    PreWorldProductResult::refuse("a zero arrow click adjusts nothing")
}

fn create_name_is_player_supplied(name: &str) -> bool {
    let t = name.trim();
    !t.is_empty() && !t.eq_ignore_ascii_case("Newchar")
}

/// Whether `(realm, class, race, gender)` is a legal creation tuple.
///
/// B3: `caer_protocol::create_validity::classify` is the **only** legality authority in the product
/// path. `Unestablished` is deliberately not allowed here — a missing EligibleRaces row is a
/// recorded oracle gap, and treating it as permission is how an illegal tuple reaches the wire.
fn combination_allowed(state: &PreWorldProductState, class_id: u8, race: u8) -> bool {
    matches!(
        caer_protocol::create_validity::classify(
            state.create_draft.realm.max(1),
            class_id,
            race,
            state.create_draft.gender,
        ),
        caer_protocol::create_validity::Legality::Allowed
    )
}

/// First class on this realm's creation form that is legal for `race`, in adapter slot order.
fn first_legal_class(state: &PreWorldProductState, race: u8) -> Option<u8> {
    let realm = state.create_draft.realm.max(1);
    caer_protocol::creation_adapters::class_adapters(realm, race)
        .into_iter()
        .map(|a| a.class_id)
        .find(|&class_id| combination_allowed(state, class_id, race))
}

/// Dispatch a pre-world UI action into product effects (no GPU, no event loop).
pub fn dispatch_preworld_action(
    state: &mut PreWorldProductState,
    action: PreWorldAction,
) -> PreWorldProductResult {
    match action {
        PreWorldAction::LoginPlay => {
            // Network auth precedes the window; Play is local navigation only.
            PreWorldProductResult::local(true)
        }
        PreWorldAction::LoginExit => PreWorldProductResult::cmds(vec![LiveCommand::Quit], false),
        PreWorldAction::ChooseRealm(realm) => {
            state.selected_protocol_slot = None;
            state.ui_select_sent = false;
            // The overview packet is realm-local: ten slots for the realm you asked about. Keeping
            // the previous realm's list while the new one is in flight is what put two Albion
            // Armsmen on the Hibernia character screen.
            state.overview = None;
            let fx = state.transition.choose_realm(realm);
            state.create_realm = state.transition.realm();
            state.create_draft.realm = state.transition.realm();
            debug_assert_eq!(state.create_draft.realm, state.transition.realm());
            let mut commands = Vec::new();
            if fx.request_overview {
                commands.push(LiveCommand::RequestCharacterOverview {
                    realm: state.transition.realm(),
                });
            }
            PreWorldProductResult::cmds(commands, true)
        }
        PreWorldAction::CharCreateContinue => {
            // E6: Continué OPENS the customize screen. It sends nothing — dispatching
            // `CreateCharacter` here is what minted a character on sight (the stray
            // Elveach/Connuonnu/Shenisnar rows). Full name validation stays with the customizer's
            // Continue,
            // the one action that encodes — but the blank-name gate lives HERE, because the
            // name box is on this form and letting an unnamed draft walk downstream would
            // surface the failure three screens later where the player can't connect it.
            if !create_name_is_player_supplied(&state.create_draft.name) {
                return PreWorldProductResult::refuse(
                    "CharacterCreate refused — blank or unset name (no invented fallback)",
                );
            }
            PreWorldProductResult::local(true)
        }
        PreWorldAction::CustomizeStats
        | PreWorldAction::StatsDismiss
        | PreWorldAction::CustomizeBack
        | PreWorldAction::CustomizeCancel => {
            // Open the local stats modal, or navigate back down the chain. The flow owns which
            // screen is next.
            PreWorldProductResult::local(true)
        }
        PreWorldAction::CustomizeAdjust { field, dir } => adjust_customizer(state, field, dir),
        PreWorldAction::CustomizeSlider { field, tick } => {
            set_customizer_slider(state, field, tick)
        }
        // The source camera controls are handled by rustdaoc's local preview controller.  They
        // intentionally produce no packet and no navigation, but they must be accepted here so
        // the same product dispatch path can prove that fact.
        PreWorldAction::CustomizeCamera(_) => PreWorldProductResult::local(false),
        PreWorldAction::CustomizeReset => reset_appearance(state),
        PreWorldAction::CustomizeRandom => randomize_appearance(state),
        PreWorldAction::CustomizeToggleLock { field } => toggle_customizer_lock(state, field),
        PreWorldAction::StatsAdjust { stat, dir } => adjust_starting_stat(state, stat, dir),
        PreWorldAction::StatsReset => {
            for (i, s) in state.create_draft.stats.iter_mut().enumerate() {
                *s = caer_protocol::starting_stats::race_base(state.create_draft.race, i);
            }
            PreWorldProductResult::local(false)
        }
        PreWorldAction::StatsOptimize => {
            // Retail's control 1021 is a distinct Optimize event, not an Accept/Continue event.
            // The observed source default allocates the 30 points to STR/CON/DEX; the draft owns
            // that exact canonical reset-and-allocate operation.
            state.create_draft.allocate_default_bonus();
            PreWorldProductResult::local(false)
        }
        PreWorldAction::CustomizeAdvance => {
            let Some(ov) = state.overview.as_ref() else {
                return PreWorldProductResult::refuse(
                    "CharacterCreate refused — overview not present",
                );
            };
            // The row the player picked on the select screen, when it is still empty — retail
            // creates into the slot you clicked, not into the first hole in the list. Falls back
            // to the first free slot when Create was pressed without a row selected.
            let picked = state
                .selected_protocol_slot
                .filter(|s| !ov.characters.iter().any(|c| c.slot == *s));
            let Some(slot) = picked.or_else(|| {
                CharacterCreateDraft::first_free_slot(ov.characters.iter().map(|c| c.slot))
            }) else {
                return PreWorldProductResult::refuse(
                    "CharacterCreate refused — no free overview slot",
                );
            };
            if !create_name_is_player_supplied(&state.create_draft.name) {
                return PreWorldProductResult::refuse(
                    "CharacterCreate refused — blank or unset name (no invented fallback)",
                );
            }
            if let Some(msg) = validate_create_name(state.create_draft.name.trim()) {
                return PreWorldProductResult::refuse(msg);
            }
            // Exactly-once: a second Continue must refuse, not re-encode. This is acceptance
            // criterion 3 — back-navigation may never double-create.
            if state.creation_sent {
                return PreWorldProductResult::refuse(
                    "CharacterCreate refused — already sent this session",
                );
            }
            state.create_draft.slot = slot;
            state.create_draft.realm = state.create_realm.max(1);
            if let Some(r) = caer_protocol::charcreate::create_packet_region(
                state.create_draft.realm,
                state.create_draft.race,
                state.create_draft.class_id,
            ) {
                state.create_draft.region = r;
            }
            // DOLSharp IsCharacterValid mirror: race bases respected and exactly 30 points
            // spent. StatsAdjust cannot produce an invalid distribution; this validates the
            // draft itself, so any future path that mutates stats directly still cannot put an
            // illegal character on the wire.
            if let Err(msg) = caer_protocol::starting_stats::validate_distribution(
                state.create_draft.race,
                &state.create_draft.stats,
            ) {
                return PreWorldProductResult::refuse(msg);
            }
            // CustomMode=1 tells the oracle this create carries appearance data (DOLSharp
            // CreateCharacter: `if pdata.CustomMode == 0x01` stores the seven bytes).
            state.create_draft.enable_customization();
            // B3: one authority for every realm. The renderer used to carry its own Albion-only
            // table that disagreed with the career data (it refused HalfOgre/Minotaur Armsmen the
            // career rows allow), and Midgard/Hibernia illegal tuples could dispatch unchecked.
            match caer_protocol::create_validity::classify(
                state.create_draft.realm,
                state.create_draft.class_id,
                state.create_draft.race,
                state.create_draft.gender,
            ) {
                caer_protocol::create_validity::Legality::Allowed => {}
                caer_protocol::create_validity::Legality::Forbidden => {
                    return PreWorldProductResult::refuse(format!(
                        "realm {} class {} race {} gender {} is not a legal combination",
                        state.create_draft.realm,
                        state.create_draft.class_id,
                        state.create_draft.race,
                        state.create_draft.gender
                    ));
                }
                // Fail closed: a missing EligibleRaces row is a provenance gap, never an allow.
                caer_protocol::create_validity::Legality::Unestablished => {
                    return PreWorldProductResult::refuse(format!(
                        "realm {} class {} race {} legality is UNESTABLISHED (oracle gap) — refusing to guess",
                        state.create_draft.realm,
                        state.create_draft.class_id,
                        state.create_draft.race
                    ));
                }
            }
            // B4: refuse to encode a draft whose model does not match its own (race, gender).
            // Female Minotaur and any desync land here rather than shipping a wrong avatar.
            if !state.create_draft.model_matches_identity() {
                return PreWorldProductResult::refuse(format!(
                    "CharacterCreate refused — model {} is not the oracle model for race {} gender {}",
                    state.create_draft.creation_model,
                    state.create_draft.race,
                    state.create_draft.gender
                ));
            }
            state.pending_create_name = Some(state.create_draft.name.clone());
            state.name_edit_focus = false;
            state.creation_sent = true;
            PreWorldProductResult::cmds(
                vec![LiveCommand::CreateCharacter {
                    draft: state.create_draft.clone(),
                }],
                true,
            )
        }
        PreWorldAction::CharCreateRandomName => {
            // Compose from the client's own fragments for the selected race. No table (or an
            // unknown race) means the button is inert — never a fabricated name.
            let race = state.create_draft.race;
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0u64, |d| d.subsec_nanos() as u64 ^ d.as_secs());
            let mut rng = seed | 1;
            let generated = state.names.generate(race, |n| {
                // xorshift64* — deterministic given the seed, no rand dependency.
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                (rng % n.max(1) as u64) as usize
            });
            match generated {
                Some(name) => {
                    state.create_draft.name = name;
                    state.name_edit_focus = false;
                    PreWorldProductResult::local(false)
                }
                None => PreWorldProductResult::refuse(format!(
                    "Random name unavailable for race {race} (no charman/names.dat fragments)"
                )),
            }
        }
        PreWorldAction::CharCreateFocusName => {
            state.name_edit_focus = true;
            PreWorldProductResult::local(true)
        }
        PreWorldAction::CharCreateGender(g) => {
            // `male_only` is derived from whether the race has a female body at all, and the HUD
            // greys the button from the same flag. A second hardcoded race-id range here would be
            // one of two rules to keep in step.
            let male_only =
                caer_protocol::creation_adapters::race_adapters(state.create_realm.max(1))
                    .iter()
                    .find(|a| a.race_id == state.create_draft.race)
                    .is_some_and(|a| a.male_only);
            if male_only && g == 1 {
                return PreWorldProductResult::refuse(format!(
                    "create gender female blocked for male-only race {}",
                    state.create_draft.race
                ));
            }
            state.create_draft.set_gender(g);
            normalize_source_appearance(state);
            if !combination_allowed(state, state.create_draft.class_id, state.create_draft.race) {
                if let Some(class_id) = first_legal_class(state, state.create_draft.race) {
                    state.create_draft.class_id = class_id;
                }
            }
            PreWorldProductResult::local(true)
        }
        PreWorldAction::CharCreateRace(i) => {
            let realm = state.create_realm.max(1);
            let race = race_id_for_realm(realm, i);
            state.create_draft.set_race(race);
            normalize_source_appearance(state);
            if !combination_allowed(state, state.create_draft.class_id, race) {
                if let Some(class_id) = first_legal_class(state, race) {
                    state.create_draft.class_id = class_id;
                }
            }
            PreWorldProductResult::local(true)
        }
        PreWorldAction::CharCreateClass(i) => {
            let realm = state.create_realm.max(1);
            // Same resolved record the button rendered from — label and wire id cannot diverge.
            let Some(adapter) = caer_protocol::creation_adapters::class_adapter_at(
                realm,
                state.create_draft.race,
                i,
            ) else {
                // Midgard legitimately leaves slot 15 empty; an empty slot must not fall through
                // to a default class.
                return PreWorldProductResult::refuse(format!(
                    "class slot {i} is empty for realm {realm}"
                ));
            };
            if !combination_allowed(state, adapter.class_id, state.create_draft.race) {
                return PreWorldProductResult::refuse(format!(
                    "class {} is not legal for race {} gender {}",
                    adapter.label, state.create_draft.race, state.create_draft.gender
                ));
            }
            state.create_draft.class_id = adapter.class_id;
            PreWorldProductResult::local(true)
        }
        PreWorldAction::OpenCharCreate => {
            let realm = state.create_realm.max(1);
            state.create_draft.realm = realm;
            let race = race_id_for_realm(realm, 0);
            state.create_draft.set_race(race);
            // This is a genuinely fresh record, not merely a return to the form.  Carrying a
            // previous character's hidden appearance bytes or Random locks into it would make
            // the first packet disagree with the visible source defaults.
            let _ = reset_appearance(state);
            state.customizer = CustomizerState::default();
            // Seed the first class that is actually legal for this race rather than slot 0, which
            // may be ineligible and would open the form on a combination Continue must refuse.
            state.create_draft.class_id = first_legal_class(state, race).unwrap_or_else(|| {
                caer_protocol::creation_adapters::class_adapters(realm, race)
                    .first()
                    .map_or(0, |a| a.class_id)
            });
            // Fresh form: empty name until the player types.
            state.create_draft.name.clear();
            state.name_edit_focus = false;
            // A fresh draft is a fresh creation: the exactly-once latch resets here and only
            // here, so returning to the form after a sent packet is a deliberate new attempt.
            state.creation_sent = false;
            PreWorldProductResult::local(true)
        }
        // The Options Menu is entirely local chrome: no packet, no flow step. `apply_hud_navigation`
        // is what opens, edits and closes it.
        PreWorldAction::OpenSettings
        | PreWorldAction::CharCreateCancel
        | PreWorldAction::CharSelectOptions
        | PreWorldAction::Options(_) => PreWorldProductResult::local(true),
        // Quit raises `quit_confirm.xml`; it does not quit. Sending the command here is what made
        // the button navigate: the session's close became `FlowEvent::Closed`, and `Closed` routes
        // to the login screen (ledger A13).
        PreWorldAction::CharSelectQuit | PreWorldAction::QuitConfirmNo => {
            PreWorldProductResult::local(true)
        }
        // Yes is the only pre-world action that ends the process. The command closes the session
        // politely; the shell is what leaves, because a `FlowEvent` cannot express "there is no
        // next screen".
        PreWorldAction::QuitConfirmYes => {
            PreWorldProductResult::cmds(vec![LiveCommand::Quit], false)
        }
        PreWorldAction::BackToRealm => {
            state.create_realm = 0;
            // The draft's realm is the second store of this same fact, and it is the one the HUD
            // draws the race and class buttons from. Leaving it behind kept the old realm's labels
            // on screen while every click resolved through `create_realm.max(1)` — Albion — so the
            // Lurikeen button produced Saracen, its opposite number in the same slot.
            state.create_draft.realm = 0;
            state.selected_protocol_slot = None;
            state.ui_select_sent = false;
            state.overview = None;
            PreWorldProductResult::local(true)
        }
        // Delete raises `delete_confirm.xml`; it does not delete. Same shape as Quit, and for a
        // stronger reason — this one is irreversible.
        PreWorldAction::DeleteCharacter | PreWorldAction::DeleteConfirmNo => {
            PreWorldProductResult::local(true)
        }
        PreWorldAction::DeleteConfirmYes => {
            // The confirmed delete. Realm and slot ARE the payload — the server resolves the target
            // as `slot + realm * 100` and deletes it.
            //
            // `transition.realm()` rather than the `create_realm.max(1)` every other realm-taking
            // arm here uses. **This is a coupling choice, not a bug fix, and the difference matters
            // enough to say so:** `choose_realm` is the only writer of either and sets both, so the
            // two cannot currently disagree. What `transition.realm()` buys is that it is the *same*
            // value `RequestCharacterOverview` was sent with, so the realm we delete in and the realm
            // whose slots we are indexing are read from one place instead of two that happen to
            // agree. `.max(1)` would additionally turn "no realm" into Albion, which is a reasonable
            // default for a create form the player is still filling in and the wrong shape entirely
            // for an irreversible action.
            //
            // The `realm == 0` arm below is **not reachable today** — `choose_realm` clamps to 1..3
            // and the field defaults to 1 — and is kept as a wire-validity check rather than
            // presented as a guard that has ever fired.
            let Some(slot) = state.selected_protocol_slot else {
                return PreWorldProductResult::refuse(
                    "Delete needs a selected character; no row is selected",
                );
            };
            let realm = state.transition.realm();
            if realm == 0 {
                return PreWorldProductResult::refuse(
                    "Delete needs a committed realm; the session has not bound one",
                );
            }
            PreWorldProductResult::cmds(vec![LiveCommand::DeleteCharacter { realm, slot }], true)
        }
        PreWorldAction::SelectCharacterSlot(slot) => {
            // Local only. Retail does not send anything when you click a row — SelectCharacter
            // goes out on Play, which is why sending on row click skipped the normal
            // select-then-Play interaction and made EnterWorld unreachable.
            state.selected_protocol_slot = Some(slot);
            PreWorldProductResult::local(false)
        }
        PreWorldAction::EnterWorld => {
            let Some(ov) = state.overview.as_ref() else {
                return PreWorldProductResult::refuse("EnterWorld refused — overview not present");
            };
            let Some(slot) = state.selected_protocol_slot else {
                return PreWorldProductResult::refuse("EnterWorld refused — no selected slot");
            };
            // Resolve by protocol slot, never by row: the overview may have been refreshed since
            // the click, and the character that was at that row may now be a different one.
            let Some(summary) = ov.characters.iter().find(|c| c.slot == slot).cloned() else {
                return PreWorldProductResult::refuse(
                    "EnterWorld refused — selected character is no longer in the overview",
                );
            };
            dispatch_select_character(state, &summary)
        }
    }
}

/// SCN-01: occupied overview row → SelectCharacter with that overview slot (not row zero).
pub fn dispatch_select_character(
    state: &mut PreWorldProductState,
    summary: &CharacterSummary,
) -> PreWorldProductResult {
    if state.ui_select_sent {
        return PreWorldProductResult::refuse("SelectCharacter already sent this session");
    }
    let _fx = state.transition.select_character(summary);
    state.ui_select_sent = true;
    PreWorldProductResult::cmds(
        vec![LiveCommand::SelectCharacter { slot: summary.slot }],
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preworld::PreWorldHud;

    pub(super) fn occupied(slot: u8, name: &str) -> CharacterSummary {
        CharacterSummary {
            slot,
            level: 50,
            name: name.into(),
            location: "Camelot Hills".into(),
            class_name: "Armsman".into(),
            race_name: "Briton".into(),
            region: 1,
            class_id: 2,
            realm: 1,
            stats: [60; 8],
            race_gender: 1,
            ..Default::default()
        }
    }

    /// A product state seeded with an overview — the shape every dispatch test starts from.
    pub(super) fn state_with(overview: CharacterOverview) -> PreWorldProductState {
        PreWorldProductState {
            overview: Some(overview),
            ..Default::default()
        }
    }

    pub(super) fn overview_with(chars: Vec<CharacterSummary>) -> CharacterOverview {
        CharacterOverview {
            flags: 0,
            characters: chars,
        }
    }

    #[test]
    fn login_play_is_local_navigation_no_auth_command() {
        let mut state = PreWorldProductState::default();
        let r = dispatch_preworld_action(&mut state, PreWorldAction::LoginPlay);
        assert!(r.commands.is_empty(), "LoginPlay must emit no LiveCommand");
        assert!(r.refused.is_none());
        assert!(r.apply_hud_navigation);
        // B5: the screen that follows LoginPlay is PreWorldFlow's call, not the HUD's. The
        // no-network contract this test exists to protect is the assertion above — that LoginPlay
        // emits zero LiveCommands and is never an account gate.
        let mut hud = PreWorldHud::new(std::path::PathBuf::from("/nonexistent"));
        let before = hud.screen();
        hud.apply_action(PreWorldAction::LoginPlay);
        assert_eq!(
            hud.screen(),
            before,
            "LoginPlay must not self-navigate; the flow projects the next screen"
        );
    }

    #[test]
    fn realms_1_2_3_emit_overview_request_with_exact_realm() {
        for realm in [1u8, 2, 3] {
            let mut state = PreWorldProductState::default();
            let r = dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(realm));
            assert_eq!(state.transition.realm(), realm);
            assert_eq!(state.create_draft.realm, realm);
            assert_eq!(state.create_realm, realm);
            match r.commands.as_slice() {
                [LiveCommand::RequestCharacterOverview { realm: got }] => {
                    assert_eq!(*got, realm, "overview realm must match pick")
                }
                other => panic!("realm {realm} expected RequestCharacterOverview, got {other:?}"),
            }
        }
    }

    #[test]
    fn customize_continue_emits_create_with_free_slot_and_draft_values() {
        let mut state = state_with(overview_with(vec![
            occupied(0, "Taken"),
            occupied(2, "Also"),
        ]));
        apply_create_name_input(&mut state, "FreeSlot");
        state.create_draft.set_gender(1);
        let race_before = state.create_draft.race;
        let class_before = state.create_draft.class_id;
        let r = confirm_create(&mut state);
        assert!(r.refused.is_none(), "{:?}", r.refused);
        match r.commands.as_slice() {
            [LiveCommand::CreateCharacter { draft }] => {
                assert_eq!(draft.slot, 1, "first free slot must skip 0 and 2");
                assert_eq!(draft.name, "FreeSlot");
                assert_eq!(draft.gender, 1);
                assert_eq!(draft.race, race_before);
                assert_eq!(draft.class_id, class_before);
                // A fresh form still has to be sent with CustomMode=1 so DOLSharp persists its
                // appearance bytes. In that explicit mode zero is not the base head: it is the
                // leftmost value of all four facial sliders. This is the exact known-bad wire
                // shape that collapsed Astiliwyr's Highlander eye during the 2026-08-30 playtest.
                // The production create path must instead serialize the retail neutral midpoint.
                assert_eq!(
                    (draft.eye_size, draft.lip_size),
                    (0x44, 0x44),
                    "an untouched explicit face must not serialize four leftmost morph ticks"
                );
                for slot in caer_protocol::customization::FacialMorphSlot::ALL {
                    assert_eq!(
                        draft.facial_morph_tick(slot),
                        caer_protocol::customization::FACIAL_MORPH_NEUTRAL_TICK,
                        "{slot:?} must stay neutral until the player moves that slider"
                    );
                }
                assert!(caer_protocol::starting_stats::validate_distribution(
                    draft.race,
                    &draft.stats
                )
                .is_ok());
            }
            other => panic!("expected CreateCharacter, got {other:?}"),
        }
    }

    #[test]
    fn customize_continue_refuses_absent_overview() {
        let mut state = PreWorldProductState::default();
        assert!(state.overview.is_none());
        apply_create_name_input(&mut state, "NoOv");
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(
            r.commands.is_empty(),
            "absent overview must not CreateCharacter"
        );
        assert!(
            !r.apply_hud_navigation,
            "absent overview must not advance HUD"
        );
        assert!(
            r.refused.as_deref().is_some_and(|m| m.contains("overview")),
            "{:?}",
            r.refused
        );
    }

    #[test]
    fn customize_continue_refuses_full_overview_slots_0_through_9() {
        let mut state = state_with(overview_with(
            (0..10u8).map(|s| occupied(s, &format!("C{s}"))).collect(),
        ));
        apply_create_name_input(&mut state, "FullBox");
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(r.commands.is_empty(), "full overview must not alias slot 0");
        assert!(!r.apply_hud_navigation);
        assert!(
            r.refused.as_deref().is_some_and(|m| m.contains("free")),
            "{:?}",
            r.refused
        );
    }

    #[test]
    fn customize_continue_refuses_blank_name_no_newchar_invention() {
        // Layer 1 — the player-facing gate: the name box lives on the create form, so Continué
        // refuses before an unnamed draft can walk downstream at all.
        let mut state = state_with(overview_with(vec![]));
        assert!(state.create_draft.name.is_empty());
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateContinue);
        assert!(r.commands.is_empty(), "refusal mints nothing");
        assert!(!r.apply_hud_navigation, "refusal does not advance");
        assert!(r.refused.is_some());

        // Layer 2 — defense in depth: open the stats modal legitimately, then blank the draft
        // directly (any future path that mutates state under the flow). Continue must refuse
        // independently, not lean on the earlier screen having checked.
        apply_create_name_input(&mut state, "Walker");
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateContinue);
        assert!(r.refused.is_none());
        dispatch_preworld_action(&mut state, PreWorldAction::CustomizeStats);
        state.create_draft.name.clear();
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(r.commands.is_empty());
        assert!(!r.apply_hud_navigation);
        assert!(r.refused.is_some());
        // Placeholder leftover must also refuse (no silent Newchar submit).
        state.create_draft.name = "Newchar".into();
        let r2 = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(r2.commands.is_empty());
        assert!(r2.refused.is_some());
    }

    #[test]
    fn typed_name_input_reaches_exactly_one_create_character() {
        let mut state = PreWorldProductState::default();
        dispatch_preworld_action(&mut state, PreWorldAction::OpenCharCreate);
        assert!(state.create_draft.name.is_empty());
        state.overview = Some(overview_with(vec![occupied(1, "Other")]));
        apply_create_name_input(&mut state, "TypedByPlayer");
        let r = confirm_create(&mut state);
        assert!(r.refused.is_none(), "{:?}", r.refused);
        match r.commands.as_slice() {
            [LiveCommand::CreateCharacter { draft }] => {
                assert_eq!(draft.name, "TypedByPlayer");
                assert_eq!(draft.slot, 0, "free slot must be 0 when only 1 occupied");
            }
            other => panic!("expected exactly one CreateCharacter, got {other:?}"),
        }
    }

    /// The client's rule, in the client's words: "Names can only contain characters A-z."
    ///
    /// Digits used to be accepted, which is how `Ca33435` reached a live server as a character
    /// name. Both halves are asserted here — the keystroke filter and the submit-time check —
    /// because a filter alone would leave Random and any future paste path unguarded.
    #[test]
    fn create_names_are_letters_only_and_cap_at_twenty() {
        let mut name = String::new();
        append_create_name_chars(&mut name, "Ab1!@#Cd_Ef");
        assert_eq!(name, "AbCdEf", "digits and punctuation must be dropped");
        append_create_name_chars(&mut name, "0123456789ABCDEFGHIJKLMNOP");
        assert_eq!(name.len(), CREATE_NAME_MAX_LEN);
        assert_eq!(&name[..6], "AbCdEf");
        // Further input must not grow past the cap.
        append_create_name_chars(&mut name, "ZZZ");
        assert_eq!(name.len(), CREATE_NAME_MAX_LEN);

        assert_eq!(
            validate_create_name("Ca33435"),
            Some("Names can only contain characters A-z.")
        );
        assert_eq!(
            validate_create_name(""),
            Some("You must enter a name before continuing.")
        );
        assert_eq!(validate_create_name("Ab"), Some("Your name is too short!"));
        assert_eq!(
            validate_create_name(&"A".repeat(21)),
            Some("That name is too long.")
        );
        assert_eq!(validate_create_name("Cadellin"), None);
    }

    /// **Ledger B5.** A confirmed delete names the realm the session actually committed to, and
    /// refuses when it cannot.
    ///
    /// Realm and slot are the entire payload — the server deletes `slot + realm * 100` — so a
    /// default here is not a fallback, it is a different character. Every other realm-taking arm in
    /// this file writes `create_realm.max(1)`, which silently means Albion; that idiom is fine for a
    /// create form the player is still filling in and wrong for an irreversible action, which is why
    /// this one refuses twice instead.
    ///
    /// **This gate has no red control against the realm source, and that is a finding, not an
    /// omission.** Swapping `transition.realm()` back to `create_realm.max(1)` leaves it green,
    /// because `choose_realm` is the only writer of either and sets both together — the two cannot
    /// diverge in any reachable state, so no test can tell them apart. The claim "seen red by
    /// restoring `create_realm.max(1)`" was written here first and was **false**; it is recorded
    /// rather than deleted because a plausible red-control claim that nobody runs is exactly the
    /// defect class this file's discipline exists to catch. The slot guard *does* have one: remove
    /// it and the no-selection case sends a packet.
    #[test]
    fn confirmed_delete_names_the_committed_realm_and_refuses_without_one() {
        // No row selected: nothing goes out.
        let mut state = state_with(overview_with(vec![]));
        let r = dispatch_preworld_action(&mut state, PreWorldAction::DeleteConfirmYes);
        assert!(r.commands.is_empty(), "no packet without a selected row");
        assert!(r.refused.is_some(), "and the refusal is reported");

        // The realm sent must be **the realm the overview was requested for**, which is the only
        // thing that makes the slot mean a particular character. `ChooseRealm` sets
        // `transition.realm()` and the overview request reads the same value, so this asserts the
        // two agree rather than asserting a constant.
        //
        // Note `transition.realm()` defaults to **1**, not 0 — so the `realm == 0` refusal in the
        // dispatch is defensive rather than reachable from this state, and this test deliberately
        // does not pretend otherwise. What protects a fresh session is `selected_protocol_slot`,
        // which is `None` until the player clicks a row, and rows only exist once an overview for a
        // specific realm has arrived.
        for realm in 1..=3u8 {
            let mut state = state_with(overview_with(vec![]));
            let chose = dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(realm));
            let requested = chose.commands.iter().find_map(|c| match c {
                LiveCommand::RequestCharacterOverview { realm } => Some(*realm),
                _ => None,
            });
            state.selected_protocol_slot = Some(1);
            let r = dispatch_preworld_action(&mut state, PreWorldAction::DeleteConfirmYes);
            let sent = match r.commands.as_slice() {
                [LiveCommand::DeleteCharacter { realm, .. }] => *realm,
                other => panic!("expected one delete, got {other:?}"),
            };
            assert_eq!(
                Some(sent),
                requested,
                "the delete must name the realm the overview was requested for"
            );
        }

        // Committed realm and a selected row: exactly one delete, for that realm and slot.
        for (realm, slot) in [(1u8, 0u8), (2, 5), (3, 9)] {
            let mut state = state_with(overview_with(vec![]));
            dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(realm));
            state.selected_protocol_slot = Some(slot);
            let r = dispatch_preworld_action(&mut state, PreWorldAction::DeleteConfirmYes);
            assert!(
                r.refused.is_none(),
                "realm {realm} slot {slot} must not refuse"
            );
            assert!(
                matches!(
                    r.commands.as_slice(),
                    [LiveCommand::DeleteCharacter { realm: cr, slot: cs }]
                        if *cr == realm && *cs == slot
                ),
                "realm {realm} slot {slot} produced {:?}",
                r.commands
            );
        }
    }

    /// **Ledger B5.** Delete itself sends nothing — it only raises the confirmation.
    #[test]
    fn pressing_delete_sends_no_packet() {
        let mut state = state_with(overview_with(vec![]));
        dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(1));
        state.selected_protocol_slot = Some(2);
        let r = dispatch_preworld_action(&mut state, PreWorldAction::DeleteCharacter);
        assert!(
            r.commands.is_empty(),
            "Delete raises delete_confirm.xml; it must not delete"
        );
        assert!(r.refused.is_none(), "and it is not a refusal either");
        let r = dispatch_preworld_action(&mut state, PreWorldAction::DeleteConfirmNo);
        assert!(r.commands.is_empty(), "Cancel sends nothing");
    }

    /// Drive the creation chain to the source stats modal. Continué opens the customizer and
    /// Adjust Attributes opens the local overlay; neither sends a character-create packet.
    fn drive_to_stats(state: &mut PreWorldProductState) {
        let r = dispatch_preworld_action(state, PreWorldAction::CharCreateContinue);
        assert!(r.commands.is_empty(), "Continué mints nothing");
        assert!(r.apply_hud_navigation, "Continué advances to customize");
        let r = dispatch_preworld_action(state, PreWorldAction::CustomizeStats);
        assert!(r.commands.is_empty(), "Adjust Attributes sends nothing");
        assert!(r.apply_hud_navigation, "Adjust Attributes opens stats");
    }

    fn confirm_create(state: &mut PreWorldProductState) -> PreWorldProductResult {
        let r = dispatch_preworld_action(state, PreWorldAction::CharCreateContinue);
        assert!(r.commands.is_empty(), "Continué mints nothing");
        dispatch_preworld_action(state, PreWorldAction::CustomizeAdvance)
    }

    /// The two 8-cell eye/skin palettes share a byte. This catches the silent destructive
    /// implementation where clicking eye colour overwrote the tone (or vice versa), and pins the
    /// source-derived selector caps that replaced the old invented 0..=20 range.
    #[cfg(any())]
    #[test]
    fn customization_palettes_preserve_packed_components_and_default_is_real_reset() {
        let mut state = PreWorldProductState::default();
        let skin = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizePalette {
                palette: 0,
                value: 4,
            },
        );
        assert!(skin.refused.is_none());
        assert_eq!(state.create_draft.eye_color, 0x04);
        let eye = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizePalette {
                palette: 1,
                value: 7,
            },
        );
        assert!(eye.refused.is_none());
        assert_eq!(
            state.create_draft.eye_color, 0x74,
            "eye picker must preserve the selected skin nibble"
        );
        let hair = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizePalette {
                palette: 2,
                value: 12,
            },
        );
        assert!(hair.refused.is_none());
        assert_eq!(state.create_draft.hair_color, 12);
        assert_eq!(state.create_draft.custom_mode, 1);

        // The no-client fixture follows the largest known bounded legacy range. The live shell
        // supplies `AppearanceCatalog`, which replaces this with each race/gender's exact source
        // list (including holes such as Celt Male Bald).
        for (field, max) in [(4u8, 7u8), (5, 8), (6, 8)] {
            for _ in 0..20 {
                let r = dispatch_preworld_action(
                    &mut state,
                    PreWorldAction::CustomizeCycle { field, dir: 1 },
                );
                assert!(r.refused.is_none());
            }
            let got = match field {
                4 => state.create_draft.face_type,
                5 => state.create_draft.hair_style,
                _ => state.create_draft.mood_type,
            };
            assert_eq!(got, max, "field {field} must cap at its authored range");
        }
        let bad = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizePalette {
                palette: 1,
                value: 9,
            },
        );
        assert!(bad.refused.is_some(), "ninth eye cell does not exist");
        assert_eq!(
            state.create_draft.eye_color, 0x74,
            "bad click is non-mutating"
        );

        let reset = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeReset);
        assert!(reset.refused.is_none());
        assert_eq!(state.create_draft.custom_mode, 0);
        assert_eq!(
            (
                state.create_draft.eye_size,
                state.create_draft.lip_size,
                state.create_draft.eye_color,
                state.create_draft.hair_color,
                state.create_draft.face_type,
                state.create_draft.hair_style,
                state.create_draft.mood_type,
            ),
            (0, 0, 0, 0, 0, 0, 0),
            "Default restores the exact uncustomized wire state"
        );
    }

    /// The replacement for the old colour-cell test: the live runtime profile carries eye and
    /// hair through textual source selectors, skin through a slider, and Mood through its own
    /// byte. This must fail if any of those controls is routed back through the palette mapper.
    #[test]
    fn runtime_selectors_and_sliders_preserve_the_real_appearance_owners() {
        let root = caer_assets::client_dep::required_caer_client_root(
            "runtime_selectors_and_sliders_preserve_the_real_appearance_owners",
        );
        let catalog =
            Arc::new(AppearanceCatalog::load(&root).expect("load retail appearance catalogue"));
        let choices = catalog
            .choices(1, 0)
            .expect("Briton male source appearance choices")
            .clone();
        let mut state = PreWorldProductState {
            appearance_catalog: Some(Arc::clone(&catalog)),
            ..PreWorldProductState::default()
        };
        state.create_draft.set_race(1);
        state.create_draft.set_gender(0);

        let skin = choices.skin_tones()[0];
        let eye = choices.eye_colours()[0];
        let skin_result = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeSlider {
                field: CustomizerField::SkinTone,
                tick: 1,
            },
        );
        assert!(skin_result.refused.is_none(), "{skin_result:?}");
        let eye_result = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeAdjust {
                field: CustomizerField::EyeColor,
                dir: 1,
            },
        );
        assert!(eye_result.refused.is_none(), "{eye_result:?}");
        assert_eq!(
            state.create_draft.eye_color,
            skin | (eye << 4),
            "Eye Color selector must preserve the Skin Tone slider's low nibble"
        );

        let skin_default = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeSlider {
                field: CustomizerField::SkinTone,
                tick: 0,
            },
        );
        assert!(skin_default.refused.is_none());
        assert_eq!(state.create_draft.eye_color & 0x0F, 0);

        let hair_style = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeAdjust {
                field: CustomizerField::HairStyle,
                dir: 1,
            },
        );
        assert!(hair_style.refused.is_none(), "{hair_style:?}");
        let hair_colour = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeAdjust {
                field: CustomizerField::HairColor,
                dir: 1,
            },
        );
        assert!(hair_colour.refused.is_none(), "{hair_colour:?}");
        assert!(
            choices
                .palette_values(2, state.create_draft.hair_style)
                .is_some_and(|values| values.contains(&state.create_draft.hair_color)),
            "Hair Color must stay inside the source values for the selected Hair Style"
        );

        let mood = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeSlider {
                field: CustomizerField::Mood,
                tick: 7,
            },
        );
        assert!(mood.refused.is_none(), "{mood:?}");
        assert_eq!(state.create_draft.mood_type, 7);
        assert_eq!(
            state.customizer.tattoo_index(),
            0,
            "Mood and Tattoo must never share the same state owner"
        );

        let before = state.create_draft.clone();
        let bad = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeSlider {
                field: CustomizerField::SkinTone,
                tick: 9,
            },
        );
        assert!(
            bad.refused.is_some(),
            "tick nine is outside the observed slider"
        );
        assert_eq!(state.create_draft, before, "refused tick is non-mutating");

        let reset = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeReset);
        assert!(reset.refused.is_none());
        assert_eq!(state.create_draft.custom_mode, 0);
        assert_eq!(state.create_draft.customization(), Default::default());
        assert_eq!(state.customizer.tattoo_index(), 0);
    }

    /// Walk every player-facing retail appearance value through the same semantic actions the
    /// customizer dispatches.  A catalogue audit alone proves that a row exists; this closes the
    /// remaining gap by proving that every face, hairstyle, eye colour, hair colour, skin tone,
    /// and (where supplied) tattoo can actually be reached without inventing a generic range.
    ///
    /// The old palette form passed a handful of representative Briton checks while omitting
    /// race-specific hair sheets and preserving invalid colour bytes after a style change.  The
    /// full 18-race × two-gender surface is the discriminating guard for that regression.
    #[test]
    fn every_retail_appearance_choice_is_reachable_from_live_customizer_actions() {
        let root = caer_assets::client_dep::required_caer_client_root(
            "every_retail_appearance_choice_is_reachable_from_live_customizer_actions",
        );
        let catalog =
            Arc::new(AppearanceCatalog::load(&root).expect("load retail appearance catalogue"));
        let mut identities = 0usize;

        for race in 1..=18u8 {
            for gender in 0..=1u8 {
                let Some(choices) = catalog.choices(race, gender).cloned() else {
                    continue;
                };
                identities += 1;
                let mut state = PreWorldProductState {
                    appearance_catalog: Some(Arc::clone(&catalog)),
                    ..PreWorldProductState::default()
                };
                state.create_draft.set_race(race);
                state.create_draft.set_gender(gender);
                let identity = format!("race {race} gender {gender}");

                for (field, selector) in [
                    (CustomizerField::Face, AppearanceSelector::Face),
                    (CustomizerField::HairStyle, AppearanceSelector::HairStyle),
                    (CustomizerField::Tattoo, AppearanceSelector::Tattoo),
                ] {
                    let values = choices.selector_values(selector);
                    let mut prior = 0u8;
                    for choice in values {
                        assert!(
                            choice.index > prior,
                            "{identity} {selector:?} indices must be strictly source-ordered: \
                             {prior} then {}",
                            choice.index
                        );
                        let result = dispatch_preworld_action(
                            &mut state,
                            PreWorldAction::CustomizeAdjust { field, dir: 1 },
                        );
                        assert!(
                            result.refused.is_none(),
                            "{identity} {field:?} value {} refused: {result:?}",
                            choice.index
                        );
                        let got = match field {
                            CustomizerField::Face => state.create_draft.face_type,
                            CustomizerField::HairStyle => state.create_draft.hair_style,
                            CustomizerField::Tattoo => state.customizer.tattoo_index(),
                            _ => unreachable!("selector table only contains selector fields"),
                        };
                        assert_eq!(
                            got, choice.index,
                            "{identity} {field:?} skipped a source value"
                        );
                        prior = choice.index;
                    }
                }

                let mut prior_eye = 0u8;
                for &eye in choices.eye_colours() {
                    assert!(
                        eye > prior_eye,
                        "{identity} Eye Color indices must preserve the retail order"
                    );
                    let result = dispatch_preworld_action(
                        &mut state,
                        PreWorldAction::CustomizeAdjust {
                            field: CustomizerField::EyeColor,
                            dir: 1,
                        },
                    );
                    assert!(result.refused.is_none(), "{identity} Eye Color: {result:?}");
                    assert_eq!(
                        state.create_draft.eye_color >> 4,
                        eye,
                        "{identity} Eye Color skipped a source value"
                    );
                    prior_eye = eye;
                }

                assert!(
                    choices.skin_tones().len()
                        <= usize::from(crate::preworld_customize::SLIDER_MAX_TICK),
                    "{identity} has more Skin Tone values than the retail eight-choice slider"
                );
                for (offset, &skin) in choices.skin_tones().iter().enumerate() {
                    let tick = u8::try_from(offset + 1).expect("retail slider tick fits u8");
                    let result = dispatch_preworld_action(
                        &mut state,
                        PreWorldAction::CustomizeSlider {
                            field: CustomizerField::SkinTone,
                            tick,
                        },
                    );
                    assert!(
                        result.refused.is_none(),
                        "{identity} Skin Tone {tick}: {result:?}"
                    );
                    assert_eq!(
                        state.create_draft.eye_color & 0x0F,
                        skin,
                        "{identity} Skin Tone tick {tick} skipped a source value"
                    );
                }

                // Hair colour maps are keyed by hairstyle.  Set each source style explicitly,
                // then prove its whole authored palette can be selected one item at a time.
                for style in choices.selector_values(AppearanceSelector::HairStyle) {
                    state.create_draft.hair_style = style.index;
                    state.create_draft.hair_color = 0;
                    let mut prior_colour = 0u8;
                    for &colour in choices.hair_colours(style.index) {
                        assert!(
                            colour > prior_colour,
                            "{identity} Hair Style {} colour indices must preserve retail order",
                            style.index
                        );
                        let result = dispatch_preworld_action(
                            &mut state,
                            PreWorldAction::CustomizeAdjust {
                                field: CustomizerField::HairColor,
                                dir: 1,
                            },
                        );
                        assert!(
                            result.refused.is_none(),
                            "{identity} Hair Style {} colour {colour}: {result:?}",
                            style.index
                        );
                        assert_eq!(
                            state.create_draft.hair_color, colour,
                            "{identity} Hair Style {} skipped a source colour",
                            style.index
                        );
                        prior_colour = colour;
                    }
                }
            }
        }
        assert_eq!(
            identities, 36,
            "the complete audited fig3 player-race surface must remain loaded"
        );
    }

    /// Random and its locks are form-local controls; Size is model-owned. Their possible
    /// appearance values must still be the retail table's values. This is the first-native-run regression:
    /// the old screen drew disabled/invented controls, so a Briton could show a tattoo row and no
    /// action could prove its selections legal.
    #[test]
    fn source_customizer_random_and_locks_use_only_legal_briton_choices() {
        let root = caer_assets::client_dep::required_caer_client_root(
            "source_customizer_random_and_locks_use_only_legal_briton_choices",
        );
        let catalog =
            Arc::new(AppearanceCatalog::load(&root).expect("load retail appearance catalogue"));
        let choices = catalog
            .choices(1, 0)
            .expect("Briton male has a retail appearance row")
            .clone();
        let mut state = PreWorldProductState {
            appearance_catalog: Some(Arc::clone(&catalog)),
            ..PreWorldProductState::default()
        };
        state.create_draft.set_race(1);
        state.create_draft.set_gender(0);

        let random = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeRandom);
        assert!(random.refused.is_none(), "{random:?}");
        assert!(random.commands.is_empty(), "Random is local UI state");
        assert!(choices
            .selector_values(AppearanceSelector::Face)
            .iter()
            .any(|choice| choice.index == state.create_draft.face_type));
        assert!(choices
            .selector_values(AppearanceSelector::HairStyle)
            .iter()
            .any(|choice| choice.index == state.create_draft.hair_style));
        assert!(choices.allows_palette(
            0,
            state.create_draft.eye_color & 0x0F,
            state.create_draft.hair_style,
        ));
        assert!(choices.allows_palette(
            1,
            state.create_draft.eye_color >> 4,
            state.create_draft.hair_style,
        ));
        assert!(choices.allows_palette(
            2,
            state.create_draft.hair_color,
            state.create_draft.hair_style,
        ));
        assert!(
            state.create_draft.mood_type <= crate::preworld_customize::SLIDER_MAX_TICK,
            "Mood is its own observed nine-position slider, not Briton's absent decal list"
        );
        assert_eq!(
            state.customizer.tattoo_index(),
            0,
            "Briton has no retail Tattoo row to randomize"
        );
        assert!(choices
            .scale()
            .iter()
            .any(|choice| choice.index == state.create_draft.creation_size()));

        let locked_face = state.create_draft.face_type;
        let lock = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeToggleLock {
                field: CustomizerField::Face,
            },
        );
        assert!(lock.refused.is_none());
        assert!(state.customizer.is_locked(CustomizerField::Face));
        for _ in 0..4 {
            let random = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeRandom);
            assert!(random.refused.is_none());
        }
        assert_eq!(
            state.create_draft.face_type, locked_face,
            "the Face lock must survive and constrain every later Random press"
        );

        let reset = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeReset);
        assert!(reset.refused.is_none());
        assert_eq!(
            state.create_draft.creation_size(),
            caer_protocol::charcreate::CREATION_SIZE_AVERAGE,
            "Default restores source Average"
        );
        let size = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeAdjust {
                field: CustomizerField::Size,
                dir: 1,
            },
        );
        assert!(size.refused.is_none());
        assert_eq!(
            state.create_draft.creation_size(),
            caer_protocol::charcreate::CREATION_SIZE_TALL,
            "Average -> Tall from source map"
        );
        let invalid_lock = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeToggleLock {
                field: CustomizerField::Morph(4),
            },
        );
        assert!(invalid_lock.refused.is_some());
    }

    /// Random is not allowed to combine a source hairstyle with a colour sheet belonging to a
    /// different hairstyle.  Exercise every audited fig3 player identity repeatedly, because a
    /// one-race control can miss the exact stale-style ordering bug that caused a random Highlander
    /// to appear correct locally yet serialize an incoherent appearance packet.
    #[test]
    fn source_random_uses_the_selected_styles_own_palette_for_every_identity() {
        let root = caer_assets::client_dep::required_caer_client_root(
            "source_random_uses_the_selected_styles_own_palette_for_every_identity",
        );
        let catalog = Arc::new(
            AppearanceCatalog::load(&root).expect("load retail appearance catalogue for Random"),
        );
        let mut identities = 0usize;

        for race in 1..=18u8 {
            for gender in 0..=1u8 {
                let Some(choices) = catalog.choices(race, gender).cloned() else {
                    continue;
                };
                identities += 1;
                let identity = format!("race {race} gender {gender}");
                let mut state = PreWorldProductState {
                    appearance_catalog: Some(Arc::clone(&catalog)),
                    ..PreWorldProductState::default()
                };
                state.create_draft.set_race(race);
                state.create_draft.set_gender(gender);

                // Multiple presses advance the deterministic UI entropy through distinct source
                // values.  One press is insufficient to prove an authored hole or per-style map
                // cannot be crossed after the initial seed.
                for press in 0..16 {
                    let result =
                        dispatch_preworld_action(&mut state, PreWorldAction::CustomizeRandom);
                    assert!(
                        result.refused.is_none(),
                        "{identity} Random press {press} refused: {result:?}"
                    );
                    assert!(
                        result.commands.is_empty(),
                        "{identity} Random press {press} must stay local"
                    );

                    if !choices.selector_values(AppearanceSelector::Face).is_empty() {
                        assert!(
                            choices
                                .selector_values(AppearanceSelector::Face)
                                .iter()
                                .any(|choice| choice.index == state.create_draft.face_type),
                            "{identity} Random press {press} chose a non-source face {}",
                            state.create_draft.face_type
                        );
                    }
                    if !choices
                        .selector_values(AppearanceSelector::HairStyle)
                        .is_empty()
                    {
                        assert!(
                            choices
                                .selector_values(AppearanceSelector::HairStyle)
                                .iter()
                                .any(|choice| choice.index == state.create_draft.hair_style),
                            "{identity} Random press {press} chose a non-source hairstyle {}",
                            state.create_draft.hair_style
                        );
                    }
                    if !choices.skin_tones().is_empty() {
                        assert!(
                            choices
                                .skin_tones()
                                .contains(&(state.create_draft.eye_color & 0x0F)),
                            "{identity} Random press {press} chose a non-source skin tone {}",
                            state.create_draft.eye_color & 0x0F
                        );
                    }
                    if !choices.eye_colours().is_empty() {
                        assert!(
                            choices
                                .eye_colours()
                                .contains(&(state.create_draft.eye_color >> 4)),
                            "{identity} Random press {press} chose a non-source eye colour {}",
                            state.create_draft.eye_color >> 4
                        );
                    }

                    let style_colours = choices.hair_colours(state.create_draft.hair_style);
                    if style_colours.is_empty() {
                        assert_eq!(
                            state.create_draft.hair_color, 0,
                            "{identity} Random press {press} borrowed a colour for map-less hairstyle {}",
                            state.create_draft.hair_style
                        );
                    } else {
                        assert!(
                            style_colours.contains(&state.create_draft.hair_color),
                            "{identity} Random press {press} used hair colour {} outside hairstyle {}'s source map",
                            state.create_draft.hair_color,
                            state.create_draft.hair_style
                        );
                    }

                    let tattoos = choices.selector_values(AppearanceSelector::Tattoo);
                    if tattoos.is_empty() {
                        assert_eq!(
                            state.customizer.tattoo_index(),
                            0,
                            "{identity} Random press {press} invented a tattoo"
                        );
                    } else {
                        assert!(
                            tattoos
                                .iter()
                                .any(|choice| choice.index == state.customizer.tattoo_index()),
                            "{identity} Random press {press} chose tattoo {} outside its source map",
                            state.customizer.tattoo_index()
                        );
                    }

                    assert_eq!(
                        state.create_draft.custom_mode, 1,
                        "{identity} Random press {press} must serialize its explicit appearance"
                    );
                    for slot in caer_protocol::customization::FacialMorphSlot::ALL {
                        assert!(
                            state.create_draft.facial_morph_tick(slot)
                                <= crate::preworld_customize::SLIDER_MAX_TICK,
                            "{identity} Random press {press} produced an invalid {slot:?} tick"
                        );
                    }
                }
            }
        }
        assert_eq!(
            identities, 36,
            "the complete audited fig3 player-race surface must remain randomized"
        );
    }

    /// Re-selecting an identity must clear a source-invalid local Tattoo selection without
    /// corrupting the independent MoodType packet byte. Briton male is the direct retail
    /// falsifier: it has no decal entries while Celt does.
    #[test]
    fn race_change_clears_invalid_tattoo_without_corrupting_mood() {
        let root = caer_assets::client_dep::required_caer_client_root(
            "race_change_clears_invalid_tattoo_without_corrupting_mood",
        );
        let catalog =
            Arc::new(AppearanceCatalog::load(&root).expect("load retail appearance catalogue"));
        let mut state = PreWorldProductState {
            appearance_catalog: Some(catalog),
            create_realm: 1,
            ..PreWorldProductState::default()
        };
        // Start with a source-valid Celt tattoo, then choose Albion's Briton slot.
        state.create_draft.set_race(9);
        state.customizer.set_tattoo_index(1);
        state
            .create_draft
            .set_customization(caer_protocol::customization::Customization {
                mood_type: 1,
                ..state.create_draft.customization()
            });

        let result = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateRace(0));
        assert!(result.refused.is_none(), "{result:?}");
        assert_eq!(state.create_draft.race, 1, "Albion slot zero is Briton");
        assert_eq!(
            state.create_draft.mood_type, 1,
            "changing the Tattoo catalogue cannot overwrite the independent MoodType byte"
        );
        assert_eq!(
            state.customizer.tattoo_index(),
            0,
            "Briton has no source tattoo/decal map, so its local selection must reset"
        );
    }

    #[test]
    fn opening_a_new_creation_form_resets_prior_appearance_and_random_locks() {
        let mut state = PreWorldProductState::default();
        state
            .create_draft
            .set_customization(caer_protocol::customization::Customization {
                eye_color: 0x21,
                hair_color: 3,
                face_type: 2,
                hair_style: 4,
                mood_type: 1,
            });
        state
            .create_draft
            .set_facial_morph_tick(caer_protocol::customization::FacialMorphSlot::Nose, 8);
        let lock = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeToggleLock {
                field: CustomizerField::Face,
            },
        );
        assert!(lock.refused.is_none());
        let result = dispatch_preworld_action(&mut state, PreWorldAction::OpenCharCreate);
        assert!(result.refused.is_none(), "{result:?}");
        assert_eq!(state.create_draft.custom_mode, 0);
        assert_eq!(state.create_draft.eye_size, 0);
        assert_eq!(state.create_draft.lip_size, 0);
        assert_eq!(
            state.create_draft.customization(),
            caer_protocol::customization::Customization::default()
        );
        assert_eq!(
            state.create_draft.creation_size(),
            caer_protocol::charcreate::CREATION_SIZE_AVERAGE,
            "fresh source Size is Average"
        );
        assert!(
            !state.customizer.is_locked(CustomizerField::Face),
            "fresh drafts do not inherit Random locks"
        );
    }

    /// The four visible form sliders must reach their distinct DOL-packed nibbles.  This is the
    /// known-bad guard for the former UI: it displayed anonymous slider rows but had no action
    /// path into `EyeSize`/`LipSize`, so every generated face stayed source-default.
    #[test]
    fn facial_morph_sliders_seed_neutral_and_preserve_each_other() {
        let mut state = PreWorldProductState::default();
        for (slot, tick) in [(0, 0), (1, 8), (2, 1), (3, 7)] {
            let result = dispatch_preworld_action(
                &mut state,
                PreWorldAction::CustomizeSlider {
                    field: CustomizerField::Morph(slot),
                    tick,
                },
            );
            assert!(
                result.refused.is_none(),
                "slot {slot} tick {tick}: {result:?}"
            );
        }
        assert_eq!(state.create_draft.eye_size, 0x08, "Nose=0, Eyes=8");
        assert_eq!(state.create_draft.lip_size, 0x71, "Lips/Ears=1, Jaw/Chin=7");
        assert_eq!(state.create_draft.custom_mode, 1);
        assert_eq!(
            state
                .create_draft
                .facial_morph_tick(caer_protocol::customization::FacialMorphSlot::Nose),
            0,
            "an explicit all-left nose remains distinguishable from an untouched default"
        );

        let before = state.create_draft.clone();
        let bad_slot = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeSlider {
                field: CustomizerField::Morph(4),
                tick: 4,
            },
        );
        assert!(bad_slot.refused.is_some());
        let bad_tick = dispatch_preworld_action(
            &mut state,
            PreWorldAction::CustomizeSlider {
                field: CustomizerField::Morph(0),
                tick: 9,
            },
        );
        assert!(bad_tick.refused.is_some());
        assert_eq!(
            state.create_draft, before,
            "invalid UI input is non-mutating"
        );
    }

    /// **E6 acceptance criterion 1.** The round trip a player actually makes: realm → race →
    /// class → Continué → customize → stats → confirm yields exactly one CreateCharacter, with
    /// the chosen values, appearance block included, and `custom_mode = 1` so the oracle stores
    /// it (DOLSharp: `if pdata.CustomMode == 0x01`).
    #[test]
    fn the_full_creation_round_trip_mints_exactly_one_character_with_the_chosen_values() {
        let mut state = state_with(overview_with(vec![
            occupied(0, "Taken"),
            occupied(2, "Also"),
        ]));
        apply_create_name_input(&mut state, "FreeSlot");
        state.create_draft.set_gender(1);
        let race_before = state.create_draft.race;
        let class_before = state.create_draft.class_id;
        // This headless flow fixture intentionally has no client catalogue. Seed non-default
        // source values directly; selector ownership is covered against the real catalogue by
        // `runtime_selectors_and_sliders_preserve_the_real_appearance_owners` above.
        state
            .create_draft
            .set_customization(caer_protocol::customization::Customization {
                face_type: 1,
                hair_style: 1,
                mood_type: 1,
                ..state.create_draft.customization()
            });
        state.create_draft.custom_mode = 1;
        let face = state.create_draft.face_type;
        let hair = state.create_draft.hair_style;
        // The stub arrives with its 30 points pre-allocated; start the stats screen from bases
        // so the arrows are exercised against a real pool, then hand-spend exactly 30.
        state.create_draft.reset_stats_to_race_bases();
        let r =
            dispatch_preworld_action(&mut state, PreWorldAction::StatsAdjust { stat: 0, dir: 1 });
        assert!(
            r.refused.is_none(),
            "first point is affordable: {:?}",
            r.refused
        );
        // ...and taken back off, so only the manual spend below counts.
        let r =
            dispatch_preworld_action(&mut state, PreWorldAction::StatsAdjust { stat: 0, dir: -1 });
        assert!(r.refused.is_none());
        state.create_draft.allocate_default_bonus();
        assert!(caer_protocol::starting_stats::validate_distribution(
            state.create_draft.race,
            &state.create_draft.stats
        )
        .is_ok());

        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(r.refused.is_none(), "{:?}", r.refused);
        match r.commands.as_slice() {
            [LiveCommand::CreateCharacter { draft }] => {
                assert_eq!(draft.slot, 1, "first free slot must skip 0 and 2");
                assert_eq!(draft.name, "FreeSlot");
                assert_eq!(draft.gender, 1);
                assert_eq!(draft.race, race_before);
                assert_eq!(draft.class_id, class_before);
                assert_eq!(
                    draft.custom_mode, 1,
                    "appearance must be flagged for storage"
                );
                assert_eq!(draft.face_type, face);
                assert_eq!(draft.hair_style, hair);
                assert!(
                    caer_protocol::starting_stats::validate_distribution(draft.race, &draft.stats)
                        .is_ok(),
                    "the wire draft must satisfy the oracle's own rule"
                );
            }
            other => panic!("expected exactly one CreateCharacter, got {other:?}"),
        }
    }

    /// **E6 acceptance criterion 2 — the red control.** Continué must not mint. The known-bad
    /// shape is the exact arm this file shipped before E6: a direct `CreateCharacter` dispatch
    /// on Continué. Run through the same product state, it must reproduce the defect (a packet
    /// leaves on a mere screen advance), and the shipping arm must not.
    #[test]
    fn continue_opens_customization_and_never_mints_on_sight() {
        let mut state = state_with(overview_with(vec![]));
        apply_create_name_input(&mut state, "MintedOnSight");

        // KNOWN-BAD: the historical Continué arm. It must be capable of firing the packet —
        // otherwise this control proves nothing.
        let historical = |state: &mut PreWorldProductState| {
            state.create_draft.slot = 0;
            PreWorldProductResult::cmds(
                vec![LiveCommand::CreateCharacter {
                    draft: state.create_draft.clone(),
                }],
                true,
            )
        };
        let bad = historical(&mut state);
        assert!(
            matches!(
                bad.commands.as_slice(),
                [LiveCommand::CreateCharacter { .. }]
            ),
            "the known-bad arm must reproduce the mint-on-sight defect"
        );

        // Shipping behaviour: Continué sends nothing, however complete the draft.
        let mut fresh = state_with(overview_with(vec![]));
        apply_create_name_input(&mut fresh, "MintedOnSight");
        let r = dispatch_preworld_action(&mut fresh, PreWorldAction::CharCreateContinue);
        assert!(r.commands.is_empty(), "Continué must not send anything");
        assert!(r.refused.is_none());
        assert!(r.apply_hud_navigation);
    }

    /// **E6 acceptance criterion 3.** Back-navigation through the chain never double-creates:
    /// walking forward and back any number of times sends nothing, Continue sends one packet,
    /// and a second Continue refuses rather than re-encoding.
    #[test]
    fn back_navigation_through_the_chain_never_double_creates() {
        let mut state = state_with(overview_with(vec![]));
        apply_create_name_input(&mut state, "OnceOnly");

        // Forward and back, twice, across both seams.
        for _ in 0..2 {
            drive_to_stats(&mut state);
            let back = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeBack);
            assert!(back.commands.is_empty());
            let back = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeBack);
            assert!(
                back.commands.is_empty(),
                "back to the create form sends nothing"
            );
        }

        let r = confirm_create(&mut state);
        assert!(r.refused.is_none(), "{:?}", r.refused);
        assert_eq!(r.commands.len(), 1, "exactly one packet for the whole walk");

        // The second Continue is a refusal, never a second packet.
        let again = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(again.commands.is_empty(), "a second Continue re-encoded");
        assert!(again.refused.is_some());
    }

    /// Control 1021 in the retail form is labeled `Optimize`, not `Accept`. It is a purely local
    /// default allocation: the Celt/Mentalist capture visibly carries +10 STR, CON and DEX and
    /// no extra packet. This protects both the label/meaning boundary and the observed default.
    #[test]
    fn stats_optimize_is_local_and_restores_the_observed_default_allocation() {
        let mut state = state_with(overview_with(vec![]));
        state.create_draft.set_race(9); // Celt, the captured Hibernia reference.
        state.create_draft.reset_stats_to_race_bases();
        let _ =
            dispatch_preworld_action(&mut state, PreWorldAction::StatsAdjust { stat: 0, dir: 1 });
        let r = dispatch_preworld_action(&mut state, PreWorldAction::StatsOptimize);
        assert!(r.refused.is_none(), "Optimize refused: {:?}", r.refused);
        assert!(
            r.commands.is_empty(),
            "Optimize must not create a character"
        );
        assert!(
            !r.apply_hud_navigation,
            "Optimize must not leave the stats modal"
        );
        let mut expected = [0u8; 8];
        for (i, stat) in expected.iter_mut().enumerate() {
            *stat = caer_protocol::starting_stats::race_base(9, i) + if i < 3 { 10 } else { 0 };
        }
        assert_eq!(state.create_draft.stats, expected);
        assert!(caer_protocol::starting_stats::validate_distribution(9, &expected).is_ok());
    }

    /// The stats arrows enforce the DOLSharp rules live: race bases are floors, the pool is
    /// exactly 30 with escalating costs, and the reset returns to all bases.
    #[test]
    fn stats_arrows_enforce_the_oracle_allocation_rules() {
        let mut state = state_with(overview_with(vec![]));
        state.create_draft.reset_stats_to_race_bases();
        let base_str = state.create_draft.stats[0];

        // Below base refused.
        let r =
            dispatch_preworld_action(&mut state, PreWorldAction::StatsAdjust { stat: 0, dir: -1 });
        assert_eq!(
            r.refused.as_deref(),
            Some("Your base statistics cannot be lowered.")
        );

        // Spend the whole pool one point at a time: 30 clicks succeed, the 31st refuses.
        let mut clicks = 0;
        loop {
            let r = dispatch_preworld_action(
                &mut state,
                PreWorldAction::StatsAdjust { stat: 0, dir: 1 },
            );
            if r.refused.is_some() {
                break;
            }
            clicks += 1;
            assert!(clicks < 100, "runaway spend");
        }
        // 10 points at cost 1 + 5 at cost 2 + beyond at cost 3: 30 points buy 18 above base
        // (10x1 + 5x2 + 3x3 = 31 > 30, so 17 above base = 10+10+6... recompute: cost(17)=23,
        // cost(18)=29, cost(19)=32). The last affordable click is the 18th; the 19th refuses.
        assert_eq!(clicks, 18, "escalated costs must cap a single stat's rise");
        assert_eq!(
            caer_protocol::starting_stats::total_spent(
                state.create_draft.race,
                &state.create_draft.stats
            ),
            29
        );
        assert_eq!(state.create_draft.stats[0], base_str + 18);

        // Reset returns to bases; the pool is whole again.
        dispatch_preworld_action(&mut state, PreWorldAction::StatsReset);
        assert_eq!(state.create_draft.stats[0], base_str);
        assert_eq!(
            caer_protocol::starting_stats::total_spent(
                state.create_draft.race,
                &state.create_draft.stats
            ),
            0
        );
    }

    #[test]
    fn apply_create_name_input_delegates_to_shared_append() {
        let mut state = PreWorldProductState::default();
        apply_create_name_input(&mut state, "Hi-There!!");
        assert_eq!(state.create_draft.name, "HiThere");
        assert!(state.name_edit_focus);
    }

    #[test]
    fn customize_continue_refuses_illegal_albion_combo() {
        let mut state = state_with(overview_with(vec![]));
        apply_create_name_input(&mut state, "Illegal");
        state.create_realm = 1;
        state.create_draft.realm = 1;
        // Highlander (3) is not in Theurgist's (5) EligibleRaces.
        state.create_draft.set_race(3);
        state.create_draft.class_id = 5;
        state.create_draft.reset_stats_to_race_bases();
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(r.commands.is_empty());
        assert!(r.refused.is_some());
    }

    /// B3 regression: the renderer's deleted Albion table refused HalfOgre Armsmen. The oracle
    /// (`CharacterClassDB.cs` Armsman `EligibleRaces` = Korazh, Avalonian, Briton, **HalfOgre**,
    /// Highlander, Inconnu, Saracen) allows it, and the old test asserted the wrong side. A
    /// legal combination must now reach the wire.
    #[test]
    fn customize_continue_accepts_half_ogre_armsman_per_the_oracle() {
        let mut state = state_with(overview_with(vec![]));
        apply_create_name_input(&mut state, "Ogrewall");
        state.create_realm = 1;
        state.create_draft.realm = 1;
        state.create_draft.set_race(16); // HalfOgre
        state.create_draft.class_id = 2; // Armsman
        state.create_draft.reset_stats_to_race_bases();
        state.create_draft.allocate_default_bonus();
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(
            r.refused.is_none(),
            "refused a legal combination: {:?}",
            r.refused
        );
        assert!(
            r.commands
                .iter()
                .any(|c| matches!(c, LiveCommand::CreateCharacter { .. })),
            "legal HalfOgre Armsman must dispatch CreateCharacter"
        );
    }

    /// Midgard and Hibernia illegal tuples used to dispatch unchecked because the only guard was
    /// Albion-shaped. Every realm goes through `classify` now.
    #[test]
    fn customize_continue_refuses_illegal_non_albion_combos() {
        for (realm, race, class_id, what) in [
            (2u8, 5u8, 21u8, "Norseman Thane is legal"),
            (3u8, 9u8, 44u8, "Celt Hero is legal"),
        ] {
            let mut state = state_with(overview_with(vec![]));
            apply_create_name_input(&mut state, "Legalone");
            state.create_realm = realm;
            state.create_draft.realm = realm;
            state.create_draft.set_race(race);
            state.create_draft.class_id = class_id;
            state.create_draft.reset_stats_to_race_bases();
            state.create_draft.allocate_default_bonus();
            let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
            assert!(
                r.refused.is_none(),
                "{what}, but was refused: {:?}",
                r.refused
            );
        }
        // Cross-realm tuple: an Albion race with a Midgard class must never dispatch.
        let mut state = state_with(overview_with(vec![]));
        apply_create_name_input(&mut state, "Crossrealm");
        state.create_realm = 2;
        state.create_draft.realm = 2;
        state.create_draft.set_race(1); // Briton
        state.create_draft.class_id = 21; // Thane (Midgard)
        state.create_draft.reset_stats_to_race_bases();
        state.create_draft.allocate_default_bonus();
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
        assert!(r.commands.is_empty(), "cross-realm tuple reached the wire");
        assert!(r.refused.is_some());
    }

    #[test]
    fn select_occupied_slot_emits_overview_slot_not_zero() {
        let mut state = PreWorldProductState::default();
        let summary = occupied(3, "Lilillyn");
        let r = dispatch_select_character(&mut state, &summary);
        match r.commands.as_slice() {
            [LiveCommand::SelectCharacter { slot }] => assert_eq!(*slot, 3),
            other => panic!("expected SelectCharacter slot 3, got {other:?}"),
        }
        assert!(state.ui_select_sent);
    }

    /// H4: selection is a protocol slot, so the first rendered row carries no special meaning.
    ///
    /// This previously stored the row index and asserted row 0 resolved to slot 3. Selection is now
    /// the slot itself, which makes that mapping unnecessary — and makes selecting a slot that is
    /// not present a refusal rather than an accidental hit on whatever sits in that row.
    #[test]
    fn enter_world_dispatches_the_selected_protocol_slot() {
        let mut state = state_with(overview_with(vec![
            occupied(3, "Lilillyn"),
            occupied(8, "Second"),
        ]));
        state.selected_protocol_slot = Some(3);
        let r = dispatch_preworld_action(&mut state, PreWorldAction::EnterWorld);
        match r.commands.as_slice() {
            [LiveCommand::SelectCharacter { slot }] => assert_eq!(*slot, 3),
            other => panic!("expected SelectCharacter slot 3, got {other:?}"),
        }
        assert!(r.refused.is_none());
        assert!(state.ui_select_sent);

        // Slot 0 is absent from this overview even though row 0 exists — the distinction the old
        // row-indexed model could not express.
        let mut state = state_with(overview_with(vec![
            occupied(3, "Lilillyn"),
            occupied(8, "Second"),
        ]));
        state.selected_protocol_slot = Some(0);
        let r = dispatch_preworld_action(&mut state, PreWorldAction::EnterWorld);
        assert!(
            r.commands.is_empty(),
            "absent slot 0 must not resolve to row 0"
        );
        assert!(r.refused.is_some());
    }

    #[test]
    fn enter_world_refuses_without_a_local_selection() {
        let mut state = state_with(overview_with(vec![occupied(3, "Lilillyn")]));
        let r = dispatch_preworld_action(&mut state, PreWorldAction::EnterWorld);
        assert!(r.commands.is_empty());
        assert!(r
            .refused
            .as_deref()
            .is_some_and(|message| message.contains("no selected slot")));
        assert!(!state.ui_select_sent);
    }

    #[test]
    fn login_exit_emits_quit() {
        let mut state = PreWorldProductState::default();
        let r = dispatch_preworld_action(&mut state, PreWorldAction::LoginExit);
        assert!(matches!(r.commands.as_slice(), [LiveCommand::Quit]));
    }

    #[test]
    fn removing_overview_send_would_break_realm_test() {
        // Named falsifier: the realm path must include RequestCharacterOverview specifically.
        let mut state = PreWorldProductState::default();
        let r = dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(2));
        assert!(
            r.commands
                .iter()
                .any(|c| matches!(c, LiveCommand::RequestCharacterOverview { realm: 2 })),
            "deleting overview send must make this red"
        );
    }
}

#[cfg(test)]
mod h4_selection_identity_tests {
    use super::tests::{occupied as ch, overview_with as ov, state_with};
    use super::*;

    /// **H4 falsifier.** Slots [0,3,7]; select the middle character; slot 0 is then removed
    /// elsewhere so the compact list shifts. Play must still enter as the character that was
    /// clicked, or refuse — never as whoever now occupies that row.
    #[test]
    fn overview_reorder_cannot_retarget_the_selected_character() {
        let mut state = state_with(ov(vec![ch(0, "Aldric"), ch(3, "Bryn"), ch(7, "Cass")]));

        // Click row 1 → Bryn, protocol slot 3.
        state.selected_protocol_slot = Some(3);

        // Refresh drops Aldric. Row 1 is now Cass; slot 3 is still Bryn.
        state.overview = Some(ov(vec![ch(3, "Bryn"), ch(7, "Cass")]));

        let r = dispatch_preworld_action(&mut state, PreWorldAction::EnterWorld);
        assert!(r.refused.is_none(), "{:?}", r.refused);
        match r.commands.as_slice() {
            [LiveCommand::SelectCharacter { slot }] => assert_eq!(
                *slot, 3,
                "Play entered as the wrong character after an overview reorder"
            ),
            other => panic!("expected SelectCharacter slot 3, got {other:?}"),
        }
    }

    /// Proves the test above is not vacuous: a row index would have picked the wrong character in
    /// exactly that scenario. This is the defect, reproduced against the old lookup rule.
    #[test]
    fn a_row_index_would_have_retargeted_in_that_same_scenario() {
        let before = ov(vec![ch(0, "Aldric"), ch(3, "Bryn"), ch(7, "Cass")]);
        let after = ov(vec![ch(3, "Bryn"), ch(7, "Cass")]);
        let row = 1usize;
        assert_eq!(before.characters[row].name, "Bryn", "row 1 was Bryn");
        assert_eq!(
            after.characters[row].name, "Cass",
            "row 1 became Cass — this is precisely the silent retarget H4 describes"
        );
        // And the old length-only guard would not have cleared it.
        assert!(
            row < after.characters.len(),
            "old guard only fired past the end"
        );
    }

    /// If the selected character is gone entirely, refuse rather than fall back to a neighbour.
    #[test]
    fn deleted_selection_refuses_instead_of_falling_back() {
        let mut state = state_with(ov(vec![ch(0, "Aldric"), ch(3, "Bryn")]));
        state.selected_protocol_slot = Some(3);
        state.overview = Some(ov(vec![ch(0, "Aldric")]));
        let r = dispatch_preworld_action(&mut state, PreWorldAction::EnterWorld);
        assert!(
            r.commands.is_empty(),
            "a removed character must not enter the world"
        );
        assert!(r.refused.is_some());
    }

    /// Slot 0 is a real slot, not "unset" — an occupied slot 0 must be enterable.
    #[test]
    fn slot_zero_is_a_valid_selection() {
        let mut state = state_with(ov(vec![ch(0, "Aldric"), ch(3, "Bryn")]));
        state.selected_protocol_slot = Some(0);
        let r = dispatch_preworld_action(&mut state, PreWorldAction::EnterWorld);
        assert!(r.refused.is_none(), "{:?}", r.refused);
        match r.commands.as_slice() {
            [LiveCommand::SelectCharacter { slot }] => assert_eq!(*slot, 0),
            other => panic!("expected SelectCharacter slot 0, got {other:?}"),
        }
    }

    /// Going back to realm select must not leave the create form describing a realm the product
    /// has stopped believing in.
    ///
    /// Two stores hold "which realm am I creating in": `create_realm` here, and
    /// `create_draft.realm`, which is what the HUD draws the race and class buttons from.
    /// `BackToRealm` cleared the first and left the second, so the screen kept Hibernia's labels
    /// while every click resolved through Albion's table — Matt clicked Lurikeen and got Saracen,
    /// because they are the same slot in their two realms.
    ///
    /// The comment on `ChooseRealm` already records this defect class for the overview packet
    /// ("what put two Albion Armsmen on the Hibernia character screen"). It was fixed there and
    /// not here.
    #[test]
    fn going_back_to_realm_select_does_not_leave_a_stale_create_realm() {
        let mut state = PreWorldProductState::default();
        dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(3));
        dispatch_preworld_action(&mut state, PreWorldAction::OpenCharCreate);
        assert_eq!(
            state.create_draft.realm, 3,
            "create opens in the chosen realm"
        );

        dispatch_preworld_action(&mut state, PreWorldAction::BackToRealm);
        assert_eq!(
            state.create_draft.realm,
            state.create_realm,
            "the two realm stores diverged: the HUD renders {} while a click resolves through {}",
            state.create_draft.realm,
            state.create_realm.max(1)
        );

        // The player-visible consequence, pinned on the round trip the player actually makes:
        // create -> Realm -> pick Hibernia again -> the Lurikeen button must produce a Lurikeen.
        dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(3));
        dispatch_preworld_action(&mut state, PreWorldAction::OpenCharCreate);
        let lurikeen = caer_protocol::creation_adapters::race_adapters(state.create_draft.realm)
            .into_iter()
            .find(|a| a.race_id == 12)
            .expect("Lurikeen is on Hibernia's race panel")
            .slot;
        dispatch_preworld_action(&mut state, PreWorldAction::CharCreateRace(lurikeen));
        assert_eq!(
            state.create_draft.race, 12,
            "clicking the Lurikeen button produced race {} — a different realm's race in the \
             same slot",
            state.create_draft.race
        );
    }

    #[test]
    fn male_celt_bainshee_click_is_refused() {
        let mut state = PreWorldProductState::default();
        dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(3));
        dispatch_preworld_action(&mut state, PreWorldAction::OpenCharCreate);
        let celt = caer_protocol::creation_adapters::race_adapters(3)
            .into_iter()
            .find(|a| a.race_id == 9)
            .expect("Celt")
            .slot;
        dispatch_preworld_action(&mut state, PreWorldAction::CharCreateRace(celt));
        dispatch_preworld_action(&mut state, PreWorldAction::CharCreateGender(0));
        let bainshee = caer_protocol::creation_adapters::class_adapters(3, 9)
            .into_iter()
            .find(|a| a.class_id == 39)
            .expect("Bainshee slot")
            .slot;
        let before = state.create_draft.class_id;
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateClass(bainshee));
        assert!(r.refused.is_some(), "male Bainshee must refuse");
        assert_eq!(state.create_draft.class_id, before);
        assert_eq!(state.create_draft.gender, 0);
    }

    #[test]
    fn minotaur_female_click_is_refused() {
        let mut state = PreWorldProductState::default();
        dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(1));
        dispatch_preworld_action(&mut state, PreWorldAction::OpenCharCreate);
        let mino = caer_protocol::creation_adapters::race_adapters(1)
            .into_iter()
            .find(|a| a.race_id == 19)
            .expect("Korazh")
            .slot;
        dispatch_preworld_action(&mut state, PreWorldAction::CharCreateRace(mino));
        let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateGender(1));
        assert!(r.refused.is_some(), "Minotaur Female must refuse");
        assert_eq!(state.create_draft.gender, 0);
    }
}
