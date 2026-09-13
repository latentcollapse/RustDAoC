//! §14A / System 3 live round-trip falsifier — **all three realms**.
//!
//! Bar (Claude): create on Albion, Midgard, and Hibernia; each must appear on the overview and
//! survive a second login. Albion-only passing is the false positive this closes.
//!
//! ```bash
//! CAER_REALMS=1,2,3 cargo \
//!   run -p caer-client --example charcreate_rt
//! ```
//!
//! `CAER_REALMS` defaults to `1,2,3`. A single-realm run is still allowed for debugging but must
//! not be treated as System 3 complete.

use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials};
use caer_client::{Closed, Config, LiveSession};
use caer_protocol::charcreate::CharacterCreateDraft;
use caer_protocol::session::ServerEvent;

fn names_in(sess: &LiveSession) -> Vec<String> {
    sess.overview()
        .map(|ov| ov.characters.iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default()
}

fn occupied_slots(sess: &LiveSession) -> Vec<u8> {
    sess.overview()
        .map(|ov| ov.characters.iter().map(|c| c.slot).collect())
        .unwrap_or_default()
}

fn wait_overview(sess: &mut LiveSession, deadline: Instant) -> Result<(), String> {
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    if matches!(e, ServerEvent::CharacterOverview(_)) {
                        return Ok(());
                    }
                }
            }
            Err(Closed(msg)) => return Err(format!("closed while waiting overview: {msg}")),
        }
    }
    Err("timeout waiting for CharacterOverview".into())
}

fn wait_name(sess: &mut LiveSession, want: &str, deadline: Instant) -> Result<Vec<String>, String> {
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    if let ServerEvent::CharacterOverview(_) = e {
                        let names = names_in(sess);
                        if names.iter().any(|n| n.eq_ignore_ascii_case(want)) {
                            return Ok(names);
                        }
                    }
                }
            }
            Err(Closed(msg)) => return Err(format!("closed while waiting name: {msg}")),
        }
        let names = names_in(sess);
        if names.iter().any(|n| n.eq_ignore_ascii_case(want)) {
            return Ok(names);
        }
    }
    Err(format!(
        "timeout waiting for name {want:?}; last overview={:?}",
        names_in(sess)
    ))
}

fn unique_name(tag: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 100_000;
    format!("C{tag}{n}")
}

fn create_one(sess: &mut LiveSession, realm: u8, tag: &str) -> String {
    let name = unique_name(tag);
    let slot = CharacterCreateDraft::first_free_slot(occupied_slots(sess)).unwrap_or_else(|| {
        eprintln!(
            "FAIL realm {realm}: no free slot; overview={:?}",
            names_in(sess)
        );
        std::process::exit(9);
    });
    let draft = CharacterCreateDraft::stub_for_realm(realm, &name, slot);
    assert_eq!(draft.realm, realm, "stub must stamp the requested realm");
    assert!(draft.points_valid());
    assert_ne!(
        draft.region, 0,
        "create region must come from StartupLocations"
    );
    eprintln!(
        "charcreate_rt: realm={realm} create name={name} slot={} race={} class={} region={}",
        draft.slot, draft.race, draft.class_id, draft.region
    );
    sess.create_character(&draft).unwrap_or_else(|e| {
        eprintln!("FAIL realm {realm} create send: {e}");
        std::process::exit(4);
    });
    let after =
        wait_name(sess, &name, Instant::now() + Duration::from_secs(20)).unwrap_or_else(|e| {
            eprintln!("FAIL realm {realm} overview after create {name}: {e}");
            std::process::exit(5);
        });
    eprintln!("charcreate_rt: realm={realm} overview after {name}: {after:?}");
    name
}

fn parse_realms() -> Vec<u8> {
    std::env::var("CAER_REALMS")
        .unwrap_or_else(|_| "1,2,3".into())
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .filter(|r| (1..=3).contains(r))
        .collect()
}

/// Pump until auth completes, then ask for the realm's character overview.
///
/// With `auto_select = false` nothing sends this request, so the harness waited for a packet the
/// server never pushes and the server closed the connection with `received bytes=0`. That read as
/// a character-create failure when creation was fine — the request simply was not sent. It also
/// has to wait for the handshake: fired at a socket the server has not finished greeting, it is
/// dropped and the symptom is identical.
fn ask_for_overview(sess: &mut LiveSession, realm: u8) -> Result<(), String> {
    use caer_protocol::session::SessionPhase;
    let ready = Instant::now() + Duration::from_secs(20);
    while Instant::now() < ready {
        if !matches!(
            sess.phase(),
            SessionPhase::Disconnected
                | SessionPhase::CryptHandshake
                | SessionPhase::Authenticating
        ) {
            break;
        }
        sess.poll()
            .map_err(|e| format!("poll during auth: {e:?}"))?;
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!(
        "charcreate_rt: realm={realm} phase after auth: {:?}",
        sess.phase()
    );
    sess.request_character_overview(realm)
        .map_err(|e| format!("request overview realm {realm}: {e}"))
}

fn run_realm(server: &str, account: &str, password: &str, realm: u8) -> Result<String, String> {
    let mut cfg = Config::new(server, account, password);
    cfg.auto_select = false;
    cfg.realm = realm;

    let mut sess = live_harness::connect(&cfg).map_err(|e| format!("connect: {e}"))?;
    // Ask for the realm's overview. With `auto_select = false` nothing sends this, so the harness
    // sat waiting for a packet the server never pushes and the server dropped the connection with
    // `received bytes=0` — a harness gap that read as a create failure.
    //
    ask_for_overview(&mut sess, realm)?;
    wait_overview(&mut sess, Instant::now() + Duration::from_secs(30))?;
    assert_eq!(
        sess.realm(),
        realm,
        "session realm byte must match Config (ChooseRealm defect)"
    );
    eprintln!(
        "charcreate_rt: realm={realm} overview before: {:?}",
        names_in(&sess)
    );

    let tag = match realm {
        2 => "m",
        3 => "h",
        _ => "a",
    };
    let name = create_one(&mut sess, realm, tag);
    drop(sess);

    std::thread::sleep(Duration::from_millis(500));
    let mut sess2 = live_harness::connect(&cfg).map_err(|e| format!("reconnect: {e}"))?;
    ask_for_overview(&mut sess2, realm)?;
    wait_overview(&mut sess2, Instant::now() + Duration::from_secs(30))?;
    assert_eq!(sess2.realm(), realm);
    let names2 = names_in(&sess2);
    eprintln!("charcreate_rt: realm={realm} second login: {names2:?}");
    if !names2.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
        return Err(format!("{name:?} missing on second login (realm {realm})"));
    }
    Ok(name)
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();
    let realms = parse_realms();
    if realms.is_empty() {
        eprintln!("FAIL CAER_REALMS empty");
        std::process::exit(1);
    }

    eprintln!("charcreate_rt: server={server} account={account} realms={realms:?}");

    let mut created = Vec::new();
    for realm in &realms {
        match run_realm(&server, &account, &password, *realm) {
            Ok(name) => {
                eprintln!("PASS realm {realm}: create+relogin ({name})");
                created.push((*realm, name));
            }
            Err(e) => {
                eprintln!("FAIL realm {realm}: {e}");
                std::process::exit(8);
            }
        }
    }

    eprintln!("PASS System 3 create RT: {created:?}");
    if realms == [1, 2, 3] {
        eprintln!("PASS three-realm create + persist falsifier");
    } else {
        eprintln!(
            "NOTE: ran {:?}; System 3 DoD requires CAER_REALMS=1,2,3",
            realms
        );
    }
}
