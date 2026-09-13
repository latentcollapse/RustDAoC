//! Live world feed: run a real DoL session on a background thread and stream its decoded
//! [`ServerEvent`]s to the render thread, which applies them to the `WorldState` it already draws.
//!
//! This is the convergence of the two halves built in parallel: the headless protocol client
//! ([`caer_client::LiveSession`]) and the renderer. The renderer stays a pure consumer — it reads
//! events off a channel and calls `WorldState::apply`; it never reaches into the protocol machine,
//! and no renderer type crosses back down the wire (the project's hard layering rule).
//!
//! Threading: `LiveSession::poll` blocks up to its read timeout, so it lives on its own thread and
//! `send`s each event down a **bounded** channel. The render loop drains the channel non-blocking
//! ([`drain_into`]) once per frame — a slow frame just batches more events, never blocks the GPU.
//!
//! System 2 / Sol finding 9: the supervisor is cancellable, exposes an explicit session state,
//! joins on Drop, and surfaces command-send failures instead of discarding them.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use caer_client::{Closed, Config, LiveSession};
use caer_protocol::session::ServerEvent;
use caer_world::WorldState;

/// Bound on the event channel (session → render). Preserves ordering under burst; applies
/// backpressure to the session thread when the render side falls behind.
pub const EVENT_CHANNEL_CAP: usize = 8192;
/// Bound on the command channel (render → session).
pub const CMD_CHANNEL_CAP: usize = 256;

/// Explicit live-session supervisor state (Sol finding 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LiveSessionState {
    Connecting = 0,
    Connected = 1,
    /// rustdaoc has dropped the dead feed and will [`spawn`] again. The supervisor itself
    /// still lands in [`Ended`] on socket death; the product loop owns retry.
    Reconnecting = 2,
    Ended = 3,
}

impl LiveSessionState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Connecting,
            1 => Self::Connected,
            2 => Self::Reconnecting,
            _ => Self::Ended,
        }
    }
}

/// Delay between dropping a dead feed and spawning a replacement (product reconnect loop).
pub const LIVE_RECONNECT_DELAY: Duration = Duration::from_millis(500);

/// What rustdaoc should do after an unexpected live-session end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectPlan {
    Idle,
    Schedule { delay: Duration },
    SpawnNow,
    GiveUp,
}

/// Clean logout / `/quit` does not reconnect. TCP/session death retries until `attempts_left` is 0.
#[must_use]
pub fn reconnect_plan(
    unexpected_end: bool,
    quitting: bool,
    attempts_left: u32,
    scheduled: Option<Instant>,
    now: Instant,
) -> ReconnectPlan {
    if quitting {
        return ReconnectPlan::GiveUp;
    }
    if let Some(at) = scheduled {
        if now >= at {
            return ReconnectPlan::SpawnNow;
        }
        return ReconnectPlan::Idle;
    }
    if !unexpected_end {
        return ReconnectPlan::Idle;
    }
    if attempts_left == 0 {
        return ReconnectPlan::GiveUp;
    }
    ReconnectPlan::Schedule {
        delay: LIVE_RECONNECT_DELAY,
    }
}

/// Default 3 retries. `CAER_LIVE_RECONNECT_ATTEMPTS=0` restores banner-only (no respawn).
#[must_use]
pub fn live_reconnect_attempts_from_env() -> u32 {
    std::env::var("CAER_LIVE_RECONNECT_ATTEMPTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3)
}

/// Why a [`LiveFeed::send`] failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveSendError {
    /// Session thread has ended or the command channel was closed.
    Ended,
    /// Command channel is full (caller should back off; never silently drop).
    Full,
}

impl fmt::Display for LiveSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ended => write!(f, "live session ended"),
            Self::Full => write!(f, "live command channel full"),
        }
    }
}

impl std::error::Error for LiveSendError {}

