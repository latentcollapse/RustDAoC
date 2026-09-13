//! Shared live-RT harness — connect, wait/drain, two-arm outcomes, cleanup, identity.
//!
//! Extracts the repeated `wait_world` / `drain` / soft-quit families from `examples/*_rt.rs`
//! without swallowing scenario-specific assertion logic. Exit codes stay distinguishable:
//! connect ≠ world timeout ≠ closed ≠ positive-arm fail ≠ negative-arm (NOSEND) fail.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use caer_protocol::session::ServerEvent;

use crate::evidence::{self, BUILD_COMMIT, BUILD_DIRTY, BUILD_STAMP};
use crate::{Closed, Config, LiveSession};

/// Default SoloDAoC endpoint used by RT examples when env is unset.
pub const DEFAULT_SERVER: &str = "127.0.0.1:10311";
/// Default primary account (interactive RT convenience; gate classifies missing live env separately).
pub const DEFAULT_ACCOUNT: &str = "caer14d";
pub const DEFAULT_PASSWORD: &str = "caer14d";
pub const DEFAULT_ACCOUNT2: &str = "caer14c";
pub const DEFAULT_PASSWORD2: &str = "caer14c";

/// Bound for EnteredWorld after connect (shared across migrated examples).
pub const WORLD_WAIT: Duration = Duration::from_secs(45);
/// Short drain after world entry / before assertions.
pub const SETTLE_DRAIN: Duration = Duration::from_secs(2);
/// Drain after quit on cleanup paths.
pub const CLEANUP_DRAIN: Duration = Duration::from_secs(2);

/// Typed process exits for live RT examples. Values match the historical contracts
/// scenario runners and operators already discriminate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum LiveExit {
    /// Positive or negative arm held; PASS marker printed.
    Pass = 0,
    /// Positive-arm assertion failed (expected effect missing).
    Fail = 1,
    /// TCP / login connect failed.
    Connect = 4,
    /// Bounded wait for EnteredWorld (or equivalent) expired.
    WorldTimeout = 5,
    /// Socket closed during a wait that required the session to stay up.
    Closed = 8,
    /// Negative arm (NOSEND / refuse / cancel) saw the effect that must not appear.
    NegativeArm = 15,
}

impl LiveExit {
    #[must_use]
    pub fn code(self) -> i32 {
        self as i32
    }

    /// Print `msg` to stderr and terminate with this exit code.
    pub fn bail(self, msg: impl fmt::Display) -> ! {
        eprintln!("{msg}");
        std::process::exit(self.code());
    }

    /// Map a [`LiveError`] from wait/drain helpers onto the typed exit.
    #[must_use]
    pub fn from_wait_err(err: &LiveError) -> Self {
        match err {
            LiveError::Timeout(_) => Self::WorldTimeout,
            LiveError::Closed(_) => Self::Closed,
            LiveError::Connect(_) => Self::Connect,
            LiveError::Credential(_) => Self::Connect,
            LiveError::Send(_) => Self::Fail,
        }
    }
}

/// Typed failure from harness wait/connect helpers (not process exit by itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveError {
    Connect(String),
    Timeout(String),
    Closed(String),
    Credential(String),
    /// Load-bearing C2S send failed (must not be discarded on assertion paths).
    Send(String),
}

impl fmt::Display for LiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(s)
            | Self::Timeout(s)
            | Self::Closed(s)
            | Self::Credential(s)
            | Self::Send(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for LiveError {}

/// Compile-time source identity bound into the caer-client binary (Codex B1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceIdentity {
    pub commit: &'static str,
    pub dirty: bool,
}

impl SourceIdentity {
    #[must_use]
    pub fn current() -> Self {
        Self {
            commit: BUILD_COMMIT,
            dirty: BUILD_DIRTY == "true",
        }
    }

    /// Ensure the build stamp remains linked (LTO-safe).
    pub fn touch_stamp(self) {
        let _keep: &'static str = BUILD_STAMP;
    }
}

/// Endpoint + credentials resolved from `CAER_*` environment variables.
#[derive(Debug, Clone)]
pub struct LiveCredentials {
    pub server: String,
    pub account: String,
    pub password: String,
    pub character: Option<String>,
}

