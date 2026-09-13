//! Player avatar (fig3) resolver — the `Self_`/white-box killer.
//!
//! A player body is not a single NIF (that's monsters, see [`crate::monsters`]). It is **assembled**
//! from body-part meshes chosen by the character's race + gender, through two tables in
//! `gamedata.mpk`:
//!
//! ```text
//!   (race, gender) ──fig3map desc──▶ figure id ──fig3map parts──▶ part-ids
//!                        part-id ──fig3parts──▶ (filename, archive) ──▶ figures/fig3/fig<NNN>.mpk : <filename>.nif
//! ```
//!
//! e.g. Highlander Male (Race 3, Gender 1) = figure 13; its Body part id 1083 → `Body01_Hig_m`
//! archive 2 → `figures/fig3/fig002.mpk : Body01_Hig_m.nif`. Full RE + verification:
//! `docs/AVATAR_RESOLUTION.md`. This resolver maps to part NIF *references*; loading + assembling
//! the meshes is the renderer's job.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// fig3parts column indices: `ID(Num), Text, Part(Filename), Expansion, Test, Archive, Double`.
const PARTS_FILENAME_COL: usize = 2;
const PARTS_ARCHIVE_COL: usize = 5;
/// fig3map column indices: `id, part, desc, 1, 2, … 25`. Variants start at col 3.
const MAP_PARTINDEX_COL: usize = 1;
const MAP_DESC_COL: usize = 2;
const MAP_VARIANTS_START: usize = 3;

/// The body-part indices that make up a naked base body: Head(0), Body(1), LBody(2), Legs(3),
/// Boots(4), Arms(5), Gloves(6), Hair(8). Boots/Gloves variant[0] is the BARE skin (feet/hands) —
/// without them the body has no hands or feet. Cloak(7)/Helm(9) are true equipment, added later.
const BASE_BODY_PARTS: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, PART_HAIR];

/// fig3map part index for a character's hairstyle mesh.
pub const PART_HAIR: u8 = 8;

/// fig3map part index for the cloak mesh (equipment-only).
pub const PART_CLOAK: u8 = 7;
/// fig3map part index for the full-helm mesh (equipment-only).
pub const PART_HELM: u8 = 9;

/// eGender (oracle `GlobalConstants.cs`): Male = 1, Female = 2.
pub const GENDER_MALE: u8 = 1;
pub const GENDER_FEMALE: u8 = 2;

/// One resolved body-part mesh: the NIF base name (lowercased, no extension) and the fig3 archive
/// number it lives in — i.e. `figures/fig3/fig{archive:03}.mpk : {filename}.nif`.
#[derive(Debug, Clone, PartialEq)]
pub struct PartRef {
    pub filename: String,
    pub archive: u16,
}

/// The result of resolving a shared fig3 skeleton through the same ordered archive search the
/// renderer uses. A member can appear more than once; an unreadable early copy is evidence, not a
/// reason to stop before a later parseable copy.
pub struct SkeletonLookup {
    pub skeleton: Option<crate::nif::Skeleton>,
    pub rejected_archives: Vec<(PathBuf, String)>,
}

/// The ordered `sfig*.mpk` archives that hold the shared fig3 skeletons.
///
/// This is intentionally the one archive predicate used by both avatar assembly and analysis
/// tools. A probe that scans a different surface can manufacture a missing-skeleton diagnosis the
/// renderer never takes.
pub fn skeleton_archives(client_root: impl AsRef<Path>) -> io::Result<Vec<PathBuf>> {
    let mut archives: Vec<PathBuf> = std::fs::read_dir(client_root.as_ref().join("figures/fig3"))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mpk"))
                && path.file_name().is_some_and(|name| {
                    name.to_string_lossy()
                        .to_ascii_lowercase()
                        .starts_with("sfig")
                })
        })
        .collect();
    archives.sort();
    Ok(archives)
}

