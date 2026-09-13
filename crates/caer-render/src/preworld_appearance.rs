//! Source-backed choices for the pre-world character customiser.
//!
//! The product UI must not turn a row count into an invented `1..=N` range.  Retail's fig3
//! tables contain deliberate holes (notably `Bald`), race-specific hairstyle counts, and texture
//! maps that do not line up with a generic human body.  This small, GPU-free catalogue is shared
//! by the HUD and product dispatch so a visible label, its click behaviour, and the avatar binder
//! all speak the same source index.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use caer_assets::fig3_look::{AppearanceControl, AppearanceLabel, Fig3Look};
use caer_assets::figures::{FigureModels, GENDER_FEMALE, GENDER_MALE, PART_HAIR};

/// One legal source value, retaining the table index instead of collapsing it into a position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearanceChoice {
    pub index: u8,
    pub label: Option<String>,
}

/// The discrete source choices for one `(race, database gender)` pair.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppearanceChoices {
    faces: Vec<AppearanceChoice>,
    hair_styles: Vec<AppearanceChoice>,
    tattoos: Vec<AppearanceChoice>,
    skin_tones: Vec<u8>,
    eye_colours: Vec<u8>,
    hair_colours: BTreeMap<u8, Vec<u8>>,
    scale: Vec<AppearanceChoice>,
    morphs: Vec<AppearanceChoice>,
}

/// Which of the source-backed arrow selectors is being changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppearanceSelector {
    Face,
    HairStyle,
    Tattoo,
}

/// Complete catalog keyed by `(DAoC race id, database gender)`, where database gender is
/// `0 = male`, `1 = female`.  Fig3 itself uses `1 = male`, `2 = female`; the conversion happens
/// exactly once at construction so it cannot leak into a click handler.
#[derive(Clone, Debug, Default)]
pub struct AppearanceCatalog {
    by_identity: BTreeMap<(u8, u8), AppearanceChoices>,
}

impl AppearanceCatalog {
    /// Load the client-authored appearance tables once, before the interactive pre-world path
    /// starts.  A click must never reopen `gamedata.mpk` or fall back to a guessed cap.
    pub fn load(client_root: impl AsRef<Path>) -> io::Result<Self> {
        let gamedata = client_root.as_ref().join("gamedata.mpk");
        let look = Fig3Look::load(&gamedata)?;
        let figures = FigureModels::load(&gamedata)?;
        Ok(Self::from_sources(&look, &figures))
    }

    /// Join independently authored look and figure tables into player-selectable choices.
    #[must_use]
    pub fn from_sources(look: &Fig3Look, figures: &FigureModels) -> Self {
        let mut by_identity = BTreeMap::new();
        // 1..=18 is the fig3 player-race surface audited by `caer audit appearance`.  Minotaur
        // variants live on a later creation path and are intentionally absent until their retail
        // fig3 rows are proven rather than copied from a human fallback.
        for race in 1..=18 {
            let Some(race_name) = FigureModels::race_name(race) else {
                continue;
            };
            for (database_gender, fig3_gender) in [(0, GENDER_MALE), (1, GENDER_FEMALE)] {
                let faces = merge_choices(
                    look.labels(race_name, fig3_gender, AppearanceControl::Face),
                    look.face_options(race_name, fig3_gender)
                        .into_iter()
                        .map(|(index, _)| index),
                );
                let eyes: Vec<u8> = look
                    .eye_options(race_name, fig3_gender)
                    .into_iter()
                    .map(|(index, _)| index)
                    .collect();
                let skin_tones: Vec<u8> = look
                    .skin_tones(race_name, fig3_gender)
                    .into_iter()
                    .map(|(index, _, _)| index)
                    .collect();

                let mut hair_indices: Vec<u8> = look
                    .hair_styles(race_name, fig3_gender, 1)
                    .into_iter()
                    .map(|(index, _)| index)
                    .collect();
                hair_indices.extend(
                    figures
                        .part_variants(race, fig3_gender, PART_HAIR)
                        .into_iter()
                        .map(|(index, _)| index),
                );
                let hair_styles = merge_choices(
                    look.labels(race_name, fig3_gender, AppearanceControl::Hair),
                    hair_indices,
                );
                let hair_colours = hair_styles
                    .iter()
                    .map(|style| {
                        (
                            style.index,
                            look.hair_colours(race_name, fig3_gender, style.index)
                                .into_iter()
                                .map(|(index, _)| index)
                                .collect(),
                        )
                    })
                    .collect();

                let tattoos = merge_choices(
                    look.labels(race_name, fig3_gender, AppearanceControl::Tattoo),
                    look.tattoo_options(race_name, fig3_gender)
                        .into_iter()
                        .map(|(index, _)| index),
                );
                let scale = labels_to_choices(look.labels(
                    race_name,
                    fig3_gender,
                    AppearanceControl::Scale,
                ));
                let morphs = labels_to_choices(look.labels(
                    race_name,
                    fig3_gender,
                    AppearanceControl::Morphs,
                ));

                if !faces.is_empty()
                    || !hair_styles.is_empty()
                    || !tattoos.is_empty()
                    || !skin_tones.is_empty()
                    || !eyes.is_empty()
                {
                    by_identity.insert(
                        (race, database_gender),
                        AppearanceChoices {
                            faces,
                            hair_styles,
                            tattoos,
                            skin_tones,
                            eye_colours: eyes,
                            hair_colours,
                            scale,
                            morphs,
                        },
                    );
                }
            }
        }
        Self { by_identity }
    }

