//! Tests for project-local output styles (#8533).

use super::*;
use tempfile::TempDir;

fn project_with_style(id: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let styles = dir.path().join(PROJECT_STYLES_DIR);
    std::fs::create_dir_all(&styles).expect("styles dir");
    std::fs::write(
        styles.join(format!("{id}.md")),
        "---\nname: x\n---\n\nProject voice.\n",
    )
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
    assert!(
        message.contains("trusty-mpm-teacher") && message.contains("fixture-voice"),
        "{message}"
    );
}

#[test]
fn a_path_like_id_is_refused() {
    let dir = project_with_style("fixture-voice");
    std::fs::write(dir.path().join("secret.md"), "outside").expect("write");
    assert!(resolve_style_in_project(dir.path(), "../../secret").is_err());
    assert!(resolve_style_in_project(dir.path(), ".hidden").is_err());
}

#[test]
fn style_resolution_order_is_flag_then_project_then_host_then_manifest() {
    // #8533 acceptance: one row per tier. Each row sets every tier from its
    // own down and expects its own value to win.
    let dir = project_with_style("fixture-voice");
    let host = |active: Option<&str>| {
        let mut config = MpmConfig::default();
        config.style.active = active.map(str::to_string);
        config
    };
    let with_project = |set: bool| {
        let file = dir.path().join(".trusty-mpm.toml");
        if set {
            std::fs::write(&file, "[style]\nactive = \"fixture-voice\"\n").expect("project config");
        } else if file.exists() {
            std::fs::remove_file(&file).expect("remove project config");
        }
    };
    let manifest = Some("trusty-mpm-research");

    with_project(true);
    assert_eq!(
        effective_style_id(
            dir.path(),
            Some("trusty-mpm"),
            &host(Some("trusty-mpm-teacher")),
            manifest
        )
        .as_deref(),
        Some("trusty-mpm"),
        "tier 1: --style"
    );
    assert_eq!(
        effective_style_id(
            dir.path(),
            None,
            &host(Some("trusty-mpm-teacher")),
            manifest
        )
        .as_deref(),
        Some("fixture-voice"),
        "tier 2: .trusty-mpm.toml [style] active"
    );
    with_project(false);
    assert_eq!(
        effective_style_id(
            dir.path(),
            None,
            &host(Some("trusty-mpm-teacher")),
            manifest
        )
        .as_deref(),
        Some("trusty-mpm-teacher"),
        "tier 3: host config"
    );
    assert_eq!(
        effective_style_id(dir.path(), None, &host(None), manifest).as_deref(),
        Some("trusty-mpm-research"),
        "tier 4: manifest"
    );
    assert_eq!(
        effective_style_id(dir.path(), None, &host(None), None),
        None
    );
}

#[test]
fn an_unreadable_style_file_warns_and_keeps_the_prompt() {
    // Fail-open: the selected style file exists but cannot be read (a
    // directory, which fails even as root). The launch warns naming the file
    // as unreadable, injects the default style, and keeps the prompt whole.
    let dir = TempDir::new().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(PROJECT_STYLES_DIR).join("broken-voice.md"))
        .expect("style path as a directory");
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "[style]\nactive = \"broken-voice\"\n",
    )
    .expect("project config");

    let (style, warning) = resolve_or_default(dir.path(), Some("broken-voice"));
    assert_eq!(style.id(), crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID);
    let warning = warning.expect("an unreadable style is never a silent fallback");
    assert!(
        warning.contains("unreadable") && warning.contains("broken-voice"),
        "{warning}"
    );

    let prompt = "BASE PROMPT\n\n## Memory & Instruction Sources".to_string();
    let injected = super::super::apply_output_style_to_prompt_with_native(
        dir.path(),
        None,
        prompt.clone(),
        false,
    );
    assert!(
        injected.ends_with(&prompt),
        "the base prompt must survive whole"
    );
    let fw_root = dir.path().join("fw-root");
    assert!(describe_effective_style(&fw_root, dir.path()).contains("unreadable"));
}

#[test]
fn project_config_style_outranks_host_config() {
    let dir = project_with_style("fixture-voice");
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "[style]\nactive = \"fixture-voice\"\n",
    )
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
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "[style]\nactive = \"fixture-voice\"\n",
    )
    .expect("project config");
    let injected = super::super::apply_output_style_to_prompt_with_native(
        dir.path(),
        None,
        "PROMPT".to_string(),
        false,
    );
    assert!(
        injected.contains("Project voice.") && injected.ends_with("PROMPT"),
        "{injected}"
    );
    let native = super::super::apply_output_style_to_prompt_with_native(
        dir.path(),
        None,
        "PROMPT".to_string(),
        true,
    );
    assert_eq!(native, "PROMPT");
}