/// Resolve `member` from the first archive copy that parses as a skeleton.
///
/// `open_member` errors continue to behave as the renderer's historical lookup did: an archive
/// that cannot be opened contributes no candidate. Parse failures are retained for diagnostics so
/// the caller can distinguish a missing member from an early bad copy followed by a good one.
pub fn first_parseable_skeleton(archives: &[PathBuf], member: &str) -> SkeletonLookup {
    let mut rejected_archives = Vec::new();
    for archive in archives {
        let Ok(Some(bytes)) = crate::open_member(archive, member) else {
            continue;
        };
        match crate::nif::read_skeleton(&bytes) {
            Ok(skeleton) => {
                return SkeletonLookup {
                    skeleton: Some(skeleton),
                    rejected_archives,
                };
            }
            Err(error) => rejected_archives.push((archive.clone(), error.to_string())),
        }
    }
    SkeletonLookup {
        skeleton: None,
        rejected_archives,
    }
}

impl PartRef {
    /// The archive path relative to the client root: `figures/fig3/figNNN.mpk`.
    pub fn archive_path(&self) -> String {
        format!("figures/fig3/fig{:03}.mpk", self.archive)
    }
    /// The NIF member name inside that archive: `<filename>.nif`.
    pub fn nif_member(&self) -> String {
        format!("{}.nif", self.filename)
    }
}

/// DAoC race id → the race name used in fig3map descriptions. The standard 1.12x race set; unknown
/// ids simply don't resolve (the caller keeps the placeholder box). Names are matched normalised
/// (lowercased, spaces stripped) so "Half Ogre" vs "HalfOgre" doesn't matter.
const RACE_NAMES: &[(u8, &str)] = &[
    (1, "Briton"),
    (2, "Avalonian"),
    (3, "Highlander"),
    (4, "Saracen"),
    (5, "Norse"),
    (6, "Troll"),
    (7, "Dwarf"),
    (8, "Kobold"),
    (9, "Celt"),
    (10, "Firbolg"),
    (11, "Elf"),
    (12, "Lurikeen"),
    (13, "Inconnu"),
    (14, "Valkyn"),
    (15, "Sylvan"),
    (16, "Half Ogre"),
    (17, "Frostalf"),
    (18, "Shar"),
    (19, "Minotaur"),
    (20, "Minotaur"),
    (21, "Minotaur"),
];

/// DAoC race id → the 3-letter token fig3 uses in asset file names. This is what names a race's
/// **skeleton**: `figures/fig3/sfigNNN.mpk : <token>_<m|f>_skeleton.nif` (e.g. Briton Male →
/// `bri_m_skeleton.nif`, 129 bones). All 18 playable races ship one; note Half Ogre is `hal`, not
/// the `hog` its display name suggests. Found by sweeping every fig3 archive for bone-bearing NIFs
/// (`rigscan`), which also turned up creature/mount rigs on the same convention (`bear_skeleton`,
/// `horse_medium_skeleton`, `mino_skeleton`) for later shapechange/mount work.
const RACE_TOKENS: &[(u8, &str)] = &[
    (1, "bri"),
    (2, "ava"),
    (3, "hig"),
    (4, "sar"),
    (5, "nor"),
    (6, "tro"),
    (7, "dwa"),
    (8, "kob"),
    (9, "cel"),
    (10, "fir"),
    (11, "elf"),
    (12, "lur"),
    (13, "inc"),
    (14, "val"),
    (15, "syl"),
    (16, "hal"),
    (17, "fro"),
    (18, "sha"),
    (19, "mino"),
    (20, "mino"),
    (21, "mino"),
];

/// Resolved fig3 tables: enough to turn a (race, gender) into the list of base-body part meshes.
pub struct FigureModels {
    /// figure id → (body-part index → first-variant part id).
    parts_by_fig: HashMap<u16, HashMap<u8, u16>>,
    /// figure id → part index → every populated source variant `(1-based index, part id)`.
    ///
    /// The base assembler uses the first populated choice, but character customisation must retain
    /// the non-contiguous variant indices exactly: `fig3map` can have `Hair 1..6`, an empty 7
    /// (Bald), then `Hair 8`.  Compressing that list makes an on-screen label select a different
    /// retail mesh.
    variants_by_fig: HashMap<u16, HashMap<u8, Vec<(u8, u16)>>>,
    /// part id → (filename lowercased, archive number).
    part_file: HashMap<u16, (String, u16)>,
    /// (normalised race name, gender) → figure id.
    fig_by_race_gender: HashMap<(String, u8), u16>,
}

