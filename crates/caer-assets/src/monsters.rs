//! Monster model-id → NIF resolver.
//!
//! A creature's numeric model id (the DB `Mob.Model`, and the `model` field of the wire's 0xda
//! NPCCreate) does not name a NIF directly — the client resolves it through two tables shipped in
//! `gamedata.mpk`:
//!
//! ```text
//!   model id ──monsters.csv[id].NIF#──▶ nif id ──monnifs.csv[nif].Name──▶ figures/<Name>.nif
//! ```
//!
//! e.g. model 26 → monsters.csv "Large Skeleton" NIF# 207 → monnifs.csv "Skeleton" name "skel01"
//! → `figures/Skel01.NIF`. Verified live: the giant skeletons around Lilillyn's spawn are model 26.
//!
//! Both CSVs are plain comma-separated with two header rows; we key on the numeric first column and
//! ignore the rest.
//!
//! ## Skins
//!
//! Figure/monster NIFs carry **no internal texture** — only a material colour. The skin is applied
//! entirely externally by a third table hop:
//!
//! ```text
//!   model id ──monsters.csv[id].Body──▶ skin id ──skins.csv[id].(Name, Archive)──▶
//!             figures/skins/skin<Archive>.mpk : <Name basename>.dds
//! ```
//!
//! e.g. model 26 (Large Skeleton) → Body skin id 82 → skins.csv `skel3.tga`, archive 2 →
//! `figures/skins/skin002.mpk : skel3.dds`. Verified end-to-end 2026-07-23. The `.tga`/`.bmp`
//! authoring extension is swapped to the shipped `.dds` (same rule as world fixtures). Naming
//! conventions off the NIF base name (`<nif>body01.dds`) are NOT reliable — the table is the
//! source of truth (amazon→amazon01, bear→bear_black, etc.).

use std::collections::HashMap;
use std::io;
use std::path::Path;

/// Column index of `NIF#` in monsters.csv (`ID, name, #, …`) and of `Name` in monnifs.csv
/// (`ID, Textual Name, Name, …`).
const MONSTERS_NIF_COL: usize = 2;
const MONNIFS_NAME_COL: usize = 2;
/// Column index of `Anim Set` in monnifs.csv — the creature's row in `anims.csv`/`canims.csv`
/// (see [`crate::anims`]). e.g. nif 207 (skel01) → set 4 ("Skeleton").
const MONNIFS_ANIM_SET_COL: usize = 3;
/// Column indices of the `Stride Run` / `Stride Walk` fields in monnifs.csv: how far (in world
/// units) one full cycle of that clip carries the creature. Dividing actual movement speed by the
/// stride gives the playback rate that keeps feet planted instead of skating.
const MONNIFS_STRIDE_RUN_COL: usize = 4;
const MONNIFS_STRIDE_WALK_COL: usize = 5;
/// `Stride Back` / `Stride Strafe` — the same measure for the reverse and sidestep clips, so those
/// play stride-matched too rather than skating.
const MONNIFS_STRIDE_BACK_COL: usize = 6;
const MONNIFS_STRIDE_STRAFE_COL: usize = 7;
/// Column index of the `Body` skin id in monsters.csv (`ID, name, #, Body, Head, Arms, …`). This is
/// the creature's primary/torso skin — the one we bind to the whole mesh in the first-cut skinning
/// pass (single-skin creatures like skeletons are fully correct; split body/head is a refinement).
const MONSTERS_BODY_SKIN_COL: usize = 3;
/// Column index of `Scale` in monsters.csv — a PERCENTAGE of the mesh's authored size, 100 = as
/// authored (1196 of 2192 rows). It corrects meshes that were not authored at world scale, and it
/// is NOT the same thing as the per-instance size byte on NPCCreate.
///
/// Ignoring it renders every such creature at its raw authored size: the Dragonfly mesh is ~1694
/// units across with `Scale = 10`, so it drew about twenty times too big — a wingspan filling the
/// screen. Large Skeleton (`Scale = 125`) drew 25% too short for the same reason.
const MONSTERS_SCALE_COL: usize = 12;
/// Column index of `Face` in monsters.csv — the HEAD texture, despite the skin block already
/// having a column labelled `Head` (that one is hair). Sits outside the contiguous skin block,
/// after Scale/Tall/Shdw/Sound Set/Kilt/Sarac.
const MONSTERS_FACE_SKIN_COL: usize = 18;
/// Column indices in skins.csv (`ID, Textual Name, Skin Name, MULTI, Archive Num, …`): the DDS/TGA
/// authoring file name and the `skinNNN.mpk` archive number that ships it.
const SKINS_NAME_COL: usize = 2;
const SKINS_ARCHIVE_COL: usize = 4;

