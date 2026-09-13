//! `caer-client` as a library — the reusable **live-session driver**.
//!
//! The headless `caer-client` bin proved our Rust protocol stack can *be* a DAoC client. That
//! same login→world-entry→keepalive loop is now needed by a *second* consumer: the renderer
//! (`caer-render --live`) wants to feed its `WorldState` from a real server, not a static dump.
//! Rather than duplicate the fragile session-driving loop, both consumers drive ONE
//! [`LiveSession`]: it owns the socket, steps the verified [`SessionState`] machine, auto-pumps
//! every outbound `Action::Send`, auto-selects a character, keepalive-pings after login, and
//! hands the caller the stream of [`ServerEvent`]s it decoded.
//!
//! Layering: this crate owns socket I/O; `caer-protocol` stays pure (machine + codecs, no
//! sockets). `caer-render` depends on THIS for the live feed — the renderer never reaches into
//! the protocol machine directly, and no renderer type leaks downward (the project's hard rule).
//!
//! ```no_run
//! use caer_client::{Config, LiveSession};
//! let mut sess = LiveSession::connect(&Config::new("127.0.0.1:10311", "rustdaoc", "rustdaoc"))?;
//! loop {
//!     match sess.poll() {
//!         Ok(events) => for ev in events { /* apply to a WorldState, log, … */ },
//!         Err(closed) => { log::info!("session ended: {}", closed.0); break; }
//!     }
//! }
//! # Ok::<(), std::io::Error>(())
//! ```

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use caer_protocol::framing::ServerPacketHeader;
use caer_protocol::session::{Action, ServerEvent, SessionPhase, SessionState};

pub mod evidence;
pub mod live_harness;

/// The 8-byte client-id trailer of a real `LoginRequest`, captured from a genuine RustDAoC login.
/// Machine-specific, but a plaintext freeshard server doesn't validate it (see the P0 capture).
pub const DEFAULT_CLIENT_ID: [u8; 8] = [0x30, 0x72, 0x4A, 0x62, 0xF1, 0x8A, 0x0C, 0x85];

/// How long a single [`LiveSession::poll`] blocks waiting for server bytes before returning
/// (empty). Short enough that a caller's loop stays responsive — a driver that interleaves
/// outbound commands (e.g. movement) between polls gets them on the wire within this window —
/// long enough to avoid busy-spin.
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// The DoL keepalive cadence after login: the real client pings ~every 4s, the server echoes.
const PING_EVERY: Duration = Duration::from_secs(4);

/// An authenticated pre-world screen is still a live DoL session. If pings wait for world entry,
/// reading realm prose or creating a character eventually trips the server's link-death timer.
fn phase_needs_keepalive(phase: SessionPhase) -> bool {
    matches!(
        phase,
        SessionPhase::RealmSelect
            | SessionPhase::CharacterSelect
            | SessionPhase::EnteringWorld
            | SessionPhase::InWorld
    )
}

/// What to connect to and who to play.
pub struct Config {
    pub server: String,
    pub account: String,
    pub password: String,
    /// Character to play, by name (case-insensitive). `None` = the first character on the overview
    /// when [`Self::auto_select`] is true.
    pub character: Option<String>,
    /// 8-byte `LoginRequest` trailer; defaults to [`DEFAULT_CLIENT_ID`].
    pub client_id: [u8; 8],
    /// When true (default), [`LiveSession::poll`] auto-sends `select_character` after the overview.
    /// The player client sets this **false** so selection goes through the UI / command seam
    /// (SCN-01): the selection packet must carry the index the UI chose.
    pub auto_select: bool,
    /// Realm whose characters to list (1 Albion / 2 Midgard / 3 Hibernia). Carried on
    /// CharacterOverviewRequest; defaults to Albion.
    pub realm: u8,
}

impl Config {
    /// A config for `server`/`account`/`password` with the default client id, first-character
    /// auto-select, and the headless-bot default of `auto_select = true`. Set
    /// [`Config::character`] afterwards to pick a specific one; set `auto_select = false` for the
    /// player UI path.
    pub fn new(
        server: impl Into<String>,
        account: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            server: server.into(),
            account: account.into(),
            password: password.into(),
            character: None,
            client_id: DEFAULT_CLIENT_ID,
            auto_select: true,
            realm: 1,
        }
    }
}

