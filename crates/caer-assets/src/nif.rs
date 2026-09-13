//! Gamebryo/NetImmerse `.nif` model reader — header layer.
//!
//! The client's `zones/Nifs/*.npk` archives (MPAK, same reader as everything else) each hold
//! one NIF. Every sampled model is **NetImmerse 4.2.1.0 or 4.2.2.0**. Format knowledge is
//! clean-room via the NifTools community documentation (our stated oracle policy).
//!
//! 4.x layout: an ASCII header line ending in `\n`, then `u32` version, `u32` block count,
//! then the blocks back-to-back. There is **no block-size table** in 4.x — a reader must know
//! every block type's field layout to walk the file, which is why the full parser is staged:
//! this module lands the header + the block-type inventory contract; geometry block layouts
//! (`NiTriShape`/`NiTriShapeData`/`NiTriStrips`/…) come next.
//!
//! Block-type inventory across a 198-model sample of the real client (2026-07-17), most
//! frequent first — the full set the parser must handle to walk any of these files:
//! NiNode, NiTriShape, NiTriShapeData, NiSourceTexture, NiMaterialProperty, NiTriStrips,
//! NiTexturingProperty, NiTriStripsData, NiStringExtraData, NiVertexColorProperty,
//! NiZBufferProperty, NiKeyframeData, NiKeyframeController, NiAlphaProperty, NiDitherProperty,
//! NiParticleSystemController (+ particle modifiers), NiStencilProperty, NiUVData,
//! NiUVController, NiLODNode, NiRotatingParticles, NiMorphData, NiGeomMorpherController.

use std::io;

/// A parsed NIF header: version + block count, with the payload offset for the block walker.
pub struct NifHeader {
    /// e.g. `0x0402_0200` = 4.2.2.0, `0x0A01_0000` = Gamebryo 10.1.0.0.
    pub version: u32,
    pub num_blocks: u32,
    /// Byte offset where the first block begins.
    pub blocks_start: usize,
    /// Gamebryo 10.1 only: each block's type name, resolved from the header's type table +
    /// per-block index (10.x blocks carry no inline type strings). Empty for 4.x files.
    pub block_types: Vec<String>,
}

/// The NetImmerse versions the DAoC 1.127 client ships and this walker handles byte-exact:
/// 4.1.0.12 (Stonehenge, mile forts, ~81 files) and the 4.2.x pair (the bulk).
/// Anything else is a red flag worth surfacing.
pub const SUPPORTED: [u32; 3] = [0x0401_000c, 0x0402_0100, 0x0402_0200];

/// The one Gamebryo version in the client: 10.1.0.0 (972 files across Nifs/Dnifs/trees —
/// the ToA/Catacombs-era inventory, verified uniform on 2026-07-18).
pub const GAMEBRYO_10_1: u32 = 0x0A01_0000;