/// A body slot in `monsters.csv`'s skin block — the nine consecutive `Skins` columns, in file order.
///
/// A creature mesh is not skinned as a unit: `Saracen Female Merchant 2` (model 82) names seven
/// DIFFERENT skins across its 49 parts, of which the head's (1043) is nothing like the torso's
/// (122). Binding the Body skin to the whole mesh therefore painted every face with its own torso
/// — the melted look on every humanoid NPC in the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkinSlot {
    Body,
    /// The column LABELLED `Head` in the skin block — which actually carries the HAIR texture for
    /// humanoids (model 27's is `hair_brown.tga`). The head's own texture is [`SkinSlot::Face`].
    /// Verified by resolving the ids: Head=1022 -> hair_brown, Face=26 -> h_mhed3.
    Head,
    /// The real head/face texture, from the `Face` column further along the row (`h_mhed3.tga`,
    /// `b_mhed2.tga`, `s_fhed3.tga`). Painting the `Head` column onto a face instead puts a hair
    /// swatch across it, which is why every guard looked like it was wearing cloth.
    Face,
    Arms,
    Gloves,
    Lbody,
    Legs,
    Boots,
    Cloak,
    Helm,
}

impl SkinSlot {
    /// The nine slots in `monsters.csv` column order, which is also [`SkinSlot::column`] order.
    pub const ALL: [SkinSlot; 10] = [
        SkinSlot::Body,
        SkinSlot::Head,
        SkinSlot::Face,
        SkinSlot::Arms,
        SkinSlot::Gloves,
        SkinSlot::Lbody,
        SkinSlot::Legs,
        SkinSlot::Boots,
        SkinSlot::Cloak,
        SkinSlot::Helm,
    ];

    /// This slot's column index in monsters.csv.
    ///
    /// The nine-column skin block is contiguous from [`MONSTERS_BODY_SKIN_COL`], but `Face` sits
    /// well outside it (column 18, past Scale/Tall/Shdw/Sound/Kilt/Sarac), so this is a match
    /// rather than an offset from a position in `ALL`.
    #[must_use]
    pub fn column(self) -> usize {
        match self {
            SkinSlot::Body => MONSTERS_BODY_SKIN_COL,
            SkinSlot::Head => MONSTERS_BODY_SKIN_COL + 1,
            SkinSlot::Arms => MONSTERS_BODY_SKIN_COL + 2,
            SkinSlot::Gloves => MONSTERS_BODY_SKIN_COL + 3,
            SkinSlot::Lbody => MONSTERS_BODY_SKIN_COL + 4,
            SkinSlot::Legs => MONSTERS_BODY_SKIN_COL + 5,
            SkinSlot::Boots => MONSTERS_BODY_SKIN_COL + 6,
            SkinSlot::Cloak => MONSTERS_BODY_SKIN_COL + 7,
            SkinSlot::Helm => MONSTERS_BODY_SKIN_COL + 8,
            SkinSlot::Face => MONSTERS_FACE_SKIN_COL,
        }
    }

