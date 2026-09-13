//! System 4 verb — loot via PickUpRequest 0xB5 (OPEN_ORACLE ground loot path).
//!
//! DOL corpse `DropLoot` spawns `WorldInventoryItem` / `GameMoney` on the ground; autoloot then
//! calls `PickupObject`. GM `IsWorthReward` blocks DropLoot when PrivLevel>1, so this RT drives
//! the same pickup contract: drop backpack → ground ObjectCreate → target → PickUpRequest →
//! InventoryUpdate. Named fake: chat "You get …" without inventory change.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example loot_rt
//! ```
//!
//! **Empty-pickup falsifier:** `CAER_LOOT_EMPTY=1` — PickUpRequest with no target; inventory
//! must stay put.

use std::collections::HashMap;
use std::time::Duration;

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::Config;
use caer_protocol::inventory::ItemData;
use caer_protocol::invverb::{FIRST_BACKPACK, LAST_BACKPACK};
use caer_protocol::session::ServerEvent;

const GROUND_SLOT: u16 = 1; // eInventorySlot.Ground

#[derive(Default)]
struct Track {
    x: u32,
    y: u32,
    slots: HashMap<u8, ItemData>,
    ground: Vec<(u16, String)>,
}

fn apply(t: &mut Track, e: &ServerEvent) {
    match e {
        ServerEvent::PlayerPosition { x, y, .. } => {
            t.x = *x as u32;
            t.y = *y as u32;
        }
        ServerEvent::InventoryUpdated(u) => {
            for entry in &u.items {
                match &entry.item {
                    Some(it) => {
                        t.slots.insert(entry.slot, it.clone());
                    }
                    None => {
                        t.slots.remove(&entry.slot);
                    }
                }
            }
        }
        ServerEvent::ObjectInView(o) => {
            t.ground.push((o.object_id, o.name.clone()));
        }
        ServerEvent::ObjectRemoved { object_id } => {
            t.ground.retain(|(id, _)| id != object_id);
        }
        _ => {}
    }
}

fn backpack_sig(slots: &HashMap<u8, ItemData>) -> Vec<(u8, u16, String)> {
    let mut v: Vec<_> = slots
        .iter()
        .filter(|(s, _)| {
            let s = u16::from(**s);
            (FIRST_BACKPACK..=LAST_BACKPACK).contains(&s)
        })
        .map(|(s, it)| (*s, it.unique_id, it.name.clone()))
        .collect();
    v.sort_by_key(|(s, ..)| *s);
    v
}

