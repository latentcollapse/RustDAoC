//! Generated census of classic dungeon zones (MEAS-010 281 denominator).
//!
//! Rows come from shipped `zones/zoneNNN/datNNN.mpk` plus Zones.xml names and the OPEN_ORACLE
//! task-skin id table. Ok(empty) and boxes-only cannot PASS. This is a breadth instrument, not
//! a 281-playable claim.
//!
//! [`zone_denominator`] adds the 400-row MEAS-010 table with **separate** columns:
//! client-authored render coverage vs server-authoritative playable transition endpoints.

use std::io::{self, Write};
use std::path::Path;

use caer_world::dungeon_zones::{
    dungeon_realm_or_category, enumerate_dungeon_zone_ids, enumerate_primary_zone_classes,
    find_dat_mpk, load_dungeon_zone, DungeonRealmOrCategory, PrimaryZoneEntry, ZoneClass,
};

use crate::dungeon_mesh::{DungeonNifCache, DungeonProductReload};
use crate::terrain::{ModelInstance, TerrainMesh};
use crate::walkable::SurfaceIndex;

/// Decode outcome for one census row. `Ok` with zero placements is recorded separately and
/// cannot be [`CensusVerdict::Pass`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CensusDecode {
    Ok,
    Err,
}

/// Collision-index result after attempting real-NIF placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CensusCollision {
    SurfacesPresent,
    SurfacesEmpty,
    NotEvaluated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CensusVerdict {
    Pass,
    Fail,
}

/// One classic dungeon zone as measured from shipped tables/archives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DungeonCensusRow {
    pub region: Option<u16>,
    pub zone: u16,
    pub label: Option<&'static str>,
    pub realm_or_category: DungeonRealmOrCategory,
    pub has_chunk: bool,
    pub has_place: bool,
    pub has_prop: bool,
    pub decode: CensusDecode,
    pub placement_count: usize,
    pub real_nif_count: usize,
    pub fallback_count: usize,
    pub collision: CensusCollision,
    pub verdict: CensusVerdict,
    pub failure_reason: Option<&'static str>,
}

impl DungeonCensusRow {
    #[must_use]
    pub fn is_pass(&self) -> bool {
        self.verdict == CensusVerdict::Pass
    }
}

/// One MEAS-010 primary zone with split render / playable columns.
///
/// Census pass is **not** a playable-coverage claim. mixed_boxes may still render real NIFs
/// and must still Fail playable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneDenominatorRow {
    pub zone: u16,
    pub region: Option<u16>,
    pub class: ZoneClass,
    pub label: Option<&'static str>,
    pub has_authored_member: bool,
    pub decode: CensusDecode,
    pub placement_count: usize,
    pub real_nif_count: usize,
    pub fallback_count: usize,
    pub collision: CensusCollision,
    pub render_coverage: CensusVerdict,
    pub render_reason: Option<&'static str>,
    pub playable_transition: CensusVerdict,
    pub playable_reason: Option<&'static str>,
}

impl ZoneDenominatorRow {
    #[must_use]
    pub fn render_pass(&self) -> bool {
        self.render_coverage == CensusVerdict::Pass
    }

    #[must_use]
    pub fn playable_pass(&self) -> bool {
        self.playable_transition == CensusVerdict::Pass
    }
}

/// Client-authored render coverage. mixed_boxes with real NIFs may Pass here.
/// Skycity/interior have no reader → Fail. Empty decode cannot Pass.
#[must_use]
pub fn render_coverage_verdict(
    class: ZoneClass,
    decode: CensusDecode,
    placement_count: usize,
    real_nif_count: usize,
    has_authored_member: bool,
) -> (CensusVerdict, Option<&'static str>) {
    match class {
        ZoneClass::Skycity => (CensusVerdict::Fail, Some("skycity_no_reader")),
        ZoneClass::Interior => (CensusVerdict::Fail, Some("interior_no_reader")),
        ZoneClass::Surface => {
            if has_authored_member {
                (CensusVerdict::Pass, None)
            } else {
                (CensusVerdict::Fail, Some("no_terrain"))
            }
        }
        ZoneClass::Dungeon => {
            if decode != CensusDecode::Ok {
                return (CensusVerdict::Fail, Some("decode_error"));
            }
            if placement_count == 0 {
                return (CensusVerdict::Fail, Some("ok_empty"));
            }
            if real_nif_count == 0 {
                return (CensusVerdict::Fail, Some("boxes_only"));
            }
            (CensusVerdict::Pass, None)
        }
    }
}

