//! End-to-end walk of the pre-world product path, GPU-free.
//!
//! Every piece of this flow has unit coverage, but nothing walked the whole sequence a player
//! actually performs. The defects fixed on 2026-08-14/15 were mostly *seams*: a screen that
//! reverted between two correct halves (B5), a selection that resolved against a stale list (H4),
//! a label that disagreed with the id it dispatched (B2). Seam defects survive per-function tests
//! and die here.
//!
//! Simulates: realm plate → choose realm → overview arrives → open create → pick race/class/gender
//! → name → Continue → customize → optional stats modal → customizer Continue → back at character
//! select → select → Play. The stats form only Reset/Optimizes local points; the customizer's
//! source Continue is the single CreateCharacter boundary. Continué still refuses before a name
//! is typed — the name box is on the create form, so that is where the gate lives.

use caer_protocol::overview::{CharacterOverview, CharacterSummary};
use caer_protocol::session::SessionPhase;
use caer_render::live::LiveCommand;
use caer_render::preworld::{PreWorldAction, PreWorldScreen};
use caer_render::preworld_flow::{FlowEvent, FlowStep, PreWorldFlow};
use caer_render::preworld_product::{
    apply_create_name_input, dispatch_preworld_action, PreWorldProductState,
};

fn character(slot: u8, name: &str) -> CharacterSummary {
    CharacterSummary {
        slot,
        level: 1,
        name: name.into(),
        location: "Camelot Hills".into(),
        class_name: "Armsman".into(),
        race_name: "Briton".into(),
        region: 1,
        class_id: 2,
        realm: 1,
        stats: [60; 8],
        race_gender: 1,
        ..Default::default()
    }
}

fn overview(chars: Vec<CharacterSummary>) -> CharacterOverview {
    CharacterOverview {
        flags: 0,
        characters: chars,
    }
}

/// The full creation walk for one realm, asserting the screen at each step and the exact packets.
fn walk_realm(realm: u8, race_slot: u8, class_slot: u8, expect_class: &str) {
    let mut flow = PreWorldFlow::from_observed_phase(0, SessionPhase::RealmSelect);
    let mut state = PreWorldProductState::default();

    // --- realm plate -------------------------------------------------------------------------
    assert_eq!(flow.screen(), Some(PreWorldScreen::RealmSelect));

    let r = dispatch_preworld_action(&mut state, PreWorldAction::ChooseRealm(realm));
    assert!(
        r.refused.is_none(),
        "realm {realm} refused: {:?}",
        r.refused
    );
    assert!(
        r.commands.iter().any(
            |c| matches!(c, LiveCommand::RequestCharacterOverview { realm: got } if *got == realm)
        ),
        "choosing realm {realm} must request that realm's overview"
    );
    flow.on_event(FlowEvent::RealmChosen(realm));
    assert!(
        flow.is_awaiting_server(),
        "the click must be acknowledged while the overview is in flight"
    );

    // B5: the socket keeps reporting the old phase until the reply lands. The plate must hold.
    for _ in 0..30 {
        flow.observe_phase(SessionPhase::RealmSelect);
    }
    assert_eq!(
        flow.screen(),
        Some(PreWorldScreen::RealmSelect),
        "realm {realm}: stale phase ticks moved the screen"
    );

    // --- overview arrives --------------------------------------------------------------------
    state.overview = Some(overview(vec![]));
    flow.on_event(FlowEvent::OverviewReady);
    assert_eq!(flow.step(), FlowStep::CharSelect);
    assert!(!flow.is_awaiting_server());

    // --- open the creation form --------------------------------------------------------------
    let r = dispatch_preworld_action(&mut state, PreWorldAction::OpenCharCreate);
    assert!(r.refused.is_none());
    flow.on_event(FlowEvent::OpenCreate);
    assert_eq!(flow.screen(), Some(PreWorldScreen::CharCreate));
    assert_eq!(
        state.create_draft.realm, realm,
        "the form must open on the chosen realm"
    );

    // The form must open on a combination Continue would accept, not an illegal seed.
    assert!(
        state.create_draft.model_matches_identity(),
        "realm {realm}: freshly opened form has a desynced model"
    );

    // --- pick race, class, gender ------------------------------------------------------------
    let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateRace(race_slot));
    assert!(r.refused.is_none());
    let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateClass(class_slot));
    assert!(r.refused.is_none(), "class slot {class_slot} refused");

    // B2: the button's label and the id now on the draft come from one record.
    let adapter = caer_protocol::creation_adapters::class_adapter_at(
        realm,
        state.create_draft.race,
        class_slot,
    )
    .expect("class slot must resolve");
    assert_eq!(
        adapter.label, expect_class,
        "realm {realm} slot {class_slot}"
    );
    assert_eq!(
        state.create_draft.class_id, adapter.class_id,
        "the draft must carry the id the button named"
    );

    let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateGender(0));
    assert!(r.refused.is_none());

    // B4: model tracks identity through every one of those changes.
    assert!(
        state.create_draft.model_matches_identity(),
        "realm {realm}: model went stale after race/class/gender edits"
    );

    // --- name and Continue -------------------------------------------------------------------
    let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateContinue);
    assert!(
        r.refused.is_some(),
        "Continue must refuse before a name is typed"
    );

    apply_create_name_input(&mut state, "Walker");
    let r = dispatch_preworld_action(&mut state, PreWorldAction::CharCreateContinue);
    assert!(
        r.refused.is_none(),
        "realm {realm} Continue: {:?}",
        r.refused
    );
    assert!(
        r.commands.is_empty(),
        "realm {realm}: Continué opens customization, it mints nothing"
    );
    assert!(r.apply_hud_navigation);
    flow.on_event(FlowEvent::CustomizeOpened);
    assert_eq!(flow.screen(), Some(PreWorldScreen::CharCustomize));

    // --- customize → stats modal: still nothing on the wire ------------------------------------
    let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeStats);
    assert!(
        r.commands.is_empty(),
        "realm {realm}: Adjust Attributes sends nothing"
    );
    flow.on_event(FlowEvent::StatsOpened);
    assert_eq!(flow.screen(), Some(PreWorldScreen::CharStats));

    // The retail stats control is Optimize, a local operation — never a misleading second
    // Continue button.
    let r = dispatch_preworld_action(&mut state, PreWorldAction::StatsOptimize);
    assert!(
        r.refused.is_none(),
        "realm {realm} Optimize: {:?}",
        r.refused
    );
    assert!(
        r.commands.is_empty(),
        "realm {realm}: Optimize must stay local"
    );
    flow.on_event(FlowEvent::StatsDismissed);
    assert_eq!(flow.screen(), Some(PreWorldScreen::CharCustomize));

    // --- customizer Continue: the one packet of the whole walk ---------------------------------
    let r = dispatch_preworld_action(&mut state, PreWorldAction::CustomizeAdvance);
    assert!(
        r.refused.is_none(),
        "realm {realm} customizer Continue: {:?}",
        r.refused
    );
    let draft = match r.commands.as_slice() {
        [LiveCommand::CreateCharacter { draft }] => draft.clone(),
        other => panic!("realm {realm}: expected one CreateCharacter, got {other:?}"),
    };
    assert_eq!(draft.realm, realm);
    assert_eq!(draft.class_id, adapter.class_id, "wrong class on the wire");
    assert_eq!(draft.name, "Walker");
    assert_ne!(draft.creation_model, 0, "no model resolved for the wire");

    // --- server confirms; back at character select --------------------------------------------
    flow.on_event(FlowEvent::CreateAccepted);
    assert_eq!(flow.screen(), Some(PreWorldScreen::CharSelect));

    state.overview = Some(overview(vec![character(draft.slot, "Walker")]));
    state.ui_select_sent = false;

    // --- select and Play ----------------------------------------------------------------------
    state.selected_protocol_slot = Some(draft.slot);
    let r = dispatch_preworld_action(&mut state, PreWorldAction::EnterWorld);
    assert!(r.refused.is_none(), "realm {realm} Play: {:?}", r.refused);
    match r.commands.as_slice() {
        [LiveCommand::SelectCharacter { slot }] => assert_eq!(
            *slot, draft.slot,
            "Play must enter as the character just created"
        ),
        other => panic!("realm {realm}: expected SelectCharacter, got {other:?}"),
    }
}