fn pick_drop_slot(t: &Track) -> Option<u8> {
    if let Ok(s) = std::env::var("CAER_LOOT_FROM") {
        if let Ok(slot) = s.parse::<u8>() {
            return Some(slot);
        }
    }
    (FIRST_BACKPACK..=LAST_BACKPACK).find_map(|s| {
        let s = s as u8;
        t.slots.get(&s).map(|_| s)
    })
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();
    let empty = std::env::var("CAER_LOOT_EMPTY").ok().as_deref() == Some("1");
    let seed = std::env::var("CAER_SEED_ITEM").unwrap_or_else(|_| "practice_sword".into());

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;

    eprintln!("loot_rt: server={server} account={account} empty={empty}");
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));

    if empty {
        let before = backpack_sig(&track.slots);
        eprintln!("loot_rt: EMPTY — pickup with no target; before={before:?}");
        let _ = sess.target(0);
        if let Err(e) = sess.pickup(track.x, track.y, 0) {
            eprintln!("loot_rt: pickup err (ok for empty): {e}");
        }
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));
        let after = backpack_sig(&track.slots);
        if after != before {
            eprintln!("FAIL loot_rt EMPTY — inventory changed {before:?} → {after:?}");
            std::process::exit(1);
        }
        eprintln!("PASS loot_rt EMPTY — inventory unchanged {after:?}");
        let _ = sess.quit();
        return;
    }

    if pick_drop_slot(&track).is_none() {
        eprintln!("loot_rt: seeding via &item create {seed}");
        let _ = sess.command(&format!("item create {seed}"));
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));
    }
    let Some(from) = pick_drop_slot(&track) else {
        eprintln!("FAIL no backpack item to drop (tried seed {seed})");
        std::process::exit(2);
    };
    let dropped = track.slots.get(&from).cloned().unwrap();
    let before_drop = backpack_sig(&track.slots);
    eprintln!(
        "loot_rt: drop slot={from} unique={} name={:?} → ground",
        dropped.unique_id, dropped.name
    );

    // Corpse flavor: spawn + kill (GM DropLoot usually skipped; still leaves a corpse nearby).
    let _ = sess.command("mob create");
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));
    let _ = sess.command("mob kill");
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));

    let ground_before: Vec<_> = track.ground.clone();
    if let Err(e) = sess.move_item(GROUND_SLOT, u16::from(from), 1) {
        eprintln!("FAIL drop move_item: {e}");
        std::process::exit(3);
    }
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));

    let after_drop = backpack_sig(&track.slots);
    if after_drop
        .iter()
        .any(|(s, uid, _)| *s == from && *uid == dropped.unique_id)
    {
        eprintln!("FAIL drop — item still in backpack slot {from}");
        std::process::exit(1);
    }
    let new_ground: Vec<_> = track
        .ground
        .iter()
        .filter(|(id, _)| !ground_before.iter().any(|(b, _)| b == id))
        .cloned()
        .collect();
    let oid = if let Some((id, name)) = new_ground.first() {
        eprintln!("loot_rt: ground ObjectInView oid={id} name={name:?}");
        *id
    } else if let Some((id, name)) = track.ground.iter().rev().find(|(_, n)| {
        n.to_ascii_lowercase()
            .contains(&dropped.name.to_ascii_lowercase())
    }) {
        eprintln!("loot_rt: matched ground by name oid={id} name={name:?}");
        *id
    } else {
        eprintln!(
            "FAIL no ground ObjectInView after drop; ground={:?} before_drop={before_drop:?} after={after_drop:?}",
            track.ground
        );
        std::process::exit(1);
    };

    let before_pick = backpack_sig(&track.slots);
    let named_before = track
        .slots
        .values()
        .filter(|it| it.name.eq_ignore_ascii_case(&dropped.name))
        .count();
    // TargetObject must be set before PickUpRequest — oracle reads TargetObject, not wire oid.
    let _ = sess.target(oid);
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));
    if let Err(e) = sess.pickup(track.x, track.y, oid) {
        eprintln!("FAIL pickup: {e}");
        std::process::exit(3);
    }
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(4), |e| apply(&mut track, e));
    // Retry once if the first pickup raced the target settle.
    let named_mid = track
        .slots
        .values()
        .filter(|it| it.name.eq_ignore_ascii_case(&dropped.name))
        .count();
    if named_mid <= named_before {
        eprintln!("loot_rt: retry target+pickup (first attempt may have raced)");
        let _ = sess.target(oid);
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));
        let _ = sess.pickup(track.x, track.y, oid);
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(4), |e| apply(&mut track, e));
    }
    let after_pick = backpack_sig(&track.slots);
    let named_after = track
        .slots
        .values()
        .filter(|it| it.name.eq_ignore_ascii_case(&dropped.name))
        .count();
    let ground_gone = !track.ground.iter().any(|(id, _)| *id == oid);

    let recovered = named_after > named_before
        || (dropped.unique_id != 0
            && after_pick
                .iter()
                .any(|(_, uid, _)| *uid == dropped.unique_id));
    if !recovered {
        eprintln!(
            "FAIL loot_rt — pickup did not restore item; named {named_before}→{named_after}; \
             before={before_pick:?} after={after_pick:?} ground_gone={ground_gone} oid={oid}"
        );
        std::process::exit(1);
    }
    if !ground_gone {
        eprintln!(
            "loot_rt: warn — ground oid {oid} still in view after pickup (server may delay delete)"
        );
    }
    eprintln!(
        "PASS loot_rt — dropped name={:?} unique={} oid={oid}; named {named_before}→{named_after}; \
         backpack {before_pick:?} → {after_pick:?}",
        dropped.name, dropped.unique_id
    );
    let _ = sess.quit();
}
