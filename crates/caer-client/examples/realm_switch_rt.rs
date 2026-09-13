//! Live pre-world realm-switch falsifier.
//!
//! This is intentionally one authenticated DOL session, rather than three new connections.  A
//! realm plate click must put a `CharacterOverviewRequest` for the clicked realm on the wire and
//! receive that realm's overview promptly.  Before the lab's `allow_all_realms` configuration was
//! fixed, the server kept answering an account-bound Albion list for Midgard/Hibernia; the UI then
//! appeared to hang while waiting for the character it had just created.
//!
//! ```bash
//! CAER_SERVER=127.0.0.1:10321 CAER_ACCOUNT=admin CAER_PASSWORD=0000 \
//!   cargo run -p caer-client --example realm_switch_rt
//! ```
//!
//! The check creates and deletes nothing. `CAER_REALMS` defaults to `1,2,3` and can be narrowed
//! for diagnosis (for example `CAER_REALMS=1,3`).

use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials};
use caer_client::{Closed, Config, LiveSession};
use caer_protocol::session::{ServerEvent, SessionPhase};

const AUTH_TIMEOUT: Duration = Duration::from_secs(20);
const OVERVIEW_TIMEOUT: Duration = Duration::from_secs(20);

fn parse_realms() -> Vec<u8> {
    std::env::var("CAER_REALMS")
        .unwrap_or_else(|_| "1,2,3".into())
        .split(',')
        .filter_map(|realm| realm.trim().parse::<u8>().ok())
        .filter(|realm| (1..=3).contains(realm))
        .collect()
}

fn wait_until_realm_ready(session: &mut LiveSession) -> Result<(), String> {
    let deadline = Instant::now() + AUTH_TIMEOUT;
    while Instant::now() < deadline {
        if !matches!(
            session.phase(),
            SessionPhase::Disconnected
                | SessionPhase::CryptHandshake
                | SessionPhase::Authenticating
        ) {
            return Ok(());
        }
        session
            .poll()
            .map_err(|error| format!("closed while authenticating: {error:?}"))?;
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!(
        "timeout authenticating (last phase {:?})",
        session.phase()
    ))
}

/// DOL normally stamps a realm on every character summary. A zero is tolerated because some
/// compatible servers omit it; a *different non-zero* realm is the cross-realm overview bug.
fn verify_realm_local(expected: u8, summaries: &[(String, u8)]) -> Result<(), String> {
    let wrong: Vec<_> = summaries
        .iter()
        .filter(|(_, realm)| *realm != 0 && *realm != expected)
        .collect();
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "requested realm {expected}, but overview included other-realm characters {wrong:?}"
        ))
    }
}

fn request_realm_overview(
    session: &mut LiveSession,
    realm: u8,
) -> Result<(Duration, Vec<(String, u8)>), String> {
    let started = Instant::now();
    session
        .request_character_overview(realm)
        .map_err(|error| format!("send CharacterOverviewRequest({realm}): {error}"))?;

    let deadline = started + OVERVIEW_TIMEOUT;
    while Instant::now() < deadline {
        let events = session.poll().map_err(|Closed(message)| {
            format!("closed waiting for realm {realm} overview: {message}")
        })?;
        if events
            .iter()
            .any(|event| matches!(event, ServerEvent::CharacterOverview(_)))
        {
            let summaries = session
                .overview()
                .map(|overview| {
                    overview
                        .characters
                        .iter()
                        .map(|character| (character.name.clone(), character.realm))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            verify_realm_local(realm, &summaries)?;
            return Ok((started.elapsed(), summaries));
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    Err(format!("timeout waiting for CharacterOverview({realm})"))
}

fn main() {
    let credentials = LiveCredentials::from_env();
    let realms = parse_realms();
    if realms.is_empty() {
        eprintln!("FAIL realm_switch_rt: CAER_REALMS has no realm in 1..=3");
        std::process::exit(1);
    }

    let mut config = Config::new(
        credentials.server.clone(),
        credentials.account.clone(),
        credentials.password.clone(),
    );
    // The connection must arrive at the unbound realm plate; the test itself drives every realm
    // request and can therefore prove the same-session transition rather than an auto-selected
    // login realm.
    config.auto_select = false;

    eprintln!(
        "realm_switch_rt: server={} account={} realms={realms:?}",
        credentials.server, credentials.account
    );
    let mut session = live_harness::connect(&config).unwrap_or_else(|error| {
        eprintln!("FAIL realm_switch_rt: connect: {error}");
        std::process::exit(2);
    });
    wait_until_realm_ready(&mut session).unwrap_or_else(|error| {
        eprintln!("FAIL realm_switch_rt: {error}");
        std::process::exit(3);
    });

    for realm in realms {
        let (elapsed, summaries) =
            request_realm_overview(&mut session, realm).unwrap_or_else(|error| {
                eprintln!("FAIL realm_switch_rt: {error}");
                std::process::exit(4);
            });
        eprintln!(
            "PASS realm_switch_rt: realm={realm} overview={} character(s) in {} ms {summaries:?}",
            summaries.len(),
            elapsed.as_millis(),
        );
    }

    eprintln!("PASS realm_switch_rt: same-session realm overview route");
}

#[cfg(test)]
mod tests {
    use super::verify_realm_local;

    #[test]
    fn red_other_realm_summary_is_rejected() {
        let summaries = vec![("AlbionOnly".to_owned(), 1)];
        let error = verify_realm_local(3, &summaries).expect_err("realm 3 must reject Albion row");
        assert!(error.contains("requested realm 3"));
    }

    #[test]
    fn green_requested_realm_and_unspecified_rows_are_accepted() {
        let summaries = vec![
            ("Hibernia".to_owned(), 3),
            ("CompatibleServer".to_owned(), 0),
        ];
        verify_realm_local(3, &summaries).expect("realm-local rows must pass");
    }
}
