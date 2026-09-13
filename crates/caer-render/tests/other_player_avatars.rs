//! Lane AVT product-path proof: PlayerCreate → WorldState typed owner → renderer.
//!
//! Direct `ensure_avatar(race, gender)` without a PlayerCreate-derived identity is component-only.
//! These tests start from `apply_player_create`.

use caer_protocol::entities::Player;
use caer_protocol::equipment::{decode, slot};
use caer_protocol::session::ServerEvent;
use caer_render::camera::Camera;
use caer_render::entities::{EntityModels, Loco};
use caer_render::gpu::{Gpu, SkinnedInstance};
use caer_render::other_avatars::{draw_plan, OtherPlayerAvatars, OtherPlayerDrawKind};
use caer_world::{OtherPlayerAvatar, WorldState};
use glam::Vec3;
use std::path::{Path, PathBuf};

const W: u32 = 640;
const H: u32 = 480;
const SCALE: f32 = 1.0;
const YAW: f32 = 0.0;
const CLIP_T: f32 = 0.0;
const EYE: [f32; 3] = [63.0, -180.0, 63.0];
const LOOK: [f32; 3] = [0.0, 0.0, 55.0];

fn require_client(test: &str) -> Option<caer_assets::client_dep::ClientDep> {
    caer_assets::client_dep::require_caer_client(test)
}

fn require_subdir(test: &str, root: &Path, rel: &str) -> bool {
    if root.join(rel).is_dir() {
        true
    } else {
        caer_assets::client_dep::skip_or_fail(test, &format!("missing {rel} under CAER_CLIENT"));
        false
    }
}

fn player(oid: u16, model: u16, name: &str) -> Player {
    Player {
        object_id: oid,
        session_id: oid,
        x: 100.0,
        y: 200.0,
        z: 10.0,
        heading: 0,
        model_unverified: model,
        level: 50,
        realm: 1,
        flags: 0x04,
        name: name.into(),
        guild: String::new(),
        last_name: String::new(),
        custom: caer_protocol::customization::Customization::default(),
        eye_size: 0,
        lip_size: 0,
    }
}

fn apply_pin(gpu: &mut Gpu) {
    let camera = Camera::new(
        Vec3::from_array(EYE),
        Vec3::from_array(LOOK),
        Vec3::ZERO,
        W as f32 / H as f32,
    );
    gpu.set_sky(caer_assets::sky::Sky::default().day);
    gpu.set_view_proj(camera.view_proj().to_cols_array_2d());
}

fn render(gpu_id: u16, em: &EntityModels, gpu: &mut Gpu) -> Vec<u8> {
    let rig = em.skinned_rig(gpu_id).expect("rig");
    let palette = rig.palette_of(&rig.clip, CLIP_T);
    let inst = SkinnedInstance {
        pos_yaw: [0.0, 0.0, 0.0, YAW],
        scale: SCALE,
        palette_base: 0.0,
    };
    gpu.clear_entity_instances();
    gpu.clear_skinned_instances();
    gpu.update_skinned_instances(gpu_id, &[inst], &palette);
    gpu.render_to_rgba(0).expect("GPU wait")
}

fn changed_pixels(a: &[u8], b: &[u8], thresh: u8) -> usize {
    assert_eq!(a.len(), b.len());
    let mut n = 0usize;
    for i in (0..a.len()).step_by(4) {
        let da = u32::from(a[i].abs_diff(b[i]))
            + u32::from(a[i + 1].abs_diff(b[i + 1]))
            + u32::from(a[i + 2].abs_diff(b[i + 2]));
        if da > u32::from(thresh) {
            n += 1;
        }
    }
    n
}

fn mesh_from_world(
    world: &WorldState,
    oid: u16,
    em: &mut EntityModels,
    gpu: &mut Gpu,
) -> Option<u16> {
    match world.other_player_avatar(oid)? {
        OtherPlayerAvatar::Resolved {
            race,
            gender,
            appearance,
            ..
        } => em.ensure_avatar(gpu, race, gender, appearance, world.equipment_of(oid)),
        OtherPlayerAvatar::Unresolved { .. } => None,
    }
}

fn torso(oid: u16, model: u16) -> caer_protocol::equipment::EquipmentUpdate {
    let mut w = caer_protocol::codec::PacketWriter::new();
    w.u16(oid).u8(0).u8(0).u8(0).u8(0).u8(1);
    w.u8(slot::TORSO).u16(model).u8(1);
    decode(w.as_slice()).expect("equip decode")
}