/// A command sent FROM a driver (the player client's input loop, or a future bot brain) INTO the
/// live session thread — the "bot-API seam". Events flow out on [`LiveFeed::rx`]; commands flow in
/// here. Keeping both directions on channels means the session thread owns the socket exclusively
/// (no shared-mutable state) and a reloading brain can never drop the live connection.
#[derive(Debug, Clone)]
pub enum LiveCommand {
    /// Move to this world position, facing `heading` (DAoC 0..4096), travelling at `speed` u/s.
    /// The driver integrates the position locally each frame (client-authoritative) and sends one
    /// of these on the wire cadence (~200 ms) so the server + other observers track the character.
    Move {
        x: f32,
        y: f32,
        z: f32,
        heading: u16,
        speed: f32,
        health_pct: u8,
        motion: caer_protocol::session::PlayerMotion,
    },
    /// A slash command without the leading `/` (e.g. `"say hello"`, `"stick"`).
    Command(String),
    /// Say a line to nearby players (the on-screen proof).
    Say(String),
    /// Set the server-side target (`0` clears). Sent when the player's selection changes.
    Target(u16),
    /// Use a skill/style/spell by its INDEX in the server's usable-skill list, with the position
    /// the request is made from (the server reads both).
    UseSkill {
        index: u8,
        skill_type: u8,
        x: f32,
        y: f32,
        z: f32,
    },
    /// Start or stop melee attack mode (`PlayerAttackRequest` 0x74).
    Attack { start: bool },
    /// Ask the server to (re)send an entity's create packet (`CreateNPCRequest` 0xBE).
    RequestNpc { object_id: u16 },
    /// Request a clean logout (`/quit`): the session thread sends it, keeps running until the
    /// server confirms (Quit 0xA4 → the thread ends cleanly, no link-death ghost). The driver
    /// should stop moving before sending this (a quit-timer server rejects a moving quit).
    Quit,
    /// SCN-01: choose a character by realm-local slot. Only used when the live session was started
    /// with `auto_select = false` (player UI path). The wire byte is exactly `slot`.
    SelectCharacter { slot: u8 },
    /// Char-create Continué: submit CharacterCreateRequest 0xFF with a filled draft.
    CreateCharacter {
        draft: caer_protocol::charcreate::CharacterCreateDraft,
    },
    /// Char-select Delete, confirmed: the same CharacterCreateRequest 0xFF with `CreateOp::Delete`
    /// and an empty name. There is no dedicated delete packet — see `Session::delete_character`.
    DeleteCharacter { realm: u8, slot: u8 },
    /// Pre-world realm pick: re-fire CharacterOverviewRequest 0xFC with the new realm byte so the
    /// session's char list matches the screen (not just the local create draft).
    RequestCharacterOverview { realm: u8 },
    /// System 4 verb 1: PlayerMoveItem 0xDD (equip = backpack → worn / paperdoll).
    MoveItem {
        to_slot: u16,
        from_slot: u16,
        count: u16,
    },
    /// ObjectInteractRequest 0x7A (merchant open / use).
    Interact {
        player_x: u32,
        player_y: u32,
        target_oid: u16,
    },
    /// System 4 verb 2: BuyRequest 0x78 (targeted merchant).
    BuyItem {
        player_x: u32,
        player_y: u32,
        merchant_id: u16,
        item_slot: u16,
        item_count: u8,
    },
    /// SellRequest 0x79. Does **not** remove the item or add money locally.
    SellItem {
        player_x: u32,
        player_y: u32,
        merchant_id: u16,
        item_slot: u16,
    },
    /// UseSlot 0x71 (1.124+ layout). Does **not** consume or equip locally.
    UseSlot {
        x: f32,
        y: f32,
        z: f32,
        speed: f32,
        heading: u16,
        flag_speed_data: u16,
        slot: u8,
        use_type: u8,
    },
    /// CraftRequest 0xED. Does **not** invent a crafted item locally.
    CraftItem { item_id: u16 },
    /// DestroyItemRequest 0x80. Does **not** remove the item locally.
    DestroyItem { slot: u16 },
    /// TrainWindowHandler 0x7B. Does **not** invent spec levels.
    TrainWindow,
    /// TrainRequest 0x7C. Does **not** award spec levels locally.
    TrainRequest {
        player_x: u32,
        player_y: u32,
        id_line: u8,
        unk: u8,
        row: u8,
        skill_index: u8,
    },
    /// SiegeCommandRequest 0xF5. Does **not** invent siege state locally.
    SiegeCommand { action: u8, ammo: u8 },
    /// PlayerSitRequest 0xC7. Does **not** invent a local sit pose.
    Sit { sit: bool },
    /// DialogResponse 0x82 — accept/refuse group invite, quest, or CustomDialog.
    DialogResponse {
        data1: u16,
        data2: u16,
        data3: u16,
        message_type: u8,
        response: u8,
    },
    /// ModifyTrade 0xEB — cancel / update / accept an open trade window.
    ModifyTrade {
        action: caer_protocol::social::ModifyTradeAction,
        repair: bool,
        combine: bool,
        slots: [u8; 10],
        money: caer_protocol::social::TradeMoney,
    },
    /// InviteToGroup 0x87 — requires a prior [`LiveCommand::Target`].
    InviteToGroup,
    /// CheckLOSResponse 0xD0 — answer a server CheckLOSRequest.
    CheckLosResponse {
        checker_oid: u16,
        target_oid: u16,
        response: u16,
    },
}

/// A handle to the background live session: the event channel out, the command channel in, plus
/// the thread that drives them. Dropping the feed **cancels, disconnects the event receiver, and
/// joins** the supervisor thread — in that order. Dropping `rx` before join is load-bearing: a
/// worker blocked in `sync_channel::send` on a saturated queue cannot observe cancel while the
/// receiver is still alive (Sol Drop-deadlock finding).
pub struct LiveFeed {
    /// Decoded server events. Taken to `None` in [`Drop`] before join; use [`Self::events`].
    rx: Option<Receiver<ServerEvent>>,
    /// Commands into the session thread. Use [`LiveFeed::send`] rather than touching this directly.
    cmd_tx: Option<SyncSender<LiveCommand>>,
    state: Arc<AtomicU8>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    /// Commands accepted by [`Self::send`] that the session thread has not yet acted on.
    ///
    /// A bounded channel cannot be asked how deep it is, so this counts. It exists because
    /// **"the command was queued" and "the packet went out" are different facts**, and the pre-world
    /// Quit conflated them: it queued `LiveCommand::Quit`, set the exit flag in the same turn, and
    /// the process could leave before the session thread ever popped it — a graceful logout that
    /// silently sent nothing. Making the difference observable is what lets a test see it.
    queued: Arc<AtomicUsize>,
}

impl LiveFeed {
    /// Event receiver for [`drain_into`]. Panics only if called after Drop has begun teardown.
    #[must_use]
    pub fn events(&self) -> &Receiver<ServerEvent> {
        self.rx
            .as_ref()
            .expect("LiveFeed::events after Drop took the receiver")
    }

    /// Current supervisor state. Drivers must treat [`LiveSessionState::Ended`] as an explicit
    /// error/reconnect surface — never leave a frozen world without noticing.
    #[must_use]
    pub fn state(&self) -> LiveSessionState {
        LiveSessionState::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Request cooperative cancellation. Drop also cancels; this is for an explicit UI abort.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    /// Queue a command for the live session thread. Surfaces channel failures — never silently
    /// discards them (System 2 / Sol finding 9).
    /// How many accepted commands the session thread has not yet acted on.
    ///
    /// Zero means every command handed to [`Self::send`] has been popped and executed — which is the
    /// condition a graceful quit must wait for before the process may leave.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.queued.load(Ordering::SeqCst)
    }

