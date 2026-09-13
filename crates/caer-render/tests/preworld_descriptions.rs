//! H7 — the creation form's race/class description panes actually render text.
//!
//! **Fails** when the retail tree is absent (REQ-025): a check that did not run must not report
//! `pass`. Set `CAER_CLIENT` to point at the client tree.

use std::path::PathBuf;

use caer_render::preworld::{PreWorldHud, PreWorldScreen};

fn require_root() -> PathBuf {
    let root = caer_assets::client_dep::required_caer_client_root("preworld_descriptions");
    assert!(
        root.join("pregame").is_dir(),
        "CAER_CLIENT has no pregame directory: {} — the description panes need real client assets. \
         REQ-025: a test that cannot run must not report pass.",
        root.display()
    );
    root
}

/// The loader finds every race and class description in the shipped client.
#[test]
fn every_race_and_class_resolves_a_description() {
    let root = require_root();
    let table = caer_assets::descriptions::load(&root);
    assert!(
        table.is_verified(),
        "game.dll did not match the recorded build — offsets are not trustworthy for this client"
    );
    // A shard may patch a description out of its own binary. That is a DECLARED property of the
    // build (`absent_race` in the offset table), not licence for an arbitrary hole: an id that is
    // missing without being declared is still a failure, which is what this used to catch.
    for row in caer_protocol::career::RACES {
        if table.is_absent_race(row.id) {
            assert_eq!(
                table.race(row.id),
                None,
                "race {} is declared absent for this build but resolved anyway",
                row.name
            );
            continue;
        }
        let d = table
            .race(row.id)
            .unwrap_or_else(|| panic!("race {} ({}) has no description", row.name, row.id));
        assert!(
            d.len() > 40,
            "race {} description is implausibly short",
            row.name
        );
    }
    for realm in 1..=3u8 {
        for a in caer_protocol::creation_adapters::class_adapters(realm, 0) {
            let d = table
                .class(a.class_id)
                .unwrap_or_else(|| panic!("class {} ({}) has no description", a.label, a.class_id));
            assert!(
                d.len() > 40,
                "class {} description is implausibly short",
                a.label
            );
        }
    }
}

/// Descriptions must be distinct per race/class — a loader bug that returned one string for
/// everything would satisfy the presence test above but be obviously wrong on screen.
#[test]
fn descriptions_are_not_all_the_same_string() {
    let root = require_root();
    let table = caer_assets::descriptions::load(&root);
    let mut race_texts: Vec<&str> = caer_protocol::career::RACES
        .iter()
        .filter_map(|r| table.race(r.id))
        .collect();
    let before = race_texts.len();
    race_texts.sort_unstable();
    race_texts.dedup();
    // The three Minotaur rows share one string where the build carries them, so the expected
    // number of duplicates is however many of that trio actually resolved, less one. On a client
    // that patched them out none resolve and every remaining race must be distinct.
    let minotaurs = [19u8, 20, 21]
        .iter()
        .filter(|&&id| table.race(id).is_some())
        .count();
    let shared = minotaurs.saturating_sub(1);
    assert_eq!(
        race_texts.len(),
        before - shared,
        "race descriptions: {minotaurs} Minotaur rows should share one string and the rest differ"
    );
    assert!(
        before >= 18,
        "only {before} race descriptions resolved — the panes are mostly blank"
    );
}

/// The panes must put glyphs on screen for the default draft — the defect this guards is the
/// whole feature silently rendering nothing because a font, offset or lookup came back empty.
#[test]
fn creation_form_emits_description_glyphs() {
    let root = require_root();
    let mut hud = PreWorldHud::new(root);
    hud.set_screen(PreWorldScreen::CharCreate);
    hud.ensure_loaded().expect("create form assets");

    let vp = (1024.0, 768.0);
    let quads = hud.layout(vp);
    assert!(!quads.is_empty(), "create form produced no quads at all");

    // Description prose lives in the left column (authored x 20..240). Chrome and the form live
    // to the right, so glyphs landing in that band are the panes and nothing else.
    let left_column = quads
        .iter()
        .filter(|q| q.dst.x >= 15.0 && q.dst.x < 245.0 && q.dst.y > 80.0)
        .count();
    assert!(
        left_column > 200,
        "expected the description panes to emit many glyphs in the left column, got {left_column} \
         — the panes are rendering blank"
    );
}

/// **Random button falsifier.** It must produce a name the client's own fragments could make, for
/// every race, and must be inert rather than inventive when the table is missing.
#[test]
fn random_button_composes_names_from_client_fragments() {
    let root = require_root();
    let table = caer_assets::names::load(&root);
    assert!(!table.is_empty(), "charman/names.dat did not load");

    for row in caer_protocol::career::RACES {
        let f = table
            .for_race(row.id)
            .unwrap_or_else(|| panic!("{} ({}) has no fragment pools", row.name, row.id));
        assert!(f.is_complete(), "{} has an incomplete pool set", row.name);

        let name = table
            .generate(row.id, |n| n / 2)
            .unwrap_or_else(|| panic!("{} generated no name", row.name));
        assert!(
            !name.is_empty() && name.len() <= caer_assets::names::MAX_NAME_LEN,
            "{}: {name:?} violates the edit box limit",
            row.name
        );
        assert!(
            name.chars().all(|c| c.is_ascii_alphanumeric()),
            "{}: {name:?} is not accepted by the create form",
            row.name
        );
        assert!(
            name.chars().next().is_some_and(char::is_uppercase),
            "{}: {name:?} is not capitalised like retail",
            row.name
        );
        // The name must be composable from this race's own pools, not another's.
        assert!(
            f.first
                .iter()
                .any(|p| name.to_lowercase().starts_with(&p.to_lowercase())),
            "{}: {name:?} does not start with one of its own first fragments",
            row.name
        );
    }
}

/// Different draws give different names — a generator that always returned one name would pass
/// the checks above.
#[test]
fn random_names_vary_across_draws() {
    let root = require_root();
    let table = caer_assets::names::load(&root);
    let mut seen = std::collections::HashSet::new();
    for i in 0..24usize {
        if let Some(n) = table.generate(1, |len| (i * 7 + 3) % len) {
            seen.insert(n);
        }
    }
    assert!(
        seen.len() > 3,
        "Random produced only {} distinct names in 24 draws",
        seen.len()
    );
}
