//! `gamedata.mpk : mskins.csv` — the client's index of every avatar skin, and which archive holds it.
//!
//! Columns, from the file's own two header rows:
//!
//! ```text
//!   ID, Textual Name, Name, MULTI (2=dark 3=glow 4=gloss 5=env 6=bump 7=detail), Archive Num, Expansion Only
//! ```
//!
//! **Why this exists.** The material binder used to resolve a starting-set sheet by *constructing* a
//! file name (`cth<Slot>01_SS_<Token>_<M|F>.dds`) and probing every `figures/Mskins/*.mpk` for it,
//! first-sorted-archive winning. That is three separate guesses — that the name is right, that the
//! file is the one the client means, and that archive order is the tie-break — and the binder's own
//! comment records **two earlier versions of it that were invented rather than read**.
//!
//! Two things the guess gets wrong, both measured:
//!
//! - `cthLegs01_ss_Bri_m.dds` **exists in `mskin014` and is not in this table.** The client lists
//!   Briton legs as the *unisex* `cthLegs01_ss_Bri.dds` for male. Preferring a `_M` suffix therefore
//!   binds an unindexed leftover on every male leg.
//! - `cthArms01_SS_Bri_M.dds` is in **both** `mskin011` and `mskin037` with different art (max alpha
//!   210 and 102). Nothing stated which should win; the table names the archive outright.
//!
//! The `MULTI` column is recorded but is **not** a useful discriminator here: 2686 of 2688 rows are
//! `0`. It is parsed so a future multitexture pass has it, and so nobody re-derives that it is empty.

use std::collections::HashMap;
use std::io;
use std::path::Path;

/// One row of `mskins.csv`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MskinRow {
    pub id: u32,
    /// The DDS file name, as the client spells it. Case varies row to row.
    pub name: String,
    /// `figures/Mskins/mskin{archive:03}.mpk`.
    pub archive: u32,
    /// Multitexture role: 0 = base art, otherwise 2 dark / 3 glow / 4 gloss / 5 env / 6 bump /
    /// 7 detail. Almost always 0.
    pub multi: u8,
}

impl MskinRow {
    /// The archive member path this row names.
    #[must_use]
    pub fn archive_rel(&self) -> String {
        format!("figures/Mskins/mskin{:03}.mpk", self.archive)
    }

    /// Is this row base art rather than a multitexture layer?
    #[must_use]
    pub fn is_base(&self) -> bool {
        self.multi == 0
    }
}

/// `mskins.csv`, keyed by lowercased file name.
#[derive(Clone, Debug, Default)]
pub struct Mskins {
    by_name: HashMap<String, MskinRow>,
}

impl Mskins {
    /// Read `mskins.csv` out of `gamedata.mpk`.
    pub fn load(gamedata_mpk: impl AsRef<Path>) -> io::Result<Self> {
        let members = crate::open(gamedata_mpk)?;
        let csv = members
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case("mskins.csv"))
            .map(|m| String::from_utf8_lossy(&m.data).into_owned())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "mskins.csv not in gamedata.mpk")
            })?;
        Ok(Self::parse(&csv))
    }

    /// Build from the CSV body, so this is unit-testable without an mpk.
    #[must_use]
    pub fn parse(csv: &str) -> Self {
        let mut by_name = HashMap::new();
        for line in csv.lines() {
            let cols: Vec<&str> = line.split(',').collect();
            if cols.len() < 5 {
                continue;
            }
            // The first two lines are headers and the ID column is not numeric there, which is the
            // whole filter — no line counting, so an extra header row cannot shift the table.
            let Ok(id) = cols[0].trim().parse::<u32>() else {
                continue;
            };
            let name = cols[2].trim();
            let Ok(archive) = cols[4].trim().parse::<u32>() else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            let multi = cols[3].trim().parse::<u8>().unwrap_or(0);
            by_name.insert(
                name.to_ascii_lowercase(),
                MskinRow {
                    id,
                    name: name.to_string(),
                    archive,
                    multi,
                },
            );
        }
        Self { by_name }
    }

    /// Look a skin up by file name, ignoring case.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&MskinRow> {
        self.by_name.get(&name.to_ascii_lowercase())
    }

    /// Is this name one the client actually ships an entry for?
    ///
    /// Distinguishes "the archive happens to contain a file with this name" from "the client means
    /// this file", which is the difference that put an unindexed leftover on every male leg.
    #[must_use]
    pub fn indexed(&self, name: &str) -> bool {
        self.by_name.contains_key(&name.to_ascii_lowercase())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Information,,Skin,,Archive,Expansion,\n\
        ID,Textual Name,Name,MULTI 2=dark 3=glow,Num,Only,\n\
        405,cthLegs01_ss_Bri.dds,cthLegs01_ss_Bri.dds,0,14,4,\n\
        1926,cthLegs01_ss_Bri_f.dds,cthLegs01_ss_Bri_f.dds,0,36,4,\n\
        9999,glowy.dds,glowy.dds,3,7,4,\n";

    #[test]
    fn parses_rows_and_skips_the_two_headers() {
        let m = Mskins::parse(SAMPLE);
        assert_eq!(m.len(), 3, "three data rows, two headers");
        let legs = m.get("CTHLEGS01_SS_BRI.DDS").expect("case-insensitive");
        assert_eq!(legs.archive, 14);
        assert_eq!(legs.archive_rel(), "figures/Mskins/mskin014.mpk");
        assert!(legs.is_base());
        assert!(!m.get("glowy.dds").expect("glow row").is_base());
    }

    /// The distinction the binder needs: shipped-and-indexed vs merely present on disk.
    #[test]
    fn an_unindexed_name_is_not_indexed() {
        let m = Mskins::parse(SAMPLE);
        assert!(m.indexed("cthLegs01_ss_Bri_f.dds"));
        // The real case: this file exists in mskin014 and the client does not list it.
        assert!(!m.indexed("cthLegs01_ss_Bri_m.dds"));
    }
}
