//! The chat window — typing back at the world (C.3).
//!
//! C.1 and C.7 built *output* surfaces: the HUD, nameplates, the combat log. This is the first
//! **interactive** overlay, and it is where the keyboard-capture path that both earlier slices
//! deliberately left as a no-op finally earns its keep: while the input line has focus, `w` must
//! type a `w`, not walk the character forward.
//!
//! ## How capture works
//! `rustdaoc`'s window handler feeds every event to egui first and returns early when egui claims
//! it. egui claims keys only while a text widget holds focus, so the game keeps its hotkeys the
//! rest of the time. [`ChatState::open`] drives that focus, and the client asks
//! [`ChatState::captures_keyboard`] before acting on movement keys — belt and braces, because a
//! movement key leaking through would be both obvious and infuriating.
//!
//! ## Channels
//! DAoC routes chat with slash commands. The server already accepts them: `LiveCommand::Say` for
//! plain speech and `LiveCommand::Command` for everything else (the `0xAF Command` packet, which is
//! `[✓cap]`-verified). So channel support is mostly parsing the leading token and picking which of
//! those two to send — no new protocol work, which is why this lands cleanly while combat does not.

/// What a submitted line should do once it leaves the input box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
    /// Plain speech to nearby players (`LiveCommand::Say`).
    Say(String),
    /// A slash command WITHOUT the leading `/` (`LiveCommand::Command`), e.g. `"gu hello"`.
    Command(String),
}

/// The prefix shown in the input box, telling the player where their text is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Channel {
    #[default]
    Say,
    Group,
    Guild,
    /// Private `/send` — never routes through [`Outgoing::Say`].
    Send,
    /// Private `/tell` — never routes through [`Outgoing::Say`].
    Tell,
}

impl Channel {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Channel::Say => "Say",
            Channel::Group => "Group",
            Channel::Guild => "Guild",
            Channel::Send => "Send",
            Channel::Tell => "Tell",
        }
    }

    /// Whether this channel is private (must not leak into public Say).
    #[must_use]
    pub fn is_private(self) -> bool {
        matches!(self, Channel::Send | Channel::Tell)
    }

    /// The slash command that routes a line to this channel.
    fn command(self) -> &'static str {
        match self {
            Channel::Say => "say",
            Channel::Group => "g",
            Channel::Guild => "gu",
            Channel::Send => "send",
            Channel::Tell => "tell",
        }
    }
}

/// Chat input state. The message *history* lives in [`crate::combat::CombatLog`] — this is only
/// the input line, so there is one message pane rather than two competing ones.
#[derive(Debug, Default)]
pub struct ChatState {
    /// Whether the input line is showing and focused.
    pub open: bool,
    /// Current text being typed.
    pub input: String,
    /// Which channel a bare line (no slash) goes to.
    pub channel: Channel,
    /// Previously sent lines, newest last, for up-arrow recall.
    history: Vec<String>,
    /// Position in `history` while recalling; `None` when typing fresh.
    recall: Option<usize>,
}

/// How many sent lines are kept for recall.
const HISTORY_LIMIT: usize = 50;

impl ChatState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the game must ignore keyboard input this frame because chat owns it.
    #[must_use]
    pub fn captures_keyboard(&self) -> bool {
        self.open
    }

    /// Open the input line (Enter, when closed).
    pub fn open(&mut self) {
        self.open = true;
        self.recall = None;
    }

    /// Close and discard whatever was typed (Escape).
    pub fn cancel(&mut self) {
        self.open = false;
        self.input.clear();
        self.recall = None;
    }

    /// Submit the current line. Returns what to send, or `None` for an empty line (which just
    /// closes the box, matching every chat client ever).
    pub fn submit(&mut self) -> Option<Outgoing> {
        let line = self.input.trim().to_string();
        self.input.clear();
        self.open = false;
        self.recall = None;
        if line.is_empty() {
            return None;
        }
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
            if self.history.len() > HISTORY_LIMIT {
                self.history.remove(0);
            }
        }
        Some(route(&line, self.channel))
    }

    /// Step back through previously sent lines (Up).
    pub fn recall_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.recall {
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.recall = Some(next);
        self.input = self.history[next].clone();
    }

    /// Step forward again (Down); past the newest entry the box returns to empty.
    pub fn recall_next(&mut self) {
        match self.recall {
            Some(i) if i + 1 < self.history.len() => {
                self.recall = Some(i + 1);
                self.input = self.history[i + 1].clone();
            }
            Some(_) => {
                self.recall = None;
                self.input.clear();
            }
            None => {}
        }
    }
}

