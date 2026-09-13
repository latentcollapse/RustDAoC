//! The observed runtime contract for the character-customisation plate.
//!
//! The loose `pregame/character_customize.xml` is an older, palette-oriented layout.  The
//! production character creator shown by the retail client uses the same chrome and source
//! catalogues, but presents the values as a compact series of text selectors and sliders.  This
//! module is deliberately the single owner of that *runtime* profile: drawing, hit-testing,
//! random locks, and product dispatch must name the same field in the same row.
//!
//! Keeping this separate from the static form prevents a tempting but false equivalence: an
//! image-picker rectangle in the archive is not evidence that the live character creator should
//! draw colour swatches.  The profile below is derived from the checked retail screen captures
//! and is covered by layout and runtime tests at its consumers.

/// First customiser row in the 1024×768 pregame authoring space.
pub const FIRST_ROW_Y: f32 = 123.0;
/// Vertical separation between live customiser rows.
pub const ROW_PITCH: f32 = 40.0;
/// Left edge of a field's value widget.
pub const CONTROL_X: f32 = 826.0;
/// Width shared by text selectors and sliders.
pub const CONTROL_WIDTH: f32 = 155.0;
/// Left/right selector arrow dimensions from the pregame button template.
pub const ARROW_SIZE: (f32, f32) = (16.0, 16.0);
/// Centre label geometry between the two arrows.
pub const VALUE_X: f32 = 837.0;
pub const VALUE_WIDTH: f32 = 130.0;
/// Left edge of customiser labels.
pub const LABEL_X: f32 = 780.0;
/// Right-side random-lock position.
pub const LOCK_X: f32 = 992.0;
/// Lock button dimensions from the pregame template.
pub const LOCK_SIZE: (f32, f32) = (16.0, 16.0);
/// The observed slider range: nine visual positions, zero through eight.
pub const SLIDER_MAX_TICK: u8 = 8;

/// One independently mutable character-customisation field.
///
/// This semantic enum replaces the previous raw packet-byte / palette-number pairing.  A row can
/// no longer silently become "Tattoo" in the renderer while dispatch changes `MoodType`, or draw
/// a colour-cell grid whose hit-test mutates a different packed component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CustomizerField {
    Face,
    Morph(u8),
    Mood,
    EyeColor,
    SkinTone,
    HairStyle,
    HairColor,
    Tattoo,
    Size,
}

impl CustomizerField {
    /// Whether this value names an actual row in the runtime contract.
    ///
    /// The enum carries a numeric morph slot because packet ownership does.  Validate that slot
    /// before using it as a lock bit so malformed input cannot alias Jaw/Chin's lock.
    #[must_use]
    pub const fn is_runtime_field(self) -> bool {
        match self {
            Self::Morph(slot) => slot <= 3,
            _ => true,
        }
    }

    /// Stable random-lock bit for this semantic field.
    ///
    /// The runtime profile adds Mood as its own row, so it intentionally owns a separate bit;
    /// the palette-era 11-slot numbering cannot be reused without making two visible rows share
    /// one lock.
    #[must_use]
    pub const fn lock_slot(self) -> u8 {
        match self {
            Self::Face => 0,
            Self::Morph(slot) => {
                if slot > 3 {
                    4
                } else {
                    1 + slot
                }
            }
            Self::Mood => 5,
            Self::EyeColor => 6,
            Self::SkinTone => 7,
            Self::HairStyle => 8,
            Self::HairColor => 9,
            Self::Tattoo => 10,
            Self::Size => 11,
        }
    }

    /// Static field label.  Morph labels are supplied by `fig3descriptions.csv` for the selected
    /// identity, so they deliberately have no generic display label here.
    #[must_use]
    pub const fn label(self) -> Option<&'static str> {
        match self {
            Self::Face => Some("Face"),
            Self::Morph(_) => None,
            Self::Mood => Some("Mood"),
            Self::EyeColor => Some("Eye Color"),
            Self::SkinTone => Some("Skin Tone"),
            Self::HairStyle => Some("Hair Style"),
            Self::HairColor => Some("Hair Color"),
            Self::Tattoo => Some("Tattoo"),
            Self::Size => Some("Size"),
        }
    }

    #[must_use]
    pub const fn is_slider(self) -> bool {
        matches!(self, Self::Morph(_) | Self::Mood | Self::SkinTone)
    }
}

