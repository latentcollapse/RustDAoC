//! System 6 Stream T — player-to-player trade, dual live client.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d \
//! CAER_ACCOUNT2=caer14c CAER_PASSWORD2=caer14c \
//! CAER_CHARACTER=Ca54338 CAER_CHARACTER2=Cc50237 \
//! CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example trade_rt
//! ```
//!
//! Happy path: open trade via PlayerMoveItem to `oid+1000` → both see TradeWindow 0xEA →
//! ModifyTrade update (item/money) → both Accept → InventoryUpdate 0x02 and/or MoneyUpdate 0xFA
//! change on **both** sides.
//!
//! **CANCEL falsifier** (`CAER_TRADE_CANCEL=1`): open trade, offer item/money, ModifyTrade
//! isok=0 → neither side's inventory/money signature may differ from the pre-trade snapshot.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials};
use caer_client::{Config, LiveSession};
use caer_protocol::inventory::ItemData;
use caer_protocol::invverb::{FIRST_BACKPACK, LAST_BACKPACK};
use caer_protocol::money::MoneyUpdate;
use caer_protocol::session::ServerEvent;
use caer_protocol::social::{trade_give_slot, ModifyTradeAction, TradeMoney};

#[derive(Default, Clone)]
struct Track {
    self_id: Option<u16>,
    x: u32,
    y: u32,
    players: HashMap<String, u16>,
    slots: HashMap<u8, ItemData>,
    money: Option<MoneyUpdate>,
    trade_open: u32,
    trade_close: u32,
    chats: Vec<String>,
}

fn apply(t: &mut Track, e: &ServerEvent) {
    match e {
        ServerEvent::PlayerPosition {
            object_id, x, y, ..
        } => {
            if *object_id != 0 {
                t.self_id = Some(*object_id);
            }
            t.x = *x as u32;
            t.y = *y as u32;
        }
        ServerEvent::CharacterJump(j) => {
            t.x = j.x as u32;
            t.y = j.y as u32;
        }
        ServerEvent::PlayerInView(p) => {
            t.players.insert(p.name.clone(), p.object_id);
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
        ServerEvent::MoneyUpdated(m) => t.money = Some(*m),
        ServerEvent::TradeWindow(tw) => {
            if tw.closed {
                t.trade_close += 1;
            } else {
                t.trade_open += 1;
            }
        }
        ServerEvent::ChatMessage { text, .. } => t.chats.push(text.clone()),
        _ => {}
    }
}

fn inv_sig(slots: &HashMap<u8, ItemData>) -> Vec<(u8, u16, u16)> {
    let mut v: Vec<_> = slots
        .iter()
        .filter(|(s, _)| (FIRST_BACKPACK as u8..=LAST_BACKPACK as u8).contains(*s))
        .map(|(s, it)| (*s, it.model, it.unique_id))
        .collect();
    v.sort_by_key(|t| t.0);
    v
}

fn money_sig(m: Option<&MoneyUpdate>) -> (u16, u16, u16, u8, u8) {
    match m {
        Some(m) => (m.platinum, m.mithril, m.gold, m.silver, m.copper),
        None => (0, 0, 0, 0, 0),
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
    eprintln!("trade_rt: connect {account} char={character}");
    let mut sess = live_harness::connect(&cfg).map_err(|e| format!("connect {account}: {e}"))?;
    let mut t = Track::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut t, e)
    })
    .map_err(|e| e.to_string())?;
    Ok((sess, t))
}