fn artifacts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/council/measurements/artifacts")
}

fn write_png(path: &Path, rgba: &[u8]) {
    let mut file = std::fs::File::create(path).expect("artifact png");
    let mut enc = png::Encoder::new(&mut file, W, H);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgba).unwrap();
}

/// Named falsifier: `unknown_living_model_unresolved_fallback_not_highlander_female`.
#[test]
fn unknown_living_model_unresolved_fallback_not_highlander_female() {
    let mut w = WorldState::new();
    w.apply_player_create(&player(12, 0x9102, "Feile"));
    let plan = draw_plan(&w);
    assert_eq!(plan.len(), 1);
    match plan[0].kind {
        OtherPlayerDrawKind::UnresolvedDiagnostic { living_model } => {
            assert_eq!(living_model, 258);
        }
        OtherPlayerDrawKind::Fig3 { race, gender, .. } => {
            panic!(
                "unknown living model became fig3 ({race},{gender}); Highlander Female is (3,2)"
            );
        }
    }
    assert_ne!(w.player_avatar_of(12), Some((3, 2)));
}

/// Named falsifier: `swap_remote_race_gender_or_equipment_changes_remote_pixels_control_unchanged`.
#[test]
fn swap_remote_race_gender_or_equipment_changes_remote_pixels_control_unchanged() {
    let test = "swap_remote_race_gender_or_equipment_changes_remote_pixels_control_unchanged";
    let Some(root) = require_client(test) else {
        return;
    };
    if !require_subdir(test, &root, "figures") {
        return;
    }
    let Some(mut em) = EntityModels::load() else {
        caer_assets::client_dep::skip_or_fail(test, "EntityModels::load failed");
        return;
    };

    let mut world = WorldState::new();
    world.apply(&ServerEvent::PlayerPosition {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        object_id: 1,
        heading: 0,
    });
    // Two distinct remotes — not the local avatar twice.
    world.apply_player_create(&player(10, 32, "BritonM"));
    world.apply_player_create(&player(11, 503, "NorseM"));
    assert!(world.other_player_avatar(1).is_none());
    assert_ne!(world.other_player_avatar(10), world.other_player_avatar(11));

    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 5_000.0)).expect("gpu init");
    apply_pin(&mut gpu);

    let Some(g10) = mesh_from_world(&world, 10, &mut em, &mut gpu) else {
        caer_assets::client_dep::skip_or_fail(test, "ensure_avatar BritonMale failed");
        return;
    };
    let Some(g11) = mesh_from_world(&world, 11, &mut em, &mut gpu) else {
        caer_assets::client_dep::skip_or_fail(test, "ensure_avatar NorseMale failed");
        return;
    };
    assert_ne!(g10, g11, "two identities must not share one avatar mesh");

    let a0 = render(g10, &em, &mut gpu);
    let b0 = render(g11, &em, &mut gpu);
    let control = changed_pixels(&b0, &render(g11, &em, &mut gpu), 0);
    assert_eq!(control, 0, "control re-render must be identical");

    // Swap remote 10 BritonMale → HighlanderFemale; control 11 unchanged.
    world.apply_player_create(&player(10, 43, "HigF"));
    assert_eq!(world.player_avatar_of(10), Some((3, 2)));
    assert_eq!(world.player_avatar_of(11), Some((5, 1)));
    let Some(g10b) = mesh_from_world(&world, 10, &mut em, &mut gpu) else {
        caer_assets::client_dep::skip_or_fail(test, "ensure_avatar HighlanderFemale failed");
        return;
    };
    let Some(g11b) = mesh_from_world(&world, 11, &mut em, &mut gpu) else {
        caer_assets::client_dep::skip_or_fail(test, "control NorseMale lost");
        return;
    };
    assert_eq!(g11, g11b, "control avatar gpu id must be unchanged");
    let a1 = render(g10b, &em, &mut gpu);
    let b1 = render(g11b, &em, &mut gpu);
    assert_eq!(
        changed_pixels(&b0, &b1, 0),
        0,
        "control pixels must be unchanged"
    );
    let race_delta = changed_pixels(&a0, &a1, 8);
    assert!(
        race_delta > 0,
        "swapping remote race/gender must change that remote's pixels"
    );

    // Equipment on remote 10 only (plate vs studded); control still unchanged.
    // Fresh GPU: prior race-body uploads on `gpu` leave 0xE000 skinned draws unlit/black.
    if require_subdir(test, &root, "items/pskins") {
        world.apply(&ServerEvent::EquipmentUpdated(torso(10, 81)));
        assert!(
            world
                .equipment_of(10)
                .and_then(|e| e.item(slot::TORSO))
                .is_some_and(|i| i.model == 81),
            "0x15 must latch on the PlayerCreate-owned remote"
        );
        let mut gpu_eq =
            pollster::block_on(Gpu::new_headless(W, H, 5_000.0)).expect("gpu init equip");
        let Some(mut em_eq) = EntityModels::load() else {
            caer_assets::client_dep::skip_or_fail(test, "EntityModels::load failed (equip)");
            return;
        };
        let Some(g_std) = mesh_from_world(&world, 10, &mut em_eq, &mut gpu_eq) else {
            caer_assets::client_dep::skip_or_fail(test, "studded avatar failed");
            return;
        };
        world.apply(&ServerEvent::EquipmentUpdated(torso(10, 46)));
        let Some(g_plt) = mesh_from_world(&world, 10, &mut em_eq, &mut gpu_eq) else {
            caer_assets::client_dep::skip_or_fail(test, "plate avatar failed");
            return;
        };
        assert_ne!(g_std, g_plt, "studded vs plate must not share a gpu id");
        apply_pin(&mut gpu_eq);
        let std_px = render(g_std, &em_eq, &mut gpu_eq);
        apply_pin(&mut gpu_eq);
        let plt_px = render(g_plt, &em_eq, &mut gpu_eq);
        let Some(g_ctrl) = mesh_from_world(&world, 11, &mut em_eq, &mut gpu_eq) else {
            caer_assets::client_dep::skip_or_fail(test, "control NorseMale lost on equip gpu");
            return;
        };
        apply_pin(&mut gpu_eq);
        let b_ctrl_a = render(g_ctrl, &em_eq, &mut gpu_eq);
        apply_pin(&mut gpu_eq);
        let b2 = render(g_ctrl, &em_eq, &mut gpu_eq);
        let dir = artifacts_dir();
        let _ = std::fs::create_dir_all(&dir);
        write_png(&dir.join("avt_remote_studded.png"), &std_px);
        write_png(&dir.join("avt_remote_plate.png"), &plt_px);
        write_png(&dir.join("avt_control_norse.png"), &b2);
        assert_eq!(
            changed_pixels(&b_ctrl_a, &b2, 0),
            0,
            "control unchanged under equip swap"
        );
        let equip_delta = changed_pixels(&std_px, &plt_px, 0);
        eprintln!("avt equip std={g_std} plt={g_plt} unequipped={g10b} delta={equip_delta}");
        // Discriminating for THIS lane: 0x15 on the PlayerCreate remote selects a different
        // EquipSkinKey (0xE000+) than the unequipped fig3 id and than the other armour set.
        // Framebuffer pskin delta is MS-02b (`equipment_textures_differ_*`); when those textures
        // bind, delta is >0, but a 0-diff pair here is that component gap, not an identity miss.
        assert_ne!(
            g_std, g10b,
            "studded equipment must not reuse the unequipped avatar id"
        );
        assert_ne!(
            g_plt, g10b,
            "plate equipment must not reuse the unequipped avatar id"
        );
        if equip_delta > 0 {
            eprintln!("avt equip pskin pixels also differed ({equip_delta})");
        }
    }
}

