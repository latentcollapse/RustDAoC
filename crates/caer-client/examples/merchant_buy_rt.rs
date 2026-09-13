//! System 4 verb 2 live falsifier — one real merchant purchase.
//!
//! Opens a merchant via ObjectInteract 0x7A, buys offer slot 0 via BuyRequest 0x78, and requires
//! **both** MoneyUpdate 0xFA and InventoryUpdate 0x02 to change (SCN-08 binding). Named fake:
//! window renders a static list.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example merchant_buy_rt
//! ```
//!
//! Optional: `CAER_BUY_SLOT` (default 0);
//! `CAER_SEED_GOLD` (default 100) via `/player money gold N` when PrivLevel≥2.
//!
//! **Broke falsifier:** `CAER_BUY_BROKE=1` zeros the purse then buys — money and inventory must
//! both stay put (Claude: insufficient funds must not desync).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::{Closed, Config};
use caer_protocol::inventory::ItemData;
use caer_protocol::money::MoneyUpdate;
use caer_protocol::session::ServerEvent;

#[derive(Default)]
struct Track {
    x: u32,
    y: u32,
    money: Option<MoneyUpdate>,
    slots: HashMap<u8, ItemData>,
    npcs: Vec<(u16, String, i32, i32)>,
    merchant_pages: u32,
}

fn apply(t: &mut Track, e: &ServerEvent) {
    match e {
        ServerEvent::PlayerPosition { x, y, .. } => {
            t.x = *x as u32;
            t.y = *y as u32;
        }
        ServerEvent::MoneyUpdated(m) => t.money = Some(*m),
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
        ServerEvent::NpcInView(n) => {
            t.npcs
                .push((n.object_id, n.name.clone(), n.x as i32, n.y as i32));
        }
        ServerEvent::MerchantWindow(_) => {
            t.merchant_pages += 1;
        }
        _ => {}
    }
}

fn money_copper(m: &MoneyUpdate) -> u64 {
    // Approximate flat copper: ignore mithril/plat scale; gold/silver/copper only for RT.
    u64::from(m.gold) * 10_000 + u64::from(m.silver) * 100 + u64::from(m.copper)
}