/// Returned by [`LiveSession::poll`] when the session has ended — clean server close, a socket
/// error, or a fatal frame-decode error. The string is a human-readable reason for logging.
#[derive(Debug)]
pub struct Closed(pub String);

/// A live connection to a DoL server, driven entirely by `caer-protocol`. Construct with
/// [`LiveSession::connect`] (which sends the opening crypt-key request), then call [`poll`] in a
/// loop to advance the session and drain decoded [`ServerEvent`]s.
///
/// [`poll`]: LiveSession::poll
pub struct LiveSession {
    sock: TcpStream,
    session: SessionState,
    /// Rolling receive buffer: complete frames are split off the front, a partial tail is kept.
    rx: Vec<u8>,
    /// Scratch read buffer (boxed to keep `LiveSession` small on the stack).
    buf: Box<[u8; 8192]>,
    /// Wall clock the session started at — the keepalive ping stamps elapsed-ms (server echoes it).
    started: Instant,
    last_ping: Instant,
    /// Character to auto-select (name match or first); consumed once on the overview when
    /// [`Config::auto_select`] is true.
    want_character: Option<String>,
    /// When false, overview never triggers an automatic `select_character` — the UI / command
    /// seam must call [`Self::select_character`] (SCN-01).
    auto_select: bool,
    /// The character we chose to play, once the overview named one (slot + name).
    chosen: Option<(u8, String)>,
    /// True once we've sent the first `select_character` (so we do it exactly once).
    selected: bool,
    /// True once the server sent `CharacterInitFinished`.
    entered: bool,
    /// True once an overview arrived with no playable character (or the requested one was absent):
    /// there is nothing to select, so a caller can stop rather than wait forever.
    stuck_at_char_select: bool,
    /// True once the server confirmed a clean logout (Quit 0xA4) — the socket can be dropped
    /// without a link-death ghost.
    logged_out: bool,
    /// Last character overview, for UI listing when `auto_select` is false.
    overview: Option<caer_protocol::overview::CharacterOverview>,
    /// When `Some`, every complete S2C GSTCP frame is appended (size+code+payload).
    /// Used by capture harnesses (`death_rt`, etc.) — not enabled by default.
    s2c_log: Option<Vec<u8>>,
}

/// How long TCP connect may block before failing (Drop must not hang on a black-holed host).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound write stalls so cancel/Drop can reclaim the session thread.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

fn resolve_first(server: &str) -> io::Result<std::net::SocketAddr> {
    server.to_socket_addrs()?.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("no addresses for {server}"),
        )
    })
}

impl LiveSession {
    /// Connect to `cfg.server`, build the session machine, and send the opening crypt-key request.
    /// The socket uses short read/write timeouts so [`poll`](Self::poll) and Drop stay responsive.
    pub fn connect(cfg: &Config) -> io::Result<Self> {
        // Typed evidence banner for scenario runners (Codex B1). Harmless for interactive play.
        crate::evidence::emit_from_env();
        let addr = resolve_first(&cfg.server)?;
        let sock = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
        sock.set_read_timeout(Some(READ_TIMEOUT))?;
        sock.set_write_timeout(Some(WRITE_TIMEOUT))?;
        sock.set_nodelay(true).ok();

        let session = SessionState::new(&cfg.account, &cfg.password)
            .with_client_id(cfg.client_id)
            .with_realm(cfg.realm.clamp(1, 3));
        let now = Instant::now();
        let mut me = Self {
            sock,
            session,
            rx: Vec::new(),
            buf: Box::new([0u8; 8192]),
            started: now,
            last_ping: now,
            want_character: cfg.character.clone(),
            auto_select: cfg.auto_select,
            chosen: None,
            selected: false,
            entered: false,
            stuck_at_char_select: false,
            logged_out: false,
            overview: None,
            s2c_log: None,
        };
        // Kick off the handshake: the crypt-key request.
        let begin = me.session.begin();
        me.pump(begin)?;
        Ok(me)
    }