    pub fn send(&self, cmd: LiveCommand) -> Result<(), LiveSendError> {
        let Some(tx) = self.cmd_tx.as_ref() else {
            return Err(LiveSendError::Ended);
        };
        match tx.try_send(cmd) {
            Ok(()) => {
                self.queued.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
            Err(TrySendError::Disconnected(_)) => Err(LiveSendError::Ended),
            Err(TrySendError::Full(_)) => Err(LiveSendError::Full),
        }
    }
}

/// Holds a command's place in [`LiveFeed::queued`] until the session thread is done with it.
///
/// A guard rather than a `fetch_sub` after the dispatch, because the ordering is the whole point and
/// a bare statement can be moved above the work by anyone tidying the function. `queued() == 0` has
/// to mean **acted on**, not **popped off the channel**: a quit that waits on the weaker reading can
/// still exit before its packet is written, which is the defect this counter exists to expose.
/// Dropping on the way out also covers the early `return` when a write fails, so a failed command is
/// not counted as still pending forever.
struct QueuedCommand(Arc<AtomicUsize>);

impl Drop for QueuedCommand {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for LiveFeed {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        // Close the command channel so the session thread's next recv wakes promptly.
        self.cmd_tx.take();
        // Disconnect the event channel *before* join. A worker blocked in sync_channel::send on a
        // full queue cannot make progress while `rx` is still alive — cancel alone is insufficient.
        drop(self.rx.take());
        if let Some(handle) = self.handle.take() {
            // After rx disconnect + cancel, the supervisor should exit on the next send/poll tick.
            // Park a join watcher with a hard deadline so a stuck connect cannot hang the UI forever.
            let (done_tx, done_rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = handle.join();
                let _ = done_tx.send(());
            });
            match done_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                Ok(()) => {}
                Err(_) => {
                    log::error!(
                        "caer-render: LiveFeed::drop — supervisor join exceeded 5s; \
                         thread may be stuck in connect/write (timeouts should prevent this)"
                    );
                }
            }
        }
    }
}

fn set_state(state: &AtomicU8, next: LiveSessionState) {
    state.store(next as u8, Ordering::Release);
}