    /// Source choices for the currently selected identity.
    #[must_use]
    pub fn choices(&self, race: u8, database_gender: u8) -> Option<&AppearanceChoices> {
        self.by_identity.get(&(race, database_gender & 1))
    }

    /// The next valid source index, preserving any authored holes and the all-zero default.
    #[must_use]
    pub fn cycle(
        &self,
        race: u8,
        database_gender: u8,
        selector: AppearanceSelector,
        current: u8,
        direction: i8,
    ) -> Option<u8> {
        let choices = self.choices(race, database_gender)?;
        let values = match selector {
            AppearanceSelector::Face => &choices.faces,
            AppearanceSelector::HairStyle => &choices.hair_styles,
            AppearanceSelector::Tattoo => &choices.tattoos,
        };
        cycle_indices(values, current, direction)
    }

    /// The next valid source size/height index for an identity.
    #[must_use]
    pub fn cycle_scale(
        &self,
        race: u8,
        database_gender: u8,
        current: u8,
        direction: i8,
    ) -> Option<u8> {
        cycle_indices(
            &self.choices(race, database_gender)?.scale,
            current,
            direction,
        )
    }

    /// Whether the source gives this identity a visible discrete selector at all.
    ///
    /// The retail form contains a tattoo *placeholder*, but its adapter removes that row for
    /// identities with no decal map (for example a male Briton).  Keeping this question beside
    /// the parsed table prevents the HUD from painting a dead generic control over a valid
    /// source-backed draft.
    #[must_use]
    pub fn has_selector(
        &self,
        race: u8,
        database_gender: u8,
        selector: AppearanceSelector,
    ) -> bool {
        self.choices(race, database_gender)
            .is_some_and(|choices| choices.has_selector(selector))
    }

    /// Does one visible palette cell map to a real source value for this identity?
    ///
    /// Hair sheets are authored per hairstyle.  `hair_style == 0` is retail's uncustomized
    /// state, so [`AppearanceChoices::palette_values`] deliberately falls back to the first
    /// available style only for drawing/selection before a hairstyle has been chosen.
    #[must_use]
    pub fn allows_palette(
        &self,
        race: u8,
        database_gender: u8,
        palette: u8,
        value: u8,
        hair_style: u8,
    ) -> bool {
        self.choices(race, database_gender)
            .and_then(|choices| choices.palette_values(palette, hair_style))
            .is_some_and(|values| values.contains(&value))
    }
}

impl AppearanceChoices {
    /// Whether the given arrow selector has at least one authored, selectable value.
    #[must_use]
    pub fn has_selector(&self, selector: AppearanceSelector) -> bool {
        !self.selector_values(selector).is_empty()
    }

    #[must_use]
    pub fn selector_values(&self, selector: AppearanceSelector) -> &[AppearanceChoice] {
        match selector {
            AppearanceSelector::Face => &self.faces,
            AppearanceSelector::HairStyle => &self.hair_styles,
            AppearanceSelector::Tattoo => &self.tattoos,
        }
    }

    #[must_use]
    pub fn selector_label(&self, selector: AppearanceSelector, index: u8) -> Option<&str> {
        self.selector_values(selector)
            .iter()
            .find(|choice| choice.index == index)
            .and_then(|choice| choice.label.as_deref())
    }

    #[must_use]
    pub fn skin_tones(&self) -> &[u8] {
        &self.skin_tones
    }

    #[must_use]
    pub fn eye_colours(&self) -> &[u8] {
        &self.eye_colours
    }

