//! System 6 Stream Q — NPC quest subscribe dialog (acceptance half).
//!
//! OPEN_ORACLE: Master Frederick / ImportantDelivery — interact → whisper `training` →
//! whisper `proceed` → Dialog 0x81 code **0x64** (QuestSubscribe) → DialogResponse 0x82 →
//! accept yields QuestEntry **0x83** with non-empty name; decline leaves quest state
//! **byte-identical** to the pre-response snapshot.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example quest_rt
//! ```
//!
//! **Decline falsifier:** `CAER_QUEST_DECLINE=1` — DialogResponse No; QuestEntry payload map
//! must match the snapshot taken before the response.

use std::collections::BTreeMap;
use std::time::Duration;

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::{Config, LiveSession};
use caer_protocol::session::ServerEvent;

/// Cotswold — Master Frederick (BaseFrederickQuest / ImportantDelivery).
const FRED_X: i32 = 567_969;
const FRED_Y: i32 = 509_880;
const FRED_Z: i32 = 2_861;
const QUEST_SUBSCRIBE: u8 = 0x64;
const NAME_NEEDLE: &str = "important delivery";

#[derive(Default)]
struct Track {
    x: u32,
    y: u32,
    npcs: Vec<(u16, String)>,
    chats: Vec<String>,
    dialog: Option<(u8, u16, u16, u16, String)>,
    /// index → last raw QuestEntry payload (quest-log state).
    quest_payloads: BTreeMap<u8, Vec<u8>>,
    /// Decoded non-clear entries seen after the DialogResponse (accept arm).
    post_entries: Vec<(u8, String, String)>,
}

fn apply(t: &mut Track, e: &ServerEvent) {
    match e {
        ServerEvent::PlayerPosition { x, y, .. } => {
            t.x = *x as u32;
            t.y = *y as u32;
        }
        ServerEvent::CharacterJump(j) => {
            t.x = j.x as u32;
            t.y = j.y as u32;
            eprintln!(
                "quest_rt: CharacterJump -> ({}, {}, {}) oid={}",
                j.x, j.y, j.z, j.object_id
            );
        }
        ServerEvent::NpcInView(n) => {
            t.npcs.retain(|(id, _)| *id != n.object_id);
            t.npcs.push((n.object_id, n.name.clone()));
        }
        ServerEvent::ObjectRemoved { object_id } => {
            t.npcs.retain(|(id, _)| id != object_id);
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
        ServerEvent::QuestEntry { entry, payload } => {
            t.quest_payloads.insert(entry.index, payload.clone());
            if !entry.is_clear() {
                t.post_entries
                    .push((entry.index, entry.name.clone(), entry.description.clone()));
            }
        }
        _ => {}
    }
}

fn pick_frederick(t: &Track) -> Option<(u16, String)> {
    if let Ok(oid) = std::env::var("CAER_QUEST_OID") {
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
        if lower.contains("frederick") || lower.contains("master frederick") {
            Some((*id, name.clone()))
        } else {
            None
        }
    })
}