/// Decide where a typed line goes.
///
/// A leading `/` is a command and is passed through with the slash stripped here; the protocol
/// layer re-adds the wire prefix. NOTE the wire form is `&verb`, NOT a bare verb: DOL registers
/// commands under keys like `&jump` and `ScriptMgr.GuessCommand` matches against those, so a bare
/// verb matches nothing, exact or prefix. `session::command` builds `&{cmd}\0` accordingly.
/// Anything else is sent on the active channel;
/// for Say that is `LiveCommand::Say`, and for the others it becomes the equivalent slash command,
/// because the server has no separate "group chat" packet — it is all `0xAF Command`.
///
/// Private channels ([`Channel::Send`], [`Channel::Tell`]) and private slash verbs (`send`, `tell`,
/// `whisper`) **never** produce [`Outgoing::Say`] — that is the Codex social private-routing
/// falsifier surface.
#[must_use]
pub fn route(line: &str, channel: Channel) -> Outgoing {
    if let Some(rest) = line.strip_prefix('/') {
        // "//text" is the conventional escape for a literal leading slash.
        if let Some(literal) = rest.strip_prefix('/') {
            // Escaped slash is literal public speech — only when not on a private channel.
            if channel.is_private() {
                return Outgoing::Command(format!("{} /{}", channel.command(), literal));
            }
            return Outgoing::Say(literal.to_string());
        }
        return Outgoing::Command(rest.to_string());
    }
    match channel {
        Channel::Say => Outgoing::Say(line.to_string()),
        other => Outgoing::Command(format!("{} {}", other.command(), line)),
    }
}

/// True when an outgoing line is private send/tell/whisper and must not be treated as public Say.
#[must_use]
pub fn is_private_outgoing(out: &Outgoing) -> bool {
    match out {
        Outgoing::Say(_) => false,
        Outgoing::Command(cmd) => {
            let verb = cmd.split_whitespace().next().unwrap_or("");
            matches!(verb, "send" | "tell" | "whisper" | "s")
        }
    }
}

/// Draw the input line, just above the message log. Returns what to send if the player pressed
/// Enter this frame.
///
/// Only drawn while open: DAoC shows no permanent input box, and an always-present focused field
/// would swallow the movement keys for the whole session.
pub fn draw(root: &mut egui::Ui, state: &mut ChatState) -> Option<Outgoing> {
    if !state.open {
        return None;
    }
    let ctx = root.ctx().clone();
    let mut submitted = None;

    egui::Area::new(egui::Id::new("chat_input"))
        // Stacks above both the quickbar band and the log block.
        .anchor(
            egui::Align2::LEFT_BOTTOM,
            [
                12.0,
                -12.0 - crate::quickbar::BAND_HEIGHT - LOG_BLOCK_HEIGHT,
            ],
        )
        .show(&ctx, |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_premultiplied(16, 16, 20, 225))
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(90, 88, 84)))
                .corner_radius(3)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("[{}]", state.channel.label()))
                                .color(egui::Color32::from_rgb(212, 196, 140))
                                .size(12.0),
                        );
                        let edit = egui::TextEdit::singleline(&mut state.input)
                            .desired_width(440.0)
                            .font(egui::FontId::proportional(12.0))
                            .hint_text("say something…");
                        let r = ui.add(edit);

                        // Submit BEFORE re-requesting focus, and accept either focus state.
                        //
                        // This previously called `request_focus()` unconditionally and then tested
                        // `lost_focus() && Enter`. The unconditional re-request meant the field
                        // never reported losing focus, so the Enter branch could never fire and the
                        // whole chat box was inert — every Say and every slash command, not just
                        // `/jump`. Keying off `has_focus() || lost_focus()` works whether egui
                        // surrenders focus on Enter or keeps it, and focus is only re-taken when we
                        // did NOT submit, so the field still swallows keystrokes while open.
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if enter && (r.has_focus() || r.lost_focus()) {
                            submitted = Some(());
                        } else {
                            r.request_focus();
                        }
                    });
                });
        });

    // History recall while the box is open.
    if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
        state.recall_previous();
    }
    if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
        state.recall_next();
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.cancel();
        return None;
    }

    if submitted.is_some() {
        return state.submit();
    }
    None
}