/// Parse a NIF header — NetImmerse 4.x or Gamebryo 10.1.
///
/// 10.1 header layout (hand-verified byte-exact against skull.npk, 2026-07-18):
///   ASCII line "Gamebryo File Format, Version 10.1.0.0\n", u32 version, u32 user version,
///   u32 num blocks, u16 num block types, sized type strings, u16 type index per block,
///   one trailing u32 (observed 0). Each BLOCK is then preceded by a u32 zero separator
///   (parsed in `read_model`), and the file ends in a footer (u32 num roots + refs).
pub fn read_header(bytes: &[u8]) -> io::Result<NifHeader> {
    let err = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
    const MAGIC: &[u8] = b"NetImmerse File Format";
    const GB_MAGIC: &[u8] = b"Gamebryo File Format";
    let gamebryo = bytes.starts_with(GB_MAGIC);
    if bytes.len() < 48 || (!bytes.starts_with(MAGIC) && !gamebryo) {
        return Err(err("not a NetImmerse NIF".into()));
    }
    let nl = bytes
        .iter()
        .position(|&b| b == b'\n')
        .ok_or_else(|| err("unterminated header line".into()))?;
    if bytes.len() < nl + 9 {
        return Err(err("truncated NIF header".into()));
    }
    let u32_at =
        |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let version = u32_at(nl + 1);
    if gamebryo {
        if version != GAMEBRYO_10_1 {
            return Err(err(format!("unsupported Gamebryo version 0x{version:08x}")));
        }
        // user version u32 (observed 0), then the block-type table.
        let mut c = Cur {
            b: bytes,
            p: nl + 5,
        };
        let _user_version = c.u32()?;
        let num_blocks = c.u32()?;
        if num_blocks > 100_000 {
            return Err(err("absurd block count".into()));
        }
        let ntypes = c.u16()? as usize;
        if ntypes > 1024 {
            return Err(err("absurd block-type count".into()));
        }
        let mut types = Vec::with_capacity(ntypes);
        for _ in 0..ntypes {
            types.push(c.string()?);
        }
        let mut block_types = Vec::with_capacity(num_blocks as usize);
        for _ in 0..num_blocks {
            let i = c.u16()? as usize;
            let t = types
                .get(i)
                .ok_or_else(|| err(format!("block type index {i} out of table")))?;
            block_types.push(t.clone());
        }
        let _trailing = c.u32()?; // unknown trailing header int, observed 0
        return Ok(NifHeader {
            version,
            num_blocks,
            blocks_start: c.p,
            block_types,
        });
    }
    if !SUPPORTED.contains(&version) {
        return Err(err(format!("unsupported NIF version 0x{version:08x}")));
    }
    let num_blocks = u32_at(nl + 5);
    Ok(NifHeader {
        version,
        num_blocks,
        blocks_start: nl + 9,
        block_types: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn client_asset(root: &Path, test: &str, relative: &str) -> Vec<u8> {
        let path = crate::uiskin::resolve_ignoring_case(root, relative);
        std::fs::read(&path)
            .unwrap_or_else(|error| panic!("REQ-025: {test} requires {}: {error}", path.display()))
    }

    fn client_member(root: &Path, test: &str, archive: &str, member: &str) -> Vec<u8> {
        let path = crate::uiskin::resolve_ignoring_case(root, archive);
        crate::open_member(&path, member)
            .unwrap_or_else(|error| panic!("REQ-025: {test} requires {}: {error}", path.display()))
            .unwrap_or_else(|| panic!("REQ-025: {test} requires {member} in {}", path.display()))
    }

    fn key1(time: f32, v: f32) -> Key<1> {
        Key { time, value: [v] }
    }

    /// The weight curve is what turns a pile of vertex offsets into motion, so it is sampled
    /// rather than assumed: linear between keys, held flat outside them, and looping on the
    /// clip length.
    #[test]
    fn morph_weights_interpolate_hold_and_loop() {
        let anim = MorphAnim {
            relative: true,
            targets: vec![
                MorphTarget {
                    keys: vec![key1(0.0, 0.0), key1(2.0, 1.0)],
                    deltas: vec![[1.0, 0.0, 0.0]],
                },
                // A single key is a constant channel, not an animated one.
                MorphTarget {
                    keys: vec![key1(0.0, 0.25)],
                    deltas: vec![[0.0, 1.0, 0.0]],
                },
                // No keys at all contributes nothing.
                MorphTarget::default(),
            ],
        };
        assert!((anim.duration() - 2.0).abs() < 1e-6);

        let at = |t: f32| anim.weights_at(t);
        assert!((at(0.0)[0] - 0.0).abs() < 1e-6);
        assert!((at(1.0)[0] - 0.5).abs() < 1e-6, "must interpolate linearly");
        assert!((at(1.999)[0] - 1.0).abs() < 1e-3);
        // Looping, not clamping: a scene plays forever and a held-at-the-end leaf is a dead leaf.
        // The wrap is why `t == duration` reads as the START of the next cycle, not its end.
        assert!((at(2.0)[0] - 0.0).abs() < 1e-6, "duration wraps to zero");
        assert!((at(3.0)[0] - 0.5).abs() < 1e-6, "t must wrap on duration");
        assert!((at(1.0)[1] - 0.25).abs() < 1e-6, "one key is a constant");
        assert!((at(1.0)[2] - 0.0).abs() < 1e-6, "no keys is zero");
    }

    /// Both morph kinds animate. Midgard ships two absolute-morph meshes and one relative, so
    /// treating `relative: false` as inert silently dropped two thirds of that stage's motion —
    /// which is what the first version of this parser did.
    #[test]
    fn both_relative_and_absolute_morphs_count_as_animated() {
        let targets = vec![MorphTarget {
            keys: vec![key1(0.0, 0.0), key1(1.0, 1.0)],
            deltas: vec![[1.0, 0.0, 0.0]],
        }];
        for relative in [true, false] {
            assert!(
                MorphAnim {
                    relative,
                    targets: targets.clone()
                }
                .is_animated(),
                "relative={relative} must animate"
            );
        }
        // Still inert without motion, whichever kind it is: one key is a constant channel.
        assert!(!MorphAnim {
            relative: true,
            targets: vec![MorphTarget {
                keys: vec![key1(0.0, 1.0)],
                deltas: vec![[1.0, 0.0, 0.0]],
            }],
        }
        .is_animated());
    }

    /// Fig3 heads are not timed scenery animation. Their `NiStringExtraData` stores authored
    /// `FM<n>=<id>` lines under `UserPropBuffer`. Keeping those tags is what lets the renderer pair the four UI
    /// sliders with the artist-authored min/max targets instead of guessing from vertex location.
    #[test]
    fn retail_head_keeps_static_facial_morph_tags() {
        let Some(root) = crate::client_dep::require_caer_client("retail facial morph tags") else {
            return;
        };
        let figures = crate::figures::FigureModels::load(root.join("gamedata.mpk"))
            .expect("REQ-025: retail figure table loads");
        let head = figures
            .base_body(1, crate::figures::GENDER_MALE)
            .into_iter()
            .next()
            .expect("REQ-025: Briton male head row exists");
        let bytes = client_member(
            &root,
            "retail facial morph tags",
            &head.archive_path(),
            &head.nif_member(),
        );
        let parsed = parse_blocks(&bytes).expect("REQ-025: Briton male head block walk");
        let buffers: Vec<_> = parsed
            .iter()
            .filter_map(|block| match block {
                Block::StringExtra { name, value, .. } => Some((name.clone(), value.clone())),
                _ => None,
            })
            .collect();
        assert!(
            buffers.iter().any(|(_, value)| value.contains("FM1=3")),
            "REQ-025: source tag payload was not retained: {buffers:?}"
        );
        let unbound_tags = face_morph_tags_for(
            &parsed,
            &AvObject {
                name: String::new(),
                translation: [0.0; 3],
                rotation: [0.0; 9],
                scale: 1.0,
                properties: Vec::new(),
                controller: -1,
                extra_data: -1,
                extra_list: Vec::new(),
            },
        );
        assert_eq!(
            unbound_tags.get(1..=8),
            Some(
                &[
                    Some(3),
                    Some(4),
                    Some(1),
                    Some(2),
                    Some(5),
                    Some(6),
                    Some(7),
                    Some(8),
                ][..]
            ),
            "REQ-025: a parent-node facial buffer must resolve for an unbound leaf"
        );
        let model = read_model(&bytes).expect("REQ-025: Briton male head parses");
        let part = model
            .parts
            .iter()
            .find(|part| {
                part.morph
                    .as_ref()
                    .is_some_and(MorphAnim::has_static_targets)
            })
            .expect("REQ-025: Briton male head carries static morph targets");
        let ids: Vec<_> = part.morph_target_ids.iter().copied().collect();
        assert_eq!(
            ids.get(1..=8),
            Some(
                &[
                    Some(3),
                    Some(4),
                    Some(1),
                    Some(2),
                    Some(5),
                    Some(6),
                    Some(7),
                    Some(8),
                ][..]
            ),
            "source order is Eyes, Nose, then the two race-specific slider pairs"
        );
    }

    /// The three pre-world stages carry vertex animation and it must survive the parse. Measured
    /// 2026-08-23 with `nifstat --morphs`: Albion 135 animated parts, Midgard 3, Hibernia 325.
    /// The counts are floors, not equalities — a parser improvement that finds MORE is not a
    /// regression, one that silently finds none is the failure this catches.
    #[test]
    fn the_preworld_stages_carry_vertex_animation() {
        let Some(root) = crate::client_dep::require_caer_client("preworld morphs") else {
            return;
        };
        for (realm, archive, floor) in [
            ("Albion", "pregame/charScreenAlb.npk", 100),
            ("Midgard", "pregame/charScreenMid.npk", 3),
            ("Hibernia", "pregame/charScreenHib.npk", 300),
        ] {
            let path = crate::uiskin::resolve_ignoring_case(&root, archive);
            let members = crate::open(&path).expect("scene archive opens");
            let nif = members
                .iter()
                .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
                .expect("scene archive holds a nif");
            let model = read_model(&nif.data).expect("scene parses");
            let animated: Vec<_> = model
                .parts
                .iter()
                .filter_map(|p| p.morph.as_ref())
                .collect();
            assert!(
                animated.len() >= floor,
                "{realm}: expected at least {floor} animated parts, got {}",
                animated.len()
            );
            // Animated means it MOVES. A morpher with one keyless target is a controller that
            // does nothing, and counting those would make this gate unfalsifiable.
            for a in &animated {
                assert!(
                    a.is_animated(),
                    "{realm}: an inert morph was kept as animated"
                );
                assert!(a.duration() > 0.0, "{realm}: zero-length clip");
            }
            // And the weights must actually differ across the clip, or every frame draws the
            // same shape and the scene is still static.
            let a = animated[0];
            let (w0, wmid) = (a.weights_at(0.0), a.weights_at(a.duration() * 0.37));
            assert!(
                w0.iter().zip(&wmid).any(|(x, y)| (x - y).abs() > 1e-4),
                "{realm}: weights are constant across the clip — nothing would move"
            );
        }
    }

    /// The stage grounds are two-layer blends and the mask that mixes them is painted per vertex.
    /// Measured 2026-08-23 with `nifstat --stand`: Albion's `terrain` blends `LightGrass3.dds`
    /// over `alb_rock01.dds`, Midgard's `foregrpound:*` blend `Mid_ground02.dds` over
    /// `Mid_ground01.dds` / `mid_mound01.dds`, and every mask spans the full 0..1 range.
    ///
    /// The range is the whole assertion. A second layer with a CONSTANT mask is a layer that
    /// never shows, and the character would stand on unbroken grass exactly as before.
    #[test]
    fn the_stage_grounds_are_two_layer_blends_with_a_painted_mask() {
        let Some(root) = crate::client_dep::require_caer_client("ground blend") else {
            return;
        };
        for (realm, archive, part, layer1, layer2) in [
            (
                "Albion",
                "pregame/charScreenAlb.npk",
                "terrain",
                "lightgrass3.dds",
                "alb_rock01.dds",
            ),
            (
                "Midgard",
                "pregame/charScreenMid.npk",
                "foregrpound:1",
                "mid_ground02.dds",
                "Mid_ground01.dds",
            ),
        ] {
            let path = crate::uiskin::resolve_ignoring_case(&root, archive);
            let members = crate::open(&path).expect("scene archive opens");
            let nif = members
                .iter()
                .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
                .expect("scene archive holds a nif");
            let model = read_model(&nif.data).expect("scene parses");
            let p = model
                .parts
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case(part))
                .unwrap_or_else(|| panic!("{realm}: no `{part}` part"));

            assert!(
                p.texture
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case(layer1)),
                "{realm}: layer 1 is {:?}, expected {layer1}",
                p.texture
            );
            assert!(
                p.texture2
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case(layer2)),
                "{realm}: layer 2 is {:?}, expected {layer2} — without it the character stands \
                 on unbroken ground",
                p.texture2
            );
            assert_eq!(
                p.colors.len(),
                p.positions.len(),
                "{realm}: the blend mask is per-vertex and must cover every vertex"
            );
            let a: Vec<f32> = p.colors.iter().map(|c| c[3]).collect();
            let lo = a.iter().copied().fold(f32::MAX, f32::min);
            let hi = a.iter().copied().fold(f32::MIN, f32::max);
            assert!(
                lo < 0.05 && hi > 0.95,
                "{realm}: mask spans {lo:.2}..{hi:.2} — a constant mask shows one layer only"
            );
        }
    }

    /// Naming the same sheet in both shader slots is not a second layer. Binding a texture to
    /// blend against itself costs an upload and a sampler to reproduce the picture we had.
    #[test]
    fn a_part_is_not_two_layered_against_itself() {
        let Some(root) = crate::client_dep::require_caer_client("self blend") else {
            return;
        };
        let path = crate::uiskin::resolve_ignoring_case(&root, "pregame/charScreenHib.npk");
        let members = crate::open(&path).expect("scene archive opens");
        let nif = members
            .iter()
            .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
            .expect("scene archive holds a nif");
        let model = read_model(&nif.data).expect("scene parses");
        for p in &model.parts {
            if let (Some(a), Some(b)) = (p.texture.as_deref(), p.texture2.as_deref()) {
                assert!(
                    !a.eq_ignore_ascii_case(b),
                    "part {} blends {a} against itself",
                    p.name
                );
            }
        }
    }

    /// Hibernia's character-screen backdrop is a two-step fixed-function material, not a spare
    /// texture in the archive: `hib_clouds` is the dome base and map slot 6 (Decal 0) contributes
    /// the RGBA forest panorama. Before this gate the reader discarded slot 6, producing a flat
    /// blue sky where the retail/Eden capture has a distant forest.
    #[test]
    fn hibernia_backdrop_decal_zero_survives_as_an_alpha_overlay() {
        let Some(root) = crate::client_dep::require_caer_client("Hibernia backdrop decal") else {
            return;
        };
        let path = crate::uiskin::resolve_ignoring_case(&root, "pregame/charScreenHib.npk");
        let members = crate::open(&path).expect("scene archive opens");
        let nif = members
            .iter()
            .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
            .expect("scene archive holds a nif");
        let model = read_model(&nif.data).expect("scene parses");
        let dome = model
            .parts
            .iter()
            .find(|p| {
                p.name.eq_ignore_ascii_case("Editable Poly")
                    && p.texture
                        .as_deref()
                        .is_some_and(|t| t.eq_ignore_ascii_case("hib_clouds.dds"))
            })
            .expect("Hibernia cloud dome part");
        assert!(
            dome.texture2
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("hib_panarama01.dds")),
            "Decal 0 panorama must reach the mesh material, got {:?}",
            dome.texture2
        );
        assert_eq!(
            dome.texture2_mode,
            TextureLayerMode::OverlayAlpha,
            "the panorama's own alpha, not vertex colours, is its mask"
        );
    }

    /// A `NiTextureTransformController` names the PROPERTY it drives, not the shape wearing it.
    /// Binding by shape index found zero animated parts in scenes that visibly have them, which is
    /// what this pins. Measured 2026-08-23 with `nifstat --uv`: every controller in all three
    /// stages targets a `NiTexturingProperty`.
    #[test]
    fn uv_controllers_target_the_texturing_property_not_the_shape() {
        let Some(root) = crate::client_dep::require_caer_client("uv binding") else {
            return;
        };
        for (realm, archive, floor) in [
            ("Albion", "pregame/charScreenAlb.npk", 6),
            ("Midgard", "pregame/charScreenMid.npk", 2),
            ("Hibernia", "pregame/charScreenHib.npk", 4),
        ] {
            let path = crate::uiskin::resolve_ignoring_case(&root, archive);
            let members = crate::open(&path).expect("scene archive opens");
            let nif = members
                .iter()
                .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
                .expect("scene archive holds a nif");

            let types = read_header(&nif.data).expect("header").block_types;
            let controllers = uv_controllers(&nif.data).expect("controllers parse");
            assert!(!controllers.is_empty(), "{realm}: no UV controllers at all");
            for (block, target, _, _, _) in &controllers {
                let named = usize::try_from(*target).ok().and_then(|t| types.get(t));
                assert_eq!(
                    named.map(String::as_str),
                    Some("NiTexturingProperty"),
                    "{realm} block {block}: a UV controller drives a property, not a {named:?}"
                );
            }

            // And the binding has to reach the model. This is the assertion that was zero.
            let model = read_model(&nif.data).expect("scene parses");
            let scrolling = model.parts.iter().filter(|p| p.uv_anim.is_some()).count();
            assert!(
                scrolling >= floor,
                "{realm}: {scrolling} parts scroll, expected at least {floor} — the controllers \
                 parsed but bound to nothing"
            );
        }
    }

    /// A UV transform is absolute in time, so sampling it twice at one instant must agree, and
    /// two instants must differ — otherwise the flame sheet is merely being rewritten, not moved.
    #[test]
    fn a_uv_scroll_is_absolute_in_time_and_actually_moves() {
        let anim = UvAnim {
            channels: vec![(UvOp::TranslateV, vec![key1(0.0, 0.0), key1(1.0, 1.0)])],
        };
        assert!(anim.is_animated());
        let rest = [0.25, 0.5];
        assert_eq!(
            anim.apply(rest, 0.5),
            anim.apply(rest, 0.5),
            "not a function of t"
        );
        assert!(
            (anim.apply(rest, 0.5)[1] - 1.0).abs() < 1e-4,
            "half a unit scroll should be 0.5 + 0.5"
        );
        assert!(
            (anim.apply(rest, 0.0)[1] - anim.apply(rest, 0.75)[1]).abs() > 0.1,
            "the sheet does not move across the clip"
        );
        // U is untouched by a V-only channel.
        assert!((anim.apply(rest, 0.9)[0] - rest[0]).abs() < 1e-6);

        // A single key is a constant transform, not an animation — rewriting the buffer every
        // frame to produce the same coordinates is pure cost.
        assert!(!UvAnim {
            channels: vec![(UvOp::TranslateU, vec![key1(0.0, 0.3)])],
        }
        .is_animated());
    }

    /// A particle emitter's placement lives in the node chain above it, not in its own
    /// translation — both stages author (0,0,0) locally. Emitting at the local value puts
    /// Hibernia's portal motes at the scene origin.
    ///
    /// And `NiParticleMeshes` is not a sprite emitter. Both stages' large emitters are mesh
    /// particles, and pushing one through a billboard path draws its authored size as a
    /// camera-facing blob: Midgard's two are size 33.8 and 42.3, which whited out a quarter of
    /// the screen. The flag is what keeps them out of that path.
    #[test]
    fn stage_emitters_are_placed_by_their_parents_and_typed_by_their_block() {
        let Some(root) = crate::client_dep::require_caer_client("stage emitters") else {
            return;
        };
        for (realm, archive, want, want_mesh) in [
            ("Albion", "pregame/charScreenAlb.npk", 0, 0),
            ("Midgard", "pregame/charScreenMid.npk", 3, 2),
            ("Hibernia", "pregame/charScreenHib.npk", 1, 0),
        ] {
            let path = crate::uiskin::resolve_ignoring_case(&root, archive);
            let members = crate::open(&path).expect("scene archive opens");
            let nif = members
                .iter()
                .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
                .expect("scene archive holds a nif");
            let emitters = read_particle_emitters(&nif.data).expect("emitters parse");
            assert_eq!(emitters.len(), want, "{realm}: emitter count");
            assert_eq!(
                emitters.iter().filter(|e| e.mesh_particles).count(),
                want_mesh,
                "{realm}: mesh-particle emitters must be distinguishable from sprites"
            );
            for e in &emitters {
                assert!(
                    e.origin.iter().all(|v| v.is_finite()),
                    "{realm}: non-finite emitter origin"
                );
                // The whole point: NOT the local (0,0,0). An emitter at the scene origin is one
                // whose parent chain was never walked.
                assert!(
                    e.origin.iter().any(|v| v.abs() > 1.0),
                    "{realm}: emitter sits at the scene origin — the parent chain was not applied"
                );
                assert!(e.emit_rate > 0.0, "{realm}: emitter with no rate");
                assert!(e.lifetime > 0.0, "{realm}: emitter with no lifetime");
            }
        }
    }

    /// Real header bytes from the client's AECentTower.NIF (4.2.2.0, 178 blocks).
    #[test]
    fn parses_a_real_422_header() {
        let mut bytes = b"NetImmerse File Format, Version 4.2.2.0\n".to_vec();
        bytes.extend_from_slice(&0x0402_0200u32.to_le_bytes());
        bytes.extend_from_slice(&178u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        let h = read_header(&bytes).unwrap();
        assert_eq!(h.version, 0x0402_0200);
        assert_eq!(h.num_blocks, 178);
        let nl = bytes.iter().position(|&b| b == b'\n').unwrap();
        assert_eq!(h.blocks_start, nl + 9);
    }

    #[test]
    fn rejects_unknown_versions_and_garbage() {
        let mut bytes = b"NetImmerse File Format, Version 10.0.1.0\n".to_vec();
        bytes.extend_from_slice(&0x0A00_0100u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        assert!(read_header(&bytes).is_err());
        assert!(
            read_header(b"MPAKnot a nif at all............................................")
                .is_err()
        );
    }

    fn node(name: &str, translation: [f32; 3], children: Vec<i32>) -> Block {
        Block::Node {
            av: AvObject {
                name: name.into(),
                translation,
                rotation: glam_identity().0,
                scale: 1.0,
                properties: Vec::new(),
                controller: -1,
                extra_data: -1,
                extra_list: Vec::new(),
            },
            children,
            billboard: false,
        }
    }

    #[test]
    fn xform_mul_accumulates_translation_down_a_chain() {
        // Two pure translations compose additively (identity rotation).
        let a = (glam_identity().0, 1.0, [1.0, 2.0, 3.0]);
        let b = (glam_identity().0, 1.0, [10.0, 20.0, 30.0]);
        let ab = xform_mul(&a, &b);
        assert_eq!(ab.2, [11.0, 22.0, 33.0]);
        // A 90° yaw on `a` rotates b's offset before adding it.
        let rot = (
            [0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
            1.0,
            [0.0, 0.0, 0.0],
        );
        let p = xform_apply(&xform_mul(&rot, &b), [0.0, 0.0, 0.0]);
        assert!((p[0] - -20.0).abs() < 1e-4 && (p[1] - 10.0).abs() < 1e-4);
    }

    #[test]
    fn build_skeleton_orders_and_accumulates_world_bind() {
        // root(0) at +5z → pelvis(1) at +10z → spine(2) at +3z; a sibling leaf(3) under root.
        let blocks = vec![
            node("root", [0.0, 0.0, 5.0], vec![1, 3]),
            node("pelvis", [0.0, 0.0, 10.0], vec![2]),
            node("spine", [0.0, 0.0, 3.0], vec![]),
            node("leaf", [1.0, 0.0, 0.0], vec![]),
        ];
        let (skel, map) = build_skeleton(&blocks);
        assert_eq!(skel.bones.len(), 4);
        // Parent precedes child; world_bind sums the chain.
        let spine = skel.name_to_index["spine"];
        assert_eq!(skel.bones[spine].world_bind.2, [0.0, 0.0, 18.0]); // 5+10+3
        assert_eq!(
            skel.bones[skel.name_to_index["leaf"]].world_bind.2,
            [1.0, 0.0, 5.0]
        );
        assert!(skel.bones[spine].parent.unwrap() == skel.name_to_index["pelvis"]);
        // The block→bone map resolves a skin's NiNode refs.
        assert_eq!(map[&2], spine);
    }

    fn le(vals: &[f32]) -> Vec<u8> {
        vals.iter().flat_map(|v| v.to_le_bytes()).collect()
    }
    fn u32b(v: u32) -> Vec<u8> {
        v.to_le_bytes().to_vec()
    }

    #[test]
    fn read_keys_captures_time_and_value_dropping_tangents() {
        // Two Vector3 keys, type 2 (has tangents that must be skipped, not captured).
        let mut b = u32b(2); // count
        b.extend(u32b(2)); // type 2 (tangents)
        for t in [0.0f32, 0.5] {
            b.extend(le(&[t, t + 1.0, t + 2.0, t + 3.0])); // time + value[3]
            b.extend(le(&[0.0; 6])); // fwd + bwd tangents (2×3), skipped
        }
        let mut c = Cur { b: &b, p: 0 };
        let keys = read_keys::<3>(&mut c).unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1].time, 0.5);
        assert_eq!(keys[1].value, [1.5, 2.5, 3.5]);
        assert_eq!(c.p, b.len()); // consumed exactly — tangents accounted for
    }

    #[test]
    fn read_rot_channel_captures_quaternions() {
        let mut b = u32b(1); // count
        b.extend(u32b(1)); // type 1 quaternion
        b.extend(le(&[0.25, 1.0, 0.0, 0.0, 0.0])); // time + quat w,x,y,z
        let mut c = Cur { b: &b, p: 0 };
        match read_rot_channel(&mut c).unwrap() {
            RotChannel::Quat(k) => {
                assert_eq!(k.len(), 1);
                assert_eq!(k[0].value, [1.0, 0.0, 0.0, 0.0]);
            }
            _ => panic!("expected quaternion channel"),
        }
        assert_eq!(c.p, b.len());
    }

    #[test]
    fn track_duration_is_max_key_time() {
        let t = TrsTrack {
            rotation: RotChannel::Quat(vec![Key {
                time: 0.3,
                value: [1.0, 0.0, 0.0, 0.0],
            }]),
            translation: vec![Key {
                time: 1.2,
                value: [0.0; 3],
            }],
            scale: Vec::new(),
        };
        assert!((track_duration(&t) - 1.2).abs() < 1e-6);
    }

    #[test]
    fn canonical_biped_table_is_internally_consistent() {
        // Anchors that pin the enumeration.
        assert_eq!(canonical_biped_id("Bip01"), Some(0));
        assert_eq!(canonical_biped_id("Bip01 Pelvis"), Some(1));
        assert_eq!(canonical_biped_id("Bip01 Spine2"), Some(4));
        assert_eq!(canonical_biped_id("Bip01 Neck"), Some(6));
        assert_eq!(canonical_biped_id("Bip01 Head"), Some(11));
        // Sides: DAoC numbers the left limb block before the right (empirical margin matches).
        assert_eq!(canonical_biped_id("Bip01 L Clavicle"), Some(22));
        assert_eq!(canonical_biped_id("Bip01 R Clavicle"), Some(35));
        assert_eq!(canonical_biped_id("Bip01 L Thigh"), Some(48));
        assert_eq!(canonical_biped_id("Bip01 R Thigh"), Some(54));
        // Non-biped names don't resolve (they hold bind pose under animation).
        assert_eq!(canonical_biped_id("Bip01 Ponytail1"), None);
        assert_eq!(canonical_biped_id("body"), None);
        // No duplicate ids, no duplicate names in the table.
        let mut ids: Vec<i32> = CANONICAL_BIPED.iter().map(|(i, _)| *i).collect();
        let mut names: Vec<&str> = CANONICAL_BIPED.iter().map(|(_, n)| *n).collect();
        let (nids, nnames) = (ids.len(), names.len());
        ids.sort();
        ids.dedup();
        names.sort();
        names.dedup();
        assert_eq!(ids.len(), nids, "duplicate bone id in canonical table");
        assert_eq!(
            names.len(),
            nnames,
            "duplicate bone name in canonical table"
        );
    }

    #[test]
    fn canonical_table_binds_skel01_clip() {
        let test = "nif::canonical_table_binds_skel01_clip";
        let Some(root) = crate::client_dep::require_caer_client(test) else {
            return;
        };
        let mb = client_asset(root.as_ref(), test, "figures/Skel01.NIF");
        let cb = client_asset(root.as_ref(), test, "anims/skel_CIDLE.kfa");
        let skel = read_skeleton(&mb).unwrap();
        let clip = read_clip(&cb).unwrap();
        // Bones on the mesh that carry a canonical id.
        let ided: std::collections::HashMap<i32, &str> = skel
            .bones
            .iter()
            .filter_map(|b| b.id.map(|id| (id, b.name.as_str())))
            .collect();
        // Every core-body clip id present on the mesh must bind; spot-check the majors.
        for &(id, want) in &[
            (0, "Bip01"),
            (2, "Bip01 Spine"),
            (6, "Bip01 Neck"),
            (23, "Bip01 L UpperArm"),
            (36, "Bip01 R UpperArm"),
            (48, "Bip01 L Thigh"),
            (54, "Bip01 R Thigh"),
        ] {
            assert!(clip.tracks.contains_key(&id), "clip missing id {id}");
            assert_eq!(ided.get(&id), Some(&want), "id {id} should bind to {want}");
        }
    }

    #[test]
    fn mat_to_quat_round_trips() {
        // The blend path converts rotation matrices to quaternions and back; all four branches of
        // Shepperd's method must round-trip, including the ones a naive trace-only version gets
        // wrong (180-degree rotations, where the trace term collapses).
        for q in [
            [1.0, 0.0, 0.0, 0.0], // identity
            [0.0, 1.0, 0.0, 0.0], // 180 about X
            [0.0, 0.0, 1.0, 0.0], // 180 about Y
            [0.0, 0.0, 0.0, 1.0], // 180 about Z
            [0.5, 0.5, 0.5, 0.5], // 120 about (1,1,1)
            [
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
        ] {
            let m = quat_to_mat(q);
            let back = mat_to_quat(&m);
            let m2 = quat_to_mat(back);
            // Compare MATRICES, not quaternions: q and -q are the same rotation.
            for (a, b) in m.iter().zip(m2.iter()) {
                assert!(
                    (a - b).abs() < 1e-4,
                    "round trip failed for {q:?}: {m:?} vs {m2:?}"
                );
            }
        }
    }

    #[test]
    fn pose_blend_hits_both_endpoints_and_stays_sane_between() {
        // Blending must be exact at w=0 and w=1 (otherwise a settled state drifts off its clip),
        // and must not blow up in between.
        let test = "nif::pose_blend_hits_both_endpoints_and_stays_sane_between";
        let Some(root) = crate::client_dep::require_caer_client(test) else {
            return;
        };
        let mesh = client_asset(root.as_ref(), test, "figures/Skel01.NIF");
        let idle = client_asset(root.as_ref(), test, "anims/i_hm.kfa");
        let walk = client_asset(root.as_ref(), test, "anims/skel_walk.kfa");
        let rig = read_rigged(&mesh)
            .expect("Skel01 rig parses")
            .expect("Skel01 is skinned");
        let (idle, walk) = (read_clip(&idle).unwrap(), read_clip(&walk).unwrap());

        let near = |a: &[Xform], b: &[Xform], what: &str| {
            for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                for k in 0..3 {
                    assert!(
                        (x.2[k] - y.2[k]).abs() < 1e-3,
                        "{what}: bone {i} axis {k} differs"
                    );
                }
            }
        };
        let pa = rig.skeleton.pose(&idle, 0.3);
        let pb = rig.skeleton.pose(&walk, 0.7);
        near(
            &rig.skeleton.pose_blend(&idle, 0.3, &walk, 0.7, 0.0),
            &pa,
            "w=0 must equal clip a",
        );
        near(
            &rig.skeleton.pose_blend(&idle, 0.3, &walk, 0.7, 1.0),
            &pb,
            "w=1 must equal clip b",
        );
        // Out-of-range weights clamp rather than extrapolate into nonsense.
        near(
            &rig.skeleton.pose_blend(&idle, 0.3, &walk, 0.7, -5.0),
            &pa,
            "negative w clamps to a",
        );
        near(
            &rig.skeleton.pose_blend(&idle, 0.3, &walk, 0.7, 9.0),
            &pb,
            "large w clamps to b",
        );

        // Mid-blend: finite, and every bone stays within the span of the two endpoints plus slack.
        let mid = rig.skeleton.pose_blend(&idle, 0.3, &walk, 0.7, 0.5);
        for (i, x) in mid.iter().enumerate() {
            for k in 0..3 {
                assert!(x.2[k].is_finite(), "bone {i} axis {k} not finite mid-blend");
                let (lo, hi) = (pa[i].2[k].min(pb[i].2[k]), pa[i].2[k].max(pb[i].2[k]));
                let slack = 25.0 + (hi - lo);
                assert!(
                    x.2[k] > lo - slack && x.2[k] < hi + slack,
                    "bone {i} axis {k} flew off mid-blend"
                );
            }
            // Rotation must stay a rotation — no shear or scale creeping in.
            let m = x.0;
            let len = (m[0] * m[0] + m[3] * m[3] + m[6] * m[6]).sqrt();
            assert!(
                (len - 1.0).abs() < 0.05,
                "bone {i} rotation column not unit mid-blend ({len})"
            );
        }
    }

    #[test]
    fn gpu_bone_matrices_reproduce_cpu_skinning() {
        // The de-risking test for per-instance GPU skinning. Before any shader exists, prove the
        // matrices we will upload — flattened, column-major — blend to exactly what the CPU path
        // produces today. If this holds, a shader that computes `Σ wᵢ · palette[base + jointᵢ] · v`
        // is correct by construction, and any later visual difference is a pipeline bug rather
        // than a maths bug. That distinction is worth a lot when debugging a black screen.
        let test = "nif::gpu_bone_matrices_reproduce_cpu_skinning";
        let Some(root) = crate::client_dep::require_caer_client(test) else {
            return;
        };
        let mesh = client_asset(root.as_ref(), test, "figures/Skel01.NIF");
        let clip_bytes = client_asset(root.as_ref(), test, "anims/i_hm.kfa");
        let rig = read_rigged(&mesh)
            .expect("Skel01 rig parses")
            .expect("Skel01 is skinned");
        let clip = read_clip(&clip_bytes).expect("clip parses");

        let t = 0.5;
        let cpu = rig.skinning_palette(&clip, t);
        let gpu = rig.bone_matrices(&clip, t);
        let stride = rig.bone_stride();
        assert_eq!(
            gpu.len(),
            rig.parts.len() * stride,
            "flattened palette has the wrong length"
        );

        let mut checked = 0usize;
        for (pi, part) in rig.parts.iter().enumerate() {
            for vi in 0..part.positions.len() {
                let v = part.positions[vi];
                let (js, ws) = (part.joints[vi], part.weights[vi]);
                // Blend exactly as a vertex shader would, straight from the flat column-major array.
                let mut out = [0.0f32; 3];
                for k in 0..4 {
                    let w = ws[k];
                    if w == 0.0 {
                        continue;
                    }
                    let m = &gpu[pi * stride + js[k] as usize];
                    for r in 0..3 {
                        // column-major: m[col][row]
                        out[r] += w * (m[0][r] * v[0] + m[1][r] * v[1] + m[2][r] * v[2] + m[3][r]);
                    }
                }
                let want = rig.animated_position(&cpu, pi, vi);
                for r in 0..3 {
                    assert!(
                        (out[r] - want[r]).abs() < 1e-2,
                        "part {pi} vertex {vi} axis {r}: gpu-layout {} vs cpu {}",
                        out[r],
                        want[r]
                    );
                }
                checked += 1;
            }
        }
        assert!(
            checked > 100,
            "expected a real mesh to compare, checked only {checked} vertices"
        );
    }

    #[test]
    fn gamebryo_101_clips_parse_with_tracks() {
        // Integration check against the real client. 10.1 dropped extra-data
        // CHAINING for a counted list, so a reader that only follows `next` finds one id and gives
        // up — which silently killed 1564 of the client's 4071 clips (38% of all animation) while
        // still "resolving" fine from the tables. Both generations must yield tracks.
        let test = "nif::gamebryo_101_clips_parse_with_tracks";
        let Some(root) = crate::client_dep::require_caer_client(test) else {
            return;
        };
        // A Gamebryo 10.1 clip (the Catacombs-era generation).
        let bytes = client_asset(root.as_ref(), test, "anims/cat_i_hm.kfa");
        assert!(
            bytes.starts_with(b"Gamebryo"),
            "cat_i_hm should be the 10.1 generation"
        );
        let clip = read_clip(&bytes).expect("10.1 clip must parse");
        assert!(
            clip.tracks.len() > 10,
            "expected a full body of tracks, got {}",
            clip.tracks.len()
        );
        assert!(clip.duration > 0.0);
        // Tracks must key by canonical bone id, same as 4.x — that's what makes them retarget.
        assert!(
            clip.tracks.contains_key(&0),
            "root bone id 0 should be driven"
        );
        // ...and the 4.x path must still work.
        let bytes = client_asset(root.as_ref(), test, "anims/i_hm.kfa");
        let clip = read_clip(&bytes).expect("4.x clip must still parse");
        assert!(clip.tracks.len() > 10);
    }

    #[test]
    fn external_skeleton_bind_reproduces_the_static_mesh() {
        // Integration check against the real client: a fig3 body part carries NO
        // bones of its own, only a NiStringsExtraData bone-NAME list, so `read_rigged` binds it to a
        // 1-bone rig and the bind reconstruction is far off. Given the figure's real skeleton
        // (Briton Male's hair ships the full Biped), `read_rigged_external` must reproduce the
        // static mesh — the property the avatar's runtime gate relies on.
        let test = "nif::external_skeleton_bind_reproduces_the_static_mesh";
        let Some(root) = crate::client_dep::require_caer_client(test) else {
            return;
        };
        // Briton Male: hair (fig015) carries the rig; the body (fig002) binds to it by name.
        let hair = client_member(
            root.as_ref(),
            test,
            "figures/fig3/fig015.mpk",
            "bri_m_hair01.nif",
        );
        let body = client_member(
            root.as_ref(),
            test,
            "figures/fig3/fig002.mpk",
            "Body01_BC_m.nif",
        );
        let skel = read_skeleton(&hair).unwrap();
        assert!(
            skel.bones.len() > 100,
            "hair should ship the full Biped, got {}",
            skel.bones.len()
        );
        let stat = read_model(&body).unwrap();
        let worst = |rig: &RiggedModel| {
            let mut w = 0.0f32;
            for (pi, part) in rig.parts.iter().enumerate() {
                let Some(sp) = stat.parts.get(pi) else {
                    continue;
                };
                for vi in 0..part.positions.len() {
                    let Some(s) = sp.positions.get(vi) else {
                        continue;
                    };
                    let b = rig.bind_position(pi, vi);
                    w = w.max(
                        ((b[0] - s[0]).powi(2) + (b[1] - s[1]).powi(2) + (b[2] - s[2]).powi(2))
                            .sqrt(),
                    );
                }
            }
            w
        };
        // The part's own (bone-less) rig cannot reconstruct the mesh...
        let own = read_rigged(&body).unwrap().expect("body is skinned");
        assert!(
            own.skeleton.bones.len() <= 1,
            "fig3 body parts carry no bone tree"
        );
        assert!(worst(&own) > 10.0, "own-rig bind should be badly wrong");
        // ...but bound to the external skeleton by name, it lands on the static mesh.
        let ext = read_rigged_external(&body, &skel)
            .unwrap()
            .expect("body is skinned");
        assert!(
            worst(&ext) < 5.0,
            "external bind drifted by {}",
            worst(&ext)
        );
    }

    /// The component order is the whole point, so both arms are asserted.
    ///
    /// Known-good: identity is zero rotation; a 90-degree Z key is the Z axis at 90 degrees.
    /// Known-bad: reading the same bytes as `[x, y, z, w]` turns identity into a half-turn about
    /// X, so that reading is asserted to differ.
    #[test]
    fn quat_axis_angle_reads_w_first() {
        let (_, a) = quat_axis_angle([1.0, 0.0, 0.0, 0.0]);
        assert!(a.abs() < 1e-4, "identity must be zero rotation, got {a}");

        let s = std::f32::consts::FRAC_1_SQRT_2;
        let (axis, angle) = quat_axis_angle([s, 0.0, 0.0, s]);
        assert!(
            (angle - 90.0).abs() < 1e-3,
            "90 deg about Z must report 90, got {angle}"
        );
        assert!(
            axis[2] > 0.999 && axis[0].abs() < 1e-3 && axis[1].abs() < 1e-3,
            "axis must be +Z, got {axis:?}"
        );

        // The known-bad reading, computed the way the broken diagnostic did it.
        let bad = |q: [f32; 4]| -> f32 {
            let [_, _, _, w] = q;
            2.0 * w.clamp(-1.0, 1.0).acos().to_degrees()
        };
        assert!(
            bad([1.0, 0.0, 0.0, 0.0]) > 179.0,
            "the xyzw misreading must turn identity into a half-turn — if it stops doing that \
             this control has stopped controlling"
        );
    }

    #[test]
    fn quat_to_mat_identity_and_orthonormal() {
        assert_eq!(quat_to_mat([1.0, 0.0, 0.0, 0.0]), glam_identity().0);
        // 90° about Z (q = [cos45, 0,0, sin45]) sends +x → +y.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let m = quat_to_mat([s, 0.0, 0.0, s]);
        let p = mat_apply(&m, 1.0, [1.0, 0.0, 0.0]);
        assert!(
            (p[0] - 0.0).abs() < 1e-5 && (p[1] - 1.0).abs() < 1e-5,
            "got {p:?}"
        );
    }

    #[test]
    fn key_span_lerp_and_clamp() {
        let keys = [
            Key {
                time: 0.0,
                value: [0.0],
            },
            Key {
                time: 2.0,
                value: [10.0],
            },
        ];
        assert_eq!(sample_lerp(&keys, -1.0), Some([0.0])); // clamp before first
        assert_eq!(sample_lerp(&keys, 3.0), Some([10.0])); // clamp after last
        assert_eq!(sample_lerp(&keys, 0.5), Some([2.5])); // 25% of the way
        assert_eq!(sample_lerp::<1>(&[], 0.0), None); // empty → hold bind
    }

    #[test]
    fn nlerp_is_unit_and_hits_endpoints() {
        let a = [1.0, 0.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0, 0.0];
        assert_eq!(nlerp(a, b, 0.0), a);
        let mid = nlerp(a, b, 0.5);
        let n = (mid[0] * mid[0] + mid[1] * mid[1] + mid[2] * mid[2] + mid[3] * mid[3]).sqrt();
        assert!((n - 1.0).abs() < 1e-6, "nlerp not unit: {n}");
    }

    /// Build a 3-bone chain root→mid→tip (pure-translation binds) with canonical ids, plus a `Clip`
    /// whose tracks hold each bone's bind local — a "rest clip". `pose` must reproduce `world_bind`.
    fn rest_chain() -> (Skeleton, Clip) {
        let mk = |name: &str, parent: Option<usize>, t: [f32; 3], id: i32, pw: Xform| {
            let local = (glam_identity().0, 1.0, t);
            Bone {
                name: name.into(),
                parent,
                local,
                world_bind: xform_mul(&pw, &local),
                id: Some(id),
            }
        };
        let root = mk("Bip01", None, [0.0, 0.0, 5.0], 0, glam_identity());
        let mid = mk("Bip01 Spine", Some(0), [0.0, 0.0, 10.0], 2, root.world_bind);
        let tip = mk("Bip01 Neck", Some(1), [3.0, 0.0, 0.0], 6, mid.world_bind);
        let bones = vec![root, mid, tip];
        let name_to_index = bones
            .iter()
            .enumerate()
            .map(|(i, b)| (b.name.clone(), i))
            .collect();
        let mut tracks = std::collections::HashMap::new();
        for b in &bones {
            tracks.insert(
                b.id.unwrap(),
                TrsTrack {
                    rotation: RotChannel::Quat(vec![Key {
                        time: 0.0,
                        value: [1.0, 0.0, 0.0, 0.0],
                    }]),
                    translation: vec![Key {
                        time: 0.0,
                        value: b.local.2,
                    }],
                    scale: vec![Key {
                        time: 0.0,
                        value: [1.0],
                    }],
                },
            );
        }
        (
            Skeleton {
                bones,
                name_to_index,
            },
            Clip {
                tracks,
                duration: 0.0,
                rate: 1.0,
            },
        )
    }

    #[test]
    fn pose_of_rest_clip_reproduces_world_bind() {
        let (skel, clip) = rest_chain();
        let world = skel.pose(&clip, 0.37); // any t: constant keys hold
        for (i, b) in skel.bones.iter().enumerate() {
            assert!(
                (world[i].2[0] - b.world_bind.2[0]).abs() < 1e-4
                    && (world[i].2[1] - b.world_bind.2[1]).abs() < 1e-4
                    && (world[i].2[2] - b.world_bind.2[2]).abs() < 1e-4,
                "bone {i} world {:?} != bind {:?}",
                world[i].2,
                b.world_bind.2
            );
        }
        // tip world = (3, 0, 15): root +5z, mid +10z, tip +3x.
        assert!((world[2].2[0] - 3.0).abs() < 1e-4 && (world[2].2[2] - 15.0).abs() < 1e-4);
    }

    #[test]
    fn pose_rotating_root_swings_children() {
        let (skel, mut clip) = rest_chain();
        // Rotate the root 90° about Z; the tip (offset +3x from mid, mid stacked +z on root) swings.
        let s = std::f32::consts::FRAC_1_SQRT_2;
        clip.tracks.get_mut(&0).unwrap().rotation = RotChannel::Quat(vec![Key {
            time: 0.0,
            value: [s, 0.0, 0.0, s],
        }]);
        let world = skel.pose(&clip, 0.0);
        // The tip's +3x local offset (accumulated under the root's +90°Z) rotates toward +y.
        assert!(
            (world[2].2[0] - 0.0).abs() < 1e-4 && (world[2].2[1] - 3.0).abs() < 1e-4,
            "tip did not swing to +y: {:?}",
            world[2].2
        );
    }

    #[test]
    fn animated_position_matches_bind_under_rest_clip() {
        // Client-gated: a rigged mesh under a rest clip (its own bind as a 1-key clip) must skin to
        // the bind mesh. Uses Skel01's skeleton to synthesise the rest clip, so no anim file needed.
        let test = "nif::animated_position_matches_bind_under_rest_clip";
        let Some(root) = crate::client_dep::require_caer_client(test) else {
            return;
        };
        let bytes = client_asset(root.as_ref(), test, "figures/Skel01.NIF");
        let rig = read_rigged(&bytes)
            .expect("Skel01 rig parses")
            .expect("Skel01 is skinned");
        // Rest clip: every bone with an id holds its bind local.
        let mut tracks = std::collections::HashMap::new();
        for b in &rig.skeleton.bones {
            if let Some(id) = b.id {
                tracks.insert(
                    id,
                    TrsTrack {
                        rotation: RotChannel::None, // hold bind rotation
                        translation: vec![Key {
                            time: 0.0,
                            value: b.local.2,
                        }],
                        scale: Vec::new(),
                    },
                );
            }
        }
        let clip = Clip {
            tracks,
            duration: 0.0,
            rate: 1.0,
        };
        let palette = rig.skinning_palette(&clip, 0.0);
        let (mut max_d, mut n) = (0.0f32, 0usize);
        for (pi, p) in rig.parts.iter().enumerate() {
            for vi in 0..p.positions.len() {
                let a = rig.animated_position(&palette, pi, vi);
                let b = rig.bind_position(pi, vi);
                let d =
                    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
                max_d = max_d.max(d);
                n += 1;
            }
        }
        assert!(
            n > 0 && max_d < 1e-2,
            "rest-clip skin drifts from bind: max={max_d}"
        );
    }

    #[test]
    fn real_clip_moves_off_bind() {
        // Client-gated: skel_CIDLE at mid-clip must differ from bind (animation actually poses).
        let test = "nif::real_clip_moves_off_bind";
        let Some(root) = crate::client_dep::require_caer_client(test) else {
            return;
        };
        let mb = client_asset(root.as_ref(), test, "figures/Skel01.NIF");
        let cb = client_asset(root.as_ref(), test, "anims/skel_CIDLE.kfa");
        let rig = read_rigged(&mb)
            .expect("Skel01 rig parses")
            .expect("Skel01 is skinned");
        let clip = read_clip(&cb).unwrap();
        let world = rig.skeleton.pose(&clip, clip.duration * 0.5);
        // All transforms finite, and at least one bone moved off its bind position.
        let mut moved = false;
        for (i, b) in rig.skeleton.bones.iter().enumerate() {
            assert!(
                world[i].2.iter().all(|c| c.is_finite()),
                "non-finite pose at bone {i}"
            );
            let d = ((world[i].2[0] - b.world_bind.2[0]).powi(2)
                + (world[i].2[1] - b.world_bind.2[1]).powi(2)
                + (world[i].2[2] - b.world_bind.2[2]).powi(2))
            .sqrt();
            if d > 1.0 {
                moved = true;
            }
        }
        assert!(moved, "clip produced no motion off bind");
    }

    /// **The rig that lists the twist bone BEFORE the forearm it twists.**
    ///
    /// Kobold male is the only shipped rig ordered this way, and it is why his wrists tore while
    /// every other race's healed: the reparent needs parents to precede children, so it correctly
    /// refused, and the arm kept the client's UpperArm parent. The fix moves the bone — and its
    /// whole subtree, because Kobold hangs `ForeTwist1` and a `Shield` attachment off it.
    #[test]
    fn a_twist_bone_listed_before_its_forearm_is_moved_after_it_with_its_subtree() {
        // Order on purpose: ForeTwist (and its children) come BEFORE Forearm.
        let blocks = vec![
            node("Bip01 R UpperArm", [0.0, 0.0, 10.0], vec![1, 4]),
            node("Bip01 R ForeTwist", [0.0, 0.0, 3.0], vec![2, 3]),
            node("Bip01 R ForeTwist1", [0.0, 0.0, 1.0], vec![]),
            node("Bip01 R Shield", [0.0, 0.0, 2.0], vec![]),
            node("Bip01 R Forearm", [0.0, 0.0, 5.0], vec![5]),
            node("Bip01 R Hand", [0.0, 0.0, 4.0], vec![]),
        ];
        let (skel, _) = build_skeleton(&blocks);
        let idx = |n: &str| skel.name_to_index[n];
        let (twist, forearm) = (idx("Bip01 R ForeTwist"), idx("Bip01 R Forearm"));

        assert_eq!(
            skel.bones[twist].parent,
            Some(forearm),
            "the twist must end up on the forearm it twists"
        );
        assert!(
            forearm < twist,
            "and after the move the forearm must PRECEDE it — the FK walk is one forward pass"
        );

        // Every bone's parent still precedes it, which is the invariant the whole reorder exists
        // to preserve. A subtree left behind would break exactly this.
        for (i, b) in skel.bones.iter().enumerate() {
            if let Some(par) = b.parent {
                assert!(par < i, "{} at {i} has parent {par} after it", b.name);
            }
        }

        // The subtree came along, still hanging off the twist.
        for child in ["Bip01 R ForeTwist1", "Bip01 R Shield"] {
            assert_eq!(
                skel.bones[idx(child)].parent,
                Some(twist),
                "{child} must still hang off the twist bone"
            );
        }

        // **The bind pose is untouched.** Reparenting rebases the local so the world bind is
        // preserved; a reorder that moved a body part in bind would be a far worse bug than the
        // one being fixed.
        assert_eq!(skel.bones[twist].world_bind.2, [0.0, 0.0, 13.0]);
        assert_eq!(
            skel.bones[idx("Bip01 R ForeTwist1")].world_bind.2,
            [0.0, 0.0, 14.0]
        );
        assert_eq!(
            skel.bones[idx("Bip01 R Hand")].world_bind.2,
            [0.0, 0.0, 19.0]
        );
    }

    #[test]
    fn twist_bones_reparent_onto_the_forearm_keeping_their_bind() {
        // A DAoC-shaped arm: ForeTwist hangs off the UPPER arm, not the forearm — which means a
        // clip that rotates the forearm never reaches it, and any mesh weighted to it tears away
        // from the hand at the wrist.
        let blocks = vec![
            node("Bip01 R UpperArm", [0.0, 0.0, 10.0], vec![1, 2]),
            node("Bip01 R Forearm", [0.0, 0.0, 5.0], vec![]),
            node("Bip01 R ForeTwist", [0.0, 0.0, 8.0], vec![]),
        ];
        let (skel, _) = build_skeleton(&blocks);

        let twist = skel.name_to_index["Bip01 R ForeTwist"];
        let forearm = skel.name_to_index["Bip01 R Forearm"];
        assert_eq!(
            skel.bones[twist].parent,
            Some(forearm),
            "twist must follow the forearm it twists"
        );

        // Re-parenting must NOT move the bone: the bind pose is what every bind-vs-static check
        // validates, so it has to come out identical.
        assert_eq!(
            skel.bones[twist].world_bind.2,
            [0.0, 0.0, 18.0],
            "world bind moved"
        );

        // A twist bone with no matching forearm is left alone rather than orphaned.
        let lone = vec![
            node("Bip01 R UpperArm", [0.0, 0.0, 1.0], vec![1]),
            node("Bip01 R ForeTwist", [0.0, 0.0, 1.0], vec![]),
        ];
        let (skel2, _) = build_skeleton(&lone);
        let t2 = skel2.name_to_index["Bip01 R ForeTwist"];
        assert_eq!(
            skel2.bones[t2].parent,
            Some(skel2.name_to_index["Bip01 R UpperArm"])
        );
    }

    #[test]
    fn pack_influence_keeps_top4_and_normalises() {
        // Five influences → top four by weight, renormalised to sum 1.
        let (j, w) = pack_influence(vec![(0, 0.5), (1, 0.2), (2, 0.1), (3, 0.15), (4, 0.05)]);
        assert_eq!(j[0], 0); // heaviest first
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(!j.contains(&4)); // the 0.05 influence dropped
                                  // Fewer than four → zero-padded, still sums to 1.
        let (_, w2) = pack_influence(vec![(7, 3.0)]);
        assert_eq!(w2, [1.0, 0.0, 0.0, 0.0]);
    }

    /// Leg 10: particle controllers are consumed into [`ParticleEmitterDef`], not discarded.
    #[test]
    fn bav_teleporter_yields_particle_emitters_when_client_present() {
        let Some(root) = crate::client_dep::require_caer_client(
            "bav_teleporter_yields_particle_emitters_when_client_present",
        ) else {
            return;
        };
        let npk = root.join("zones/Nifs/BAvTeleporter.npk");
        if !npk.is_file() {
            crate::client_dep::skip_or_fail(
                "bav_teleporter_yields_particle_emitters_when_client_present",
                &format!("missing {}", npk.display()),
            );
            return;
        }
        let members = crate::open(&npk).expect("open BAvTeleporter.npk");
        let nif = members
            .iter()
            .find(|m| m.name.to_ascii_lowercase().ends_with(".nif"))
            .expect("nif member");
        // Mesh path must still succeed (particles typed, not broken).
        let _ = read_model(&nif.data).expect("BAvTeleporter mesh still loads");
        let emitters = read_particle_emitters(&nif.data).expect("particle extract");
        assert!(
            !emitters.is_empty(),
            "BAvTeleporter must expose ≥1 particle emitter, got 0"
        );
        for e in &emitters {
            eprintln!(
                "emitter {:?}: rate={} life={} size={} color={:?}",
                e.name, e.emit_rate, e.lifetime, e.size, e.color
            );
            assert!(e.emit_rate > 0.0, "emit_rate must be positive");
            assert!(e.lifetime > 0.0, "lifetime must be positive");
        }
        // Hand-verified comment: emit rate 60, lifetime 2.0 on this asset.
        let any_classic = emitters
            .iter()
            .any(|e| (e.emit_rate - 60.0).abs() < 0.5 && (e.lifetime - 2.0).abs() < 0.1);
        assert!(
            any_classic,
            "expected BAvTeleporter classic emit_rate≈60 lifetime≈2 among {emitters:?}"
        );
    }

    #[test]
    fn acidglob_yields_particle_emitters_when_client_present() {
        let Some(root) = crate::client_dep::require_caer_client(
            "acidglob_yields_particle_emitters_when_client_present",
        ) else {
            return;
        };
        let path = root.join("effects/acidglob.NIF");
        if !path.is_file() {
            crate::client_dep::skip_or_fail(
                "acidglob_yields_particle_emitters_when_client_present",
                &format!("missing {}", path.display()),
            );
            return;
        }
        let bytes = std::fs::read(&path).unwrap();
        let emitters = read_particle_emitters(&bytes).expect("acidglob particles");
        assert!(!emitters.is_empty(), "acidglob should have emitters");
        assert!(emitters
            .iter()
            .all(|e| e.emit_rate > 0.0 && e.lifetime > 0.0));
    }
}

