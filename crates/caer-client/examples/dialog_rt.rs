//! System 4 verb — one NPC CustomDialog (0x81 code 0x06) + DialogResponse 0x82.
//!
//! OPEN_ORACLE path: spawn `WeaponCraftingMaster` → ObjectInteract → whisper `Weaponcrafters`
//! (`WeaponCraftingMaster.GuildOrder`) → CustomDialog → Yes → crafting acceptance message.
//! Named fake: Pass on interact chat alone without Dialog packet + DialogResponse outcome.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example dialog_rt
//! ```
//!
//! **Decline falsifier:** `CAER_DIALOG_DECLINE=1` — respond No; must NOT see Accepted chat.

use std::time::Duration;

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::{Config, LiveSession};
use caer_protocol::session::ServerEvent;

/// EN `WeaponCraftingMaster.GuildOrder`.
const GUILD_ORDER: &str = "Weaponcrafters";
/// EN `CraftNPC.CraftNpcDialogResponse.Accepted` fragment.
const ACCEPTED_NEEDLE: &str = "accepted by";
const CUSTOM_DIALOG: u8 = 0x06;

#[derive(Default)]
struct Track {
    x: u32,
    y: u32,
    npcs: Vec<(u16, String)>,
    chats: Vec<String>,
    dialog: Option<(u8, u16, u16, u16, String)>,
    saw_crafting_update: bool,
}

fn apply(t: &mut Track, e: &ServerEvent) {
    match e {
        ServerEvent::PlayerPosition { x, y, .. } => {
            t.x = *x as u32;
            t.y = *y as u32;
        }
        ServerEvent::NpcInView(n) => {
            t.npcs.push((n.object_id, n.name.clone()));
        }
        ServerEvent::ChatMessage { text, .. } => {
            t.chats.push(text.clone());
        }
        ServerEvent::Dialog {
            code,
            data1,
            data2,
            data3,
            message,
            ..
        } => {
            t.dialog = Some((*code, *data1, *data2, *data3, message.clone()));
        }
        ServerEvent::Raw { code, payload }
            if *code == 0x16 && !payload.is_empty() && payload[0] == 0x08 =>
        {
            // VariousUpdate subcode 0x08 = SendUpdateCraftingSkills.
            t.saw_crafting_update = true;
        }
        _ => {}
    }
}

fn pick_crafter(t: &Track) -> Option<(u16, String)> {
    if let Ok(oid) = std::env::var("CAER_DIALOG_OID") {
        if let Ok(id) = oid.parse::<u16>() {
            let name = t
                .npcs
                .iter()
                .find(|(o, _)| *o == id)
                .map(|(_, n)| n.clone())
                .unwrap_or_else(|| "forced".into());
            return Some((id, name));
        }
    }
    t.npcs.iter().rev().find_map(|(id, name)| {
        let lower = name.to_ascii_lowercase();
        if lower.contains("weapon") || lower.contains("craft") || lower.contains("smith") {
            Some((*id, name.clone()))
        } else {
            None
        }
    })
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();
    let decline = std::env::var("CAER_DIALOG_DECLINE").ok().as_deref() == Some("1");
    let response: u8 = if decline { 0x00 } else { 0x01 };

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;

    eprintln!("dialog_rt: server={server} account={account} decline={decline}");
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));

    if pick_crafter(&track).is_none() {
        eprintln!("dialog_rt: spawning DOL.GS.WeaponCraftingMaster");
        let _ = sess.command("mob create DOL.GS.WeaponCraftingMaster");
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));
    }
    let Some((oid, name)) = pick_crafter(&track) else {
        // Last resort: newest NPC.
        let Some((oid, name)) = track.npcs.last().cloned() else {
            eprintln!("FAIL no NPC in view after spawn");
            std::process::exit(2);
        };
        eprintln!("dialog_rt: using last NPC oid={oid} name={name:?}");
        run_dialog(&mut sess, &mut track, oid, &name, response, decline);
        return;
    };
    run_dialog(&mut sess, &mut track, oid, &name, response, decline);
}

fn run_dialog(
    sess: &mut LiveSession,
    track: &mut Track,
    oid: u16,
    name: &str,
    response: u8,
    decline: bool,
) {
    eprintln!("dialog_rt: target oid={oid} name={name:?}");
    track.dialog = None;
    track.chats.clear();
    track.saw_crafting_update = false;

    let _ = sess.target(oid);
    if let Err(e) = sess.interact(track.x, track.y, oid) {
        eprintln!("FAIL interact: {e}");
        std::process::exit(3);
    }
    live_harness::drain_or_exit(sess, Duration::from_secs(3), |e| apply(track, e));

    let whisper = std::env::var("CAER_DIALOG_WHISPER").unwrap_or_else(|_| GUILD_ORDER.into());
    eprintln!("dialog_rt: whisper {whisper:?}");
    let _ = sess.command(&format!("whisper {whisper}"));
    live_harness::drain_or_exit(sess, Duration::from_secs(4), |e| apply(track, e));

    let Some((code, data1, data2, data3, msg)) = track.dialog.clone() else {
        eprintln!(
            "FAIL no CustomDialog 0x81 — interact/whisper did not open dialog; chats={:?}",
            track.chats
        );
        std::process::exit(1);
    };
    if code != CUSTOM_DIALOG {
        eprintln!("FAIL Dialog code=0x{code:02x} expected CustomDialog 0x06; msg={msg:?}");
        std::process::exit(1);
    }
    eprintln!("dialog_rt: CustomDialog data1={data1} data2={data2} msg={msg:?}");

    track.chats.clear();
    track.saw_crafting_update = false;
    if let Err(e) = sess.dialog_response(data1, data2, data3, CUSTOM_DIALOG, response) {
        eprintln!("FAIL dialog_response: {e}");
        std::process::exit(3);
    }
    live_harness::drain_or_exit(sess, Duration::from_secs(4), |e| apply(track, e));

    let accepted = track
        .chats
        .iter()
        .any(|c| c.to_ascii_lowercase().contains(ACCEPTED_NEEDLE));

    if decline {
        if accepted || track.saw_crafting_update {
            eprintln!(
                "FAIL dialog_rt DECLINE — server accepted anyway; chats={:?} craft={}",
                track.chats, track.saw_crafting_update
            );
            std::process::exit(1);
        }
        eprintln!(
            "PASS dialog_rt DECLINE — Dialog+No; no acceptance (chats={:?})",
            track.chats
        );
        let _ = sess.quit();
        return;
    }

    // Pass requires Dialog packet (already) + real outcome — not interact chat alone.
    if !accepted && !track.saw_crafting_update {
        eprintln!(
            "FAIL dialog_rt — DialogResponse Yes but no acceptance/crafting update; chats={:?}",
            track.chats
        );
        std::process::exit(1);
    }
    eprintln!(
        "PASS dialog_rt — CustomDialog+Yes; accepted={accepted} craft_vu={}; msg={msg:?}",
        track.saw_crafting_update
    );
    let _ = sess.quit();
}