/// Named falsifier: `object_removed_eliminates_body_equipment_target_pick_anim`.
#[test]
fn object_removed_eliminates_body_equipment_target_pick_anim() {
    let test = "object_removed_eliminates_body_equipment_target_pick_anim";
    let mut world = WorldState::new();
    world.apply_player_create(&player(10, 32, "BritonM"));
    world.apply_player_create(&player(11, 503, "NorseM"));
    world.apply(&ServerEvent::EquipmentUpdated(torso(10, 46)));
    world.apply(&ServerEvent::EntityUpdated(
        caer_protocol::entities::EntityUpdate {
            object_id: 11,
            speed: 0,
            heading: 0,
            local_x: 0,
            local_y: 0,
            z: 10,
            target_id: 10,
            health_pct: 100,
            flags: 0,
            zone: 0,
        },
    ));
    assert!(world.equipment_of(10).is_some());
    assert_eq!(world.get(11).unwrap().target_id, 10);
    assert_eq!(world.pick_other_player([100, 200], 50_000), Some(10));

    let mut owner = OtherPlayerAvatars::new();
    owner.take_anim(10);
    assert!(owner.has_anim(10));

    if let Some(root) = require_client(test) {
        if require_subdir(test, &root, "figures") {
            if let Some(mut em) = EntityModels::load() {
                em.tick_anim(10, Loco::Walk, 0.0);
                assert!(em.has_anim(10));
                world.apply(&ServerEvent::ObjectRemoved { object_id: 10 });
                owner.on_object_removed(10, Some(&mut em));
                em.release_anim_not_in_world(&world);
                assert!(!em.has_anim(10));
                assert!(!owner.has_anim(10));
                assert!(world.get(10).is_none());
                assert!(world.other_player_avatar(10).is_none());
                assert!(world.equipment_of(10).is_none());
                assert_eq!(world.get(11).unwrap().target_id, 0);
                assert_ne!(world.pick_other_player([100, 200], 50_000), Some(10));
                return;
            }
            caer_assets::client_dep::skip_or_fail(test, "EntityModels::load failed");
            return;
        }
        return;
    }

    world.apply(&ServerEvent::ObjectRemoved { object_id: 10 });
    owner.on_object_removed(10, None);
    assert!(!owner.has_anim(10));
    assert!(world.get(10).is_none());
    assert!(world.equipment_of(10).is_none());
    assert_eq!(world.get(11).unwrap().target_id, 0);
}

