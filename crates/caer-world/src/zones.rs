//! Zone grid → world-space coordinate reconstruction, for every zone in the game.
//!
//! DAoC's world is a grid of `ZONE_UNIT`-sided cells. A zone's `OffsetX`/`OffsetY` (from the
//! server's `Zones` table) are in **grid cells**; world coordinates are
//! `offset_cells * ZONE_UNIT + local`. ObjectUpdate (0xA1) carries zone-local coordinates plus
//! the zone id (low byte in the packet, 9th bit folded into the flags byte), so we look the
//! offset up and fold it back in — sharing one world frame with the entity/player creates
//! (which already carry world coordinates). Z is absolute in both.
//!
//! The full offset table (all 476 zones, all realms + dungeons) is generated from the DB into
//! [`crate::zone_offsets`]. Verified against the golden trace: NPC 12846 created at world
//! (561375, 509674); its ObjectUpdate carried zone-local (12511, 26346) in zone 0 (Camelot
//! Hills, offset 67/59) → `67*8192+12511`, `59*8192+26346`. Exact.

use crate::zone_offsets::ZONE_OFFSETS;

/// The side length of one zone grid cell, in world units.
pub const ZONE_UNIT: i32 = 8192;

/// Zone ids fit in 9 bits (max is < 512, checked against the DB), so a direct-indexed lookup
/// table gives O(1) offset resolution on the hot ingest path — no binary search per update.
const LUT_SIZE: usize = 512;

/// Sentinel offset for an unmapped zone id (no real zone has this offset).
const UNMAPPED: i16 = i16::MIN;

/// Compile-time direct-indexed table `(region, ox, oy)`, built from the generated
/// [`ZONE_OFFSETS`]. Offsets are only meaningful WITHIN a region — different regions reuse the
/// same grid-cell space, so world-space placement must always pair the offset with the region.
const OFFSET_LUT: [(u16, i16, i16); LUT_SIZE] = build_lut();

const fn build_lut() -> [(u16, i16, i16); LUT_SIZE] {
    let mut lut = [(0, UNMAPPED, UNMAPPED); LUT_SIZE];
    let mut i = 0;
    while i < ZONE_OFFSETS.len() {
        let (id, region, ox, oy) = ZONE_OFFSETS[i];
        if (id as usize) < LUT_SIZE {
            lut[id as usize] = (region, ox, oy);
        }
        i += 1;
    }
    lut
}

/// Look up a zone's grid offset by id in O(1). `None` for an unknown/unmapped id.
#[must_use]
fn offset_of(zone_id: u16) -> Option<(i32, i32)> {
    let (_, ox, oy) = OFFSET_LUT[(zone_id as usize) & (LUT_SIZE - 1)];
    if ox == UNMAPPED {
        None
    } else {
        Some((i32::from(ox), i32::from(oy)))
    }
}

/// The region a zone belongs to. `None` for an unknown/unmapped id.
#[must_use]
pub fn zone_region(zone_id: u16) -> Option<u16> {
    let (region, ox, _) = OFFSET_LUT[(zone_id as usize) & (LUT_SIZE - 1)];
    if ox == UNMAPPED {
        None
    } else {
        Some(region)
    }
}

/// A zone's grid-cell offset `(x, y)` — its lower corner in world space is
/// `(x * ZONE_UNIT, y * ZONE_UNIT)`. `None` for an unmapped zone id. Used by the renderer to
/// place per-zone terrain heightmaps in world coordinates.
#[must_use]
pub fn zone_grid_offset(zone_id: u16) -> Option<(i32, i32)> {
    offset_of(zone_id)
}

/// All zones of a region as `(zone_id, grid_offset_x, grid_offset_y)` — the renderer uses this
/// to cover a whole region when no population dump bounds the load.
#[must_use]
pub fn region_zone_offsets(region: u16) -> Vec<(u16, i32, i32)> {
    ZONE_OFFSETS
        .iter()
        .filter(|(_, r, _, _)| *r == region)
        .map(|&(id, _, ox, oy)| (id, i32::from(ox), i32::from(oy)))
        .collect()
}

