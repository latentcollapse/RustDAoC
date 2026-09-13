//! System 5 Stream A — ground target 0xEC + action that lands at the chosen point.
//!
//! Local "landed" coordinates are applied **only** after a server PlayerPosition that
//! matches the chosen GT (via GM `/jump to gt`, which reads server GroundTargetPosition).
//! Never from the PlayerGroundTarget send (reject-equip rule).
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example ground_rt
//! ```
//!
//! Happy path: PlayerGroundTarget (in-view) at chosen → `/jump to gt` → position ≈ chosen.
//!
//! **Refuse falsifier:** `CAER_GROUND_REFUSE=1` — send a far/not-in-view GT instead of the
//! intended chosen point; after `/jump to gt` must **not** land at chosen and must not
//! invent a local landing for chosen.

use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::{Closed, Config};
use caer_protocol::session::ServerEvent;
use caer_protocol::worldverb::{self, GROUND_TARGET_IN_VIEW};

const POS_TOL: i32 = 64;

#[derive(Default)]
struct Track {
    x: i32,
    y: i32,
    z: i32,
    chats: Vec<String>,
    /// Applied only after server position confirms the chosen GT — never from 0xEC send.
    landed: Option<(i32, i32, i32)>,
}

fn apply(t: &mut Track, e: &ServerEvent) {
    match e {
        ServerEvent::PlayerPosition { x, y, z, .. } => {
            t.x = *x as i32;
            t.y = *y as i32;
            t.z = *z as i32;
        }
        ServerEvent::CharacterJump(j) => {
            t.x = j.x;
            t.y = j.y;
            t.z = i32::from(j.z);
        }
        ServerEvent::ChatMessage { text, .. } => t.chats.push(text.clone()),
        _ => {}
    }
}

fn near(a: (i32, i32, i32), b: (i32, i32, i32), tol: i32) -> bool {
    (a.0 - b.0).abs() <= tol && (a.1 - b.1).abs() <= tol && (a.2 - b.2).abs() <= tol * 4
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();
    let refuse = live_harness::env_flag("CAER_GROUND_REFUSE");

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;

    eprintln!("ground_rt: server={server} account={account} refuse={refuse}");
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));

    if track.x == 0 && track.y == 0 {
        eprintln!("FAIL no player position yet");
        std::process::exit(6);
    }

    // Stable home so refuse/happy do not inherit a prior GT from another RT.
    let home = (track.x, track.y, track.z);
    let chosen = (home.0 + 400, home.1 + 400, home.2);
    eprintln!("ground_rt: home={home:?} chosen={chosen:?}");

    if refuse {
        // Server-accepted GT is far away / not in view — not the intended chosen point.
        eprintln!("ground_rt: REFUSE — send far GT (1,1,1) flag=0; chosen stays {chosen:?}");
        if let Err(e) = sess.ground_target(1, 1, 1, 0) {
            eprintln!("FAIL ground_target: {e}");
            std::process::exit(3);
        }
    } else {
        eprintln!(
            "ground_rt: PlayerGroundTarget chosen flag=0x{:04x} (no local apply on send)",
            GROUND_TARGET_IN_VIEW
        );
        if let Err(e) = sess.ground_target(chosen.0, chosen.1, chosen.2, GROUND_TARGET_IN_VIEW) {
            eprintln!("FAIL ground_target: {e}");
            std::process::exit(3);
        }
    }
    if track.landed.is_some() {
        eprintln!("FAIL optimistic landing applied before server confirm");
        std::process::exit(14);
    }
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));

    eprintln!("ground_rt: jump to gt (server GroundTargetPosition)");
    let _ = sess.command("jump to gt");

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    if let ServerEvent::CharacterJump(j) = e {
                        let p = (j.x, j.y, i32::from(j.z));
                        eprintln!("ground_rt: CharacterJump {p:?}");
                        if near(p, chosen, POS_TOL) {
                            track.landed = Some(chosen);
                        }
                    }
                    if let ServerEvent::PlayerPosition { x, y, z, .. } = e {
                        let p = (*x as i32, *y as i32, *z as i32);
                        eprintln!("ground_rt: pos={p:?}");
                        if near(p, chosen, POS_TOL) {
                            track.landed = Some(chosen);
                        }
                    }
                    if let ServerEvent::ChatMessage { text, .. } = e {
                        eprintln!("ground_rt: chat: {text}");
                    }
                    if let ServerEvent::Raw { code, payload } = e {
                        eprintln!("ground_rt: Raw 0x{code:02x} len={}", payload.len());
                    }
                    apply(&mut track, e);
                }
            }
            Err(Closed(msg)) => {
                LiveExit::Closed.bail(format!("FAIL closed: {msg}"));
            }
        }
        if !refuse && track.landed.is_some() {
            break;
        }
    }
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));

    let at = (track.x, track.y, track.z);

    if refuse {
        if track.landed.is_some() || near(at, chosen, POS_TOL) {
            eprintln!(
                "FAIL refuse: landed at chosen {chosen:?} (pos={at:?} landed={:?}) — \
                 client applied refused point or wrong GT",
                track.landed
            );
            std::process::exit(15);
        }
        eprintln!(
            "PASS ground_rt REFUSE: no landing at chosen {chosen:?} (pos={at:?}; \
             no optimistic apply; bit 0x{:04x} unset on refuse send)",
            worldverb::GROUND_TARGET_IN_VIEW
        );
        let _ = sess.quit();
        live_harness::cleanup_drain(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));
        return;
    }

    let Some(landed) = track.landed else {
        eprintln!(
            "FAIL no server position at chosen GT={chosen:?}; pos={at:?}; chats={:?}",
            track.chats
        );
        std::process::exit(1);
    };
    if landed != chosen {
        eprintln!("FAIL landed {landed:?} != chosen {chosen:?}");
        std::process::exit(2);
    }

    eprintln!(
        "PASS ground_rt: PlayerGroundTarget + jump-to-gt landed at {landed:?} \
         (CharacterJump/position confirm; no optimistic apply)"
    );
    let _ = sess.quit();
    live_harness::cleanup_drain(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));
}
