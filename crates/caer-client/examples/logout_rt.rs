//! SCN-12 live falsifier — clean `/quit` → LoggedOut (or clean close) → reconnect world rebuild.
//!
//! Happy path: enter world, fingerprint local state, `LiveSession::quit`, observe logout, drop
//! the session, reconnect, `EnteredWorld` again with world rebuilt from packets (not a stale
//! pre-logout inventory cache).
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo run -p caer-client --example logout_rt
//! ```
//!
//! Pass marker (exact substring SCN-12 looks for): `PASS logout_reconnect`

use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::{Closed, Config, LiveSession};
use caer_protocol::session::ServerEvent;
use caer_world::{Kind, WorldState};

#[derive(Default)]
struct Track {
    entered: bool,
    logged_out: bool,
    inv_updates: u32,
    chats: Vec<String>,
}

#[derive(Clone, Debug)]
struct Fingerprint {
    entities: usize,
    self_oid: Option<u16>,
    inventory_present: bool,
}

fn fingerprint(world: &WorldState) -> Fingerprint {
    let self_oid = world
        .iter()
        .find(|e| e.kind == Kind::Self_)
        .map(|e| e.object_id);
    Fingerprint {
        entities: world.len(),
        self_oid,
        inventory_present: world.inventory().is_some(),
    }
}

fn apply(t: &mut Track, world: &mut WorldState, e: &ServerEvent) {
    match e {
        ServerEvent::EnteredWorld => t.entered = true,
        ServerEvent::LoggedOut { .. } => t.logged_out = true,
        ServerEvent::InventoryUpdated(_) => t.inv_updates = t.inv_updates.saturating_add(1),
        ServerEvent::ChatMessage { text, .. } => t.chats.push(text.clone()),
        _ => {}
    }
    world.apply(e);
}

