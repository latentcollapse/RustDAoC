//! S2: analysis/developer bins must not build on a focused product-test invocation.
//!
//! Named falsifier: removing `required-features = ["analysis-bins"]` from an analysis
//! `[[bin]]` in caer-render/Cargo.toml turns this red.

const CARGO_TOML: &str = include_str!("../Cargo.toml");

const ANALYSIS_BINS: &[&str] = &[
    "m9_frametime",
    "worldprobe",
    "uilayout",
    "rigcompare",
    "mobground",
    "clipsweep",
    "avatarcover",
    "animcover",
];

#[test]
fn analysis_bins_require_analysis_bins_feature() {
    assert!(
        CARGO_TOML.contains("autobins = false"),
        "caer-render must disable bin autodisovery so src/bin/* cannot sneak into cargo test"
    );
    for name in ANALYSIS_BINS {
        let needle = format!("name = \"{name}\"");
        assert!(
            CARGO_TOML.contains(&needle),
            "analysis bin `{name}` must be an explicit [[bin]]"
        );
        let after = CARGO_TOML
            .split(&needle)
            .nth(1)
            .unwrap_or("")
            .split("[[bin]]")
            .next()
            .unwrap_or("");
        assert!(
            after.contains("required-features = [\"analysis-bins\"]"),
            "analysis bin `{name}` must require feature analysis-bins; section was:\n{after}"
        );
    }
    assert!(
        CARGO_TOML.contains("name = \"rustdaoc\""),
        "rustdaoc stays a default product bin"
    );
    let rustdaoc_section = CARGO_TOML
        .split("name = \"rustdaoc\"")
        .nth(1)
        .unwrap_or("")
        .split("[[bin]]")
        .next()
        .unwrap_or("");
    assert!(
        !rustdaoc_section.contains("required-features"),
        "rustdaoc must remain runnable without extra features"
    );
}
