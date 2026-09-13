//! Face skins, skin tints, and hair dye tables from `gamedata.mpk`.
//!
//! Retail layout (Clean Retail `gamedata.mpk`, 2026-08-16 audit):
//! - `fig3facemap.csv` — `#`, `Description`, then face-type columns `1`..=`7` holding **skins.csv
//!   ids** (e.g. Briton Male face 1 = 3238 → `Bri_m_Head01.dds`). Not UV rectangles.
//! - `fig3skincolormap.csv` — `#`, description (`Cata Briton Male`), then tone columns `1`..=`8`
//!   holding **tint ids**.
//! - `fig3tints.csv` — `ID`, name, `Red`, `Green`, `Blue`.
//! - `fig3haircolormap.csv` — style × colour as `NS`/`NT`/`NM` triples (skin ids, not RGB).
//! - `fig3eyecolormap.csv` / `fig3decalmap.csv` — eye and tattoo skin ids.
//! - `fig3descriptions.csv` — the exact UI labels and legal indices for each race/gender.

use std::collections::HashMap;
use std::io;
use std::path::Path;

/// One source-authored label from `fig3descriptions.csv`.
///
/// The index is deliberately retained instead of inferred from the count.  The client table has
/// holes (for example a race can author "Bald" between two hair styles), and compacting the list
/// would silently make the UI send a different source value than the label it displays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearanceLabel {
    pub index: u8,
    pub label: String,
}

/// The five character-create control families that `fig3descriptions.csv` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppearanceControl {
    Face = 0,
    Hair = 1,
    Tattoo = 2,
    Scale = 3,
    Morphs = 4,
}

impl AppearanceControl {
    fn from_table_part(part: u8) -> Option<Self> {
        match part {
            0 => Some(Self::Face),
            1 => Some(Self::Hair),
            2 => Some(Self::Tattoo),
            3 => Some(Self::Scale),
            4 => Some(Self::Morphs),
            _ => None,
        }
    }
}

/// Skin / hair look tables keyed the way fig3map descriptions are keyed.
pub struct Fig3Look {
    /// tint id → RGB 0..255.
    tints: HashMap<u16, [u8; 3]>,
    /// (normalised race, fig3 gender, tone 1..=n) → tint id.
    skin_map: HashMap<(String, u8, u8), u16>,
    /// (normalised race, fig3 gender, face_type 1..=7) → skins.csv id.
    faces: HashMap<(String, u8, u8), u16>,
    /// (normalised race, fig3 gender, hair_color) → skins.csv id from the `1S`/`2S`/… column.
    hair_map: HashMap<(String, u8, u8), u16>,
    /// `(race, gender, style, colour) -> skins.csv id`. The colour map above keeps only the first
    /// style, which is why a mesh and its sheet could disagree.
    hair_styles: HashMap<(String, u8, u8, u8), u16>,
    /// (normalised race, fig3 gender, eye colour) → skins.csv id.
    eyes: HashMap<(String, u8, u8), u16>,
    /// (normalised race, fig3 gender, tattoo) → skins.csv id.
    decals: HashMap<(String, u8, u8), u16>,
    /// Exact source labels and their non-compacted table indices, keyed by control family.
    descriptions: HashMap<(String, u8, AppearanceControl), Vec<AppearanceLabel>>,
}