    #[must_use]
    pub fn hair_colours(&self, style: u8) -> &[u8] {
        self.hair_colours
            .get(&style)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Source values for a visible image picker.
    ///
    /// Skin and eye tables are identity-wide. Hair is tied to a style, and default style zero
    /// has no corresponding table entry; while retail displays the picker in that default state,
    /// it uses the first available style's palette.  We preserve that behaviour without making
    /// up a generic `1..=16` range.
    #[must_use]
    pub fn palette_values(&self, palette: u8, hair_style: u8) -> Option<&[u8]> {
        match palette {
            0 => Some(&self.skin_tones),
            1 => Some(&self.eye_colours),
            2 => self
                .hair_colours
                .get(&hair_style)
                .filter(|values| !values.is_empty())
                .map(Vec::as_slice)
                .or_else(|| {
                    self.hair_colours
                        .values()
                        .find(|values| !values.is_empty())
                        .map(Vec::as_slice)
                }),
            _ => None,
        }
    }

    /// Is a one-based image-picker index source-valid for the current selector state?
    #[must_use]
    pub fn allows_palette(&self, palette: u8, value: u8, hair_style: u8) -> bool {
        self.palette_values(palette, hair_style)
            .is_some_and(|values| values.contains(&value))
    }

    /// Source label for an authored size/height index.
    #[must_use]
    pub fn scale_label(&self, index: u8) -> Option<&str> {
        self.scale
            .iter()
            .find(|choice| choice.index == index)
            .and_then(|choice| choice.label.as_deref())
    }

    #[must_use]
    pub fn scale(&self) -> &[AppearanceChoice] {
        &self.scale
    }

    #[must_use]
    pub fn morphs(&self) -> &[AppearanceChoice] {
        &self.morphs
    }

    /// The source label for one of `character_customize.xml`'s four zero-based morph slots.
    ///
    /// `fig3descriptions.csv` numbers its Morphs rows 1..=4 while the form and packed protocol
    /// use slots 0..=3.  Keeping that conversion here prevents a UI row from silently borrowing
    /// the next race-specific label (Firbolg's Ears/Jaw Length are the useful falsifier).
    #[must_use]
    pub fn morph_label(&self, slot: u8) -> Option<&str> {
        let source_index = slot.checked_add(1)?;
        self.morphs
            .iter()
            .find(|choice| choice.index == source_index)
            .and_then(|choice| choice.label.as_deref())
    }
}

fn labels_to_choices(labels: Vec<AppearanceLabel>) -> Vec<AppearanceChoice> {
    labels
        .into_iter()
        .map(|label| AppearanceChoice {
            index: label.index,
            label: Some(label.label),
        })
        .collect()
}

fn merge_choices(
    labels: Vec<AppearanceLabel>,
    indices: impl IntoIterator<Item = u8>,
) -> Vec<AppearanceChoice> {
    let mut values: BTreeMap<u8, Option<String>> = labels
        .into_iter()
        .map(|label| (label.index, Some(label.label)))
        .collect();
    for index in indices {
        values.entry(index).or_insert(None);
    }
    values
        .into_iter()
        .map(|(index, label)| AppearanceChoice { index, label })
        .collect()
}

fn cycle_indices(values: &[AppearanceChoice], current: u8, direction: i8) -> Option<u8> {
    if values.is_empty() || direction == 0 {
        return None;
    }
    if direction < 0 {
        values
            .iter()
            .rev()
            .find(|value| value.index < current)
            .map(|value| value.index)
            .or(Some(0))
    } else {
        values
            .iter()
            .find(|value| value.index > current)
            .map(|value| value.index)
            .or_else(|| values.last().map(|value| value.index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices(indices: &[u8]) -> AppearanceChoices {
        AppearanceChoices {
            faces: indices
                .iter()
                .copied()
                .map(|index| AppearanceChoice { index, label: None })
                .collect(),
            ..AppearanceChoices::default()
        }
    }

    #[test]
    fn cycling_preserves_source_holes_and_returns_to_default() {
        let mut catalog = AppearanceCatalog::default();
        catalog.by_identity.insert((9, 0), choices(&[1, 2, 5, 8]));

        assert_eq!(catalog.cycle(9, 0, AppearanceSelector::Face, 0, 1), Some(1));
        assert_eq!(
            catalog.cycle(9, 0, AppearanceSelector::Face, 2, 1),
            Some(5),
            "the authored 3/4 hole must not become a different face"
        );
        assert_eq!(
            catalog.cycle(9, 0, AppearanceSelector::Face, 5, -1),
            Some(2)
        );
        assert_eq!(
            catalog.cycle(9, 0, AppearanceSelector::Face, 1, -1),
            Some(0),
            "left from the first source choice restores retail's default byte"
        );
    }
}