    /// Advance the session by one read tick and return every [`ServerEvent`] decoded from the
    /// bytes that arrived. Handles all the plumbing internally: pumps the machine's outbound
    /// `Action::Send`s, auto-selects a character on the overview, and keepalive-pings throughout
    /// authenticated pre-world and in-world phases. Returns `Ok(vec![])` when no bytes were ready,
    /// `Err(Closed)` when the connection ends.
    pub fn poll(&mut self) -> Result<Vec<ServerEvent>, Closed> {
        // Realm/character/create screens can remain open longer than the server idle timeout.
        if phase_needs_keepalive(self.session.phase()) && self.last_ping.elapsed() >= PING_EVERY {
            self.last_ping = Instant::now();
            let ms = self.started.elapsed().as_millis() as u32;
            let ping = self.session.ping(ms);
            self.pump(ping)
                .map_err(|e| Closed(format!("ping send failed: {e}")))?;
        }

        let n = match self.sock.read(&mut self.buf[..]) {
            Ok(0) => return Err(Closed("server closed the connection".into())),
            Ok(n) => n,
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                return Ok(Vec::new()); // idle tick — no bytes ready
            }
            Err(e) => return Err(Closed(format!("read error: {e}"))),
        };
        self.rx.extend_from_slice(&self.buf[..n]);

