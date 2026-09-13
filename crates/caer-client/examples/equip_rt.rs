//! System 4 verb 1 live falsifier — equip via PlayerMoveItem 0xDD, require server confirm.
//!
//! Does **not** mutate the avatar locally: if the server rejects the move, equipment items must
//! stay unchanged (Claude falsifier). Tracks inventory/equipment from the event stream only —
//! no optimistic WorldState write.
//!
//! ```bash
//! CAER_ACCOUNT=caer14d CAER_PASSWORD=caer14d CAER_SERVER=127.0.0.1:10311 \
//!   cargo \
//!   run -p caer-client --example equip_rt
//! ```
//!
//! Optional: `CAER_EQUIP_FROM` / `CAER_EQUIP_TO` (decimal slots). Default: first backpack item
//! → paperdoll (100).
//!
//! **Reject falsifier:** `CAER_EQUIP_REJECT=1` moves from an empty backpack slot. Server must not
//! change worn inventory; client must not invent an equip (Claude: avatar unchanged on reject).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use caer_client::live_harness::{self, LiveCredentials, LiveExit};
use caer_client::{Closed, Config};
use caer_protocol::equipment::VisibleItem;
use caer_protocol::inventory::ItemData;
use caer_protocol::invverb::{FIRST_BACKPACK, LAST_BACKPACK};
use caer_protocol::session::ServerEvent;

#[derive(Default)]
struct InvTrack {
    /// Local player backpack / worn names from 0x02.
    slots: HashMap<u8, ItemData>,
    /// Last EquipmentUpdate items for self (object_id matching entered player, or first seen).
    self_id: Option<u16>,
    equipment: Vec<VisibleItem>,
}

fn apply(track: &mut InvTrack, e: &ServerEvent) {
    match e {
        ServerEvent::InventoryUpdated(u) => {
            for entry in &u.items {
                match &entry.item {
                    Some(it) => {
                        track.slots.insert(entry.slot, it.clone());
                    }
                    None => {
                        track.slots.remove(&entry.slot);
                    }
                }
            }
        }
        ServerEvent::EquipmentUpdated(u) => {
            if track.self_id.is_none() || track.self_id == Some(u.object_id) {
                track.self_id = Some(u.object_id);
                track.equipment = u.items.clone();
            }
        }
        ServerEvent::PlayerPosition { object_id, .. }
            if *object_id != 0 && track.self_id.is_none() =>
        {
            // Prefer the oid that owns PlayerPosition as Self_ when we have not yet locked.
            track.self_id = Some(*object_id);
        }
        _ => {}
    }
}

fn equip_signature(items: &[VisibleItem]) -> Vec<(u8, u16, Option<u8>)> {
    let mut v: Vec<_> = items
        .iter()
        .map(|i| (i.slot, i.model, i.extension))
        .collect();
    v.sort_by_key(|t| t.0);
    v
}

/// Worn inventory signature — **unique_id**, not model. Equipping bronze→bronze keeps model 3
/// and used to look "unchanged" while `wield_chat` greened SCN-06 (Claude rejection 2026-08-09).
fn worn_unique_sig(slots: &HashMap<u8, ItemData>) -> Vec<(u8, u16, u16)> {
    let mut v: Vec<_> = slots
        .iter()
        .filter(|(s, _)| (10..=37).contains(*s))
        .map(|(s, it)| (*s, it.model, it.unique_id))
        .collect();
    v.sort_by_key(|t| t.0);
    v
}