// ============================== block walker ==============================
//
// 4.2.x field layouts pinned byte-exact against the real client (hand-walked elm2.NIF /
// AECentTower.NIF, 2026-07-17):
//  * bools are u8 (NifTools: 8-bit from 4.1.0.1)
//  * NiGeometryData has NO "has UV" bool — `num_uv_sets: u16` then the sets directly
//  * NiLODNode carries an extra unknown u32 between its LOD center and range count
//  * NiZBufferProperty has the u32 depth-function (4.1.0.12+)
// A desync fails fast: every block starts with a length-prefixed ASCII type name, so the
// walker validates each name and errors the whole model rather than guess.

/// How a part composites, from its `NiAlphaProperty` flags.
///
/// The flag word encodes a source and a destination blend factor, and the difference matters: a
/// DAoC glow texture is usually **black RGB with the shape in alpha**, meant to be *added* to what
/// is behind it. Composite one of those source-over and you get a black rectangle, which is what
/// Albion's character screen showed where a torch corona belongs. Reading the property as a bool
/// cannot tell the two apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, PartialOrd, Ord)]
pub enum AlphaMode {
    /// No `NiAlphaProperty`, or blending disabled: opaque, alpha-tested.
    #[default]
    Opaque,
    /// Source-over — `SRC_ALPHA` / `INV_SRC_ALPHA`. Foliage, decals, water sheets.
    Blend,
    /// Additive — any mode whose destination factor is `ONE`. Coronas, flames, sun discs.
    Add,
}

/// How a part's optional second texture combines with its base texture.
///
/// NetImmerse shader texture 1 is a painted vertex-mask layer (the character-screen ground
/// sheets), while fixed map slot 6 is Decal 0: an RGBA sheet composited over the base image. The
/// distinction matters for Hibernia's backdrop: its opaque cloud dome is the base and the forest
/// panorama's own alpha is the mask; treating it as a vertex blend makes the panorama unreachable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextureLayerMode {
    /// Mix base/second using the mesh's per-vertex alpha channel.
    #[default]
    VertexAlpha,
    /// Composite second over base using the sampled second texture's alpha channel.
    OverlayAlpha,
}

impl AlphaMode {
    /// Classify a `NiAlphaProperty` flag word.
    ///
    /// Layout is NetImmerse's: bit 0 enables blending, bits 1-4 are the source factor and bits
    /// 5-8 the destination factor, where 0 = `ONE` and 1 = `ZERO`.
    #[must_use]
    pub fn from_flags(flags: u16) -> Self {
        if flags & 1 == 0 {
            return Self::Opaque;
        }
        let dst = (flags >> 5) & 0xF;
        if dst == 0 {
            Self::Add
        } else {
            Self::Blend
        }
    }

    /// Does this part composite at all, rather than draw opaque?
    #[must_use]
    pub fn is_blended(self) -> bool {
        !matches!(self, Self::Opaque)
    }
}

/// One flattened, render-ready mesh part: model-space geometry + basic material.
pub struct MeshPart {
    /// The shape node's own name (e.g. `"Body1"`, `"HeadA2"`, `"Boots1"`).
    ///
    /// Creature/figure meshes name their parts by BODY SLOT, which is the only link between a mesh
    /// part and the per-slot skin columns in `monsters.csv` — those NIFs carry no internal texture
    /// at all, so without this every part had to share one skin and heads wore the torso.
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub diffuse: [f32; 3],
    /// Base texture file name (e.g. `"elmbark.dds"`), looked up in the client's Nifs dir.
    pub texture: Option<String>,
    /// How this part composites — see [`AlphaMode`].
    pub alpha: AlphaMode,
    /// True when this shape sat under a `NiBillboardNode`.
    pub billboard: bool,
    /// Per-vertex colour, empty when the shape carries none. For a
    /// [`TextureLayerMode::VertexAlpha`] second layer, alpha is its blend mask; an
    /// [`TextureLayerMode::OverlayAlpha`] layer instead supplies its own mask.
    pub colors: Vec<[f32; 4]>,
    /// Second texture layer when the part is authored as a two-layer blend.
    ///
    /// The stage grounds are painted this way: slot 7 is the sheet that covers most of the
    /// surface and slot 8 is what shows through where the vertex mask says so — grass over a
    /// stone slab in Albion, snow over rock in Midgard. Drawing slot 7 alone is why the character
    /// stands on unbroken grass and unbroken snow. A fixed-function `Decal 0` can also occupy
    /// this field: Hibernia's `hib_panarama01.dds` alpha-overlays the cloud dome and must not be
    /// discarded merely because it is not a shader slot.
    pub texture2: Option<String>,
    /// How [`Self::texture2`] combines with [`Self::texture`].
    pub texture2_mode: TextureLayerMode,
    /// Animated UV transform, when the shape carried `NiTextureTransformController`s.
    pub uv_anim: Option<UvAnim>,
    /// Vertex animation or static facial blend data bound to this part, when its shape carried a
    /// `NiGeomMorpherController`.
    ///
    /// Deltas arrive in the shape's local space and are rotated/scaled into the same flattened
    /// space as `positions`, so a consumer adds them directly without re-deriving the transform.
    pub morph: Option<MorphAnim>,
    /// Artist-authored `FM<n>=<id>` tags from a Gamebryo `UserPropBufferY`, indexed by NIF morph
    /// target.  Player heads use these tags to identify their static facial blend targets; stage
    /// scenery normally leaves this empty.  Target zero is the base shape and therefore has no
    /// `FM` tag.
    pub morph_target_ids: Vec<Option<u8>>,
}

/// One morph target: a weight curve over time, and the per-vertex offsets it blends in at
/// full weight.
///
/// `deltas` is parallel to the shape's own vertex list. `keys` is the `NiMorphData` float channel
/// for this target — sampled the same way every other float channel is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MorphTarget {
    pub keys: Vec<Key<1>>,
    pub deltas: Vec<[f32; 3]>,
}

/// `NiMorphData`: the vertex animation a `NiGeomMorpherController` drives.
///
/// Target 0 is the base shape and conventionally carries no motion; the rest are blended on top
/// of it. The pre-world stages use these for foliage — a billboard card with two or three targets
/// is a leaf cluster leaning in the wind, which is why Hibernia carries 325 of them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MorphAnim {
    /// `relative` in the block. True: each target holds per-vertex OFFSETS blended onto the rest
    /// shape. False: each target holds absolute POSITIONS and the result is their weighted sum.
    ///
    /// Both occur in the stages. Albion and Hibernia's foliage is relative; two of Midgard's three
    /// morphs are absolute, and reading them additively would draw the mesh at roughly twice its
    /// own coordinates.
    pub relative: bool,
    pub targets: Vec<MorphTarget>,
}

impl MorphAnim {
    /// The clip length — the last key time across every target, or 0 when nothing is keyed.
    #[must_use]
    pub fn duration(&self) -> f32 {
        self.targets
            .iter()
            .filter_map(|t| t.keys.last().map(|k| k.time))
            .fold(0.0, f32::max)
    }

    /// Whether this actually animates over time. A morpher whose targets are all unkeyed, or whose
    /// only time span is Gamebryo's `0.00001` static-head sentinel, is not scene animation.
    ///
    /// Player heads are intentionally separate: their authored targets are static blend shapes,
    /// selected by character appearance rather than sampled against a clock. Use
    /// [`Self::has_static_targets`] to retain those.
    ///
    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.duration() > 1.0e-4
            && self
                .targets
                .iter()
                .any(|t| t.keys.len() > 1 && !t.deltas.is_empty())
    }

    /// Whether this controller carries non-base geometry that can be chosen as a static blend
    /// shape.  Retail fig3 heads hold their twelve facial targets this way: the only two timeline
    /// keys belong to target zero and are a `0 → 0.00001` compatibility sentinel, while targets
    /// one onward carry the real source-authored facial offsets.
    #[must_use]
    pub fn has_static_targets(&self) -> bool {
        self.targets
            .iter()
            .skip(1)
            .any(|target| !target.deltas.is_empty())
    }

    /// Each target's blend weight at `t` seconds, looping over [`duration`].
    ///
    /// Linear between keys, held flat outside the keyed range, and zero for a target with no keys
    /// at all — the same convention the skeletal sampler uses, so a scene and a body agree about
    /// what an unkeyed channel means.
    ///
    /// [`duration`]: Self::duration
    #[must_use]
    pub fn weights_at(&self, t: f32) -> Vec<f32> {
        let dur = self.duration();
        let at = if dur > 0.0 && t.is_finite() {
            t.rem_euclid(dur)
        } else {
            0.0
        };
        self.targets
            .iter()
            .map(|target| sample_scalar(&target.keys, at))
            .collect()
    }
}

/// Linear sample of a scalar key channel, clamped at both ends.
fn sample_scalar(keys: &[Key<1>], t: f32) -> f32 {
    match keys {
        [] => 0.0,
        [only] => only.value[0],
        _ => {
            if t <= keys[0].time {
                return keys[0].value[0];
            }
            let last = &keys[keys.len() - 1];
            if t >= last.time {
                return last.value[0];
            }
            let hi = keys.partition_point(|k| k.time <= t).max(1);
            let (a, b) = (&keys[hi - 1], &keys[hi]);
            let span = b.time - a.time;
            if span <= f32::EPSILON {
                return b.value[0];
            }
            let f = (t - a.time) / span;
            a.value[0] + (b.value[0] - a.value[0]) * f
        }
    }
}

/// Which term of a UV transform a `NiTextureTransformController` animates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UvOp {
    TranslateU,
    TranslateV,
    Rotate,
    ScaleU,
    ScaleV,
}

impl UvOp {
    #[must_use]
    fn from_operation(op: u32) -> Option<Self> {
        match op {
            0 => Some(Self::TranslateU),
            1 => Some(Self::TranslateV),
            2 => Some(Self::Rotate),
            3 => Some(Self::ScaleU),
            4 => Some(Self::ScaleV),
            _ => None,
        }
    }
}

/// Animated UV transform on a part: one keyed channel per term.
///
/// This is what makes Albion's clouds roll and its torch flames lick — a static sheet whose
/// coordinates move under it. A controller per term is the format's own shape, so a scroll in both
/// axes arrives as two channels on the same shape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UvAnim {
    pub channels: Vec<(UvOp, Vec<Key<1>>)>,
}

impl UvAnim {
    /// Clip length across every channel.
    #[must_use]
    pub fn duration(&self) -> f32 {
        self.channels
            .iter()
            .filter_map(|(_, k)| k.last().map(|k| k.time))
            .fold(0.0, f32::max)
    }

    /// Whether anything actually moves. A single-key channel is a constant transform, which the
    /// scenes carry plenty of and which would cost a per-frame rewrite to change nothing.
    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.duration() > 0.0 && self.channels.iter().any(|(_, k)| k.len() > 1)
    }

    /// Apply this transform at `t` seconds to one UV pair, looping on [`duration`].
    ///
    /// Rotation and scale are about (0.5, 0.5) rather than the origin: a sheet scaled about its
    /// corner slides off its own quad, which reads as the texture drifting rather than zooming.
    ///
    /// [`duration`]: Self::duration
    #[must_use]
    pub fn apply(&self, uv: [f32; 2], t: f32) -> [f32; 2] {
        let dur = self.duration();
        let at = if dur > 0.0 && t.is_finite() {
            t.rem_euclid(dur)
        } else {
            0.0
        };
        let (mut u, mut v) = (uv[0], uv[1]);
        let (mut su, mut sv, mut rot) = (1.0_f32, 1.0_f32, 0.0_f32);
        let (mut du, mut dv) = (0.0_f32, 0.0_f32);
        for (op, keys) in &self.channels {
            let x = sample_scalar(keys, at);
            match op {
                UvOp::TranslateU => du += x,
                UvOp::TranslateV => dv += x,
                UvOp::Rotate => rot += x,
                UvOp::ScaleU => su *= x,
                UvOp::ScaleV => sv *= x,
            }
        }
        if su != 1.0 || sv != 1.0 || rot != 0.0 {
            let (cu, cv) = (u - 0.5, v - 0.5);
            let (c, s) = (rot.cos(), rot.sin());
            u = 0.5 + (cu * c - cv * s) * su;
            v = 0.5 + (cu * s + cv * c) * sv;
        }
        [u + du, v + dv]
    }
}

/// A parsed model: every visible mesh part, transforms flattened, nearest LOD chosen,
/// collision subtrees (`coll*` nodes, RootCollisionNode) dropped.
pub struct Model {
    pub parts: Vec<MeshPart>,
}

/// One particle emitter extracted from a NIF (leg 10 — consumed, not skip-parsed).
///
/// Fields come from `NiParticleSystemController` (layout hand-verified against BAvTeleporter.NIF).
/// Origin is the particle node's local translation (full parent chain left for a later pass).
#[derive(Debug, Clone, PartialEq)]
pub struct ParticleEmitterDef {
    pub name: String,
    pub origin: [f32; 3],
    pub speed: f32,
    pub speed_random: f32,
    pub declination: f32,
    pub declination_variation: f32,
    pub planar_angle: f32,
    pub planar_angle_variation: f32,
    pub color: [f32; 4],
    pub size: f32,
    pub emit_start: f32,
    pub emit_stop: f32,
    pub emit_rate: f32,
    pub lifetime: f32,
    pub lifetime_random: f32,
    pub grow: f32,
    pub fade: f32,
    /// True for `NiParticleMeshes`: every particle is a mesh instance, not a sprite.
    ///
    /// Rendering one of these through the billboard path draws its authored SIZE as a camera-
    /// facing blob — Midgard's two mesh emitters are size 33.8 and 42.3, which covered a quarter
    /// of the screen in white. Kept as data rather than dropped at parse: the emitter is real and
    /// a mesh-particle path can consume it later.
    pub mesh_particles: bool,
}

/// Extract every particle emitter definition from NIF bytes.
///
/// Returns an empty vec for models with no particle system (not an error). Mesh flatten behaviour
/// is unchanged — particle blocks are typed but not turned into [`MeshPart`]s.
/// World-space translation of every block reachable from the root, by block index.
///
/// A particle node's own `translation` is relative to its parent, and in both stages that value is
/// (0,0,0) — the placement lives entirely in the chain above it. Emitting at the local translation
/// puts Hibernia's portal sprites at the scene origin instead of at the portal.
fn world_translations(blocks: &[Block]) -> std::collections::HashMap<usize, [f32; 3]> {
    let mut out = std::collections::HashMap::new();
    // (block, parent rotation, parent scale, parent translation)
    let mut stack = vec![(
        0i32,
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
        1.0f32,
        [0.0f32; 3],
    )];
    let mut guard = 0usize;
    while let Some((idx, prot, pscale, ppos)) = stack.pop() {
        guard += 1;
        if guard > 100_000 {
            break; // a cyclic or malformed tree must not hang the reader
        }
        let Some(block) = usize::try_from(idx).ok().and_then(|i| blocks.get(i)) else {
            continue;
        };
        let (av, children): (&AvObject, &[i32]) = match block {
            Block::Node { av, children, .. } | Block::Lod { av, children } => (av, children),
            Block::Particles { av, .. } => (av, &[]),
            _ => continue,
        };
        let local = mat_apply(&prot, pscale, av.translation);
        let pos = [local[0] + ppos[0], local[1] + ppos[1], local[2] + ppos[2]];
        if let Ok(i) = usize::try_from(idx) {
            out.insert(i, pos);
        }
        let rot = mat_mul(&prot, &av.rotation);
        let scale = pscale * av.scale;
        for &c in children {
            stack.push((c, rot, scale, pos));
        }
    }
    out
}

pub fn read_particle_emitters(bytes: &[u8]) -> io::Result<Vec<ParticleEmitterDef>> {
    let blocks = parse_blocks(bytes)?;
    let world = world_translations(&blocks);
    let get =
        |idx: i32| -> Option<&Block> { usize::try_from(idx).ok().and_then(|i| blocks.get(i)) };

    let mut out = Vec::new();
    for (self_idx, block) in blocks.iter().enumerate() {
        let Block::Particles {
            av, mesh_particles, ..
        } = block
        else {
            continue;
        };
        // Walk the node's controller chain for a ParticleController.
        let mut ctrl_idx = av.controller;
        let mut ctrl = None;
        let mut guard = 0;
        while ctrl_idx >= 0 && guard < 64 {
            guard += 1;
            match get(ctrl_idx) {
                Some(Block::ParticleController { next, .. }) => {
                    ctrl = get(ctrl_idx);
                    let _ = next;
                    break;
                }
                Some(Block::KeyframeController { next, .. }) => ctrl_idx = *next,
                _ => break,
            }
        }
        let Some(Block::ParticleController {
            next: _,
            speed,
            speed_random,
            declination,
            declination_variation,
            planar_angle,
            planar_angle_variation,
            color,
            size,
            emit_start,
            emit_stop,
            emit_rate,
            lifetime,
            lifetime_random,
            modifier,
        }) = ctrl
        else {
            continue;
        };

        let mut grow = 0.0f32;
        let mut fade = 0.0f32;
        let mut mod_idx = *modifier;
        let mut mguard = 0;
        while mod_idx >= 0 && mguard < 32 {
            mguard += 1;
            match get(mod_idx) {
                Some(Block::ParticleGrowFade {
                    next,
                    grow: g,
                    fade: f,
                }) => {
                    grow = *g;
                    fade = *f;
                    mod_idx = *next;
                }
                Some(Block::ParticleModifierStub { next }) => mod_idx = *next,
                _ => break,
            }
        }

        out.push(ParticleEmitterDef {
            name: av.name.clone(),
            origin: world.get(&self_idx).copied().unwrap_or(av.translation),
            speed: *speed,
            speed_random: *speed_random,
            declination: *declination,
            declination_variation: *declination_variation,
            planar_angle: *planar_angle,
            planar_angle_variation: *planar_angle_variation,
            color: *color,
            size: *size,
            emit_start: *emit_start,
            emit_stop: *emit_stop,
            emit_rate: *emit_rate,
            lifetime: *lifetime,
            lifetime_random: *lifetime_random,
            grow,
            fade,
            mesh_particles: *mesh_particles,
        });
    }
    Ok(out)
}

/// One skeleton bone in the bind pose. `parent` indexes into the owning [`Skeleton::bones`] (`None`
/// for the root); `local` is the bind transform relative to the parent; `world_bind` is that
/// transform accumulated down from the root (bone space → model space at bind).
#[derive(Clone)]
pub struct Bone {
    pub name: String,
    pub parent: Option<usize>,
    pub local: Xform,
    pub world_bind: Xform,
    /// DAoC bone id (from the node's `NiStringExtraData`), the key a `.kfa` clip retargets onto —
    /// animations reference bones by this numeric id, not by name. `None` if the node carries none.
    pub id: Option<i32>,
}

/// The bones of a rigged model, topologically ordered (every parent precedes its children), plus a
/// name→index map so an animation's per-bone tracks bind by name.
#[derive(Clone)]
pub struct Skeleton {
    pub bones: Vec<Bone>,
    pub name_to_index: std::collections::HashMap<String, usize>,
}

/// One rigged mesh part: the raw skin-space geometry the bones deform, plus per-vertex bone bindings.
/// `joints`/`weights` carry up to four influences per vertex (bone index into [`Skeleton::bones`] +
/// its weight, weights summing to ~1). `inverse_bind[b]` maps skin space into bone `b`'s space at
/// bind — the animation skinning matrix for a bone is `world_anim(b) · inverse_bind[b]`.
pub struct RiggedPart {
    /// The shape node's own name — see [`MeshPart::name`]. This is what maps a part to its body
    /// slot, and therefore to its own skin.
    pub name: String,
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
    /// Per-skeleton-bone inverse-bind transform (identity for bones this part doesn't reference).
    pub inverse_bind: Vec<Xform>,
    /// Base texture file name, as the static [`MeshPart::texture`] carries it.
    ///
    /// The rigged path used to drop this, which cost the skinned meshes their NIF-named textures:
    /// creatures didn't notice (their skin comes from `monsters.csv`), but a player avatar's HAIR
    /// names its texture here and nowhere else, so it drew white.
    pub texture: Option<String>,
    /// Raw shape-space morpher data.  Unlike [`MeshPart::morph`], this is deliberately not
    /// flattened: a player head's facial offsets must be applied before skinning, in the same
    /// coordinate space as `positions` and its inverse-bind matrices.
    pub morph: Option<MorphAnim>,
    /// Artist-authored `FM<n>=<id>` tags, indexed by morph target.  See
    /// [`MeshPart::morph_target_ids`].
    pub morph_target_ids: Vec<Option<u8>>,
}

/// A rigged model: the shared skeleton and the skinned parts. Produced by [`read_rigged`]; `None`
/// when the NIF carries no skin (a static prop). The static [`read_model`] geometry is unaffected.
pub struct RiggedModel {
    pub skeleton: Skeleton,
    pub parts: Vec<RiggedPart>,
}

/// One keyed value at a time (seconds). Rotations are quaternions `[w, x, y, z]`; translations are
/// `[x, y, z]`; scales are scalars — carried as fixed-width arrays so all three share this type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key<const N: usize> {
    pub time: f32,
    pub value: [f32; N],
}

/// A bone's rotation channel. Most clips store quaternion keys; a few use per-axis XYZ Euler keys
/// (NiKeyframeData rotation-type 4) — kept separate so sampling ([A.3.3]) converts them faithfully
/// rather than guessing. Empty channels mean "no rotation animation — hold the bind rotation".
#[derive(Clone)]
pub enum RotChannel {
    None,
    Quat(Vec<Key<4>>),
    Euler {
        x: Vec<Key<1>>,
        y: Vec<Key<1>>,
        z: Vec<Key<1>>,
    },
}

/// One bone's animation track: independent rotation / translation / scale channels, each a set of
/// time-keyed values to interpolate. An empty channel holds the bone's bind value.
#[derive(Clone)]
pub struct TrsTrack {
    pub rotation: RotChannel,
    pub translation: Vec<Key<3>>,
    pub scale: Vec<Key<1>>,
}

/// A parsed animation clip: per-bone tracks keyed by DAoC **bone id** (the numeric retarget key —
/// see [`Bone::id`]), so one clip drives any mesh whose bones carry those ids. Plus the clip length
/// in seconds. Produced by [`read_clip`] from a `.kfa` file.
#[derive(Clone)]
pub struct Clip {
    pub tracks: std::collections::HashMap<i32, TrsTrack>,
    /// Length of the key timeline in seconds, in AUTHORED time (`frames / base_fps`). This is the
    /// range to sample poses over, and is NOT how long the clip lasts on screen.
    pub duration: f32,
    /// Clip-seconds to advance per real second — `animnifs.csv`'s `fps / base fps`.
    ///
    /// The `.kfa` carries no playback rate; only the anim tables know it, so whoever loads a clip
    /// has to attach it here. 1.0 means "play as authored". `I_hm`, the humanoid idle that most
    /// NPCs retarget, is 30 frames authored at 15fps but played at 4fps — a rate of 0.267. Leaving
    /// this at 1.0 ran every humanoid idle 3.75x too fast while creatures with their own
    /// authored-rate idle (the minotaur) looked correct.
    pub rate: f32,
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn need(&self, n: usize) -> io::Result<()> {
        if self.p + n > self.b.len() {
            Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "NIF truncated",
            ))
        } else {
            Ok(())
        }
    }
    fn u8(&mut self) -> io::Result<u8> {
        self.need(1)?;
        self.p += 1;
        Ok(self.b[self.p - 1])
    }
    fn bool(&mut self) -> io::Result<bool> {
        Ok(self.u8()? != 0)
    }
    fn u16(&mut self) -> io::Result<u16> {
        self.need(2)?;
        let v = u16::from_le_bytes([self.b[self.p], self.b[self.p + 1]]);
        self.p += 2;
        Ok(v)
    }
    fn i16(&mut self) -> io::Result<i16> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> io::Result<u32> {
        self.need(4)?;
        let v = u32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        Ok(v)
    }
    fn i32(&mut self) -> io::Result<i32> {
        Ok(self.u32()? as i32)
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn vec3(&mut self) -> io::Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
    fn mat33(&mut self) -> io::Result<[f32; 9]> {
        let mut m = [0.0; 9];
        for v in &mut m {
            *v = self.f32()?;
        }
        Ok(m)
    }
    fn skip(&mut self, n: usize) -> io::Result<()> {
        self.need(n)?;
        self.p += n;
        Ok(())
    }
    fn string(&mut self) -> io::Result<String> {
        self.string_max(4096)
    }

    /// A string whose length may legitimately be large.
    ///
    /// The 4096 cap on [`Cur::string`] is a desync tripwire, and it is the right bound for names
    /// and identifiers — but it is not a property of *every* string. `charScreenHib.nif` carries a
    /// `NiStringExtraData` named `UserPropBuffer` holding **16,568 bytes** of 3ds Max user
    /// properties, which the tripwire rejected as "absurd string length (desync)" at block #3805.
    /// The stream was never out of sync: blocks #3804 and #3805 both decode byte-exactly by hand.
    /// A tripwire that fires on valid data costs more than it catches — it read as a parser gap in
    /// the pre-world scene work for a file that was fine.
    ///
    /// Payload strings keep a bound, just an honest one: [`Cur::need`] already refuses anything
    /// past the end of the buffer, and this rejects the obviously-garbage lengths (0xFFFF_FFFF and
    /// friends) that a real desync produces.
    fn string_max(&mut self, limit: usize) -> io::Result<String> {
        let n = self.u32()? as usize;
        if n > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("absurd string length {n} (limit {limit}) (desync)"),
            ));
        }
        self.need(n)?;
        let s = String::from_utf8_lossy(&self.b[self.p..self.p + n]).into_owned();
        self.p += n;
        Ok(s)
    }
}