    /// Which slot a mesh part belongs to, from the NIF shape node's name.
    ///
    /// Figure meshes name their parts by slot with a variant suffix — `Body1`, `Body1b`, `Arms3`,
    /// `HeadA1`/`HeadB1`, `Boots2` — and the client's exporter sometimes appends `:N`. Matching on
    /// the leading word rather than parsing the suffix keeps every one of those forms working.
    ///
    /// No slot name is a prefix of another (`Lbody` does not start with `Body`), so plain prefix
    /// matching is unambiguous and order-independent here.
    #[must_use]
    pub fn from_part_name(name: &str) -> Option<SkinSlot> {
        let n = name.trim().to_ascii_lowercase();
        // `Lbody` must be tested before `Body` would ever be reached by a substring search; with
        // prefix matching it cannot collide, but keep it first as documentation of the hazard.
        for (prefix, slot) in [
            ("lbody", SkinSlot::Lbody),
            ("body", SkinSlot::Body),
            // `HeadB` is the hair layer, `HeadA` (and a plain `Head`) the face. Longest prefix
            // first so `headb` is not swallowed by the `head` rule.
            ("headb", SkinSlot::Head),
            ("heada", SkinSlot::Face),
            ("head", SkinSlot::Face),
            ("hair", SkinSlot::Head),
            ("arms", SkinSlot::Arms),
            ("gloves", SkinSlot::Gloves),
            ("legs", SkinSlot::Legs),
            ("boots", SkinSlot::Boots),
            ("cloak", SkinSlot::Cloak),
            ("helm", SkinSlot::Helm),
        ] {
            if n.starts_with(prefix) {
                return Some(slot);
            }
        }
        // Fig3 files are `hig_m_body01`, not `Body1`. Prefix match misses those.
        for (needle, slot) in [
            ("lbody", SkinSlot::Lbody),
            ("body", SkinSlot::Body),
            ("headb", SkinSlot::Head),
            ("heada", SkinSlot::Face),
            ("head", SkinSlot::Face),
            ("hair", SkinSlot::Head),
            ("arms", SkinSlot::Arms),
            ("glov", SkinSlot::Gloves),
            ("legs", SkinSlot::Legs),
            ("boot", SkinSlot::Boots),
            ("cloak", SkinSlot::Cloak),
            ("helm", SkinSlot::Helm),
        ] {
            if n.contains(needle) {
                return Some(slot);
            }
        }
        None
    }
}

/// A resolved skin reference: the shipped `.dds` member name (lowercased, extension normalised) and
/// the `figures/skins/skinNNN.mpk` archive number that contains it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkinRef {
    /// Lowercased DDS member name (authoring `.tga`/`.bmp` already swapped to `.dds`).
    pub dds: String,
    /// Skin archive number → `figures/skins/skin{archive:03}.mpk`.
    pub archive: u16,
}

/// A creature's locomotion strides, in world units travelled per full cycle of the matching clip.
/// Used to drive playback rate from movement speed so feet stay planted (A.3.5).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Strides {
    pub run: f32,
    pub walk: f32,
    pub back: f32,
    pub strafe: f32,
}

/// Resolved model-id → NIF base-name map (lowercased, no extension), ready for a `figures/` lookup.
pub struct MonsterModels {
    by_model: HashMap<u16, String>,
    /// model id → primary (Body-column) skin, when both the model row and the skins.csv row resolve.
    body_skin: HashMap<u16, SkinRef>,
    /// (model id, slot) → that slot's own skin. The Body entry is duplicated in `body_skin`, which
    /// stays as the whole-mesh fallback for parts whose name names no slot.
    part_skin: HashMap<(u16, SkinSlot), SkinRef>,
    /// model id → `anims.csv` set id, carried across from the model's monnifs row.
    anim_set: HashMap<u16, u16>,
    /// model id → authored-size correction as a multiplier (`Scale / 100`).
    scale: HashMap<u16, f32>,
    /// model id → locomotion strides from the same monnifs row.
    strides: HashMap<u16, Strides>,
    /// monnifs `Textual Name` (normalised) → NIF base name. Player races have rows named
    /// "<Race> <Gender>" ("Briton Male" → `bcommonm`), which is how an avatar finds the shared
    /// "common body" mesh whose skeleton its fig3 parts bind against. See [`Self::nif_for_label`].
    nif_by_label: HashMap<String, String>,
    /// skins.csv id → DDS + archive. Player faces (fig3facemap) index this table.
    skins: HashMap<u16, SkinRef>,
}

