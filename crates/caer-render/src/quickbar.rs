//! The quickbar — ability/spell slots with number keybinds (C.2).
//!
//! Slots are filled from the player's real skill list ([`caer_protocol::skills`], decoded from
//! `VariousUpdate` 0x16 subcode 0x01), so this shows what the character can actually do rather
//! than placeholder art.
//!
//! ## Addressing: the list order IS the protocol
//! Both halves are now verified against a capture of the reference client (115 `UseSkill` samples).
//! `UseSkillHandler` reads position + heading + an **INDEX into the usable-skill list** and that
//! entry's `eSkillPage` type — NOT an internal id, which is what this used to send and the server
//! never reads.
//!
//! So the order those `VariousUpdate` pages arrive in is the addressing scheme, and a slot must
//! remember where its skill sat in the FULL list. `autofill` therefore enumerates before filtering:
//! dropping specialisations first would renumber everything after them and fire the wrong ability.
//!
//! The server still sends no acknowledgement for a use request, so a slot flash means "sent".
//!
//! ## Input
//! Plain Digit1–0 activate slots **before** the 74-action `GameAction` table
//! (`Client::use_quickbar_slot` → [`Quickbar::press`]). Ctrl+Digit1–3 stay weapon-switch chords
//! in that table. Numpad is not mirrored here (Numpad8/2 are `Forward2`/`Back2`). While chat
//! (or name-edit) owns the keyboard, digits type into the line and do **not** fire the bar —
//! same as movement keys.

use caer_protocol::skills::{Skill, SkillKind};
use winit::keyboard::KeyCode;

/// Slots on the bar. DAoC's bar is ten, keyed 1–9 then 0.
pub const SLOTS: usize = 10;

/// Vertical band the quickbar occupies along the bottom edge, including its margin.
///
/// Shared so the combat log and chat input stack ABOVE the bar instead of behind it — the bar is
/// centred and wide enough to cover a bottom-left log, which is exactly what happened the first
/// time these were drawn together.
pub const BAND_HEIGHT: f32 = 70.0;

/// What sits in one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub name: String,
    pub kind: SkillKind,
    pub internal_id: u16,
    pub level: u8,
    /// Position in the server's usable-skill list. **This is what `UseSkill` addresses**, not the
    /// internal id — so the list order the server sent is the addressing scheme, and a slot has to
    /// remember where its skill sat.
    pub index: u8,
    /// The `eSkillPage` byte for this entry, sent alongside the index.
    pub skill_type: u8,
}

/// The bar's contents plus transient press feedback.
#[derive(Debug, Default)]
pub struct Quickbar {
    slots: Vec<Option<Slot>>,
    /// Slot index flashed this frame, and how long the flash has left (seconds).
    flash: Option<(usize, f32)>,
}

/// How long a slot stays highlighted after being triggered.
const FLASH_TIME: f32 = 0.25;

impl Quickbar {
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: vec![None; SLOTS],
            flash: None,
        }
    }

    /// Fill empty slots from the player's skill list, in list order.
    ///
    /// Only *actionable* skills are placed: specialisations are ratings ("Slash 37"), not buttons,
    /// and a slot that can never fire is a bug the player has to discover by pressing it.
    ///
    /// Existing slots are left alone so a re-sent skill list (level up, respec) doesn't rearrange
    /// a bar the player has gotten used to.
    pub fn autofill(&mut self, skills: &[Skill]) {
        if self.slots.len() != SLOTS {
            self.slots.resize(SLOTS, None);
        }
        // Enumerate BEFORE filtering: the index the server addresses is the position in the full
        // list it sent, so filtering first would renumber every skill and fire the wrong one.
        let mut usable = skills
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind.is_usable() && !s.name.is_empty());
        for slot in &mut self.slots {
            if slot.is_some() {
                continue;
            }
            let Some((idx, s)) = usable.next() else { break };
            *slot = Some(Slot {
                name: s.name.clone(),
                kind: s.kind,
                internal_id: s.internal_id,
                level: s.level,
                index: idx.min(u8::MAX as usize) as u8,
                skill_type: skill_type_byte(s.kind),
            });
        }
    }

    /// The skill in a slot, if any.
    #[must_use]
    pub fn slot(&self, index: usize) -> Option<&Slot> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    #[must_use]
    pub fn filled(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Trigger a slot. Returns the skill to send, and flashes the slot.
    ///
    /// Returns `None` for an empty slot rather than flashing it — pressing an empty key should be
    /// silent, not look like it did something.
    pub fn press(&mut self, index: usize) -> Option<Slot> {
        let slot = self.slots.get(index).and_then(Option::as_ref).cloned()?;
        self.flash = Some((index, FLASH_TIME));
        Some(slot)
    }

    /// Age the press feedback.
    pub fn tick(&mut self, dt: f32) {
        if let Some((i, t)) = self.flash {
            let left = t - dt;
            self.flash = if left > 0.0 { Some((i, left)) } else { None };
        }
    }

    /// Map a physical number key to a slot: 1–9 then 0 for the tenth.
    #[must_use]
    pub fn slot_for_digit(digit: u32) -> Option<usize> {
        match digit {
            1..=9 => Some(digit as usize - 1),
            0 => Some(9),
            _ => None,
        }
    }

    /// Digit label for a slot index (0 → key `1`, …, 9 → key `0`).
    #[must_use]
    pub fn digit_for_slot(index: usize) -> Option<u32> {
        match index {
            0..=8 => Some((index + 1) as u32),
            9 => Some(0),
            _ => None,
        }
    }
}