/// Upper bound for an extra-data *payload* string. Generous because `UserPropBuffer` holds
/// whatever the artist typed into Max; still far below any real file size, so a garbage length
/// fails immediately rather than allocating.
const EXTRA_DATA_STRING_MAX: usize = 1 << 20;

/// Common leading fields of every scene object (NiObjectNET + NiAVObject, 4.2.x).
struct AvObject {
    name: String,
    translation: [f32; 3],
    rotation: [f32; 9],
    scale: f32,
    properties: Vec<i32>,
    /// Block ref to the node's first `NiTimeController` (`-1` if none) — heads the controller chain.
    controller: i32,
    /// Block ref to the node's first `NiExtraData` (`-1` if none) — heads the `NiStringExtraData`
    /// chain that carries a `.kfa`'s bone-id retargeting strings.
    extra_data: i32,
    /// EVERY extra-data ref on the object. 4.x stores one ref that chains via `next`; 10.x stores a
    /// counted LIST and drops the chain entirely. A 10.x `.kfa` puts its whole bone-id sequence
    /// here, so keeping only the first ref loses the clip (see [`read_clip`]).
    extra_list: Vec<i32>,
}

/// The blocks the flattener cares about; everything else parses (to stay in sync) into Other.
enum Block {
    Node {
        av: AvObject,
        children: Vec<i32>,
        billboard: bool,
    },
    /// LOD: children ordered nearest-first; the flattener takes the first (highest-detail).
    Lod {
        av: AvObject,
        children: Vec<i32>,
    },
    /// `skin` = block ref to this shape's `NiSkinInstance` (`-1` for a static, unrigged shape). The
    /// static flattener ignores it; the rigged path (`read_rigged`) uses it to attach bone weights.
    Shape {
        av: AvObject,
        data: i32,
        skin: i32,
    },
    ShapeData(GeoData),
    /// `NiTextureTransformController`: one animated channel of a UV transform on `slot`.
    /// `operation` selects which: 0 translate U, 1 translate V, 2 rotate, 3 scale U, 4 scale V.
    TexTransform {
        target: i32,
        slot: u32,
        operation: u32,
        data: i32,
    },
    FloatData(Vec<Key<1>>),
    /// `NiGeomMorpherController`: `target` is the shape it drives, `data` refs the `NiMorphData`.
    /// The chain's `next` is parsed for sync and dropped — a shape's morpher is found by `target`
    /// or by being the head of its own chain, and neither walks past one.
    GeomMorpher {
        target: i32,
        data: i32,
    },
    MorphData(MorphAnim),
    Material {
        diffuse: [f32; 3],
    },
    Texturing {
        base_source: i32,
        /// Shader texture 1 — the SECOND layer of a two-layer blend, `-1` when the part has none.
        /// Slot 7 covers the surface, this shows through where the vertex mask says so.
        blend_source: i32,
        /// Every populated map slot as `(slot, NiSourceTexture ref)`, base included.
        ///
        /// Every stage stays available to material resolution. The renderer selects a base plus
        /// the supported authored secondary stage (shader layer or `Decal 0`); retaining this
        /// complete list lets diagnostics distinguish an unsupported later stage from a missing
        /// asset.
        slots: Vec<(usize, i32)>,
        /// Number of fixed map slots; entries in `slots` at or above it are shader textures.
        map_slots: usize,
    },
    SourceTexture {
        file: Option<String>,
    },
    AlphaProp {
        flags: u16,
    },
    /// `NiSkinInstance`: links a shape to its `NiSkinData` and lists the bones (as `NiNode` block
    /// refs) the skin's per-bone data is ordered by.
    SkinInstance {
        data: i32,
        bones: Vec<i32>,
    },
    /// `NiSkinData`: per-bone `(inverse-bind transform, the per-vertex weights)` in the same order as
    /// the owning `SkinInstance`'s bone list. (The block's leading overall skin→shape transform is
    /// parsed for sync but dropped — it's the shape's inverse-placement, not part of the skinning
    /// chain; folding it in mis-shifts every vertex. See [`read_rigged`].)
    SkinData {
        bones: Vec<BoneSkin>,
    },
    /// `NiKeyframeController`: `next` chains to the following controller; `data` = block ref to the
    /// `NiKeyframeData` holding its TRS keys. (The controller's `target` back-ref is null in DAoC's
    /// `.kfa`; binding is by the parallel bone-id chain — see [`read_clip`].)
    KeyframeController {
        next: i32,
        data: i32,
    },
    /// `NiKeyframeData`: one bone's animation track (rotation + translation + scale key sets).
    KeyframeData(TrsTrack),
    /// `NiStringExtraData` (4.x): `next` chains to the following extra-data; `value` is the string —
    /// in a `.kfa`, the bone id the parallel controller's track retargets onto.
    StringExtra {
        next: i32,
        /// 10.x stores a semantic key before the payload.  Player heads use
        /// `UserPropBufferY` to name static facial morph targets (`FM1=…`).
        name: Option<String>,
        value: String,
    },
    /// `NiStringsExtraData`: a counted string array. On fig3 player-body parts this is the skin's
    /// **bone NAME list** — those parts carry no bones of their own, so the names are what bind the
    /// skin to an EXTERNAL skeleton NIF (see [`read_rigged_external`]).
    StringsExtra {
        values: Vec<String>,
    },
    /// Particle node (`NiParticles` / `NiRotatingParticles` / `NiAutoNormalParticles`).
    Particles {
        av: AvObject,
        #[allow(dead_code)] // retained for future spawn-from-vertex work
        data: i32,
        /// `NiParticleMeshes`: each particle is a MESH, not a camera-facing sprite.
        mesh_particles: bool,
    },
    /// Per-particle geometry extras (positions from geometry common + optional sizes).
    #[allow(dead_code)] // retained for future spawn-from-vertex work
    ParticlesData {
        positions: Vec<[f32; 3]>,
        sizes: Vec<f32>,
    },
    /// `NiParticleSystemController` / `NiBSPArrayController` — emitter parameters.
    ParticleController {
        next: i32,
        speed: f32,
        speed_random: f32,
        declination: f32,
        declination_variation: f32,
        planar_angle: f32,
        planar_angle_variation: f32,
        color: [f32; 4],
        size: f32,
        emit_start: f32,
        emit_stop: f32,
        emit_rate: f32,
        lifetime: f32,
        lifetime_random: f32,
        /// First modifier ref (often GrowFade); `-1` if none.
        modifier: i32,
    },
    ParticleGrowFade {
        next: i32,
        grow: f32,
        fade: f32,
    },
    /// Other particle modifiers — chain only (payload skipped).
    ParticleModifierStub {
        next: i32,
    },
    Other,
}

/// One bone's contribution to a skin, from `NiSkinData`: the inverse-bind transform (skin space →
/// this bone's space at bind) and the `(vertex index, weight)` pairs it drives.
struct BoneSkin {
    inverse_bind: Xform,
    weights: Vec<(u16, f32)>,
}

struct GeoData {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    triangles: Vec<[u16; 3]>,
    /// Per-vertex `Color4`, empty when the shape stores none.
    ///
    /// Not decoration. The stage grounds are authored as two texture layers and the blend between
    /// them is painted here — skipping this is why a character stands on unbroken grass where the
    /// asset paints a stone slab under their feet.
    colors: Vec<[f32; 4]>,
}

fn err(m: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m.into())
}

/// `NiObjectNET` prefix. 4.x chains extra data via a single ref; 10.x replaces it with a
/// counted ref LIST (byte-verified against skull.npk: name, u32 count, refs, controller).
/// `NiObjectNET` prefix: name, extra-data ref(s), and the block ref to this object's first
/// `NiTimeController` (`-1` if none). The controller ref is how an animation's `.kfa` binds a track
/// to its bone node — the node points at the `NiKeyframeController` that drives it.
fn read_onet_name(c: &mut Cur, v10: bool) -> io::Result<(String, i32, i32, Vec<i32>)> {
    let name = c.string()?;
    // First extra-data ref (4.x: a single ref + chain; 10.x: a counted list). In a `.kfa`/rigged
    // NIF this heads the `NiStringExtraData` chain carrying bone-id retargeting strings.
    let extra = if v10 {
        let n = c.u32()? as usize;
        if n > 512 {
            return Err(err("absurd extra-data count (desync)"));
        }
        let mut list = Vec::with_capacity(n);
        for _ in 0..n {
            list.push(c.i32()?);
        }
        let first = list.first().copied().unwrap_or(-1);
        (first, list)
    } else {
        let r = c.i32()?;
        (r, vec![r])
    };
    let (extra, extra_list) = extra;
    let controller = c.i32()?;
    Ok((name, extra, controller, extra_list))
}

fn read_av(c: &mut Cur, v10: bool) -> io::Result<AvObject> {
    let (name, extra_data, controller, extra_list) = read_onet_name(c, v10)?;
    let _flags = c.u16()?;
    let translation = c.vec3()?;
    let rotation = c.mat33()?;
    let scale = c.f32()?;
    if !v10 {
        let _velocity = c.vec3()?; // dropped in 10.x
    }
    let nprops = c.u32()? as usize;
    if nprops > 512 {
        return Err(err("absurd property count (desync)"));
    }
    let mut properties = Vec::with_capacity(nprops);
    for _ in 0..nprops {
        properties.push(c.i32()?);
    }
    if v10 {
        // 10.x: velocity + bounding volume are gone; a collision-object ref closes the block
        // (byte-verified: skull.npk Scene Root ends name/props with ref -1).
        let _collision = c.i32()?;
    } else if c.bool()? {
        read_bounding_volume(c)?;
    }
    Ok(AvObject {
        name,
        translation,
        rotation,
        scale,
        properties,
        controller,
        extra_data,
        extra_list,
    })
}

fn read_bounding_volume(c: &mut Cur) -> io::Result<()> {
    match c.u32()? {
        0 => c.skip(16), // sphere: center + radius
        1 => c.skip(60), // box: center + axes + extents
        2 => c.skip(32), // capsule: center, origin, radius, extent
        4 => {
            // union: nested volumes
            let n = c.u32()? as usize;
            if n > 64 {
                return Err(err("absurd BV union size"));
            }
            for _ in 0..n {
                read_bounding_volume(c)?;
            }
            Ok(())
        }
        5 => c.skip(28), // half-space: plane + center
        t => Err(err(format!("BV type {t} unhandled"))),
    }
}

fn read_node_refs(c: &mut Cur) -> io::Result<(Vec<i32>, Vec<i32>)> {
    let nch = c.u32()? as usize;
    if nch > 4096 {
        return Err(err("absurd child count (desync)"));
    }
    let mut children = Vec::with_capacity(nch);
    for _ in 0..nch {
        children.push(c.i32()?);
    }
    let neff = c.u32()? as usize;
    if neff > 256 {
        return Err(err("absurd effect count (desync)"));
    }
    let mut effects = Vec::with_capacity(neff);
    for _ in 0..neff {
        effects.push(c.i32()?);
    }
    Ok((children, effects))
}

/// 10.1 `NiDynamicEffect`: a counted affected-node ref list follows the AVObject (4.2.x has
/// none — verified byte-exact back when the light blocks landed).
fn read_affected_nodes(c: &mut Cur) -> io::Result<()> {
    let n = c.u32()? as usize;
    if n > 1024 {
        return Err(err("absurd affected-node count (desync)"));
    }
    for _ in 0..n {
        let _node = c.i32()?;
    }
    Ok(())
}

/// `NiGeometryData` common prefix: vertices, normals, bound, colours, UV sets — everything up
/// to (but not including) the triangle section, which is TriBasedGeom-only. Particle data
/// blocks (`NiParticlesData`) end here and carry per-particle extras instead of triangles.
/// 10.1 field-order changes (per NifTools docs, footer-verified against the real client):
/// keep/compress flag bytes after the vertex count; a `vector_flags` u16 right after the
/// vertex array carrying the UV-set count (low 6 bits) and the tangents bit (0x1000, tangent +
/// bitangent arrays follow the normals); UV count no longer its own u16; a trailing
/// `consistency_flags` u16 closes the base struct (before any triangle section).
fn read_geometry_common(c: &mut Cur, v10: bool) -> io::Result<GeoData> {
    let nv = c.u16()? as usize;
    if v10 {
        let _keep_flags = c.u8()?;
        let _compress_flags = c.u8()?;
    }
    let mut positions = Vec::new();
    if c.bool()? {
        positions.reserve(nv);
        for _ in 0..nv {
            positions.push(c.vec3()?);
        }
    }
    let vector_flags = if v10 { c.u16()? } else { 0 };
    let mut normals = Vec::new();
    if c.bool()? {
        normals.reserve(nv);
        for _ in 0..nv {
            normals.push(c.vec3()?);
        }
    }
    if v10 && vector_flags & 0x1000 != 0 {
        c.skip(nv * 24)?; // tangents + bitangents
    }
    let _center = c.vec3()?;
    let _radius = c.f32()?;
    let mut colors = Vec::new();
    if c.bool()? {
        colors.reserve(nv);
        for _ in 0..nv {
            colors.push([c.f32()?, c.f32()?, c.f32()?, c.f32()?]);
        }
    }
    let nuv = if v10 {
        (vector_flags & 0x3f) as usize
    } else {
        (c.u16()? & 0x3f) as usize // low bits = set count
    };
    let mut uvs = Vec::new();
    for set in 0..nuv {
        for _ in 0..nv {
            let u = c.f32()?;
            let v = c.f32()?;
            if set == 0 {
                uvs.push([u, v]);
            }
        }
    }
    if v10 {
        let _consistency_flags = c.u16()?;
    }
    Ok(GeoData {
        positions,
        normals,
        uvs,
        colors,
        triangles: Vec::new(),
    })
}

fn read_geometry_data(c: &mut Cur, strips: bool, v10: bool) -> io::Result<GeoData> {
    let mut geo = read_geometry_common(c, v10)?;
    let ntri = c.u16()? as usize;
    let mut triangles = Vec::with_capacity(ntri);
    if strips {
        // NiTriStripsData has NO num_triangle_points u32 — that field is TriShapeData-only
        // (verified byte-exact: nv=21, ntri=36, one 38-point strip → 36 tris).
        let nstrips = c.u16()? as usize;
        let mut lens = Vec::with_capacity(nstrips);
        for _ in 0..nstrips {
            lens.push(c.u16()? as usize);
        }
        // 10.x adds a has-points bool; false = strip lengths only, no point data at all.
        if !v10 || c.bool()? {
            for len in lens {
                let mut strip = Vec::with_capacity(len);
                for _ in 0..len {
                    strip.push(c.u16()?);
                }
                for w in strip.windows(3).enumerate() {
                    let (i, t) = w;
                    let (a, b, cc) = (t[0], t[1], t[2]);
                    if a != b && b != cc && a != cc {
                        // strips alternate winding
                        triangles.push(if i % 2 == 0 { [a, b, cc] } else { [b, a, cc] });
                    }
                }
            }
        }
    } else {
        let _num_triangle_points = c.u32()?;
        // 10.x adds a has-triangles bool; the count stays even when the data is absent.
        if !v10 || c.bool()? {
            for _ in 0..ntri {
                triangles.push([c.u16()?, c.u16()?, c.u16()?]);
            }
        }
        let nmatch = c.u16()? as usize;
        for _ in 0..nmatch {
            let cnt = c.u16()? as usize;
            c.skip(cnt * 2)?;
        }
    }
    geo.triangles = triangles;
    Ok(geo)
}

/// `NiObjectNET` prefix shared by all property blocks (see `read_onet_name` for the 4.x/10.x
/// extra-data difference).
fn read_onet(c: &mut Cur, v10: bool) -> io::Result<()> {
    let (_name, _extra, _controller, _extra_list) = read_onet_name(c, v10)?;
    Ok(())
}

/// One float-key group of NiKeyframeData/NiUVData/NiMorphData (`value_size` = floats per value).
fn skip_key_group(c: &mut Cur, value_size: usize) -> io::Result<()> {
    let n = c.u32()? as usize;
    if n > 100_000 {
        return Err(err("absurd key count (desync)"));
    }
    if n == 0 {
        return Ok(());
    }
    let ty = c.u32()?;
    let per = match ty {
        1 => 1 + value_size,     // time + value
        2 => 1 + value_size * 3, // + forward/backward tangents
        3 => 1 + value_size + 3, // + TBC
        t => return Err(err(format!("key type {t} unhandled"))),
    };
    c.skip(n * per * 4)
}

/// NiTimeController prefix. Returns `(next, target)` — the ref to the next controller in the chain,
/// and the object this controller drives — both used to walk/bind an animation `.kfa`.
fn read_time_controller(c: &mut Cur) -> io::Result<(i32, i32)> {
    let next = c.i32()?;
    let _flags = c.u16()?;
    let _frequency = c.f32()?;
    let _phase = c.f32()?;
    let _start = c.f32()?;
    let _stop = c.f32()?;
    let target = c.i32()?;
    Ok((next, target))
}

/// Capture a float-key group (NiKeyframeData translation/scale channels): `n` keys of `N` floats
/// each, time-tagged. Interpolation extras (type-2 tangents, type-3 TBC) are parsed for sync but
/// dropped — sampling ([A.3.3]) interpolates linearly. Mirrors [`skip_key_group`] byte-for-byte.
fn read_keys<const N: usize>(c: &mut Cur) -> io::Result<Vec<Key<N>>> {
    let n = c.u32()? as usize;
    if n > 100_000 {
        return Err(err("absurd key count (desync)"));
    }
    if n == 0 {
        return Ok(Vec::new());
    }
    let ty = c.u32()?;
    let mut keys = Vec::with_capacity(n);
    for _ in 0..n {
        let time = c.f32()?;
        let mut value = [0.0f32; N];
        for v in &mut value {
            *v = c.f32()?;
        }
        match ty {
            1 => {}
            2 => c.skip(N * 2 * 4)?, // forward + backward tangents (one N-vector each)
            3 => c.skip(3 * 4)?,     // TBC (tension/bias/continuity)
            t => return Err(err(format!("key type {t} unhandled"))),
        }
        keys.push(Key { time, value });
    }
    Ok(keys)
}

/// Capture a rotation channel (NiKeyframeData). Quaternion keys (types 1/2/3, stored `[w,x,y,z]`;
/// type 3 adds a TBC triple) or per-axis XYZ Euler float keys (type 4). Mirrors
/// [`skip_rotation_keys`] byte-for-byte.
fn read_rot_channel(c: &mut Cur) -> io::Result<RotChannel> {
    let n = c.u32()? as usize;
    if n > 100_000 {
        return Err(err("absurd rot-key count"));
    }
    if n == 0 {
        return Ok(RotChannel::None);
    }
    match c.u32()? {
        ty @ 1..=3 => {
            let mut keys = Vec::with_capacity(n);
            for _ in 0..n {
                let time = c.f32()?;
                let value = [c.f32()?, c.f32()?, c.f32()?, c.f32()?]; // quaternion w,x,y,z
                if ty == 3 {
                    c.skip(3 * 4)?; // TBC
                }
                keys.push(Key { time, value });
            }
            Ok(RotChannel::Quat(keys))
        }
        4 => {
            let _unknown = c.u32()?; // axis order, precedes the three per-axis groups
            let x = read_keys::<1>(c)?;
            let y = read_keys::<1>(c)?;
            let z = read_keys::<1>(c)?;
            Ok(RotChannel::Euler { x, y, z })
        }
        t => Err(err(format!("rot key type {t} unhandled"))),
    }
}