/// Connect to `server` as `account`/`password` (optionally choosing `character` by name) and
/// stream the live session's events. Errors (connect/decode/close) end the thread in
/// [`LiveSessionState::Ended`]; the render loop observes via [`LiveFeed::state`] / drain `ended`.
///
/// `auto_select`: headless bots pass `true` (legacy). The player client (`rustdaoc`) passes
/// `false` so character selection goes through [`LiveCommand::SelectCharacter`] (SCN-01).
pub fn spawn(
    server: String,
    account: String,
    password: String,
    character: Option<String>,
    auto_select: bool,
) -> LiveFeed {
    let (tx, rx) = mpsc::sync_channel(EVENT_CHANNEL_CAP);
    let (cmd_tx, cmd_rx) = mpsc::sync_channel::<LiveCommand>(CMD_CHANNEL_CAP);
    let state = Arc::new(AtomicU8::new(LiveSessionState::Connecting as u8));
    let cancel = Arc::new(AtomicBool::new(false));
    let queued = Arc::new(AtomicUsize::new(0));
    let state_t = Arc::clone(&state);
    let cancel_t = Arc::clone(&cancel);
    let queued_t = Arc::clone(&queued);

    let handle = thread::spawn(move || {
        let end = |s: &AtomicU8| set_state(s, LiveSessionState::Ended);

        let mut cfg = Config::new(server, account, password);
        cfg.character = character;
        cfg.auto_select = auto_select;
        let mut sess = match LiveSession::connect(&cfg) {
            Ok(s) => s,
            Err(e) => {
                log::info!("caer-render: live connect failed: {e}");
                end(&state_t);
                return;
            }
        };
        if cancel_t.load(Ordering::Acquire) {
            end(&state_t);
            return;
        }
        set_state(&state_t, LiveSessionState::Connected);
        log::info!(
            "caer-render: live session connected — streaming world events (auto_select={auto_select})"
        );

        loop {
            if cancel_t.load(Ordering::Acquire) {
                log::info!("caer-render: live session cancelled");
                end(&state_t);
                return;
            }

            // Drain queued driver commands. `poll` blocks on the socket; commands wait at most one
            // read-timeout before the next drain. Disconnected means the feed was dropped.
            loop {
                match cmd_rx.try_recv() {
                    Ok(cmd) => {
                        // Held until this command has been acted on — see `QueuedCommand`.
                        let _acting = QueuedCommand(Arc::clone(&queued_t));
                        let r = match cmd {
                            LiveCommand::Move {
                                x,
                                y,
                                z,
                                heading,
                                speed,
                                health_pct,
                                motion,
                            } => {
                                sess.set_heading(heading);
                                sess.position_update_full(x, y, z, speed, health_pct, motion)
                            }
                            LiveCommand::Command(c) => sess.command(&c),
                            LiveCommand::Say(m) => sess.say(&m),
                            LiveCommand::Target(oid) => sess.target(oid),
                            LiveCommand::UseSkill {
                                index,
                                skill_type,
                                x,
                                y,
                                z,
                            } => sess.use_skill(index, skill_type, x, y, z),
                            LiveCommand::Attack { start } => sess.attack(start),
                            LiveCommand::RequestNpc { object_id } => sess.request_npc(object_id),
                            LiveCommand::Quit => sess.quit(),
                            LiveCommand::SelectCharacter { slot } => sess.select_character(slot),
                            LiveCommand::CreateCharacter { draft } => sess.create_character(&draft),
                            LiveCommand::DeleteCharacter { realm, slot } => {
                                sess.delete_character(realm, slot)
                            }
                            LiveCommand::RequestCharacterOverview { realm } => {
                                sess.request_character_overview(realm)
                            }
                            LiveCommand::MoveItem {
                                to_slot,
                                from_slot,
                                count,
                            } => sess.move_item(to_slot, from_slot, count),
                            LiveCommand::Interact {
                                player_x,
                                player_y,
                                target_oid,
                            } => sess.interact(player_x, player_y, target_oid),
                            LiveCommand::BuyItem {
                                player_x,
                                player_y,
                                merchant_id,
                                item_slot,
                                item_count,
                            } => sess.buy_item(
                                player_x,
                                player_y,
                                merchant_id,
                                item_slot,
                                item_count,
                            ),
                            LiveCommand::SellItem {
                                player_x,
                                player_y,
                                merchant_id,
                                item_slot,
                            } => sess.sell_item(player_x, player_y, merchant_id, item_slot),
                            LiveCommand::UseSlot {
                                x,
                                y,
                                z,
                                speed,
                                heading,
                                flag_speed_data,
                                slot,
                                use_type,
                            } => sess.use_slot(
                                x,
                                y,
                                z,
                                speed,
                                heading,
                                flag_speed_data,
                                slot,
                                use_type,
                            ),
                            LiveCommand::CraftItem { item_id } => sess.craft_item(item_id),
                            LiveCommand::DestroyItem { slot } => sess.destroy_item(slot),
                            LiveCommand::TrainWindow => sess.train_window(),
                            LiveCommand::TrainRequest {
                                player_x,
                                player_y,
                                id_line,
                                unk,
                                row,
                                skill_index,
                            } => sess.train_request(
                                player_x,
                                player_y,
                                id_line,
                                unk,
                                row,
                                skill_index,
                            ),
                            LiveCommand::SiegeCommand { action, ammo } => {
                                sess.siege_command(action, ammo)
                            }
                            LiveCommand::Sit { sit } => sess.sit(sit),
                            LiveCommand::DialogResponse {
                                data1,
                                data2,
                                data3,
                                message_type,
                                response,
                            } => sess.dialog_response(data1, data2, data3, message_type, response),
                            LiveCommand::ModifyTrade {
                                action,
                                repair,
                                combine,
                                slots,
                                money,
                            } => sess.modify_trade(action, repair, combine, &slots, money),
                            LiveCommand::InviteToGroup => sess.invite_to_group(),
                            LiveCommand::CheckLosResponse {
                                checker_oid,
                                target_oid,
                                response,
                            } => sess.check_los_response(checker_oid, target_oid, response),
                        };
                        if let Err(e) = r {
                            log::info!("caer-render: live command send failed: {e}");
                            end(&state_t);
                            return;
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        end(&state_t);
                        return;
                    }
                }
            }

            match sess.poll() {
                Ok(events) => {
                    for ev in events {
                        // Check cancel before blocking send — Drop sets cancel then drops rx;
                        // without this check a cancel-during-full-queue still enters send first.
                        if cancel_t.load(Ordering::Acquire) {
                            end(&state_t);
                            return;
                        }
                        // Blocking send preserves ordering under burst. Drop disconnects `rx`
                        // *before* join so a saturated send unblocks with Disconnected.
                        if tx.send(ev).is_err() {
                            end(&state_t);
                            return;
                        }
                    }
                }
                Err(Closed(reason)) => {
                    log::info!("caer-render: live session ended: {reason}");
                    end(&state_t);
                    return;
                }
            }
            // If the server confirmed a clean logout, end the thread now — dropping the socket
            // here (after the Quit packet) is a graceful close, not a link-death.
            if sess.logged_out() {
                log::info!("caer-render: logged out cleanly — closing session");
                end(&state_t);
                return;
            }
        }
    });

    LiveFeed {
        rx: Some(rx),
        cmd_tx: Some(cmd_tx),
        state,
        cancel,
        handle: Some(handle),
        queued,
    }
}

/// What one [`drain_into`] pass observed: how many events applied, the latest player world position
/// if a `PlayerPosition` arrived (to re-centre the origin/camera), and whether the session thread
/// has ended (the channel disconnected — a clean logout or a dropped connection). A driver waiting
/// on a `/quit` watches `ended` to know the socket closed and it can exit.
pub struct Drain {
    pub applied: usize,
    pub player: Option<[f32; 3]>,
    pub ended: bool,
    /// Our own object id, if a spawn packet arrived this tick. Position updates must echo it from
    /// client 1.127 onward.
    pub self_object_id: Option<u16>,
    /// System/chat messages (0xAF) seen this tick, as `(chat_type, text)` in arrival order.
    ///
    /// Surfaced here rather than folded into `WorldState` because they are *events*, not state:
    /// the world model holds what currently exists, while a message is a thing that happened once
    /// and then scrolls away. The combat log and floating damage text consume these.
    pub messages: Vec<(u8, String)>,
    /// CombatAnimation 0xBC events this tick (REQ-021 / SCN-05 structured path).
    pub combat_anims: Vec<caer_protocol::combat_anim::CombatAnimation>,
    /// SpellCastAnimation 0x72 this tick (System 8 cast-start audio).
    pub spell_casts: Vec<caer_protocol::spells::SpellCastAnimation>,
    /// SpellEffectAnimation 0x1B this tick (System 8 effect-land audio).
    pub spell_effects: Vec<caer_protocol::spells::SpellEffectAnimation>,
    /// Character overview from this drain (char-select UI). Not applied to `WorldState`.
    pub overview: Option<caer_protocol::overview::CharacterOverview>,
    /// Highest-priority session-phase hint inferred from events this drain (pre-world UI drive).
    pub phase: Option<caer_protocol::session::SessionPhase>,
    /// Region skin id if a RegionChanged 0xB7 arrived this tick (terrain reload trigger).
    pub region_changed: Option<u16>,
    /// Object ids removed this tick (`ObjectRemoved`) — request/target cache invalidation (S1).
    pub removed_ids: Vec<u16>,
    /// True when a `LoggedOut` event arrived this tick (session teardown / reconnect scrub).
    pub logged_out: bool,
    /// Dialog boxes (0x81) this tick — group invite / quest / CustomDialog for social UI.
    pub dialogs: Vec<SocialDialog>,
    /// TradeWindow 0xEA open/update/close this tick.
    pub trade_windows: Vec<caer_protocol::social::TradeWindow>,
    /// GroupWindow 0x16:0x06 this tick — authoritative roster for social UI.
    pub group_windows: Vec<caer_protocol::social::GroupWindow>,
    /// GroupMemberUpdate 0x70 this tick — roster vitals, not Dialog Yes.
    pub group_member_updates: Vec<caer_protocol::social::GroupMemberUpdate>,
    /// QuestEntry 0x83 this tick.
    pub quest_entries: Vec<caer_protocol::quest::QuestEntry>,
    /// InventoryUpdate 0x02 this tick (trade-complete presentation gate).
    pub inventory_updated: bool,
    /// MoneyUpdate 0xFA this tick (trade-complete presentation gate).
    pub money_updated: bool,
    /// `ServerEvent::EnteredWorld` this tick (addon `zone.entered_world`).
    pub entered_world: bool,
    /// `ServerEvent::LoginGranted` this tick (addon `session.login_granted`).
    pub login_granted: bool,
    /// `ServerEvent::CryptKeyReceived` this tick (addon `session.crypt_key_received`).
    pub crypt_key_received: bool,
    /// LoginDenied 0x2C this tick (auth failure; surface to player, do not invent grant).
    pub login_denied: Option<u8>,
    /// AttackMode 0x74 authoritative stance this tick.
    pub attack_mode: Option<bool>,
    /// MaxSpeed 0xB6 percent this tick (`None` if absent).
    pub max_speed_percent: Option<u16>,
    /// ChangeTarget 0xF6 this tick (`None` if absent; `Some(0)` = clear).
    pub server_target_oid: Option<u16>,
    /// UDPInitReply 0x2F this tick.
    pub udp_init: Option<caer_protocol::view_control::UdpInitReply>,
    /// CheckLOSRequest 0xD0 this tick — product must answer CheckLOSResponse.
    pub pending_los: Option<caer_protocol::shape2_loop::CheckLosRequest>,
    /// BadNameCheckReply 0xC3 this tick.
    pub name_check_bad: Option<caer_protocol::shape2_loop::BadNameCheckReply>,
    /// DupNameCheckReply 0xCC this tick.
    pub name_check_dup: Option<caer_protocol::shape2_loop::DupNameCheckReply>,
    /// CharacterCreateReply 0xF0 this tick.
    pub create_reply: Option<caer_protocol::shape2_loop::CharacterCreateReply>,
    /// DelveInfo 0xD8 this tick.
    pub delve: Option<caer_protocol::shape2_loop::DelveInfo>,
}

/// Dialog payload surfaced to the player social overlay (not folded into WorldState).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocialDialog {
    pub code: u8,
    pub data1: u16,
    pub data2: u16,
    pub data3: u16,
    pub data4: u16,
    pub message: String,
}