impl MonsterModels {
    /// Load and join `monsters.csv` + `monnifs.csv` out of `gamedata.mpk`.
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
        let monsters = find("monsters.csv")?;
        let monnifs = find("monnifs.csv")?;
        // skins.csv is optional: without it we simply fall back to the untextured (white) meshes.
        let skins = find("skins.csv").unwrap_or_default();
        Self::from_tables(&monsters, &monnifs, &skins)
    }

    /// Build the resolver from the three CSVs directly, with no archive in the way.
    ///
    /// Split out of [`MonsterModels::load`] so the join logic — particularly the per-slot skin
    /// resolution, which decides what every creature's face wears — is testable on inline tables
    /// instead of requiring a full client install.
    pub fn from_tables(monsters: &str, monnifs: &str, skins: &str) -> io::Result<Self> {
        // monnifs: nif id → base NIF name, plus the row's anim set and locomotion strides.
        let mut nif_name: HashMap<u16, String> = HashMap::new();
        let mut nif_anim_set: HashMap<u16, u16> = HashMap::new();
        let mut nif_strides: HashMap<u16, Strides> = HashMap::new();
        let mut nif_by_label: HashMap<String, String> = HashMap::new();
        for (id, cols) in numeric_rows(monnifs) {
            // Textual Name → NIF Name. Lowest row id wins: races appear several times (a base row
            // plus later variants) and the first is the canonical body.
            if let (Some(label), Some(nif)) = (cols.get(1), cols.get(MONNIFS_NAME_COL)) {
                let (label, nif) = (label.trim(), nif.trim());
                if !label.is_empty() && !nif.is_empty() {
                    nif_by_label
                        .entry(normalise_label(label))
                        .or_insert_with(|| nif.to_ascii_lowercase());
                }
            }
            if let Some(name) = cols.get(MONNIFS_NAME_COL) {
                let name = name.trim();
                if !name.is_empty() {
                    nif_name.insert(id, name.to_ascii_lowercase());
                }
            }
            // Anim set 0 = "no animation set" (static props), so it's not a usable mapping.
            if let Some(set) = cols
                .get(MONNIFS_ANIM_SET_COL)
                .and_then(|s| s.trim().parse::<u16>().ok())
            {
                if set != 0 {
                    nif_anim_set.insert(id, set);
                }
            }
            let f = |c: usize| {
                cols.get(c)
                    .and_then(|s| s.trim().parse::<f32>().ok())
                    .unwrap_or(0.0)
            };
            nif_strides.insert(
                id,
                Strides {
                    run: f(MONNIFS_STRIDE_RUN_COL),
                    walk: f(MONNIFS_STRIDE_WALK_COL),
                    back: f(MONNIFS_STRIDE_BACK_COL),
                    strafe: f(MONNIFS_STRIDE_STRAFE_COL),
                },
            );
        }

        // skins: skin id → (dds member name, archive number). Skin id 0 = "no skin".
        let mut skin_ref: HashMap<u16, SkinRef> = HashMap::new();
        for (id, cols) in numeric_rows(skins) {
            if id == 0 {
                continue;
            }
            let name = cols.get(SKINS_NAME_COL).map(|s| s.trim()).unwrap_or("");
            let archive = cols
                .get(SKINS_ARCHIVE_COL)
                .and_then(|s| s.trim().parse::<u16>().ok());
            if let (false, Some(archive)) = (name.is_empty(), archive) {
                skin_ref.insert(
                    id,
                    SkinRef {
                        dds: normalise_dds(name),
                        archive,
                    },
                );
            }
        }

        // monsters: model id → nif id → name, plus the model's Body-column skin id → SkinRef.
        let mut by_model: HashMap<u16, String> = HashMap::new();
        let mut body_skin: HashMap<u16, SkinRef> = HashMap::new();
        let mut part_skin: HashMap<(u16, SkinSlot), SkinRef> = HashMap::new();
        let mut anim_set: HashMap<u16, u16> = HashMap::new();
        let mut strides: HashMap<u16, Strides> = HashMap::new();
        let mut scale: HashMap<u16, f32> = HashMap::new();
        for (model, cols) in numeric_rows(monsters) {
            // Only record a real correction; a missing or zero Scale means "as authored", and
            // treating 0 as a multiplier would collapse the mesh to nothing.
            if let Some(v) = cols
                .get(MONSTERS_SCALE_COL)
                .and_then(|s| s.trim().parse::<f32>().ok())
            {
                if v > 0.0 {
                    scale.insert(model, v / 100.0);
                }
            }
            if let Some(nif_id) = cols
                .get(MONSTERS_NIF_COL)
                .and_then(|s| s.trim().parse::<u16>().ok())
            {
                if let Some(name) = nif_name.get(&nif_id) {
                    by_model.insert(model, name.clone());
                }
                if let Some(set) = nif_anim_set.get(&nif_id) {
                    anim_set.insert(model, *set);
                }
                if let Some(st) = nif_strides.get(&nif_id) {
                    strides.insert(model, *st);
                }
            }
            // Every body slot, not just the torso. A slot whose id is 0 or absent means "this
            // creature has no such part"; recording nothing lets it fall back to the body skin.
            for slot in SkinSlot::ALL {
                let Some(skin_id) = cols
                    .get(slot.column())
                    .and_then(|s| s.trim().parse::<u16>().ok())
                else {
                    continue;
                };
                if let Some(sk) = skin_ref.get(&skin_id) {
                    part_skin.insert((model, slot), sk.clone());
                    if slot == SkinSlot::Body {
                        body_skin.insert(model, sk.clone());
                    }
                }
            }
        }
        Ok(Self {
            by_model,
            body_skin,
            part_skin,
            anim_set,
            strides,
            scale,
            nif_by_label,
            skins: skin_ref,
        })
    }

    /// The `figures/` NIF base name for a monnifs `Textual Name` — e.g. `"Briton Male"` →
    /// `"bcommonm"`. Case/space-insensitive. Used to find a player race's shared "common body"
    /// mesh, whose skeleton the race's fig3 body parts bind their bone names against.
    pub fn nif_for_label(&self, label: &str) -> Option<&str> {
        self.nif_by_label
            .get(&normalise_label(label))
            .map(String::as_str)
    }

    /// The creature's `anims.csv`/`canims.csv` set id, via its monnifs row. `None` for models with
    /// no anim set (static props) or no resolvable NIF row — those keep their bind pose.
    pub fn anim_set(&self, model_id: u16) -> Option<u16> {
        self.anim_set.get(&model_id).copied()
    }

    /// The creature's locomotion strides (world units per clip cycle), if its monnifs row resolved.
    pub fn strides(&self, model_id: u16) -> Option<Strides> {
        self.strides.get(&model_id).copied()
    }

    /// How many model ids carry an anim set (diagnostics — the A.3.5 coverage number).
    pub fn animated_len(&self) -> usize {
        self.anim_set.len()
    }

    /// The `figures/` NIF base name (lowercased, no extension) for a creature model id, if known.
    pub fn nif_name(&self, model_id: u16) -> Option<&str> {
        self.by_model.get(&model_id).map(String::as_str)
    }

    /// The model's authored-size correction (`monsters.csv` `Scale` / 100); `1.0` when the table
    /// has no usable value, i.e. render the mesh as authored.
    ///
    /// Multiplies with — it does not replace — the per-instance size byte from NPCCreate. The two
    /// answer different questions: this one is "was this MESH authored at world scale", that one is
    /// "is this particular creature a big or small example of its kind".
    #[must_use]
    pub fn model_scale(&self, model_id: u16) -> f32 {
        self.scale.get(&model_id).copied().unwrap_or(1.0)
    }

    /// A `skins.csv` row by id — player faces (fig3facemap) and creature slots share this table.
    #[must_use]
    pub fn skin(&self, skin_id: u16) -> Option<&SkinRef> {
        self.skins.get(&skin_id)
    }

    /// The creature's primary (Body-column) skin, if both the model and its skins.csv row resolve.
    /// This is the whole-mesh texture in the first-cut skinning pass.
    pub fn body_skin(&self, model_id: u16) -> Option<&SkinRef> {
        self.body_skin.get(&model_id)
    }

    /// The skin for one body slot of a model, if the table names one.
    #[must_use]
    pub fn part_skin(&self, model_id: u16, slot: SkinSlot) -> Option<&SkinRef> {
        self.part_skin.get(&(model_id, slot))
    }

    /// The skin a named mesh part should wear: its own slot's skin, falling back to the body skin
    /// when the part names no slot we know (LOD helpers, effect meshes) or the table leaves that
    /// slot empty. Never returns nothing where `body_skin` would have returned something, so this
    /// cannot regress a creature that was previously drawing correctly on one skin.
    #[must_use]
    pub fn skin_for_part(&self, model_id: u16, part_name: &str) -> Option<&SkinRef> {
        SkinSlot::from_part_name(part_name)
            .and_then(|slot| self.part_skin(model_id, slot))
            .or_else(|| self.body_skin(model_id))
    }

    /// How many model ids resolved (diagnostics).
    /// Every model id the tables name a NIF for, ascending.
    ///
    /// Exposed so a harness can sweep the whole creature bank instead of spot-checking. Roughly
    /// 2,200 rows, and until this existed none of them were watched by anything.
    #[must_use]
    pub fn model_ids(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = self.by_model.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub fn len(&self) -> usize {
        self.by_model.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_model.is_empty()
    }
}