fn read_block(c: &mut Cur, name: &str, version: u32) -> io::Result<Block> {
    // One dispatch serves both eras: 4.x layouts stay byte-identical, 10.1 (Gamebryo) branches
    // where the format evolved. New 10.x-only types sit at the end of the match.
    let v10 = version >= 0x0A00_0000;
    Ok(match name {
        "NiNode" | "RootCollisionNode" | "AvoidNode" | "NiBillboardNode" | "NiBSAnimationNode" => {
            let av = read_av(c, v10)?;
            let (children, _effects) = read_node_refs(c)?;
            let billboard = name == "NiBillboardNode";
            if v10 && billboard {
                let _mode = c.u16()?; // billboard mode enum, new in 10.1
            }
            Block::Node {
                av,
                children,
                billboard,
            }
        }
        "NiLODNode" => {
            let av = read_av(c, v10)?;
            let (children, _effects) = read_node_refs(c)?;
            if v10 {
                // 10.1: NiLODNode inherits NiSwitchNode (flags + active index) and its level
                // ranges moved out to a NiRangeLODData/NiScreenLODData block.
                let _switch_flags = c.u16()?;
                let _index = c.i32()?;
                let _lod_data = c.i32()?;
            } else {
                let _center = c.vec3()?;
                let _unknown = c.u32()?;
                let n = c.u32()? as usize;
                if n > 64 {
                    return Err(err("absurd LOD count"));
                }
                c.skip(n * 8)?;
            }
            Block::Lod { av, children }
        }
        "NiTriShape" | "NiTriStrips" => {
            let av = read_av(c, v10)?;
            let data = c.i32()?;
            let skin = c.i32()?; // -> NiSkinInstance (-1 if this shape isn't rigged)
            if v10 && c.bool()? {
                let _shader_name = c.string()?; // has-shader: name + unknown ref, new in 10.x
                let _unknown = c.i32()?;
            }
            Block::Shape { av, data, skin }
        }
        // Particle emitters share NiTriShape's layout (AVObject + data + skin). Typed for leg 10.
        "NiAutoNormalParticles" | "NiRotatingParticles" | "NiParticles" | "NiParticleMeshes" => {
            let mesh_particles = name == "NiParticleMeshes";
            let av = read_av(c, v10)?;
            let data = c.i32()?;
            let _skin = c.i32()?;
            if v10 && c.bool()? {
                let _shader_name = c.string()?;
                let _unknown = c.i32()?;
            }
            Block::Particles {
                av,
                data,
                mesh_particles,
            }
        }
        "NiAutoNormalParticlesData"
        | "NiParticlesData"
        | "NiRotatingParticlesData"
        | "NiParticleMeshesData" => {
            // Geometry common only — particle data has NO triangle section.
            let geo = read_geometry_common(c, v10)?;
            let nv = geo.positions.len();
            let mut sizes = Vec::new();
            if v10 {
                if c.bool()? {
                    c.skip(nv * 4)?; // radii
                }
                let _active = c.u16()?;
                if c.bool()? {
                    sizes.reserve(nv);
                    for _ in 0..nv {
                        sizes.push(c.f32()?);
                    }
                }
                if c.bool()? {
                    c.skip(nv * 16)?; // rotation quaternions
                }
            } else {
                let _radius = c.f32()?;
                let _active = c.u16()?;
                if c.bool()? {
                    sizes.reserve(nv);
                    for _ in 0..nv {
                        sizes.push(c.f32()?);
                    }
                }
                if name == "NiRotatingParticlesData" && c.bool()? {
                    c.skip(nv * 16)?;
                }
            }
            if name == "NiParticleMeshesData" {
                // The one field this type adds over NiRotatingParticlesData: a ref to the mesh
                // instanced per particle (Midgard's snowfall). We do not render particles yet, so
                // the link is consumed for stream position and dropped.
                let _model = c.i32()?;
            }
            Block::ParticlesData {
                positions: geo.positions,
                sizes,
            }
        }
        "NiTriShapeData" => Block::ShapeData(read_geometry_data(c, false, v10)?),
        "NiTriStripsData" => Block::ShapeData(read_geometry_data(c, true, v10)?),
        "NiMaterialProperty" => {
            read_onet(c, v10)?;
            if !v10 {
                let _flags = c.u16()?; // property flags dropped in 10.x
            }
            let _ambient = c.vec3()?;
            let diffuse = c.vec3()?;
            let _specular = c.vec3()?;
            let _emissive = c.vec3()?;
            let _gloss = c.f32()?;
            let _alpha = c.f32()?;
            Block::Material { diffuse }
        }
        "NiTexturingProperty" => {
            read_onet(c, v10)?;
            if !v10 {
                let _flags = c.u16()?; // dropped in 10.x (returns in 20.1+, irrelevant here)
            }
            let _apply = c.u32()?;
            let count = c.u32()? as usize;
            if count > 16 {
                return Err(err("absurd texture slot count"));
            }
            // One TexDesc: shared by the fixed map slots and the 10.x shader-texture list.
            let tex_desc = |c: &mut Cur| -> io::Result<i32> {
                let src = c.i32()?;
                let _clamp = c.u32()?;
                let _filter = c.u32()?;
                let _uv_set = c.u32()?;
                let _ps2_l = c.i16()?;
                let _ps2_k = c.i16()?;
                if version <= 0x0401_000c {
                    let _unknown = c.u16()?; // trailing unknown short, dropped after 4.1.0.12
                }
                if v10 && c.bool()? {
                    // has-transform: translation + tiling + w-rotation + type + center offset
                    c.skip(2 * 4 + 2 * 4 + 4 + 4 + 2 * 4)?;
                }
                Ok(src)
            };
            let mut map_base = -1;
            let mut shader_base = -1;
            let mut shader_blend = -1;
            let mut slots = Vec::new();
            for slot in 0..count {
                if c.bool()? {
                    let src = tex_desc(c)?;
                    slots.push((slot, src));
                    if slot == 0 {
                        map_base = src;
                    }
                    if slot == 5 {
                        c.skip(4 + 4 + 16)?; // bump luma scale/offset + 2x2 matrix
                    }
                }
            }
            if v10 {
                let nshader = c.u32()? as usize;
                if nshader > 64 {
                    return Err(err("absurd shader texture count"));
                }
                for i in 0..nshader {
                    if c.bool()? {
                        let src = tex_desc(c)?;
                        let _map_id = c.u32()?;
                        slots.push((count + i, src));
                        if i == 0 {
                            shader_base = src;
                        }
                        if i == 1 {
                            shader_blend = src;
                        }
                    }
                }
            }
            // Shader texture 0 wins over the fixed map slot when both exist. These scenes are
            // authored for a blend shader (tex0 = the object's own diffuse, tex1 = the ground
            // layer drifted over it), and map slot 0 is a fixed-function fallback the artists let
            // rot: it holds a prop atlas on Midgard's ground sheet, `mid_rock01.nif` on its rock,
            // rock on Albion's grass terrain, and moss on Hibernia's tree trunks.
            let base_source = if shader_base >= 0 {
                shader_base
            } else {
                map_base
            };
            Block::Texturing {
                base_source,
                blend_source: shader_blend,
                slots,
                map_slots: count,
            }
        }
        "NiSourceTexture" => {
            read_onet(c, v10)?;
            let file = if c.bool()? {
                let f = c.string()?;
                if v10 {
                    let _unknown_link = c.i32()?; // new in 10.1 alongside the external name
                }
                Some(f)
            } else if v10 {
                // internal: original file name + pixel-data ref (unknown byte gone in 10.x)
                let _orig_name = c.string()?;
                let _pixel_ref = c.i32()?;
                None
            } else {
                let _unknown = c.u8()?;
                let _pixel_ref = c.i32()?;
                None
            };
            let _layout = c.u32()?;
            let _mipmaps = c.u32()?;
            let _alpha_fmt = c.u32()?;
            // NB: no direct-render byte at 10.1.0.0 — that field arrives at 10.1.0.106
            // (hand-verified against OCMPier.npk: is_static is the block's last byte).
            let _is_static = c.u8()?;
            Block::SourceTexture { file }
        }
        "NiAlphaProperty" => {
            read_onet(c, v10)?;
            let flags = c.u16()?;
            let _threshold = c.u8()?;
            Block::AlphaProp { flags }
        }
        "NiVertexColorProperty" => {
            read_onet(c, v10)?;
            c.skip(2 + 4 + 4)?;
            Block::Other
        }
        "NiZBufferProperty" => {
            read_onet(c, v10)?;
            c.skip(2 + 4)?; // flags + depth function (4.1.0.12+)
            Block::Other
        }
        "NiDitherProperty" | "NiSpecularProperty" | "NiWireframeProperty" | "NiShadeProperty" => {
            read_onet(c, v10)?;
            c.skip(2)?;
            Block::Other
        }
        "NiFogProperty" => {
            read_onet(c, v10)?;
            c.skip(2 + 4 + 12)?; // flags + fog depth + fog colour
            Block::Other
        }
        "NiStencilProperty" => {
            read_onet(c, v10)?;
            if v10 {
                c.skip(1 + 7 * 4)?; // enabled byte + function/ref/mask/fail/zfail/pass/draw (no flags)
            } else {
                c.skip(2 + 1 + 7 * 4)?;
            }
            Block::Other
        }
        "NiStringExtraData" => {
            if v10 {
                // 10.x NiExtraData: a NAME string (lists replaced ref-chaining), then the value.
                // The value still carries the bone id in a `.kfa`; `next` is unused at this version.
                // `UserPropBufferY` on player heads is also load-bearing: it maps `FM<n>` to the
                // source facial-blend id, so retain the key instead of treating every extra as a
                // generic string.
                let name = c.string()?;
                let s = c.string_max(EXTRA_DATA_STRING_MAX)?;
                Block::StringExtra {
                    next: -1,
                    name: Some(name),
                    value: s,
                }
            } else {
                let next = c.i32()?;
                let _bytes = c.u32()?;
                let s = c.string_max(EXTRA_DATA_STRING_MAX)?;
                // A `.kfa` chains these off the root node: each holds a bone id (as a string), and the
                // chain runs parallel to the keyframe-controller chain (i-th id ↔ i-th controller).
                Block::StringExtra {
                    next,
                    name: None,
                    value: s,
                }
            }
        }
        // 10.x string-array extra data (name string, then a counted list of strings). Common on
        // fig3 player-body parts; we don't consume the values, just walk past them.
        "NiStringsExtraData" => {
            let _name = c.string()?;
            let n = c.u32()? as usize;
            if n > 65_536 {
                return Err(err("absurd strings-extra count"));
            }
            let mut values = Vec::with_capacity(n);
            for _ in 0..n {
                values.push(c.string()?);
            }
            Block::StringsExtra { values }
        }
        // 10.x typed extra-data family: name string + a fixed-shape value each.
        "NiIntegerExtraData" => {
            let _name = c.string()?;
            let _v = c.u32()?;
            Block::Other
        }
        "NiIntegersExtraData" => {
            let _name = c.string()?;
            let n = c.u32()? as usize;
            if n > 65_536 {
                return Err(err("absurd integers-extra count"));
            }
            c.skip(n * 4)?;
            Block::Other
        }
        "NiBooleanExtraData" => {
            let _name = c.string()?;
            let _v = c.u8()?;
            Block::Other
        }
        "NiFloatExtraData" => {
            let _name = c.string()?;
            let _v = c.f32()?;
            Block::Other
        }
        "NiVectorExtraData" => {
            let _name = c.string()?;
            c.skip(16)?; // vector4
            Block::Other
        }
        "NiBinaryExtraData" => {
            let _name = c.string()?;
            let n = c.u32()? as usize;
            if n > 16 * 1024 * 1024 {
                return Err(err("absurd binary-extra size"));
            }
            c.skip(n)?;
            Block::Other
        }
        // 10.x LOD level data (moved out of NiLODNode).
        "NiRangeLODData" => {
            let _center = c.vec3()?;
            let n = c.u32()? as usize;
            if n > 64 {
                return Err(err("absurd LOD range count"));
            }
            c.skip(n * 8)?; // near/far pairs
            Block::Other
        }
        "NiScreenLODData" => {
            c.skip(16 + 16)?; // bound (center+radius) + world bound (center+radius)
            let n = c.u32()? as usize;
            if n > 64 {
                return Err(err("absurd LOD proportion count"));
            }
            c.skip(n * 4)?;
            Block::Other
        }
        // Flip-book style texture animation: which map slot + operation, keys in a NiFloatData.
        "NiTextureTransformController" => {
            let (_next, target) = read_time_controller(c)?;
            let _shader_map = c.u8()?;
            let slot = c.u32()?;
            let operation = c.u32()?;
            let data = c.i32()?; // -> NiFloatData
            Block::TexTransform {
                target,
                slot,
                operation,
                data,
            }
        }
        "NiVisController" => {
            read_time_controller(c)?;
            let _data = c.i32()?;
            Block::Other
        }
        "NiVisData" => {
            let n = c.u32()? as usize;
            if n > 100_000 {
                return Err(err("absurd vis-key count"));
            }
            c.skip(n * 5)?; // time f32 + on/off byte
            Block::Other
        }
        "NiPathController" => {
            read_time_controller(c)?;
            if v10 {
                let _path_flags = c.u16()?; // new in 10.1
            }
            let _bank_dir = c.i32()?;
            let _max_bank_angle = c.f32()?;
            let _smoothing = c.f32()?;
            let _follow_axis = c.i16()?;
            let _path_data = c.i32()?;
            let _percent_data = c.i32()?;
            Block::Other
        }
        "NiKeyframeController" => {
            let (next, _target) = read_time_controller(c)?;
            let data = c.i32()?; // -> NiKeyframeData
            Block::KeyframeController { next, data }
        }
        // Particle-system controller: emitter parameters + a snapshot of live particle state
        // (40 bytes each). Layout hand-verified byte-exact against BAvTeleporter.NIF (emit rate
        // 60, lifetime 2.0, 120 particles / 116 valid, ps2-style unknowns).
        "NiParticleSystemController" | "NiBSPArrayController" => {
            let (next, _target) = read_time_controller(c)?;
            let speed = c.f32()?;
            let speed_random = c.f32()?;
            let declination = c.f32()?;
            let declination_variation = c.f32()?;
            let planar_angle = c.f32()?;
            let planar_angle_variation = c.f32()?;
            let _unk_n0 = c.f32()?;
            let _unk_n1 = c.f32()?;
            let _unk_n2 = c.f32()?;
            let color = [c.f32()?, c.f32()?, c.f32()?, c.f32()?];
            let size = c.f32()?;
            let emit_start = c.f32()?;
            let emit_stop = c.f32()?;
            let _unknown = c.u8()?;
            let emit_rate = c.f32()?;
            let lifetime = c.f32()?;
            let lifetime_random = c.f32()?;
            let _emit_flags = c.u16()?;
            let _start_random = c.vec3()?;
            let _emitter = c.i32()?;
            c.skip(2 + 4 + 4 + 4 + 2)?; // unknown short/float/int/int/short
            let num_particles = c.u16()? as usize;
            let num_valid = c.u16()? as usize;
            // This was `num_particles > 10_000`, which rejected Midgard's snowfall: 10,075 slots
            // with 150 valid. The data is self-evidently right — emit rate 75/s × 2.0 s lifetime is
            // exactly the 150 live particles the block reports — so the cap was a guess that had
            // only ever been tried on smaller emitters. A u16 cannot express an absurd count
            // anyway; the real bound is the buffer, which `skip` enforces below.
            //
            // The invariant that *does* catch a desync is the pair disagreeing.
            if num_valid > num_particles {
                return Err(err(format!(
                    "particle count invariant violated: {num_valid} valid of {num_particles} (desync)"
                )));
            }
            c.skip(num_particles * 40)?; // velocity, unknown v3, lifetime/lifespan/timestamp, unknowns, vertex id
            let modifier = c.i32()?; // first modifier (often GrowFade)
            let _extra = c.i32()?;
            let _unknown_ref2 = c.i32()?;
            let _trailer = c.u8()?;
            Block::ParticleController {
                next,
                speed,
                speed_random,
                declination,
                declination_variation,
                planar_angle,
                planar_angle_variation,
                color,
                size,
                emit_start,
                emit_stop,
                emit_rate,
                lifetime,
                lifetime_random,
                modifier,
            }
        }
        // Particle modifiers: NiParticleModifier prefix (next-modifier ref + controller ptr),
        // then per-type payload.
        "NiGravity" => {
            let next = c.i32()?;
            let _controller = c.i32()?;
            c.skip(4 + 4 + 4 + 12 + 12)?; // unknown float, force, type, position, direction
            Block::ParticleModifierStub { next }
        }
        "NiParticleGrowFade" => {
            let next = c.i32()?;
            let _controller = c.i32()?;
            let grow = c.f32()?;
            let fade = c.f32()?;
            Block::ParticleGrowFade { next, grow, fade }
        }
        "NiParticleMeshModifier" => {
            let next = c.i32()?;
            let _controller = c.i32()?;
            let n = c.u32()? as usize;
            if n > 4096 {
                return Err(err("absurd particle-mesh count (desync)"));
            }
            for _ in 0..n {
                let _mesh = c.i32()?;
            }
            Block::ParticleModifierStub { next }
        }
        "NiParticleColorModifier" => {
            let next = c.i32()?;
            let _controller = c.i32()?;
            let _color_data = c.i32()?;
            Block::ParticleModifierStub { next }
        }
        "NiParticleRotation" => {
            let next = c.i32()?;
            let _controller = c.i32()?;
            let _random_initial = c.u8()?;
            c.skip(12 + 4)?; // initial axis, rotation speed
            Block::ParticleModifierStub { next }
        }
        "NiParticleBomb" => {
            let next = c.i32()?;
            let _controller = c.i32()?;
            c.skip(4 + 4 + 4 + 4 + 4 + 4 + 12 + 12)?;
            Block::ParticleModifierStub { next }
        }
        "NiPlanarCollider" => {
            let next = c.i32()?;
            let _controller = c.i32()?;
            c.skip(4 + 4 + 4 + 12 + 12 + 12 + 4)?;
            Block::ParticleModifierStub { next }
        }
        // Controllers we skip past: NiTimeController prefix + one target/data ref each.
        "NiAlphaController" | "NiLookAtController" => {
            read_time_controller(c)?;
            let _data = c.i32()?;
            Block::Other
        }
        // Gamebryo's light-colour controller inserts a two-byte target-colour selector before its
        // interpolator/data ref. It appears in the authored Firbolg skeleton (`sfig001`) as part
        // of the Max lighting rig. Treating it like the sibling controllers consumes those two
        // bytes as the low half of the next ref and leaves `NiPosData` two bytes out of phase,
        // which made a perfectly valid race skeleton look unreadable and forced the renderer onto
        // the wrong fallback rig.
        "NiLightColorController" => {
            read_time_controller(c)?;
            let _target_color = c.u16()?;
            let _data = c.i32()?;
            Block::Other
        }
        // `NiMaterialColorController` carries a `target_color` u16 after the data ref that its two
        // siblings above do not. Sharing their arm left the walk exactly two bytes short.
        //
        // Measured in `gargoyle_air.NIF`: the following `NiPosData` then read its key count as
        // 65536 and its interpolation type as 131072 — both a two-byte shift of a sane `1` and `2`.
        // The block separator is zeros on either alignment, so nothing caught it at the boundary;
        // it surfaced as an impossible key type one block later.
        "NiMaterialColorController" => {
            read_time_controller(c)?;
            let _data = c.i32()?;
            if v10 {
                let _target_color = c.u16()?;
            }
            Block::Other
        }
        // `NiFlipController` — an animated texture that cycles a list of `NiSourceTexture` refs.
        // Skip-parsed: the cycling itself is animation we do not drive, but the block has to be
        // stepped over exactly or every block after it desyncs.
        //
        // Measured in `ghostKing01.nif` at 0x5208: the time-controller prefix ends with a plausible
        // stop time of 1.0667 and target 261, then texture slot 1, an unknown u32, a delta of
        // 0.0667 (a fifteenth of a second per frame), a count of 16, and 16 refs cycling
        // 265, 266, 267, 268 — four textures shown four times each.
        "NiFlipController" => {
            read_time_controller(c)?;
            let _texture_slot = c.u32()?;
            let _unknown = c.u32()?;
            let _delta = c.f32()?;
            let sources = c.u32()?;
            if sources > 4096 {
                return Err(err(format!(
                    "NiFlipController claims {sources} sources — refusing to skip on a desynced count"
                )));
            }
            c.skip(sources as usize * 4)?;
            Block::Other
        }
        "NiFloatData" => {
            // Same bytes `skip_key_group(c, 1)` consumed. `read_keys` mirrors it byte-for-byte,
            // so keeping them cannot desync where skipping did not.
            Block::FloatData(read_keys::<1>(c)?)
        }
        "NiPosData" => {
            skip_key_group(c, 3)?;
            Block::Other
        }
        // Scene lights: NiLight = AVObject + dimmer + 3 colours (4.2.x NiDynamicEffect carries
        // NO affected-node list — verified byte-exact against CMine_entrance_01.nif). Point/spot
        // add attenuation. Lighting comes from our own shader, so skip-parse only.
        "NiDirectionalLight" | "NiAmbientLight" | "NiPointLight" | "NiSpotLight" => {
            let _av = read_av(c, v10)?;
            if v10 {
                read_affected_nodes(c)?;
            }
            c.skip(4 + 12 + 12 + 12)?; // dimmer + ambient/diffuse/specular colours
            if name == "NiPointLight" || name == "NiSpotLight" {
                c.skip(3 * 4)?; // constant/linear/quadratic attenuation
            }
            if name == "NiSpotLight" {
                c.skip(2 * 4)?; // cutoff angle + exponent
            }
            Block::Other
        }
        // Environment-map/light-projection effect: AVObject straight into projection + texture
        // wiring (no affected-node list in 4.2.x, same as lights; 10.1 restores a counted list).
        "NiTextureEffect" => {
            let _av = read_av(c, v10)?;
            if v10 {
                read_affected_nodes(c)?;
            }
            let _proj_matrix = c.mat33()?;
            let _proj_translation = c.vec3()?;
            let _filter = c.u32()?;
            let _clamp = c.u32()?;
            let _kind = c.u32()?;
            let _coord_gen = c.u32()?;
            let _source = c.i32()?;
            let _clip_plane = c.u8()?;
            let _plane_normal = c.vec3()?;
            let _plane_const = c.f32()?;
            let _ps2_l = c.i16()?;
            let _ps2_k = c.i16()?;
            if version <= 0x0401_000c {
                let _unknown = c.u16()?;
            }
            Block::Other
        }
        "NiKeyframeData" => {
            let rotation = read_rot_channel(c)?;
            let translation = read_keys::<3>(c)?;
            let scale = read_keys::<1>(c)?;
            Block::KeyframeData(TrsTrack {
                rotation,
                translation,
                scale,
            })
        }
        "NiColorData" => {
            skip_key_group(c, 4)?;
            Block::Other
        }
        "NiUVData" => {
            for _ in 0..4 {
                skip_key_group(c, 1)?;
            }
            Block::Other
        }
        "NiUVController" => {
            read_time_controller(c)?;
            let _unknown = c.u16()?;
            let _data = c.i32()?;
            Block::Other
        }
        "NiGeomMorpherController" => {
            let (_next, target) = read_time_controller(c)?;
            if v10 {
                let _extra_flags = c.u16()?; // new in 10.x
            }
            let data = c.i32()?; // -> NiMorphData
            let _always = c.u8()?;
            Block::GeomMorpher { target, data }
        }
        "NiMorphData" => {
            let nmorphs = c.u32()? as usize;
            let nv = c.u32()? as usize;
            if nmorphs > 256 || nv > 65_536 {
                return Err(err("absurd morph sizes"));
            }
            let relative = c.bool()?;
            let mut targets = Vec::with_capacity(nmorphs);
            for _ in 0..nmorphs {
                // Same bytes `skip_key_group(c, 1)` consumed, kept instead of discarded. Reading
                // them cannot desync where skipping did not: `read_keys` mirrors it byte-for-byte.
                let keys = read_keys::<1>(c)?;
                let mut deltas = Vec::with_capacity(nv);
                for _ in 0..nv {
                    deltas.push(c.vec3()?);
                }
                targets.push(MorphTarget { keys, deltas });
            }
            Block::MorphData(MorphAnim { relative, targets })
        }
        // ── Skinned geometry (monster/creature meshes in `figures/`) ──────────────────────────
        // We render the BIND-POSE geometry (already carried by the NiTriShapeData the shape points
        // at); the skin blocks only add bone bindings + per-vertex weights we don't need yet. So
        // these handlers exist purely to walk the layout byte-exactly and keep the parse in sync —
        // without them the walker desyncs the instant it meets a rigged mesh (2.4% of figures/).
        "NiSkinInstance" => {
            let data = c.i32()?; // -> NiSkinData
                                 // (A NiSkinPartition ref sits here only in ≥10.2.0.0 — newer than anything DAoC ships.)
            let _skeleton_root = c.i32()?; // Ptr<NiNode>
            let num_bones = c.u32()? as usize;
            if num_bones > 4096 {
                return Err(err("absurd skin-instance bone count"));
            }
            let mut bones = Vec::with_capacity(num_bones);
            for _ in 0..num_bones {
                bones.push(c.i32()?); // Ptr<NiNode> — the bone's block index
            }
            Block::SkinInstance { data, bones }
        }
        "NiSkinData" => {
            // Leading overall skin transform (Matrix33 + Vector3 + float) — parsed for sync, dropped
            // (it's the shape's inverse-placement, not part of the vertex skinning chain).
            let _rot = c.mat33()?;
            let _trans = c.vec3()?;
            let _scale = c.f32()?;
            let num_bones = c.u32()? as usize;
            if num_bones > 4096 {
                return Err(err("absurd skin-data bone count"));
            }
            // Skin-partition ref: present from 4.0.0.2 THROUGH 10.1.0.0 (NifTools `NiSkinData`,
            // `ver1="4.0.0.2" ver2="10.1.0.0"`), not only at 10.1.
            //
            // Reading it only at exactly 10.1 desynced every NetImmerse 4.2.2.0 file by four
            // bytes. The reader then took garbage as a bone count or a vertex count and ran off
            // the end, reporting "NIF truncated [NiSkinData]" — 130 creature models, all of them
            // present and named, all failing here. The truncation was the symptom; this condition
            // was the cause.
            if (0x0400_0002..=GAMEBRYO_10_1).contains(&version) {
                let _skin_partition = c.i32()?;
            }
            // `has_vertex_weights` byte added in 4.2.1.0; before that the weights are always present.
            let has_weights = if version >= 0x0402_0100 {
                c.u8()? != 0
            } else {
                true
            };
            let mut bones = Vec::with_capacity(num_bones);
            for _ in 0..num_bones {
                // Per-bone inverse-bind SkinTransform, then a bounding sphere, then the weights.
                let rot = c.mat33()?;
                let trans = c.vec3()?;
                let scale = c.f32()?;
                c.skip(12 + 4)?; // bounding sphere: Vector3 centre + float radius (unused)
                let nv = c.u16()? as usize;
                let mut weights = Vec::new();
                if has_weights {
                    if nv > 65_536 {
                        return Err(err("absurd skin vertex count"));
                    }
                    weights.reserve(nv);
                    for _ in 0..nv {
                        let vi = c.u16()?;
                        let w = c.f32()?;
                        weights.push((vi, w));
                    }
                }
                bones.push(BoneSkin {
                    inverse_bind: (rot, scale, trans),
                    weights,
                });
            }
            Block::SkinData { bones }
        }
        // The pre-computed hardware-skinning partition (Gamebryo 10.1). It carries a second copy of
        // the triangles (grouped by bone set) plus vertex maps/weights — all redundant with the
        // NiTriShapeData we already render, so this only needs to be walked byte-exactly to stay in
        // sync. Layout per NifTools SkinPartition; the has-* bools exist from 10.1.0.0.
        "NiSkinPartition" => {
            let num_partitions = c.u32()? as usize;
            if num_partitions > 4096 {
                return Err(err("absurd skin-partition count"));
            }
            for _ in 0..num_partitions {
                let num_vertices = c.u16()? as usize;
                let num_triangles = c.u16()? as usize;
                let num_bones = c.u16()? as usize;
                let num_strips = c.u16()? as usize;
                let num_wpv = c.u16()? as usize; // weights per vertex
                c.skip(num_bones * 2)?; // bone map (u16 each)
                let has_map = if v10 { c.u8()? != 0 } else { true };
                if has_map {
                    c.skip(num_vertices * 2)?; // vertex map (u16 each)
                }
                let has_weights = if v10 { c.u8()? != 0 } else { true };
                if has_weights {
                    c.skip(num_vertices * num_wpv * 4)?; // f32 weights
                }
                let mut strip_points = 0usize;
                for _ in 0..num_strips {
                    strip_points += c.u16()? as usize; // strip lengths
                }
                let has_faces = if v10 { c.u8()? != 0 } else { true };
                if has_faces {
                    if num_strips != 0 {
                        c.skip(strip_points * 2)?; // strip index points (u16)
                    } else {
                        c.skip(num_triangles * 6)?; // Triangle = 3× u16
                    }
                }
                // `Has Bone Indices` carries NO version condition in NifTools' `SkinPartition` —
                // unlike the has-map / has-weights / has-faces bools above it, which are 10.1+.
                // Gating it on 10.1 too swallowed one byte on every 4.x file.
                let has_bone_indices = c.u8()? != 0;
                if has_bone_indices {
                    c.skip(num_vertices * num_wpv)?; // u8 bone index per weight
                }
            }
            Block::Other
        }
        // Colour extra-data (NiExtraData family): base header then a Color4 payload. Follows the
        // same 10.x-name-string vs 4.x-next/bytes split as NiStringExtraData above.
        "NiColorExtraData" => {
            if v10 {
                let _name = c.string()?;
            } else {
                let _next = c.i32()?;
                let _bytes = c.u32()?;
            }
            c.skip(16)?; // Color4 rgba
            Block::Other
        }
        // Animation text keys (`NiTextKeyExtraData`): `(time, string)` pairs marking events on a
        // clip — footfalls, hit frames. Nothing here consumes them, but they sit inline in the
        // block stream, so they must be walked byte-exactly or every later block desyncs.
        "NiTextKeyExtraData" => {
            // The `unknown_int` before the key count exists only BELOW 10.0.1.0. On the 10.1 path
            // it is not there, and reading it consumed the real count — leaving the first key's
            // time (`0.0f`) to be read as the count, so zero keys were parsed and the walk stopped
            // on the first key's string length.
            //
            // Measured in `cata_spider.nif`: after the name `NiTextKeyED001` the u32 is 14, then
            // `0.0f`, then a 31-byte "begin spider_idle\r\npriority 4\r\n" — a count followed by
            // `(time, string)` pairs, with nothing in between.
            //
            // Exactly three v10.1 files in `figures/` carry this block, and they were exactly the
            // three that failed: this branch had never parsed one successfully.
            if v10 {
                let _name = c.string()?;
            } else {
                let _next = c.i32()?;
                let _bytes = c.u32()?;
                let _unknown = c.u32()?;
            }
            let num_keys = c.u32()? as usize;
            if num_keys > 100_000 {
                return Err(err("absurd text-key count"));
            }
            for _ in 0..num_keys {
                let _time = c.f32()?;
                let _value = c.string_max(EXTRA_DATA_STRING_MAX)?;
            }
            Block::Other
        }
        // `NiPixelData` — an embedded texture bitmap. We do not want the pixels; we need to step
        // over the block without desyncing everything after it.
        //
        // **Measured off the files, not taken from a schema.** Two earlier attempts at the
        // NifTools layout both desynced, so this one was derived from the eight NIFs that carry
        // the block (all version 10.1.0.0) and is checked by arithmetic the file itself supplies:
        //
        //   +40 num_mipmaps, +44 bytes_per_pixel, +48 mipmaps as (width, height, offset) triples,
        //   then a u32 total byte count, then that many bytes.
        //
        // `CSRgreen.NIF` is the clearest witness: 7 mipmaps of a 64x64 24-bit image, and each
        // triple's offset is the running sum of `width * height * bytes_per_pixel`
        // (12288, 15360, 16128, …) ending at a total of 16383 — which is exactly the count stored
        // after the table. `winterlord_b.nif` is the same shape with 11 mipmaps of a DXT1 1024x1024
        // (`bytes_per_pixel` 0, offsets stepping by half a byte per texel), and `CSRblue.NIF` is a
        // single 64x64 8-bit mip totalling 4096. Three different pixel formats, one layout.
        //
        // The mipmap count is bounded before it is used as a length: a desync upstream would
        // otherwise turn a garbage u32 into a multi-gigabyte skip.
        "NiPixelData" => {
            // No `NiObjectNET` prefix: this derives from NiObject, so the first field is the
            // format. `CSRgreen.NIF` reads 0 / ff / ff00 / ff0000 / 0 / 24 straight off the block
            // start — an RGB888 descriptor with no name in front of it.
            let _pixel_format = c.u32()?;
            let (_r, _g, _b, _a) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
            let _bits_per_pixel = c.u32()?;
            c.skip(12)?; // three fields that are constant across every file here
            let _palette_ref = c.i32()?;
            let mipmaps = c.u32()?;
            let _bytes_per_pixel = c.u32()?;
            if mipmaps > 32 {
                return Err(err(format!(
                    "NiPixelData claims {mipmaps} mipmaps — refusing to skip on a desynced count"
                )));
            }
            c.skip(mipmaps as usize * 12)?;
            let bytes = c.u32()? as usize;
            c.skip(bytes)?;
            Block::Other
        }
        // `NiPalette` — the colour table an 8-bit `NiPixelData` indexes into. Same treatment: step
        // over it rather than decode it.
        //
        // Measured, like the block above. In `CSRblue.NIF` the palette sits one zero separator
        // after the pixel data and reads as a single byte, then `num_entries` = 256, then 256
        // four-byte RGBA entries — ending exactly on the next separator. The entry count is
        // bounded because a palette cannot exceed 256 entries and a larger one means the walk has
        // already desynced.
        "NiPalette" => {
            c.skip(1)?;
            let entries = c.u32()?;
            if entries > 256 {
                return Err(err(format!(
                    "NiPalette claims {entries} entries — refusing to skip on a desynced count"
                )));
            }
            c.skip(entries as usize * 4)?;
            Block::Other
        }
        other => Err(err(format!("unhandled block type {other}")))?,
    })
}