/// How one runtime row presents its current source value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomizerWidget {
    /// Left/right arrows around one textual label.
    TextSelector,
    /// The retail `generic_horizontal_slider` art, with an inclusive zero-based tick range.
    Slider { max_tick: u8 },
}

/// Geometry and semantic ownership for one visible runtime row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustomizerRow {
    pub field: CustomizerField,
    pub widget: CustomizerWidget,
    /// Design-space y coordinate shared by label, control, and lock.
    pub y: i16,
}

impl CustomizerRow {
    #[must_use]
    pub const fn y_f32(self) -> f32 {
        self.y as f32
    }
}

/// Return every visible row in the retail runtime order for one identity.
///
/// Tattoo is genuinely conditional, not a blank or disabled placeholder.  When absent, Size
/// closes the gap — this is visible in the Midgard/Albion references and prevents a dead lock at
/// the former Tattoo location.
#[must_use]
pub fn runtime_rows(has_tattoo: bool) -> Vec<CustomizerRow> {
    let mut fields = vec![
        (CustomizerField::Face, CustomizerWidget::TextSelector),
        (
            CustomizerField::Morph(0),
            CustomizerWidget::Slider {
                max_tick: SLIDER_MAX_TICK,
            },
        ),
        (
            CustomizerField::Morph(1),
            CustomizerWidget::Slider {
                max_tick: SLIDER_MAX_TICK,
            },
        ),
        (
            CustomizerField::Morph(2),
            CustomizerWidget::Slider {
                max_tick: SLIDER_MAX_TICK,
            },
        ),
        (
            CustomizerField::Morph(3),
            CustomizerWidget::Slider {
                max_tick: SLIDER_MAX_TICK,
            },
        ),
        (
            CustomizerField::Mood,
            CustomizerWidget::Slider {
                max_tick: SLIDER_MAX_TICK,
            },
        ),
        (CustomizerField::EyeColor, CustomizerWidget::TextSelector),
        (
            CustomizerField::SkinTone,
            CustomizerWidget::Slider {
                max_tick: SLIDER_MAX_TICK,
            },
        ),
        (CustomizerField::HairStyle, CustomizerWidget::TextSelector),
        (CustomizerField::HairColor, CustomizerWidget::TextSelector),
    ];
    if has_tattoo {
        fields.push((CustomizerField::Tattoo, CustomizerWidget::TextSelector));
    }
    fields.push((CustomizerField::Size, CustomizerWidget::TextSelector));

    fields
        .into_iter()
        .enumerate()
        .map(|(index, (field, widget))| CustomizerRow {
            field,
            widget,
            y: (FIRST_ROW_Y + index as f32 * ROW_PITCH) as i16,
        })
        .collect()
}

/// Find one visible runtime row without re-creating its geometry at the call site.
#[must_use]
pub fn runtime_row(field: CustomizerField, has_tattoo: bool) -> Option<CustomizerRow> {
    runtime_rows(has_tattoo)
        .into_iter()
        .find(|row| row.field == field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_profile_is_the_observed_text_and_slider_contract() {
        let rows = runtime_rows(true);
        assert_eq!(rows.len(), 12, "Celt-style identity has Tattoo and Size");
        assert_eq!(rows[0].field, CustomizerField::Face);
        assert_eq!(rows[5].field, CustomizerField::Mood);
        assert_eq!(rows[6].field, CustomizerField::EyeColor);
        assert_eq!(rows[7].field, CustomizerField::SkinTone);
        assert_eq!(rows[10].field, CustomizerField::Tattoo);
        assert_eq!(rows[11].field, CustomizerField::Size);
        assert_eq!(rows[11].y_f32(), 563.0);
        assert!(rows.iter().all(|row| matches!(
            row.widget,
            CustomizerWidget::TextSelector | CustomizerWidget::Slider { .. }
        )));
        assert!(rows
            .iter()
            .filter(|row| row.field.is_slider())
            .all(|row| row.widget == CustomizerWidget::Slider { max_tick: 8 }));
    }

    #[test]
    fn no_tattoo_identity_closes_the_gap_before_size() {
        let rows = runtime_rows(false);
        assert_eq!(rows.len(), 11);
        assert!(rows.iter().all(|row| row.field != CustomizerField::Tattoo));
        assert_eq!(
            runtime_row(CustomizerField::Size, false)
                .expect("Size row")
                .y_f32(),
            523.0
        );
    }
}