/// The zone containing world position `(x, y)` in `region`, or `None` if the point lies in an
/// inter-zone gap. Region-1 frontier zones are staggered and can overlap, so among all zone
/// rectangles that contain the point we return the one whose centre is nearest — the intuitive
/// "which zone am I standing in" answer for the renderer's title readout.
#[must_use]
pub fn zone_at(region: u16, x: i32, y: i32) -> Option<u16> {
    let mut best: Option<(i64, u16)> = None;
    for (id, ox, oy) in region_zone_offsets(region) {
        let (x0, y0) = (ox * ZONE_UNIT, oy * ZONE_UNIT);
        if x < x0 || x >= x0 + 65_536 || y < y0 || y >= y0 + 65_536 {
            continue;
        }
        let (dx, dy) = (i64::from(x - (x0 + 32_768)), i64::from(y - (y0 + 32_768)));
        let d2 = dx * dx + dy * dy;
        if best.is_none_or(|(bd, _)| d2 < bd) {
            best = Some((d2, id));
        }
    }
    best.map(|(_, id)| id)
}

/// Reconstruct the full zone id from an ObjectUpdate's `zone` byte and `flags` byte. The server
/// writes only the low 8 bits in `zone`; the 9th bit (0x100) lives in `flags` bit 0x04
/// (oracle `SendObjectUpdate`: `flags |= (z.ZoneSkinID & 0x100) >> 6`).
#[must_use]
pub fn zone_id_from_packet(zone_byte: u8, flags: u8) -> u16 {
    u16::from(zone_byte) | (u16::from(flags & 0x04) << 6)
}

/// Reconstruct world coordinates from an ObjectUpdate's zone-local coordinates and its zone id.
/// An unknown zone falls back to treating the local coords as world coords (better than
/// silently placing the entity at the world origin).
#[must_use]
pub fn world_from_local(local_x: u16, local_y: u16, z: u16, zone_id: u16) -> [i32; 3] {
    match offset_of(zone_id) {
        Some((ox, oy)) => [
            ox * ZONE_UNIT + i32::from(local_x),
            oy * ZONE_UNIT + i32::from(local_y),
            i32::from(z),
        ],
        None => [i32::from(local_x), i32::from(local_y), i32::from(z)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_complete() {
        assert_eq!(ZONE_OFFSETS.len(), 476);
        assert!(
            ZONE_OFFSETS.windows(2).all(|w| w[0].0 < w[1].0),
            "must be sorted, unique ids"
        );
    }

    #[test]
    fn golden_reconstruction_camelot_hills() {
        // zone 0 = Camelot Hills (offset 67/59)
        assert_eq!(
            world_from_local(12511, 26346, 2445, 0),
            [561375, 509674, 2445]
        );
    }

    #[test]
    fn other_realm_zones_resolve() {
        // zone 13 = Caerwent (Midgard-side region 2, offset 64/64) — proves the whole table
        // is live, not just Albion's starter zone.
        assert_eq!(
            world_from_local(1000, 2000, 500, 13),
            [64 * 8192 + 1000, 64 * 8192 + 2000, 500]
        );
    }

    #[test]
    fn zone_at_locates_camera() {
        // Centre of Camelot Hills (offset 67/59) resolves to zone 0 by name.
        let (cx, cy) = (67 * ZONE_UNIT + 32_768, 59 * ZONE_UNIT + 32_768);
        assert_eq!(zone_at(1, cx, cy), Some(0));
        assert_eq!(crate::zone_name(0), Some("Camelot Hills"));
        // Snowdonia (offset 61/37) centre resolves to zone 12.
        assert_eq!(
            zone_at(1, 61 * ZONE_UNIT + 100, 37 * ZONE_UNIT + 100),
            Some(12)
        );
        // Far outside every region-1 rectangle → no zone.
        assert_eq!(zone_at(1, -5_000_000, -5_000_000), None);
    }

    #[test]
    fn packet_zone_id_reconstructs_high_bit() {
        assert_eq!(zone_id_from_packet(0x2A, 0x40), 0x2A); // flags bit 0x04 clear → low byte only
        assert_eq!(zone_id_from_packet(0x2A, 0x44), 0x12A); // 0x04 set → +0x100
    }

    #[test]
    fn unknown_zone_falls_back_to_local() {
        assert_eq!(world_from_local(100, 200, 50, 9999), [100, 200, 50]);
    }
}