/// Apply every event currently waiting on `rx` to `world` (non-blocking).
pub fn drain_into(rx: &Receiver<ServerEvent>, world: &mut WorldState) -> Drain {
    use caer_protocol::session::SessionPhase;
    use std::sync::mpsc::TryRecvError;
    let mut applied = 0usize;
    let mut player: Option<[f32; 3]> = None;
    let mut ended = false;
    let mut messages = Vec::new();
    let mut combat_anims = Vec::new();
    let mut spell_casts = Vec::new();
    let mut spell_effects = Vec::new();
    let mut self_object_id = None;
    let mut overview = None;
    let mut phase = None;
    let mut region_changed = None;
    let mut removed_ids = Vec::new();
    let mut logged_out = false;
    let mut dialogs = Vec::new();
    let mut trade_windows = Vec::new();
    let mut group_windows = Vec::new();
    let mut group_member_updates = Vec::new();
    let mut quest_entries = Vec::new();
    let mut inventory_updated = false;
    let mut money_updated = false;
    let mut entered_world = false;
    let mut login_granted = false;
    let mut crypt_key_received = false;
    let mut login_denied = None;
    let mut attack_mode = None;
    let mut max_speed_percent = None;
    let mut server_target_oid = None;
    let mut udp_init = None;
    let mut pending_los = None;
    let mut name_check_bad = None;
    let mut name_check_dup = None;
    let mut create_reply = None;
    let mut delve = None;
    loop {
        match rx.try_recv() {
            Ok(ev) => {
                if let ServerEvent::PlayerPosition {
                    x, y, z, object_id, ..
                } = &ev
                {
                    player = Some([*x, *y, *z]);
                    self_object_id = Some(*object_id);
                }
                if let ServerEvent::ChatMessage { chat_type, text } = &ev {
                    messages.push((*chat_type, text.clone()));
                }
                if let ServerEvent::CombatAnimation(a) = &ev {
                    combat_anims.push(a.clone());
                }
                if let ServerEvent::SpellCast(c) = &ev {
                    spell_casts.push(*c);
                }
                if let ServerEvent::SpellEffect(e) = &ev {
                    spell_effects.push(*e);
                }
                if let ServerEvent::CharacterOverview(ov) = &ev {
                    overview = Some(ov.clone());
                    phase = Some(bump_phase(phase, SessionPhase::CharacterSelect));
                }
                if let ServerEvent::RegionChanged(r) = &ev {
                    region_changed = Some(r.region_id);
                }
                if let ServerEvent::ObjectRemoved { object_id } = &ev {
                    removed_ids.push(*object_id);
                }
                if let ServerEvent::LoggedOut { .. } = &ev {
                    logged_out = true;
                }
                if let ServerEvent::Dialog {
                    code,
                    data1,
                    data2,
                    data3,
                    data4,
                    message,
                } = &ev
                {
                    dialogs.push(SocialDialog {
                        code: *code,
                        data1: *data1,
                        data2: *data2,
                        data3: *data3,
                        data4: *data4,
                        message: message.clone(),
                    });
                }
                if let ServerEvent::TradeWindow(tw) = &ev {
                    trade_windows.push(tw.clone());
                }
                if let ServerEvent::GroupWindow(gw) = &ev {
                    group_windows.push(gw.clone());
                }
                if let ServerEvent::GroupMemberUpdate(g) = &ev {
                    group_member_updates.push(g.clone());
                }
                if let ServerEvent::QuestEntry { entry, .. } = &ev {
                    quest_entries.push(entry.clone());
                }
                if let ServerEvent::InventoryUpdated(_) = &ev {
                    inventory_updated = true;
                }
                if let ServerEvent::MoneyUpdated(_) = &ev {
                    money_updated = true;
                }
                match &ev {
                    ServerEvent::CryptKeyReceived => {
                        crypt_key_received = true;
                        phase = Some(bump_phase(phase, SessionPhase::CryptHandshake));
                    }
                    ServerEvent::LoginGranted => {
                        login_granted = true;
                        // Mirrors SessionState: grant lands at RealmSelect until overview.
                        phase = Some(bump_phase(phase, SessionPhase::RealmSelect));
                    }
                    ServerEvent::Realm { realm } => {
                        if *realm == 0 {
                            phase = Some(bump_phase(phase, SessionPhase::RealmSelect));
                        }
                        // Bound realm stays RealmSelect until CharacterOverview arrives.
                    }
                    ServerEvent::LoginDenied { error } => {
                        login_denied = Some(*error);
                        phase = Some(bump_phase(phase, SessionPhase::Closed));
                    }
                    ServerEvent::AttackMode { attacking } => {
                        attack_mode = Some(*attacking);
                    }
                    ServerEvent::MaxSpeed { percent, .. } => {
                        max_speed_percent = Some(*percent);
                    }
                    ServerEvent::TargetChanged(t) => {
                        server_target_oid = Some(t.object_id);
                    }
                    ServerEvent::UdpInitReply(u) => {
                        udp_init = Some(u.clone());
                    }
                    ServerEvent::CheckLosRequest(r) => {
                        pending_los = Some(*r);
                    }
                    ServerEvent::BadNameCheckReply(r) => {
                        name_check_bad = Some(r.clone());
                    }
                    ServerEvent::DupNameCheckReply(r) => {
                        name_check_dup = Some(r.clone());
                    }
                    ServerEvent::CharacterCreateReply(r) => {
                        create_reply = Some(r.clone());
                    }
                    ServerEvent::DelveInfo(d) => {
                        delve = Some(d.clone());
                    }
                    ServerEvent::RemoveObject(o) => {
                        removed_ids.push(o.object_id);
                    }
                    ServerEvent::RegionHandoff { .. } | ServerEvent::PlayerPosition { .. } => {
                        phase = Some(bump_phase(phase, SessionPhase::EnteringWorld));
                    }
                    ServerEvent::EnteredWorld => {
                        entered_world = true;
                        phase = Some(bump_phase(phase, SessionPhase::InWorld));
                    }
                    _ => {}
                }
                if !matches!(&ev, ServerEvent::Raw { .. })
                    && !caer_protocol::coverage::is_product_consumer(ev.coverage_name())
                {
                    caer_protocol::packet_telemetry::record_unconsumed(ev.coverage_name());
                }
                world.apply(&ev);
                applied += 1;
            }
            Err(TryRecvError::Empty) => break, // nothing more this tick
            Err(TryRecvError::Disconnected) => {
                ended = true;
                break;
            } // session thread gone
        }
    }
    Drain {
        applied,
        player,
        ended,
        messages,
        combat_anims,
        spell_casts,
        spell_effects,
        self_object_id,
        overview,
        phase,
        region_changed,
        removed_ids,
        logged_out,
        dialogs,
        trade_windows,
        group_windows,
        group_member_updates,
        quest_entries,
        inventory_updated,
        money_updated,
        entered_world,
        login_granted,
        crypt_key_received,
        login_denied,
        attack_mode,
        max_speed_percent,
        server_target_oid,
        udp_init,
        pending_los,
        name_check_bad,
        name_check_dup,
        create_reply,
        delve,
    }
}

