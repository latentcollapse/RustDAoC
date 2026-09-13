//! Wave-B LANE UIX — KILL-03 14-class adapters + WindowManager parity.
//!
//! Named falsifiers:
//! - `kill03_fourteen_classes_named` (also in skinhud unit tests)
//! - `kill03_class_adapter_requires_packet` — delete the adapter arm → this goes red
//! - `parity_window_input_lifecycle` — delete hit-test / drag / Escape → this goes red
//!
//! egui is not involved (`CAER_DEV_EGUI` is not the product path).

use caer_protocol::charsheet::CharacterSheet;
use caer_protocol::codec::PacketWriter;
use caer_protocol::equipment::{decode as decode_eq, slot};
use caer_protocol::merchant::{MerchantOffer, MerchantWindow};
use caer_protocol::overview::{CharacterOverview, CharacterSummary};
use caer_protocol::social::{GroupWindow, GroupWindowMember};
use caer_protocol::status::PlayerStatus;
use caer_render::adapters::{self, AdapterState, ExtraBind, SocialBind};
use caer_render::product_ui;
use caer_render::skinhud::{self, CriticalWindowClass};
use caer_render::skinui::WindowManager;

fn status() -> PlayerStatus {
    PlayerStatus {
        health_pct: 62,
        mana_pct: 40,
        endurance_pct: 88,
        concentration_pct: 50,
        sitting: false,
        health: 620,
        max_health: 1000,
        mana: 200,
        max_mana: 500,
        endurance: 88,
        max_endurance: 100,
        concentration: 5,
        max_concentration: 10,
    }
}

fn empty_state<'a>(st: &'a PlayerStatus) -> AdapterState<'a> {
    AdapterState {
        player_name: "",
        status: st,
        target: None,
        zone: None,
        fps: 0,
        sheet: None,
        char_stats: None,
        char_resists: None,
        equipment: None,
        money: None,
        inventory: None,
        merchant: None,
        weapon_armor: None,
        attack_mode: None,
        login_account: None,
        login_password_mask: None,
        create_name: None,
    }
}

fn stub_window(name: &str, movable: bool) -> caer_assets::uiskin::WindowTemplate {
    caer_assets::uiskin::WindowTemplate {
        name: name.into(),
        width: 200,
        height: 120,
        title_width: 200,
        title_height: 18,
        close_button: movable,
        move_button: movable,
        controls: vec![caer_assets::uiskin::Control::Label(
            caer_assets::uiskin::Label {
                control_id: Some("hit".into()),
                click_event: Some("hit".into()),
                data: Some("Hit".into()),
                pos: (20, 40),
                width: 80,
                height: 24,
                font: Some("arial11".into()),
                ..Default::default()
            },
        )],
        ..Default::default()
    }
}

/// Falsifier `kill03_class_adapter_requires_packet`.
#[test]
fn kill03_class_adapter_requires_packet() {
    let st = PlayerStatus::default();
    let empty = empty_state(&st);
    let extra_empty = ExtraBind::default();
    let social_empty = SocialBind::default();

    for class in CriticalWindowClass::ALL {
        let a = class.representative_adapter();
        let bound = adapters::resolve(&empty, a)
            .or_else(|| adapters::resolve_social(&social_empty, a))
            .or_else(|| adapters::resolve_extra(&extra_empty, a));
        assert_eq!(
            bound, None,
            "{:?} adapter {a} must stay unbound without packet state",
            class
        );
        assert!(
            adapters::adapter_provenance(a).is_some(),
            "{a} must name packet/table provenance"
        );
    }

    let sheet = CharacterSheet {
        name: "Lilillyn".into(),
        level: 50,
        race_name: "Briton".into(),
        profession: "Fighter".into(),
        class_name: "Armsman".into(),
        realm_specialty_points: 3,
        ..Default::default()
    };
    let mut w = PacketWriter::new();
    w.u16(1).u8(0).u8(0).u8(0).u8(0).u8(1);
    w.u8(slot::TORSO).u16(0x0456).u8(3);
    let eq = decode_eq(w.as_slice()).unwrap();
    let mw = MerchantWindow {
        window_type: 0,
        page: 0,
        items: vec![MerchantOffer {
            slot: 0,
            level: 1,
            value1: 0,
            spd_abs: 0,
            hand_byte: 0,
            object_type_byte: 0,
            usable: true,
            value2: 0,
            price: 0,
            model: 1,
            name: "widget".into(),
        }],
    };
    let live_st = status();
    let live = AdapterState {
        player_name: "Lilillyn",
        status: &live_st,
        target: Some(("giant skeleton", 45)),
        zone: Some("Camelot Hills"),
        fps: 60,
        sheet: Some(&sheet),
        char_stats: None,
        char_resists: None,
        equipment: Some(&eq),
        money: None,
        inventory: None,
        merchant: Some(&mw),
        weapon_armor: None,
        attack_mode: None,
        login_account: Some("rustdaoc"),
        login_password_mask: Some("****"),
        create_name: Some("Newblade"),
    };
    let gw = GroupWindow {
        members: vec![GroupWindowMember {
            name: "Feile".into(),
            salutation: String::new(),
            object_id: 12,
            level: 50,
        }],
    };
    let social = SocialBind {
        group: Some(&gw),
        ..Default::default()
    };
    let ov = CharacterOverview {
        flags: 0,
        characters: vec![CharacterSummary {
            slot: 0,
            level: 50,
            name: "Lilillyn".into(),
            location: "Camelot Hills".into(),
            class_name: "Armsman".into(),
            race_name: "Briton".into(),
            region: 1,
            class_id: 2,
            realm: 1,
            stats: [0; 8],
            race_gender: 0,
            ..Default::default()
        }],
    };
    let extra = ExtraBind {
        overview: Some(&ov),
        chat_entry: Some("/say hi"),
        skills: None,
        points: None,
        timer: None,
        merchant_quantity: None,
        create_class_label: Some("Armsman"),
        create_class_desc: Some("Heavy infantry."),
        create_race_desc: Some("Highlander."),
    };

    for class in CriticalWindowClass::ALL {
        let a = class.representative_adapter();
        let bound = product_ui::resolve_product_extra(&live, &social, &extra, None, a);
        assert!(
            bound.is_some(),
            "{:?} adapter {a} must resolve from packet/table state, got {bound:?}",
            class
        );
    }
}