fn first_backpack(t: &Track) -> Option<u8> {
    (FIRST_BACKPACK as u8..=LAST_BACKPACK as u8).find(|s| t.slots.contains_key(s))
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
    let cancel = live_harness::env_flag("CAER_TRADE_CANCEL");
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
    let seed = std::env::var("CAER_SEED_ITEM").unwrap_or_else(|_| "bronze_short_sword".into());

    let (mut a, mut ta) =
        connect_world(&account, &password, &character, &server).unwrap_or_else(|e| {
            eprintln!("FAIL {e}");
            std::process::exit(4);
        });
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
            eprintln!("FAIL second: {e}");
            live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
            std::process::exit(4);
        });

    eprintln!("trade_rt: colocate via GM jump");
    let _ = a.command(&format!("jump to {jump_x} {jump_y} {jump_z} 1"));
    let _ = a.command(&format!(
        "jump {character2} to {jump_x} {jump_y} {jump_z} 1"
    ));
    live_harness::drain_pair_or_exit(
        &mut a,
        &mut b,
        Duration::from_secs(4),
        |e| apply(&mut ta, e),
        |e| apply(&mut tb, e),
    );

    // Seed tradable item + a little gold on the GM side.
    if first_backpack(&ta).is_none() && seed != "0" {
        eprintln!("trade_rt: seeding item {seed}");
        let _ = a.command(&format!("item create {seed}"));
        live_harness::drain_pair_or_exit(
            &mut a,
            &mut b,
            Duration::from_secs(4),
            |e| apply(&mut ta, e),
            |e| apply(&mut tb, e),
        );
    }
    let _ = a.command("player money gold 5");
    live_harness::drain_pair_or_exit(
        &mut a,
        &mut b,
        Duration::from_secs(2),
        |e| apply(&mut ta, e),
        |e| apply(&mut tb, e),
    );

    let need = character2.to_ascii_lowercase();
    let oid_b = ta
        .players
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(&need))
        .map(|(_, id)| *id)
        .or(tb.self_id);
    let Some(oid_b) = oid_b else {
        eprintln!("FAIL trade_rt — no partner oid");
        live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
        live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
        std::process::exit(6);
    };
    let Some(from_slot) = first_backpack(&ta) else {
        eprintln!("FAIL trade_rt — no backpack item on A to offer");
        live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
        live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
        std::process::exit(7);
    };

    let snap_a_inv = inv_sig(&ta.slots);
    let snap_b_inv = inv_sig(&tb.slots);
    let snap_a_money = money_sig(ta.money.as_ref());
    let snap_b_money = money_sig(tb.money.as_ref());
    ta.trade_open = 0;
    tb.trade_open = 0;
    ta.trade_close = 0;
    tb.trade_close = 0;

    eprintln!("trade_rt: open trade A→B oid={oid_b} from_slot={from_slot} cancel={cancel}");
    live_harness::require_send(a.target(oid_b), "target trade partner");
    live_harness::drain_pair_or_exit(
        &mut a,
        &mut b,
        Duration::from_secs(1),
        |e| apply(&mut ta, e),
        |e| apply(&mut tb, e),
    );
    live_harness::require_send(
        a.move_item(trade_give_slot(oid_b), u16::from(from_slot), 1),
        "trade open move_item",
    );
    live_harness::drain_pair_or_exit(
        &mut a,
        &mut b,
        Duration::from_secs(4),
        |e| apply(&mut ta, e),
        |e| apply(&mut tb, e),
    );

    if ta.trade_open == 0 && tb.trade_open == 0 {
        eprintln!(
            "FAIL trade_rt — no TradeWindow 0xEA after give-to-player; chats={:?}",
            ta.chats
                .iter()
                .chain(tb.chats.iter())
                .rev()
                .take(8)
                .collect::<Vec<_>>()
        );
        live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
        live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
        std::process::exit(1);
    }
    eprintln!(
        "trade_rt: TradeWindow open A={} B={}",
        ta.trade_open, tb.trade_open
    );

    // Put item + 1 gold on A's offer via ModifyTrade update.
    let mut slots = [0u8; 10];
    slots[0] = from_slot;
    let offer_money = TradeMoney {
        gold: 1,
        ..TradeMoney::default()
    };
    live_harness::require_send(
        a.modify_trade(ModifyTradeAction::Update, false, false, &slots, offer_money),
        "trade update offer",
    );
    live_harness::drain_pair_or_exit(
        &mut a,
        &mut b,
        Duration::from_secs(3),
        |e| apply(&mut ta, e),
        |e| apply(&mut tb, e),
    );

    if cancel {
        eprintln!("trade_rt: CANCEL — ModifyTrade isok=0");
        live_harness::require_send(
            a.modify_trade(
                ModifyTradeAction::Cancel,
                false,
                false,
                &[0; 10],
                TradeMoney::default(),
            ),
            "trade cancel",
        );
        live_harness::drain_pair_or_exit(
            &mut a,
            &mut b,
            Duration::from_secs(4),
            |e| apply(&mut ta, e),
            |e| apply(&mut tb, e),
        );
        let a_inv = inv_sig(&ta.slots);
        let b_inv = inv_sig(&tb.slots);
        let a_m = money_sig(ta.money.as_ref());
        let b_m = money_sig(tb.money.as_ref());
        if a_inv != snap_a_inv || b_inv != snap_b_inv || a_m != snap_a_money || b_m != snap_b_money
        {
            eprintln!(
                "FAIL trade_rt CANCEL — inventory/money changed after cancel\n\
                 A inv {snap_a_inv:?} → {a_inv:?}\n\
                 B inv {snap_b_inv:?} → {b_inv:?}\n\
                 A money {snap_a_money:?} → {a_m:?}\n\
                 B money {snap_b_money:?} → {b_m:?}"
            );
            live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
            live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
            std::process::exit(1);
        }
        eprintln!(
            "PASS trade_rt CANCEL: neither side 0x02/0xFA differed from pre-trade snapshot \
             (TradeWindow close A={} B={})",
            ta.trade_close, tb.trade_close
        );
        live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
        live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
        return;
    }

    // Both accept.
    live_harness::require_send(
        a.modify_trade(ModifyTradeAction::Accept, false, false, &slots, offer_money),
        "trade accept A",
    );
    live_harness::require_send(
        b.modify_trade(
            ModifyTradeAction::Accept,
            false,
            false,
            &[0; 10],
            TradeMoney::default(),
        ),
        "trade accept B",
    );
    live_harness::drain_pair_or_exit(
        &mut a,
        &mut b,
        Duration::from_secs(6),
        |e| apply(&mut ta, e),
        |e| apply(&mut tb, e),
    );

    let a_inv = inv_sig(&ta.slots);
    let b_inv = inv_sig(&tb.slots);
    let a_m = money_sig(ta.money.as_ref());
    let b_m = money_sig(tb.money.as_ref());
    let a_changed = a_inv != snap_a_inv || a_m != snap_a_money;
    let b_changed = b_inv != snap_b_inv || b_m != snap_b_money;
    if !(a_changed && b_changed) {
        eprintln!(
            "FAIL trade_rt — accept did not change both sides via 0x02/0xFA\n\
             A changed={a_changed} {snap_a_inv:?}/{snap_a_money:?} → {a_inv:?}/{a_m:?}\n\
             B changed={b_changed} {snap_b_inv:?}/{snap_b_money:?} → {b_inv:?}/{b_m:?}\n\
             chats={:?}",
            ta.chats
                .iter()
                .chain(tb.chats.iter())
                .filter(|c| c.to_ascii_lowercase().contains("trade"))
                .collect::<Vec<_>>()
        );
        live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
        live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
        std::process::exit(1);
    }

    eprintln!(
        "PASS trade_rt: TradeWindow both sides + accept moved items/money on A and B \
         (observation that fails if broken: local send alone / one-sided 0x02)"
    );
    live_harness::soft_quit(&mut b, |e| apply(&mut tb, e));
    live_harness::soft_quit(&mut a, |e| apply(&mut ta, e));
}