/// Server-authoritative playable transition endpoint.
///
/// Fail when append is disabled, decode is empty, geometry is boxes-only, mixed_boxes leftovers
/// remain, collision is missing, or the zone is absent from the server Zones table.
/// A census Pass is not sufficient.
#[must_use]
pub fn playable_transition_verdict(
    class: ZoneClass,
    decode: CensusDecode,
    placement_count: usize,
    real_nif_count: usize,
    fallback_count: usize,
    collision: CensusCollision,
    server_region: Option<u16>,
    append_invoked: bool,
) -> (CensusVerdict, Option<&'static str>) {
    if !append_invoked {
        return (CensusVerdict::Fail, Some("append_disabled"));
    }
    match class {
        ZoneClass::Skycity => return (CensusVerdict::Fail, Some("skycity_no_reader")),
        ZoneClass::Interior => return (CensusVerdict::Fail, Some("interior_no_reader")),
        ZoneClass::Surface => {
            if decode != CensusDecode::Ok || placement_count == 0 {
                return (CensusVerdict::Fail, Some("ok_empty"));
            }
            if server_region.is_none() {
                return (CensusVerdict::Fail, Some("no_server_endpoint"));
            }
            return (CensusVerdict::Pass, None);
        }
        ZoneClass::Dungeon => {}
    }
    let (v, reason) = census_verdict(
        decode,
        placement_count,
        real_nif_count,
        fallback_count,
        collision,
    );
    if v == CensusVerdict::Fail {
        return (v, reason);
    }
    if server_region.is_none() {
        return (CensusVerdict::Fail, Some("no_server_endpoint"));
    }
    (CensusVerdict::Pass, None)
}

/// Map a product surface↔dungeon reload onto the playable column.
///
/// Skip-append, empty decode, FixtureBox-only, and mixed_boxes are red. `representative_pass`
/// may still be true with box leftovers — that is not a playable claim.
#[must_use]
pub fn playable_from_product_reload(
    reload: &DungeonProductReload,
) -> (CensusVerdict, Option<&'static str>) {
    let placement = reload.dungeon_stats.model_instances + reload.dungeon_stats.fixture_boxes;
    let collision = if reload.collision_present {
        CensusCollision::SurfacesPresent
    } else if reload.append_invoked && placement > 0 {
        CensusCollision::SurfacesEmpty
    } else {
        CensusCollision::NotEvaluated
    };
    playable_transition_verdict(
        ZoneClass::Dungeon,
        CensusDecode::Ok,
        placement,
        reload.dungeon_stats.model_instances,
        reload.dungeon_stats.fixture_boxes,
        collision,
        Some(reload.dungeon_region),
        reload.append_invoked,
    )
}

/// Verdict used by census and by the Ok(empty) falsifier. Never returns Pass for empty decode
/// or boxes-only geometry.
#[must_use]
pub fn census_verdict(
    decode: CensusDecode,
    placement_count: usize,
    real_nif_count: usize,
    fallback_count: usize,
    collision: CensusCollision,
) -> (CensusVerdict, Option<&'static str>) {
    if decode != CensusDecode::Ok {
        return (CensusVerdict::Fail, Some("decode_error"));
    }
    if placement_count == 0 {
        return (CensusVerdict::Fail, Some("ok_empty"));
    }
    if real_nif_count == 0 && fallback_count > 0 {
        return (CensusVerdict::Fail, Some("boxes_only"));
    }
    if real_nif_count == 0 {
        return (CensusVerdict::Fail, Some("no_real_nif"));
    }
    if fallback_count > 0 {
        return (CensusVerdict::Fail, Some("mixed_boxes"));
    }
    match collision {
        CensusCollision::SurfacesPresent => (CensusVerdict::Pass, None),
        CensusCollision::SurfacesEmpty => (CensusVerdict::Fail, Some("collision_empty")),
        CensusCollision::NotEvaluated => (CensusVerdict::Fail, Some("collision_not_evaluated")),
    }
}

/// Typed INT hook: census every primary `zones/` dungeon id (MEAS-010 denominator).
pub fn dungeon_census(client_root: &Path) -> std::io::Result<Vec<DungeonCensusRow>> {
    let ids = enumerate_dungeon_zone_ids(client_root)?;
    let mut cache = DungeonNifCache::new(client_root);
    let mut rows = Vec::with_capacity(ids.len());
    for zid in ids {
        rows.push(census_one_zone(client_root, zid, &mut cache));
    }
    Ok(rows)
}

