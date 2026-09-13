//! System 5 Stream A — door open via DoorRequest 0x99, confirm DoorState 0x99 S2C.
//!
//! Does **not** flip local door state on send. Pass requires a server DoorState with
//! `open=true` for the requested InternalID (PlayScenario bar).
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example door_rt
//! ```
//!
//! **Falsifier:** `CAER_DOOR_NOSEND=1` — skip DoorRequest; must NOT see DoorState open
//! for the target id (would green if Pass ignored the request path).
//!
//! Optional: `CAER_DOOR_ID`, `CAER_DOOR_X/Y/Z` (defaults: SoloDAoC Door.json InternalID
//! 2184203 @ Cotswold-area coords).

use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::{Closed, Config};
use caer_protocol::session::ServerEvent;
use caer_protocol::worldverb::{self, DoorState};

/// Default unlocked door from SoloDAoC `Door.json` (zone-local Cotswold vicinity).
const DEFAULT_DOOR_ID: u32 = 2_184_203;
const DEFAULT_X: i32 = 530_932;
const DEFAULT_Y: i32 = 478_046;
const DEFAULT_Z: i32 = 2_456;

#[derive(Default)]
struct Track {
    door_states: Vec<DoorState>,
    chats: Vec<String>,
}

fn apply(t: &mut Track, e: &ServerEvent) {
    match e {
        ServerEvent::DoorState(d) => t.door_states.push(*d),
        ServerEvent::ChatMessage { text, .. } => t.chats.push(text.clone()),
        _ => {}
    }
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();
    let nosend = live_harness::env_flag("CAER_DOOR_NOSEND");
    let door_id: u32 = std::env::var("CAER_DOOR_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_DOOR_ID);
    let x: i32 = std::env::var("CAER_DOOR_X")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_X);
    let y: i32 = std::env::var("CAER_DOOR_Y")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_Y);
    let z: i32 = std::env::var("CAER_DOOR_Z")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_Z);

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;

    eprintln!("door_rt: server={server} account={account} door={door_id} nosend={nosend}");
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));

    // Region 1 Albion — jump onto the door so ChangeDoorAction radius check passes.
    eprintln!("door_rt: jump to {x} {y} {z} region 1");
    let _ = sess.command(&format!("jump to {x} {y} {z} 1"));
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(4), |e| apply(&mut track, e));

    track.door_states.clear();
    if nosend {
        eprintln!("door_rt: NOSEND — skipping DoorRequest (falsifier)");
    } else {
        eprintln!("door_rt: DoorRequest open id={door_id}");
        if let Err(e) = sess.door_request(door_id, worldverb::DOOR_STATE_OPEN) {
            eprintln!("FAIL door_request: {e}");
            std::process::exit(3);
        }
    }

    let deadline = Instant::now() + Duration::from_secs(if nosend { 5 } else { 12 });
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    if let ServerEvent::DoorState(d) = e {
                        eprintln!(
                            "door_rt: DoorState id={} open={} flag={}",
                            d.door_id, d.open, d.flag
                        );
                    }
                    if let ServerEvent::ChatMessage { text, .. } = e {
                        eprintln!("door_rt: chat: {text}");
                    }
                    apply(&mut track, e);
                }
            }
            Err(Closed(msg)) => {
                LiveExit::Closed.bail(format!("FAIL closed: {msg}"));
            }
        }
        if !nosend
            && track
                .door_states
                .iter()
                .any(|d| d.door_id == door_id && d.open)
        {
            break;
        }
    }
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));

    let opened = track
        .door_states
        .iter()
        .any(|d| d.door_id == door_id && d.open);

    if nosend {
        if opened {
            eprintln!(
                "FAIL door_rt NOSEND: saw DoorState open for {door_id} without DoorRequest \
                 (request path not load-bearing)"
            );
            std::process::exit(15);
        }
        eprintln!(
            "PASS door_rt NOSEND: no DoorState open for {door_id} without DoorRequest \
             (falsifier holds)"
        );
        let _ = sess.quit();
        live_harness::cleanup_drain(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));
        return;
    }

    if !opened {
        eprintln!(
            "FAIL no DoorState open for door {door_id}; states={:?}; chats={:?}",
            track.door_states, track.chats
        );
        std::process::exit(1);
    }

    eprintln!(
        "PASS door_rt: DoorRequest → DoorState open id={door_id} \
         (provenance={})",
        worldverb::DOOR_STATE_PROVENANCE
    );
    let _ = sess.quit();
    live_harness::cleanup_drain(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));
}