/// Falsifier `parity_window_input_lifecycle`.
#[test]
fn parity_window_input_lifecycle() {
    let mut skin = caer_assets::uiskin::Skin::default();
    for class in CriticalWindowClass::ALL {
        let name = class.skin_names()[0];
        skin.windows
            .insert(name.into(), stub_window(name, !class.is_preworld()));
    }
    skinhud::inject_product_social_overlays(&mut skin);

    let mut wm = WindowManager::default();
    let mut positions = Vec::new();
    for (i, class) in CriticalWindowClass::ALL.iter().enumerate() {
        let name = class.skin_names()[0];
        let pos = (i as f32 * 220.0, 0.0);
        wm.open(name, pos);
        positions.push((name, pos, *class));
    }

    // Focus follows last open; z-order last-open is top.
    let last = positions.last().unwrap().0;
    assert_eq!(wm.keyboard_focus(), Some(last));

    // Hit-test the first window's body. Raising it must change z-order (press).
    let (first, first_pos, first_class) = positions[0];
    assert_eq!(
        wm.hit(&skin, first_pos.0 + 10.0, first_pos.1 + 10.0),
        Some(first),
        "hit-test must name {first}"
    );
    assert_eq!(
        wm.hit_control(&skin, first_pos.0 + 30.0, first_pos.1 + 50.0),
        Some((first, "hit")),
        "control hit-test must land on the labelled gadget"
    );
    assert!(wm.on_press(&skin, first_pos.0 + 10.0, first_pos.1 + 8.0));
    assert_eq!(wm.keyboard_focus(), Some(first));

    if first_class.is_preworld() {
        wm.on_motion(first_pos.0 + 80.0, first_pos.1 + 40.0);
        wm.on_release();
        let after = wm
            .visible()
            .find(|w| w.name == first)
            .map(|w| w.pos)
            .unwrap();
        assert_eq!(after, first_pos, "preworld templates must not drag");
    } else {
        wm.on_motion(first_pos.0 + 80.0, first_pos.1 + 40.0);
        wm.on_release();
        let after = wm
            .visible()
            .find(|w| w.name == first)
            .map(|w| w.pos)
            .unwrap();
        assert_ne!(after, first_pos, "press/motion/release must drag {first}");
    }

    // Overlapping z-order: later window covers earlier at the same pixel after raise.
    let second = positions[1].0;
    wm.close(second);
    wm.open(second, first_pos);
    wm.set_pos(second, first_pos);
    wm.raise(second);
    assert_eq!(
        wm.hit(&skin, first_pos.0 + 5.0, first_pos.1 + 5.0),
        Some(second),
        "topmost window must win hit-test"
    );

    // Escape closes focused (second). Close gadget on a movable window.
    assert_eq!(wm.keyboard_focus(), Some(second));
    let closed = wm.on_escape();
    assert_eq!(closed.as_deref(), Some(second));
    assert!(!wm.is_open(second));

    let movable = positions
        .iter()
        .find(|(_, _, c)| !c.is_preworld())
        .map(|(n, p, _)| (*n, *p))
        .expect("in-world class");
    wm.open(movable.0, movable.1);
    assert_eq!(
        wm.hit_close(&skin, movable.1 .0 + 195.0, movable.1 .1 + 5.0),
        Some(movable.0)
    );
    wm.close(movable.0);
    assert!(!wm.is_open(movable.0));
    assert_eq!(
        wm.hit(&skin, movable.1 .0 + 10.0, movable.1 .1 + 10.0),
        None,
        "closed window must not ghost-hit"
    );
}

#[test]
fn resolve_product_extra_does_not_invent_unbound() {
    let st = PlayerStatus::default();
    let state = empty_state(&st);
    assert!(product_ui::resolve_product_extra(
        &state,
        &SocialBind::default(),
        &ExtraBind::default(),
        None,
        "bounty_points"
    )
    .is_none());
    assert!(product_ui::resolve_product_extra(
        &state,
        &SocialBind::default(),
        &ExtraBind::default(),
        None,
        "stats_strength"
    )
    .is_none());
}