/// Vertical space the combat log occupies, so the input line can sit clear of it.
const LOG_BLOCK_HEIGHT: f32 = 120.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare line goes to the active channel; a slash line becomes a command with the slash
    /// stripped, because the wire format carries the bare verb.
    #[test]
    fn routing_follows_the_slash_and_the_channel() {
        assert_eq!(route("hello", Channel::Say), Outgoing::Say("hello".into()));
        assert_eq!(
            route("/gu hello", Channel::Say),
            Outgoing::Command("gu hello".into())
        );
        assert_eq!(
            route("/stick", Channel::Say),
            Outgoing::Command("stick".into())
        );

        // On a non-Say channel a bare line becomes that channel's slash command — the server has
        // no separate group/guild chat packet.
        assert_eq!(
            route("hi team", Channel::Group),
            Outgoing::Command("g hi team".into())
        );
        assert_eq!(
            route("hi guild", Channel::Guild),
            Outgoing::Command("gu hi guild".into())
        );

        // "//" escapes a literal leading slash into speech.
        assert_eq!(
            route("//not a command", Channel::Say),
            Outgoing::Say("not a command".into())
        );
    }

    /// Private send/tell payload must never appear as public Say (Codex social falsifier).
    #[test]
    fn private_send_tell_never_routes_to_say() {
        let secret = "trade terms stay private";
        for ch in [Channel::Send, Channel::Tell] {
            let out = route(secret, ch);
            assert!(
                !matches!(out, Outgoing::Say(_)),
                "{ch:?} bare line must not be Say"
            );
            assert!(is_private_outgoing(&out));
            if let Outgoing::Command(cmd) = &out {
                assert!(
                    cmd.contains(secret),
                    "private body retained in command: {cmd}"
                );
            }
        }
        for line in [
            "/tell Bob secret-payload",
            "/send Bob secret-payload",
            "/whisper Bob secret-payload",
        ] {
            let out = route(line, Channel::Say);
            assert!(
                !matches!(out, Outgoing::Say(_)),
                "{line} must not become Say"
            );
            assert!(is_private_outgoing(&out));
            assert!(
                !matches!(out, Outgoing::Say(ref s) if s.contains("secret-payload")),
                "private payload must not leak into Say text"
            );
        }
        // Channel::Tell + //escape still stays Command (private channel wins).
        let out = route("//literally", Channel::Tell);
        assert!(matches!(out, Outgoing::Command(_)));
        assert!(!matches!(out, Outgoing::Say(_)));
    }

    /// Submitting clears and closes; an empty line sends nothing but still closes the box.
    #[test]
    fn submit_clears_and_closes() {
        let mut c = ChatState::new();
        c.open();
        assert!(c.captures_keyboard(), "an open chat must own the keyboard");

        c.input = "  hello world  ".into();
        assert_eq!(
            c.submit(),
            Some(Outgoing::Say("hello world".into())),
            "input should be trimmed"
        );
        assert!(c.input.is_empty());
        assert!(!c.open);
        assert!(
            !c.captures_keyboard(),
            "a closed chat must release the keyboard"
        );

        c.open();
        c.input = "   ".into();
        assert_eq!(c.submit(), None, "an empty line sends nothing");
        assert!(!c.open, "…but still closes the box");
    }

    /// Escape abandons the line rather than sending it.
    #[test]
    fn cancel_discards_the_line() {
        let mut c = ChatState::new();
        c.open();
        c.input = "half-typed".into();
        c.cancel();
        assert!(!c.open);
        assert!(
            c.input.is_empty(),
            "cancelled text must not survive to the next open"
        );
    }

    /// Up/Down walk the sent history and back out to an empty line.
    #[test]
    fn history_recall_walks_both_ways() {
        let mut c = ChatState::new();
        for line in ["first", "second", "third"] {
            c.open();
            c.input = line.into();
            c.submit();
        }

        c.open();
        c.recall_previous();
        assert_eq!(c.input, "third", "Up should reach the newest line first");
        c.recall_previous();
        assert_eq!(c.input, "second");
        c.recall_next();
        assert_eq!(c.input, "third");
        c.recall_next();
        assert_eq!(c.input, "", "past the newest, the box empties again");
    }

    /// Repeating the same line must not fill the history with duplicates.
    #[test]
    fn repeated_lines_are_not_duplicated() {
        let mut c = ChatState::new();
        for _ in 0..3 {
            c.open();
            c.input = "same".into();
            c.submit();
        }
        c.open();
        c.recall_previous();
        assert_eq!(c.input, "same");
        c.recall_previous();
        assert_eq!(c.input, "same", "only one entry should exist to recall");
    }

    /// The history is bounded — a long session must not grow it without limit.
    #[test]
    fn history_is_bounded() {
        let mut c = ChatState::new();
        for i in 0..(HISTORY_LIMIT + 20) {
            c.open();
            c.input = format!("line {i}");
            c.submit();
        }
        assert_eq!(c.history.len(), HISTORY_LIMIT);
        c.open();
        c.recall_previous();
        let newest = HISTORY_LIMIT + 19;
        assert_eq!(
            c.input,
            format!("line {newest}"),
            "the newest line must survive trimming"
        );
    }
}