/// Keep the most-advanced phase seen in a single drain (InWorld beats EnteringWorld, etc.).
fn bump_phase(
    cur: Option<caer_protocol::session::SessionPhase>,
    next: caer_protocol::session::SessionPhase,
) -> caer_protocol::session::SessionPhase {
    use caer_protocol::session::SessionPhase;
    let rank = |p: SessionPhase| -> u8 {
        match p {
            SessionPhase::Disconnected => 0,
            SessionPhase::CryptHandshake => 1,
            SessionPhase::Authenticating => 2,
            SessionPhase::RealmSelect => 3,
            SessionPhase::CharacterSelect => 4,
            SessionPhase::EnteringWorld => 5,
            SessionPhase::InWorld => 6,
            SessionPhase::Closed => 7,
        }
    };
    match cur {
        Some(prev) if rank(prev) > rank(next) => prev,
        _ => next,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Sol falsifier: capacity-1 queue, worker blocked in `sync_channel::send`, Drop must
    /// disconnect `rx` before join. Hard 2s deadline — the pre-fix ownership order exited 124
    /// under `timeout 2s`.
    #[test]
    fn saturated_send_drop_unblocks_under_deadline() {
        let (tx, rx) = mpsc::sync_channel::<ServerEvent>(1);
        tx.send(ServerEvent::CryptKeyReceived).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Arc::new(AtomicU8::new(LiveSessionState::Connected as u8));
        let cancel_t = Arc::clone(&cancel);
        let state_t = Arc::clone(&state);
        let handle = thread::spawn(move || {
            loop {
                if cancel_t.load(Ordering::Acquire) {
                    // Cancel alone is insufficient while send is blocked and rx is alive.
                    // Progress requires rx disconnect (send → Disconnected).
                }
                if tx.send(ServerEvent::EnteredWorld).is_err() {
                    set_state(&state_t, LiveSessionState::Ended);
                    return;
                }
            }
        });
        // Real LiveFeed ownership — Drop order is the product under test.
        let feed = LiveFeed {
            queued: Arc::new(AtomicUsize::new(0)),
            rx: Some(rx),
            cmd_tx: None,
            state: Arc::clone(&state),
            cancel,
            handle: Some(handle),
        };
        thread::sleep(Duration::from_millis(50)); // ensure worker is blocked in send
        let (watch_tx, watch_rx) = mpsc::channel();
        thread::spawn(move || {
            let start = Instant::now();
            drop(feed);
            let _ = watch_tx.send(start.elapsed());
        });
        let elapsed = watch_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("LiveFeed::drop deadlocked under saturated sync_channel (Sol BLOCKER 1)");
        assert!(elapsed < Duration::from_secs(2), "Drop took {elapsed:?}");
        assert_eq!(
            LiveSessionState::from_u8(state.load(Ordering::Acquire)),
            LiveSessionState::Ended,
            "Ended must be terminal after Drop"
        );
    }

    /// **A queued command is not a sent packet**, and `queued()` is what tells them apart.
    ///
    /// The pre-world quit used to set its exit flag in the same turn it called `send`, then leave on
    /// the next event-loop tick. `send` is a `try_send` into a bounded channel: the session thread
    /// still has to pop the command and write it. So a graceful logout could exit before the packet
    /// existed, and **nothing could observe the difference** — which is why the counter exists.
    ///
    /// This asserts the property the quit path actually waits on — *a command nobody has acted on is
    /// never counted as sent* — with no worker at all, so there is no race to decide the result.
    ///
    /// **What it does not cover, stated rather than implied:** the ordering *inside* the production
    /// worker. An earlier version of this test re-implemented that worker and asserted the ordering
    /// against the copy; it stayed green when the copy was deliberately broken, because the pop had
    /// not necessarily happened when the assertion ran. Testing a re-implementation proves nothing
    /// about the original. The ordering is instead enforced by construction: [`QueuedCommand`] holds
    /// the count until it drops, so the decrement cannot be moved above the work.
    #[test]
    fn a_command_nobody_has_acted_on_is_never_counted_as_sent() {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<LiveCommand>(4);
        let (_tx, rx) = mpsc::sync_channel::<ServerEvent>(1);
        let queued = Arc::new(AtomicUsize::new(0));
        let feed = LiveFeed {
            queued: Arc::clone(&queued),
            rx: Some(rx),
            cmd_tx: Some(cmd_tx),
            state: Arc::new(AtomicU8::new(LiveSessionState::Connected as u8)),
            cancel: Arc::new(AtomicBool::new(false)),
            handle: None,
        };

        assert_eq!(feed.queued(), 0, "nothing sent yet");
        feed.send(LiveCommand::Quit).expect("accepted");
        assert_eq!(
            feed.queued(),
            1,
            "accepted into the channel — this is the state a quit must NOT exit in"
        );
        // No worker exists, so it stays pending. A quit polling this leaves on its deadline and says
        // so, rather than believing the packet went out.
        thread::sleep(Duration::from_millis(20));
        assert_eq!(feed.queued(), 1, "still nobody has acted on it");

        // The guard is what closes it, and it is the same type the session thread uses.
        {
            let _acting = QueuedCommand(Arc::clone(&queued));
            assert_eq!(feed.queued(), 1, "still held while acting");
        }
        assert_eq!(feed.queued(), 0, "released once the work is done");

        // The command is still in the channel and is dropped with it.
        assert!(cmd_rx.try_recv().is_ok());
        drop(feed);
    }

    /// The guard releases on the failure path too.
    ///
    /// A command whose write errors is *acted on* — badly, but acted on. Counting it as pending
    /// forever would make every later quit wait out its full deadline and report commands unsent
    /// that were merely unsuccessful.
    #[test]
    fn the_queue_guard_releases_even_when_the_command_fails() {
        let queued = Arc::new(AtomicUsize::new(0));
        queued.fetch_add(1, Ordering::SeqCst);
        let r: Result<(), &str> = (|| {
            let _acting = QueuedCommand(Arc::clone(&queued));
            Err("write failed")
        })();
        assert!(r.is_err());
        assert_eq!(
            queued.load(Ordering::SeqCst),
            0,
            "a failed command must not stay counted as pending"
        );
    }

    /// PLT boundedness: Drop disconnects the event sink then joins the worker.
    /// Does **not** prove a 6-hour soak (see `scripts/transplant_soak.sh`, gated).
    #[test]
    fn disconnect_path_joins_worker_and_drops_sink() {
        let (tx, rx) = mpsc::sync_channel::<ServerEvent>(4);
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Arc::new(AtomicU8::new(LiveSessionState::Connected as u8));
        let cancel_t = Arc::clone(&cancel);
        let state_t = Arc::clone(&state);
        let handle = thread::spawn(move || {
            while !cancel_t.load(Ordering::Acquire) {
                if tx.send(ServerEvent::CryptKeyReceived).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            set_state(&state_t, LiveSessionState::Ended);
        });
        let feed = LiveFeed {
            queued: Arc::new(AtomicUsize::new(0)),
            rx: Some(rx),
            cmd_tx: None,
            state: Arc::clone(&state),
            cancel,
            handle: Some(handle),
        };
        let start = Instant::now();
        drop(feed);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "disconnect Drop must join the worker promptly"
        );
        assert_eq!(
            LiveSessionState::from_u8(state.load(Ordering::Acquire)),
            LiveSessionState::Ended
        );
    }

    /// Failed-connect Drop must finish promptly (connect_timeout + join watcher), not hang.
    #[test]
    fn drop_during_failed_connect_finishes_under_deadline() {
        let feed = spawn(
            "127.0.0.1:1".into(),
            "caer".into(),
            "caer".into(),
            None,
            true,
        );
        let (watch_tx, watch_rx) = mpsc::channel();
        thread::spawn(move || {
            let start = Instant::now();
            drop(feed);
            let _ = watch_tx.send(start.elapsed());
        });
        let elapsed = watch_rx
            .recv_timeout(Duration::from_secs(8))
            .expect("Drop during connect hung past 8s");
        assert!(elapsed < Duration::from_secs(8), "Drop took {elapsed:?}");
    }

    /// After the session ends, further sends must surface an error (not vanish).
    #[test]
    fn send_after_session_end_surfaces_error() {
        let feed = spawn(
            "127.0.0.1:1".into(),
            "caer".into(),
            "caer".into(),
            None,
            true,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while feed.state() != LiveSessionState::Ended {
            assert!(
                Instant::now() < deadline,
                "supervisor never reached Ended after failed connect"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let err = feed
            .send(LiveCommand::Quit)
            .expect_err("send must surface after session end");
        assert_eq!(err, LiveSendError::Ended);
        drop(feed);
    }

    /// Closing the command channel (Drop takes cmd_tx) must leave state Ended after join.
    #[test]
    fn cancel_then_drop_reaches_ended() {
        let feed = spawn(
            "127.0.0.1:1".into(),
            "caer".into(),
            "caer".into(),
            None,
            true,
        );
        feed.cancel();
        let deadline = Instant::now() + Duration::from_secs(5);
        while feed.state() != LiveSessionState::Ended {
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        drop(feed);
    }

    /// Named falsifier: drain must surface ObjectRemoved ids for Client cache scrub.
    /// Delete `removed_ids.push` in [`drain_into`] → this fails.
    #[test]
    fn drain_surfaces_object_removed_ids() {
        let (tx, rx) = mpsc::channel();
        tx.send(ServerEvent::ObjectRemoved { object_id: 42 })
            .unwrap();
        tx.send(ServerEvent::ObjectRemoved { object_id: 99 })
            .unwrap();
        drop(tx);
        let mut world = WorldState::default();
        let d = drain_into(&rx, &mut world);
        assert_eq!(
            d.removed_ids,
            vec![42, 99],
            "ObjectRemoved must reach Drain.removed_ids for requested_npcs/target invalidation"
        );
        assert!(d.ended, "sender drop ends the drain");
    }

    /// Named falsifier: drain must surface LoggedOut for logout/reconnect scrub.
    /// Delete `logged_out = true` in [`drain_into`] → this fails.
    #[test]
    fn drain_surfaces_logged_out() {
        let (tx, rx) = mpsc::channel();
        tx.send(ServerEvent::LoggedOut {
            total_out: true,
            level: 50,
        })
        .unwrap();
        drop(tx);
        let mut world = WorldState::default();
        let d = drain_into(&rx, &mut world);
        assert!(
            d.logged_out,
            "LoggedOut must set Drain.logged_out so Client clears request/target caches"
        );
    }

    /// Named falsifier: RegionChanged remains on Drain (Client clears caches from this flag).
    #[test]
    fn drain_surfaces_region_changed() {
        let (tx, rx) = mpsc::channel();
        tx.send(ServerEvent::RegionChanged(
            caer_protocol::region::RegionChanged {
                region_id: 51,
                zone_skin_id: 0,
                cause: 1,
                server_id: 0x0C,
            },
        ))
        .unwrap();
        drop(tx);
        let mut world = WorldState::default();
        let d = drain_into(&rx, &mut world);
        assert_eq!(d.region_changed, Some(51));
    }

    /// Named falsifier: GroupWindow 0x16:0x06 must leave Drain, not vanish into WorldState.
    /// Delete `group_windows.push` in [`drain_into`] → this fails.
    #[test]
    fn drain_surfaces_group_window() {
        let (tx, rx) = mpsc::channel();
        tx.send(ServerEvent::GroupWindow(
            caer_protocol::social::GroupWindow {
                members: vec![caer_protocol::social::GroupWindowMember {
                    name: "Feile".into(),
                    salutation: String::new(),
                    object_id: 12,
                    level: 50,
                }],
            },
        ))
        .unwrap();
        tx.send(ServerEvent::GroupMemberUpdate(
            caer_protocol::social::GroupMemberUpdate { members: vec![] },
        ))
        .unwrap();
        drop(tx);
        let mut world = WorldState::default();
        let d = drain_into(&rx, &mut world);
        assert_eq!(
            d.group_windows.len(),
            1,
            "GroupWindow must reach Drain.group_windows for social roster"
        );
        assert_eq!(d.group_windows[0].members[0].object_id, 12);
        assert_eq!(
            d.group_member_updates.len(),
            1,
            "GroupMemberUpdate must reach Drain.group_member_updates"
        );
    }

    #[test]
    fn unexpected_end_with_attempts_schedules_retry() {
        let now = Instant::now();
        assert_eq!(
            reconnect_plan(true, false, 3, None, now),
            ReconnectPlan::Schedule {
                delay: LIVE_RECONNECT_DELAY
            }
        );
    }

    #[test]
    fn quit_and_zero_attempts_do_not_reconnect() {
        let now = Instant::now();
        assert_eq!(
            reconnect_plan(true, true, 3, None, now),
            ReconnectPlan::GiveUp
        );
        assert_eq!(
            reconnect_plan(true, false, 0, None, now),
            ReconnectPlan::GiveUp
        );
        assert_eq!(
            reconnect_plan(false, false, 3, None, now),
            ReconnectPlan::Idle
        );
    }

    #[test]
    fn due_schedule_spawns_and_waiting_stays_idle() {
        let now = Instant::now();
        let due = now - Duration::from_millis(1);
        let later = now + Duration::from_secs(5);
        assert_eq!(
            reconnect_plan(true, false, 2, Some(due), now),
            ReconnectPlan::SpawnNow
        );
        assert_eq!(
            reconnect_plan(true, false, 2, Some(later), now),
            ReconnectPlan::Idle
        );
    }

    #[test]
    fn product_consumer_registry_is_wired_in_drain() {
        let src = include_str!("live.rs");
        let start = src.find("pub fn drain_into").expect("drain_into");
        let body = &src[start
            ..src[start..]
                .find("\n/// Keep the most-advanced")
                .unwrap_or(src.len())
                + start];
        for name in caer_protocol::coverage::PRODUCT_CONSUMER_EVENTS {
            assert!(
                body.contains(&format!("ServerEvent::{name}")),
                "{name} is PRODUCT_CONSUMER but drain_into does not name it"
            );
        }
    }

    #[test]
    fn unconsumed_typed_event_is_reported() {
        caer_protocol::packet_telemetry::reset();
        let (tx, rx) = mpsc::channel();
        tx.send(ServerEvent::TrainerWindow(
            caer_protocol::trainer::TrainerWindow {
                count: 0,
                points: 0,
                code: 0,
                unk: 0,
                lines: Vec::new(),
                rest: Vec::new(),
            },
        ))
        .unwrap();
        drop(tx);
        let mut world = WorldState::default();
        let _ = drain_into(&rx, &mut world);
        let report = caer_protocol::packet_telemetry::render_tsv();
        assert!(
            report.contains("unconsumed_typed") && report.contains("TrainerWindow"),
            "report:\n{report}"
        );
    }
}
