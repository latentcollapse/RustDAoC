//! Combat log + floating combat text (C.7).
//!
//! ## Why this is not blocked on the B.2 capture
//! B.2 (melee attack) is blocked because the *request* code `0x74` is unverified and no structured
//! combat packets are decoded. It is easy to conclude from that that combat text is blocked too.
//! It isn't: the server narrates every combat result as **ordinary system text** on `0xAF`, which
//! this client has decoded since P1. Surveyed across the captures:
//!
//! ```text
//! type 0x11  "You attack Ire Fairy with your sword and hit for 125 (+129) damage!"
//! type 0x1d  "The giant skeleton hits your leg for 10 (-8) damage!"
//! type 0x1a  "The giant skeleton drops a bag of coins."
//! ```
//!
//! So the damage numbers are already on the wire in a form we can read. What we genuinely cannot do
//! without the capture is anything needing *structured* combat data — attributing a hit to a
//! specific object id, swing animations, or knowing a result arrived at all when no text is sent.
//! Numbers here are therefore attributed by DIRECTION (ours vs theirs), not by entity id, which is
//! why [`Damage`] carries no object id.
//!
//! ## Message classification
//! The `chat_type` byte on 0xAF sorts the stream. Types below were observed with counts in the
//! captures; anything unrecognised falls back to [`MessageKind::System`] rather than being dropped.
//! Type `0x64` is deliberately EXCLUDED from the log: it carries structured client directives
//! (`"I,1,0,0,0,0,2081,…"`), not prose, and printing it would spray comma-separated noise at the
//! player.

use std::collections::VecDeque;

/// What a system message is about, derived from the 0xAF `chat_type` byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    /// Attacks, damage, combat mode (0x11 ours, 0x1d incoming).
    Combat,
    /// Spellcasting and effects landing (0x10, 0x13, 0x19, 0x1f).
    Spell,
    /// Player speech and location banners (0x01).
    Chat,
    /// Loot drops and merchant transactions (0x1a, 0x14).
    Loot,
    /// Targeting/examine feedback (0x1e).
    Target,
    /// Everything else, including broadcasts and timers.
    System,
}

/// Structured client directives rather than text meant for a human (0xAF type 0x64).
///
/// Kept as a named constant because the interesting property is that it must NOT be logged; a bare
/// `0x64` in a match arm invites someone to "fix" the gap later.
pub const CHAT_TYPE_CLIENT_DIRECTIVE: u8 = 0x64;

/// Classify a 0xAF message by its type byte.
#[must_use]
pub fn classify(chat_type: u8) -> MessageKind {
    match chat_type {
        0x11 | 0x1d => MessageKind::Combat,
        0x10 | 0x13 | 0x19 | 0x1f => MessageKind::Spell,
        0x01 => MessageKind::Chat,
        0x14 | 0x1a => MessageKind::Loot,
        0x1e => MessageKind::Target,
        _ => MessageKind::System,
    }
}

/// Where a floating combat value came from (REQ-021).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatProvenance {
    /// Structured CombatAnimation 0xBC — the only provenance SCN-05 accepts for combat feedback.
    CombatAnimation,
    /// Scraped from Message 0xAF chat text. Pixel-identical numbers are a fake for SCN-05.
    ChatMessage,
}

impl CombatProvenance {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CombatAnimation => caer_protocol::combat_anim::PROVENANCE,
            Self::ChatMessage => "Message 0xAF",
        }
    }
}

/// A parsed damage result, for floating combat text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Damage {
    /// The headline number — damage actually dealt.
    pub amount: u32,
    /// The parenthesised modifier: style/bonus damage `(+N)` or absorbed `(-N)`. 0 when absent.
    pub modifier: i32,
    /// True when we were hit, false when we hit something.
    pub incoming: bool,
    /// A critical hit, which the server reports as a SEPARATE follow-up line ("…for an additional
    /// N damage!") rather than folding into the original number. Worth distinguishing so the
    /// floating text can mark it instead of showing two unexplained numbers.
    pub critical: bool,
    /// REQ-021 provenance — chat-scraped damage is tagged [`CombatProvenance::ChatMessage`].
    pub provenance: CombatProvenance,
}