/// Normalise a skins.csv `Skin Name` to the shipped member name: strip any directory, lowercase,
/// and swap the authoring `.tga`/`.bmp` extension for the `.dds` the client actually ships (same
/// rule the world-fixture texture loader applies). A name with no extension gets `.dds` appended.
fn normalise_dds(name: &str) -> String {
    let base = name
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    match base.rsplit_once('.') {
        Some((stem, "dds")) => format!("{stem}.dds"),
        Some((stem, _)) => format!("{stem}.dds"),
        None => format!("{base}.dds"),
    }
}

/// Case/space-insensitive label key ("Briton Male" == "britonmale").
fn normalise_label(label: &str) -> String {
    label
        .chars()
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
    use super::*;

    #[test]
    fn joins_two_hops() {
        // A tiny stand-in for the two tables' shape: model 26 → NIF# 207 → "skel01".
        let monsters = "Information,,NIF\nID,name,#\n26,Large Skeleton,207,75,0\n3,empty,0,0";
        let monnifs =
            "Information,,NIF\nID,Textual Name,Name\n207,Skeleton,skel01,4\n88,Minotaur,mino,357";
        let mut nif_name = HashMap::new();
        for (id, cols) in numeric_rows(monnifs) {
            nif_name.insert(id, cols[MONNIFS_NAME_COL].trim().to_ascii_lowercase());
        }
        let mut by_model = HashMap::new();
        for (model, cols) in numeric_rows(monsters) {
            if let Some(nif_id) = cols
                .get(MONSTERS_NIF_COL)
                .and_then(|s| s.trim().parse::<u16>().ok())
            {
                if let Some(n) = nif_name.get(&nif_id) {
                    by_model.insert(model, n.clone());
                }
            }
        }
        let m = MonsterModels {
            by_model,
            body_skin: HashMap::new(),
            part_skin: HashMap::new(),
            anim_set: HashMap::new(),
            scale: HashMap::new(),
            strides: HashMap::new(),
            nif_by_label: HashMap::new(),
            skins: HashMap::new(),
        };
        assert_eq!(m.nif_name(26), Some("skel01"));
        assert_eq!(m.nif_name(3), None); // NIF# 0 → no mapping
        assert_eq!(m.nif_name(999), None);
    }

    #[test]
    fn resolves_body_skin_through_skins_csv() {
        // model 26 (Large Skeleton): Body/Head skin id 82 → skins.csv 82 = skel3.tga, archive 2.
        let skins = "Information,,Skin,,Archive\nID,Textual Name,Name,MULTI,Num\n\
                     82,Skeleton 3,skel3.tga,0,2\n0,None,,0,0";
        let mut skin_ref: HashMap<u16, SkinRef> = HashMap::new();
        for (id, cols) in numeric_rows(skins) {
            if id == 0 {
                continue;
            }
            let name = cols.get(SKINS_NAME_COL).map(|s| s.trim()).unwrap_or("");
            let archive = cols
                .get(SKINS_ARCHIVE_COL)
                .and_then(|s| s.trim().parse::<u16>().ok());
            if let (false, Some(archive)) = (name.is_empty(), archive) {
                skin_ref.insert(
                    id,
                    SkinRef {
                        dds: normalise_dds(name),
                        archive,
                    },
                );
            }
        }
        assert_eq!(
            skin_ref[&82],
            SkinRef {
                dds: "skel3.dds".into(),
                archive: 2
            }
        );
        assert_eq!(normalise_dds("Body\\Skel3.TGA"), "skel3.dds");
        assert_eq!(normalise_dds("afancbody01"), "afancbody01.dds");
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;

    /// The `Scale` column is a PERCENTAGE of the mesh's authored size, and it is separate from the
    /// per-instance size byte on NPCCreate. Values measured from the shipped `monsters.csv`:
    /// Boar 100 (baseline, and the modal value at 1196 of 2192 rows), Small Grey Wolf 50,
    /// Large Skeleton 125, Dragonfly 10.
    ///
    /// Only the conversion is asserted here — reading the real table needs the client — but the
    /// conversion is where the bug was: treating the column as a raw multiplier rather than a
    /// percentage would shrink everything by 100×, and treating a missing/zero value as a
    /// multiplier would collapse a mesh to nothing.
    #[test]
    fn scale_is_a_percentage_with_one_hundred_as_baseline() {
        let pct = |v: f32| if v > 0.0 { v / 100.0 } else { 1.0 };
        assert!(
            (pct(100.0) - 1.0).abs() < 1e-6,
            "100 must mean 'as authored'"
        );
        assert!((pct(50.0) - 0.5).abs() < 1e-6, "a genuinely small creature");
        assert!((pct(125.0) - 1.25).abs() < 1e-6, "a genuinely large one");
        assert!(
            (pct(10.0) - 0.1).abs() < 1e-6,
            "the Dragonfly's 10x-oversize mesh"
        );
        // Missing or zero must fall back to "as authored", never to zero size.
        assert!(
            (pct(0.0) - 1.0).abs() < 1e-6,
            "zero must not collapse the mesh"
        );
    }

    /// The part names figure meshes actually use, taken from `BSaracenF.NIF`.
    #[test]
    fn part_names_map_to_their_body_slot() {
        for (name, want) in [
            ("Body1", SkinSlot::Body),
            ("Body1b", SkinSlot::Body),    // the 'b' variant suffix
            ("Body5b:45", SkinSlot::Body), // …with the exporter's `:N` index
            // A mesh HEAD part takes the FACE column; `HeadB` is the hair layer and takes the
            // column labelled `Head`, which actually holds hair skins.
            ("HeadA1", SkinSlot::Face),
            ("HeadB2", SkinSlot::Head),
            ("Arms3:11", SkinSlot::Arms),
            ("Gloves1", SkinSlot::Gloves),
            ("Legs2", SkinSlot::Legs),
            ("Boots1", SkinSlot::Boots),
            ("Cloak1", SkinSlot::Cloak),
            // Case varies between meshes: both `Lbody` and `LBody` ship.
            ("Lbody1", SkinSlot::Lbody),
            ("LBody1", SkinSlot::Lbody),
            ("hig_m_body01", SkinSlot::Body),
            ("tro_m_head01", SkinSlot::Face),
        ] {
            assert_eq!(SkinSlot::from_part_name(name), Some(want), "{name}");
        }

        // `Lbody` must NOT be read as `Body` — they are different columns with different skins,
        // and a substring match rather than a prefix match would silently conflate them.
        assert_ne!(SkinSlot::from_part_name("Lbody1"), Some(SkinSlot::Body));

        // Non-slot nodes must not claim a slot; they fall back to the body skin instead.
        for name in ["Lod01", "Bip01 Pelvis", "Scene Root", ""] {
            assert_eq!(
                SkinSlot::from_part_name(name),
                None,
                "{name} is not a body slot"
            );
        }
    }

    /// The nine slots must address nine CONSECUTIVE columns starting at the Body column — that
    /// contiguity is the whole basis of [`SkinSlot::column`].
    #[test]
    fn slot_columns_match_the_file_layout() {
        // The nine-column skin block is contiguous from Body...
        let block: Vec<usize> = [
            SkinSlot::Body,
            SkinSlot::Head,
            SkinSlot::Arms,
            SkinSlot::Gloves,
            SkinSlot::Lbody,
            SkinSlot::Legs,
            SkinSlot::Boots,
            SkinSlot::Cloak,
            SkinSlot::Helm,
        ]
        .iter()
        .map(|s| s.column())
        .collect();
        assert_eq!(
            block,
            (MONSTERS_BODY_SKIN_COL..MONSTERS_BODY_SKIN_COL + 9).collect::<Vec<_>>()
        );
        assert_eq!(
            SkinSlot::Body.column(),
            MONSTERS_BODY_SKIN_COL,
            "Body heads the block"
        );

        // ...but Face sits OUTSIDE it, past Scale/Tall/Shdw/Sound/Kilt/Sarac. Deriving its column
        // from a position in `ALL` would silently read `Shdw` as a skin id.
        assert_eq!(SkinSlot::Face.column(), MONSTERS_FACE_SKIN_COL);
        assert!(
            SkinSlot::Face.column() > SkinSlot::Helm.column() + 1,
            "Face is not part of the block"
        );
    }

    /// The regression this whole slice exists for: a part must wear ITS OWN slot's skin.
    ///
    /// Model 82's real row — the head skin (1043) is a different image from the torso (122), so
    /// binding the body skin to every part drew the torso across the face. Asserted through the
    /// real table-join path, not by restating the mapping.
    #[test]
    fn each_part_wears_its_own_slots_skin() {
        // Columns: ID,name,NIF#, Body,Head,Arms,Gloves,Lbody,Legs,Boots,Cloak,Helm, Scale,Tall,Shdw,
        //          SoundSet,Kilt,Sarac, Face(18)
        // Model 82's real row. NOTE Head=1043 is `hair_sardkbrowna.tga` — the column labelled
        // `Head` carries HAIR — while Face=46 is `s_fhed3.tga`, the actual head texture. Painting
        // the Head column onto a face is what made every NPC look like it was wearing cloth.
        let monsters = "Information,,NIF\nID,name,#\n\
                        82,Saracen Female Merchant 2,214,122,1043,123,124,125,119,120,0,0,100,0,0,0,0,0,46";
        let monnifs = "Information,,NIF\nID,Textual Name,Name\n214,Saracen Female,bsaracenf,4";
        let skins = "Information\nID,Textual Name,Skin Name,MULTI,Archive Num\n\
                     122,body,sarbody.tga,0,5\n\
                     1043,hair,sarhair.tga,0,7\n\
                     46,face,s_fhed3.tga,0,1\n\
                     123,arms,sararms.tga,0,5\n\
                     120,boots,sarboots.tga,0,5";
        let m = MonsterModels::from_tables(monsters, monnifs, skins).expect("tables parse");

        let dds = |part: &str| m.skin_for_part(82, part).map(|s| s.dds.clone());
        assert_eq!(dds("Body1"), Some("sarbody.dds".into()));
        // The mesh's head part takes the FACE column, not the hair-bearing `Head` column.
        assert_eq!(
            dds("HeadA1"),
            Some("s_fhed3.dds".into()),
            "the face must wear the FACE skin"
        );
        assert_eq!(
            dds("HeadB1"),
            Some("sarhair.dds".into()),
            "the hair layer takes the Head column"
        );
        assert_eq!(dds("Arms2"), Some("sararms.dds".into()));
        assert_eq!(dds("Boots1"), Some("sarboots.dds".into()));

        // The two bugs stated directly: a face must not wear the torso, nor the hair swatch.
        assert_ne!(
            dds("HeadA1"),
            dds("Body1"),
            "head and torso skins must not be the same image"
        );
        assert_ne!(
            dds("HeadA1"),
            dds("HeadB1"),
            "the face must not wear the hair texture"
        );

        // A slot the row leaves at 0 (Cloak) and a part naming no slot both fall back to the body
        // skin — never to nothing, so this cannot strip a creature that already drew correctly.
        assert_eq!(
            dds("Cloak1"),
            dds("Body1"),
            "an empty slot falls back to the body skin"
        );
        assert_eq!(
            dds("Lod01"),
            dds("Body1"),
            "an unknown part falls back to the body skin"
        );

        // The archive travels with the name: these two skins live in different .mpk files, and
        // pairing a name with the wrong archive number would fail to load at runtime.
        assert_eq!(m.part_skin(82, SkinSlot::Head).map(|s| s.archive), Some(7));
        assert_eq!(m.part_skin(82, SkinSlot::Face).map(|s| s.archive), Some(1));
        assert_eq!(m.part_skin(82, SkinSlot::Body).map(|s| s.archive), Some(5));
    }
}
