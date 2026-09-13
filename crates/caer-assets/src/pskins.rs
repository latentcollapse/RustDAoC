//! Player armour skins — `objects.csv` × `pskins.csv` → `items/pskins/pskinNNN.mpk`.
//!
//! ## Why this exists (MS-02b)
//!
//! Figure NIFs and fig3 body parts carry **no** authored texture. Equipped armour appearance is
//! entirely table-driven: the `EquipmentUpdate` (0x15) item `model` is an `objects.csv` id whose
//! Body/Arms/… columns are skin ids into `pskins.csv`, which names the DDS and the `pskin` archive.
//! Binding those textures is what makes armour read as armour instead of a white/silhouette
//! mannequin (REQ-020 perceptual bar).
//!
//! Creature skins (`skins.csv` → `figures/skins/`) are a separate bank — see [`crate::monsters`].

use std::collections::HashMap;
use std::io;
use std::path::Path;

use crate::monsters::SkinSlot;

/// A resolved player-armour skin: DDS member + `items/pskins/pskin{archive:03}.mpk`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PskinRef {
    /// Lowercased DDS member name (authoring `.tga`/`.bmp` already swapped to `.dds`).
    pub dds: String,
    /// Archive number → `items/pskins/pskin{archive:03}.mpk`.
    pub archive: u16,
}

impl PskinRef {
    /// Path relative to the client root.
    #[must_use]
    pub fn archive_path(&self) -> String {
        format!("items/pskins/pskin{:03}.mpk", self.archive)
    }
}

/// Skin ids contributed by one `objects.csv` row (0 = none).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectSkinIds {
    pub body: u16,
    pub body_female: u16,
    pub arms: u16,
    pub gloves: u16,
    pub lbody: u16,
    pub legs: u16,
    pub boots: u16,
    pub cloak: u16,
    pub helm: u16,
}

impl ObjectSkinIds {
    /// Iterate (slot, skin_id) pairs that are nonzero for `female` body selection.
    pub fn slots(&self, female: bool) -> impl Iterator<Item = (SkinSlot, u16)> + '_ {
        let body = if female && self.body_female != 0 {
            self.body_female
        } else {
            self.body
        };
        [
            (SkinSlot::Body, body),
            (SkinSlot::Arms, self.arms),
            (SkinSlot::Gloves, self.gloves),
            (SkinSlot::Lbody, self.lbody),
            (SkinSlot::Legs, self.legs),
            (SkinSlot::Boots, self.boots),
            (SkinSlot::Cloak, self.cloak),
            (SkinSlot::Helm, self.helm),
        ]
        .into_iter()
        .filter(|(_, id)| *id != 0)
    }
}

/// `objects.csv` + `pskins.csv` joined for equipment texture lookup.
pub struct ObjectSkins {
    by_object: HashMap<u16, ObjectSkinIds>,
    pskins: HashMap<u16, PskinRef>,
}

impl ObjectSkins {
    /// Load from `gamedata.mpk`.
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
            &find("objects.csv")?,
            &find("pskins.csv")?,
        ))
    }

    /// Build from CSV text (tests / offline).
    #[must_use]
    pub fn from_tables(objects: &str, pskins: &str) -> Self {
        let mut pskin_map: HashMap<u16, PskinRef> = HashMap::new();
        for (id, cols) in numeric_rows(pskins) {
            // ID, Textual Name, Name, MULTI, Num(archive), …
            let Some(name) = cols.get(2).map(|s| s.trim()).filter(|s| !s.is_empty()) else {
                continue;
            };
            let archive = cols
                .get(4)
                .and_then(|s| s.trim().parse::<u16>().ok())
                .unwrap_or(0);
            if archive == 0 {
                continue;
            }
            pskin_map.insert(
                id,
                PskinRef {
                    dds: to_dds_name(name),
                    archive,
                },
            );
        }

        let mut by_object: HashMap<u16, ObjectSkinIds> = HashMap::new();
        for (id, cols) in numeric_rows(objects) {
            // objects.csv data cols (after 2 header lines):
            // 0 ID, 1 name, 2 NIF#, 3 Icon#, 4 Body, 5 FemBody, 6 Arms, 7 Gloves, 8 Lbody,
            // 9 Legs, 10 Boots, 11 Cloak, 12 Helm
            let parse = |i: usize| -> u16 {
                cols.get(i)
                    .and_then(|s| s.trim().parse::<u16>().ok())
                    .unwrap_or(0)
            };
            let skins = ObjectSkinIds {
                body: parse(4),
                body_female: parse(5),
                arms: parse(6),
                gloves: parse(7),
                lbody: parse(8),
                legs: parse(9),
                boots: parse(10),
                cloak: parse(11),
                helm: parse(12),
            };
            if skins.slots(false).next().is_none() && skins.slots(true).next().is_none() {
                continue;
            }
            by_object.insert(id, skins);
        }

        Self {
            by_object,
            pskins: pskin_map,
        }
    }

    /// The object that dresses the most armour slots — the best available "full suit", used as
    /// create-preview cloth when overview/0x15 has nothing to wear.
    ///
    /// This used to require body+arms+legs+boots ALL non-zero. On the retail tables that set is
    /// empty: 1,575 objects declare at least one armour slot and **zero** declare all four, so the
    /// old predicate could never return `Some` and any caller silently got "no armour exists".
    /// Ranking by filled-slot count answers the question actually being asked, and ties break on
    /// the lowest id so the choice is deterministic.
    pub fn starter_armor_object(&self) -> Option<u16> {
        self.by_object
            .iter()
            .map(|(id, s)| {
                let filled = [s.body, s.arms, s.gloves, s.legs, s.boots, s.lbody]
                    .iter()
                    .filter(|v| **v != 0)
                    .count();
                (filled, *id)
            })
            .filter(|(filled, _)| *filled > 0)
            // Most slots wins; lowest id breaks the tie (Reverse on id inside a max_by_key).
            .max_by_key(|(filled, id)| (*filled, std::cmp::Reverse(*id)))
            .map(|(_, id)| id)
    }

    /// Skin ids declared on an object (equipment model id), if any.
    #[must_use]
    pub fn object(&self, object_id: u16) -> Option<&ObjectSkinIds> {
        self.by_object.get(&object_id)
    }

    /// Resolve a pskin id to its archive member.
    #[must_use]
    pub fn pskin(&self, skin_id: u16) -> Option<&PskinRef> {
        self.pskins.get(&skin_id)
    }

    /// For one equipped object, yield `(mesh SkinSlot, PskinRef)` pairs for `female`.
    pub fn skins_for_object(&self, object_id: u16, female: bool) -> Vec<(SkinSlot, &PskinRef)> {
        let Some(ids) = self.object(object_id) else {
            return Vec::new();
        };
        ids.slots(female)
            .filter_map(|(slot, sid)| self.pskin(sid).map(|r| (slot, r)))
            .collect()
    }
}