/// Walk every block of a NIF into a `Vec<Block>` (the desync tripwires — per-block name validation
/// in 4.x, the zero separators + EOF-exact footer in 10.1 — are enforced here). Shared by the static
/// flattener [`read_model`] and the rigged extractor [`read_rigged`].
fn parse_blocks(bytes: &[u8]) -> io::Result<Vec<Block>> {
    let header = read_header(bytes)?;
    let v10 = !header.block_types.is_empty();
    let mut c = Cur {
        b: bytes,
        p: header.blocks_start,
    };
    let mut blocks = Vec::with_capacity(header.num_blocks as usize);
    for i in 0..header.num_blocks as usize {
        // 4.x: every block leads with its own validated type string (the desync tripwire).
        // 10.1: types come from the header table; each block is instead preceded by a u32 zero
        // separator, and the FOOTER landing exactly at EOF below is the desync tripwire.
        let name = if v10 {
            let at = c.p;
            let sep = c.u32()?;
            if sep != 0 {
                // The offset is worth printing: the value is usually a string length, which says
                // the previous block stopped short rather than that this one is malformed, and
                // without a position there is nowhere to look.
                return Err(err(format!(
                    "nonzero block separator 0x{sep:08x} at 0x{at:x} before block #{i} \
                     (desync; previous block was {})",
                    blocks
                        .len()
                        .checked_sub(1)
                        .and_then(|j| header.block_types.get(j))
                        .map_or("<none>", |t| t.as_str())
                )));
            }
            header.block_types[i].clone()
        } else {
            let name = c.string()?;
            if name.len() < 4
                || name.len() > 40
                || !(name.starts_with("Ni")
                    || name.starts_with("Root")
                    || name.starts_with("Avoid"))
                || !name.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err(err(format!("bad block name {name:?} (desync)")));
            }
            name
        };
        let at = c.p;
        blocks.push(
            read_block(&mut c, &name, header.version)
                .map_err(|e| err(format!("{e} [block #{} {name} at 0x{at:x}]", blocks.len())))?,
        );
    }
    if v10 {
        // Footer: root-node ref list, then nothing. Any leftover bytes mean some block layout
        // above silently mis-parsed — fail the model rather than render garbage.
        let nroots = c.u32()? as usize;
        if nroots > 4096 {
            return Err(err("absurd root count in footer (desync)"));
        }
        c.skip(nroots * 4)?;
        if c.p != bytes.len() {
            return Err(err(format!(
                "{} bytes left after footer (desync)",
                bytes.len() - c.p
            )));
        }
    }
    Ok(blocks)
}

/// Parse a whole model out of NIF bytes: walk every block, then flatten the scene graph.
pub fn read_model(bytes: &[u8]) -> io::Result<Model> {
    let blocks = parse_blocks(bytes)?;
    let mut model = Model { parts: Vec::new() };
    let mut seen = std::collections::HashSet::new();
    flatten(
        &blocks,
        0,
        glam_identity(),
        &mut model,
        &mut seen,
        false,
        false,
        &MatCtx::default(),
        false,
    );
    if model.parts.is_empty() {
        // Nothing visible under the normal name rules. Some models (dungeon/cave patches like
        // abovewater_cave_p00) put ALL their geometry under `coll` — the walkable walls are
        // the mesh. Re-flatten with the visible-override on: collision geometry beats nothing.
        seen.clear();
        flatten(
            &blocks,
            0,
            glam_identity(),
            &mut model,
            &mut seen,
            false,
            true,
            &MatCtx::default(),
            false,
        );
    }
    if model.parts.is_empty() {
        return Err(err("no visible geometry"));
    }
    Ok(model)
}

/// A named model-space point recovered from the NIF scene graph (node translation and/or mesh
/// centroid). Used by M15 to get `m_slot` when door markers have no surviving MeshPart geometry.
#[derive(Clone, Debug)]
pub struct DoorAnchor {
    pub name: String,
    /// World/model-space translation of the node (or mesh centroid).
    pub translation: [f32; 3],
    /// `node` = NiNode/LOD transform only; `mesh` = MeshPart centroid.
    pub kind: &'static str,
}

/// Collect every door-named NiNode (including empty markers) and every door-named MeshPart
/// centroid. Door openings are often authored as transform markers (`door001`, `door-left`) with
/// no triangles — [`read_model`] drops those; this walk keeps their composed translations.
pub fn read_door_anchors(bytes: &[u8]) -> io::Result<Vec<DoorAnchor>> {
    let blocks = parse_blocks(bytes)?;
    let mut out = Vec::new();
    collect_door_anchors(&blocks, 0, glam_identity(), &mut out, false);
    // Also mesh centroids from the normal flatten (textured door leaves on frontier keeps, etc.).
    if let Ok(model) = read_model(bytes) {
        for p in &model.parts {
            if !name_looks_like_door(&p.name) {
                continue;
            }
            let (cx, cy, cz) = mesh_centroid(&p.positions);
            out.push(DoorAnchor {
                name: p.name.clone(),
                translation: [cx, cy, cz],
                kind: "mesh",
            });
        }
    }
    Ok(out)
}

fn name_looks_like_door(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("door")
}

fn mesh_centroid(positions: &[[f32; 3]]) -> (f32, f32, f32) {
    let n = positions.len();
    if n == 0 {
        return (0.0, 0.0, 0.0);
    }
    let mut sx = 0.0f64;
    let mut sy = 0.0f64;
    let mut sz = 0.0f64;
    for v in positions {
        sx += f64::from(v[0]);
        sy += f64::from(v[1]);
        sz += f64::from(v[2]);
    }
    let inv = 1.0 / n as f64;
    ((sx * inv) as f32, (sy * inv) as f32, (sz * inv) as f32)
}

fn collect_door_anchors(
    blocks: &[Block],
    idx: i32,
    xf: Xform,
    out: &mut Vec<DoorAnchor>,
    in_visible: bool,
) {
    let Some(block) = usize::try_from(idx).ok().and_then(|i| blocks.get(i)) else {
        return;
    };
    match block {
        Block::Node {
            av,
            children,
            billboard: _,
        }
        | Block::Lod { av, children } => {
            let n = av.name.to_ascii_lowercase();
            let in_visible = in_visible || n == "visible";
            if !in_visible && is_hidden(&av.name) {
                return;
            }
            let xf = compose(&xf, av);
            if name_looks_like_door(&av.name) {
                // Skip near-origin group folders with no useful bearing (e.g. a parent "doors").
                let t = xf.2;
                let r2 = t[0] * t[0] + t[1] * t[1];
                if r2 > 1.0 {
                    out.push(DoorAnchor {
                        name: av.name.clone(),
                        translation: t,
                        kind: "node",
                    });
                }
            }
            let kids = if matches!(block, Block::Lod { .. }) {
                children.first().copied().into_iter().collect::<Vec<_>>()
            } else {
                children.clone()
            };
            for ch in kids {
                collect_door_anchors(blocks, ch, xf, out, in_visible);
            }
        }
        _ => {}
    }
}

/// Dedup key for one placed shape: its geometry block plus a quantised transform. NIF scene graphs
/// are DAGs, so a shared subtree gets walked once per parent path — emitting the same geometry at
/// the same transform twice, which renders as coincident faces that z-fight (the flickering
/// same-colour shapes on mile gates). Skipping repeats of this key removes the dup; a legitimately
/// *instanced* shape (same geometry, different transform) has a different key and still renders.
fn shape_key(data_idx: i32, xf: &Xform) -> (i32, u64) {
    let mut h = 1469598103934665603u64; // FNV-1a
    let mut mix = |f: f32| {
        h ^= (f * 8.0).round() as i64 as u64; // ~1/8-unit quantisation absorbs float noise
        h = h.wrapping_mul(1099511628211);
    };
    for &m in &xf.0 {
        mix(m);
    }
    mix(xf.1);
    for &t in &xf.2 {
        mix(t);
    }
    (data_idx, h)
}

// Tiny 4x3 affine transform (rotation*scale | translation) to avoid a math-crate dependency.
/// Local / world bone transform: row-major 3×3 rotation, uniform scale, translation.
pub type Xform = ([f32; 9], f32, [f32; 3]);

fn glam_identity() -> Xform {
    ([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], 1.0, [0.0; 3])
}

fn mat_mul(a: &[f32; 9], b: &[f32; 9]) -> [f32; 9] {
    let mut m = [0.0; 9];
    for r in 0..3 {
        for cc in 0..3 {
            m[r * 3 + cc] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + cc]).sum();
        }
    }
    m
}

fn mat_apply(m: &[f32; 9], s: f32, v: [f32; 3]) -> [f32; 3] {
    [
        s * (m[0] * v[0] + m[1] * v[1] + m[2] * v[2]),
        s * (m[3] * v[0] + m[4] * v[1] + m[5] * v[2]),
        s * (m[6] * v[0] + m[7] * v[1] + m[8] * v[2]),
    ]
}

fn compose(parent: &Xform, av: &AvObject) -> Xform {
    // `CAER_NIF_TRACE_NAN` names the first node whose own transform is not finite. Composition is
    // pure multiply/add, so a NaN downstream always originates in some node's parsed fields — but
    // by the time it reaches a vertex there is nothing left to say where. Ledger D7 sat as "we
    // produce NaN somewhere" for exactly this reason.
    if std::env::var_os("CAER_NIF_TRACE_NAN").is_some()
        && !(av.rotation.iter().all(|v| v.is_finite())
            && av.translation.iter().all(|v| v.is_finite())
            && av.scale.is_finite())
    {
        eprintln!(
            "nif: non-finite transform on node {:?} — scale {} translation {:?} rotation {:?}",
            av.name, av.scale, av.translation, av.rotation
        );
    }
    let rot = mat_mul(&parent.0, &av.rotation);
    let scale = parent.1 * av.scale;
    let t = mat_apply(&parent.0, parent.1, av.translation);
    (
        rot,
        scale,
        [parent.2[0] + t[0], parent.2[1] + t[1], parent.2[2] + t[2]],
    )
}

/// Skip collision/shadow subtrees by name. Careful: `collisionswitch` NODES CONTAIN THE
/// VISUALS (their children are [collision, visual, …]) and must pass. `collidee` NODES also
/// traverse — some models (aegcliffpiece7, abovewater_cave_p00) author their real meshes
/// UNDER a node named `collidee` with the collision shapes in a sibling — so the per-shape
/// filter below is what actually drops collision geometry.
fn is_hidden(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "coll"
        || n.starts_with("coll ")
        || n.starts_with("coll_")
        || n.starts_with("shadow")
        || n.contains("!lod_cullme")
}

/// Per-shape filter: any `coll*` name is collision geometry (`coll`, `collidee`, `coll:234`,
/// `coll tower2`, …), regardless of which node it sits under.
fn shape_hidden(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("coll") || n.starts_with("shadow") || n.contains("!lod_cullme")
}

/// Material context inherited down the scene graph: NetImmerse properties attach at ANY
/// level (multi-material 3ds-max exports hang NiTexturingProperty on group nodes, not the
/// shapes), and children inherit unless they override.
#[derive(Clone)]
struct MatCtx {
    diffuse: Option<[f32; 3]>,
    texture: Option<String>,
    texture2: Option<String>,
    texture2_mode: TextureLayerMode,
    alpha: AlphaMode,
    /// Block index of the `NiTexturingProperty` in force, or `-1`.
    ///
    /// `-1` and not the derived `0`: block 0 is a real index — the root node in every file we
    /// read — so a default of 0 would let a controller aimed at block 0 bind to every untextured
    /// shape in the scene.
    ///
    /// Kept because a `NiTextureTransformController` names the PROPERTY it drives, not the shape
    /// wearing it — binding by shape index finds nothing at all, which is how the first version of
    /// this reported zero animated UVs in scenes that visibly have them.
    texturing_block: i32,
}

impl Default for MatCtx {
    fn default() -> Self {
        Self {
            diffuse: None,
            texture: None,
            texture2: None,
            texture2_mode: TextureLayerMode::VertexAlpha,
            alpha: AlphaMode::default(),
            texturing_block: -1,
        }
    }
}

/// Fold a property-ref list into an inherited material context (child overrides parent).
fn apply_props(blocks: &[Block], props: &[i32], mut ctx: MatCtx) -> MatCtx {
    for &p in props {
        match usize::try_from(p).ok().and_then(|i| blocks.get(i)) {
            Some(Block::Material { diffuse }) => ctx.diffuse = Some(*diffuse),
            Some(Block::Texturing {
                base_source,
                blend_source,
                slots,
                ..
            }) => {
                ctx.texturing_block = p;
                let name_of = |r: i32| match usize::try_from(r).ok().and_then(|i| blocks.get(i)) {
                    Some(Block::SourceTexture { file: Some(f) }) => Some(f.clone()),
                    _ => None,
                };
                if let Some(f) = name_of(*base_source) {
                    ctx.texture = Some(f);
                }
                // Only a genuinely different sheet is a second layer. Several parts name the same
                // file in both shader slots, and binding a texture to blend against itself costs
                // an upload and a sampler to produce the picture we already had.
                let shader_layer =
                    name_of(*blend_source).filter(|f| ctx.texture.as_deref() != Some(f.as_str()));
                // Fixed map slot 6 is NetImmerse's Decal 0. The Hibernia character-screen dome
                // carries `hib_clouds` in base and `hib_panarama01` here. It is an RGBA overlay,
                // not a ground-like vertex-mask blend. Keep shader texture 1 authoritative when
                // present; that is the already-proven terrain path and this renderer has exactly
                // two texture bindings per mesh range.
                let decal_layer = slots
                    .iter()
                    .find_map(|&(slot, source)| (slot == 6).then_some(source))
                    .and_then(name_of)
                    .filter(|f| ctx.texture.as_deref() != Some(f.as_str()));
                match (shader_layer, decal_layer) {
                    (Some(layer), _) => {
                        ctx.texture2 = Some(layer);
                        ctx.texture2_mode = TextureLayerMode::VertexAlpha;
                    }
                    (None, Some(layer)) => {
                        ctx.texture2 = Some(layer);
                        ctx.texture2_mode = TextureLayerMode::OverlayAlpha;
                    }
                    (None, None) => {
                        ctx.texture2 = None;
                        ctx.texture2_mode = TextureLayerMode::VertexAlpha;
                    }
                }
            }
            Some(Block::AlphaProp { flags }) => ctx.alpha = AlphaMode::from_flags(*flags),
            _ => {}
        }
    }
    ctx
}

fn flatten(
    blocks: &[Block],
    idx: i32,
    xf: Xform,
    model: &mut Model,
    seen: &mut std::collections::HashSet<(i32, u64)>,
    in_collidee: bool,
    in_visible: bool,
    ctx: &MatCtx,
    in_billboard: bool,
) {
    let Some(block) = usize::try_from(idx).ok().and_then(|i| blocks.get(i)) else {
        return;
    };
    match block {
        Block::Node {
            av,
            children,
            billboard,
        } => {
            if std::env::var_os("CAER_NIF_DEBUG").is_some() {
                log::debug!(
                    "nif-debug: node {:?} children={} hidden={}",
                    av.name,
                    children.len(),
                    is_hidden(&av.name)
                );
            }
            let n = av.name.to_ascii_lowercase();
            // `collidee` nodes traverse, but flagged: most models keep collision hulls there
            // (dropped below unless textured), while a node explicitly named `visible` marks a
            // subtree that renders even when its nodes/shapes carry coll names (aegcliffpiece7
            // and friends author visuals as `visible` → `coll` → `coll:233`, with the untextured
            // hulls under `collidee` → `Object02` — names fully inverted from the convention).
            let in_collidee = in_collidee || n.starts_with("collidee");
            let in_visible = in_visible || n == "visible";
            if !in_visible && is_hidden(&av.name) {
                return;
            }
            let in_billboard = in_billboard || *billboard;
            let ctx = apply_props(blocks, &av.properties, ctx.clone());
            let xf = compose(&xf, av);
            for &ch in children {
                flatten(
                    blocks,
                    ch,
                    xf,
                    model,
                    seen,
                    in_collidee,
                    in_visible,
                    &ctx,
                    in_billboard,
                );
            }
        }
        Block::Lod { av, children } => {
            if is_hidden(&av.name) {
                return;
            }
            let ctx = apply_props(blocks, &av.properties, ctx.clone());
            let xf = compose(&xf, av);
            // nearest (highest-detail) LOD child only
            if let Some(&first) = children.first() {
                flatten(
                    blocks,
                    first,
                    xf,
                    model,
                    seen,
                    in_collidee,
                    in_visible,
                    &ctx,
                    in_billboard,
                );
            }
        }
        Block::Shape { av, data, .. } => {
            if !in_visible && shape_hidden(&av.name) {
                return;
            }
            let xf = compose(&xf, av);
            let Some(Block::ShapeData(geo)) =
                usize::try_from(*data).ok().and_then(|i| blocks.get(i))
            else {
                return;
            };
            if geo.positions.is_empty() || geo.triangles.is_empty() {
                return;
            }
            // material/texture: the shape's own properties override whatever it inherited
            let m = apply_props(blocks, &av.properties, ctx.clone());
            if std::env::var_os("CAER_NIF_DEBUG").is_some() {
                log::debug!(
                    "nif-debug: shape {:?} tris={} texture={:?} in_collidee={in_collidee}",
                    av.name,
                    geo.triangles.len(),
                    m.texture
                );
            }
            // Inside a collidee subtree only TEXTURED shapes are visuals — untextured ones are
            // the collision hulls that node conventionally holds.
            if in_collidee && m.texture.is_none() {
                return;
            }
            if !seen.insert(shape_key(*data, &xf)) {
                return; // this exact geometry was already placed here — skip the coincident dup
            }
            let positions = geo.positions.iter().map(|&v| {
                let w = mat_apply(&xf.0, xf.1, v);
                [w[0] + xf.2[0], w[1] + xf.2[1], w[2] + xf.2[2]]
            });
            let normals: Vec<[f32; 3]> = if geo.normals.len() == geo.positions.len() {
                geo.normals
                    .iter()
                    .map(|&n| mat_apply(&xf.0, 1.0, n))
                    .collect()
            } else {
                vec![[0.0, 0.0, 1.0]; geo.positions.len()]
            };
            let nv = geo.positions.len() as u32;
            let indices = geo
                .triangles
                .iter()
                .flat_map(|t| t.iter().map(|&i| u32::from(i)))
                .filter(|&i| i < nv)
                .collect();
            model.parts.push(MeshPart {
                name: av.name.clone(),
                positions: positions.collect(),
                normals,
                uvs: if geo.uvs.len() == geo.positions.len() {
                    geo.uvs.clone()
                } else {
                    Vec::new()
                },
                indices,
                diffuse: m.diffuse.unwrap_or([0.7, 0.7, 0.7]),
                colors: if geo.colors.len() == geo.positions.len() {
                    geo.colors.clone()
                } else {
                    Vec::new()
                },
                texture: m.texture,
                texture2: m.texture2,
                texture2_mode: m.texture2_mode,
                alpha: m.alpha,
                billboard: in_billboard,
                uv_anim: uv_anim_for(blocks, m.texturing_block),
                morph: morph_for(blocks, idx, av.controller, &xf),
                morph_target_ids: face_morph_tags_for(blocks, av),
            });
        }
        _ => {}
    }
}

// ── Rigged extraction (Phase A.3.1): skeleton + per-vertex skin bindings ───────────────────────

/// Compose two transforms: the result applies `b` then `a` (`a ∘ b`), matching [`compose`]'s
/// parent∘child convention. Used to accumulate a bone's world-bind transform down the node tree.
fn xform_mul(a: &Xform, b: &Xform) -> Xform {
    let rot = mat_mul(&a.0, &b.0);
    let scale = a.1 * b.1;
    let t = mat_apply(&a.0, a.1, b.2);
    (rot, scale, [a.2[0] + t[0], a.2[1] + t[1], a.2[2] + t[2]])
}

/// Encode an [`Xform`] as a column-major `mat4x4` for WGSL (same layout as skinning palette entries).
#[must_use]
pub fn xform_to_mat4(x: &Xform) -> [[f32; 4]; 4] {
    let (m, s, tr) = x;
    [
        [s * m[0], s * m[3], s * m[6], 0.0],
        [s * m[1], s * m[4], s * m[7], 0.0],
        [s * m[2], s * m[5], s * m[8], 0.0],
        [tr[0], tr[1], tr[2], 1.0],
    ]
}

/// Apply a transform to a point: `rot·(scale·v) + translation`.
fn xform_apply(x: &Xform, v: [f32; 3]) -> [f32; 3] {
    let r = mat_apply(&x.0, x.1, v);
    [r[0] + x.2[0], r[1] + x.2[1], r[2] + x.2[2]]
}

/// Canonical DAoC Biped **bone-id → bone-name** table (Phase A.3.2b). A `.kfa` clip keys its tracks
/// by these numeric ids; mesh skeletons carry only Biped **names**, so this is the bridge that lets a
/// clip retarget onto any humanoid mesh. Bind path: mesh bone → its canonical id (by name, via
/// [`canonical_biped_id`]) → `clip.tracks[id]`.
///
/// **Derivation (empirical, not guessed):** a bone's bind *local translation* is constant across
/// clips and distinctive per bone, so each clip track's `t=0` translation was matched to the nearest
/// bone of the `Skel01` reference biped. Clips authored for `Skel01` fit at distance ~0, voting the
/// mapping with 100% agreement (`bonevote` bin). The blocks are anatomically contiguous — spine
/// chain, then the two arms, then the two legs — and every mapped id is a high-frequency "animated by
/// nearly every clip" bone (frequency cross-check), while the gaps (5, 7–10, finger sub-joints) are
/// the low-frequency face/finger detail bones absent from `Skel01`. Sides come from the real-margin
/// clavicle/thigh matches (id 22 = L Clavicle, 35 = R Clavicle, 48 = L Thigh, 54 = R Thigh — DAoC
/// numbers the left side first). Face/hair/finger-tip ids we can't pin are deliberately omitted:
/// an unmapped bone simply holds its bind pose, which is correct and harmless.
const CANONICAL_BIPED: &[(i32, &str)] = &[
    (0, "Bip01"),
    (1, "Bip01 Pelvis"),
    (2, "Bip01 Spine"),
    (3, "Bip01 Spine1"),
    (4, "Bip01 Spine2"),
    (5, "Bip01 Spine3"),
    (6, "Bip01 Neck"),
    (11, "Bip01 Head"),
    // Left arm (contiguous block 22–34): clavicle, upper/fore/hand, then 3 fingers × 3 joints.
    (22, "Bip01 L Clavicle"),
    (23, "Bip01 L UpperArm"),
    (24, "Bip01 L Forearm"),
    (25, "Bip01 L Hand"),
    (26, "Bip01 L Finger0"),
    (27, "Bip01 L Finger01"),
    (28, "Bip01 L Finger02"),
    (29, "Bip01 L Finger1"),
    (30, "Bip01 L Finger11"),
    (31, "Bip01 L Finger12"),
    (32, "Bip01 L Finger2"),
    (33, "Bip01 L Finger21"),
    (34, "Bip01 L Finger22"),
    // Right arm (contiguous block 35–47).
    (35, "Bip01 R Clavicle"),
    (36, "Bip01 R UpperArm"),
    (37, "Bip01 R Forearm"),
    (38, "Bip01 R Hand"),
    (39, "Bip01 R Finger0"),
    (40, "Bip01 R Finger01"),
    (41, "Bip01 R Finger02"),
    (42, "Bip01 R Finger1"),
    (43, "Bip01 R Finger11"),
    (44, "Bip01 R Finger12"),
    (45, "Bip01 R Finger2"),
    (46, "Bip01 R Finger21"),
    (47, "Bip01 R Finger22"),
    // Left leg (block 48–52): thigh, calf, foot, toe (id 50 = a mid bone Skel01 lacks, omitted).
    (48, "Bip01 L Thigh"),
    (49, "Bip01 L Calf"),
    (51, "Bip01 L Foot"),
    (52, "Bip01 L Toe0"),
    // Right leg (block 54–58).
    (54, "Bip01 R Thigh"),
    (55, "Bip01 R Calf"),
    (57, "Bip01 R Foot"),
    (58, "Bip01 R Toe0"),
];

/// The canonical Biped id for a bone name, or `None` if the name isn't a mapped biped bone (face,
/// hair, cloth, prop, or a creature-specific bone — it will simply hold its bind pose under an anim).
pub fn canonical_biped_id(name: &str) -> Option<i32> {
    CANONICAL_BIPED
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(id, _)| *id)
}

/// Follow a node's extra-data chain and return the first `NiStringExtraData` whose value parses as a
/// number — the node's DAoC bone id, which a `.kfa` retargets onto. `None` if the chain has none.
/// The vertex animation bound to the shape at block `shape_idx`, in flattened space.
///
/// Two ways in, because neither alone is enough. A `NiTimeController` names the object it drives,
/// so the authoritative link is `target == shape_idx`; but the controller chain hanging off the
/// shape is the link the format documents, and a morpher sitting behind a controller type we parse
/// into `Other` would be invisible to a pure chain walk. Target first, chain as the fallback.
///
/// A relative target's values are offsets in the shape's local space, so they take the transform's
/// rotation and scale but NOT its translation — adding the translation would move the whole card
/// instead of bending it. An absolute target's values ARE positions and take the full transform,
/// exactly like the rest vertices beside them.
/// The raw `NiMorphData` bound to one shape.  Static player-head blend targets and timed scene
/// animation use the same controller type; consumers decide whether to sample it as a clock or as
/// an authored appearance value.
fn raw_morph_for(blocks: &[Block], shape_idx: i32, av_controller: i32) -> Option<&MorphAnim> {
    let data_ref = blocks
        .iter()
        .find_map(|b| match b {
            Block::GeomMorpher { target, data, .. } if *target == shape_idx => Some(*data),
            _ => None,
        })
        .or_else(|| {
            let mut cur = av_controller;
            loop {
                match usize::try_from(cur).ok().and_then(|i| blocks.get(i)) {
                    Some(Block::GeomMorpher { data, .. }) => return Some(*data),
                    Some(Block::KeyframeController { next, .. }) => cur = *next,
                    _ => return None,
                }
            }
        })?;
    let Some(Block::MorphData(anim)) = usize::try_from(data_ref).ok().and_then(|i| blocks.get(i))
    else {
        return None;
    };
    (anim.is_animated() || anim.has_static_targets()).then_some(anim)
}

/// Vertex morph data for a flattened static [`MeshPart`].
///
/// Deltas arrive in local shape coordinates.  Flattened geometry is already in the shape's world
/// coordinates, so rotate/scale relative offsets but do not translate them; absolute target
/// positions take the full transform.  The controller may be a timed scene morph or a static
/// player-head blend set — the caller distinguishes those via [`MorphAnim::is_animated`].
fn morph_for(
    blocks: &[Block],
    shape_idx: i32,
    av_controller: i32,
    xf: &Xform,
) -> Option<MorphAnim> {
    let anim = raw_morph_for(blocks, shape_idx, av_controller)?;
    let mut out = anim.clone();
    let relative = out.relative;
    for t in &mut out.targets {
        for d in &mut t.deltas {
            let w = mat_apply(&xf.0, xf.1, *d);
            *d = if relative {
                w
            } else {
                [w[0] + xf.2[0], w[1] + xf.2[1], w[2] + xf.2[2]]
            };
        }
    }
    Some(out)
}

/// Parse one artist-authored `FM<n>=<id>` entry from a Gamebryo user-property buffer.
///
/// The NIF target array has no names of its own.  Fig3 heads attach a `UserPropBufferY` string to
/// the shape instead, e.g. `FM1=3` / `FM2=4`; the left-hand number is the one-based target index.
fn face_morph_tag(line: &str) -> Option<(usize, u8)> {
    let line = line.trim();
    let body = line.strip_prefix("FM")?;
    let (target, id) = body.split_once('=')?;
    let target = target.trim().parse::<usize>().ok()?;
    let id = id.trim().parse::<u8>().ok()?;
    (target > 0).then_some((target, id))
}

/// Merge one `UserPropBuffer` extra-data payload into a target-indexed tag vector.
///
/// Gamebryo stores the buffer key in `name`; a handful of legacy exports put `UserPropBufferY` on
/// the first line of `value`. Keeping this in one helper makes the direct-shape and parent-node
/// routes below obey the same acceptance policy. A generic user-property buffer with no `FM` lines
/// cannot add a target, so it is not misclassified as facial data.
fn append_face_morph_tags(out: &mut Vec<Option<u8>>, name: Option<&str>, value: &str) {
    let keyed_by_name = name.is_some_and(|name| name.eq_ignore_ascii_case("UserPropBuffer"));
    let keyed_by_value = value
        .lines()
        .next()
        .is_some_and(|line| line.trim().eq_ignore_ascii_case("UserPropBufferY"));
    if !(keyed_by_name || keyed_by_value) {
        return;
    }
    for line in value.lines().skip(usize::from(keyed_by_value)) {
        let Some((target, id)) = face_morph_tag(line) else {
            continue;
        };
        if target > 256 {
            continue;
        }
        if out.len() <= target {
            out.resize(target + 1, None);
        }
        out[target] = Some(id);
    }
}