impl Fig3Look {
    /// Load every look table that is present. Missing members leave that axis empty.
    pub fn load(gamedata_mpk: impl AsRef<Path>) -> io::Result<Self> {
        let members = crate::open(gamedata_mpk)?;
        let find = |name: &str| -> Option<String> {
            members
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(name))
                .map(|m| String::from_utf8_lossy(&m.data).into_owned())
        };
        Ok(Self::from_all_tables(
            find("fig3tints.csv").as_deref().unwrap_or(""),
            find("fig3skincolormap.csv").as_deref().unwrap_or(""),
            find("fig3facemap.csv").as_deref().unwrap_or(""),
            find("fig3haircolormap.csv").as_deref().unwrap_or(""),
            find("fig3eyecolormap.csv").as_deref().unwrap_or(""),
            find("fig3decalmap.csv").as_deref().unwrap_or(""),
            find("fig3descriptions.csv").as_deref().unwrap_or(""),
        ))
    }

    #[must_use]
    pub fn from_tables(tints: &str, skin: &str, faces: &str, hair: &str) -> Self {
        Self::from_all_tables(tints, skin, faces, hair, "", "", "")
    }

    /// Build the complete character-appearance table set.  Kept separate from [`Self::from_tables`]
    /// so existing focused parser tests can construct the four render-critical maps without
    /// fabricating unrelated CSV columns.
    #[must_use]
    pub fn from_all_tables(
        tints: &str,
        skin: &str,
        faces: &str,
        hair: &str,
        eyes: &str,
        decals: &str,
        descriptions: &str,
    ) -> Self {
        Self {
            tints: parse_tints(tints),
            skin_map: parse_indexed_ids(skin, 1),
            faces: parse_indexed_ids(faces, 1),
            hair_map: parse_hair_s_columns(hair),
            hair_styles: parse_hair_styles(hair),
            eyes: parse_indexed_ids(eyes, 1),
            decals: parse_indexed_ids(decals, 1),
            descriptions: parse_descriptions(descriptions),
        }
    }

    #[must_use]
    pub fn tint_rgb(&self, id: u16) -> Option<[u8; 3]> {
        self.tints.get(&id).copied()
    }

    /// Skin tone from the create/overview `EyeColor` byte: low nibble is 1–4 on the wire.
    #[must_use]
    pub fn skin_rgb(&self, race_name: &str, gender: u8, eye_color: u8) -> Option<[u8; 3]> {
        let tone = (eye_color & 0x0F).max(1);
        let id = *self.skin_map.get(&(normalise(race_name), gender, tone))?;
        self.tint_rgb(id)
    }

    /// `skins.csv` id for this race/gender/face-type (1-based, matching the facemap columns).
    #[must_use]
    pub fn face_skin_id(&self, race_name: &str, gender: u8, face_type: u8) -> Option<u16> {
        lookup(&self.faces, race_name, gender, face_type.max(1))
    }

    /// Every authored face entry, retaining its source table index.
    #[must_use]
    pub fn face_options(&self, race_name: &str, gender: u8) -> Vec<(u8, u16)> {
        indexed_options(&self.faces, race_name, gender)
    }

    /// Every authored eye-colour entry, retaining its source table index.
    #[must_use]
    pub fn eye_options(&self, race_name: &str, gender: u8) -> Vec<(u8, u16)> {
        indexed_options(&self.eyes, race_name, gender)
    }

    /// Every authored tattoo/decal entry, retaining its source table index.
    #[must_use]
    pub fn tattoo_options(&self, race_name: &str, gender: u8) -> Vec<(u8, u16)> {
        indexed_options(&self.decals, race_name, gender)
    }

    /// Every authored skin-tone entry as `(source index, tint id, RGB)`.
    #[must_use]
    pub fn skin_tones(&self, race_name: &str, gender: u8) -> Vec<(u8, u16, [u8; 3])> {
        indexed_options(&self.skin_map, race_name, gender)
            .into_iter()
            .filter_map(|(index, tint_id)| self.tint_rgb(tint_id).map(|rgb| (index, tint_id, rgb)))
            .collect()
    }

    /// Exact labels for one control family, with their source table indices intact.
    #[must_use]
    pub fn labels(
        &self,
        race_name: &str,
        gender: u8,
        control: AppearanceControl,
    ) -> Vec<AppearanceLabel> {
        lookup_descriptions(&self.descriptions, race_name, gender, control).unwrap_or_default()
    }

    /// `skins.csv` id for this race/gender/hair-colour, from the `nS` column.
    ///
    /// Hair is a texture, not a tint, and it is NOT named after its mesh: the mesh is
    /// `Hig_M_hair01` while the texture is `Hig_m_Hair1_blonde.dds`. Only this table connects the
    /// two, which is why binding `<mesh>.dds` leaves every race's hair white.
    #[must_use]
    pub fn hair_skin_id(&self, race_name: &str, gender: u8, hair_color: u8) -> Option<u16> {
        lookup(&self.hair_map, race_name, gender, hair_color.max(1))
    }

    #[must_use]
    pub fn hair_rgb(&self, race_name: &str, gender: u8, hair_color: u8) -> Option<[u8; 3]> {
        let id = *self
            .hair_map
            .get(&(normalise(race_name), gender, hair_color.max(1)))?;
        self.tint_rgb(id)
    }

    /// Every authored hairstyle for this race/gender at `colour`, as `(style, skins.csv id)`.
    ///
    /// A hairstyle selects a MESH and a SHEET together. Binding colour-1-style-1 regardless of the
    /// mesh fig3 assigns is how Kobold ended up with a `hair01` mesh wearing a `hair02` sheet.
    #[must_use]
    pub fn hair_styles(&self, race_name: &str, gender: u8, colour: u8) -> Vec<(u8, u16)> {
        hair_options(&self.hair_styles, race_name, gender, Some(colour.max(1)))
    }

    /// Every authored colour sheet for one hairstyle, retaining the map's exact colour indices.
    #[must_use]
    pub fn hair_colours(&self, race_name: &str, gender: u8, style: u8) -> Vec<(u8, u16)> {
        hair_options_for_style(&self.hair_styles, race_name, gender, style)
    }

    /// The sheet one specific hairstyle wears at `colour`, or `None` when this race+gender has no
    /// such style — including style `0`, which is what DOL stores for a character nobody
    /// customised and means "no style chosen", not "style zero".
    #[must_use]
    pub fn hair_style_skin_id(
        &self,
        race_name: &str,
        gender: u8,
        style: u8,
        colour: u8,
    ) -> Option<u16> {
        if style == 0 {
            return None;
        }
        self.hair_styles(race_name, gender, colour)
            .into_iter()
            .find(|(s, _)| *s == style)
            .map(|(_, id)| id)
    }

    #[must_use]
    pub fn tint_count(&self) -> usize {
        self.tints.len()
    }

    #[must_use]
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }
}