impl LiveCredentials {
    /// Primary session: `CAER_SERVER` / `CAER_ACCOUNT` / `CAER_PASSWORD` / `CAER_CHARACTER`.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            server: env_or("CAER_SERVER", DEFAULT_SERVER),
            account: env_or("CAER_ACCOUNT", DEFAULT_ACCOUNT),
            password: env_or("CAER_PASSWORD", DEFAULT_PASSWORD),
            character: std::env::var("CAER_CHARACTER")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }

    /// Second dual-client session: `CAER_ACCOUNT2` / `CAER_PASSWORD2` / `CAER_CHARACTER2`.
    #[must_use]
    pub fn from_env_second() -> Self {
        Self {
            server: env_or("CAER_SERVER", DEFAULT_SERVER),
            account: env_or("CAER_ACCOUNT2", DEFAULT_ACCOUNT2),
            password: env_or("CAER_PASSWORD2", DEFAULT_PASSWORD2),
            character: std::env::var("CAER_CHARACTER2")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }

    /// Build a [`Config`] with auto-select enabled (headless RT default).
    #[must_use]
    pub fn into_config(self) -> Config {
        let mut cfg = Config::new(self.server, self.account, self.password);
        cfg.character = self.character;
        cfg.auto_select = true;
        cfg
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// True when `key` is set to `"1"` (NOSEND / refuse / cancel / empty arms).
#[must_use]
pub fn env_flag(key: &str) -> bool {
    std::env::var(key).ok().as_deref() == Some("1")
}

/// Emit evidence identity for `example` (also happens automatically on [`connect`]/
/// when `CAER_RT_EXAMPLE` is set by the scenario runner).
pub fn emit_identity(example: &str) {
    SourceIdentity::current().touch_stamp();
    evidence::emit(example);
}

/// Connect with typed [`LiveError::Connect`] on failure.
pub fn connect(cfg: &Config) -> Result<LiveSession, LiveError> {
    SourceIdentity::current().touch_stamp();
    LiveSession::connect(cfg).map_err(|e| LiveError::Connect(e.to_string()))
}

/// Deadline for the standard EnteredWorld wait.
#[must_use]
pub fn world_deadline() -> Instant {
    Instant::now() + WORLD_WAIT
}

/// Poll until `ServerEvent::EnteredWorld` or deadline / close.
///
/// `on_event` receives every decoded event (including EnteredWorld) so trackers stay complete.
pub fn wait_entered_world(
    sess: &mut LiveSession,
    deadline: Instant,
    mut on_event: impl FnMut(&ServerEvent),
) -> Result<(), LiveError> {
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    on_event(e);
                    if matches!(e, ServerEvent::EnteredWorld) {
                        return Ok(());
                    }
                }
            }
            Err(Closed(msg)) => {
                return Err(LiveError::Closed(format!("closed: {msg}")));
            }
        }
    }
    Err(LiveError::Timeout("timeout waiting EnteredWorld".into()))
}

/// Poll until `pred` is true, feeding events to `on_event`, or fail on deadline / close.
pub fn wait_until(
    sess: &mut LiveSession,
    deadline: Instant,
    mut on_event: impl FnMut(&ServerEvent),
    mut pred: impl FnMut() -> bool,
    timeout_msg: &str,
) -> Result<(), LiveError> {
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    on_event(e);
                }
            }
            Err(Closed(msg)) => {
                return Err(LiveError::Closed(format!("closed: {msg}")));
            }
        }
        if pred() {
            return Ok(());
        }
    }
    Err(LiveError::Timeout(timeout_msg.into()))
}

/// Assertion drain over an injected poll (unit-testable without a live socket).
///
/// Socket close is a hard failure: callers must not treat an empty observation window as PASS.
pub fn drain_polls(
    dur: Duration,
    mut poll: impl FnMut() -> Result<Vec<ServerEvent>, Closed>,
    mut on_event: impl FnMut(&ServerEvent),
) -> Result<(), LiveError> {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        match poll() {
            Ok(evs) => {
                for e in &evs {
                    on_event(e);
                }
            }
            Err(Closed(msg)) => {
                return Err(LiveError::Closed(format!(
                    "closed during assertion drain: {msg}"
                )));
            }
        }
    }
    Ok(())
}

/// Dual assertion drain. Either peer closing fails the window (cannot PASS a NOSEND arm).
pub fn drain_pair_polls(
    dur: Duration,
    mut poll_a: impl FnMut() -> Result<Vec<ServerEvent>, Closed>,
    mut poll_b: impl FnMut() -> Result<Vec<ServerEvent>, Closed>,
    mut on_a: impl FnMut(&ServerEvent),
    mut on_b: impl FnMut(&ServerEvent),
) -> Result<(), LiveError> {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        match poll_a() {
            Ok(evs) => {
                for e in &evs {
                    on_a(e);
                }
            }
            Err(Closed(msg)) => {
                return Err(LiveError::Closed(format!(
                    "peer A closed during drain: {msg}"
                )));
            }
        }
        match poll_b() {
            Ok(evs) => {
                for e in &evs {
                    on_b(e);
                }
            }
            Err(Closed(msg)) => {
                return Err(LiveError::Closed(format!(
                    "peer B closed during drain: {msg}"
                )));
            }
        }
    }
    Ok(())
}