/// The player-head `FM` target ids attached to a shape's extra-data list.
///
/// A `NiStringExtraData` block can appear in either a 10.x counted list or a 4.x `next` chain.
/// Read both, cap traversal so malformed data cannot loop, and accept only `FM` entries in the
/// source user-property buffer. The blank zero entry deliberately lines up with target zero (the
/// base shape). Some retail fig3 heads attach the buffer to their parent `NiNode`, not the leaf
/// `NiTriShape`; when the leaf has no direct tag, the file-wide fallback is deliberately scoped to
/// parsed `FM` entries and therefore cannot mistake arbitrary artist notes for facial data.
fn face_morph_tags_for(blocks: &[Block], av: &AvObject) -> Vec<Option<u8>> {
    let mut refs = av.extra_list.clone();
    if refs.is_empty() && av.extra_data >= 0 {
        refs.push(av.extra_data);
    }
    let mut out = Vec::new();
    for first in refs {
        let mut current = first;
        for _ in 0..256 {
            let Some(Block::StringExtra { next, name, value }) =
                usize::try_from(current).ok().and_then(|i| blocks.get(i))
            else {
                break;
            };
            append_face_morph_tags(&mut out, name.as_deref(), value);
            if *next < 0 {
                break;
            }
            current = *next;
        }
    }
    if out.is_empty() {
        for block in blocks {
            let Block::StringExtra { name, value, .. } = block else {
                continue;
            };
            append_face_morph_tags(&mut out, name.as_deref(), value);
        }
    }
    out
}

/// Every animated UV channel bound to the shape at block `shape_idx`.
///
/// Slot 0 only, and that is a real limitation rather than a simplification. Albion's rolling
/// clouds are a transform on map slot 6 (`alb_clouds copy.dds`) of the sky's property, and we
/// sample slot 0 — so the sky gradient drifts and no cloud appears. Animating slot 0 with slot 6's
/// curve would slide the wrong sheet. Drawing the later stages needs multi-stage compositing,
/// which is a bigger change than this one.
fn uv_anim_for(blocks: &[Block], texturing_block: i32) -> Option<UvAnim> {
    if texturing_block < 0 {
        return None;
    }
    let mut channels = Vec::new();
    for b in blocks {
        let Block::TexTransform {
            target,
            slot,
            operation,
            data,
        } = b
        else {
            continue;
        };
        if *target != texturing_block || *slot != 0 {
            continue;
        }
        let (Some(op), Some(Block::FloatData(keys))) = (
            UvOp::from_operation(*operation),
            usize::try_from(*data).ok().and_then(|i| blocks.get(i)),
        ) else {
            continue;
        };
        if keys.len() > 1 {
            channels.push((op, keys.clone()));
        }
    }
    let anim = UvAnim { channels };
    anim.is_animated().then_some(anim)
}

fn bone_id_from_extra(blocks: &[Block], mut extra: i32) -> Option<i32> {
    while let Some(Block::StringExtra { next, value, .. }) =
        usize::try_from(extra).ok().and_then(|i| blocks.get(i))
    {
        if let Ok(id) = value.trim().parse::<i32>() {
            return Some(id);
        }
        extra = *next;
    }
    None
}

/// Build the bind-pose skeleton by walking the `NiNode` tree from the root (block 0), accumulating
/// each node's world-bind transform. Returns the skeleton plus a `block index → bone index` map so
/// a skin's bone list (which references `NiNode` block indices) resolves to skeleton bones.
fn build_skeleton(blocks: &[Block]) -> (Skeleton, std::collections::HashMap<i32, usize>) {
    let mut bones: Vec<Bone> = Vec::new();
    let mut block_to_bone: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
    let mut name_to_index: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    // Explicit stack (idx, parent bone, parent world-bind) to avoid deep recursion / borrow tangles.
    let mut stack = vec![(0i32, None::<usize>, glam_identity())];
    while let Some((idx, parent, parent_world)) = stack.pop() {
        let Some(block) = usize::try_from(idx).ok().and_then(|i| blocks.get(i)) else {
            continue;
        };
        // Only NiNode-family blocks form the skeleton; shapes/props are not bones.
        let (av, children) = match block {
            Block::Node {
                av,
                children,
                billboard: _,
            }
            | Block::Lod { av, children } => (av, children),
            _ => continue,
        };
        if block_to_bone.contains_key(&idx) {
            continue; // a node reached twice (shared ref) — keep the first placement
        }
        let local = (av.rotation, av.scale, av.translation);
        let world_bind = xform_mul(&parent_world, &local);
        let bone_idx = bones.len();
        // Prefer an explicit id from the node's extra-data chain (rare on meshes); otherwise resolve
        // it from the canonical Biped table by name, so `.kfa` clips (keyed by id) bind to this bone.
        let id = bone_id_from_extra(blocks, av.extra_data).or_else(|| canonical_biped_id(&av.name));
        bones.push(Bone {
            name: av.name.clone(),
            parent,
            local,
            world_bind,
            id,
        });
        block_to_bone.insert(idx, bone_idx);
        name_to_index.entry(av.name.clone()).or_insert(bone_idx);
        // Push children (reverse so they pop in authored order — cosmetic, keeps indices stable).
        for &ch in children.iter().rev() {
            stack.push((ch, Some(bone_idx), world_bind));
        }
    }
    let mut skel = Skeleton {
        bones,
        name_to_index,
    };
    // Reordering renumbers bones, so the NIF-block map has to follow it or every skinned vertex
    // binds to the wrong bone.
    let remap = reparent_twist_bones(&mut skel);
    for v in block_to_bone.values_mut() {
        *v = remap[*v];
    }
    (skel, block_to_bone)
}

/// Transpose a 3x3 (row-major) — the inverse of a pure rotation.
fn mat_transpose(m: &[f32; 9]) -> [f32; 9] {
    [m[0], m[3], m[6], m[1], m[4], m[7], m[2], m[5], m[8]]
}

/// The local transform a bone needs under `parent` to keep its existing `world` bind:
/// `local = parent⁻¹ ∘ world`.
fn rebase_local(parent: &Xform, world: &Xform) -> Xform {
    let rt = mat_transpose(&parent.0);
    let rot = mat_mul(&rt, &world.0);
    let ps = if parent.1.abs() > 1e-6 { parent.1 } else { 1.0 };
    let d = [
        world.2[0] - parent.2[0],
        world.2[1] - parent.2[1],
        world.2[2] - parent.2[2],
    ];
    (rot, world.1 / ps, mat_apply(&rt, 1.0 / ps, d))
}

/// Re-parent forearm TWIST bones onto the forearm they twist.
///
/// DAoC's Biped rigs hang `Bip01 <side> ForeTwist` off the **UpperArm**, not the Forearm, and give
/// it no DAoC bone id — so a `.kfa` clip (which keys bones by id) can never animate it, and FK only
/// carries the UPPER arm's motion into it. The original client drives twist bones procedurally from
/// the limb's roll; we don't.
///
/// That is invisible at bind pose and ruinous in motion, because the avatar's `arms01` mesh is
/// weighted to `ForeTwist` while `gloves01` is weighted to `Hand`/`Forearm`. When the forearm
/// rotates, hand vertices follow it and the arm's wrist-end vertices do not — the mesh tears apart
/// exactly at the wrist ("the wrists ain't wristin"). Reparenting onto the Forearm makes ordinary
/// FK deliver the forearm's rotation, which is what the twist bone is meant to inherit.
///
/// The world bind is preserved (`rebase_local`), so the bind pose — and every bind-vs-static check
/// that validates it — is unchanged. Only bones whose new parent already precedes them are moved,
/// since the FK walk relies on parents coming first.
fn reparent_twist_bones(skel: &mut Skeleton) -> Vec<usize> {
    let n = skel.bones.len();
    let forearm_of = |skel: &Skeleton, i: usize| -> Option<usize> {
        let b = &skel.bones[i];
        if !b.name.ends_with("ForeTwist") {
            return None;
        }
        let forearm = b.name.trim_end_matches("ForeTwist").to_string() + "Forearm";
        skel.name_to_index.get(&forearm).copied()
    };

    // The easy half: the forearm already precedes, so the parent can simply be changed.
    let ahead: Vec<(usize, usize)> = (0..n)
        .filter_map(|i| forearm_of(skel, i).map(|p| (i, p)))
        .filter(|&(i, p)| p < i && skel.bones[i].parent != Some(p))
        .collect();
    for (i, parent) in ahead {
        let world = skel.bones[i].world_bind;
        let parent_world = skel.bones[parent].world_bind;
        skel.bones[i].parent = Some(parent);
        skel.bones[i].local = rebase_local(&parent_world, &world);
    }

    // **The hard half: a rig that lists the twist bone BEFORE the forearm it twists.**
    //
    // Kobold male is the only one of the 36 that does, and refusing to reparent there is why his
    // wrists tore while every other race's healed. The guard is right — the FK walk is one forward
    // pass and needs parents first — so the answer is to MOVE the bone, not to relax it.
    //
    // The twist bone's whole SUBTREE travels with it. Kobold male hangs `ForeTwist1` and a
    // `Shield` attachment off each ForeTwist, so moving the bone alone would orphan them past
    // their own parent and break the very ordering this is fixing.
    let moves: Vec<(usize, usize)> = (0..n)
        .filter_map(|i| forearm_of(skel, i).map(|p| (i, p)))
        .filter(|&(i, p)| p > i)
        .collect();
    if moves.is_empty() {
        return (0..n).collect();
    }
    // Descendants in ascending order: the array is parent-before-child, so one forward pass marks
    // the whole subtree, and keeping that order means every mover's parent still precedes it.
    let subtree_of = |root: usize| -> Vec<usize> {
        let mut inside = vec![false; n];
        inside[root] = true;
        for i in root + 1..n {
            if let Some(p) = skel.bones[i].parent {
                if inside[p] {
                    inside[i] = true;
                }
            }
        }
        (0..n).filter(|&i| inside[i]).collect()
    };
    let mut after: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    let mut is_moved = vec![false; n];
    for &(twist, forearm) in &moves {
        let sub = subtree_of(twist);
        for &b in &sub {
            is_moved[b] = true;
        }
        after.entry(forearm).or_default().extend(sub);
    }
    let mut order: Vec<usize> = Vec::with_capacity(n);
    for (old, moved) in is_moved.iter().enumerate() {
        if *moved {
            continue;
        }
        order.push(old);
        if let Some(sub) = after.get(&old) {
            order.extend(sub.iter().copied());
        }
    }
    debug_assert_eq!(order.len(), n, "the reorder must keep every bone");

    let mut new_of = vec![0usize; n];
    for (new, &old) in order.iter().enumerate() {
        new_of[old] = new;
    }
    let mut bones: Vec<Bone> = Vec::with_capacity(n);
    for &old in &order {
        let mut b = skel.bones[old].clone();
        b.parent = b.parent.map(|p| new_of[p]);
        bones.push(b);
    }
    // Now that the forearm precedes it, the twist bone can take it as a parent.
    for &(old_twist, old_forearm) in &moves {
        let (i, parent) = (new_of[old_twist], new_of[old_forearm]);
        let world = bones[i].world_bind;
        let parent_world = bones[parent].world_bind;
        bones[i].parent = Some(parent);
        bones[i].local = rebase_local(&parent_world, &world);
    }
    skel.bones = bones;
    for v in skel.name_to_index.values_mut() {
        *v = new_of[*v];
    }
    new_of
}

/// Collapse a vertex's accumulated `(bone, weight)` influences to the top four by weight, normalised
/// to sum to 1 (the GPU skin shader blends exactly four). Fewer than four → zero-weight padding.
fn pack_influence(mut infl: Vec<(u16, f32)>) -> ([u16; 4], [f32; 4]) {
    infl.sort_by(|a, b| b.1.total_cmp(&a.1));
    infl.truncate(4);
    let sum: f32 = infl.iter().map(|(_, w)| *w).sum();
    let mut joints = [0u16; 4];
    let mut weights = [0.0f32; 4];
    for (k, &(b, w)) in infl.iter().enumerate() {
        joints[k] = b;
        weights[k] = if sum > 0.0 { w / sum } else { 0.0 };
    }
    (joints, weights)
}