/// Pull a damage number out of a combat message, if it is one.
///
/// Deliberately parses only the two forms actually observed on the wire, and returns `None` for
/// anything else (misses, combat-mode chatter, spell text) rather than guessing — a wrong number
/// floating over the screen is worse than no number.
///
/// ```text
/// "You attack Ire Fairy with your sword and hit for 125 (+129) damage!" → 125, +129, outgoing
/// "The giant skeleton hits your leg for 10 (-8) damage!"                → 10,  -8,   incoming
/// ```
#[must_use]
pub fn parse_damage(text: &str) -> Option<Damage> {
    // Every damage line ends this way; the check cheaply rejects the bulk of the stream.
    if !text.ends_with("damage!") {
        return None;
    }
    // The LAST " for " is the one preceding the number ("...hits your leg for 10").
    let mut after = &text[text.rfind(" for ")? + 5..];

    // Critical hits arrive as their own line, phrased "for an additional N damage!". Validated
    // across the captures: without this, 2 of 60 damage messages failed to parse — and they were
    // precisely the crits, the ones a player most wants to see.
    let critical = after.starts_with("an additional ");
    if critical {
        after = &after["an additional ".len()..];
    }

    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    let amount: u32 = digits.parse().ok()?;

    // Optional "(+N)" / "(-N)" modifier.
    let modifier = match (after.find('('), after.find(')')) {
        (Some(o), Some(c)) if c > o + 1 => after[o + 1..c].parse::<i32>().unwrap_or(0),
        _ => 0,
    };

    // "You attack …" is ours; "The giant skeleton hits your …" is theirs. Direction is all we can
    // attribute without structured combat packets (see the module header).
    let incoming = !text.starts_with("You ");

    Some(Damage {
        amount,
        modifier,
        incoming,
        critical,
        provenance: CombatProvenance::ChatMessage,
    })
}

/// One line in the combat log.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub kind: MessageKind,
    pub text: String,
}

/// A bounded scrollback of recent system messages.
///
/// Bounded because a long session produces thousands of lines and the log is a HUD element, not an
/// archive; the oldest entries fall off the back.
#[derive(Debug, Default)]
pub struct CombatLog {
    entries: VecDeque<LogEntry>,
    /// Lines scrolled back from the newest window (`0` = live tail).
    scroll_back: usize,
}

/// How many lines the log retains.
pub const LOG_CAPACITY: usize = 200;
/// How many are shown on screen at once.
pub const LOG_VISIBLE: usize = 8;

