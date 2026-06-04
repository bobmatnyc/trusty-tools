//! Tests for the voice module: types, principles, loader, and VoiceConfig layering.
//!
//! Why: comprehensive coverage ensures the 3-layer pipeline (stock→principles→voice)
//! is assembled correctly, the bundled duetto fixture parses cleanly, and all
//! fallback paths (no voice, principles-only, full stack) work as specified.
//! What: unit tests for VoicePackage serde, principles content, VoiceLoader
//! discovery (bundled + filesystem), and VoiceConfig composition.
//! Test: this is the test module; included via `#[path = "tests.rs"]` from mod.rs.

use super::{loader::VoiceLoader, principles::principles_addendum, types::*};

// ── VoicePackage serde round-trip ─────────────────────────────────────────────

/// Verify a hand-crafted TOML round-trips through VoicePackage correctly.
///
/// Why: serde derivation on nested structs can silently miss fields; this
/// ensures every section deserialises with correct field names.
/// What: parses a minimal-but-complete voice.toml string and asserts key fields.
/// Test: no network, no filesystem.
#[test]
fn voice_package_roundtrip_serde() {
    let toml = r#"
[meta]
name = "test"
version = "1.0.0"
description = "A test voice"
generated_at = "2026-06-04"
pipeline_version = "tr-voice-0.1"
corpus_cutoff = "2025-01-01"
llm_model = "opus"

[meta.provenance]
source_reviewers = ["Alice", "Bob"]
reviewers_count = 2
total_comments_analyzed = 100
notes = "Test corpus"

[voice]
system_addendum = "Focus on correctness above all."

[knobs]
tone = "formal"
block_bias = "strict"
comment_length = "verbose"

[custom]
extra_addendum = "Also check for performance regressions."
"#;

    let pkg: VoicePackage = toml::from_str(toml).expect("should parse test voice.toml");
    assert_eq!(pkg.meta.name, "test");
    assert_eq!(pkg.meta.version, "1.0.0");
    assert_eq!(pkg.meta.provenance.reviewers_count, 2);
    assert_eq!(pkg.meta.provenance.total_comments_analyzed, 100);
    assert!(
        pkg.voice.system_addendum.contains("correctness"),
        "voice addendum must be present"
    );
    assert_eq!(pkg.knobs.tone, "formal");
    assert_eq!(pkg.knobs.block_bias, "strict");
    assert!(
        pkg.custom.extra_addendum.contains("performance"),
        "custom extra_addendum must be present"
    );
}

// ── VoicePackage::effective_addendum ─────────────────────────────────────────

/// Base-only: effective_addendum returns the generated voice addendum.
///
/// Why: the most common case — no custom override.
/// What: creates a package with only [voice]; asserts effective == voice.system_addendum.
/// Test: no filesystem.
#[test]
fn effective_addendum_base_only() {
    let pkg = VoicePackage {
        voice: VoiceSection {
            system_addendum: "Review carefully.".to_string(),
        },
        ..Default::default()
    };
    assert_eq!(pkg.effective_addendum(), "Review carefully.");
}

/// Custom extra_addendum is appended after the base.
///
/// Why: user hand-edits must survive synthesis re-runs and be appended.
/// What: sets extra_addendum; asserts it appears after the base in the output.
/// Test: no filesystem.
#[test]
fn effective_addendum_custom_append() {
    let pkg = VoicePackage {
        voice: VoiceSection {
            system_addendum: "Base review guidance.".to_string(),
        },
        custom: CustomOverlay {
            extra_addendum: "Additional org-specific rule.".to_string(),
            voice: None,
        },
        ..Default::default()
    };
    let effective = pkg.effective_addendum();
    let base_pos = effective.find("Base review").unwrap();
    let extra_pos = effective.find("Additional org").unwrap();
    assert!(
        base_pos < extra_pos,
        "extra_addendum must come AFTER the base"
    );
    assert!(
        effective.contains("Base review guidance."),
        "base must be present"
    );
    assert!(
        effective.contains("Additional org-specific rule."),
        "extra must be present"
    );
}

