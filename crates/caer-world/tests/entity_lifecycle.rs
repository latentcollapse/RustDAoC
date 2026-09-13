//! End-to-end entity lifecycle over a real captured session (B.6).
//!
//! The unit tests in `lib.rs` prove `remove` keeps the columns consistent and that a decoded
//! `PlayerCreate` lands in the world. What they cannot show is whether any of it actually *fires*
//! against a real server's packet stream — the decode could be wired to a code the server never
//! sends (exactly the `0xD4` vs `0x4B` trap this slice hit) and every unit test would stay green.
//!
//! So this replays a full captured session and measures the thing the slice exists to fix: how
//! many entities the world would have accumulated without culling.
//!
//! **Skipped when the capture is absent.** `captures/` is gitignored — these are real session
//! traces, not committed fixtures — so this test is a no-op on any machine without them, in the
//! same spirit as the client-asset gate in `caer-render`'s golden tests.

use caer_protocol::session::ServerEvent;

/// A long Camelot Hills session: ~16k events, two other players present, entities coming and going.
const CAPTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../captures/cap_20260714_230416_conn41042.bin"
);

fn trace() -> Option<Vec<ServerEvent>> {
    let buf = std::fs::read(CAPTURE).ok()?;
    Some(caer_world::replay::decode_trace(&buf))
}

/// Culling must actually happen, and must measurably shrink the world versus ignoring deletes.
///
/// The delta is the leak: before this slice the client had no removal path at all, so every one of
/// these entities stayed forever at its last known position.
#[test]
fn object_delete_culls_departed_entities_over_a_real_session() {
    let Some(events) = trace() else { return }; // no capture on this machine

    let removals = events
        .iter()
        .filter(|e| matches!(e, ServerEvent::ObjectRemoved { .. }))
        .count();
    assert!(removals > 0, "capture should contain ObjectDelete packets");

    let mut culled = caer_world::WorldState::new();
    for e in &events {
        culled.apply(e);
    }

    // The same replay with removals suppressed — i.e. the old behaviour.
    let mut leaky = caer_world::WorldState::new();
    for e in &events {
        if !matches!(e, ServerEvent::ObjectRemoved { .. }) {
            leaky.apply(e);
        }
    }

    assert!(
        culled.len() < leaky.len(),
        "culling changed nothing: {} entities either way — is ObjectDelete reaching the world?",
        culled.len()
    );
    // Sanity: culling must not empty the world either. A bug that removed the wrong index (or
    // every entity) would still satisfy the assertion above.
    assert!(
        culled.len() > 100,
        "culling removed far too much: only {} left",
        culled.len()
    );
}

/// Other players must actually decode out of a real stream, with plausible identity.
///
/// This is the test that would have caught the wrong packet code: wired to the enum's `0xD4`, the
/// server never sends it and this finds zero players.
#[test]
fn player_create_decodes_from_a_real_session() {
    let Some(events) = trace() else { return };

    let players: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ServerEvent::PlayerInView(p) => Some(p),
            _ => None,
        })
        .collect();

    assert!(
        !players.is_empty(),
        "no players decoded — is PlayerCreate wired to the right code?"
    );
    for p in &players {
        assert!(
            !p.name.is_empty(),
            "player has no name — the pascal strings are misaligned"
        );
        assert!(
            p.name.is_ascii(),
            "player name {:?} is not plausible text",
            p.name
        );
        assert!(
            p.level > 0 && p.level <= 50,
            "implausible level {}",
            p.level
        );
        assert!(p.heading < 4096, "heading {} out of range", p.heading);
        assert!((1..=3).contains(&p.realm), "implausible realm {}", p.realm);
    }

    // And they must reach the world as players, not as a nameless placeholder.
    let mut w = caer_world::WorldState::new();
    for e in &events {
        w.apply(e);
    }
    let named = players
        .iter()
        .filter(|p| w.get(p.object_id).is_some_and(|e| e.name == p.name))
        .count();
    assert!(named > 0, "no decoded player survived into the world model");
}