/// Extract the rigged (skeletal) form of a NIF: the bind-pose skeleton and every skinned mesh part
/// with per-vertex bone bindings, ready for animation. Returns `Ok(None)` when the NIF has no skin
/// (a static prop — use [`read_model`]). Independent of [`read_model`]: the static render path is
/// untouched.
///
/// The animated position of a skinned vertex `v` is
/// `Σ_b weightᵦ · world_animᵦ · inverse_bindᵦ · v` — at bind pose `world_anim = world_bind`, which
/// the extraction test checks reproduces the static mesh.
/// The bone NAMES a skinned NIF binds to, in skin-slot order (its `NiStringsExtraData`).
///
/// Diagnostic, and load-bearing for one: a fig3 body part binds to a shared skeleton BY NAME, and
/// a name that doesn't resolve makes [`read_rigged_with`] skip that bone entirely — every vertex
/// weighted only to it ends up with no influences and stops animating, which reads on screen as a
/// piece of the body detaching. That failure is invisible to a bind-pose check, because at bind
/// pose every bone is at its bind transform by definition.
#[must_use]
pub fn skin_bone_names(bytes: &[u8]) -> Vec<String> {
    let Ok(blocks) = parse_blocks(bytes) else {
        return Vec::new();
    };
    blocks
        .iter()
        .find_map(|b| match b {
            Block::StringsExtra { values } => Some(values.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The node names the first skin instance's bone pointers refer to, in skin-slot order.
///
/// The fallback for parts that ship no `NiStringsExtraData`: `fig014`'s `hal_f_head01` and
/// `sar_f_head01` have none, and without a name list the external-rig path silently reverts to
/// in-file block indices, which then index a *different* skeleton — eyeball vertices ended up
/// driven by beard bones. Empty when the pointers name nothing usable.
#[must_use]
pub fn skin_bone_block_names(bytes: &[u8]) -> Vec<String> {
    let Ok(blocks) = parse_blocks(bytes) else {
        return Vec::new();
    };
    let get = |r: i32| usize::try_from(r).ok().and_then(|i| blocks.get(i));
    for b in &blocks {
        let Block::Shape { skin, .. } = b else {
            continue;
        };
        if *skin < 0 {
            continue;
        }
        let Some(Block::SkinInstance { bones, .. }) = get(*skin) else {
            continue;
        };
        return bones
            .iter()
            .map(|r| match get(*r) {
                Some(Block::Node { av, .. }) => av.name.clone(),
                _ => String::new(),
            })
            .collect();
    }
    Vec::new()
}

pub fn read_rigged(bytes: &[u8]) -> io::Result<Option<RiggedModel>> {
    read_rigged_with(bytes, None)
}

/// Rig a mesh whose skin binds to an **external** skeleton — the fig3 player-body parts.
///
/// Those parts carry no bone hierarchy of their own (a single `"Scene Root"` node). Their
/// `NiSkinInstance` bone pointers are therefore unresolvable in-file; instead the part ships a
/// `NiStringsExtraData` block listing the skin's bone **names** ("Bip01 Pelvis", "Bip01 Spine", …)
/// in the same order as the skin's per-bone data. Those names resolve against a separate skeleton
/// NIF (the client's `monnifs.csv` `Skeleton Archive` column; `figures/skeleton.nif` is the
/// canonical Biped). Pass that skeleton here — via [`read_skeleton`] — and the part rigs normally.
///
/// Posing such a part against its own 1-bone skeleton collapses the mesh, which is exactly what
/// the first avatar attempt did; the bind-reconstruction check is what catches it.
pub fn read_rigged_external(bytes: &[u8], skeleton: &Skeleton) -> io::Result<Option<RiggedModel>> {
    read_rigged_with(bytes, Some(skeleton))
}

/// Shared implementation: `external = None` binds the skin to the file's own bone tree (creatures),
/// `Some(skel)` binds it by name to a supplied skeleton (fig3 player-body parts).
fn read_rigged_with(bytes: &[u8], external: Option<&Skeleton>) -> io::Result<Option<RiggedModel>> {
    let blocks = parse_blocks(bytes)?;
    let (own_skeleton, block_to_bone) = build_skeleton(&blocks);
    let skeleton = match external {
        Some(s) => s.clone(),
        None => own_skeleton,
    };
    let nbones = skeleton.bones.len();
    // The skin's bone-NAME list, used only in the external case to map skin bone slot → skeleton
    // bone. Taken from the file's first `NiStringsExtraData` (parts carry exactly one).
    let bone_names: Option<&Vec<String>> = external.and(blocks.iter().find_map(|b| match b {
        Block::StringsExtra { values } => Some(values),
        _ => None,
    }));
    let get = |r: i32| usize::try_from(r).ok().and_then(|i| blocks.get(i));

    let mut parts = Vec::new();
    for (shape_index, block) in blocks.iter().enumerate() {
        let Block::Shape { av, data, skin } = block else {
            continue;
        };
        if *skin < 0 {
            continue; // unrigged shape in an otherwise-rigged file (rare) — skip
        }
        let Some(Block::SkinInstance {
            data: skin_data,
            bones: skin_bones,
        }) = get(*skin)
        else {
            continue;
        };
        // The overall `NiSkinData` transform maps skeleton-root space back to the shape's local space
        // (its inverse-placement); the per-bone inverse-binds already map shape-local straight into
        // bone space, so it is NOT part of the vertex skinning chain and is deliberately unused.
        // (Verified: `world_bind · inverse_bind · v` reproduces the static bind mesh exactly; folding
        // the overall transform in shifted every vertex by its ~48-unit translation.)
        let Some(Block::SkinData {
            bones: bone_skins, ..
        }) = get(*skin_data)
        else {
            continue;
        };
        let Some(Block::ShapeData(geo)) = get(*data) else {
            continue;
        };
        let nv = geo.positions.len();
        if nv == 0 || geo.triangles.is_empty() {
            continue;
        }

        // Accumulate every bone's weighted vertices; record each referenced bone's inverse-bind
        // (shape-local → bone space at bind). Skinning is then `world_animᵦ · inverse_bindᵦ · v`.
        let mut infl: Vec<Vec<(u16, f32)>> = vec![Vec::new(); nv];
        let mut inverse_bind = vec![glam_identity(); nbones];
        for (i, bs) in bone_skins.iter().enumerate() {
            // External rig: slot i names its bone; own rig: slot i points at a block in this file.
            //
            // Parts state that name in one of two ways and the client ships both. Most carry a
            // `NiStringsExtraData` list. `fig014`'s `hal_f_head01` / `sar_f_head01` carry none, and
            // instead point at in-file bone NODES that hold the names ("Bip01 EyeLids", …) — the
            // reverse of Briton, whose nodes are all "Scene Root" and whose list is authoritative.
            //
            // Falling through to the own-rig branch here was the Half Ogre "swollen skewed head":
            // in-file block indices were used to index the EXTERNAL skeleton, so eyeball and eyelid
            // vertices were driven by `Bip01 Ponytail1` / `Beard1` / `Beard2`.
            //
            // The list wins when present, per slot resolution unchanged, so files that already
            // worked are untouched.
            let external_bone = external.and_then(|_| match bone_names {
                Some(names) => names
                    .get(i)
                    .and_then(|n| skeleton.name_to_index.get(n))
                    .copied(),
                None => skin_bones
                    .get(i)
                    .and_then(|&r| get(r))
                    .and_then(|b| match b {
                        Block::Node { av, .. } => skeleton.name_to_index.get(&av.name).copied(),
                        _ => None,
                    }),
            });
            let bone = match (external.is_some(), external_bone) {
                (true, Some(b)) => b,
                (true, None) => continue,
                (false, _) => {
                    let Some(&block_idx) = skin_bones.get(i) else {
                        continue;
                    };
                    let Some(&b) = block_to_bone.get(&block_idx) else {
                        continue;
                    };
                    b
                }
            };
            inverse_bind[bone] = bs.inverse_bind;
            for &(vi, w) in &bs.weights {
                if (vi as usize) < nv && w > 0.0 {
                    infl[vi as usize].push((bone as u16, w));
                }
            }
        }

        let mut joints = Vec::with_capacity(nv);
        let mut weights = Vec::with_capacity(nv);
        for per_vertex in infl {
            let (j, w) = pack_influence(per_vertex);
            joints.push(j);
            weights.push(w);
        }
        let nvu = nv as u32;
        parts.push(RiggedPart {
            name: av.name.clone(),
            positions: geo.positions.clone(),
            normals: if geo.normals.len() == nv {
                geo.normals.clone()
            } else {
                Vec::new()
            },
            uvs: if geo.uvs.len() == nv {
                geo.uvs.clone()
            } else {
                Vec::new()
            },
            indices: geo
                .triangles
                .iter()
                .flat_map(|t| t.iter().map(|&i| u32::from(i)))
                .filter(|&i| i < nvu)
                .collect(),
            joints,
            weights,
            inverse_bind,
            texture: apply_props(&blocks, &av.properties, MatCtx::default()).texture,
            morph: raw_morph_for(&blocks, shape_index as i32, av.controller).cloned(),
            morph_target_ids: face_morph_tags_for(&blocks, av),
        });
    }

    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(RiggedModel { skeleton, parts }))
}

/// Extract just the bind-pose skeleton (bone tree + world-bind transforms) from a NIF, with no skin.
/// Used for pure skeleton references like `figures/skeleton.nif` (the fullest canonical Biped, which
/// carries no `NiTriShape`/skin so [`read_rigged`] returns `None`) — the source of the canonical
/// bone-id ordering — and any time the runtime needs a mesh's bone hierarchy without its geometry.
pub fn read_skeleton(bytes: &[u8]) -> io::Result<Skeleton> {
    let blocks = parse_blocks(bytes)?;
    let (skeleton, _) = build_skeleton(&blocks);
    if skeleton.bones.is_empty() {
        return Err(err("no bones (not a skeleton nif?)"));
    }
    Ok(skeleton)
}

impl RiggedModel {
    /// The bind-pose world position of a skinned vertex, via the full skinning chain at rest
    /// (`world_anim = world_bind`). At bind this should reproduce the static mesh vertex — the
    /// extraction correctness gate. `part`/`vi` index [`RiggedModel::parts`] and its vertex arrays.
    pub fn bind_position(&self, part: usize, vi: usize) -> [f32; 3] {
        let p = &self.parts[part];
        let v = p.positions[vi];
        let (js, ws) = (p.joints[vi], p.weights[vi]);
        let mut out = [0.0f32; 3];
        for k in 0..4 {
            let w = ws[k];
            if w == 0.0 {
                continue;
            }
            let bone = js[k] as usize;
            // skinning matrix at bind = world_bind · inverse_bind (which already folds the overall
            // skin transform), applied to the raw shape-space vertex.
            let skin = xform_mul(&self.skeleton.bones[bone].world_bind, &p.inverse_bind[bone]);
            let d = xform_apply(&skin, v);
            out[0] += w * d[0];
            out[1] += w * d[1];
            out[2] += w * d[2];
        }
        out
    }
}

// ── Animation clips (Phase A.3.2): parse a `.kfa` into per-bone TRS tracks ──────────────────────

/// The latest key time across a track's channels — the track's contribution to the clip length.
fn track_duration(t: &TrsTrack) -> f32 {
    let rot = match &t.rotation {
        RotChannel::None => 0.0,
        RotChannel::Quat(k) => k.last().map(|k| k.time).unwrap_or(0.0),
        RotChannel::Euler { x, y, z } => {
            let m = |k: &[Key<1>]| k.last().map(|k| k.time).unwrap_or(0.0);
            m(x).max(m(y)).max(m(z))
        }
    };
    let trans = t.translation.last().map(|k| k.time).unwrap_or(0.0);
    let scale = t.scale.last().map(|k| k.time).unwrap_or(0.0);
    rot.max(trans).max(scale)
}

/// Parse an animation `.kfa` into a [`Clip`]: per-bone TRS tracks keyed by DAoC bone id, plus the
/// clip length.
///
/// A `.kfa` is the same NetImmerse container as a mesh NIF, structured as a **retargetable** clip
/// off the root node: two parallel chains hang from block 0 — a `NiStringExtraData` chain (via each
/// node's `extra_data`, each holding a bone id as a string) and a `NiKeyframeController` chain (via
/// `controller`, each with a `NiKeyframeData` track). They run in lockstep — the i-th bone id owns
/// the i-th track — so we bind by id, letting one clip play on any mesh whose bones carry those ids.
pub fn read_clip(bytes: &[u8]) -> io::Result<Clip> {
    let blocks = parse_blocks(bytes)?;
    let get = |r: i32| usize::try_from(r).ok().and_then(|i| blocks.get(i));
    // The root node (block 0) heads both chains.
    let Some(Block::Node { av, .. }) = blocks.first() else {
        return Err(err("no root node (not a .kfa?)"));
    };
    // The bone-id sequence reaches us two different ways depending on the format generation:
    //   4.x  — ONE extra-data ref off the root, chained via each block's `next`.
    //   10.1 — a counted LIST on the root; 10.x dropped extra-data chaining entirely.
    // Both run parallel to the controller chain (i-th id ↔ i-th controller), so collect the ids
    // whichever way they're stored and zip. Without the list branch every 10.1 clip parses to zero
    // tracks — 1564 of the client's 4071 `.kfa` files, i.e. 38% of all animation.
    let mut ids: Vec<i32> = Vec::new();
    if av.extra_list.len() > 1 {
        ids.extend(av.extra_list.iter().copied());
    } else {
        let mut extra = av.extra_data;
        while let Some(Block::StringExtra { next, .. }) = get(extra) {
            ids.push(extra);
            extra = *next;
        }
    }
    // The controller chain is version-agnostic.
    let mut ctrls: Vec<i32> = Vec::new();
    let mut ctrl = av.controller;
    while let Some(Block::KeyframeController { next, .. }) = get(ctrl) {
        ctrls.push(ctrl);
        ctrl = *next;
    }

    let mut tracks = std::collections::HashMap::new();
    let mut duration = 0.0f32;
    for (e, c) in ids.iter().zip(ctrls.iter()) {
        let (Some(Block::StringExtra { value, .. }), Some(Block::KeyframeController { data, .. })) =
            (get(*e), get(*c))
        else {
            continue;
        };
        if let (Ok(id), Some(Block::KeyframeData(track))) =
            (value.trim().parse::<i32>(), get(*data))
        {
            duration = duration.max(track_duration(track));
            tracks.insert(id, track.clone());
        }
    }
    if tracks.is_empty() {
        return Err(err("no animation tracks (not a .kfa?)"));
    }
    // Rate is not in the .kfa — the caller attaches it from the anim tables.
    Ok(Clip {
        tracks,
        duration,
        rate: 1.0,
    })
}

// ── Pose sampling (Phase A.3.3): a clip + time → the per-bone skinning-matrix palette ───────────

/// A rotation key as (axis, angle-in-degrees) — what a clip rotates ABOUT, rather than raw
/// components. Takes `[w, x, y, z]`, the order the `.kfa` stores and [`quat_to_mat`] destructures.
#[must_use]
pub fn quat_axis_angle(q: [f32; 4]) -> ([f32; 3], f32) {
    let [w, x, y, z] = q;
    // Both q and -q name the same rotation; pick the short arc so the angle is in [0, 180].
    let (w, x, y, z) = if w < 0.0 {
        (-w, -x, -y, -z)
    } else {
        (w, x, y, z)
    };
    let sin_half = (1.0 - w * w).max(0.0).sqrt();
    let angle = 2.0 * w.clamp(-1.0, 1.0).acos();
    if sin_half < 1e-6 {
        return ([0.0, 0.0, 0.0], 0.0);
    }
    (
        [x / sin_half, y / sin_half, z / sin_half],
        angle.to_degrees(),
    )
}

/// Unit quaternion `[w, x, y, z]` → row-major 3×3 rotation matrix, in the same `v' = M·v` convention
/// the NIF bind matrices use (so a sampled rotation composes with bind transforms via [`mat_mul`]).
fn quat_to_mat(q: [f32; 4]) -> [f32; 9] {
    let [w, x, y, z] = q;
    let (xx, yy, zz) = (x * x, y * y, z * z);
    let (xy, xz, yz) = (x * y, x * z, y * z);
    let (wx, wy, wz) = (w * x, w * y, w * z);
    [
        1.0 - 2.0 * (yy + zz),
        2.0 * (xy - wz),
        2.0 * (xz + wy),
        2.0 * (xy + wz),
        1.0 - 2.0 * (xx + zz),
        2.0 * (yz - wx),
        2.0 * (xz - wy),
        2.0 * (yz + wx),
        1.0 - 2.0 * (xx + yy),
    ]
}

/// Locate `t` among ascending key times: returns `(i, u)` where the value lies `u∈[0,1]` of the way
/// from key `i` to key `i+1`. Clamps before the first / after the last key (hold the end value).
fn key_span<const N: usize>(keys: &[Key<N>], t: f32) -> Option<(usize, f32)> {
    if keys.is_empty() {
        return None;
    }
    if t <= keys[0].time {
        return Some((0, 0.0));
    }
    let last = keys.len() - 1;
    if t >= keys[last].time {
        return Some((last, 0.0));
    }
    // Linear scan (tracks are short — tens of keys); find the bracketing pair.
    let j = keys.iter().position(|k| k.time > t).unwrap_or(last);
    let (a, b) = (&keys[j - 1], &keys[j]);
    let span = b.time - a.time;
    let u = if span > 1e-9 {
        (t - a.time) / span
    } else {
        0.0
    };
    Some((j - 1, u))
}

/// Interpolate a translation/scale channel (linear) at time `t`; `None` if the channel is empty.
fn sample_lerp<const N: usize>(keys: &[Key<N>], t: f32) -> Option<[f32; N]> {
    let (i, u) = key_span(keys, t)?;
    let a = keys[i].value;
    if u == 0.0 {
        return Some(a);
    }
    let b = keys[i + 1].value;
    let mut out = [0.0f32; N];
    for k in 0..N {
        out[k] = a[k] + (b[k] - a[k]) * u;
    }
    Some(out)
}

/// Normalised-lerp between two quaternions (cheap, stable for the dense keys DAoC ships). Flips the
/// second quaternion to the near hemisphere first so the short arc is taken.
fn nlerp(a: [f32; 4], mut b: [f32; 4], u: f32) -> [f32; 4] {
    let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    if dot < 0.0 {
        b = [-b[0], -b[1], -b[2], -b[3]];
    }
    let mut q = [0.0f32; 4];
    for k in 0..4 {
        q[k] = a[k] + (b[k] - a[k]) * u;
    }
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if n > 1e-9 {
        for e in &mut q {
            *e /= n;
        }
    } else {
        q = [1.0, 0.0, 0.0, 0.0];
    }
    q
}

/// Sample a rotation channel at time `t` → a rotation matrix, or `None` if the channel holds no
/// rotation (the bone keeps its bind rotation). Quaternion keys nlerp; per-axis Euler keys build
/// X·Y·Z rotations and lerp the angles.
fn sample_rotation(ch: &RotChannel, t: f32) -> Option<[f32; 9]> {
    match ch {
        RotChannel::None => None,
        RotChannel::Quat(keys) => {
            let (i, u) = key_span(keys, t)?;
            let q = if u == 0.0 {
                keys[i].value
            } else {
                nlerp(keys[i].value, keys[i + 1].value, u)
            };
            Some(quat_to_mat(q))
        }
        RotChannel::Euler { x, y, z } => {
            // Axis angles (radians); a missing axis contributes no rotation.
            let ax = sample_lerp(x, t).map(|v| v[0]).unwrap_or(0.0);
            let ay = sample_lerp(y, t).map(|v| v[0]).unwrap_or(0.0);
            let az = sample_lerp(z, t).map(|v| v[0]).unwrap_or(0.0);
            let rx = axis_rot(0, ax);
            let ry = axis_rot(1, ay);
            let rz = axis_rot(2, az);
            Some(mat_mul(&rx, &mat_mul(&ry, &rz)))
        }
    }
}

/// Row-major rotation about principal axis `k` (0=x,1=y,2=z) by `a` radians.
fn axis_rot(k: usize, a: f32) -> [f32; 9] {
    let (s, c) = a.sin_cos();
    match k {
        0 => [1.0, 0.0, 0.0, 0.0, c, -s, 0.0, s, c],
        1 => [c, 0.0, s, 0.0, 1.0, 0.0, -s, 0.0, c],
        _ => [c, -s, 0.0, s, c, 0.0, 0.0, 0.0, 1.0],
    }
}

/// Row-major rotation matrix → unit quaternion `[w, x, y, z]` (Shepperd's method: pick the largest
/// diagonal term so the divisor never approaches zero). The inverse of [`quat_to_mat`].
///
/// Public so diagnostics share one implementation rather than reimplementing the branch logic.
#[must_use]
pub fn mat_to_quat(m: &[f32; 9]) -> [f32; 4] {
    let (m00, m11, m22) = (m[0], m[4], m[8]);
    let trace = m00 + m11 + m22;
    if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [
            0.25 * s,
            (m[7] - m[5]) / s,
            (m[2] - m[6]) / s,
            (m[3] - m[1]) / s,
        ]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [
            (m[7] - m[5]) / s,
            0.25 * s,
            (m[1] + m[3]) / s,
            (m[2] + m[6]) / s,
        ]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [
            (m[2] - m[6]) / s,
            (m[1] + m[3]) / s,
            0.25 * s,
            (m[5] + m[7]) / s,
        ]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [
            (m[3] - m[1]) / s,
            (m[2] + m[6]) / s,
            (m[5] + m[7]) / s,
            0.25 * s,
        ]
    }
}

/// Blend two local transforms: short-arc quaternion `nlerp` for rotation, linear for scale and
/// translation. See [`Skeleton::pose_blend`] for why this happens in local space.
fn blend_xform(a: &Xform, b: &Xform, w: f32) -> Xform {
    let q = nlerp(mat_to_quat(&a.0), mat_to_quat(&b.0), w);
    let lerp = |x: f32, y: f32| x + (y - x) * w;
    (
        quat_to_mat(q),
        lerp(a.1, b.1),
        [
            lerp(a.2[0], b.2[0]),
            lerp(a.2[1], b.2[1]),
            lerp(a.2[2], b.2[2]),
        ],
    )
}

/// Sample one bone's local transform from a clip.
///
/// `rigid_length` keeps the bind translation instead of the clip's. A bone's offset from its parent
/// IS its length, and that belongs to the skeleton, not the animation. Clips are retargeted across
/// bodies by bone id, so a clip authored on a longer forearm otherwise stretches a shorter one:
/// Half Ogre female's `Bip01 L Hand` sits 10.9 from her wrist while the shared idle places it at
/// 16.0, which dragged her hand out into a claw. Measured 5.09 units of disagreement on her, 1.39
/// on the male.
///
/// This is a no-op whenever a clip is played on the skeleton it was authored for — there the clip's
/// translation already equals the bind translation — so it only changes retargeted playback.
/// Root motion is exempt (see the caller): a root has no parent offset to preserve.
///
/// A bone's animated **local** transform at time `t`: each channel the clip drives replaces the bind
/// value; an absent channel holds the bind (`NiKeyframeData` values are absolute local transforms,
/// not deltas). `track` is `None` for bones the clip doesn't animate — they keep their bind local.
/// [`sample_local`] for one track, exposed so a diagnostic can ask what a clip actually says at a
/// time rather than inferring it from a composed world transform.
///
/// Rigid length is forced on: a caller asking this question wants the ROTATION the clip carries,
/// and the translation rule is a separate concern that would only confuse the answer.
#[must_use]
pub fn sample_track_local(track: &TrsTrack, t: f32, bind: &Xform) -> Xform {
    sample_local(Some(track), t, bind, true)
}

fn sample_local(track: Option<&TrsTrack>, t: f32, bind: &Xform, rigid_length: bool) -> Xform {
    let Some(tr) = track else { return *bind };
    let rot = sample_rotation(&tr.rotation, t).unwrap_or(bind.0);
    let scale = sample_lerp(&tr.scale, t).map(|v| v[0]).unwrap_or(bind.1);
    let trans = if rigid_length {
        bind.2
    } else {
        sample_lerp(&tr.translation, t).unwrap_or(bind.2)
    };
    (rot, scale, trans)
}

/// Does this bone carry root motion (so the clip's translation is authoritative)?
///
/// True for the skeleton root and its direct children — `Scene Root` and `Bip01` in a Biped rig,
/// where clip translation is the character's own movement rather than a limb length.
fn carries_root_motion(bones: &[Bone], i: usize) -> bool {
    match bones.get(i).and_then(|b| b.parent) {
        None => true,
        Some(p) => bones.get(p).is_none_or(|b| b.parent.is_none()),
    }
}

/// A deliberately narrow exception to normal rigid-retargeting for an authored pose.
///
/// The default animation path keeps every non-root local translation at bind.  That is the safe
/// rule for locomotion: a `.kfa` can be shared by bodies with different limb lengths, and applying
/// all of its translation keys moves fingers, feet, and wrists independently of their own meshes.
/// A character-select pose may need one measured exception, but it must name that exception rather
/// than silently turning the whole skeleton into an elastic retarget.
///
/// `translation_ids` preserves local translation only for the named non-root bones. Root motion is
/// always preserved, as it is in [`Skeleton::pose`]. `rotation_weights` blends a named bone's
/// sampled rotation toward its bind rotation (`1.0` = authored clip; `0.0` = bind). It lets a
/// presentation-only shoulder correction retain a race's authored spine, head, and limb posture.
#[derive(Clone, Copy, Debug, Default)]
pub struct PosePolicy<'a> {
    pub translation_ids: &'a [i32],
    pub rotation_weights: &'a [(i32, f32)],
}

impl<'a> PosePolicy<'a> {
    /// The ordinary world/rigid-retarget policy.
    pub const RIGID: Self = Self {
        translation_ids: &[],
        rotation_weights: &[],
    };

    fn retains_translation(self, id: i32) -> bool {
        self.translation_ids.contains(&id)
    }

    fn rotation_weight(self, id: i32) -> Option<f32> {
        self.rotation_weights
            .iter()
            .find_map(|&(candidate, weight)| (candidate == id).then_some(weight.clamp(0.0, 1.0)))
    }
}

impl Skeleton {
    /// World-space animated transform of every bone at clip time `t` (seconds). Each bone the clip
    /// drives (matched by its canonical [`Bone::id`]) is posed from the clip; the rest hold their
    /// bind local. Bones are topologically ordered (parent before child), so one forward pass
    /// accumulates world transforms. At a clip whose keys equal the bind local this reproduces
    /// `world_bind` exactly (the A.3.3 correctness gate).
    /// Pose blended between two clips: sample each bone's LOCAL transform from both, blend, then
    /// run forward kinematics once. `w` is the weight of `b` (0 = pure `a`, 1 = pure `b`).
    ///
    /// Blending **locals before FK** is the correct order — blending world transforms, or lerping
    /// the final skinning matrices, lets a limb's length change mid-blend and can shear the mesh.
    /// Rotations blend as quaternions (short-arc `nlerp`), not as matrices, for the same reason.
    pub fn pose_blend(&self, a: &Clip, ta: f32, b: &Clip, tb: f32, w: f32) -> Vec<Xform> {
        let mut world = Vec::new();
        self.pose_blend_into(a, ta, b, tb, w, &mut world);
        world
    }

    /// [`Self::pose_blend`] into a reused buffer (MS-08 assemble scratch).
    pub fn pose_blend_into(
        &self,
        a: &Clip,
        ta: f32,
        b: &Clip,
        tb: f32,
        w: f32,
        world: &mut Vec<Xform>,
    ) {
        let w = w.clamp(0.0, 1.0);
        world.clear();
        world.resize(self.bones.len(), glam_identity());
        for (i, bone) in self.bones.iter().enumerate() {
            let rigid = !carries_root_motion(&self.bones, i);
            let la = sample_local(
                bone.id.and_then(|id| a.tracks.get(&id)),
                ta,
                &bone.local,
                rigid,
            );
            let lb = sample_local(
                bone.id.and_then(|id| b.tracks.get(&id)),
                tb,
                &bone.local,
                rigid,
            );
            let local = blend_xform(&la, &lb, w);
            world[i] = match bone.parent {
                Some(p) => xform_mul(&world[p], &local),
                None => local,
            };
        }
    }

    pub fn pose(&self, clip: &Clip, t: f32) -> Vec<Xform> {
        let mut world = Vec::new();
        self.pose_into(clip, t, &mut world);
        world
    }

    /// [`Self::pose`] into a reused buffer — avoids per-call `Vec` alloc on the hot skin path (MS-08).
    pub fn pose_into(&self, clip: &Clip, t: f32, world: &mut Vec<Xform>) {
        self.pose_with_policy_into(clip, t, PosePolicy::RIGID, world);
    }

    /// Pose a clip with explicit, per-bone character-screen adjustments.
    ///
    /// This is intentionally the only non-rigid translation path. Callers must pass the exact
    /// biped IDs they have measured; there is no whole-rig "honour translations" switch because
    /// that switch visibly stretched Firbolg hands and feet in the character screen.
    pub fn pose_with_policy(&self, clip: &Clip, t: f32, policy: PosePolicy<'_>) -> Vec<Xform> {
        let mut world = Vec::new();
        self.pose_with_policy_into(clip, t, policy, &mut world);
        world
    }

    /// [`Self::pose_with_policy`] into a reused caller buffer.
    pub fn pose_with_policy_into(
        &self,
        clip: &Clip,
        t: f32,
        policy: PosePolicy<'_>,
        world: &mut Vec<Xform>,
    ) {
        world.clear();
        world.resize(self.bones.len(), glam_identity());
        for (i, b) in self.bones.iter().enumerate() {
            let track = b.id.and_then(|id| clip.tracks.get(&id));
            let retains_translation = carries_root_motion(&self.bones, i)
                || b.id.is_some_and(|id| policy.retains_translation(id));
            let mut local = sample_local(track, t, &b.local, !retains_translation);
            if let Some(weight) = b.id.and_then(|id| policy.rotation_weight(id)) {
                local.0 = quat_to_mat(nlerp(
                    mat_to_quat(&b.local.0),
                    mat_to_quat(&local.0),
                    weight,
                ));
            }
            world[i] = match b.parent {
                Some(p) => xform_mul(&world[p], &local),
                None => local,
            };
        }
    }

    /// Like [`Self::pose`], attributing wall time to keyframe sampling vs parent composition.
    /// Only for ATTR/QA runs — the hot path must use [`Self::pose`].
    pub fn pose_timed(&self, clip: &Clip, t: f32, timing: &mut BoneMatrixTiming) -> Vec<Xform> {
        let mut world = Vec::new();
        self.pose_timed_into(clip, t, timing, &mut world);
        world
    }

    /// [`Self::pose_timed`] into a reused buffer.
    pub fn pose_timed_into(
        &self,
        clip: &Clip,
        t: f32,
        timing: &mut BoneMatrixTiming,
        world: &mut Vec<Xform>,
    ) {
        world.clear();
        world.resize(self.bones.len(), glam_identity());
        for (i, b) in self.bones.iter().enumerate() {
            let t_sample = std::time::Instant::now();
            let track = b.id.and_then(|id| clip.tracks.get(&id));
            let local = sample_local(track, t, &b.local, !carries_root_motion(&self.bones, i));
            timing.keyframe_ns += t_sample.elapsed().as_nanos() as u64;
            let t_compose = std::time::Instant::now();
            world[i] = match b.parent {
                Some(p) => xform_mul(&world[p], &local),
                None => local,
            };
            timing.bone_compose_ns += t_compose.elapsed().as_nanos() as u64;
        }
    }
}

/// Nanosecond accumulators for one palette build (QA-4 / MS-01 anim_skin sub-attribution).
#[derive(Debug, Clone, Copy, Default)]
pub struct BoneMatrixTiming {
    /// [`sample_local`] / keyframe search + interpolate.
    pub keyframe_ns: u64,
    /// Parent-chain [`xform_mul`] while building world transforms.
    pub bone_compose_ns: u64,
    /// Inverse-bind fold + column-major matrix emit ([`RiggedModel::matrices_from_world`]).
    pub assemble_ns: u64,
}

impl RiggedModel {
    /// Per-part skinning-matrix palette at clip time `t`: `palette[part][bone] = world_animᵦ ·
    /// inverse_bindᵦ`. Applying `palette[part][bone]` to a raw shape-space vertex of `part` maps it to
    /// its animated world position; a skinned vertex blends its ≤4 influences (see
    /// [`RiggedModel::animated_position`]). At bind time this equals the bind skinning matrix, so a
    /// vertex reconstructs its [`RiggedModel::bind_position`].
    pub fn skinning_palette(&self, clip: &Clip, t: f32) -> Vec<Vec<Xform>> {
        let world = self.skeleton.pose(clip, t);
        self.parts
            .iter()
            .map(|p| {
                (0..self.skeleton.bones.len())
                    .map(|b| xform_mul(&world[b], &p.inverse_bind[b]))
                    .collect()
            })
            .collect()
    }

    /// Number of bones in one part's palette slice — the stride into [`RiggedModel::bone_matrices`].
    pub fn bone_stride(&self) -> usize {
        self.skeleton.bones.len()
    }

    /// The skinning palette as **GPU-ready 4×4 column-major matrices**, flattened to
    /// `parts × bone_stride()`. Part `p`'s matrix for bone `b` is at `p * bone_stride() + b`.
    ///
    /// This is [`RiggedModel::skinning_palette`] in the layout a storage buffer wants, so the
    /// vertex shader can do `Σ wᵢ · palette[base + jointᵢ] · v` — the same blend
    /// [`RiggedModel::animated_position`] does on the CPU today.
    ///
    /// The palette is **per part**, not per model, because each part carries its own inverse-bind
    /// transforms; that's why the flattening needs a stride rather than one array of bones.
    ///
    /// Column-major because that is what WGSL's `mat4x4<f32>` expects. The source [`Xform`] applies
    /// as `v' = s·(R·v) + t` with `R` row-major, so the scale folds into the rotation columns and
    /// the translation becomes column 3.
    pub fn bone_matrices(&self, clip: &Clip, t: f32) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.parts.len() * self.bone_stride());
        let mut world = Vec::new();
        self.bone_matrices_extend(clip, t, &mut world, &mut out);
        out
    }

    /// Hot-path palette emit into reused buffers (MS-08). `world` is pose scratch; matrices append to `out`.
    pub fn bone_matrices_extend(
        &self,
        clip: &Clip,
        t: f32,
        world: &mut Vec<Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        self.skeleton.pose_into(clip, t, world);
        self.matrices_from_world_extend(world, out);
    }

    /// GPU-ready palette for an explicitly measured pose policy.
    ///
    /// Unlike [`Self::bone_matrices`], this can retain a named local translation or soften a named
    /// rotation. It cannot preserve every translation indiscriminately; see [`PosePolicy`].
    pub fn bone_matrices_with_policy(
        &self,
        clip: &Clip,
        t: f32,
        policy: PosePolicy<'_>,
    ) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.parts.len() * self.bone_stride());
        let mut world = Vec::new();
        self.bone_matrices_with_policy_extend(clip, t, policy, &mut world, &mut out);
        out
    }

    /// [`Self::bone_matrices_with_policy`] into reused buffers.
    pub fn bone_matrices_with_policy_extend(
        &self,
        clip: &Clip,
        t: f32,
        policy: PosePolicy<'_>,
        world: &mut Vec<Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        self.skeleton.pose_with_policy_into(clip, t, policy, world);
        self.matrices_from_world_extend(world, out);
    }

    /// Like [`Self::bone_matrices`], with QA-4 phase attribution (ATTR only — not the hot path).
    pub fn bone_matrices_timed(
        &self,
        clip: &Clip,
        t: f32,
        timing: &mut BoneMatrixTiming,
    ) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.parts.len() * self.bone_stride());
        let mut world = Vec::new();
        self.bone_matrices_timed_extend(clip, t, timing, &mut world, &mut out);
        out
    }

    /// ATTR path with reused buffers.
    pub fn bone_matrices_timed_extend(
        &self,
        clip: &Clip,
        t: f32,
        timing: &mut BoneMatrixTiming,
        world: &mut Vec<Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        self.skeleton.pose_timed_into(clip, t, timing, world);
        let t_assemble = std::time::Instant::now();
        self.matrices_from_world_extend(world, out);
        timing.assemble_ns += t_assemble.elapsed().as_nanos() as u64;
    }

    /// Shared tail of [`Self::bone_matrices`] / [`Self::bone_matrices_blend`]: fold each part's
    /// inverse-bind into the posed world transforms and emit GPU-ready column-major matrices.
    pub fn matrices_from_world(&self, world: &[Xform]) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.parts.len() * self.bone_stride());
        self.matrices_from_world_extend(world, &mut out);
        out
    }

    /// Append skinning matrices into `out` without an intermediate allocation (MS-08).
    pub fn matrices_from_world_extend(&self, world: &[Xform], out: &mut Vec<[[f32; 4]; 4]>) {
        out.reserve(self.parts.len() * self.bone_stride());
        for p in &self.parts {
            for (b, w) in world.iter().enumerate() {
                out.push(xform_to_mat4(&xform_mul(w, &p.inverse_bind[b])));
            }
        }
    }

    /// Flatten every part's inverse-bind into GPU mat4s (`parts × bone_stride`), same order as
    /// [`Self::matrices_from_world_extend`]. Uploaded once; the hot path only streams posed bones.
    #[must_use]
    pub fn inverse_bind_mat4s(&self) -> Vec<[[f32; 4]; 4]> {
        let stride = self.bone_stride();
        let mut out = Vec::with_capacity(self.parts.len() * stride);
        for p in &self.parts {
            for b in 0..stride {
                out.push(xform_to_mat4(&p.inverse_bind[b]));
            }
        }
        out
    }

    /// [`RiggedModel::bone_matrices`] for a pose blended between two clips — the cross-fade used
    /// when an entity changes locomotion state, so idle→walk eases instead of snapping.
    pub fn bone_matrices_blend(
        &self,
        a: &Clip,
        ta: f32,
        b: &Clip,
        tb: f32,
        w: f32,
    ) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.parts.len() * self.bone_stride());
        let mut world = Vec::new();
        self.bone_matrices_blend_extend(a, ta, b, tb, w, &mut world, &mut out);
        out
    }

    /// Hot-path blend palette emit into reused buffers.
    pub fn bone_matrices_blend_extend(
        &self,
        a: &Clip,
        ta: f32,
        b: &Clip,
        tb: f32,
        w: f32,
        world: &mut Vec<Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        self.skeleton.pose_blend_into(a, ta, b, tb, w, world);
        self.matrices_from_world_extend(world, out);
    }

    /// Like [`Self::bone_matrices_blend`], with QA-4 phase attribution.
    pub fn bone_matrices_blend_timed(
        &self,
        a: &Clip,
        ta: f32,
        b: &Clip,
        tb: f32,
        w: f32,
        timing: &mut BoneMatrixTiming,
    ) -> Vec<[[f32; 4]; 4]> {
        let mut out = Vec::with_capacity(self.parts.len() * self.bone_stride());
        let mut world = Vec::new();
        self.bone_matrices_blend_timed_extend(a, ta, b, tb, w, timing, &mut world, &mut out);
        out
    }

    /// ATTR blend path with reused buffers.
    pub fn bone_matrices_blend_timed_extend(
        &self,
        a: &Clip,
        ta: f32,
        b: &Clip,
        tb: f32,
        w: f32,
        timing: &mut BoneMatrixTiming,
        world: &mut Vec<Xform>,
        out: &mut Vec<[[f32; 4]; 4]>,
    ) {
        let t_pose = std::time::Instant::now();
        self.skeleton.pose_blend_into(a, ta, b, tb, w, world);
        // Blend path: attribute whole dual-sample under keyframe so totals still sum.
        timing.keyframe_ns += t_pose.elapsed().as_nanos() as u64;
        let t_assemble = std::time::Instant::now();
        self.matrices_from_world_extend(world, out);
        timing.assemble_ns += t_assemble.elapsed().as_nanos() as u64;
    }

    /// The animated world position of skinned vertex `vi` of `part`, given a palette from
    /// [`RiggedModel::skinning_palette`] — the pose analogue of [`RiggedModel::bind_position`].
    pub fn animated_position(&self, palette: &[Vec<Xform>], part: usize, vi: usize) -> [f32; 3] {
        let p = &self.parts[part];
        let v = p.positions[vi];
        let (js, ws) = (p.joints[vi], p.weights[vi]);
        let mut out = [0.0f32; 3];
        for k in 0..4 {
            let w = ws[k];
            if w == 0.0 {
                continue;
            }
            let d = xform_apply(&palette[part][js[k] as usize], v);
            out[0] += w * d[0];
            out[1] += w * d[1];
            out[2] += w * d[2];
        }
        out
    }

    /// The animated normal of skinned vertex `vi` of `part`: the bind normal rotated by the same
    /// blended bone matrices (rotation only — translation/scale don't apply to directions), then
    /// renormalised. Returns `[0,0,1]` if the part carries no normal for the vertex.
    pub fn animated_normal(&self, palette: &[Vec<Xform>], part: usize, vi: usize) -> [f32; 3] {
        let p = &self.parts[part];
        let Some(&n) = p.normals.get(vi) else {
            return [0.0, 0.0, 1.0];
        };
        let (js, ws) = (p.joints[vi], p.weights[vi]);
        let mut out = [0.0f32; 3];
        for k in 0..4 {
            let w = ws[k];
            if w == 0.0 {
                continue;
            }
            let r = mat_apply(&palette[part][js[k] as usize].0, 1.0, n); // rotation only (s=1)
            out[0] += w * r[0];
            out[1] += w * r[1];
            out[2] += w * r[2];
        }
        let len = (out[0] * out[0] + out[1] * out[1] + out[2] * out[2]).sqrt();
        if len > 1e-6 {
            [out[0] / len, out[1] / len, out[2] / len]
        } else {
            [0.0, 0.0, 1.0]
        }
    }
}

/// Block-type sequence of a NIF, for diagnosing a parse desync.
///
/// A truncation reported at block N is rarely block N's fault: the reader desyncs earlier and only
/// runs off the end once some later block reads a length from misaligned bytes. Knowing which types
/// precede the failure is what turns "130 broken files" into one named parser gap.
pub fn block_type_sequence(bytes: &[u8]) -> io::Result<Vec<String>> {
    Ok(read_header(bytes)?.block_types)
}

/// Every NiTexturingProperty's populated slots, as `(block, map_slot_count, [(slot, filename)])`.
///
/// `Model::parts` retains the base plus its selected supported secondary layer, while this view
/// exposes every authored stage. It is how diagnostics distinguish a missing asset from a later
/// stage the renderer has not yet implemented. Slots below `map_slot_count` are fixed maps; at or
/// above it they are 10.x shader textures.
pub type TexturingStages = Vec<(usize, usize, Vec<(usize, String)>)>;

/// Every `NiTextureTransformController` as `(block, target, slot, operation, key count)`.
///
/// Diagnostic: "the scene has N of these" and "N of them are bound to something we draw" are
/// different claims, and only the second one animates anything.
pub type UvControllers = Vec<(usize, i32, u32, u32, usize)>;

pub fn uv_controllers(bytes: &[u8]) -> io::Result<UvControllers> {
    let blocks = parse_blocks(bytes)?;
    Ok(blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| match b {
            Block::TexTransform {
                target,
                slot,
                operation,
                data,
            } => {
                let n = match usize::try_from(*data).ok().and_then(|d| blocks.get(d)) {
                    Some(Block::FloatData(k)) => k.len(),
                    _ => 0,
                };
                Some((i, *target, *slot, *operation, n))
            }
            _ => None,
        })
        .collect())
}

pub fn texturing_stages(bytes: &[u8]) -> io::Result<TexturingStages> {
    let blocks = parse_blocks(bytes)?;
    let name_of = |r: i32| match usize::try_from(r).ok().and_then(|i| blocks.get(i)) {
        Some(Block::SourceTexture { file: Some(f) }) => f.clone(),
        Some(Block::SourceTexture { file: None }) => "<internal>".to_string(),
        _ => format!("<ref {r} is not a NiSourceTexture>"),
    };
    Ok(blocks
        .iter()
        .enumerate()
        .filter_map(|(i, b)| match b {
            Block::Texturing {
                slots, map_slots, ..
            } => Some((
                i,
                *map_slots,
                slots.iter().map(|&(s, r)| (s, name_of(r))).collect(),
            )),
            _ => None,
        })
        .collect())
}