impl FigureModels {
    /// Load and join `fig3map.csv` + `fig3parts.csv` out of `gamedata.mpk`.
    pub fn load(gamedata_mpk: impl AsRef<Path>) -> io::Result<Self> {
        let members = crate::open(gamedata_mpk)?;
        let find = |name: &str| -> io::Result<String> {
            members
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(name))
                .map(|m| String::from_utf8_lossy(&m.data).into_owned())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("{name} not in gamedata.mpk"),
                    )
                })
        };
        Ok(Self::from_tables(
            &find("fig3map.csv")?,
            &find("fig3parts.csv")?,
        ))
    }

    /// Build the resolver from the two CSV bodies (split out so it's unit-testable without an mpk).
    pub fn from_tables(fig3map: &str, fig3parts: &str) -> Self {
        // fig3parts: part id → (filename, archive).
        let mut part_file: HashMap<u16, (String, u16)> = HashMap::new();
        for (id, cols) in numeric_rows(fig3parts) {
            let file = cols.get(PARTS_FILENAME_COL).map(|s| s.trim());
            let arch = cols
                .get(PARTS_ARCHIVE_COL)
                .and_then(|s| s.trim().parse::<u16>().ok());
            if let (Some(file), Some(arch)) = (file, arch) {
                if !file.is_empty() {
                    // FIRST row wins. 12 part ids are claimed twice, and the second claim is
                    // always a degenerate block that repeats one filename across a run of ids:
                    // 189-194 are Inconnu Female Cloak02/03 and Hair01-04 in the first block and
                    // six copies of `bri_f_Hair02` in the second. `fig3map` points Inconnu
                    // Female Hair at exactly 191-194, and only the first block's descriptions
                    // agree with that row. Last-wins put a Briton cap on an Inconnu skull, and
                    // since her skull is taller it swallowed the cap and she rendered bald.
                    //
                    // Description agreement is the reason first-wins is right *here*, not a
                    // general invariant: across all 1,557 first-variant bindings the two tables'
                    // descriptions disagree 1,238 times even when resolution is correct, so a
                    // "descriptions must match" gate would be noise. The gate is the two
                    // characters instead.
                    part_file
                        .entry(id)
                        .or_insert_with(|| (file.to_ascii_lowercase(), arch));
                }
            }
        }

        // fig3map: figure id → part index → first non-empty variant AND every populated source
        // variant; plus (race, gender) from the part-0 (Head) row's description
        // ("<Race> <Gender> Head").
        let mut parts_by_fig: HashMap<u16, HashMap<u8, u16>> = HashMap::new();
        let mut variants_by_fig: HashMap<u16, HashMap<u8, Vec<(u8, u16)>>> = HashMap::new();
        let mut fig_by_race_gender: HashMap<(String, u8), u16> = HashMap::new();
        for (fig_id, cols) in numeric_rows(fig3map) {
            let Some(part_idx) = cols
                .get(MAP_PARTINDEX_COL)
                .and_then(|s| s.trim().parse::<u8>().ok())
            else {
                continue;
            };
            let variants: Vec<(u8, u16)> = cols
                .iter()
                .skip(MAP_VARIANTS_START)
                .enumerate()
                .filter_map(|(offset, raw)| {
                    raw.trim()
                        .parse::<u16>()
                        .ok()
                        .filter(|id| *id != 0)
                        .map(|id| (u8::try_from(offset + 1).unwrap_or(u8::MAX), id))
                })
                .collect();
            if let Some((_, pid)) = variants.first() {
                parts_by_fig
                    .entry(fig_id)
                    .or_default()
                    .insert(part_idx, *pid);
            }
            if !variants.is_empty() {
                variants_by_fig
                    .entry(fig_id)
                    .or_default()
                    .insert(part_idx, variants);
            }
            if part_idx == 0 {
                if let Some(desc) = cols.get(MAP_DESC_COL) {
                    if let Some((race, gender)) = parse_race_gender(desc.trim()) {
                        // Some races have duplicate figure rows (a base + a later remodel); keep the
                        // FIRST (lowest id = the canonical base body) so resolution is deterministic.
                        fig_by_race_gender.entry((race, gender)).or_insert(fig_id);
                    }
                }
            }
        }
        Self {
            parts_by_fig,
            variants_by_fig,
            part_file,
            fig_by_race_gender,
        }
    }

    /// The display name of a race id ("Briton", "Half Ogre"), if known. Exposed so the animation
    /// tables can be keyed by race+gender — `anims.csv` rows are named that way ("Briton Male").
    pub fn race_name(race: u8) -> Option<&'static str> {
        RACE_NAMES
            .iter()
            .find(|(id, _)| *id == race)
            .map(|(_, n)| *n)
    }

    /// The short token this race's fig3 assets are named with.
    ///
    /// One token spans meshes, skeletons and skins: `bri_m_head01.nif`, `bri_m_skeleton.nif`,
    /// `cthBody01_SS_Bri_M.dds`. Exposed so the material binder can name the starting set the way
    /// `mskins.csv` does instead of guessing at file names.
    #[must_use]
    pub fn race_token(race: u8) -> Option<&'static str> {
        RACE_TOKENS
            .iter()
            .find(|(id, _)| *id == race)
            .map(|(_, t)| *t)
    }

    /// The `sfigNNN.mpk` member name of a race+gender's skeleton NIF — the external rig its fig3
    /// body parts bind their bone names against. See [`RACE_TOKENS`].
    pub fn skeleton_member(race: u8, gender: u8) -> Option<String> {
        let token = RACE_TOKENS
            .iter()
            .find(|(id, _)| *id == race)
            .map(|(_, t)| *t)?;
        let g = if gender == GENDER_FEMALE { 'f' } else { 'm' };
        Some(format!("{token}_{g}_skeleton.nif"))
    }

    /// The figure id for a race id + gender, if the race is known and present in the tables.
    pub fn figure_id(&self, race: u8, gender: u8) -> Option<u16> {
        let name = Self::race_name(race)?;
        let key = normalise(name);
        self.fig_by_race_gender
            .get(&(key.clone(), gender))
            .copied()
            .or_else(|| {
                // LotM names in fig3map may be Korazh/Deifrang/Graoch rather than Minotaur.
                for alias in race_fig3_aliases(race) {
                    if let Some(id) = self.fig_by_race_gender.get(&(normalise(alias), gender)) {
                        return Some(*id);
                    }
                }
                None
            })
    }

    /// The naked base-body part meshes for a race + gender, in assembly order (Head, Body, LBody,
    /// Legs, Arms, Hair). Empty if the race/gender or its parts don't resolve.
    pub fn base_body(&self, race: u8, gender: u8) -> Vec<PartRef> {
        let Some(fig) = self.figure_id(race, gender) else {
            return Vec::new();
        };
        let Some(parts) = self.parts_by_fig.get(&fig) else {
            return Vec::new();
        };
        BASE_BODY_PARTS
            .iter()
            .filter_map(|idx| parts.get(idx))
            .filter_map(|pid| self.part_file.get(pid))
            .map(|(filename, archive)| PartRef {
                filename: filename.clone(),
                archive: *archive,
            })
            .collect()
    }

    /// One fig3 part mesh by map index (e.g. [`PART_HELM`], [`PART_CLOAK`]) — first variant.
    /// Used when equipment adds helm/cloak meshes that are not in the naked base body.
    /// Which fig3 archive the client's own index puts this mesh in, when it names exactly one.
    ///
    /// The base-part path never needs this — it resolves part id → (filename, archive) and always
    /// had the archive. Anything that arrives holding only a *name*, like the hairstyle override,
    /// otherwise falls back to probing archives in sorted order and taking the first hit. That is
    /// not a tie-break: 65 fig3 meshes ship with genuinely different geometry under one name, and
    /// on 54 of the 61 the index can decide, first-sorted lands on the wrong shape. `cel_f_hair04`
    /// is a 8-unit cap in `fig008` and 42x15x38 spanning z 35..73 — long hair down the back — in
    /// `fig009`, which is the one the index names.
    ///
    /// `None` when the index is silent or names several, because 30 filenames (mostly heads) are
    /// carried by more than one part id at different archives. A name alone genuinely cannot
    /// decide those, and guessing is what this exists to stop.
    #[must_use]
    pub fn archive_for_filename(&self, filename: &str) -> Option<u16> {
        let want = filename
            .trim_end_matches(".nif")
            .trim_end_matches(".NIF")
            .to_ascii_lowercase();
        let mut found: Option<u16> = None;
        for (name, archive) in self.part_file.values() {
            if *name != want {
                continue;
            }
            match found {
                Some(a) if a != *archive => return None,
                _ => found = Some(*archive),
            }
        }
        found
    }

    #[must_use]
    pub fn part(&self, race: u8, gender: u8, part_idx: u8) -> Option<PartRef> {
        let fig = self.figure_id(race, gender)?;
        let pid = *self.parts_by_fig.get(&fig)?.get(&part_idx)?;
        let (filename, archive) = self.part_file.get(&pid)?;
        Some(PartRef {
            filename: filename.clone(),
            archive: *archive,
        })
    }

    /// Every populated source variant for one fig3 part, with its original 1-based table index.
    ///
    /// `PART_HAIR` is the character-customisation use case, but the API intentionally takes a
    /// part index because helmets, cloaks and future equipment selectors have the same authoring
    /// shape.  It resolves the part id through the same first-wins `fig3parts` rule as
    /// [`Self::base_body`] and [`Self::part`].
    #[must_use]
    pub fn part_variants(&self, race: u8, gender: u8, part_idx: u8) -> Vec<(u8, PartRef)> {
        let Some(fig) = self.figure_id(race, gender) else {
            return Vec::new();
        };
        let Some(variants) = self
            .variants_by_fig
            .get(&fig)
            .and_then(|parts| parts.get(&part_idx))
        else {
            return Vec::new();
        };
        variants
            .iter()
            .filter_map(|(index, part_id)| {
                self.part_file.get(part_id).map(|(filename, archive)| {
                    (
                        *index,
                        PartRef {
                            filename: filename.clone(),
                            archive: *archive,
                        },
                    )
                })
            })
            .collect()
    }
}

