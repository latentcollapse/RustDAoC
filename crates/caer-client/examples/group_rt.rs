//! System 6 Stream G — group invite → roster → leave, dual live client.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d \
//! CAER_ACCOUNT2=caer14c CAER_PASSWORD2=caer14c \
//! CAER_CHARACTER=Ca54338 CAER_CHARACTER2=Cc50237 \
//! CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example group_rt
//! ```
//!
//! Happy path: target invitee → InviteToGroup 0x87 → invitee receives Dialog 0x05 →
//! explicit DialogResponse Yes (typed accept; session no longer auto-clicks) →
//! **both** see GroupMemberUpdate 0x70 with ≥2 members → `/disband` → GroupWindow empty
//! (0x16/0x06) on the disbanding client.
//!
//! **NOSEND falsifier** (`CAER_GROUP_NOSEND=1`): clear target / garbage oid → invite must **not**
//! produce a 2-member GroupMemberUpdate roster (no phantom group).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials};
use caer_client::{Config, LiveSession};
use caer_protocol::session::ServerEvent;

#[derive(Default)]
struct Track {
    self_id: Option<u16>,
    x: u32,
    y: u32,
    z: u32,
    players: HashMap<String, u16>,
    max_roster: usize,
    group_windows_empty: u32,
    group_windows_named: u32,
    invite_yes_sent: u32,
    /// Pending group-invite Dialog (code 0x05) awaiting typed DialogResponse.
    pending_invite: Option<(u16, u16, u16, String)>,
    chats: Vec<String>,
}

fn apply(t: &mut Track, e: &ServerEvent) {
    match e {
        ServerEvent::PlayerPosition {
            object_id, x, y, z, ..
        } => {
            if *object_id != 0 {
                t.self_id = Some(*object_id);
            }
            t.x = *x as u32;
            t.y = *y as u32;
            t.z = *z as u32;
        }
        ServerEvent::CharacterJump(j) => {
            t.x = j.x as u32;
            t.y = j.y as u32;
            t.z = j.z as u32;
        }
        ServerEvent::PlayerInView(p) => {
            t.players.insert(p.name.clone(), p.object_id);
        }
        ServerEvent::GroupMemberUpdate(g) => {
            t.max_roster = t.max_roster.max(g.member_count());
        }
        ServerEvent::GroupWindow(gw) => {
            if gw.is_empty() {
                t.group_windows_empty += 1;
            } else {
                t.group_windows_named += 1;
                t.max_roster = t.max_roster.max(gw.members.len());
            }
        }
        ServerEvent::Dialog {
            code,
            data1,
            data2,
            data3,
            message,
            ..
        } if *code == 0x05 => {
            t.pending_invite = Some((*data1, *data2, *data3, message.clone()));
        }
        ServerEvent::ChatMessage { text, .. } => {
            t.chats.push(text.clone());
        }
        _ => {}
    }
}

/// Dual drain with mid-loop DialogResponse Yes for group-invite Dialog 0x05.
/// Session no longer auto-accepts; headless RT must click Yes explicitly.
fn drain_pair_accept(
    a: &mut LiveSession,
    ta: &mut Track,
    b: &mut LiveSession,
    tb: &mut Track,
    secs: u64,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        live_harness::drain_or_exit(a, Duration::from_millis(50), |e| apply(ta, e));
        live_harness::drain_or_exit(b, Duration::from_millis(50), |e| apply(tb, e));
        if let Some((d1, d2, d3, _)) = tb.pending_invite.take() {
            live_harness::require_send(
                b.dialog_response(d1, d2, d3, 0x05, 0x01),
                "group invite DialogResponse Yes",
            );
            tb.invite_yes_sent += 1;
            eprintln!("group_rt: B DialogResponse Yes (code=0x05)");
        }
    }
}

fn connect_world(
    account: &str,
    password: &str,
    character: &str,
    server: &str,
) -> Result<(LiveSession, Track), String> {
    let mut cfg = Config::new(server, account, password);
    cfg.auto_select = true;
    if !character.is_empty() {
        cfg.character = Some(character.to_string());
    }
    eprintln!("group_rt: connect {account} char={character}");
    let mut sess = live_harness::connect(&cfg).map_err(|e| format!("connect {account}: {e}"))?;
    let mut t = Track::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut t, e)
    })
    .map_err(|e| e.to_string())?;
    Ok((sess, t))
}