impl CombatLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a 0xAF message. Returns any damage parsed from it, so the caller can spawn floating
    /// text from the same event without re-parsing.
    ///
    /// Client directives are dropped rather than logged — see [`CHAT_TYPE_CLIENT_DIRECTIVE`].
    pub fn push(&mut self, chat_type: u8, text: &str) -> Option<Damage> {
        if chat_type == CHAT_TYPE_CLIENT_DIRECTIVE || text.is_empty() {
            return None;
        }
        let kind = classify(chat_type);
        self.entries.push_back(LogEntry {
            kind,
            text: text.to_string(),
        });
        while self.entries.len() > LOG_CAPACITY {
            self.entries.pop_front();
        }
        if kind == MessageKind::Combat {
            parse_damage(text)
        } else {
            None
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The most recent entries, oldest first — what the on-screen log shows.
    pub fn recent(&self) -> impl Iterator<Item = &LogEntry> {
        let end = self.entries.len().saturating_sub(self.scroll_back);
        let start = end.saturating_sub(LOG_VISIBLE);
        self.entries
            .iter()
            .skip(start)
            .take(end.saturating_sub(start))
    }

    /// Page the visible window toward older lines.
    pub fn page_up(&mut self) {
        let max = self.entries.len().saturating_sub(LOG_VISIBLE);
        self.scroll_back = (self.scroll_back + LOG_VISIBLE).min(max);
    }

    /// Page toward the live tail.
    pub fn page_down(&mut self) {
        self.scroll_back = self.scroll_back.saturating_sub(LOG_VISIBLE);
    }

    #[must_use]
    pub fn scroll_back(&self) -> usize {
        self.scroll_back
    }
}

/// One damage number drifting up the screen.
#[derive(Debug, Clone)]
pub struct FloatingText {
    /// Damage amount, when known (chat scrape). `None` for 0xBC result labels (MISS/HIT/…).
    pub amount: Option<u32>,
    /// Result label from CombatAnimation (HIT/MISS/PARRY/…).
    pub label: Option<&'static str>,
    pub incoming: bool,
    pub critical: bool,
    /// REQ-021 — SCN-05 asserts this equals [`CombatProvenance::CombatAnimation`].
    pub provenance: CombatProvenance,
    /// Where it started, in pixels.
    pub anchor: [f32; 2],
    /// Seconds since it spawned.
    pub age: f32,
    /// Defender object id when known (0xBC path).
    pub defender_id: u16,
}

/// How long a floating number lives.
pub const FLOAT_LIFETIME: f32 = 1.5;
/// How far it drifts upward over its life, in pixels.
const FLOAT_RISE: f32 = 48.0;

/// Live floating numbers, aged and expired each frame.
#[derive(Debug, Default)]
pub struct FloatingTexts {
    items: Vec<FloatingText>,
}

impl FloatingTexts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn(&mut self, damage: Damage, anchor: [f32; 2]) {
        self.items.push(FloatingText {
            amount: Some(damage.amount),
            label: None,
            incoming: damage.incoming,
            critical: damage.critical,
            provenance: damage.provenance,
            anchor,
            age: 0.0,
            defender_id: 0,
        });
    }

    /// Spawn structured combat feedback from CombatAnimation 0xBC (REQ-021 / SCN-05).
    pub fn spawn_combat_anim(
        &mut self,
        anim: &caer_protocol::combat_anim::CombatAnimation,
        anchor: [f32; 2],
        incoming: bool,
    ) {
        self.items.push(FloatingText {
            amount: None,
            label: Some(anim.result.label()),
            incoming,
            critical: false,
            provenance: CombatProvenance::CombatAnimation,
            anchor,
            age: 0.0,
            defender_id: anim.defender_id,
        });
    }

    /// Advance and retire expired numbers. Call once per frame with the frame delta.
    pub fn tick(&mut self, dt: f32) {
        for f in &mut self.items {
            f.age += dt;
        }
        self.items.retain(|f| f.age < FLOAT_LIFETIME);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &FloatingText> {
        self.items.iter()
    }
}

/// Colour per message kind — combat red-ish, loot gold, spell blue, chat pale.
fn kind_colour(kind: MessageKind) -> egui::Color32 {
    match kind {
        MessageKind::Combat => egui::Color32::from_rgb(232, 150, 140),
        MessageKind::Spell => egui::Color32::from_rgb(150, 180, 236),
        MessageKind::Loot => egui::Color32::from_rgb(226, 198, 120),
        MessageKind::Chat => egui::Color32::from_rgb(226, 226, 226),
        MessageKind::Target => egui::Color32::from_rgb(180, 214, 180),
        MessageKind::System => egui::Color32::from_rgb(176, 176, 182),
    }
}

/// Draw the combat log, bottom-left.
pub fn draw_log(root: &mut egui::Ui, log: &CombatLog) {
    if log.is_empty() {
        return;
    }
    let ctx = root.ctx().clone();
    egui::Area::new(egui::Id::new("combat_log"))
        // Stacked above the quickbar's band; the bar is centred and wide enough to cover a
        // bottom-anchored log otherwise.
        .anchor(
            egui::Align2::LEFT_BOTTOM,
            [12.0, -12.0 - crate::quickbar::BAND_HEIGHT],
        )
        .interactable(false)
        .show(&ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_premultiplied(10, 10, 12, 170))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(12, 11, 10)))
                .corner_radius(3)
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.set_max_width(520.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for e in log.recent() {
                        ui.label(
                            egui::RichText::new(&e.text)
                                .size(11.0)
                                .color(kind_colour(e.kind)),
                        );
                    }
                });
        });
}