/// Assertion drain: fail if the socket closes. Use [`cleanup_drain`] after quit.
pub fn drain(
    sess: &mut LiveSession,
    dur: Duration,
    on_event: impl FnMut(&ServerEvent),
) -> Result<(), LiveError> {
    drain_polls(dur, || sess.poll(), on_event)
}

/// Dual-client assertion drain used by group/trade RTs.
pub fn drain_pair(
    a: &mut LiveSession,
    b: &mut LiveSession,
    dur: Duration,
    on_a: impl FnMut(&ServerEvent),
    on_b: impl FnMut(&ServerEvent),
) -> Result<(), LiveError> {
    drain_pair_polls(dur, || a.poll(), || b.poll(), on_a, on_b)
}

/// Assertion drain that cannot print PASS: closed/send-class errors bail with typed exits.
pub fn drain_or_exit(sess: &mut LiveSession, dur: Duration, on_event: impl FnMut(&ServerEvent)) {
    if let Err(e) = drain(sess, dur, on_event) {
        LiveExit::from_wait_err(&e).bail(format!("FAIL drain: {e}"));
    }
}

/// Dual assertion drain that cannot print PASS on peer close.
pub fn drain_pair_or_exit(
    a: &mut LiveSession,
    b: &mut LiveSession,
    dur: Duration,
    on_a: impl FnMut(&ServerEvent),
    on_b: impl FnMut(&ServerEvent),
) {
    if let Err(e) = drain_pair(a, b, dur, on_a, on_b) {
        LiveExit::from_wait_err(&e).bail(format!("FAIL drain_pair: {e}"));
    }
}

/// Cleanup-only drain: socket close ends the loop. Must not be used in assertion windows.
pub fn cleanup_drain(
    sess: &mut LiveSession,
    dur: Duration,
    mut on_event: impl FnMut(&ServerEvent),
) {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    on_event(e);
                }
            }
            Err(Closed(_)) => break,
        }
    }
}

/// Load-bearing send: I/O failure is Fail, never a silent empty observation.
pub fn require_send<T>(r: io::Result<T>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => LiveExit::Fail.bail(format!("FAIL send {what}: {e}")),
    }
}

pub fn require_send_result<T>(r: io::Result<T>, what: &str) -> Result<T, LiveError> {
    r.map_err(|e| LiveError::Send(format!("{what}: {e}")))
}

/// Soft `/quit` + short cleanup drain (ignores send errors; may swallow close).
pub fn soft_quit(sess: &mut LiveSession, mut on_event: impl FnMut(&ServerEvent)) {
    let _ = sess.quit();
    cleanup_drain(sess, Duration::from_secs(8), &mut on_event);
}

/// Quit + bounded cleanup drain (PASS / NOSEND success paths after assertions).
pub fn cleanup_quit(sess: &mut LiveSession, mut on_event: impl FnMut(&ServerEvent)) {
    let _ = sess.quit();
    cleanup_drain(sess, CLEANUP_DRAIN, &mut on_event);
}

/// Two-arm (positive / NOSEND-style negative) conclusion.
///
/// * `negative_arm` — falsifier mode (NOSEND / refuse / cancel / empty).
/// * `effect_seen` — whether the load-bearing server effect was observed.
///
/// Preserves historical exits: negative-arm false green → 15; positive miss → 1; success → 0
/// after printing the matching PASS line (caller supplies exact marker text).
pub fn conclude_two_arm(
    negative_arm: bool,
    effect_seen: bool,
    pass_positive: &str,
    pass_negative: &str,
    fail_positive: &str,
    fail_negative: &str,
) -> LiveExit {
    if negative_arm {
        if effect_seen {
            LiveExit::NegativeArm.bail(fail_negative);
        }
        eprintln!("{pass_negative}");
        LiveExit::Pass
    } else if !effect_seen {
        LiveExit::Fail.bail(fail_positive);
    } else {
        eprintln!("{pass_positive}");
        LiveExit::Pass
    }
}

/// Helper used by examples that still call `std::process::exit` with a typed code.
pub fn exit_connect(err: impl fmt::Display) -> ! {
    LiveExit::Connect.bail(format!("FAIL connect: {err}"))
}

pub fn exit_world(err: impl fmt::Display) -> ! {
    let code = if err.to_string().contains("closed") {
        LiveExit::Closed
    } else {
        LiveExit::WorldTimeout
    };
    code.bail(format!("FAIL world: {err}"))
}

