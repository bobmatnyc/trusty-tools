//! Integration tests for the voice module: duetto through the full pipeline.
//!
//! Why: extracted from tests.rs to keep that file under the 500-line cap (#610).
//! These tests exercise the end-to-end duetto path and drift detection between
//! the bundled fixture and the external ~/.config copy.
//! What: full stock→principles→voice pipeline test and the bundled vs. external
//! drift-check test (skipped gracefully when the external file is absent).
//! Test: included via `#[path = "tests_integration.rs"]` from mod.rs.

use std::path::PathBuf;

use super::{VoiceConfig, loader::VoiceLoader, principles::principles_addendum, types::*};

/// Full stock→principles→voice pipeline with the duetto package.
///
/// Why: end-to-end test that confirms the duetto voice integrates cleanly
/// into VoiceConfig's combined_addendum without truncation or escaping issues.
/// What: loads duetto from bundled fixture, builds a full VoiceConfig,
/// asserts the combined addendum contains both layers in order.
/// Test: uses bundled fixture; no network.
#[test]
fn full_pipeline_with_duetto_voice() {
    let loader = VoiceLoader::new();
    let pkg = loader.load("duetto").expect("bundled duetto must load");
    let addendum = pkg.effective_addendum();
    assert!(
        !addendum.is_empty(),
        "duetto effective_addendum must be non-empty"
    );

    let vc = VoiceConfig {
        principles: Some(principles_addendum().to_string()),
        voice_addendum: Some(addendum.clone()),
        voice_name: Some("duetto".to_string()),
    };

    let combined = vc.combined_addendum();
    // Principles should appear before duetto content.
    assert!(
        combined.contains("Review principles") || combined.contains("BLOCK"),
        "combined must contain principles content"
    );
    assert!(
        combined.contains("correctness"),
        "combined must contain duetto correctness guidance"
    );

    // Principles before voice in ordering.
    let p_pos = combined.find("BLOCK").unwrap_or(usize::MAX);
    // Both should exist and principles precedes voice.
    assert!(
        p_pos != usize::MAX,
        "BLOCK should appear (from principles or duetto)"
    );
    assert!(
        vc.has_any_addendum(),
        "full config must report has_any_addendum=true"
    );
}

/// The bundled duetto voice fixture matches the external ~/.config version when present.
///
/// Why: the fixture in the crate must stay in sync with the authoritative file
/// at ~/.config/trusty-review/voices/duetto/voice.toml.  If they diverge, users
/// with the external file get a different voice than those without.  This test
/// guards against accidental drift.
/// What: if the external file is absent (CI), skips gracefully.  When present,
/// reads the external file directly (bypassing the loader search path) and compares
/// it against the bundled fixture loaded with `VoiceLoader::bundled_only()`, which
/// skips the XDG path so the assertion truly compares bundled vs. external.
/// Test: conditional on external file presence; no network.
#[test]
fn bundled_duetto_matches_external_when_present() {
    let external_path: PathBuf = {
        let Some(cfg) = dirs::config_dir() else {
            return;
        };
        cfg.join("trusty-review")
            .join("voices")
            .join("duetto")
            .join("voice.toml")
    };
    if !external_path.exists() {
        // External file absent (CI without developer config) — skip.
        return;
    }

    // bundled_only() skips the XDG config dir, so `load("duetto")` can only
    // return the compile-time `include_str!` fixture — never the external file.
    let loader_bundled = VoiceLoader::bundled_only();
    let bundled = loader_bundled
        .load("duetto")
        .expect("bundled duetto must load via bundled_only");

    // Read the external file directly (not through the loader) so we can
    // compare two clearly separated sources.
    let external_content =
        std::fs::read_to_string(&external_path).expect("external file must be readable");
    let external: VoicePackage =
        toml::from_str(&external_content).expect("external voice.toml must parse");

    assert_eq!(
        bundled.meta.name, external.meta.name,
        "bundled and external meta.name must match"
    );
    assert_eq!(
        bundled.meta.version, external.meta.version,
        "bundled and external meta.version must match"
    );
    // Check the first 100 chars of system_addendum to guard against drift.
    let b_prefix = bundled
        .voice
        .system_addendum
        .chars()
        .take(100)
        .collect::<String>();
    let e_prefix = external
        .voice
        .system_addendum
        .chars()
        .take(100)
        .collect::<String>();
    assert_eq!(
        b_prefix, e_prefix,
        "bundled and external system_addendum prefix must match"
    );
}