/// Generated 400-row MEAS-010 denominator. Census rows are not playable coverage.
pub fn zone_denominator(client_root: &Path) -> io::Result<Vec<ZoneDenominatorRow>> {
    let entries = enumerate_primary_zone_classes(client_root)?;
    let mut cache = DungeonNifCache::new(client_root);
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        rows.push(denominator_one_zone(client_root, entry, &mut cache));
    }
    Ok(rows)
}

fn authored_members(client_root: &Path, zone_id: u16) -> (bool, bool, bool, bool) {
    let mut terrain = false;
    let mut dungeon = false;
    let mut skycity = false;
    let mut any_dat = false;
    if let Some(dat) = find_dat_mpk(client_root, zone_id) {
        any_dat = true;
        if let Ok(bytes) = std::fs::read(&dat) {
            if let Ok(names) = caer_assets::list_names_bytes(&bytes) {
                for n in names {
                    let l = n.to_ascii_lowercase();
                    match l.as_str() {
                        "terrain.pcx" => terrain = true,
                        "dungeon.chunk" | "dungeon.place" => dungeon = true,
                        "skycity.chunk" | "skycity.place" => skycity = true,
                        _ => {}
                    }
                }
            }
        }
    }
    (any_dat, terrain, dungeon, skycity)
}

fn denominator_one_zone(
    client_root: &Path,
    entry: PrimaryZoneEntry,
    cache: &mut DungeonNifCache,
) -> ZoneDenominatorRow {
    let region = caer_world::zone_region(entry.zone_id);
    let label = caer_world::zone_name(entry.zone_id);
    let (any_dat, has_terrain, has_dungeon, has_skycity) =
        authored_members(client_root, entry.zone_id);
    let has_authored_member = match entry.class {
        ZoneClass::Surface => has_terrain,
        ZoneClass::Dungeon => has_dungeon,
        ZoneClass::Skycity => has_skycity,
        ZoneClass::Interior => any_dat,
    };

    let (decode, placement_count, real_nif_count, fallback_count, collision) = match entry.class {
        ZoneClass::Dungeon => {
            let row = census_one_zone(client_root, entry.zone_id, cache);
            (
                row.decode,
                row.placement_count,
                row.real_nif_count,
                row.fallback_count,
                row.collision,
            )
        }
        ZoneClass::Surface => {
            if has_terrain {
                (CensusDecode::Ok, 1, 0, 0, CensusCollision::NotEvaluated)
            } else {
                (CensusDecode::Err, 0, 0, 0, CensusCollision::NotEvaluated)
            }
        }
        ZoneClass::Skycity | ZoneClass::Interior => {
            (CensusDecode::Ok, 0, 0, 0, CensusCollision::NotEvaluated)
        }
    };

    let (render_coverage, render_reason) = render_coverage_verdict(
        entry.class,
        decode,
        placement_count,
        real_nif_count,
        has_authored_member,
    );
    // TSV records the authored+server conjunction. Skip-append is a product-path falsifier,
    // not a missing archive — evaluate as append_invoked=true here.
    let (playable_transition, playable_reason) = playable_transition_verdict(
        entry.class,
        decode,
        placement_count,
        real_nif_count,
        fallback_count,
        collision,
        region,
        true,
    );

    ZoneDenominatorRow {
        zone: entry.zone_id,
        region,
        class: entry.class,
        label,
        has_authored_member,
        decode,
        placement_count,
        real_nif_count,
        fallback_count,
        collision,
        render_coverage,
        render_reason,
        playable_transition,
        playable_reason,
    }
}

fn verdict_cell(v: CensusVerdict) -> &'static str {
    match v {
        CensusVerdict::Pass => "PASS",
        CensusVerdict::Fail => "FAIL",
    }
}

fn decode_cell(d: CensusDecode) -> &'static str {
    match d {
        CensusDecode::Ok => "ok",
        CensusDecode::Err => "err",
    }
}

