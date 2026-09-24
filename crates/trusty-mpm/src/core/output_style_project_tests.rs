//! Tests for project-local output styles (#8533).

use super::*;
use tempfile::TempDir;

fn project_with_style(id: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let styles = dir.path().join(PROJECT_STYLES_DIR);
    std::fs::create_dir_all(&styles).expect("styles dir");
    std::fs::write(styles.join(format!("{id}.md")), "---\nname: x\n---\n\nProject voice.\n")
        .expect("style file");
    dir
}

#[test]
fn a_project_style_file_resolves_by_id() {
    let dir = project_with_style("fixture-voice");
    let style = resolve_style_in_project(dir.path(), "fixture-voice").expect("resolves");
    assert_eq!(style.id(), "fixture-voice");
    assert!(style.content().contains("Project voice."));
    assert!(matches!(style, ActiveStyle::Project { .. }));
    // A bundled id keeps meaning the shipped style.
    let bundled = resolve_style_in_project(dir.path(), "trusty-mpm").expect("bundled");
    assert!(matches!(bundled, ActiveStyle::Bundled(_)));
}

#[test]
fn unknown_id_lists_bundled_and_project_styles() {
    let dir = project_with_style("fixture-voice");
    let err = resolve_style_in_project(dir.path(), "nope").expect_err("unknown");
    let message = err.to_string();
    assert!(message.contains("trusty-mpm-teacher") && message.contains("fixture-voice"), "{message}");
}

#[test]
fn a_path_like_id_is_refused() {
    let dir = project_with_style("fixture-voice");
    std::fs::write(dir.path().join("secret.md"), "outside").expect("write");
    assert!(resolve_style_in_project(dir.path(), "../../secret").is_err());
    assert!(resolve_style_in_project(dir.path(), ".hidden").is_err());
}

#[test]
fn project_config_style_outranks_host_config() {
    let dir = project_with_style("fixture-voice");
    std::fs::write(dir.path().join(".trusty-mpm.toml"), "[style]\nactive = \"fixture-voice\"\n")
        .expect("project config");
    let mut config = MpmConfig::default();
    config.style.active = Some("trusty-mpm-teacher".to_string());
    assert_eq!(
        effective_style_id(dir.path(), None, &config, Some("trusty-mpm-research")).as_deref(),
        Some("fixture-voice")
    );
    assert_eq!(
        effective_style_id(dir.path(), Some("trusty-mpm"), &config, None).as_deref(),
        Some("trusty-mpm"),
        "the --style flag stays on top"
    );
}

#[test]
fn an_unknown_style_warns_and_uses_the_default() {
    let dir = TempDir::new().expect("tempdir");
    let (style, warning) = resolve_or_default(dir.path(), Some("typo-style"));
    assert_eq!(style.id(), crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID);
    let warning = warning.expect("an unknown id is never a silent fallback");
    assert!(warning.contains("typo-style"), "{warning}");
    let (_, none) = resolve_or_default(dir.path(), None);
    assert!(none.is_none());
}

#[test]
fn a_project_style_is_injected_when_native_is_unsupported() {
    let dir = project_with_style("fixture-voice");
    std::fs::write(dir.path().join(".trusty-mpm.toml"), "[style]\nactive = \"fixture-voice\"\n")
        .expect("project config");
    let injected = super::super::apply_output_style_to_prompt_with_native(
        dir.path(),
        None,
        "PROMPT".to_string(),
        false,
    );
    assert!(injected.contains("Project voice.") && injected.ends_with("PROMPT"), "{injected}");
    let native = super::super::apply_output_style_to_prompt_with_native(
        dir.path(),
        None,
        "PROMPT".to_string(),
        true,
    );
    assert_eq!(native, "PROMPT");
}