/// Custom voice.system_addendum REPLACES the generated base.
///
/// Why: advanced users who want full control can override the entire addendum.
/// What: sets custom.voice.system_addendum; asserts it replaces the base.
/// Test: no filesystem.
#[test]
fn effective_addendum_custom_replace() {
    let pkg = VoicePackage {
        voice: VoiceSection {
            system_addendum: "Generated base.".to_string(),
        },
        custom: CustomOverlay {
            extra_addendum: String::new(),
            voice: Some(CustomVoiceOverride {
                system_addendum: "Full user replacement.".to_string(),
            }),
        },
        ..Default::default()
    };
    let effective = pkg.effective_addendum();
    assert!(
        !effective.contains("Generated base"),
        "custom replace must remove the generated base"
    );
    assert!(
        effective.contains("Full user replacement"),
        "custom replacement must be present"
    );
}

// ── Principles layer ──────────────────────────────────────────────────────────

/// Principles addendum is non-empty and contains expected concepts.
///
/// Why: a silent empty principles layer would silently degrade review quality
/// without triggering any error.
/// What: asserts the principles text covers the key concepts from #756.
/// Test: no network, no filesystem.
#[test]
fn principles_addendum_is_non_empty() {
    let p = principles_addendum();
    assert!(!p.is_empty(), "principles addendum must not be empty");
    assert!(p.len() > 100, "principles addendum must be substantive");
}

/// Principles addendum contains key concepts from the #756 research synthesis.
///
/// Why: ensures the distilled guidance is actually present and not accidentally
/// replaced with placeholder text.
/// What: spot-checks for concepts from Google Eng Practices and Conventional
/// Comments that should appear in the injected text.
/// Test: no network.
#[test]
fn principles_contains_key_concepts() {
    let p = principles_addendum();
    // Priority ordering (design/correctness first).
    assert!(
        p.contains("correctness") || p.contains("design"),
        "principles must mention design/correctness priority"
    );
    // Severity / BLOCK guidance.
    assert!(
        p.contains("BLOCK"),
        "principles must reference BLOCK severity"
    );
    // Actionable feedback guidance.
    assert!(
        p.contains("actionable") || p.contains("explain"),
        "principles must reference actionable feedback"
    );
    // Conventional Comments (or equivalent label guidance).
    assert!(
        p.to_lowercase().contains("label")
            || p.to_lowercase().contains("nitpick")
            || p.to_lowercase().contains("suggestion"),
        "principles must reference comment labeling (Conventional Comments)"
    );
    // Scope discipline.
    assert!(
        p.to_lowercase().contains("scope"),
        "principles must reference scope discipline"
    );
}

// ── VoiceLoader — bundled fixtures ────────────────────────────────────────────

/// The bundled duetto voice loads correctly and has expected content.
///
/// Why: the embedded `include_str!` fixture must parse cleanly at runtime; if
/// the TOML is malformed the crate would panic on any review that selects the
/// voice.
/// What: loads `duetto` via `VoiceLoader::new()` and asserts key fields.
/// Test: uses compile-time embedded fixture; no filesystem I/O to external paths.
#[test]
fn load_bundled_duetto_voice() {
    let loader = VoiceLoader::new();
    // This call hits the bundled fixture (no ~/.config/trusty-review/voices/duetto
    // required for the test to pass).
    let pkg = loader.load("duetto").expect("bundled duetto must load");
    assert_eq!(pkg.meta.name, "duetto", "meta.name must be duetto");
    assert!(
        !pkg.voice.system_addendum.is_empty(),
        "duetto voice.system_addendum must be non-empty"
    );
    assert!(
        pkg.voice.system_addendum.contains("correctness"),
        "duetto addendum must mention correctness"
    );
    // Provenance sanity.
    assert!(
        pkg.meta.provenance.reviewers_count >= 5,
        "duetto must have >= 5 source reviewers"
    );
}

/// Loading a non-existent voice returns NotFound.
///
/// Why: callers must be able to detect and handle a missing voice gracefully
/// (degrade to stock rather than crash).
/// What: asks the loader for "nonexistent-xyz" and asserts NotFound.
/// Test: no filesystem writes.
#[test]
fn load_missing_voice_errors() {
    let loader = VoiceLoader::new();
    let err = loader
        .load("nonexistent-voice-xyz-abc")
        .expect_err("missing voice must produce an error");
    // Must be NotFound, not a parse or I/O error.
    assert!(
        matches!(err, super::loader::VoiceLoaderError::NotFound { .. }),
        "error must be NotFound, got: {err}"
    );
    assert!(
        err.to_string().contains("nonexistent-voice-xyz-abc"),
        "error message must include the requested name"
    );
}

