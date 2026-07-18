//! VoiceConfig composition tests.
//!
//! Why: extracted from tests.rs to keep that file under the 500-line cap (#610).
//! What: tests for stock_only(), default_production(), combined_addendum(),
//! has_any_addendum(), and their various combinations.
//! Test: included via `#[path = "tests_voice_config.rs"]` from mod.rs.

use super::VoiceConfig;

/// stock_only() gives an empty VoiceConfig — no addenda.
///
/// Why: stock_only() is used in tests and legacy code paths that don't want
/// any layering; asserting it's truly zero-addendum prevents accidental injection.
/// What: asserts has_any_addendum() is false and combined_addendum() is empty.
/// Test: no filesystem.
#[test]
fn voice_config_none_gives_stock_only() {
    let vc = VoiceConfig::stock_only();
    assert!(!vc.has_any_addendum(), "stock_only must have no addenda");
    assert_eq!(
        vc.combined_addendum(),
        "",
        "stock_only combined_addendum must be empty"
    );
    assert!(
        vc.voice_name.is_none(),
        "stock_only must have no voice_name"
    );
    assert!(
        vc.principles.is_none(),
        "stock_only must have no principles"
    );
    assert!(
        vc.voice_addendum.is_none(),
        "stock_only must have no voice_addendum"
    );
}

/// default_production() enables principles and no voice.
///
/// Why: issue #756 specifies principles is default-on (universal/safe); voice
/// is opt-in.  This test guards against regression.
/// What: asserts principles is Some and non-empty; voice_addendum is None.
/// Test: no filesystem.
#[test]
fn voice_config_production_default_enables_principles() {
    let vc = VoiceConfig::default_production();
    assert!(
        vc.principles.is_some(),
        "default_production must have principles"
    );
    assert!(
        !vc.principles.as_deref().unwrap_or("").is_empty(),
        "default_production principles must be non-empty"
    );
    assert!(
        vc.voice_addendum.is_none(),
        "default_production must have no voice_addendum"
    );
    assert!(
        vc.has_any_addendum(),
        "default_production must report has_any_addendum=true"
    );
}

/// combined_addendum() joins principles then voice in the correct order.
///
/// Why: the order matters — principles sets the universal baseline, voice
/// overlays the org-specific tone on top.
/// What: sets both layers; asserts principles appears before voice in output.
/// Test: no filesystem.
#[test]
fn voice_config_combined_addendum_ordering() {
    let vc = VoiceConfig {
        principles: Some("Principles text.".to_string()),
        voice_addendum: Some("Voice text.".to_string()),
        voice_name: Some("test".to_string()),
        ..Default::default()
    };
    let combined = vc.combined_addendum();
    let p_pos = combined.find("Principles text").unwrap();
    let v_pos = combined.find("Voice text").unwrap();
    assert!(
        p_pos < v_pos,
        "principles must come before voice in combined_addendum"
    );
    assert!(combined.contains("Principles text."));
    assert!(combined.contains("Voice text."));
}

/// combined_addendum() with only principles omits voice section.
///
/// Why: the default_production config has no voice; the combined output must
/// not have a trailing separator or voice placeholder.
/// What: only principles set; asserts combined == principles text (no extra text).
/// Test: no filesystem.
#[test]
fn voice_config_combined_principles_only() {
    let vc = VoiceConfig {
        principles: Some("Only principles.".to_string()),
        voice_addendum: None,
        voice_name: None,
        ..Default::default()
    };
    let combined = vc.combined_addendum();
    assert_eq!(
        combined, "Only principles.",
        "principles-only combined must equal principles text"
    );
}

/// combined_addendum() with only voice omits principles section.
///
/// Why: an operator may disable principles and enable only a voice package.
/// What: only voice_addendum set; asserts combined == voice text.
/// Test: no filesystem.
#[test]
fn voice_config_combined_voice_only() {
    let vc = VoiceConfig {
        principles: None,
        voice_addendum: Some("Only voice.".to_string()),
        voice_name: Some("myvoice".to_string()),
        ..Default::default()
    };
    let combined = vc.combined_addendum();
    assert_eq!(
        combined, "Only voice.",
        "voice-only combined must equal voice text"
    );
}

/// has_any_addendum() helpers behave correctly for all combinations.
///
/// Why: prompt builder uses this to skip the addendum section entirely;
/// incorrect results would either add spurious blank sections or drop real layers.
/// What: exercises all four combinations of Some/None for principles/voice.
/// Test: no filesystem.
#[test]
fn voice_config_has_addendum_helpers() {
    // Both absent.
    assert!(!VoiceConfig::default().has_any_addendum());
    // Principles only.
    assert!(
        VoiceConfig {
            principles: Some("p".to_string()),
            ..Default::default()
        }
        .has_any_addendum()
    );
    // Voice only.
    assert!(
        VoiceConfig {
            voice_addendum: Some("v".to_string()),
            ..Default::default()
        }
        .has_any_addendum()
    );
    // Both present.
    assert!(
        VoiceConfig {
            principles: Some("p".to_string()),
            voice_addendum: Some("v".to_string()),
            ..Default::default()
        }
        .has_any_addendum()
    );
    // Empty strings treated as absent.
    assert!(
        !VoiceConfig {
            principles: Some(String::new()),
            voice_addendum: Some(String::new()),
            ..Default::default()
        }
        .has_any_addendum()
    );
    // Review template only (#2995).
    assert!(
        VoiceConfig {
            review_template: Some("t".to_string()),
            ..Default::default()
        }
        .has_any_addendum()
    );
}

/// combined_addendum() layers the review template LAST, after principles and
/// voice (#2995).
///
/// Why: the review template is the most project-specific layer; it must read
/// as the final word, composing with (not replacing) the broader layers above
/// it — see `VoiceConfig::combined_addendum`'s doc for the rationale.
/// What: sets all three layers with distinct markers; asserts the document
/// order is principles < voice < review_template.
/// Test: no filesystem.
#[test]
fn voice_config_review_template_layers_last() {
    let vc = VoiceConfig {
        principles: Some("PRINCIPLES_MARKER".to_string()),
        voice_addendum: Some("VOICE_MARKER".to_string()),
        voice_name: Some("test".to_string()),
        review_template: Some("TEMPLATE_MARKER".to_string()),
        review_template_name: Some("strict-security".to_string()),
    };
    let combined = vc.combined_addendum();
    let p_pos = combined.find("PRINCIPLES_MARKER").unwrap();
    let v_pos = combined.find("VOICE_MARKER").unwrap();
    let t_pos = combined.find("TEMPLATE_MARKER").unwrap();
    assert!(p_pos < v_pos, "principles must precede voice");
    assert!(v_pos < t_pos, "voice must precede the review template");
}

/// combined_addendum() with only a review template omits the other sections.
///
/// Why: a project may select a review template without a voice package and
/// with principles disabled.
/// What: only review_template set; asserts combined == template text.
/// Test: no filesystem.
#[test]
fn voice_config_combined_review_template_only() {
    let vc = VoiceConfig {
        review_template: Some("Only template.".to_string()),
        review_template_name: Some("t".to_string()),
        ..Default::default()
    };
    let combined = vc.combined_addendum();
    assert_eq!(
        combined, "Only template.",
        "template-only combined must equal template text"
    );
}