fn to_dds_name(name: &str) -> String {
    let mut n = name.to_ascii_lowercase();
    for ext in [".tga", ".bmp", ".png"] {
        if let Some(stem) = n.strip_suffix(ext) {
            n = format!("{stem}.dds");
            break;
        }
    }
    if !n.ends_with(".dds") {
        n.push_str(".dds");
    }
    n
}

/// Rows whose first column is a numeric id. Skips the two-line Information/header preamble.
fn numeric_rows(csv: &str) -> impl Iterator<Item = (u16, Vec<&str>)> + '_ {
    csv.lines().filter_map(|line| {
        let cols: Vec<&str> = line.split(',').collect();
        let id = cols.first()?.trim().parse::<u16>().ok()?;
        Some((id, cols))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJECTS: &str = "Information,,,,,,,,\n\
ID,name,NIF,Icon,Body,Fem,Arms,Gloves,Lbody,Legs,Boots,Cloak,Helm\n\
81,Studded Studs Vest,1,161,330,331,0,0,334,0,0,0,0\n\
46,Alb Plate 1 Breast,1,1,345,0,0,0,0,0,0,0,0\n";

    const PSKINS: &str = "Information,,,,,,\n\
ID,Textual Name,Name,MULTI,Num,Only\n\
330,Studded,b_std2_body1m.tga,0,1,0\n\
331,Studded,b_std2_body1f.tga,0,1,0\n\
334,Studded,b_std2_lbody1.tga,0,1,0\n\
345,Alb Plate,b_plt1_body1m.tga,0,2,0\n";

    #[test]
    fn studded_vest_resolves_body_and_lbody() {
        let t = ObjectSkins::from_tables(OBJECTS, PSKINS);
        let skins = t.skins_for_object(81, false);
        assert_eq!(skins.len(), 2);
        assert!(skins
            .iter()
            .any(|(s, r)| *s == SkinSlot::Body && r.dds == "b_std2_body1m.dds"));
        assert!(skins
            .iter()
            .any(|(s, r)| *s == SkinSlot::Lbody && r.dds == "b_std2_lbody1.dds"));
        assert_eq!(
            t.pskin(330).unwrap().archive_path(),
            "items/pskins/pskin001.mpk"
        );
    }

    #[test]
    fn female_uses_fem_body_column() {
        let t = ObjectSkins::from_tables(OBJECTS, PSKINS);
        let skins = t.skins_for_object(81, true);
        assert!(skins
            .iter()
            .any(|(s, r)| *s == SkinSlot::Body && r.dds == "b_std2_body1f.dds"));
    }

    #[test]
    fn plate_and_studded_differ() {
        let t = ObjectSkins::from_tables(OBJECTS, PSKINS);
        let std = t.skins_for_object(81, false)[0].1.dds.clone();
        let plt = t.skins_for_object(46, false)[0].1.dds.clone();
        assert_ne!(std, plt);
    }
}