/// Loading from a custom (temp) directory works and takes priority.
///
/// Why: the extra_dirs mechanism must let tests inject fixtures and operators
/// override the bundled defaults without touching ~/.config.
/// What: writes a minimal voice.toml to a temp dir, injects it via
/// `VoiceLoader::with_extra_dirs`, and verifies it is found.
/// Test: tempfile-based, no network.
#[test]
fn load_from_custom_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let voice_dir = dir.path().join("myvoice");
    std::fs::create_dir_all(&voice_dir).unwrap();
    std::fs::write(
        voice_dir.join("voice.toml"),
        r#"
[meta]
name = "myvoice"
version = "0.1.0"

[voice]
system_addendum = "Custom guidance for tests."
"#,
    )
    .unwrap();

    let loader = VoiceLoader::with_extra_dirs(vec![dir.path().to_path_buf()]);
    let pkg = loader.load("myvoice").expect("custom voice must load");
    assert_eq!(pkg.meta.name, "myvoice");
    assert!(pkg.voice.system_addendum.contains("Custom guidance"));
}

/// Custom dir takes priority over the bundled fixture for the same name.
///
/// Why: operators must be able to override bundled packages without changing
/// the binary.
/// What: writes a duetto voice.toml to a temp dir with different content;
/// asserts the loader returns the custom version, not the bundled one.
/// Test: tempfile-based.
#[test]
fn custom_dir_takes_priority_over_bundled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let voice_dir = dir.path().join("duetto");
    std::fs::create_dir_all(&voice_dir).unwrap();
    std::fs::write(
        voice_dir.join("voice.toml"),
        r#"
[meta]
name = "duetto"
version = "99.0.0"

[voice]
system_addendum = "Overridden for test."
"#,
    )
    .unwrap();

    let loader = VoiceLoader::with_extra_dirs(vec![dir.path().to_path_buf()]);
    let pkg = loader.load("duetto").expect("custom duetto must load");
    // The custom version has version 99.0.0, not the bundled 0.1.0.
    assert_eq!(
        pkg.meta.version, "99.0.0",
        "custom version must take priority"
    );
    assert!(
        pkg.voice.system_addendum.contains("Overridden"),
        "custom addendum must take priority"
    );
}

/// list() always includes the bundled duetto.
///
/// Why: `voice list` must show duetto even when the user-config dir is absent.
/// What: constructs a loader with an empty extra_dirs list, calls list(),
/// and asserts "duetto" is present.
/// Test: no filesystem writes.
#[test]
fn list_includes_bundled_duetto() {
    let loader = VoiceLoader::new();
    let names = loader.list();
    assert!(
        names.contains(&"duetto".to_string()),
        "list must include bundled duetto; got: {names:?}"
    );
}

/// list() includes voices from extra_dirs.
///
/// Why: the list command must show both bundled and user-installed voices.
/// What: writes two voices to a temp dir, asserts both appear in the list.
/// Test: tempfile-based.
#[test]
fn list_includes_extra_dir_voices() {
    let dir = tempfile::tempdir().expect("tempdir");
    for name in ["alpha", "beta"] {
        let vd = dir.path().join(name);
        std::fs::create_dir_all(&vd).unwrap();
        std::fs::write(
            vd.join("voice.toml"),
            format!("[meta]\nname = \"{name}\"\n"),
        )
        .unwrap();
    }
    let loader = VoiceLoader::with_extra_dirs(vec![dir.path().to_path_buf()]);
    let names = loader.list();
    assert!(names.contains(&"alpha".to_string()));
    assert!(names.contains(&"beta".to_string()));
    assert!(names.contains(&"duetto".to_string())); // always present
}

// ── VoiceConfig composition tests ────────────────────────────────────────────
// Extracted to tests_voice_config.rs to keep this file under the 500-line cap (#610).

// ── Integration tests (duetto + drift-check) ─────────────────────────────────
// Extracted to tests_integration.rs to keep this file under the 500-line cap (#610).
