//! The pre-world button art states, read from the client's own `pregame/styles.xml`.
//!
//! Every `ButtonTemplate` authors four crops — Normal, NormalHighlit, Pressed, Disabled — and four
//! label colours. A client that draws only `Normal` looks broken in a specific way: nothing
//! responds to the pointer, and a selected race is indistinguishable from an unselected one. CAER
//! hardcoded `(0, 106, 102, 21)`, which is exactly `button_pregame_medium`'s Normal crop — the
//! table was read once and one row was taken.
//!
//! **Fails** when the retail tree is absent (REQ-025). Set `CAER_CLIENT`.

use std::path::PathBuf;

use caer_assets::uiskin::{ButtonState, Element, Skin};

fn styles() -> (Skin, PathBuf) {
    let root = caer_assets::client_dep::required_caer_client_root("pregame_button_templates");
    assert!(
        root.join("pregame").is_dir(),
        "CAER_CLIENT has no pregame directory: {} — the pre-world button templates can only be read \
         from the client. REQ-025: a test that cannot run must not report pass.",
        root.display()
    );
    let path = root.join("pregame/styles.xml");
    let xml = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let el = Element::parse(&xml).expect("pregame/styles.xml must parse");
    let mut skin = Skin::default();
    absorb_all(&mut skin, &el);
    (skin, root)
}

/// `Skin::absorb` takes one template element; the file is a list of them.
fn absorb_all(skin: &mut Skin, el: &Element) {
    skin.absorb(el);
    for c in &el.children {
        absorb_all(skin, c);
    }
}

/// The templates the pre-world screens actually name are present and fully modelled.
#[test]
fn pregame_templates_are_modelled() {
    let (skin, _) = styles();
    for name in [
        "button_pregame_medium",
        "button_pregame_small",
        "button_large_radio",
        "quit",
        "realm",
        "customize",
        "options",
        "play",
        "delete_char",
    ] {
        let t = skin
            .button(name)
            .unwrap_or_else(|| panic!("styles.xml must define the {name} button template"));
        assert!(t.size.0 > 0 && t.size.1 > 0, "{name}: zero-sized crop");
        assert!(!t.texture.is_empty(), "{name}: no texture page named");
    }
}

/// The four states must be four *different* crops, or drawing state-aware art changes nothing.
#[test]
fn medium_button_states_are_distinct() {
    let (skin, _) = styles();
    let t = skin.button("button_pregame_medium").expect("template");
    let normal = t.crop(ButtonState::Normal);
    let highlit = t.crop(ButtonState::Highlit);
    let pressed = t.crop(ButtonState::Pressed);
    let disabled = t.crop(ButtonState::Disabled);
    assert_eq!(
        (normal.2, normal.3),
        (102, 21),
        "the crop CAER hardcoded was 102x21"
    );
    assert_eq!(
        normal,
        (0, 106, 102, 21),
        "Normal is the row CAER hardcoded"
    );
    for (a, b, what) in [
        (normal, highlit, "Normal vs Highlit"),
        (normal, pressed, "Normal vs Pressed"),
        (normal, disabled, "Normal vs Disabled"),
        (highlit, pressed, "Highlit vs Pressed"),
    ] {
        assert_ne!(a, b, "{what} must differ or the state is invisible");
    }
}

/// Label colour is part of the state, and the client's choices are not subtle: grey normal, red
/// under the pointer, gold pressed. Losing them would make hover half-work.
#[test]
fn medium_button_label_colours_are_per_state() {
    let (skin, _) = styles();
    let t = skin.button("button_pregame_medium").expect("template");
    let n = t.color(ButtonState::Normal);
    let h = t.color(ButtonState::Highlit);
    let p = t.color(ButtonState::Pressed);
    assert_eq!((n.r, n.g, n.b), (192, 192, 192), "normal label is grey");
    assert_eq!((h.r, h.g, h.b), (255, 0, 0), "highlit label is red");
    assert_eq!((p.r, p.g, p.b), (255, 192, 0), "pressed label is gold");
    assert_ne!(
        (n.r, n.g, n.b),
        (255, 255, 255),
        "a white normal colour means the ColorNormal block was not read — `Element::color` looks \
         for a nested <Color> child, which these do not have"
    );
}

/// The texture name is an indirection through `asset.xml`, not a filename.
#[test]
fn button_texture_name_resolves_to_an_archive_member() {
    let (skin, root) = styles();
    let t = skin.button("button_pregame_medium").expect("template");
    assert_eq!(t.texture, "misc_controls_new");
    let xml = std::fs::read_to_string(root.join("pregame/asset.xml")).expect("asset.xml");
    let el = Element::parse(&xml).expect("asset.xml must parse");
    let mut assets = Skin::default();
    absorb_all(&mut assets, &el);
    let tex = assets
        .texture(&t.texture)
        .unwrap_or_else(|| panic!("asset.xml must map {}", t.texture));
    assert!(
        tex.file.ends_with("misc_pieces_new.tga"),
        "expected the pregame atlas, got {}",
        tex.file
    );
}