/// Map a physical Digit key to the quickbar digit (1–9, 0).
///
/// Used on the product input path *before* the `GameAction` table lookup so plain number keys fire
/// the bar without expanding the frozen 74-action table. Modifier chords (e.g. Ctrl+Digit1) stay
/// with bindings. Numpad is intentionally omitted: Numpad8/Numpad2 are primary chords for
/// `Forward2` / `Back2` in the 74-action table.
#[must_use]
pub fn digit_for_key(code: KeyCode) -> Option<u32> {
    match code {
        KeyCode::Digit1 => Some(1),
        KeyCode::Digit2 => Some(2),
        KeyCode::Digit3 => Some(3),
        KeyCode::Digit4 => Some(4),
        KeyCode::Digit5 => Some(5),
        KeyCode::Digit6 => Some(6),
        KeyCode::Digit7 => Some(7),
        KeyCode::Digit8 => Some(8),
        KeyCode::Digit9 => Some(9),
        KeyCode::Digit0 => Some(0),
        _ => None,
    }
}

/// The `eSkillPage` byte the server expects alongside a skill index.
fn skill_type_byte(kind: SkillKind) -> u8 {
    match kind {
        SkillKind::Specialization => 0,
        SkillKind::Ability => 1,
        SkillKind::Style => 2,
        SkillKind::Spell => 3,
        SkillKind::Song => 4,
        SkillKind::AbilitySpell => 5,
        SkillKind::RealmAbility => 6,
        SkillKind::Unknown(v) => v,
    }
}

/// Short tag shown on a slot so spells and styles are tellable apart at a glance.
fn kind_tag(kind: SkillKind) -> &'static str {
    match kind {
        SkillKind::Spell => "spell",
        SkillKind::Style => "style",
        SkillKind::Ability => "abil",
        SkillKind::Song => "song",
        SkillKind::RealmAbility => "RA",
        SkillKind::AbilitySpell => "abil",
        SkillKind::Specialization | SkillKind::Unknown(_) => "",
    }
}

/// Draw the bar, centred along the bottom.
///
/// Returns the slot index the player clicked this frame, if any. Empty slots still accept clicks
/// so the client can no-op via [`Quickbar::press`] rather than looking broken.
pub fn draw(root: &mut egui::Ui, bar: &Quickbar) -> Option<usize> {
    if bar.filled() == 0 {
        return None; // nothing known yet — an empty bar is just clutter
    }
    let ctx = root.ctx().clone();
    let mut clicked = None;
    egui::Area::new(egui::Id::new("quickbar"))
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -12.0])
        .show(&ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_premultiplied(10, 10, 12, 200))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(12, 11, 10)))
                .corner_radius(3)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for i in 0..SLOTS {
                            if draw_slot(ui, bar, i) {
                                clicked = Some(i);
                            }
                        }
                    });
                });
        });
    clicked
}

fn draw_slot(ui: &mut egui::Ui, bar: &Quickbar, index: usize) -> bool {
    const W: f32 = 74.0;
    const H: f32 = 46.0;

    let (rect, response) = ui.allocate_exact_size(egui::vec2(W, H), egui::Sense::click());
    let painter = ui.painter();

    let pressed = matches!(bar.flash, Some((i, _)) if i == index);
    let fill = if pressed {
        egui::Color32::from_rgb(70, 64, 40)
    } else {
        egui::Color32::from_rgb(26, 25, 28)
    };
    painter.rect_filled(rect, 2, fill);
    painter.rect_stroke(
        rect,
        2,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(64, 62, 60)),
        egui::StrokeKind::Inside,
    );

    // Keybind digit, top-left: 1–9 then 0.
    let digit = Quickbar::digit_for_slot(index).unwrap_or(0);
    painter.text(
        rect.left_top() + egui::vec2(4.0, 2.0),
        egui::Align2::LEFT_TOP,
        digit.to_string(),
        egui::FontId::proportional(10.0),
        egui::Color32::from_rgb(150, 148, 146),
    );

    if let Some(slot) = bar.slot(index) {
        // Name, wrapped to the slot width — ability names are long ("Weaponry: Slashing").
        painter.text(
            rect.center() + egui::vec2(0.0, 2.0),
            egui::Align2::CENTER_CENTER,
            truncate(&slot.name, 12),
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgb(232, 228, 220),
        );

        let tag = kind_tag(slot.kind);
        if !tag.is_empty() {
            painter.text(
                rect.right_bottom() + egui::vec2(-4.0, -2.0),
                egui::Align2::RIGHT_BOTTOM,
                tag,
                egui::FontId::proportional(9.0),
                egui::Color32::from_rgb(140, 152, 170),
            );
        }
    }

    response.clicked()
}