/// TSV text for the 400-row denominator. Does not claim census Pass as playable.
#[must_use]
pub fn format_zone_denominator_tsv(rows: &[ZoneDenominatorRow]) -> String {
    let mut surface = 0usize;
    let mut dungeon = 0usize;
    let mut skycity = 0usize;
    let mut interior = 0usize;
    let mut render_pass = 0usize;
    let mut playable_pass = 0usize;
    for r in rows {
        match r.class {
            ZoneClass::Surface => surface += 1,
            ZoneClass::Dungeon => dungeon += 1,
            ZoneClass::Skycity => skycity += 1,
            ZoneClass::Interior => interior += 1,
        }
        if r.render_pass() {
            render_pass += 1;
        }
        if r.playable_pass() {
            playable_pass += 1;
        }
    }
    let mut out = String::new();
    out.push_str("# CAER MEAS-010 zone denominator (generated; do not hand-edit)\n");
    out.push_str(&format!(
        "# partition surface={surface} dungeon={dungeon} skycity={skycity} interior={interior} total={}\n",
        rows.len()
    ));
    out.push_str(
        "# columns: render_coverage=client-authored; playable_transition=server Zones endpoint + usable geometry\n",
    );
    out.push_str(&format!(
        "# census is NOT playable coverage: render_pass={render_pass} playable_pass={playable_pass}\n"
    ));
    out.push_str(
        "zone\tregion\tclass\tlabel\trender_coverage\trender_reason\tplayable_transition\tplayable_reason\tdecode\tplacement_count\treal_nif_count\tfallback_count\n",
    );
    for r in rows {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.zone,
            r.region.map(|x| x.to_string()).unwrap_or_default(),
            r.class.as_str(),
            r.label.unwrap_or(""),
            verdict_cell(r.render_coverage),
            r.render_reason.unwrap_or(""),
            verdict_cell(r.playable_transition),
            r.playable_reason.unwrap_or(""),
            decode_cell(r.decode),
            r.placement_count,
            r.real_nif_count,
            r.fallback_count,
        ));
    }
    out
}

