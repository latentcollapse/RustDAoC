//! SCN-11 live RT — say via Command 0xAF, confirm S2C Message 0xAF ChatMessage echo.
//!
//! Does **not** Pass on local echo of the say text. Pass requires a server
//! [`ServerEvent::ChatMessage`] (Message 0xAF decode) whose text contains the unique token.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo run -p caer-client --example chat_rt
//! ```
//!
//! **Falsifier:** `CAER_CHAT_NOSEND=1` — skip Command 0xAF say; must NOT see ChatMessage
//! containing the unique token (would green if Pass invented local echo / ignored the wire).
//!
//! Observation that fails if broken: Pass after `say <unique>` without any S2C ChatMessage
//! carrying that token (local-only echo, missing Message 0xAF arm, or inventing the marker).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::Closed;
use caer_protocol::session::ServerEvent;

#[derive(Default)]
struct Track {
    /// ChatMessage texts observed from the server (Message 0xAF → ChatMessage).
    chats: Vec<(u8, String)>,
}

fn apply(t: &mut Track, e: &ServerEvent) {
    if let ServerEvent::ChatMessage { chat_type, text } = e {
        t.chats.push((*chat_type, text.clone()));
    }
}

/// True when a server ChatMessage text contains `token` (case-sensitive substring).
fn chat_echo_contains(chats: &[(u8, String)], token: &str) -> bool {
    chats.iter().any(|(_, text)| text.contains(token))
}

fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("CAERRT{nanos}")
}

fn main() {
    let creds = LiveCredentials::from_env();
    let nosend = live_harness::env_flag("CAER_CHAT_NOSEND");
    let token = std::env::var("CAER_CHAT_TOKEN").unwrap_or_else(|_| unique_token());
    let cfg = creds.clone().into_config();

    eprintln!(
        "chat_rt: server={} account={} nosend={nosend} token={token}",
        creds.server, creds.account
    );
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, e);
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, live_harness::SETTLE_DRAIN, |e| {
        apply(&mut track, e)
    });

    // Clear pre-world / login chatter so Pass cannot hitchhike on unrelated Message 0xAF.
    track.chats.clear();

    if nosend {
        eprintln!("chat_rt: NOSEND — skipping Command 0xAF say (falsifier)");
    } else {
        // LiveSession::command → SessionState::command → client Command 0xAF `&say …\0`.
        // Do not invent ChatMessage here — wait for S2C Message 0xAF.
        eprintln!("chat_rt: command(say {token})");
        if let Err(e) = sess.command(&format!("say {token}")) {
            eprintln!("FAIL command say: {e}");
            std::process::exit(3);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(if nosend { 5 } else { 12 });
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    if let ServerEvent::ChatMessage { chat_type, text } = e {
                        eprintln!("chat_rt: ChatMessage type={chat_type:#04x} text={text}");
                    }
                    apply(&mut track, e);
                }
            }
            Err(Closed(msg)) => {
                LiveExit::Closed.bail(format!("FAIL closed: {msg}"));
            }
        }
        if !nosend && chat_echo_contains(&track.chats, &token) {
            break;
        }
    }
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));

    let echoed = chat_echo_contains(&track.chats, &token);
    let _ = live_harness::conclude_two_arm(
        nosend,
        echoed,
        &format!(
            "PASS chat_rt: Command 0xAF say → Message 0xAF ChatMessage contains {token:?} \
             (chats={})",
            track.chats.len()
        ),
        &format!(
            "PASS chat_rt NOSEND: no ChatMessage containing {token:?} without Command 0xAF say \
             (falsifier holds)"
        ),
        &format!(
            "FAIL no S2C ChatMessage (Message 0xAF) containing {token:?}; chats={:?}",
            track.chats
        ),
        &format!(
            "FAIL chat_rt NOSEND: saw ChatMessage containing {token:?} without Command 0xAF say \
             (local-echo / invent path not load-bearing)"
        ),
    );
    live_harness::cleanup_quit(&mut sess, |e| apply(&mut track, e));
}

#[cfg(test)]
mod tests {
    use super::chat_echo_contains;

    /// Named falsifier: local-only invent must not satisfy the echo check.
    #[test]
    fn echo_check_requires_observed_chat_text() {
        let local_only: Vec<(u8, String)> = Vec::new();
        assert!(
            !chat_echo_contains(&local_only, "CAERRTunique"),
            "empty ChatMessage list must not Pass — that is inventing local echo"
        );
        let from_server = vec![(0x01, "You say, \"CAERRTunique\"".into())];
        assert!(chat_echo_contains(&from_server, "CAERRTunique"));
        assert!(!chat_echo_contains(&from_server, "CAERRTother"));
    }
}