fn main() {
    let creds = LiveCredentials::from_env();
    let server = creds.server.clone();
    let account = creds.account.clone();
    let password = creds.password.clone();
    let character = std::env::var("CAER_CHARACTER").ok();

    let mut cfg = Config::new(&server, &account, &password);
    cfg.auto_select = true;
    if let Some(name) = character {
        cfg.character = Some(name);
    }

    eprintln!("equip_rt: server={server} account={account}");
    let mut sess = live_harness::connect(&cfg).unwrap_or_else(|e| live_harness::exit_connect(e));
    let mut track = InvTrack::default();
    live_harness::wait_entered_world(&mut sess, live_harness::world_deadline(), |e| {
        apply(&mut track, e)
    })
    .unwrap_or_else(|e| {
        LiveExit::from_wait_err(&e).bail(format!("FAIL world: {e}"));
    });
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(3), |e| apply(&mut track, e));

    eprintln!(
        "equip_rt: self_id={:?} inventory filled slots={}",
        track.self_id,
        track.slots.len()
    );

    // Fresh characters often have an empty bag (DOL skips 0x02 when the range is empty). Seed a
    // visible torso piece via GM `/item create` when allowed (PrivLevel≥2). Override with
    // CAER_SEED_ITEM=<Id_nb> or CAER_SEED_ITEM=0 to skip.
    // Default: level-1 bronze sword (model 3) so a fresh Armsman can equip without /level.
    // For armour silhouette shots: CAER_SEED_ITEM=daringstuddedjerkin_alb after /player level 5+.
    let seed = std::env::var("CAER_SEED_ITEM").unwrap_or_else(|_| "bronze_short_sword".into());
    let need_seed = (FIRST_BACKPACK..=LAST_BACKPACK).all(|s| !track.slots.contains_key(&(s as u8)));
    if need_seed && seed != "0" {
        eprintln!("equip_rt: backpack empty — seeding via &item create {seed}");
        sess.command(&format!("item create {seed}"))
            .unwrap_or_else(|e| {
                eprintln!("FAIL seed command: {e}");
                std::process::exit(11);
            });
        let seed_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < seed_deadline {
            match sess.poll() {
                Ok(evs) => {
                    for e in &evs {
                        apply(&mut track, e);
                    }
                }
                Err(Closed(msg)) => {
                    eprintln!("FAIL closed during seed: {msg}");
                    std::process::exit(12);
                }
            }
            if (FIRST_BACKPACK..=LAST_BACKPACK).any(|s| track.slots.contains_key(&(s as u8))) {
                break;
            }
        }
        eprintln!("equip_rt: after seed, filled slots={}", track.slots.len());
    }

    let from = std::env::var("CAER_EQUIP_FROM")
        .ok()
        .and_then(|s| s.parse().ok());
    let to = std::env::var("CAER_EQUIP_TO")
        .ok()
        .and_then(|s| s.parse().ok());

    let reject = std::env::var("CAER_EQUIP_REJECT").ok().as_deref() == Some("1");
    let forced = from.is_some() && to.is_some();

    // Happy path: if the hand already holds a weapon, unique_id is often 0 and bronze→bronze
    // looks unchanged while chat still says "wield" (Claude SCN-06 rejection). Unequip first so
    // the worn occupancy delta is mandatory, then pick a bag source and equip to paperdoll.
    if !reject {
        const RIGHT_HAND: u8 = 10;
        if track.slots.contains_key(&RIGHT_HAND) {
            let Some(empty_bag) = (FIRST_BACKPACK..=LAST_BACKPACK)
                .map(|s| s as u8)
                .find(|s| !track.slots.contains_key(s))
            else {
                eprintln!("FAIL no empty backpack slot to unequip into");
                std::process::exit(13);
            };
            let before_unequip = worn_unique_sig(&track.slots);
            eprintln!(
                "equip_rt: unequip worn slot {RIGHT_HAND} → bag {empty_bag} before equip \
                 (force worn occupancy change; unique_id often 0)"
            );
            sess.move_item(u16::from(empty_bag), u16::from(RIGHT_HAND), 1)
                .unwrap_or_else(|e| {
                    eprintln!("FAIL unequip send: {e}");
                    std::process::exit(7);
                });
            let unequip_deadline = Instant::now() + Duration::from_secs(10);
            let mut saw = false;
            while Instant::now() < unequip_deadline {
                match sess.poll() {
                    Ok(evs) => {
                        for e in &evs {
                            if matches!(e, ServerEvent::InventoryUpdated(_)) {
                                saw = true;
                            }
                            if let ServerEvent::ChatMessage { text, .. } = e {
                                eprintln!("equip_rt: chat: {text}");
                            }
                            apply(&mut track, e);
                        }
                    }
                    Err(Closed(msg)) => {
                        eprintln!("FAIL closed during unequip: {msg}");
                        std::process::exit(8);
                    }
                }
                if saw && worn_unique_sig(&track.slots) != before_unequip {
                    break;
                }
            }
            let after_unequip = worn_unique_sig(&track.slots);
            if after_unequip == before_unequip {
                eprintln!(
                    "FAIL unequip did not change worn set (before={before_unequip:?} after={after_unequip:?})"
                );
                std::process::exit(14);
            }
            eprintln!("equip_rt: unequip ok worn {before_unequip:?} → {after_unequip:?}");
        }
    }

    let (from_slot, to_slot, item_name) = if reject {
        let empty = (FIRST_BACKPACK..=LAST_BACKPACK)
            .find(|s| !track.slots.contains_key(&(*s as u8)))
            .unwrap_or(LAST_BACKPACK);
        eprintln!(
            "equip_rt: REJECT mode — MoveItem from empty slot {empty} → paperdoll (expect no worn change)"
        );
        (empty, 100u16, "empty-reject".into())
    } else if forced {
        (from.unwrap(), to.unwrap(), "forced".into())
    } else {
        let Some((slot_u8, name, _)) = (FIRST_BACKPACK..=LAST_BACKPACK).find_map(|s| {
            let s8 = s as u8;
            track
                .slots
                .get(&s8)
                .map(|it| (s8, it.name.clone(), it.model))
        }) else {
            eprintln!("FAIL no backpack item in slots {FIRST_BACKPACK}..{LAST_BACKPACK}");
            eprintln!("  filled slots: {:?}", {
                let mut k: Vec<_> = track.slots.keys().copied().collect();
                k.sort_unstable();
                k
            });
            eprintln!(
                "  hint: GM PrivLevel≥2 + CAER_SEED_ITEM, or put an item in the bag, or set CAER_EQUIP_FROM/TO"
            );
            std::process::exit(6);
        };
        // Paperdoll (100): server remaps to item.Item_Type (OPEN_ORACLE MoveItem handler).
        (u16::from(slot_u8), 100u16, name)
    };

    let _before = equip_signature(&track.equipment);
    let before_worn = worn_unique_sig(&track.slots);
    let before_inv: Vec<(u8, String)> = {
        let mut v: Vec<_> = track
            .slots
            .iter()
            .map(|(s, it)| (*s, it.name.clone()))
            .collect();
        v.sort_by_key(|t| t.0);
        v
    };
    eprintln!(
        "equip_rt: move {item_name:?} from={from_slot} → to={to_slot}; worn before={before_worn:?}; inv={before_inv:?}"
    );

    sess.move_item(to_slot, from_slot, 1).unwrap_or_else(|e| {
        eprintln!("FAIL send move_item: {e}");
        std::process::exit(7);
    });

    let deadline = Instant::now()
        + if reject {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(15)
        };
    let mut saw_inv = false;
    while Instant::now() < deadline {
        match sess.poll() {
            Ok(evs) => {
                for e in &evs {
                    match &e {
                        ServerEvent::InventoryUpdated(_) => {
                            saw_inv = true;
                        }
                        ServerEvent::EquipmentUpdated(u) => {
                            eprintln!(
                                "equip_rt: 0x15 oid={} n_items={}",
                                u.object_id,
                                u.items.len()
                            );
                        }
                        ServerEvent::ChatMessage { text, .. } => {
                            // Diagnostic only — never a Pass arm (Claude SCN-06 rejection).
                            eprintln!("equip_rt: chat: {text}");
                        }
                        _ => {}
                    }
                    apply(&mut track, e);
                }
            }
            Err(Closed(msg)) => {
                eprintln!("FAIL closed waiting equip: {msg}");
                std::process::exit(8);
            }
        }
        if !reject && saw_inv && worn_unique_sig(&track.slots) != before_worn {
            break;
        }
    }
    live_harness::drain_or_exit(&mut sess, Duration::from_secs(1), |e| apply(&mut track, e));

    let after_worn = worn_unique_sig(&track.slots);
    let after = equip_signature(&track.equipment);
    let after_inv: Vec<(u8, String)> = {
        let mut v: Vec<_> = track
            .slots
            .iter()
            .map(|(s, it)| (*s, it.name.clone()))
            .collect();
        v.sort_by_key(|t| t.0);
        v
    };

    if reject {
        if after_worn != before_worn {
            eprintln!(
                "FAIL reject-equip: worn unique set changed without a valid source item \
                 (before={before_worn:?} after={after_worn:?})"
            );
            std::process::exit(15);
        }
        eprintln!(
            "PASS equip_rt REJECT: empty-slot MoveItem left worn unchanged {before_worn:?} \
             (no optimistic equip; peer_equip={after:?})"
        );
        let _ = sess.quit();
        return;
    }

    // OPEN_ORACLE: UpdateEquipmentAppearance broadcasts 0x15 to *other* players only. Self_
    // confirmation is InventoryUpdate on worn slots. Chat is not evidence (SCN-06 named fake).
    // WorldState projects worn inventory → equipment_of(Self_) for the render binding.
    if !saw_inv {
        eprintln!("FAIL no InventoryUpdate after MoveItem");
        eprintln!("  worn={after_worn:?} inv={after_inv:?}");
        std::process::exit(9);
    }
    if after_worn == before_worn {
        eprintln!(
            "FAIL worn unique set unchanged after MoveItem — chat/model-only is not enough \
             (REQ-020 / SCN-06). before={before_worn:?} after={after_worn:?} inv={after_inv:?}"
        );
        std::process::exit(10);
    }

    eprintln!(
        "PASS equip_rt: MoveItem confirmed by 0x02 worn unique slots {before_worn:?} → {after_worn:?} \
         (self 0x15 peers-only; peer_equip_log={after:?}; chat is not a Pass arm)"
    );
    let _ = sess.quit();
}