        // Split every complete server frame off the front of the rolling buffer, stepping the
        // machine on each and collecting the events for the caller.
        let mut events = Vec::new();
        loop {
            let (header, payload, used) = match ServerPacketHeader::decode_prefix(&self.rx) {
                Ok(Some((h, payload, used))) => (h, payload.to_vec(), used),
                Ok(None) => break, // partial frame — wait for more bytes
                Err(e) => return Err(Closed(format!("frame decode error: {e}"))),
            };
            if let Some(log) = self.s2c_log.as_mut() {
                log.extend_from_slice(&self.rx[..used]);
            }
            let actions = self.session.on_server_packet(header, &payload);
            // Send whatever the machine wants to send in response (world-entry sequence, etc.).
            let mut to_send = Vec::new();
            for a in actions {
                match a {
                    Action::Send(bytes) => to_send.push(bytes),
                    Action::Event(ev) => {
                        self.note_event(&ev);
                        events.push(ev);
                    }
                    // Phase transitions are observability-only; the caller reads phase via
                    // `LiveSession::phase()` when it wants it.
                    _ => {}
                }
            }
            for bytes in to_send {
                if let Err(e) = self.write_frame(&bytes) {
                    return Err(Closed(format!("send failed: {e}")));
                }
            }
            self.rx.drain(..used);

            // Auto-select only when configured (headless bots). The player client disables this so
            // SCN-01 holds: selection packet carries the UI-chosen slot via [`Self::select_character`].
            if self.auto_select {
                if let Some((slot, _)) = self.chosen.clone() {
                    if !self.selected {
                        self.selected = true;
                        let sel = self.session.select_character(slot);
                        if let Err(e) = self.pump(sel) {
                            return Err(Closed(format!("select_character send failed: {e}")));
                        }
                    }
                }
            }
        }
        Ok(events)
    }

    /// Update the machine's bookkeeping from an event before it reaches the caller: pick a
    /// character on the overview, mark world entry (starts keepalive).
    fn note_event(&mut self, ev: &ServerEvent) {
        match ev {
            ServerEvent::CharacterOverview(ov) => {
                self.overview = Some(ov.clone());
                if self.auto_select {
                    self.chosen = match &self.want_character {
                        Some(want) => ov
                            .characters
                            .iter()
                            .find(|c| c.name.eq_ignore_ascii_case(want))
                            .map(|c| (c.slot, c.name.clone())),
                        None => ov.characters.first().map(|c| (c.slot, c.name.clone())),
                    };
                    if self.chosen.is_none() {
                        self.stuck_at_char_select = true;
                    }
                } else if ov.characters.is_empty() {
                    self.stuck_at_char_select = true;
                }
            }
            ServerEvent::EnteredWorld => {
                self.entered = true;
                self.last_ping = Instant::now();
            }
            ServerEvent::LoggedOut { .. } => {
                self.logged_out = true;
            }
            _ => {}
        }
    }

    /// UI / command-seam character select (SCN-01). Sends `CharacterSelectRequest` with `slot` and
    /// starts world entry. Idempotent after the first successful call.
    pub fn select_character(&mut self, slot: u8) -> io::Result<()> {
        if self.selected {
            return Ok(());
        }
        let name = self
            .overview
            .as_ref()
            .and_then(|ov| ov.characters.iter().find(|c| c.slot == slot))
            .map(|c| c.name.clone())
            .unwrap_or_default();
        self.chosen = Some((slot, name));
        self.selected = true;
        let sel = self.session.select_character(slot);
        self.pump(sel)
    }

    /// Submit CharacterCreateRequest 0xFF (create form Continué). Does not enter the world.
    pub fn create_character(
        &mut self,
        draft: &caer_protocol::charcreate::CharacterCreateDraft,
    ) -> io::Result<()> {
        let a = self.session.create_character(draft);
        self.pump(a)
    }

    /// Delete the character in `slot` of `realm`, confirmed through `delete_confirm.xml`.
    ///
    /// The same 0xFF the create form sends, with `CreateOp::Delete` and an empty name — there is no
    /// separate delete packet. See `Session::delete_character` for the oracle.
    pub fn delete_character(&mut self, realm: u8, slot: u8) -> io::Result<()> {
        let a = self.session.delete_character(realm, slot);
        self.pump(a)
    }

    /// Re-request CharacterOverview for `realm` after a pre-world realm pick.
    pub fn request_character_overview(&mut self, realm: u8) -> io::Result<()> {
        let a = self.session.request_character_overview(realm);
        self.pump(a)
    }

    /// CheckLOSResponse 0xD0 — answer CheckLOSRequest 0xD0 S2C.
    pub fn check_los_response(
        &mut self,
        checker_oid: u16,
        target_oid: u16,
        response: u16,
    ) -> io::Result<()> {
        let a = self
            .session
            .check_los_response(checker_oid, target_oid, response);
        self.pump(a)
    }

    /// PlayerMoveItem 0xDD — equip / bag move. Self confirm is 0x02 worn slots (not self-0x15).
    pub fn move_item(&mut self, to_slot: u16, from_slot: u16, count: u16) -> io::Result<()> {
        let a = self.session.move_item(to_slot, from_slot, count);
        self.pump(a)
    }

    /// ObjectInteractRequest 0x7A — open merchant / use object.
    pub fn interact(&mut self, player_x: u32, player_y: u32, target_oid: u16) -> io::Result<()> {
        let a = self.session.interact(player_x, player_y, target_oid);
        self.pump(a)
    }

    /// PickUpRequest 0xB5 — pick up targeted ground loot (call [`Self::target`] first).
    pub fn pickup(&mut self, player_x: u32, player_y: u32, object_id: u16) -> io::Result<()> {
        let a = self.session.pickup(player_x, player_y, object_id);
        self.pump(a)
    }

    /// BuyRequest 0x78 — purchase from the currently targeted merchant.
    pub fn buy_item(
        &mut self,
        player_x: u32,
        player_y: u32,
        merchant_id: u16,
        item_slot: u16,
        item_count: u8,
    ) -> io::Result<()> {
        let a = self
            .session
            .buy_item(player_x, player_y, merchant_id, item_slot, item_count);
        self.pump(a)
    }

    /// SellRequest 0x79. Does not mutate local inventory or purse.
    pub fn sell_item(
        &mut self,
        player_x: u32,
        player_y: u32,
        merchant_id: u16,
        item_slot: u16,
    ) -> io::Result<()> {
        let a = self
            .session
            .sell_item(player_x, player_y, merchant_id, item_slot);
        self.pump(a)
    }

    /// UseSlot 0x71 (1.124+). Does not consume or equip locally.
    pub fn use_slot(
        &mut self,
        x: f32,
        y: f32,
        z: f32,
        speed: f32,
        heading: u16,
        flag_speed_data: u16,
        slot: u8,
        use_type: u8,
    ) -> io::Result<()> {
        let a = self
            .session
            .use_slot(x, y, z, speed, heading, flag_speed_data, slot, use_type);
        self.pump(a)
    }

    /// CraftRequest 0xED. Does not invent a crafted item locally.
    pub fn craft_item(&mut self, item_id: u16) -> io::Result<()> {
        let a = self.session.craft_item(item_id);
        self.pump(a)
    }

    /// DestroyItemRequest 0x80. Does not remove the item locally.
    pub fn destroy_item(&mut self, slot: u16) -> io::Result<()> {
        let a = self.session.destroy_item(slot);
        self.pump(a)
    }

    /// TrainWindowHandler 0x7B. Does not invent spec levels.
    pub fn train_window(&mut self) -> io::Result<()> {
        let a = self.session.train_window();
        self.pump(a)
    }

    /// TrainRequest 0x7C. Does not award spec levels locally.
    pub fn train_request(
        &mut self,
        player_x: u32,
        player_y: u32,
        id_line: u8,
        unk: u8,
        row: u8,
        skill_index: u8,
    ) -> io::Result<()> {
        let a = self
            .session
            .train_request(player_x, player_y, id_line, unk, row, skill_index);
        self.pump(a)
    }

    /// SiegeCommandRequest 0xF5. Does not invent siege state.
    pub fn siege_command(&mut self, action: u8, ammo: u8) -> io::Result<()> {
        let a = self.session.siege_command(action, ammo);
        self.pump(a)
    }

    /// PlayerSitRequest 0xC7. Does not invent a local sit pose.
    pub fn sit(&mut self, sit: bool) -> io::Result<()> {
        let a = self.session.sit(sit);
        self.pump(a)
    }

    /// DialogResponse 0x82 — answer a CustomDialog / invite Dialog.
    pub fn dialog_response(
        &mut self,
        data1: u16,
        data2: u16,
        data3: u16,
        message_type: u8,
        response: u8,
    ) -> io::Result<()> {
        let a = self
            .session
            .dialog_response(data1, data2, data3, message_type, response);
        self.pump(a)
    }

    /// DoorRequest 0x99 — open/close by InternalID. Confirm is DoorState 0x99 S2C.
    pub fn door_request(&mut self, door_id: u32, door_state: u8) -> io::Result<()> {
        let a = self.session.door_request(door_id, door_state);
        self.pump(a)
    }

    /// PlayerGroundTarget 0xEC — set cast/siege ground point (no optimistic local apply).
    pub fn ground_target(&mut self, x: i32, y: i32, z: i32, flag: u16) -> io::Result<()> {
        let a = self.session.ground_target(x, y, z, flag);
        self.pump(a)
    }

    /// InviteToGroup 0x87 — requires prior [`Self::target`]. Confirm is 0x70 / GroupWindow.
    pub fn invite_to_group(&mut self) -> io::Result<()> {
        let a = self.session.invite_to_group();
        self.pump(a)
    }

    /// ModifyTrade 0xEB — cancel/update/accept. Confirm via 0xEA and (on accept) 0x02+0xFA both sides.
    pub fn modify_trade(
        &mut self,
        action: caer_protocol::social::ModifyTradeAction,
        repair: bool,
        combine: bool,
        slots: &[u8; 10],
        money: caer_protocol::social::TradeMoney,
    ) -> io::Result<()> {
        let a = self
            .session
            .modify_trade(action, repair, combine, slots, money);
        self.pump(a)
    }

    /// Current session overview-request realm byte.
    #[must_use]
    pub fn realm(&self) -> u8 {
        self.session.realm()
    }

    /// Last overview received, if any (for UI listing).
    #[must_use]
    pub fn overview(&self) -> Option<&caer_protocol::overview::CharacterOverview> {
        self.overview.as_ref()
    }

    /// Whether a selection packet has already been sent.
    #[must_use]
    pub fn character_selected(&self) -> bool {
        self.selected
    }

    /// `/say` a line, visible to nearby players (the on-screen proof).
    pub fn say(&mut self, msg: &str) -> io::Result<()> {
        let a = self.session.command(&format!("say {msg}"));
        self.pump(a)
    }

    /// Tell the server which object we have targeted (`0` clears it). Targeting is
    /// client-authoritative — the server records it for later attack/spell/interact requests and
    /// sends no acknowledgement, so there is nothing to await here.
    pub fn target(&mut self, object_id: u16) -> io::Result<()> {
        let a = self.session.target(object_id);
        self.pump(a)
    }

    /// Position update carrying the full motion state (zone, object id, jump/strafe flags).
    pub fn position_update_full(
        &mut self,
        x: f32,
        y: f32,
        z: f32,
        speed: f32,
        health_pct: u8,
        motion: caer_protocol::session::PlayerMotion,
    ) -> io::Result<()> {
        let a = self
            .session
            .position_update_full(x, y, z, speed, health_pct, motion);
        self.pump(a)
    }

    /// Use a skill/style/spell by its INDEX in the server's usable-skill list.
    pub fn use_skill(
        &mut self,
        index: u8,
        skill_type: u8,
        x: f32,
        y: f32,
        z: f32,
    ) -> io::Result<()> {
        let a = self.session.use_skill(index, skill_type, x, y, z);
        self.pump(a)
    }

    /// Ask the server to (re)send an entity's create packet.
    pub fn request_npc(&mut self, object_id: u16) -> io::Result<()> {
        let a = self.session.request_npc(object_id);
        self.pump(a)
    }

    /// Start or stop melee attack mode.
    pub fn attack(&mut self, start: bool) -> io::Result<()> {
        let a = self.session.attack(start);
        self.pump(a)
    }

    /// Send a raw slash command (without the leading `/`).
    pub fn command(&mut self, cmd: &str) -> io::Result<()> {
        let a = self.session.command(cmd);
        self.pump(a)
    }

    /// Begin appending complete S2C GSTCP frames to an internal buffer (clears any prior log).
    pub fn start_s2c_log(&mut self) {
        self.s2c_log = Some(Vec::new());
    }

    /// Stop logging and return the accumulated S2C frame bytes (empty if never started).
    pub fn take_s2c_log(&mut self) -> Vec<u8> {
        self.s2c_log.take().unwrap_or_default()
    }

    /// Whether S2C frame logging is active.
    #[must_use]
    pub fn s2c_logging(&self) -> bool {
        self.s2c_log.is_some()
    }

    /// Request a clean logout (`/quit`). The server saves + removes the character and replies with
    /// Quit (0xA4); keep [`poll`](Self::poll)ing until [`logged_out`](Self::logged_out) is true
    /// (or a `ServerEvent::LoggedOut` surfaces), THEN drop the session — closing earlier trips
    /// link-death. On a quit-timer server the character must be stationary for the quit to take.
    pub fn quit(&mut self) -> io::Result<()> {
        let a = self.session.quit();
        self.pump(a)
    }

    /// True once the server confirmed a clean logout — the socket may now be dropped without a
    /// link-death ghost.
    pub fn logged_out(&self) -> bool {
        self.logged_out
    }

    /// Send one position/heading update (a single movement tick; the real client sends these
    /// ~every 200 ms). Position persists server-side across logout.
    pub fn position_update(
        &mut self,
        x: f32,
        y: f32,
        z: f32,
        speed: f32,
        health_pct: u8,
    ) -> io::Result<()> {
        let a = self.session.position_update(x, y, z, speed, health_pct);
        self.pump(a)
    }

    /// Set the facing (DAoC heading units, 0..4096) sent by the next [`position_update`]. Callers
    /// driving movement set this from the travel direction so the character faces where it walks.
    ///
    /// [`position_update`]: Self::position_update
    pub fn set_heading(&mut self, heading: u16) {
        self.session.set_heading(heading);
    }

    /// The character we chose to play (slot + name), once the overview arrived. `None` before the
    /// overview or when the account/realm had no playable character.
    pub fn chosen_character(&self) -> Option<&(u8, String)> {
        self.chosen.as_ref()
    }

    /// True once the server confirmed world entry (`CharacterInitFinished`).
    pub fn in_world(&self) -> bool {
        self.entered
    }

    /// True once an overview arrived with no character to play — a caller can stop rather than
    /// wait for a world entry that will never come.
    pub fn stuck_at_char_select(&self) -> bool {
        self.stuck_at_char_select
    }

    /// The current session phase (observability).
    pub fn phase(&self) -> SessionPhase {
        self.session.phase()
    }

    /// The server-assigned session id (0 until assigned).
    pub fn session_id(&self) -> u16 {
        self.session.session_id()
    }

    /// Send every `Action::Send` in `actions` (dropping `Action::Event`s — those are for callers
    /// that decode the stream, not for the pump path).
    fn pump(&mut self, actions: Vec<Action>) -> io::Result<()> {
        for a in actions {
            if let Action::Send(bytes) = a {
                self.write_frame(&bytes)?;
            }
        }
        Ok(())
    }

    /// Write one framed packet to the socket.
    fn write_frame(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.sock.write_all(bytes)?;
        self.sock.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_preworld_phases_require_keepalive() {
        assert!(!phase_needs_keepalive(SessionPhase::Disconnected));
        assert!(!phase_needs_keepalive(SessionPhase::CryptHandshake));
        assert!(!phase_needs_keepalive(SessionPhase::Authenticating));
        assert!(phase_needs_keepalive(SessionPhase::RealmSelect));
        assert!(phase_needs_keepalive(SessionPhase::CharacterSelect));
        assert!(phase_needs_keepalive(SessionPhase::EnteringWorld));
        assert!(phase_needs_keepalive(SessionPhase::InWorld));
        assert!(!phase_needs_keepalive(SessionPhase::Closed));
    }
}