/// Write [`format_zone_denominator_tsv`] to `path`.
pub fn write_zone_denominator_tsv(path: &Path, rows: &[ZoneDenominatorRow]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = format_zone_denominator_tsv(rows);
    let mut f = std::fs::File::create(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

fn sample_collision(cache: &mut DungeonNifCache, stem: &str) -> CensusCollision {
    let Some(batch) = cache.template_for_stem(stem) else {
        return CensusCollision::NotEvaluated;
    };
    let mut mesh = TerrainMesh::default();
    let mut batch = batch.clone();
    batch.instances.push(ModelInstance {
        pos: [0.0, 0.0, 0.0],
        base_pos: [0.0, 0.0, 0.0],
        yaw: 0.0,
        base_yaw: 0.0,
        rot: [0.0, 0.0, 0.0, 1.0],
        base_rot: [0.0, 0.0, 0.0, 1.0],
        scale: 1.0,
        zone_id: 0,
        fixture_id: 0,
    });
    mesh.models.push(batch);
    mesh.surfaces = SurfaceIndex::build(&mesh);
    if mesh.surfaces.is_empty() {
        CensusCollision::SurfacesEmpty
    } else {
        CensusCollision::SurfacesPresent
    }
}

fn census_one_zone(
    client_root: &Path,
    zone_id: u16,
    cache: &mut DungeonNifCache,
) -> DungeonCensusRow {
    let region = caer_world::zone_region(zone_id);
    let label = caer_world::zone_name(zone_id);
    let realm_or_category = dungeon_realm_or_category(zone_id);
    let mut has_chunk = false;
    let mut has_place = false;
    let mut has_prop = false;
    if let Some(dat) = find_dat_mpk(client_root, zone_id) {
        if let Ok(bytes) = std::fs::read(&dat) {
            if let Ok(names) = caer_assets::list_names_bytes(&bytes) {
                for n in names {
                    let l = n.to_ascii_lowercase();
                    match l.as_str() {
                        "dungeon.chunk" => has_chunk = true,
                        "dungeon.place" => has_place = true,
                        "dungeon.prop" => has_prop = true,
                        _ => {}
                    }
                }
            }
        }
    }

    let (decode, placement_count, real_nif_count, fallback_count, collision) =
        match load_dungeon_zone(client_root, zone_id) {
            Ok(zone) => {
                let fixtures = zone.as_fixtures();
                let placement_count = fixtures.len();
                if placement_count == 0 {
                    (CensusDecode::Ok, 0, 0, 0, CensusCollision::NotEvaluated)
                } else {
                    let mut real_nif_count = 0usize;
                    let mut fallback_count = 0usize;
                    let mut sample_stem: Option<String> = None;
                    for f in &fixtures {
                        let stem = f.filename.to_ascii_lowercase();
                        let stem = stem.strip_suffix(".nif").unwrap_or(&stem).to_string();
                        if cache.template_for_stem(&stem).is_some() {
                            real_nif_count += 1;
                            if sample_stem.is_none() {
                                sample_stem = Some(stem);
                            }
                        } else {
                            fallback_count += 1;
                        }
                    }
                    let collision = match sample_stem {
                        Some(stem) => sample_collision(cache, &stem),
                        None => CensusCollision::NotEvaluated,
                    };
                    (
                        CensusDecode::Ok,
                        placement_count,
                        real_nif_count,
                        fallback_count,
                        collision,
                    )
                }
            }
            Err(_) => (CensusDecode::Err, 0, 0, 0, CensusCollision::NotEvaluated),
        };

    let (verdict, failure_reason) = census_verdict(
        decode,
        placement_count,
        real_nif_count,
        fallback_count,
        collision,
    );
    DungeonCensusRow {
        region,
        zone: zone_id,
        label,
        realm_or_category,
        has_chunk,
        has_place,
        has_prop,
        decode,
        placement_count,
        real_nif_count,
        fallback_count,
        collision,
        verdict,
        failure_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn census_ok_empty_cannot_pass() {
        let (v, reason) = census_verdict(CensusDecode::Ok, 0, 0, 0, CensusCollision::NotEvaluated);
        assert_eq!(v, CensusVerdict::Fail);
        assert_eq!(reason, Some("ok_empty"));
    }

    #[test]
    fn census_boxes_only_cannot_pass() {
        let (v, reason) =
            census_verdict(CensusDecode::Ok, 12, 0, 12, CensusCollision::NotEvaluated);
        assert_eq!(v, CensusVerdict::Fail);
        assert_eq!(reason, Some("boxes_only"));
    }

    #[test]
    fn census_real_nif_without_collision_cannot_pass() {
        let (v, reason) = census_verdict(CensusDecode::Ok, 4, 4, 0, CensusCollision::SurfacesEmpty);
        assert_eq!(v, CensusVerdict::Fail);
        assert_eq!(reason, Some("collision_empty"));
    }

    #[test]
    fn census_pass_requires_real_nif_and_surfaces() {
        let (v, reason) =
            census_verdict(CensusDecode::Ok, 8, 8, 0, CensusCollision::SurfacesPresent);
        assert_eq!(v, CensusVerdict::Pass);
        assert_eq!(reason, None);
    }

    #[test]
    fn census_mixed_boxes_cannot_pass() {
        let (v, reason) =
            census_verdict(CensusDecode::Ok, 8, 7, 1, CensusCollision::SurfacesPresent);
        assert_eq!(v, CensusVerdict::Fail);
        assert_eq!(reason, Some("mixed_boxes"));
    }

    #[test]
    fn render_coverage_allows_mixed_boxes_playable_does_not() {
        let (rv, _) = render_coverage_verdict(ZoneClass::Dungeon, CensusDecode::Ok, 8, 7, true);
        assert_eq!(rv, CensusVerdict::Pass);
        let (pv, reason) = playable_transition_verdict(
            ZoneClass::Dungeon,
            CensusDecode::Ok,
            8,
            7,
            1,
            CensusCollision::SurfacesPresent,
            Some(20),
            true,
        );
        assert_eq!(pv, CensusVerdict::Fail);
        assert_eq!(reason, Some("mixed_boxes"));
    }

    #[test]
    fn empty_decode_makes_playable_column_red() {
        let (pv, reason) = playable_transition_verdict(
            ZoneClass::Dungeon,
            CensusDecode::Ok,
            0,
            0,
            0,
            CensusCollision::NotEvaluated,
            Some(20),
            true,
        );
        assert_eq!(pv, CensusVerdict::Fail);
        assert_eq!(reason, Some("ok_empty"));
    }

    #[test]
    fn disable_append_makes_playable_column_red() {
        let (pv, reason) = playable_transition_verdict(
            ZoneClass::Dungeon,
            CensusDecode::Ok,
            8,
            8,
            0,
            CensusCollision::SurfacesPresent,
            Some(20),
            false,
        );
        assert_eq!(pv, CensusVerdict::Fail);
        assert_eq!(reason, Some("append_disabled"));
    }

    #[test]
    fn boxes_only_is_fail_for_render_and_playable() {
        let (rv, rreason) =
            render_coverage_verdict(ZoneClass::Dungeon, CensusDecode::Ok, 12, 0, true);
        assert_eq!(rv, CensusVerdict::Fail);
        assert_eq!(rreason, Some("boxes_only"));
        let (pv, preason) = playable_transition_verdict(
            ZoneClass::Dungeon,
            CensusDecode::Ok,
            12,
            0,
            12,
            CensusCollision::NotEvaluated,
            Some(21),
            true,
        );
        assert_eq!(pv, CensusVerdict::Fail);
        assert_eq!(preason, Some("boxes_only"));
    }
}