fn snapshot_log(t: &Track) -> BTreeMap<u8, Vec<u8>> {
    t.quest_payloads.clone()
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();
    let decline = std::env::var("CAER_QUEST_DECLINE").ok().as_deref() == Some("1");
    let response: u8 = if decline { 0x00 } else { 0x01 };

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;
    if let Ok(name) = std::env::var("CAER_CHARACTER") {
        cfg.character = Some(name);
    }

    eprintln!(
        "quest_rt: server={server} account={account} decline={decline} \
         frederick_world=({FRED_X},{FRED_Y},{FRED_Z})"
    );
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));

    // Needs GM (PrivLevel≥2). Always re-bind ImportantDelivery + clear any in-progress
    // ImportantDelivery on this character — CanGiveQuest returns ≤0 when already on the
    // quest, which makes TalkToMasterFrederick return before Interact/Whisper (examine
    // still works; Dialog 0x64 never opens). Claude's both-arm miss is this class of
    // failure when verify hits a dirty character or an unbound Frederick.
    //
    // Keep /code lines short — long one-liners have bitten DOL's chat compiler ("Newline in
    // constant") when someone wrapped them in try/catch. Prefer several short commands.
    track.npcs.clear();
    track.chats.clear();
    eprintln!("quest_rt: ensure Frederick + abort stale ImportantDelivery");
    let _ = sess.command(
        "code var q=player.IsDoingQuest(typeof(DOL.GS.Quests.Albion.ImportantDelivery)); if(q!=null){q.AbortQuest(); print(\"aborted ImportantDelivery\");} else print(\"no active ImportantDelivery\");",
    );
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));
    let _ = sess.command(
        "code try{DOL.GS.Quests.Albion.ImportantDelivery.ScriptUnloaded(null,null,null);}catch(Exception e){print(\"unload:\"+e.Message);}",
    );
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));
    let _ = sess.command(
        "code DOL.GS.Quests.Albion.ImportantDelivery.ScriptLoaded(null,null,null); print(\"ImportantDelivery ScriptLoaded\");",
    );
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));
    let _ = sess.command(
        "code var f=DOL.GS.Quests.Albion.BaseFrederickQuest.GetMasterFrederick(); print(f==null?\"Frederick:null\":(\"Master Frederick:\"+f.ObjectID+\":\"+f.Position)); if(f!=null) player.MoveTo(f.Position);",
    );
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(4), |e| apply(&mut track, e));
    for oid in 1u16..80 {
        let _ = sess.request_npc(oid);
    }
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(4), |e| apply(&mut track, e));
    eprintln!(
        "quest_rt: after ensure pos=({}, {}) npcs={} names={:?} chats={:?}",
        track.x,
        track.y,
        track.npcs.len(),
        track
            .npcs
            .iter()
            .rev()
            .take(12)
            .map(|(_, n)| n.as_str())
            .collect::<Vec<_>>(),
        track.chats.iter().rev().take(8).collect::<Vec<_>>()
    );

    // Prefer NpcInView; fall back to ObjectID from /code chat (create packets can lag).
    let (oid, name) = if let Some(p) = pick_frederick(&track) {
        p
    } else if let Some((oid, name)) = track.chats.iter().rev().find_map(|c| {
        let c = c.trim();
        let rest = c.strip_prefix("Master Frederick:")?;
        let oid: u16 = rest.split(':').next()?.parse().ok()?;
        Some((oid, "Master Frederick".into()))
    }) {
        eprintln!("quest_rt: using Frederick oid={oid} from /code chat (no NpcInView yet)");
        let _ = sess.request_npc(oid);
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));
        (oid, name)
    } else {
        eprintln!(
            "FAIL no Master Frederick in view (pos={},{} npcs={:?} chats={:?}). \
             QUARANTINE: known SoloDAoC live-only Frederick flake on long-lived servers \
             (no Mob DB row; ScriptLoaded self-heal). Need GM caer14d + in-world Frederick; \
             restart DOL or re-bind quest scripts outside fragile long /code strings.",
            track.x,
            track.y,
            track.npcs.iter().rev().take(16).collect::<Vec<_>>(),
            track.chats.iter().rev().take(12).collect::<Vec<_>>()
        );
        std::process::exit(2);
    };

    run_quest(&mut sess, &mut track, oid, &name, response, decline);
}