#[test]
fn albion_creation_walk() {
    // Albion slot 1 is Armsman; Briton (race slot 0) is eligible for every Albion class.
    walk_realm(1, 0, 1, "Armsman");
}

#[test]
fn midgard_creation_walk() {
    // Midgard slot 1 is Warrior; Norseman is race slot 0.
    walk_realm(2, 0, 1, "Warrior");
}

#[test]
fn hibernia_creation_walk() {
    // Hibernia slot 5 is Hero; Celt is race slot 0.
    walk_realm(3, 0, 5, "Hero");
}

/// H4 at the seam: the overview refreshing between selection and Play must never enter the world
/// as a different character.
#[test]
fn refresh_between_select_and_play_cannot_swap_the_character() {
    let mut state = PreWorldProductState {
        overview: Some(overview(vec![
            character(0, "Aldric"),
            character(3, "Bryn"),
            character(7, "Cass"),
        ])),
        selected_protocol_slot: Some(3), // Bryn
        ..Default::default()
    };

    // Aldric is deleted elsewhere; the compact list shifts under the selection.
    state.overview = Some(overview(vec![character(3, "Bryn"), character(7, "Cass")]));

    let r = dispatch_preworld_action(&mut state, PreWorldAction::EnterWorld);
    assert!(r.refused.is_none(), "{:?}", r.refused);
    match r.commands.as_slice() {
        [LiveCommand::SelectCharacter { slot }] => {
            assert_eq!(*slot, 3, "entered the world as the wrong character")
        }
        other => panic!("expected SelectCharacter slot 3, got {other:?}"),
    }
}

/// Every class on every realm's form produces a legal, encodable draft for at least one race —
/// a visible button that can never be completed is a dead control.
#[test]
fn every_offered_class_is_completable_by_some_race() {
    for realm in 1..=3u8 {
        for slot in 0..caer_protocol::creation_adapters::CLASS_ADAPTER_SLOTS as u8 {
            let Some(adapter) = caer_protocol::creation_adapters::class_adapter_at(realm, 0, slot)
            else {
                continue; // Midgard legitimately leaves slot 15 empty.
            };
            let row = caer_protocol::career::class_career_by_id(adapter.class_id)
                .expect("offered class must be a known career");
            assert!(
                !row.eligible_races.is_empty(),
                "realm {realm} offers {} with no eligible race — it can never be created",
                adapter.label
            );
            // And at least one of those races resolves a creation model.
            assert!(
                row.eligible_races
                    .iter()
                    .any(|&race| caer_protocol::career::race_model(race, 0).is_some()),
                "realm {realm} {}: no eligible race resolves a model",
                adapter.label
            );
        }
    }
}