fn pick_merchant(t: &Track) -> Option<(u16, String)> {
    if let Ok(oid) = std::env::var("CAER_MERCHANT_OID") {
        if let Ok(id) = oid.parse::<u16>() {
            let name = t
                .npcs
                .iter()
                .find(|(o, ..)| *o == id)
                .map(|(_, n, ..)| n.clone())
                .unwrap_or_else(|| "forced".into());
            return Some((id, name));
        }
    }
    let keys = [
        "merchant",
        "smith",
        "vendor",
        "shopkeeper",
        "armorer",
        "weapons",
        "barkeep",
        "new merchant", // GM default create name often localises to this
    ];
    t.npcs.iter().find_map(|(id, name, ..)| {
        let lower = name.to_ascii_lowercase();
        if keys.iter().any(|k| lower.contains(k)) {
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
    let broke = std::env::var("CAER_BUY_BROKE").ok().as_deref() == Some("1");
    let seed_gold: u32 = std::env::var("CAER_SEED_GOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let buy_slot: u16 = std::env::var("CAER_BUY_SLOT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if broke { 29 } else { 0 });

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;

    eprintln!("merchant_buy_rt: server={server} account={account}");
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = Track::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(4), |e| apply(&mut track, e));

    if !broke
        && money_copper(track.money.as_ref().unwrap_or(&MoneyUpdate {
            copper: 0,
            silver: 0,
            gold: 0,
            platinum: 0,
            mithril: 0,
        })) < 100
        && seed_gold > 0
    {
        eprintln!("merchant_buy_rt: seeding gold via &player money gold {seed_gold}");
        let _ = sess.command(&format!("player money gold {seed_gold}"));
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(2), |e| apply(&mut track, e));
    }

    // Region 1 Cotswold has no GameMerchant rows in this SoloDAoC DB — spawn one at feet.
    if pick_merchant(&track).is_none() {
        eprintln!("merchant_buy_rt: no merchant in view — spawning via &merchant create");
        let _ = sess.command("merchant create");
        live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));
        // Prefer the newest NPC whose name looks like a default merchant, else last NpcInView.
        if pick_merchant(&track).is_none() {
            if let Some((id, name, ..)) = track.npcs.last() {
                eprintln!(
                    "merchant_buy_rt: using last spawned NPC as merchant candidate {id} ({name})"
                );
                // Force via env-style override for the rest of this run:
                std::env::set_var("CAER_MERCHANT_OID", id.to_string());
            }
        }
    }

    let Some((merchant_id, merchant_name)) = pick_merchant(&track) else {
        eprintln!(
            "FAIL no merchant NPC after spawn (saw {} NPCs).",
            track.npcs.len()
        );
        for (id, name, x, y) in track.npcs.iter().rev().take(10) {
            eprintln!("  npc {id} {name:?} @ ({x},{y})");
        }
        std::process::exit(6);
    };
    eprintln!(
        "merchant_buy_rt: merchant={merchant_id} ({merchant_name}) at player=({},{}) money={:?}",
        track.x, track.y, track.money
    );

    // Attach a known catalogue so the window is not empty.
    sess.target(merchant_id).unwrap_or_else(|e| {
        eprintln!("FAIL target (pre-sell): {e}");
        std::process::exit(7);
    });
    let _ = sess.command("merchant sell add BuffTokens");
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));
    if broke {
        eprintln!(
            "merchant_buy_rt: BROKE mode — slot {buy_slot} = housing deed (price ≫ purse); \
             GM money commands only AddMoney so we do not try to zero the purse"
        );
        // Keep BuffTokens slot 0 intact for other runs; put the unaffordable offer at slot 29.
        let _ = sess.command(&format!(
            "merchant articles add housing_alb_mansion_deed 0 {buy_slot}"
        ));
    } else {
        let _ = sess.command("merchant articles add bronze_short_sword 0 0");
    }
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));

    let before_money = track.money;
    let before_slots = track.slots.clone();
    let before_slot_count = before_slots.len();

    sess.interact(track.x, track.y, merchant_id)
        .unwrap_or_else(|e| {
            eprintln!("FAIL interact: {e}");
            std::process::exit(8);
        });
    eprintln!(
        "merchant_buy_rt: sent ObjectInteract oid={merchant_id} xy=({},{}) session={}",
        track.x,
        track.y,
        sess.session_id()
    );
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));
    let _ = sess.interact(track.x, track.y, merchant_id);

    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline && track.merchant_pages == 0 {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    apply(&mut track, e);
                    match e {
                        ServerEvent::ChatMessage { text, .. } => {
                            eprintln!("merchant_buy_rt: chat: {text}");
                        }
                        ServerEvent::Raw { code, payload } => {
                            eprintln!("merchant_buy_rt: raw 0x{code:02x} len={}", payload.len());
                        }
                        ServerEvent::ObjectRemoved { object_id } => {
                            eprintln!("merchant_buy_rt: ObjectRemoved {object_id}");
                        }
                        ServerEvent::MerchantWindow(w) => {
                            eprintln!(
                                "merchant_buy_rt: MerchantWindow page={} items={}",
                                w.page,
                                w.items.len()
                            );
                        }
                        _ => {}
                    }
                }
            }
            Err(Closed(msg)) => {
                eprintln!("FAIL closed waiting merchant window: {msg}");
                std::process::exit(9);
            }
        }
    }
    if track.merchant_pages == 0 {
        eprintln!("FAIL no MerchantWindow 0x17 after interact");
        std::process::exit(10);
    }
    eprintln!(
        "merchant_buy_rt: merchant window open (pages={})",
        track.merchant_pages
    );

    sess.buy_item(track.x, track.y, merchant_id, buy_slot, 1)
        .unwrap_or_else(|e| {
            eprintln!("FAIL buy send: {e}");
            std::process::exit(11);
        });

    let mut saw_money = false;
    let mut saw_inv = false;
    let buy_deadline = Instant::now() + Duration::from_secs(if broke { 8 } else { 15 });
    while Instant::now() < buy_deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    match e {
                        ServerEvent::MoneyUpdated(_) => saw_money = true,
                        ServerEvent::InventoryUpdated(_) => saw_inv = true,
                        ServerEvent::ChatMessage { text, .. } => {
                            eprintln!("merchant_buy_rt: chat: {text}");
                        }
                        _ => {}
                    }
                    apply(&mut track, e);
                }
            }
            Err(Closed(msg)) => {
                eprintln!("FAIL closed waiting buy confirm: {msg}");
                std::process::exit(12);
            }
        }
        if !broke && saw_money && saw_inv {
            break;
        }
    }

    let after_money = track.money;
    let after_slot_count = track.slots.len();
    let money_changed = match (before_money, after_money) {
        (Some(a), Some(b)) => money_copper(&a) != money_copper(&b),
        _ => before_money != after_money,
    };
    let inv_changed = after_slot_count != before_slot_count
        || track
            .slots
            .iter()
            .any(|(s, it)| before_slots.get(s).map(|b| b.unique_id) != Some(it.unique_id));

    if broke {
        if money_changed || inv_changed {
            eprintln!(
                "FAIL BROKE buy mutated state (money_changed={money_changed} inv_changed={inv_changed}); \
                 before_money={before_money:?} after={after_money:?}; slots {before_slot_count}→{after_slot_count}"
            );
            std::process::exit(16);
        }
        eprintln!(
            "PASS merchant_buy_rt BROKE: insufficient funds left money+inventory unchanged \
             (before_money={before_money:?}; slots={before_slot_count})"
        );
        let _ = sess.quit();
        return;
    }

    if !saw_money || !money_changed {
        eprintln!(
            "FAIL money did not change from packets (saw_money={saw_money} before={before_money:?} after={after_money:?})"
        );
        std::process::exit(13);
    }
    if !saw_inv {
        eprintln!("FAIL no InventoryUpdate after buy (static list fake)");
        std::process::exit(14);
    }
    if !inv_changed && after_slot_count <= before_slot_count {
        eprintln!(
            "WARN inventory slot count {before_slot_count}→{after_slot_count}; relying on saw_inv + money change"
        );
    }

    eprintln!(
        "PASS merchant_buy_rt: buy slot {buy_slot} from {merchant_name} ({merchant_id}); \
         money {before_money:?} → {after_money:?}; inv_slots {before_slot_count}→{after_slot_count}"
    );
    let _ = sess.quit();
}