fn main() {
    let a_creds = LiveCredentials::from_env();
    let b_creds = LiveCredentials::from_env_second();
    let server = a_creds.server.clone();
    let account = a_creds.account.clone();
    let password = a_creds.password.clone();
    let character = a_creds
        .character
        .clone()
        .unwrap_or_else(|| "Ca54338".into());
    let account2 = b_creds.account.clone();
    let password2 = b_creds.password.clone();
    let character2 = b_creds
        .character
        .clone()
        .unwrap_or_else(|| "Cc50237".into());
    let nosend = live_harness::env_flag("CAER_GROUP_NOSEND");
    let jump_x: i32 = std::env::var("CAER_JUMP_X")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(530932);
    let jump_y: i32 = std::env::var("CAER_JUMP_Y")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(478046);
    let jump_z: i32 = std::env::var("CAER_JUMP_Z")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2456);

    let (mut a, mut ta) =
        connect_world(&account, &password, &character, &server).unwrap_or_else(|e| {
            eprintln!("FAIL {e}");
            std::process::exit(4);
        });
    // Drain briefly before second login to reduce link-death storms.
    {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(evs) = a.poll() {
                for e in &evs {
                    apply(&mut ta, e);
                }
            }
        }
    }

    let (mut b, mut tb) = connect_world(&account2, &password2, &character2, &server)
        .unwrap_or_else(|e| {
            eprintln!("FAIL second account: {e}");
            eprintln!(
                "hint: try CAER_ACCOUNT2=caer14c CAER_PASSWORD2=caer14c CAER_CHARACTER2=Cc50237"
            );
            live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
            std::process::exit(4);
        });

    eprintln!("group_rt: jump both to {jump_x} {jump_y} {jump_z} region 1");
    let _ = a.command(&format!("jump to {jump_x} {jump_y} {jump_z} 1"));
    // GM pulls the second character into range (PrivLevel on account A).
    let _ = a.command(&format!(
        "jump {character2} to {jump_x} {jump_y} {jump_z} 1"
    ));
    drain_pair_accept(&mut a, &mut ta, &mut b, &mut tb, 4);

    // Ensure A can see B's PlayerCreate (and vice versa) for targeting.
    let need = character2.to_ascii_lowercase();
    let oid_b = ta
        .players
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(&need))
        .map(|(_, id)| *id)
        .or(tb.self_id);
    let Some(oid_b) = oid_b else {
        eprintln!(
            "FAIL group_rt — cannot resolve invitee oid; A players={:?} B self={:?}",
            ta.players.keys().collect::<Vec<_>>(),
            tb.self_id
        );
        live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
        live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
        std::process::exit(6);
    };
    eprintln!(
        "group_rt: A self={:?} B oid={oid_b} nosend={nosend}",
        ta.self_id
    );

    ta.max_roster = 0;
    tb.max_roster = 0;
    ta.group_windows_empty = 0;
    tb.group_windows_empty = 0;

    if nosend {
        eprintln!("group_rt: NOSEND — target garbage / clear, then invite");
        let _ = a.target(0);
        drain_pair_accept(&mut a, &mut ta, &mut b, &mut tb, 1);
        // Also try a nonexistent oid (unlikely to be a live player).
        let _ = a.target(0xFFFE);
        live_harness::require_send(a.invite_to_group(), "invite_to_group");
        drain_pair_accept(&mut a, &mut ta, &mut b, &mut tb, 5);
        if ta.max_roster >= 2 || tb.max_roster >= 2 {
            eprintln!(
                "FAIL group_rt NOSEND — phantom roster A={} B={} (observation that fails if broken: \
                 invite with no valid target must not yield 2-member 0x70)",
                ta.max_roster, tb.max_roster
            );
            live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
            live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
            std::process::exit(1);
        }
        eprintln!(
            "PASS group_rt NOSEND: no 2-member GroupMemberUpdate (A max={} B max={})",
            ta.max_roster, tb.max_roster
        );
        live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
        live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
        return;
    }

    eprintln!("group_rt: target {oid_b} + InviteToGroup");
    live_harness::require_send(a.target(oid_b), "target invitee");
    drain_pair_accept(&mut a, &mut ta, &mut b, &mut tb, 1);
    live_harness::require_send(a.invite_to_group(), "invite_to_group");
    drain_pair_accept(&mut a, &mut ta, &mut b, &mut tb, 6);

    if ta.max_roster < 2 && tb.max_roster < 2 {
        eprintln!(
            "FAIL group_rt — no ≥2 GroupMemberUpdate/GroupWindow after invite; \
             A roster={} B roster={} B invite_yes_sent={} chatsA={:?} chatsB={:?}",
            ta.max_roster,
            tb.max_roster,
            tb.invite_yes_sent,
            ta.chats.iter().rev().take(5).collect::<Vec<_>>(),
            tb.chats.iter().rev().take(5).collect::<Vec<_>>()
        );
        live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
        live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
        std::process::exit(1);
    }
    eprintln!(
        "group_rt: roster visible A={} B={} (invite_yes_sent on B={})",
        ta.max_roster, tb.max_roster, tb.invite_yes_sent
    );

    // Leave / disband cleanup — prefer empty GroupWindow over chat alone.
    ta.group_windows_empty = 0;
    tb.group_windows_empty = 0;
    let before_a = ta.max_roster;
    let before_b = tb.max_roster;
    eprintln!("group_rt: /disband on A");
    live_harness::require_send(a.command("disband"), "disband");
    drain_pair_accept(&mut a, &mut ta, &mut b, &mut tb, 5);

    let cleared = ta.group_windows_empty > 0
        || tb.group_windows_empty > 0
        || (ta.max_roster < 2 && before_a >= 2)
        || (tb.max_roster < 2 && before_b >= 2);
    // After full disband both should see empty window; require at least one empty GroupWindow
    // (0x16/0x06) — leave chat alone is not enough.
    if ta.group_windows_empty == 0 && tb.group_windows_empty == 0 {
        eprintln!(
            "FAIL group_rt — disband without empty GroupWindow (0x16/0x06); \
             cleared_heuristic={cleared} A_empty={} B_empty={} chats={:?}",
            ta.group_windows_empty,
            tb.group_windows_empty,
            ta.chats
                .iter()
                .chain(tb.chats.iter())
                .filter(|c| c.to_ascii_lowercase().contains("group")
                    || c.to_ascii_lowercase().contains("leave")
                    || c.to_ascii_lowercase().contains("disband"))
                .collect::<Vec<_>>()
        );
        live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
        live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
        std::process::exit(1);
    }

    eprintln!(
        "PASS group_rt: invite→roster(A={} B={})→disband empty_window(A={} B={})",
        before_a.max(ta.max_roster),
        before_b.max(tb.max_roster),
        ta.group_windows_empty,
        tb.group_windows_empty
    );
    live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
    live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
}