/// The fig3 tables spell some races differently from `RACE_NAMES`.
///
/// `fig3haircolormap.csv` and `fig3facemap.csv` describe rows as "Cata **Norse** Male", while the
/// race is named "Norseman" everywhere else. Without this, Norseman matched no row at all and fell
/// back to a texture named after the mesh, which is how it ended up in white hair.
fn table_aliases(n: &str) -> [&'static str; 1] {
    match n {
        "norseman" => ["norse"],
        _ => [""],
    }
}

/// Look a key up under the race's own name, then under any table spelling of it.
fn lookup(map: &HashMap<(String, u8, u8), u16>, race_name: &str, gender: u8, n: u8) -> Option<u16> {
    table_names(race_name)
        .into_iter()
        .find_map(|name| map.get(&(name, gender, n)).copied())
}

/// All populated source indices for a race/gender map, in source-index order.
///
/// This deliberately walks the parsed keys rather than a protocol-sized `1..=N` range.  A future
/// expansion can add a twelfth eye colour or leave a deliberate hole without the inspection tool
/// quietly inventing either choice.
fn indexed_options(
    map: &HashMap<(String, u8, u8), u16>,
    race_name: &str,
    gender: u8,
) -> Vec<(u8, u16)> {
    for name in table_names(race_name) {
        let mut out: Vec<(u8, u16)> = map
            .iter()
            .filter_map(|((known, g, index), id)| {
                (known == &name && *g == gender).then_some((*index, *id))
            })
            .collect();
        if !out.is_empty() {
            out.sort_unstable_by_key(|(index, _)| *index);
            return out;
        }
    }
    Vec::new()
}