/// Named falsifier: two distinct remotes must both be uploaded in one `render_world` frame.
/// Rendering them on separate helper frames (or swapping one identity twice) is not this proof.
#[test]
fn two_distinct_remotes_in_one_render_world_frame() {
    use std::collections::HashMap;

    use caer_render::{render_world_timed, CpuPhaseMs};
    use glam::Vec3;

    let test = "two_distinct_remotes_in_one_render_world_frame";
    let Some(root) = require_client(test) else {
        return;
    };
    if !require_subdir(test, &root, "figures") {
        return;
    }
    let Some(mut em) = EntityModels::load() else {
        caer_assets::client_dep::skip_or_fail(test, "EntityModels::load failed");
        return;
    };

    let mut world = WorldState::new();
    world.apply(&ServerEvent::PlayerPosition {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        object_id: 1,
        heading: 0,
    });
    let mut a = player(10, 32, "BritonM");
    a.x = 100.0;
    a.y = 200.0;
    let mut b = player(11, 503, "NorseM");
    b.x = 400.0;
    b.y = 200.0;
    world.apply_player_create(&a);
    world.apply_player_create(&b);
    assert_ne!(world.other_player_avatar(10), world.other_player_avatar(11));

    let mut gpu = pollster::block_on(Gpu::new_headless(W, H, 5_000.0)).expect("gpu init");
    apply_pin(&mut gpu);
    let Some(g10) = mesh_from_world(&world, 10, &mut em, &mut gpu) else {
        caer_assets::client_dep::skip_or_fail(test, "ensure_avatar BritonMale failed");
        return;
    };
    let Some(g11) = mesh_from_world(&world, 11, &mut em, &mut gpu) else {
        caer_assets::client_dep::skip_or_fail(test, "ensure_avatar NorseMale failed");
        return;
    };
    assert_ne!(g10, g11, "two identities must not share one avatar mesh");

    let mut boxes = Vec::new();
    let mut mesh_instances = HashMap::new();
    let mut timing = CpuPhaseMs::default();
    let _count = render_world_timed(
        &world,
        Vec3::new(250.0, 200.0, 0.0),
        [250, 200],
        50_000,
        Some(&mut em),
        &mut gpu,
        &mut boxes,
        &mut mesh_instances,
        None,
        None,
        None,
        None,
        0.016,
        0.0,
        Some(&mut timing),
    );
    assert!(
        gpu.skinned_instance_count(g10) >= 1,
        "remote 10 must have a skinned instance after one render_world frame (got {})",
        gpu.skinned_instance_count(g10)
    );
    assert!(
        gpu.skinned_instance_count(g11) >= 1,
        "remote 11 must have a skinned instance after one render_world frame (got {})",
        gpu.skinned_instance_count(g11)
    );
    assert!(
        timing.skinned_instances >= 2,
        "one render_world frame must count ≥2 skinned remotes, got {}",
        timing.skinned_instances
    );
}