/// Clip a long skill name to fit a slot, with an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sk(name: &str, kind: SkillKind) -> Skill {
        Skill {
            level: 5,
            internal_id: 42,
            kind,
            bonus: 0,
            icon: 0,
            name: name.into(),
        }
    }

    /// Autofill takes actionable skills only. Specialisations are ratings, not buttons — a slot
    /// bound to "Slash" the *rating* could never fire, and the player would only find out by
    /// pressing it.
    #[test]
    fn autofill_skips_specialisations() {
        // Mirrors a real Paladin list: specs first, then abilities.
        let skills = vec![
            sk("Slash", SkillKind::Specialization),
            sk("Crush", SkillKind::Specialization),
            sk("Sprint", SkillKind::Ability),
            sk("Slam", SkillKind::Style),
            sk("Heal", SkillKind::Spell),
        ];
        let mut bar = Quickbar::new();
        bar.autofill(&skills);

        assert_eq!(
            bar.filled(),
            3,
            "only the three actionable skills belong on the bar"
        );
        assert_eq!(bar.slot(0).unwrap().name, "Sprint");
        assert_eq!(bar.slot(1).unwrap().name, "Slam");
        assert_eq!(bar.slot(2).unwrap().name, "Heal");
    }

    /// A re-sent skill list must not rearrange a bar the player is used to.
    #[test]
    fn autofill_leaves_existing_slots_alone() {
        let mut bar = Quickbar::new();
        bar.autofill(&[sk("Sprint", SkillKind::Ability)]);
        assert_eq!(bar.slot(0).unwrap().name, "Sprint");

        // The server re-sends with a new skill first (level up).
        bar.autofill(&[
            sk("Charge", SkillKind::Ability),
            sk("Sprint", SkillKind::Ability),
        ]);
        assert_eq!(
            bar.slot(0).unwrap().name,
            "Sprint",
            "slot 0 must not be rewritten"
        );
        assert_eq!(
            bar.slot(1).unwrap().name,
            "Charge",
            "the new skill fills the next empty slot"
        );
    }

    /// Pressing an empty slot is silent; pressing a filled one returns the skill and flashes.
    #[test]
    fn pressing_an_empty_slot_does_nothing() {
        let mut bar = Quickbar::new();
        bar.autofill(&[sk("Sprint", SkillKind::Ability)]);

        assert!(
            bar.press(5).is_none(),
            "an empty slot must not report a press"
        );
        assert!(bar.flash.is_none(), "…and must not flash");

        let s = bar.press(0).expect("filled slot should press");
        assert_eq!(s.name, "Sprint");
        assert!(bar.flash.is_some());
    }

    /// The flash expires so a slot doesn't stay lit forever.
    #[test]
    fn flash_expires() {
        let mut bar = Quickbar::new();
        bar.autofill(&[sk("Sprint", SkillKind::Ability)]);
        bar.press(0);
        bar.tick(FLASH_TIME * 0.5);
        assert!(bar.flash.is_some());
        bar.tick(FLASH_TIME);
        assert!(bar.flash.is_none());
    }

    /// Keybinds are 1–9 then 0 for the tenth slot, matching DAoC.
    #[test]
    fn digit_keys_map_one_through_zero() {
        assert_eq!(Quickbar::slot_for_digit(1), Some(0));
        assert_eq!(Quickbar::slot_for_digit(9), Some(8));
        assert_eq!(
            Quickbar::slot_for_digit(0),
            Some(9),
            "0 is the TENTH slot, not the first"
        );
        assert_eq!(Quickbar::slot_for_digit(11), None);
        assert_eq!(Quickbar::digit_for_slot(0), Some(1));
        assert_eq!(Quickbar::digit_for_slot(9), Some(0));
        assert_eq!(digit_for_key(KeyCode::Digit1), Some(1));
        assert_eq!(digit_for_key(KeyCode::Digit0), Some(0));
        assert_eq!(
            digit_for_key(KeyCode::Numpad1),
            None,
            "numpad is reserved for Forward2/Back2 primaries"
        );
        for d in 0u32..=9 {
            let slot = Quickbar::slot_for_digit(d).unwrap();
            assert_eq!(Quickbar::digit_for_slot(slot), Some(d));
        }
    }

    /// Autofill must never overflow the bar, however many skills arrive.
    #[test]
    fn autofill_is_bounded_by_the_slot_count() {
        let many: Vec<Skill> = (0..200)
            .map(|i| sk(&format!("s{i}"), SkillKind::Spell))
            .collect();
        let mut bar = Quickbar::new();
        bar.autofill(&many);
        assert_eq!(bar.filled(), SLOTS);
        assert!(bar.slot(SLOTS).is_none());
    }

    #[test]
    fn long_names_are_truncated_to_fit() {
        assert_eq!(truncate("Sprint", 12), "Sprint");
        let t = truncate("Weaponry: Slashing", 12);
        assert!(t.chars().count() <= 12, "got {t:?}");
        assert!(t.ends_with('…'));
    }
}