fn lookup_descriptions(
    map: &HashMap<(String, u8, AppearanceControl), Vec<AppearanceLabel>>,
    race_name: &str,
    gender: u8,
    control: AppearanceControl,
) -> Option<Vec<AppearanceLabel>> {
    table_names(race_name)
        .into_iter()
        .find_map(|name| map.get(&(name, gender, control)).cloned())
}

/// All candidate spellings of one race as the source tables write it, in preference order.
fn table_names(race_name: &str) -> Vec<String> {
    let key = normalise(race_name);
    std::iter::once(key.clone())
        .chain(
            table_aliases(&key)
                .into_iter()
                .filter(|alias| !alias.is_empty())
                .map(str::to_string),
        )
        .collect()
}

/// Every style at one colour, sourced from the actual four-dimensional hair map.
fn hair_options(
    map: &HashMap<(String, u8, u8, u8), u16>,
    race_name: &str,
    gender: u8,
    colour: Option<u8>,
) -> Vec<(u8, u16)> {
    for name in table_names(race_name) {
        let mut out: Vec<(u8, u16)> = map
            .iter()
            .filter_map(|((known, g, style, mapped_colour), id)| {
                (known == &name && *g == gender && colour.is_none_or(|c| *mapped_colour == c))
                    .then_some((*style, *id))
            })
            .collect();
        if !out.is_empty() {
            out.sort_unstable_by_key(|(style, _)| *style);
            return out;
        }
    }
    Vec::new()
}

/// All colour sheets for one hair style, retaining the source colour indices.
fn hair_options_for_style(
    map: &HashMap<(String, u8, u8, u8), u16>,
    race_name: &str,
    gender: u8,
    style: u8,
) -> Vec<(u8, u16)> {
    for name in table_names(race_name) {
        let mut out: Vec<(u8, u16)> = map
            .iter()
            .filter_map(|((known, g, mapped_style, colour), id)| {
                (known == &name && *g == gender && *mapped_style == style).then_some((*colour, *id))
            })
            .collect();
        if !out.is_empty() {
            out.sort_unstable_by_key(|(colour, _)| *colour);
            return out;
        }
    }
    Vec::new()
}

