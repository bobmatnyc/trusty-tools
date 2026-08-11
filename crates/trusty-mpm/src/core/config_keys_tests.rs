//! Tests for the host-level unrecognised-key reporter (#5207).
//!
//! Why: the reporter is derived from a serde round-trip rather than a declared
//! key list, so the cases that matter are the two false-positive shapes that
//! derivation could produce — a `HashMap`-valued section whose keys are
//! arbitrary, and an `Option` field written as an explicit `null` — alongside
//! the true positives it exists for.
//! What: cases against [`super::unknown_key_paths`] using the real
//! [`crate::core::config::MpmConfig`] and
//! [`crate::core::trusty_tools_config::TrustyToolsConfig`] schemas, so the test
//! cannot drift from the structs it guards.

use super::{toml_document, unknown_key_paths, yaml_document};
use crate::core::config::MpmConfig;
use crate::core::trusty_tools_config::TrustyToolsConfig;

/// Parse TOML as both a raw document and an `MpmConfig`, then diff.
fn mpm_unknown(raw: &str) -> Vec<String> {
    let doc = toml_document(raw).expect("valid toml");
    let parsed: MpmConfig = toml::from_str(raw).expect("MpmConfig parses leniently");
    unknown_key_paths(&doc, &parsed)
}

/// Why (#5207): the exact silence the owner ruling complains about — a
/// misspelled top-level section is accepted and dropped with no signal.
#[test]
fn unknown_top_level_key_is_reported() {
    assert_eq!(
        mpm_unknown("[modles]\ndefault = \"opus\"\n"),
        vec!["modles"]
    );
}

/// Why: a typo one level down is the more common and more confusing case —
/// `[models]` is spelled right, so the operator sees a section that "works"
/// while their key does nothing.
#[test]
fn unknown_nested_key_is_reported() {
    assert_eq!(
        mpm_unknown("[models]\ndefualt = \"opus\"\n"),
        vec!["models.defualt"]
    );
}

/// Why: the reporter must be silent on a correct file, or it is noise that
/// trains operators to ignore it.
#[test]
fn known_keys_are_not_reported() {
    assert!(
        mpm_unknown("[models]\ndefault = \"opus\"\n\n[models.tiers]\nopus = \"claude-opus-4-5\"\n")
            .is_empty()
    );
}

/// Why: `models.agents` is a `HashMap<String, String>` whose keys are arbitrary
/// agent names. A naive schema check would flag every one of them. This is the
/// main false positive the round-trip derivation has to survive.
#[test]
fn map_valued_sections_are_not_reported() {
    assert!(
        mpm_unknown("[models.agents]\nengineer = \"haiku\"\nsome-custom-agent = \"opus\"\n")
            .is_empty(),
        "arbitrary map keys are data, not schema violations"
    );
}

/// Why: the report must name EVERY offender, not stop at the first — an
/// operator fixing them one launch at a time is the failure mode this avoids.
#[test]
fn every_unknown_key_is_reported() {
    let unknown = mpm_unknown("[modles]\nx = 1\n\n[models]\ndefualt = \"opus\"\n");
    assert!(unknown.contains(&"modles".to_string()), "{unknown:?}");
    assert!(
        unknown.contains(&"models.defualt".to_string()),
        "{unknown:?}"
    );
}

/// Why: YAML permits an explicit `null`, which a `skip_serializing_if =
/// "Option::is_none"` field legitimately round-trips to nothing. Reporting it
/// would flag a correct file — the second false positive shape.
#[test]
fn explicit_null_is_not_reported() {
    let raw = "default_model: null\nauto_resume: true\n";
    let doc = yaml_document(raw).expect("valid yaml mapping");
    let parsed: TrustyToolsConfig = serde_yaml::from_str(raw).expect("parses");
    assert!(
        unknown_key_paths(&doc, &parsed).is_empty(),
        "an explicitly-null optional field is not an unknown key"
    );
}

/// Why: the YAML host config is the surface that carries the previously-orphaned
/// `default_model`; a typo there must be reported too.
#[test]
fn unknown_yaml_key_is_reported() {
    let raw = "defualt_model: opus\n";
    let doc = yaml_document(raw).expect("valid yaml mapping");
    let parsed: TrustyToolsConfig = serde_yaml::from_str(raw).expect("parses leniently");
    assert_eq!(unknown_key_paths(&doc, &parsed), vec!["defualt_model"]);
}

/// Why: a clean document must produce nothing to log.
#[test]
fn report_is_silent_for_a_clean_document() {
    let raw = "workspace_root_template: ~/code\nauto_resume: false\n";
    let doc = yaml_document(raw).expect("valid yaml mapping");
    let parsed: TrustyToolsConfig = serde_yaml::from_str(raw).expect("parses");
    assert!(unknown_key_paths(&doc, &parsed).is_empty());
}

/// Why: a YAML document that is not a mapping has no keys to check and must not
/// panic or invent findings.
#[test]
fn non_mapping_yaml_yields_no_document() {
    assert!(yaml_document("").is_none());
    assert!(yaml_document("just-a-scalar\n").is_none());
}
