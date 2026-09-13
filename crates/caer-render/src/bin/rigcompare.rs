//! `rigcompare` — group authored avatar skeletons without collapsing different hierarchies.

use std::error::Error;

use caer_assets::figures::{first_parseable_skeleton, skeleton_archives, FigureModels};
use caer_render::rig_family::{
    complete_link_groups, RigTolerance, COMPARISON_BASIS, DEFAULT_TOLERANCE,
};

fn main() -> Result<(), Box<dyn Error>> {
    let tolerance = configured_tolerance()?;
    let root = caer_assets::client_dep::caer_client_root()
        .map_err(|reason| format!("rigcompare requires CAER_CLIENT: {reason}"))?;
    let archives = skeleton_archives(&root)?;
    if archives.is_empty() {
        return Err(format!("no sfig*.mpk archives under {}", root.display()).into());
    }

    println!("rigcompare: {}", root.display());
    println!("basis: {COMPARISON_BASIS}");
    println!(
        "tolerance: translation <= {:.4}; rotation component <= {:.6}; scale <= {:.6}",
        tolerance.max_local_translation_delta,
        tolerance.max_rotation_component_delta,
        tolerance.max_scale_delta,
    );

    for gender in [
        caer_assets::figures::GENDER_MALE,
        caer_assets::figures::GENDER_FEMALE,
    ] {
        let gender_name = if gender == caer_assets::figures::GENDER_FEMALE {
            "Female"
        } else {
            "Male"
        };
        let mut loaded = Vec::new();
        println!("\n================ {gender_name} ================");

        for race in 1..=18u8 {
            let (Some(name), Some(member)) = (
                FigureModels::race_name(race),
                FigureModels::skeleton_member(race, gender),
            ) else {
                continue;
            };
            let lookup = first_parseable_skeleton(&archives, &member);
            for (archive, error) in &lookup.rejected_archives {
                eprintln!(
                    "rigcompare: {member} in {} rejected ({error}); continuing the renderer's scan",
                    archive.display()
                );
            }
            match lookup.skeleton {
                Some(skeleton) => loaded.push((name.to_string(), skeleton)),
                None => eprintln!("rigcompare: no parseable {member} for {name} {gender_name}"),
            }
        }

        let references: Vec<(&str, &caer_assets::nif::Skeleton)> = loaded
            .iter()
            .map(|(name, skeleton)| (name.as_str(), skeleton))
            .collect();
        let groups = complete_link_groups(&references, tolerance)?;
        println!(
            "-- TOLERANCE-DERIVED RIG FAMILIES (complete link; every pair fits the limits above) --"
        );
        for group in &groups {
            let names: Vec<&str> = group
                .members
                .iter()
                .map(|&index| references[index].0)
                .collect();
            let bones = group
                .members
                .first()
                .map(|&index| references[index].1.bones.len())
                .unwrap_or(0);
            let divergence = group.max_divergence;
            println!(
                "  [{bones} bones] {:>2} races: {}\n    max local divergence: translation {:.4}, rotation component {:.6}, scale {:.6}",
                names.len(),
                names.join(", "),
                divergence.max_local_translation_delta,
                divergence.max_rotation_component_delta,
                divergence.max_scale_delta,
            );
        }
        println!(
            "  => {} tolerance-derived families across {} loaded races; this is a repair-scoping report, not a source-art identity claim",
            groups.len(),
            loaded.len(),
        );
    }
    Ok(())
}

fn configured_tolerance() -> Result<RigTolerance, Box<dyn Error>> {
    fn number(name: &str, default: f32) -> Result<f32, Box<dyn Error>> {
        match std::env::var(name) {
            Ok(value) => value
                .parse::<f32>()
                .map_err(|error| format!("{name}={value:?} is not a number: {error}").into()),
            Err(std::env::VarError::NotPresent) => Ok(default),
            Err(error) => Err(format!("cannot read {name}: {error}").into()),
        }
    }

    RigTolerance {
        max_local_translation_delta: number(
            "CAER_RIG_TOL",
            DEFAULT_TOLERANCE.max_local_translation_delta,
        )?,
        max_rotation_component_delta: number(
            "CAER_RIG_ROTATION_TOL",
            DEFAULT_TOLERANCE.max_rotation_component_delta,
        )?,
        max_scale_delta: number("CAER_RIG_SCALE_TOL", DEFAULT_TOLERANCE.max_scale_delta)?,
    }
    .validate()
    .map_err(Into::into)
}