fn run_quest(
    sess: &mut LiveSession,
    track: &mut Track,
    oid: u16,
    name: &str,
    response: u8,
    decline: bool,
) {
    eprintln!("quest_rt: target oid={oid} name={name:?}");
    track.dialog = None;
    track.chats.clear();
    track.post_entries.clear();

    // Stand on the NPC — WHISPER_DISTANCE is only 512; jump+interact alone can leave us
    // just outside whisper range while examine still works.
    let _ = sess.command(&format!(
        "code var f=WorldMgr.GetObjectByIDFromRegion(player.CurrentRegionID,{oid}) as GameNPC; if(f!=null) player.MoveTo(f.Position); else print(\"no npc {oid}\");"
    ));
    live_harness::drain_or_exit(sess, Duration::from_secs(2), |e| apply(track, e));

    let _ = sess.target(oid);
    if let Err(e) = sess.interact(track.x, track.y, oid) {
        eprintln!("FAIL interact: {e}");
        std::process::exit(3);
    }
    live_harness::drain_or_exit(sess, Duration::from_secs(4), |e| apply(track, e));

    let greeted = track.chats.iter().any(|c| {
        let l = c.to_ascii_lowercase();
        l.contains("greetings") || l.contains("training") || l.contains("master frederick says")
    });
    if !greeted {
        eprintln!(
            "FAIL interact did not trigger ImportantDelivery TalkToMasterFrederick \
             (need CanGiveQuest>0 + handlers bound). chats={:?}",
            track.chats
        );
        std::process::exit(1);
    }

    eprintln!("quest_rt: whisper training");
    let _ = sess.target(oid);
    let _ = sess.command("whisper training");
    live_harness::drain_or_exit(sess, Duration::from_secs(3), |e| apply(track, e));

    eprintln!("quest_rt: whisper proceed");
    let _ = sess.target(oid);
    let _ = sess.command("whisper proceed");
    live_harness::drain_or_exit(sess, Duration::from_secs(5), |e| apply(track, e));

    // Wait a bit more if dialog not yet seen.
    if track.dialog.is_none() {
        live_harness::drain_or_exit(sess, Duration::from_secs(4), |e| apply(track, e));
    }

    let Some((code, data1, data2, data3, msg)) = track.dialog.clone() else {
        eprintln!(
            "FAIL no QuestSubscribe Dialog 0x64 — chats={:?}",
            track.chats
        );
        std::process::exit(1);
    };
    if code != QUEST_SUBSCRIBE {
        eprintln!("FAIL Dialog code=0x{code:02x} expected QuestSubscribe 0x64; msg={msg:?}");
        std::process::exit(1);
    }
    eprintln!(
        "quest_rt: QuestSubscribe data1(quest)={data1} data2(npc)={data2} data3={data3} msg={msg:?}"
    );

    // Pre-response snapshot (falsifier baseline).
    let before = snapshot_log(track);
    eprintln!(
        "quest_rt: snapshot {} QuestEntry slot(s) before DialogResponse",
        before.len()
    );

    track.post_entries.clear();
    if let Err(e) = sess.dialog_response(data1, data2, data3, QUEST_SUBSCRIBE, response) {
        eprintln!("FAIL dialog_response: {e}");
        std::process::exit(3);
    }
    live_harness::drain_or_exit(sess, Duration::from_secs(5), |e| apply(track, e));

    if decline {
        let after = snapshot_log(track);
        if after != before {
            eprintln!(
                "FAIL quest_rt DECLINE — quest state changed; before={before:?} after={after:?}"
            );
            std::process::exit(1);
        }
        // Also: no new non-clear entry should have been recorded post-response.
        let accepted = track
            .post_entries
            .iter()
            .any(|(_, n, _)| n.to_ascii_lowercase().contains(NAME_NEEDLE));
        if accepted {
            eprintln!(
                "FAIL quest_rt DECLINE — saw ImportantDelivery QuestEntry anyway: {:?}",
                track.post_entries
            );
            std::process::exit(1);
        }
        eprintln!(
            "PASS quest_rt DECLINE — Dialog+No; quest payloads byte-identical ({} slots)",
            before.len()
        );
        let _ = sess.quit();
        live_harness::cleanup_drain(sess, Duration::from_secs(2), |e| apply(track, e));
        return;
    }

    let got = track.post_entries.iter().find(|(_, n, _)| {
        let lower = n.to_ascii_lowercase();
        lower.contains(NAME_NEEDLE) || lower.contains("delivery")
    });
    let Some((idx, qname, qdesc)) = got else {
        eprintln!(
            "FAIL quest_rt — accept but no QuestEntry 0x83 with quest name; \
             post_entries={:?}; chats={:?}; payloads={:?}",
            track.post_entries, track.chats, track.quest_payloads
        );
        std::process::exit(1);
    };
    eprintln!(
        "PASS quest_rt — QuestSubscribe+Yes; QuestEntry idx={idx} name={qname:?} \
         desc_len={} provenance={}",
        qdesc.len(),
        caer_protocol::quest::PROVENANCE
    );
    let _ = sess.quit();
    live_harness::cleanup_drain(sess, Duration::from_secs(2), |e| apply(track, e));
}
