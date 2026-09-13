//! Nameplates — world-space labels floating over entities (C.7).
//!
//! The second consumer of the 2D overlay layer ([`crate::ui`]), and the first that has to bridge
//! 3D and 2D: each label is positioned by projecting an entity's world position through the same
//! view-projection the scene is drawn with, then drawn as flat text at the resulting pixel.
//!
//! This is what makes the world legible — before it, identifying anything meant tabbing through
//! targets and reading the HUD. It matters more now that B.6 put other players in the world.
//!
//! ## The projection, and the trap in it
//! Points *behind* the camera still produce finite screen coordinates once divided by `w` — they
//! just come out mirrored. Projecting naively puts labels for everything behind you on screen,
//! upside down and backwards. [`project`] rejects `clip.w <= 0` for exactly that reason, and
//! `behind_the_camera_is_rejected` pins it.
//!
//! Conventions are taken from [`crate::camera::Camera::screen_ray`], which is the inverse of this
//! transform: wgpu clip space is x,y ∈ [-1,1] with **y up**, so the pixel conversion flips y.
//! `projection_round_trips_through_screen_ray` asserts the two agree rather than trusting that I
//! transcribed them consistently.

use glam::{Mat4, Vec3, Vec4};

use caer_world::{Kind, WorldState};

/// How far above an entity's ground position its label floats, in world units.
///
/// A single constant rather than a per-entity mesh height: entity meshes vary and none of them
/// publish a bounding box through the render path yet. Sized a little above the placeholder box
/// (`BOX_SIZE` = 110) so the label clears both a box and a typical humanoid.
const LABEL_HEIGHT: f32 = 150.0;

/// Beyond this many world units an entity gets no nameplate. DAoC's own plates fade out at a
/// similar range; without a limit a busy zone paints hundreds of unreadable labels.
pub const MAX_PLATE_DIST: f32 = 6_000.0;

/// Most nameplates drawn in one frame, nearest first. A hard cap keeps a dense spawn camp from
/// turning the screen into a wall of text (and bounds the per-frame text layout cost).
pub const MAX_PLATES: usize = 40;

/// One entity's label, already projected to pixels and ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Nameplate {
    /// Pixel position, origin top-left — where the label's centre-bottom anchor sits.
    pub screen: [f32; 2],
    /// Squared world distance to the camera, for sorting and fading.
    pub dist2: f32,
    pub name: String,
    /// Guild name for players, role subtitle for NPCs ("Armor Merchant"). Empty when absent.
    pub subtitle: String,
    pub kind: Kind,
    /// Health percent as the server last reported it (100 when unknown).
    pub health_pct: u8,
    /// Whether this is the entity the player currently has targeted.
    pub is_target: bool,
}

/// Project a **render-space** point to pixels. `None` when the point is behind the camera or
/// outside the viewport.
///
/// Returning `None` for off-screen points (rather than clamping to an edge) is deliberate: DAoC
/// does not pin nameplates to the screen border, and doing so would crowd the edges with labels
/// for things the player cannot see.
#[must_use]
pub fn project(view_proj: &Mat4, point: Vec3, viewport: [f32; 2]) -> Option<[f32; 2]> {
    let clip = *view_proj * Vec4::new(point.x, point.y, point.z, 1.0);
    // Behind the camera (or exactly on the plane): the perspective divide would still yield finite
    // coordinates, mirrored into view. This is THE nameplate bug; reject before dividing.
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    if !(-1.0..=1.0).contains(&ndc.x) || !(-1.0..=1.0).contains(&ndc.y) {
        return None;
    }
    Some([
        (ndc.x * 0.5 + 0.5) * viewport[0],
        // wgpu NDC has y up; pixels have y down.
        (1.0 - (ndc.y * 0.5 + 0.5)) * viewport[1],
    ])
}

/// Gather the nameplates to draw this frame, nearest first and capped at [`MAX_PLATES`].
///
/// `origin` is the render-space origin the scene is drawn around. `viewer_world` is what distance
/// is measured from, in world units — the client passes the **player's** position, not the
/// camera's: nameplate range in DAoC is about how far things are from your character, and the
/// follow camera sitting a few hundred units behind should not change which labels appear.
#[must_use]
pub fn collect(
    world: &WorldState,
    origin: Vec3,
    viewer_world: Vec3,
    view_proj: &Mat4,
    viewport: [f32; 2],
    target_id: Option<u16>,
) -> Vec<Nameplate> {
    let max_d2 = MAX_PLATE_DIST * MAX_PLATE_DIST;
    let mut plates: Vec<Nameplate> = Vec::new();

    for v in world.iter() {
        // Our own character gets no plate — the HUD already names us, and a label pinned to the
        // middle of the screen would sit over the camera's focus permanently.
        if v.kind == Kind::Self_ {
            continue;
        }
        // Dead entities are culled from the living draw; keep nameplates in the same presence set.
        if !crate::death::in_living_draw(v.is_dead) {
            continue;
        }
        // Unnamed things (static scenery, not-yet-created placeholders) have nothing to say.
        if v.name.is_empty() || v.name == "<self>" {
            continue;
        }

        let wp = Vec3::new(v.pos[0] as f32, v.pos[1] as f32, v.pos[2] as f32);
        let dist2 = (wp - viewer_world).length_squared();
        if dist2 > max_d2 {
            continue;
        }

        // World → render space: y is mirrored (world +Y is south, render +Y is north), matching
        // the scene path in `render_world`.
        let rp = Vec3::new(
            wp.x - origin.x,
            -(wp.y - origin.y),
            wp.z - origin.z + LABEL_HEIGHT,
        );
        let Some(screen) = project(view_proj, rp, viewport) else {
            continue;
        };

        plates.push(Nameplate {
            screen,
            dist2,
            name: v.name.to_string(),
            subtitle: v.guild.to_string(),
            kind: v.kind,
            health_pct: v.health_pct,
            is_target: target_id == Some(v.object_id),
        });
    }

    // Nearest first, so the cap keeps what matters and nearer labels draw over farther ones.
    plates.sort_by(|a, b| a.dist2.total_cmp(&b.dist2));
    plates.truncate(MAX_PLATES);
    plates
}

