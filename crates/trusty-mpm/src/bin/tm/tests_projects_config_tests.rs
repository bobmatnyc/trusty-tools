//! CLI parse + shared-suite tests for `tm projects config` (DOC-35 §3.1/§6,
//! #2120).
//!
//! Why: split out of `tests_projects.rs` to keep that file under the
//! test-file line cap (this file's basename ends in `_tests.rs`, so it is
//! classified as a test file with the 1500-SLOC cap rather than the
//! 500-SLOC production cap `tests_projects.rs` itself is held to — see
//! CLAUDE.md's SLOC-cap convention). Reuses `tests_projects::projects_action`
//! rather than duplicating it.
//! What: `Cli::try_parse_from` round-trips for every `config` verb (bare
//! view, `set`, `unset`, `tags`, the disclosed `unset default-branch`
//! parse-time rejection) plus
//! `cli_projects_config_shared_cases_match_wire_semantics` — the CLI half of
//! the #2120 "one shared validation/persistence test suite exercised from
//! both a CLI integration test and a TUI form unit test" requirement (see
//! `trusty_mpm::project_config`'s module doc for the full design; the TUI
//! half lives in `tui::project_ctl::state::modals::tests`).
//! Test: this file is the test.

use clap::Parser;

use crate::cli::{ClearableConfigField, Cli, ConfigAction, ProjectsAction, SettableConfigField};
use crate::tests_projects::projects_action;

fn config_action(argv: &[&str]) -> (String, bool, Option<ConfigAction>) {
    match projects_action(argv) {
        ProjectsAction::Config { name, json, action } => (name, json, action),
        other => panic!("expected config, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_config_bare_view() {
    let (name, json, action) = config_action(&["trusty-mpm", "projects", "config", "widget"]);
    assert_eq!(name, "widget");
    assert!(!json);
    assert!(action.is_none());
}

#[test]
fn cli_parses_projects_config_bare_view_json() {
    let (name, json, action) =
        config_action(&["trusty-mpm", "projects", "config", "widget", "--json"]);
    assert_eq!(name, "widget");
    assert!(json);
    assert!(action.is_none());
}

#[test]
fn cli_parses_projects_config_set() {
    let (name, _json, action) = config_action(&[
        "trusty-mpm",
        "projects",
        "config",
        "widget",
        "set",
        "stack-hint",
        "rust",
    ]);
    assert_eq!(name, "widget");
    match action.expect("action") {
        ConfigAction::Set { field, value } => {
            assert_eq!(field, SettableConfigField::StackHint);
            assert_eq!(value, "rust");
        }
        other => panic!("expected set, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_config_unset() {
    let (name, _json, action) = config_action(&[
        "trusty-mpm",
        "projects",
        "config",
        "widget",
        "unset",
        "description",
    ]);
    assert_eq!(name, "widget");
    match action.expect("action") {
        ConfigAction::Unset { field } => {
            assert_eq!(field, ClearableConfigField::Description);
        }
        other => panic!("expected unset, got {other:?}"),
    }
}

/// #2120 disclosed deviation: `unset default-branch` is rejected AT PARSE
/// TIME (a clap "invalid value" usage error), never proxied to the server —
/// `ClearableConfigField` has no `DefaultBranch` variant because
/// `default_branch` has no wire representation for clearing.
#[test]
fn cli_rejects_unset_default_branch_at_parse_time() {
    let err = Cli::try_parse_from([
        "trusty-mpm",
        "projects",
        "config",
        "widget",
        "unset",
        "default-branch",
    ])
    .expect_err("unset default-branch must fail to parse");
    assert_eq!(err.exit_code(), 2);
    assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
}

#[test]
fn cli_parses_projects_config_tags_add_and_remove() {
    let (name, _json, action) = config_action(&[
        "trusty-mpm",
        "projects",
        "config",
        "widget",
        "tags",
        "--add",
        "ml,oss",
        "--remove",
        "legacy",
    ]);
    assert_eq!(name, "widget");
    match action.expect("action") {
        ConfigAction::Tags { add, remove } => {
            assert_eq!(add, vec!["ml".to_string(), "oss".to_string()]);
            assert_eq!(remove, vec!["legacy".to_string()]);
        }
        other => panic!("expected tags, got {other:?}"),
    }
}

/// #2120 issue requirement: "one shared validation/persistence test suite
/// exercised from both a CLI integration test and a TUI form unit test" — see
/// `trusty_mpm::project_config`'s module doc for the full design. This test
/// exercises the CLI's OWN edit-to-`ConfigEdit` mapping: build the exact argv
/// this front end would produce for each shared case, parse it through the
/// real `Cli`, route it through `convert.rs`'s `From` impls (the same ones
/// `commands::projects::registry::config` calls), and assert the resulting
/// wire args match the shared table — so a future edit to either the CLI's
/// argv-building or its `From` impls that silently changes wire semantics
/// fails here, not just in `trusty_mpm::project_config`'s own (necessarily
/// circular, since it calls its own builder) unit test.
#[test]
fn cli_projects_config_shared_cases_match_wire_semantics() {
    for case in trusty_mpm::project_config::config_edit_cases() {
        let argv = argv_for_case(&case.edit);
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let (_, _, action) = config_action(&argv_refs);
        let edit = match action.expect("action") {
            ConfigAction::Set { field, value } => {
                trusty_mpm::project_config::ConfigEdit::Set(field.into(), value)
            }
            ConfigAction::Unset { field } => {
                trusty_mpm::project_config::ConfigEdit::Unset(field.into())
            }
            ConfigAction::Tags { add, remove } => {
                trusty_mpm::project_config::ConfigEdit::Tags { add, remove }
            }
        };
        let args = trusty_mpm::project_config::build_patch_args(&edit);
        trusty_mpm::project_config::assert_matches_case(&args, &case);
    }
}

/// Build the argv this CLI front end would use to express one shared
/// [`trusty_mpm::project_config::ConfigEdit`] case, mirroring
/// `SettableConfigField`/`ClearableConfigField`'s default kebab-case value
/// names.
fn argv_for_case(edit: &trusty_mpm::project_config::ConfigEdit) -> Vec<String> {
    use trusty_mpm::project_config::{ClearableField, ConfigEdit, ConfigField};

    let base = ["trusty-mpm", "projects", "config", "widget"].map(str::to_string);
    let mut argv: Vec<String> = base.to_vec();
    match edit {
        ConfigEdit::Set(field, value) => {
            let name = match field {
                ConfigField::DefaultBranch => "default-branch",
                ConfigField::Description => "description",
                ConfigField::StackHint => "stack-hint",
                ConfigField::GhUser => "gh-user",
            };
            argv.push("set".to_string());
            argv.push(name.to_string());
            argv.push(value.clone());
        }
        ConfigEdit::Unset(field) => {
            let name = match field {
                ClearableField::Description => "description",
                ClearableField::StackHint => "stack-hint",
                ClearableField::GhUser => "gh-user",
            };
            argv.push("unset".to_string());
            argv.push(name.to_string());
        }
        ConfigEdit::Tags { add, remove } => {
            argv.push("tags".to_string());
            if !add.is_empty() {
                argv.push("--add".to_string());
                argv.push(add.join(","));
            }
            if !remove.is_empty() {
                argv.push("--remove".to_string());
                argv.push(remove.join(","));
            }
        }
    }
    argv
}