/// Draw floating damage numbers. Painted directly — they are positioned in screen space and must
/// not participate in layout.
pub fn draw_floating(ui: &egui::Ui, texts: &FloatingTexts) {
    let painter = ui.painter();
    for f in texts.iter() {
        let t = (f.age / FLOAT_LIFETIME).clamp(0.0, 1.0);
        // Rise and fade. Fading only over the back half keeps the number readable when it matters.
        let alpha = (1.0 - (t - 0.5).max(0.0) / 0.5).clamp(0.0, 1.0);
        let pos = egui::pos2(f.anchor[0], f.anchor[1] - FLOAT_RISE * t);

        let colour = if f.incoming {
            egui::Color32::from_rgb(236, 96, 84) // we got hit — red
        } else {
            egui::Color32::from_rgb(248, 226, 140) // we hit — gold
        };
        let a = |c: egui::Color32| {
            egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (alpha * 255.0) as u8)
        };
        // Crits read larger and are marked, so a second number appearing right after the first
        // is self-explaining rather than confusing.
        let font = egui::FontId::proportional(match (f.critical, f.incoming) {
            (true, _) => 21.0,
            (false, true) => 18.0,
            (false, false) => 16.0,
        });
        let label = if let Some(text) = f.label {
            text.to_string()
        } else if let Some(n) = f.amount {
            if f.critical {
                format!("{n}!")
            } else {
                n.to_string()
            }
        } else {
            continue;
        };

        painter.text(
            pos + egui::vec2(1.0, 1.0),
            egui::Align2::CENTER_CENTER,
            &label,
            font.clone(),
            a(egui::Color32::from_rgb(8, 8, 10)),
        );
        painter.text(pos, egui::Align2::CENTER_CENTER, &label, font, a(colour));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two damage forms actually observed on the wire, verbatim from the captures.
    #[test]
    fn parses_the_captured_damage_forms() {
        let out =
            parse_damage("You attack Ire Fairy with your sword and hit for 125 (+129) damage!")
                .expect("outgoing damage should parse");
        assert_eq!(out.amount, 125);
        assert_eq!(out.modifier, 129);
        assert!(!out.incoming);

        let plain =
            parse_damage("You attack the giant skeleton with your sword and hit for 48 damage!")
                .expect("outgoing without a modifier should parse");
        assert_eq!(plain.amount, 48);
        assert_eq!(plain.modifier, 0, "no parenthesised modifier in this form");
        assert!(!plain.incoming);

        let inc = parse_damage("The giant skeleton hits your leg for 10 (-8) damage!")
            .expect("incoming damage should parse");
        assert_eq!(inc.amount, 10);
        assert_eq!(
            inc.modifier, -8,
            "the (-N) form is armour absorption, and is negative"
        );
        assert!(inc.incoming);
    }

    /// Critical hits use a different sentence shape and arrive as their own follow-up line.
    ///
    /// This was found by running the parser over EVERY damage message in the captures rather than
    /// only the forms it was written from: 58 of 60 parsed, and both failures were crits — the
    /// hits a player most wants to see float. Designing from a handful of examples missed them.
    #[test]
    fn critical_hits_parse_and_are_flagged() {
        let c = parse_damage("You critical hit the giant skeleton for an additional 40 damage!")
            .expect("critical hit should parse");
        assert_eq!(c.amount, 40);
        assert!(c.critical);
        assert!(!c.incoming);

        // A normal hit must NOT be flagged critical.
        let n =
            parse_damage("You attack the giant skeleton with your sword and hit for 48 damage!")
                .unwrap();
        assert!(!n.critical);
    }

    /// Non-damage combat chatter must yield nothing rather than a wrong number. A bogus figure
    /// floating over the screen is worse than none.
    #[test]
    fn non_damage_text_yields_no_number() {
        for text in [
            "You enter combat mode and target [Ire Fairy]",
            "Disciple Imogen casts a spell!",
            "The giant skeleton drops a bag of coins.",
            "You target [Disciple Imogen].",
            "",
            "damage!", // ends right, but has no " for N"
        ] {
            assert!(
                parse_damage(text).is_none(),
                "{text:?} should not parse as damage"
            );
        }
    }

    /// Direction is inferred from the sentence, since we cannot attribute by object id.
    #[test]
    fn direction_follows_the_subject_of_the_sentence() {
        assert!(
            !parse_damage("You attack X with your sword and hit for 5 damage!")
                .unwrap()
                .incoming
        );
        assert!(
            parse_damage("Something nasty hits your arm for 5 damage!")
                .unwrap()
                .incoming
        );
    }

    /// Client directives (0x64) carry comma-separated control data, not prose — they must never
    /// reach the log.
    #[test]
    fn client_directives_are_not_logged() {
        let mut log = CombatLog::new();
        log.push(
            CHAT_TYPE_CLIENT_DIRECTIVE,
            "I,1,0,0,0,0,2081,0,2,None,\"Use /gu <text>\"",
        );
        assert!(
            log.is_empty(),
            "structured client data leaked into the combat log"
        );

        log.push(0x00, "If you need in-game assistance…");
        assert_eq!(log.len(), 1);
    }

    /// Message types seen in the captures must classify sensibly, and unknown ones fall back to
    /// System rather than being lost.
    #[test]
    fn captured_message_types_classify() {
        assert_eq!(classify(0x11), MessageKind::Combat);
        assert_eq!(classify(0x1d), MessageKind::Combat);
        assert_eq!(classify(0x10), MessageKind::Spell);
        assert_eq!(classify(0x1a), MessageKind::Loot);
        assert_eq!(classify(0x01), MessageKind::Chat);
        assert_eq!(classify(0x1e), MessageKind::Target);
        assert_eq!(classify(0x00), MessageKind::System);
        assert_eq!(
            classify(0xEE),
            MessageKind::System,
            "unknown types must still be shown"
        );
    }

    /// Pushing a combat line returns its damage so the caller can spawn floating text without
    /// parsing twice; a non-combat line returns none even if it happens to contain a number.
    #[test]
    fn push_returns_damage_only_for_combat_lines() {
        let mut log = CombatLog::new();
        let d = log.push(
            0x11,
            "You attack the giant skeleton with your sword and hit for 63 damage!",
        );
        assert_eq!(d.expect("combat line should yield damage").amount, 63);

        // Loot text mentioning a number is not damage.
        assert!(log
            .push(0x1a, "The giant skeleton drops 12 gold.")
            .is_none());
    }

    /// The log is bounded and shows only the most recent lines, oldest-first.
    #[test]
    fn log_is_bounded_and_shows_the_tail() {
        let mut log = CombatLog::new();
        for i in 0..(LOG_CAPACITY + 50) {
            log.push(0x00, &format!("line {i}"));
        }
        assert_eq!(log.len(), LOG_CAPACITY, "log must be bounded");

        let shown: Vec<&str> = log.recent().map(|e| e.text.as_str()).collect();
        assert_eq!(shown.len(), LOG_VISIBLE);
        let last = LOG_CAPACITY + 49;
        assert_eq!(
            shown.last().copied(),
            Some(format!("line {last}").as_str()),
            "newest line should be last"
        );
    }

    /// PageUp/PageDown must move the visible window; deleting scroll_back makes this red.
    #[test]
    fn falsifier_page_scroll_moves_visible_window() {
        let mut log = CombatLog::new();
        for i in 0..40 {
            log.push(0x00, &format!("line {i}"));
        }
        let live: Vec<String> = log.recent().map(|e| e.text.clone()).collect();
        log.page_up();
        let scrolled: Vec<String> = log.recent().map(|e| e.text.clone()).collect();
        assert_ne!(live, scrolled, "page_up must change the visible window");
        assert!(log.scroll_back() >= LOG_VISIBLE, "scroll_back must advance");
        log.page_down();
        let back: Vec<String> = log.recent().map(|e| e.text.clone()).collect();
        assert_eq!(back, live, "page_down must restore the live tail");
        assert_eq!(log.scroll_back(), 0);
    }

    /// Floating numbers expire, so they cannot accumulate forever over a long fight.
    #[test]
    fn floating_text_ages_out() {
        let mut f = FloatingTexts::new();
        f.spawn(
            Damage {
                amount: 42,
                modifier: 0,
                incoming: false,
                critical: false,
                provenance: CombatProvenance::ChatMessage,
            },
            [100.0, 100.0],
        );
        f.spawn(
            Damage {
                amount: 7,
                modifier: -3,
                incoming: true,
                critical: false,
                provenance: CombatProvenance::ChatMessage,
            },
            [120.0, 140.0],
        );
        assert_eq!(f.len(), 2);

        f.tick(FLOAT_LIFETIME * 0.6);
        assert_eq!(f.len(), 2, "should still be alive mid-life");

        f.tick(FLOAT_LIFETIME);
        assert!(f.is_empty(), "expired numbers must be retired");
    }

    /// REQ-021 / SCN-05: structured 0xBC floaters carry CombatAnimation provenance, never chat.
    ///
    /// SCN-05 PlayScenario elevates this path: `run_scn05` drives OWN_CAPTURE through
    /// [`FloatingTexts::spawn_combat_anim`] and asserts a displayed floater. Numeric damage from
    /// [`parse_damage`] remains a named gap (chat-only) and must not be required for SCN-05 Pass.
    #[test]
    fn combat_anim_floater_provenance_is_not_chat() {
        let anim = caer_protocol::combat_anim::CombatAnimation {
            attacker_id: 1,
            defender_id: 2,
            weapon_id: 0,
            shield_id: 0,
            style: 0,
            stance: 0,
            result: caer_protocol::combat_anim::CombatResult::Missed,
            target_health_pct: 80,
            unk: 0,
        };
        let mut f = FloatingTexts::new();
        f.spawn_combat_anim(&anim, [10.0, 10.0], false);
        let item = f.iter().next().unwrap();
        assert_eq!(item.provenance, CombatProvenance::CombatAnimation);
        assert_eq!(item.provenance.as_str(), "CombatAnimation 0xBC");
        assert_eq!(item.label, Some("MISS"));
        assert!(
            item.amount.is_none(),
            "0xBC must not invent a damage number"
        );

        // Chat scrape is a different provenance — SCN-05 must be able to tell them apart.
        let chat = parse_damage("You attack X with your sword and hit for 5 damage!").unwrap();
        assert_eq!(chat.provenance, CombatProvenance::ChatMessage);
        assert_ne!(chat.provenance.as_str(), item.provenance.as_str());
    }
}