fn race_fig3_aliases(race: u8) -> &'static [&'static str] {
    match race {
        19 => &["Korazh", "Albion Minotaur", "Minotaur Male Alb"],
        20 => &["Deifrang", "Midgard Minotaur", "Minotaur Male Mid"],
        21 => &["Graoch", "Hibernia Minotaur", "Minotaur Male Hib"],
        _ => &[],
    }
}

/// Parse a fig3map Head-row description ("Highlander Male Head", "Half Ogre Female Head") into
/// (normalised race name, gender). Returns `None` for variant figures (Stone/Shade/…) whose race
/// won't match a known name, or malformed descriptions.
fn parse_race_gender(desc: &str) -> Option<(String, u8)> {
    let tokens: Vec<&str> = desc.split_whitespace().collect();
    let gpos = tokens
        .iter()
        .position(|t| t.eq_ignore_ascii_case("Male") || t.eq_ignore_ascii_case("Female"))?;
    if gpos == 0 {
        return None; // "Female Briton Full" oddballs — ignore (the standard rows are Race-first)
    }
    let gender = if tokens[gpos].eq_ignore_ascii_case("Male") {
        GENDER_MALE
    } else {
        GENDER_FEMALE
    };
    let race = tokens[..gpos].join(" ");
    Some((normalise(&race), gender))
}