fn normalise(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn skip_headers(csv: &str) -> impl Iterator<Item = Vec<&str>> {
    csv.lines()
        .map(|l| l.split(',').collect::<Vec<_>>())
        .skip(2)
        .filter(|c| !c.is_empty() && c.iter().any(|s| !s.trim().is_empty()))
}

fn parse_tints(csv: &str) -> HashMap<u16, [u8; 3]> {
    let mut out = HashMap::new();
    for cols in skip_headers(csv) {
        let Some(id) = cols.first().and_then(|s| s.trim().parse::<u16>().ok()) else {
            continue;
        };
        // ID, Name, R, G, B — skip the name, take the first three 0..=255 numbers.
        let nums: Vec<u8> = cols
            .iter()
            .skip(1)
            .filter_map(|s| s.trim().parse::<u8>().ok())
            .collect();
        if nums.len() >= 3 {
            out.insert(id, [nums[0], nums[1], nums[2]]);
        }
    }
    out
}

/// Build `(race, gender, index) -> id` from a description-keyed table, **keeping the first row**.
///
/// Description in `desc_col`, then 1-based variant columns of numeric ids.
///
/// A race and gender appear on several rows: the player entry, then monster reskins of the same
/// body whose descriptions differ only by a trailing word — `fig3facemap.csv` ships `Briton Male`
/// (row 1, seven distinct face ids) followed by `Briton Male Stone` (70), `Briton Male Shade`
/// (71) and `Briton Male Mummy` (72), each repeating one id across every column.
/// [`parse_race_gender`] reduces all four to `("briton", male)`, so overwriting on collision hands
/// every Briton on the character screen `Bri_m_MummyHead01.dds`. The player row is the lowest row
/// id in every case, which is the same "keep the FIRST, it is the canonical one" rule
/// `figures::FigureModels` already applies to duplicate figure rows.
fn parse_indexed_ids(csv: &str, desc_col: usize) -> HashMap<(String, u8, u8), u16> {
    let mut out = HashMap::new();
    for cols in skip_headers(csv) {
        let Some(desc) = cols
            .get(desc_col)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let Some((race, gender)) = parse_race_gender(desc) else {
            continue;
        };
        let mut idx = 1u8;
        for cell in cols.iter().skip(desc_col + 1) {
            let t = cell.trim();
            if t.is_empty() {
                idx = idx.saturating_add(1);
                continue;
            }
            if let Ok(id) = t.parse::<u16>() {
                if id != 0 {
                    out.entry((race.clone(), gender, idx)).or_insert(id);
                }
                idx = idx.saturating_add(1);
            }
        }
    }
    out
}

/// Parse the labels the client offers for the five appearance-control families.
///
/// The table has duplicate remodel / monster rows just like the maps.  The first player row is
/// canonical for the same reason `parse_indexed_ids` keeps first: a later collision is not a
/// replacement for the playable race's UI vocabulary.
fn parse_descriptions(csv: &str) -> HashMap<(String, u8, AppearanceControl), Vec<AppearanceLabel>> {
    let mut out = HashMap::new();
    for cols in skip_headers(csv) {
        let Some(control) = cols
            .get(1)
            .and_then(|raw| raw.trim().parse::<u8>().ok())
            .and_then(AppearanceControl::from_table_part)
        else {
            continue;
        };
        let Some(desc) = cols
            .get(2)
            .map(|raw| raw.trim())
            .filter(|raw| !raw.is_empty())
        else {
            continue;
        };
        let Some((race, gender)) = parse_race_gender(desc) else {
            continue;
        };
        let labels: Vec<AppearanceLabel> = cols
            .iter()
            .skip(3)
            .enumerate()
            .filter_map(|(offset, raw)| {
                let label = raw.trim();
                (!label.is_empty()).then_some(AppearanceLabel {
                    index: u8::try_from(offset + 1).unwrap_or(u8::MAX),
                    label: label.to_string(),
                })
            })
            .collect();
        if !labels.is_empty() {
            out.entry((race, gender, control)).or_insert(labels);
        }
    }
    out
}

/// Hair table: `#, style #, Description, 1S, 1T, 1M, 2S, …`. We keep the `nS` skin id per colour.
///
/// **Keeps the FIRST row per (race, gender, colour)**, exactly as [`parse_indexed_ids`] does.
///
/// The table carries one row per STYLE, so a race and gender appear on several — and the styles are
/// not ordered such that the last one is canonical. Overwriting handed Avalonian males
/// `val_f_hair03` (a Valkyn *female* style) and Dwarf females `fro_f_hair02`, i.e. hair from
/// another race entirely. The lowest row is the base style, matching the convention
/// `figures::FigureModels` uses for duplicate figure rows.
fn parse_hair_s_columns(csv: &str) -> HashMap<(String, u8, u8), u16> {
    let mut out = HashMap::new();
    for cols in skip_headers(csv) {
        let Some(desc) = cols.get(2).map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some((race, gender)) = parse_race_gender(desc) else {
            continue;
        };
        // After Description: groups of three (S, T, M). Colour 1 uses column 1S.
        let mut colour = 1u8;
        let mut i = 3usize;
        while i < cols.len() {
            if let Ok(id) = cols[i].trim().parse::<u16>() {
                if id != 0 {
                    out.entry((race.clone(), gender, colour)).or_insert(id);
                }
            }
            colour = colour.saturating_add(1);
            i += 3;
        }
    }
    out
}

/// Like [`parse_hair_s_columns`] but keyed by the `style #` column as well, so every authored
/// hairstyle survives instead of only the first.
fn parse_hair_styles(csv: &str) -> HashMap<(String, u8, u8, u8), u16> {
    let mut out = HashMap::new();
    for cols in skip_headers(csv) {
        let Some(desc) = cols.get(2).map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some((race, gender)) = parse_race_gender(desc) else {
            continue;
        };
        let Ok(style) = cols.get(1).map(|s| s.trim()).unwrap_or("").parse::<u8>() else {
            continue;
        };
        let mut colour = 1u8;
        let mut i = 3usize;
        while i < cols.len() {
            if let Ok(id) = cols[i].trim().parse::<u16>() {
                if id != 0 {
                    out.entry((race.clone(), gender, style, colour))
                        .or_insert(id);
                }
            }
            colour = colour.saturating_add(1);
            i += 3;
        }
    }
    out
}

fn parse_race_gender(desc: &str) -> Option<(String, u8)> {
    let mut tokens: Vec<&str> = desc.split_whitespace().collect();
    if tokens
        .first()
        .is_some_and(|t| t.eq_ignore_ascii_case("Cata"))
    {
        tokens.remove(0);
    }
    let gpos = tokens
        .iter()
        .position(|t| t.eq_ignore_ascii_case("Male") || t.eq_ignore_ascii_case("Female"))?;
    if gpos == 0 {
        return None;
    }
    let gender = if tokens[gpos].eq_ignore_ascii_case("Male") {
        1
    } else {
        2
    };
    Some((normalise(&tokens[..gpos].join(" ")), gender))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tints_parse_rgb() {
        let csv = "id,name,r,g,b\n#,,,,\n1,pale,255,255,255\n4,tan,209,160,108\n";
        let look = Fig3Look::from_tables(csv, "", "", "");
        assert_eq!(look.tint_rgb(1), Some([255, 255, 255]));
        assert_eq!(look.tint_rgb(4), Some([209, 160, 108]));
    }

    #[test]
    fn facemap_columns_are_skins_csv_ids() {
        let csv = "id,desc,1,2,3\n#,,,,\n1, Briton Male,3238,3778,3779\n";
        let look = Fig3Look::from_tables("", "", csv, "");
        assert_eq!(look.face_skin_id("Briton", 1, 1), Some(3238));
        assert_eq!(look.face_skin_id("Briton", 1, 2), Some(3778));
    }

    #[test]
    fn skin_colormap_strips_cata_prefix() {
        let csv = "id,desc,1,2,3,4\n#,,,,,\n1,Cata Briton Male,1,2,3,4\n";
        let tints = "id,name,r,g,b\n#,,,,\n1,a,255,255,255\n4,b,209,160,108\n";
        let look = Fig3Look::from_tables(tints, csv, "", "");
        assert_eq!(look.skin_rgb("Briton", 1, 1), Some([255, 255, 255]));
        assert_eq!(look.skin_rgb("Briton", 1, 4), Some([209, 160, 108]));
    }

    #[test]
    fn complete_appearance_catalogue_keeps_source_indices_and_labels() {
        let tints = "id,name,r,g,b\n#,,,,\n1,pale,255,255,255\n2,tan,209,160,108\n";
        let skin = "id,desc,1,2\n#,,,,\n1,Cata Briton Male,1,2\n";
        let faces = "id,desc,1,2\n#,,,,\n1,Briton Male,3238,3778\n";
        let hair = "id,style,desc,1S,1T,1M,2S,2T,2M\n#,,,,,,,,\n1,1,Briton Male,100,0,0,101,0,0\n2,3,Briton Male,300,0,0,301,0,0\n";
        let eyes = "id,desc,1,2\n#,,,,\n1,Cata Briton Male,6275,6276\n";
        let decals = "id,desc,1,2\n#,,,,\n1,Briton Male,0,6033\n";
        let descriptions = concat!(
            "id,part,desc,1,2,3\n",
            "#,,,,,\n",
            "1,0,Briton Male Face,Face 1,Face 2,\n",
            "1,1,Briton Male Hair,Hair 1,Bald,Hair 3\n",
            "1,2,Briton Male Tattoo,No Tattoo,Tattoo 1,\n",
            "1,3,Briton Male Scale,Short,Average,Tall\n",
            "1,4,Briton Male Morphs,Nose,Eyes,Jaw\n",
            // A later remodel must not overwrite the player's UI labels.
            "2,0,Briton Male Mummy Face,Mummy 1,Mummy 2,\n",
        );
        let look = Fig3Look::from_all_tables(tints, skin, faces, hair, eyes, decals, descriptions);

        assert_eq!(look.face_options("Briton", 1), vec![(1, 3238), (2, 3778)]);
        assert_eq!(look.eye_options("Briton", 1), vec![(1, 6275), (2, 6276)]);
        assert_eq!(look.tattoo_options("Briton", 1), vec![(2, 6033)]);
        assert_eq!(
            look.skin_tones("Briton", 1),
            vec![(1, 1, [255, 255, 255]), (2, 2, [209, 160, 108])]
        );
        assert_eq!(look.hair_colours("Briton", 1, 3), vec![(1, 300), (2, 301)]);
        assert_eq!(
            look.labels("Briton", 1, AppearanceControl::Hair),
            vec![
                AppearanceLabel {
                    index: 1,
                    label: "Hair 1".into(),
                },
                AppearanceLabel {
                    index: 2,
                    label: "Bald".into(),
                },
                AppearanceLabel {
                    index: 3,
                    label: "Hair 3".into(),
                },
            ]
        );
        assert_eq!(
            look.labels("Briton", 1, AppearanceControl::Morphs),
            vec![
                AppearanceLabel {
                    index: 1,
                    label: "Nose".into(),
                },
                AppearanceLabel {
                    index: 2,
                    label: "Eyes".into(),
                },
                AppearanceLabel {
                    index: 3,
                    label: "Jaw".into(),
                },
            ]
        );
    }

    /// Several rows in the real table describe the same race and gender — the player face, and
    /// then monster reskins of it ("… Mummy", "… Shade", "… Stone"). Whichever we keep is the
    /// face every character on the create screen wears, so the choice cannot be "last one parsed".
    #[test]
    fn duplicate_race_gender_rows_keep_the_first() {
        let csv = "id,desc,1\n#,,\n1,Briton Male,3238\n2,Briton Male Mummy,6137\n";
        let look = Fig3Look::from_tables("", "", csv, "");
        assert_eq!(
            look.face_skin_id("Briton", 1, 1),
            Some(3238),
            "a later monster row must not overwrite the player face"
        );
    }

    /// **Named falsifier, real table.** The synthetic single-row test above passes either way;
    /// only the shipped `fig3facemap.csv` has the duplicate rows that expose the defect.
    ///
    /// `Bri_m_MummyHead01.dds` (skins id 6137) is what a Briton wore on every character screen
    /// before this: a monster reskin, because the map was built last-write-wins.
    #[test]
    fn real_facemap_gives_the_player_face_not_a_monster_reskin() {
        let Some(root) = crate::client_dep::require_caer_client("real_facemap_player_face") else {
            return;
        };
        let look = Fig3Look::load(root.join("gamedata.mpk")).expect("gamedata.mpk look tables");

        // Show the rows that collide, so a future change can see what it is choosing between.
        if let Ok(members) = crate::open(root.join("gamedata.mpk")) {
            if let Some(m) = members
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case("fig3facemap.csv"))
            {
                let text = String::from_utf8_lossy(&m.data);
                for line in text.lines() {
                    if line.to_ascii_lowercase().contains("briton male") {
                        eprintln!("fig3facemap: {line}");
                    }
                }
            }
        }

        assert_eq!(
            look.face_skin_id("Briton", GENDER_MALE_FIG3, 1),
            Some(3238),
            "Briton male face 1 is skins.csv 3238 (Bri_m_Head01.dds) — see this module's header"
        );
    }

    /// **Look-table coverage for every playable race and gender.**
    ///
    /// One row per combination, one column per lookup the avatar binder makes. A gap here is a
    /// white part on the character screen, and until this existed the only way to find one was to
    /// render a race and look at it — which is how bare hair shipped on 27 of 33 bodies while the
    /// bind counters read "fully bound".
    #[test]
    fn look_table_coverage_for_every_race() {
        let Some(root) = crate::client_dep::require_caer_client("look_table_coverage") else {
            return;
        };
        let look = Fig3Look::load(root.join("gamedata.mpk")).expect("gamedata.mpk look tables");

        // Names come from `figures::FigureModels::race_name`, never a second hardcoded list.
        // A local copy of this table is how "Norseman" got tested when the tables say "Norse",
        // producing a MISSING row for a race the product path resolves perfectly.
        let races: Vec<(u8, &'static str)> = (1..=18u8)
            .filter_map(|id| crate::figures::FigureModels::race_name(id).map(|n| (id, n)))
            .collect();

        println!(
            "\n{:<12} {:<7} {:>8} {:>8} {:>14}",
            "race", "gender", "face id", "hair id", "skin tone 1"
        );
        let mut missing_hair = Vec::new();
        let mut missing_face = Vec::new();
        for (id, name) in &races {
            for (g, gname) in [(1u8, "male"), (2, "female")] {
                let face = look.face_skin_id(name, g, 1);
                let hair = look.hair_skin_id(name, g, 1);
                let tone = look.skin_rgb(name, g, 1);
                println!(
                    "{name:<12} {gname:<7} {:>8} {:>8} {:>14}",
                    face.map_or("MISSING".into(), |v| v.to_string()),
                    hair.map_or("MISSING".into(), |v| v.to_string()),
                    tone.map_or("MISSING".into(), |c| format!("{},{},{}", c[0], c[1], c[2])),
                );
                if hair.is_none() {
                    missing_hair.push(format!("{name} {gname}"));
                }
                if face.is_none() {
                    missing_face.push(format!("{name} {gname}"));
                }
                let _ = id;
            }
        }
        println!(
            "\nmissing face: {}\nmissing hair: {}",
            if missing_face.is_empty() {
                "none".into()
            } else {
                missing_face.join(", ")
            },
            if missing_hair.is_empty() {
                "none".into()
            } else {
                missing_hair.join(", ")
            }
        );
    }

    /// `fig3skincolormap.csv` goes through the same collision-prone parser, so report what it
    /// resolves to now. A tone table that answers `255,255,255` for every race is a table we are
    /// reading wrong, not a client that ships one skin colour.
    #[test]
    fn real_skin_tones_are_not_all_white() {
        let Some(root) = crate::client_dep::require_caer_client("real_skin_tones") else {
            return;
        };
        let look = Fig3Look::load(root.join("gamedata.mpk")).expect("gamedata.mpk look tables");
        let mut distinct = std::collections::BTreeSet::new();
        for race in [
            "Briton", "Troll", "Firbolg", "Inconnu", "Lurikeen", "Kobold",
        ] {
            for gender in [GENDER_MALE_FIG3, 2] {
                let tones: Vec<String> = (1..=4)
                    .map(|t| {
                        look.skin_rgb(race, gender, t)
                            .map_or("-".into(), |c| format!("{},{},{}", c[0], c[1], c[2]))
                    })
                    .collect();
                eprintln!("skin {race:<9} g{gender}: {}", tones.join("  "));
                for t in tones {
                    distinct.insert(t);
                }
            }
        }
        assert!(
            distinct.len() > 1,
            "every race/gender/tone resolved to the same colour ({distinct:?}) — the tone axis is \
             not being read"
        );
    }
}

/// fig3 gender for male, matching `figures::GENDER_MALE`. Local so the tests do not reach across
/// modules for a constant.
#[cfg(test)]
const GENDER_MALE_FIG3: u8 = 1;