/// Label colour by entity kind. Players read green, NPCs warm amber, anything else neutral —
/// distinct at a glance without being a full con-colour system (that needs level + realm rules,
/// which belong with the combat work).
fn colour(kind: Kind, is_target: bool) -> egui::Color32 {
    if is_target {
        return egui::Color32::from_rgb(255, 236, 150); // the selected entity always stands out
    }
    match kind {
        Kind::Player => egui::Color32::from_rgb(150, 230, 160),
        Kind::Npc => egui::Color32::from_rgb(238, 206, 150),
        _ => egui::Color32::from_rgb(206, 206, 206),
    }
}

/// Draw the collected nameplates. Pass alongside the HUD inside one [`crate::ui::Ui::run`].
///
/// Each plate is painted directly rather than laid out as a widget: they are positioned by the 3D
/// projection, not by egui's layout, and painting avoids allocating a `Area` per entity per frame.
pub fn draw(ui: &egui::Ui, plates: &[Nameplate]) {
    let painter = ui.painter();
    for p in plates {
        // Fade with distance so a crowded background recedes instead of competing with the
        // foreground. Full opacity for the nearer half of the range, easing off beyond it.
        let d = p.dist2.sqrt() / MAX_PLATE_DIST;
        let alpha = (1.0 - (d - 0.5).max(0.0) / 0.5).clamp(0.25, 1.0);
        let a = |c: egui::Color32| {
            egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (alpha * 255.0) as u8)
        };

        let pos = egui::pos2(p.screen[0], p.screen[1]);
        let font = egui::FontId::proportional(if p.is_target { 14.0 } else { 12.0 });

        // Shadow then text: nameplates sit over arbitrary terrain, and unshadowed light text on
        // bright ground is unreadable.
        painter.text(
            pos + egui::vec2(1.0, 1.0),
            egui::Align2::CENTER_BOTTOM,
            &p.name,
            font.clone(),
            a(egui::Color32::from_rgb(8, 8, 10)),
        );
        painter.text(
            pos,
            egui::Align2::CENTER_BOTTOM,
            &p.name,
            font.clone(),
            a(colour(p.kind, p.is_target)),
        );

        if !p.subtitle.is_empty() {
            let sub = egui::FontId::proportional(10.0);
            let below = pos + egui::vec2(0.0, 12.0);
            painter.text(
                below + egui::vec2(1.0, 1.0),
                egui::Align2::CENTER_BOTTOM,
                &p.subtitle,
                sub.clone(),
                a(egui::Color32::from_rgb(8, 8, 10)),
            );
            painter.text(
                below,
                egui::Align2::CENTER_BOTTOM,
                &p.subtitle,
                sub,
                a(egui::Color32::from_rgb(170, 170, 176)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;

    fn camera() -> Camera {
        // Looking down -Y in render space from above, a vantage like the client's follow camera.
        let mut c = Camera::new(
            Vec3::new(0.0, -1000.0, 200.0),
            Vec3::ZERO,
            Vec3::ZERO,
            16.0 / 9.0,
        );
        c.ensure_far(50_000.0);
        c
    }

    /// A point in front of the camera projects into the viewport; the same point behind it must be
    /// rejected, NOT mirrored onto the screen. This is the classic nameplate bug.
    #[test]
    fn behind_the_camera_is_rejected() {
        let c = camera();
        let vp = c.view_proj();
        let viewport = [1600.0, 900.0];

        let front = project(&vp, Vec3::new(0.0, 0.0, 200.0), viewport);
        assert!(
            front.is_some(),
            "a point the camera looks at should project"
        );

        // The camera sits at render y = +1000 looking toward the origin, so "behind" is y beyond
        // the eye — not negative y, which is further along the view direction.
        let behind = project(&vp, Vec3::new(0.0, 3000.0, 200.0), viewport);
        assert!(
            behind.is_none(),
            "a point behind the camera must not produce a nameplate"
        );

        // The failure mode this guards is specifically MIRRORING: without the `w <= 0` rejection a
        // behind-camera point still divides to a finite, plausible-looking pixel. Prove that is
        // what would happen, so the check above cannot be quietly deleted as redundant.
        let clip = vp * Vec4::new(0.0, 3000.0, 200.0, 1.0);
        assert!(
            clip.w <= 0.0,
            "test point is not actually behind the camera"
        );
        let mirrored = clip.truncate() / clip.w;
        assert!(
            (-1.0..=1.0).contains(&mirrored.x) && (-1.0..=1.0).contains(&mirrored.y),
            "expected the naive divide to land on screen (that is the bug being prevented)",
        );
    }

    /// The projection must agree with `Camera::screen_ray`, which is its inverse. Round-tripping
    /// through the real unprojection is what proves the NDC handling and the y-flip are right,
    /// rather than merely self-consistent.
    #[test]
    fn projection_round_trips_through_screen_ray() {
        let c = camera();
        let vp = c.view_proj();
        let (w, h) = (1600.0, 900.0);

        let point = Vec3::new(120.0, 300.0, 260.0);
        let px = project(&vp, point, [w, h]).expect("point should be on screen");

        // Casting a ray back through that pixel must pass through the original point.
        let (origin, dir) = c.screen_ray(px[0], px[1], w, h);
        let to_point = point - origin;
        let along = to_point.dot(dir);
        assert!(along > 0.0, "point should be in front along the pick ray");
        let closest = origin + dir * along;
        let miss = (closest - point).length();
        assert!(
            miss < 1.0,
            "projection and screen_ray disagree by {miss} units"
        );
    }

    /// Off-screen points are dropped rather than clamped to the border.
    #[test]
    fn offscreen_points_are_dropped() {
        let c = camera();
        let vp = c.view_proj();
        // Far to the side, still in front of the camera.
        assert!(project(&vp, Vec3::new(50_000.0, 0.0, 200.0), [1600.0, 900.0]).is_none());
    }

    fn world_with(names: &[(u16, &str, i32)]) -> WorldState {
        use caer_protocol::entities::Npc;
        let mut w = WorldState::new();
        for (id, name, x) in names {
            w.apply(&caer_protocol::session::ServerEvent::NpcInView(Npc {
                object_id: *id,
                speed: 0,
                heading: 0,
                x: *x as u32,
                y: 0,
                z: 200,
                model: 1,
                size: 50,
                level: 10,
                flags: 0,
                name: (*name).to_string(),
                guild: String::new(),
            }));
        }
        w
    }

    /// Distant entities get no plate, and the result is capped and ordered nearest-first so the cap
    /// keeps the entities that matter.
    #[test]
    fn collect_culls_by_distance_and_sorts_nearest_first() {
        let c = camera();
        let w = world_with(&[(1, "near", 100), (2, "mid", 900), (3, "far", 500_000)]);

        let plates = collect(
            &w,
            Vec3::ZERO,
            Vec3::new(0.0, 1000.0, 200.0),
            &c.view_proj(),
            [1600.0, 900.0],
            None,
        );

        let names: Vec<&str> = plates.iter().map(|p| p.name.as_str()).collect();
        assert!(
            !names.contains(&"far"),
            "an entity 500k units away should be culled: {names:?}"
        );
        for pair in plates.windows(2) {
            assert!(
                pair[0].dist2 <= pair[1].dist2,
                "plates must be sorted nearest-first"
            );
        }
    }

    /// Our own character must never get a nameplate — the HUD already names us.
    #[test]
    fn the_player_gets_no_nameplate() {
        use caer_protocol::session::ServerEvent;
        let c = camera();
        let mut w = world_with(&[(1, "a boar", 100)]);
        w.apply(&ServerEvent::PlayerPosition {
            x: 0.0,
            y: 0.0,
            z: 200.0,
            object_id: 99,
            heading: 0,
        });

        let plates = collect(
            &w,
            Vec3::ZERO,
            Vec3::new(0.0, 1000.0, 200.0),
            &c.view_proj(),
            [1600.0, 900.0],
            None,
        );
        assert!(
            plates.iter().all(|p| p.name != "<self>"),
            "the player must not be labelled"
        );
        assert!(
            plates.iter().any(|p| p.name == "a boar"),
            "other entities should still be labelled"
        );
    }

    /// The targeted entity is flagged so it can be drawn differently.
    #[test]
    fn the_target_is_marked() {
        let c = camera();
        let w = world_with(&[(1, "a boar", 100), (2, "a wolf", 200)]);
        let plates = collect(
            &w,
            Vec3::ZERO,
            Vec3::new(0.0, 1000.0, 200.0),
            &c.view_proj(),
            [1600.0, 900.0],
            Some(2),
        );
        let wolf = plates
            .iter()
            .find(|p| p.name == "a wolf")
            .expect("wolf should have a plate");
        let boar = plates
            .iter()
            .find(|p| p.name == "a boar")
            .expect("boar should have a plate");
        assert!(wolf.is_target);
        assert!(!boar.is_target);
    }
}