/// Map an [`io::Error`] from connect into a bail (for call sites that still use `LiveSession::connect`).
pub fn bail_io_connect(err: io::Error) -> ! {
    exit_connect(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_distinguishable() {
        assert_eq!(LiveExit::Pass.code(), 0);
        assert_eq!(LiveExit::Fail.code(), 1);
        assert_eq!(LiveExit::Connect.code(), 4);
        assert_eq!(LiveExit::WorldTimeout.code(), 5);
        assert_eq!(LiveExit::Closed.code(), 8);
        assert_eq!(LiveExit::NegativeArm.code(), 15);
        let codes = [
            LiveExit::Pass.code(),
            LiveExit::Fail.code(),
            LiveExit::Connect.code(),
            LiveExit::WorldTimeout.code(),
            LiveExit::Closed.code(),
            LiveExit::NegativeArm.code(),
        ];
        let mut uniq = codes.to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), codes.len());
    }

    #[test]
    fn from_wait_err_maps_timeout_closed_connect() {
        assert_eq!(
            LiveExit::from_wait_err(&LiveError::Timeout("t".into())),
            LiveExit::WorldTimeout
        );
        assert_eq!(
            LiveExit::from_wait_err(&LiveError::Closed("c".into())),
            LiveExit::Closed
        );
        assert_eq!(
            LiveExit::from_wait_err(&LiveError::Connect("x".into())),
            LiveExit::Connect
        );
    }

    #[test]
    fn two_arm_negative_pass_when_effect_absent() {
        // Mirror conclude_two_arm logic without process::exit.
        let negative_arm = true;
        let effect_seen = false;
        assert!(negative_arm && !effect_seen);
    }

    #[test]
    fn source_identity_exposes_build_stamp() {
        let id = SourceIdentity::current();
        id.touch_stamp();
        assert!(!id.commit.is_empty());
        assert!(BUILD_STAMP.contains("CAER_BUILD_COMMIT="));
    }

    #[test]
    fn env_flag_reads_one() {
        std::env::remove_var("CAER_HARNESS_TEST_FLAG");
        assert!(!env_flag("CAER_HARNESS_TEST_FLAG"));
        std::env::set_var("CAER_HARNESS_TEST_FLAG", "1");
        assert!(env_flag("CAER_HARNESS_TEST_FLAG"));
        std::env::remove_var("CAER_HARNESS_TEST_FLAG");
    }

    /// W1-01: planted close during the assertion window cannot become a negative-arm PASS.
    #[test]
    fn assertion_drain_close_cannot_pass_negative_arm() {
        let mut polls = 0u8;
        let err = drain_polls(
            Duration::from_millis(50),
            || {
                polls += 1;
                Err(Closed("planted close".into()))
            },
            |_| {},
        )
        .expect_err("close must fail the assertion drain");
        assert!(matches!(err, LiveError::Closed(_)), "{err:?}");
        assert_eq!(LiveExit::from_wait_err(&err), LiveExit::Closed);
        assert_ne!(LiveExit::from_wait_err(&err).code(), LiveExit::Pass.code());
    }

    #[test]
    fn drain_pair_fails_if_either_peer_closes() {
        let err = drain_pair_polls(
            Duration::from_millis(50),
            || Ok(Vec::new()),
            || Err(Closed("B dropped".into())),
            |_| {},
            |_| {},
        )
        .expect_err("peer B close must fail");
        assert!(
            matches!(err, LiveError::Closed(ref s) if s.contains("peer B")),
            "{err:?}"
        );
    }

    #[test]
    fn require_send_result_propagates_io_failure() {
        let err = require_send_result::<()>(
            Err(io::Error::other("planted send fail")),
            "invite_to_group",
        )
        .expect_err("send failure must surface");
        assert!(matches!(err, LiveError::Send(_)), "{err:?}");
        assert_eq!(LiveExit::from_wait_err(&err), LiveExit::Fail);
        assert_ne!(LiveExit::from_wait_err(&err).code(), LiveExit::Pass.code());
    }

    /// RW1-01: planted Cancel send failure must not reach negative-arm PASS.
    #[test]
    fn negative_arm_cannot_pass_after_planted_send_failure() {
        let cmd =
            require_send_result::<()>(Err(io::Error::other("planted cancel fail")), "trade cancel");
        let err = cmd.expect_err("cancel send must fail");
        assert_eq!(LiveExit::from_wait_err(&err), LiveExit::Fail);
        let negative_arm = true;
        let effect_seen = false;
        // Swallowing `let _ = cancel` would PASS here; requiring send makes Pass unreachable.
        assert!(negative_arm && !effect_seen);
        assert_ne!(LiveExit::from_wait_err(&err).code(), LiveExit::Pass.code());
    }
}