/// Wait for LoggedOut confirmation or a clean socket close after `/quit`.
/// Quit-timer servers can hold ~60s; stay stationary and poll.
fn wait_logout(
    sess: &mut LiveSession,
    t: &mut Track,
    world: &mut WorldState,
    deadline: Instant,
) -> Result<&'static str, String> {
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    if let ServerEvent::LoggedOut { total_out, level } = e {
                        eprintln!("logout_rt: LoggedOut total_out={total_out} level={level}");
                    }
                    if let ServerEvent::ChatMessage { text, .. } = e {
                        eprintln!("logout_rt: chat: {text}");
                    }
                    apply(t, world, e);
                    if matches!(e, ServerEvent::LoggedOut { .. }) {
                        return Ok("LoggedOut");
                    }
                }
            }
            Err(Closed(msg)) => {
                eprintln!("logout_rt: clean close after quit: {msg}");
                return Ok("Closed");
            }
        }
        if sess.logged_out() || t.logged_out {
            return Ok("LoggedOut");
        }
    }
    Err("timeout waiting LoggedOut / clean close after /quit".into())
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;
    if let Ok(name) = std::env::var("CAER_CHARACTER") {
        cfg.character = Some(name);
    }

    eprintln!(
        "logout_rt: server={server} account={account} char={:?}",
        cfg.character
    );

    // --- Session 1: enter world, fingerprint ---
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    let mut world = WorldState::new();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, &mut world, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| {
        apply(&mut track, &mut world, e)
    });

    let fp1 = fingerprint(&world);
    eprintln!(
        "logout_rt: fingerprint1 entities={} self_oid={:?} inventory={}",
        fp1.entities, fp1.self_oid, fp1.inventory_present
    );
    if fp1.self_oid.is_none() {
        eprintln!("FAIL never got Self_ (PlayerPosition) after EnteredWorld");
        std::process::exit(6);
    }

    // Hold still so quit-timer servers accept /quit (moving cancels the countdown).
    let self_pos = world.iter().find(|e| e.kind == Kind::Self_).map(|e| e.pos);
    if let Some([x, y, z]) = self_pos {
        let _ = sess.position_update(x as f32, y as f32, z as f32, 0.0, 100);
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| {
            apply(&mut track, &mut world, e)
        });
    }

    eprintln!("logout_rt: → /quit (waiting LoggedOut or clean close)");
    sess.quit().unwrap_or_else(|e| {
        eprintln!("FAIL quit send: {e}");
        std::process::exit(3);
    });

    let how = wait_logout(
        &mut sess,
        &mut track,
        &mut world,
        Instant::now() + Duration::from_secs(70),
    )
    .unwrap_or_else(|e| {
        eprintln!("FAIL logout: {e}");
        std::process::exit(7);
    });

    // LoggedOut must scrub local caches (SCN-12 clear_for_logout). Clean close without the
    // packet still ends the session; we rebuild from a fresh WorldState on reconnect either way.
    if how == "LoggedOut" {
        let after = fingerprint(&world);
        if after.entities != 0 {
            eprintln!(
                "FAIL LoggedOut left {} entities — clear_for_logout incomplete",
                after.entities
            );
            std::process::exit(8);
        }
        if after.inventory_present {
            eprintln!("FAIL LoggedOut left inventory — stale cache would survive reconnect");
            std::process::exit(9);
        }
        if after.self_oid.is_some() {
            eprintln!("FAIL LoggedOut left Self_ oid — clear_for_logout incomplete");
            std::process::exit(10);
        }
        eprintln!("logout_rt: post-LoggedOut world empty (clear_for_logout ok)");
    }
    drop(sess);
    eprintln!("logout_rt: session closed via {how}");

    // --- Session 2: reconnect, EnteredWorld, rebuild from packets ---
    std::thread::sleep(Duration::from_millis(750));
    let mut sess2 = LiveSession::connect(&cfg).unwrap_or_else(|e| {
        eprintln!("FAIL reconnect: {e}");
        std::process::exit(4);
    });
    let mut track2 = Track::default();
    let mut world2 = WorldState::new();
    live_harness::wait_entered_world(&mut sess2, live_harness::world_deadline(), |e| {
        apply(&mut track2, &mut world2, e)
    })
    .unwrap_or_else(|e| {
        eprintln!("FAIL reconnect world: {e}");
        std::process::exit(5);
    });
    live_harness::drain_or_exit(&mut sess2, Duration::from_secs(4), |e| {
        apply(&mut track2, &mut world2, e)
    });

    let fp2 = fingerprint(&world2);
    eprintln!(
        "logout_rt: fingerprint2 entities={} self_oid={:?} inventory={} inv_updates={}",
        fp2.entities, fp2.self_oid, fp2.inventory_present, track2.inv_updates
    );

    if !track2.entered || !sess2.in_world() {
        eprintln!("FAIL reconnect never EnteredWorld");
        std::process::exit(5);
    }
    if fp2.self_oid.is_none() {
        eprintln!("FAIL reconnect: no Self_ from PlayerPosition packets");
        std::process::exit(11);
    }
    // Inventory must come from InventoryUpdate packets on this session — never from a carried
    // pre-logout local cache (world2 started empty).
    if fp2.inventory_present && track2.inv_updates == 0 {
        eprintln!(
            "FAIL inventory present without InventoryUpdate after reconnect \
             (stale local cache restore)"
        );
        std::process::exit(12);
    }

    eprintln!(
        "PASS logout_reconnect: quit→{how}; clear; reconnect EnteredWorld \
         fp1={{e={},oid={:?},inv={}}} fp2={{e={},oid={:?},inv={},upd={}}}",
        fp1.entities,
        fp1.self_oid,
        fp1.inventory_present,
        fp2.entities,
        fp2.self_oid,
        fp2.inventory_present,
        track2.inv_updates
    );

    let _ = sess2.quit();
    let mut tq = Track::default();
    let mut wq = WorldState::new();
    let _ = wait_logout(
        &mut sess2,
        &mut tq,
        &mut wq,
        Instant::now() + Duration::from_secs(70),
    );
}