/// Case-insensitive, space-insensitive key ("Half Ogre" == "halfogre").
fn normalise(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Iterate a CSV's data rows whose first column is a `u16`, yielding `(id, columns)`. Header rows
/// (non-numeric first column) are skipped; fields are split on commas (DAoC's tables don't quote).
fn numeric_rows(csv: &str) -> impl Iterator<Item = (u16, Vec<&str>)> {
    csv.lines().filter_map(|line| {
        let cols: Vec<&str> = line.split(',').collect();
        let id = cols.first()?.trim().parse::<u16>().ok()?;
        Some((id, cols))
    })
}

#[cfg(test)]
mod tests {
    /// **Ledger E4.** 12 part ids in `fig3parts.csv` are claimed by two rows, and the second claim
    /// is a degenerate block that repeats one filename across a run of ids. Reading the file into
    /// a map with `insert` let the second block win, so `fig3map`'s "Inconnu Female Hair" row,
    /// which points at ids 191-194, resolved to `bri_f_Hair02` instead of `inc_f_Hair01..04`.
    ///
    /// A Briton cap on an Inconnu skull is not merely the wrong hairstyle: her crown is taller, so
    /// the head swallowed the cap and she rendered **completely bald** — 0 hair pixels against a
    /// population of 68-548. Half Ogre male was the same defect via ids 195-198 and rendered 14.
    ///
    /// The synthetic half is the mechanism and needs no client; the client half is the two
    /// characters a player would have seen. Seen red by restoring `insert`, which returns
    /// `bri_f_hair02` and `hig_m_hair03`.
    #[test]
    fn a_part_id_claimed_twice_keeps_its_first_meaning() {
        // desc, part, archive columns are 1, 2 and 5; variants start at column 3 in fig3map.
        let parts = "ID,Text,Part,Expansion,Test,Archive,Double\n\
                     Num,Desc,Filename,Min,Status,,Sided\n\
                     191,Inconnu Female Hair,inc_f_Hair01,4,1,9,1\n\
                     191,Briton Female Hair,bri_f_Hair02,4,1,10,1\n";
        let map = "id,,,,\n#,part,desc,1,2\n24,8,Inconnu Female Hair,191,\n\
                   24,0,Inconnu Female Head,191,\n";
        let f = FigureModels::from_tables(map, parts);
        let hair = f
            .part(13, GENDER_FEMALE, 8)
            .expect("fig3map binds Inconnu female hair");
        assert_eq!(
            hair.filename, "inc_f_hair01",
            "the first row that claims a part id is the one fig3map's description agrees with"
        );

        let Some(root) = crate::client_dep::require_caer_client("a_part_id_claimed_twice") else {
            return;
        };
        let real = FigureModels::load(root.join("gamedata.mpk")).expect("gamedata.mpk");
        for (race, gender, want) in [
            (13u8, GENDER_FEMALE, "inc_f_hair01"),
            (16u8, GENDER_MALE, "hal_m_hair01"),
        ] {
            let p = real
                .part(race, gender, 8)
                .unwrap_or_else(|| panic!("race {race} gender {gender} authors a hair part"));
            assert_eq!(
                p.filename, want,
                "race {race} gender {gender} must wear its own hair, not the colliding id's"
            );
        }
    }

    use super::*;

    // Minimal stand-ins for the two tables, shaped like the real ones (two header rows + data).
    const FIG3MAP: &str = "id,,,,\n\
        #,part,desc,1,2,3\n\
        13,0,Highlander Male Head,1289,,\n\
        13,1,Highlander Male Body,1083,1084,\n\
        13,2,Highlander Male LBody,1099,,\n\
        13,3,Highlander Male Legs,563,,\n\
        13,5,Highlander Male Arms,570,,\n\
        13,8,Highlander Male Hair,102,103,\n\
        82,0,Stone Highlander Male Head,1289,,\n";
    const FIG3PARTS: &str = "ID,Text,Part,Expansion,Test,Archive,Double\n\
        Num,Desc,Filename,Min,Status,,Sided\n\
        1289,Hig_m_Head01,Hig_m_Head01,4,1,15,\n\
        1083,Body,Body01_Hig_m,4,1,2,\n\
        1099,LBody,LBody01_Hig_m,4,1,4,\n\
        563,Legs,Legs01_HN_m,4,1,4,\n\
        570,Arms,Arms01_HN_m,4,1,1,\n\
        102,Hair,hig_m_Hair01,4,1,15,1\n";

    #[test]
    fn hair_variants_keep_their_source_indices_including_a_bald_hole() {
        let map = "id,,,,,,\n\
                   #,part,desc,1,2,3,4\n\
                   13,0,Highlander Male Head,1,,,\n\
                   13,8,Highlander Male Hair,10,11,,13\n";
        let parts = "ID,Text,Part,Expansion,Test,Archive,Double\n\
                     Num,Desc,Filename,Min,Status,,Sided\n\
                     1,Head,hig_m_head01,4,1,15,1\n\
                     10,Hair,hig_m_hair01,4,1,15,1\n\
                     11,Hair,hig_m_hair02,4,1,15,1\n\
                     13,Hair,hig_m_hair04,4,1,15,1\n";
        let figures = FigureModels::from_tables(map, parts);
        let variants = figures.part_variants(3, GENDER_MALE, PART_HAIR);
        assert_eq!(
            variants
                .iter()
                .map(|(index, part)| (*index, part.filename.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (1, "hig_m_hair01"),
                (2, "hig_m_hair02"),
                (4, "hig_m_hair04")
            ],
            "an empty source third variant is a deliberate bald hole, not a reason to compact \
             Hair 4 to source index 3"
        );
    }

    #[test]
    fn resolves_highlander_male_base_body() {
        let f = FigureModels::from_tables(FIG3MAP, FIG3PARTS);
        // Race 3 = Highlander, Gender 1 = Male → figure 13.
        assert_eq!(f.figure_id(3, GENDER_MALE), Some(13));
        let body = f.base_body(3, GENDER_MALE);
        // Six base parts, in order, each with the right archive.
        assert_eq!(body.len(), 6);
        assert_eq!(
            body[0],
            PartRef {
                filename: "hig_m_head01".into(),
                archive: 15
            }
        ); // Head
        assert_eq!(
            body[1],
            PartRef {
                filename: "body01_hig_m".into(),
                archive: 2
            }
        ); // Body (variant0)
        assert_eq!(
            body[4],
            PartRef {
                filename: "arms01_hn_m".into(),
                archive: 1
            }
        ); // Arms
        assert_eq!(body[0].archive_path(), "figures/fig3/fig015.mpk");
        assert_eq!(body[0].nif_member(), "hig_m_head01.nif");
    }

    #[test]
    fn equipment_helm_and_cloak_parts_resolve() {
        // Extend the fixture with cloak(7)/helm(9) rows so PART_* lookups work.
        let map = concat!(
            "id,,,,\n",
            "#,part,desc,1,2,3\n",
            "13,0,Highlander Male Head,1289,,\n",
            "13,1,Highlander Male Body,1083,1084,\n",
            "13,2,Highlander Male LBody,1099,,\n",
            "13,3,Highlander Male Legs,563,,\n",
            "13,5,Highlander Male Arms,570,,\n",
            "13,7,Highlander Male Cloak,200,,\n",
            "13,8,Highlander Male Hair,102,103,\n",
            "13,9,Highlander Male Full Helm,201,,\n",
        );
        let parts = concat!(
            "ID,Text,Part,Expansion,Test,Archive,Double\n",
            "Num,Desc,Filename,Min,Status,,Sided\n",
            "1289,Hig_m_Head01,Hig_m_Head01,4,1,15,\n",
            "1083,Body,Body01_Hig_m,4,1,2,\n",
            "1099,LBody,LBody01_Hig_m,4,1,4,\n",
            "563,Legs,Legs01_HN_m,4,1,4,\n",
            "570,Arms,Arms01_HN_m,4,1,1,\n",
            "102,Hair,hig_m_Hair01,4,1,15,1\n",
            "200,Cloak,Cloak01_hig_m,4,1,10,\n",
            "201,Helm,Helm01_hig_m,4,1,22,\n",
        );
        let f = FigureModels::from_tables(map, parts);
        assert_eq!(
            f.part(3, GENDER_MALE, PART_CLOAK),
            Some(PartRef {
                filename: "cloak01_hig_m".into(),
                archive: 10
            })
        );
        assert_eq!(
            f.part(3, GENDER_MALE, PART_HELM),
            Some(PartRef {
                filename: "helm01_hig_m".into(),
                archive: 22
            })
        );
    }

    #[test]
    fn skeleton_member_names_every_playable_race() {
        // The rig each race's fig3 parts bind to. Half Ogre is the trap: token `hal`, not `hog`.
        assert_eq!(
            FigureModels::skeleton_member(1, GENDER_MALE).as_deref(),
            Some("bri_m_skeleton.nif")
        );
        assert_eq!(
            FigureModels::skeleton_member(1, GENDER_FEMALE).as_deref(),
            Some("bri_f_skeleton.nif")
        );
        assert_eq!(
            FigureModels::skeleton_member(16, GENDER_FEMALE).as_deref(),
            Some("hal_f_skeleton.nif")
        );
        assert_eq!(FigureModels::skeleton_member(99, GENDER_MALE), None);
        // All 18 playable races must map, and to distinct tokens.
        let names: Vec<String> = (1..=18)
            .filter_map(|r| FigureModels::skeleton_member(r, GENDER_MALE))
            .collect();
        assert_eq!(names.len(), 18);
        let unique: std::collections::HashSet<&String> = names.iter().collect();
        assert_eq!(unique.len(), 18, "race tokens must be distinct");
    }

    #[test]
    fn every_race_skeleton_actually_ships_in_the_client() {
        // Client-gated. This is the encoded form of the
        // `rigscan` sweep that originally located these files: the tool was deleted, so this test
        // is what keeps the finding honest. If a token is ever wrong (Half Ogre is `hal`, not the
        // `hog` its display name suggests), this fails instead of the avatar silently T-posing.
        let test = "figures::every_race_skeleton_actually_ships_in_the_client";
        let Some(root) = crate::client_dep::require_caer_client(test) else {
            return;
        };
        let archives = skeleton_archives(root.as_ref()).unwrap_or_else(|error| {
            panic!("REQ-025: {test} requires fig3 skeleton archives: {error}")
        });
        assert!(!archives.is_empty(), "no sfig* skeleton archives found");
        for race in 1..=18u8 {
            for gender in [GENDER_MALE, GENDER_FEMALE] {
                let want = FigureModels::skeleton_member(race, gender).expect("token");
                let found = archives.iter().any(|a| {
                    crate::open(a)
                        .unwrap_or_else(|error| {
                            panic!("REQ-025: {test} requires {}: {error}", a.display())
                        })
                        .iter()
                        .any(|member| member.name.eq_ignore_ascii_case(&want))
                });
                assert!(
                    found,
                    "race {race} gender {gender}: {want} not in any sfig archive"
                );
            }
        }
    }

    #[test]
    fn minotaur_figure_id_matches_plain_or_lotm_name() {
        let map = "id,,,,\n\
            #,part,desc,1\n\
            90,0,Minotaur Male Head,1\n\
            91,0,Korazh Male Head,1\n";
        let parts = "ID,Text,Part,Expansion,Test,Archive,Double\n\
            Num,Desc,Filename,Min,Status,,Sided\n\
            1,Head,mino_m_head,4,1,1,\n";
        let f = FigureModels::from_tables(map, parts);
        assert_eq!(f.figure_id(19, GENDER_MALE), Some(90));
        assert_eq!(f.figure_id(20, GENDER_MALE), Some(90));
        let f2 =
            FigureModels::from_tables("id,,,,\n#,part,desc,1\n91,0,Korazh Male Head,1\n", parts);
        assert_eq!(f2.figure_id(19, GENDER_MALE), Some(91));
        assert_eq!(
            FigureModels::skeleton_member(19, GENDER_MALE).as_deref(),
            Some("mino_m_skeleton.nif")
        );
    }

    #[test]
    fn stone_variant_and_unknown_race_do_not_resolve() {
        let f = FigureModels::from_tables(FIG3MAP, FIG3PARTS);
        // "Stone Highlander" (figure 82) is a variant — its race name isn't in RACE_NAMES.
        assert!(f
            .fig_by_race_gender
            .contains_key(&(normalise("Stone Highlander"), GENDER_MALE)));
        // …but it's unreachable via a real race id, and unknown races return nothing.
        assert_eq!(f.figure_id(99, GENDER_MALE), None);
        assert!(f.base_body(99, GENDER_MALE).is_empty());
    }
}
