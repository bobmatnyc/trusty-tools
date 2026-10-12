//! Tests for the D07 legacy-name acceptance (#9755). Each goes red when its
//! alias is removed from `architect_compat.rs`.

use std::path::Path;

use super::*;

#[test]
fn both_profile_values_name_the_architect() {
    assert!(is_architect_profile_id("architect"));
    assert!(is_architect_profile_id("supervisor"));
    assert!(!is_architect_profile_id("pm"));
    assert!(!is_architect_profile_id("architects"));
}

#[test]
fn both_stamps_name_the_architect() {
    assert!(is_architect_stamp(Some(OsStr::new("architect"))));
    assert!(is_architect_stamp(Some(OsStr::new("supervisor"))));
    assert!(!is_architect_stamp(Some(OsStr::new("pm"))));
    assert!(!is_architect_stamp(Some(OsStr::new(" supervisor"))));
    assert!(!is_architect_stamp(None));
}

#[test]
fn both_style_ids_name_the_architect_style() {
    assert!(is_architect_output_style_id("trusty-mpm-architect"));
    assert!(is_architect_output_style_id("trusty-mpm-supervisor"));
    assert!(!is_architect_output_style_id("trusty-mpm"));
}

#[test]
fn only_architect_content_paths_have_a_legacy_path() {
    assert_eq!(
        legacy_instruction_path("architect/identity.md").as_deref(),
        Some("supervisor/identity.md")
    );
    assert_eq!(
        legacy_instruction_path("output-styles/trusty-mpm-architect.md").as_deref(),
        Some("output-styles/trusty-mpm-supervisor.md")
    );
    assert_eq!(legacy_instruction_path("sections/core.md"), None);
}

/// Fail-closed: a kind written by a newer `tm` reads as the Architect, and a
/// kind nobody knows passes through to `Unknown` (protected), never `ordinary`.
#[test]
fn a_stored_architect_kind_reads_as_the_architect() {
    use crate::session_manager::SessionKind;
    let read = |wire: &str| -> SessionKind {
        serde_json::from_value(serde_json::Value::String(wire.into())).unwrap()
    };
    assert_eq!(read("architect"), read("supervisor"));
    assert_eq!(read("architect_aux"), read("supervisor_aux"));
    assert_ne!(read("architect"), SessionKind::Unknown);
    assert_eq!(read("overseer"), SessionKind::Unknown);
    assert_eq!(read("ordinary"), SessionKind::Ordinary);
}

/// The repository content still carries only `instructions/supervisor/` (D2d),
/// so every Architect section resolves through the legacy path.
#[test]
fn a_source_with_only_legacy_supervisor_sections_still_loads() {
    let content = crate::core::content_source::test_support::rc();
    for (_, path) in crate::core::session_profile::ARCHITECT_SECTIONS {
        let legacy = legacy_instruction_path(path).expect("an Architect section");
        assert!(content.instruction(path).is_some(), "{path} must resolve");
        assert_eq!(content.instruction(path), content.instruction(&legacy));
    }
    assert!(
        content
            .instruction("output-styles/trusty-mpm-architect.md")
            .is_some()
    );
}

/// Fail-closed: a PM project naming the Architect style by its new id gets the
/// Architect-only refusal, not the style.
#[test]
fn a_pm_project_cannot_select_the_architect_style_by_its_new_id() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let selected = crate::core::output_style::select_style(
        dir.path(),
        Some(ARCHITECT_OUTPUT_STYLE_ID),
        &crate::core::config::MpmConfig::default(),
        || None,
        crate::core::session_profile::SessionProfile::Pm,
    );
    let warning = selected.warning.expect("a refusal warning");
    assert!(warning.contains("profile only"), "{warning}");
}

/// Load `text` as `<root>/config.toml` through the production loader.
fn load_user_config(text: &str) -> crate::core::config::MpmConfig {
    let root = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(root.path().join("config.toml"), text).expect("write config");
    crate::core::config::MpmConfig::load(root.path())
}

/// Fail-closed (Q7): a pre-D07 file with only `[supervisor]` still grants the
/// Architect, through the one accessor every reader uses.
#[test]
fn legacy_supervisor_table_still_grants_architect() {
    use crate::core::session_profile::{SessionProfile, resolve};
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n",
    )
    .unwrap();
    let config = load_user_config(&format!(
        "[supervisor]\nprojects = [{:?}]\n",
        dir.path().display().to_string()
    ));
    assert_eq!(
        config.architect_grant().projects,
        [dir.path().to_path_buf()]
    );
    assert_eq!(resolve(dir.path(), &config), SessionProfile::Architect);
}

/// Fail-closed (Q7): both tables in one file parse — no duplicate-field error,
/// so the loader does not reset the config to defaults — and `[architect]`
/// wins whole.
#[test]
fn both_architect_tables_keep_architect_and_the_rest_of_the_config() {
    let config = load_user_config(
        "[builders]\nmax_concurrent = 7\n\n[pm_guard]\nenabled = false\n\n\
         [architect]\nprojects = [\"/new\"]\n\n[supervisor]\nprojects = [\"/old\"]\n",
    );
    assert_eq!(config.architect_grant().projects, [Path::new("/new")]);
    assert!(config.has_both_architect_tables());
    assert_eq!(config.builders.max_concurrent, Some(7), "config was reset");
    assert!(!config.pm_guard.enabled, "config was reset");
}

/// A writer edits the table the reader uses: `[architect]` once it exists,
/// else the legacy `[supervisor]` an older `tm` still reads.
#[test]
fn the_writer_targets_the_architect_table_once_it_exists() {
    assert_eq!(grant_table_to_write(true), ARCHITECT_TABLE);
    assert_eq!(grant_table_to_write(false), LEGACY_SUPERVISOR_TABLE);
}

/// The new `profile = "architect"` value selects the Architect when the
/// project is allow-listed.
#[test]
fn the_new_profile_value_resolves_the_architect() {
    use crate::core::session_profile::{SessionProfile, resolve};
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "profile = \"architect\"\n",
    )
    .unwrap();
    let mut config = crate::core::config::MpmConfig::default();
    config.architect.get_or_insert_default().projects = vec![dir.path().to_path_buf()];
    assert_ne!(resolve(dir.path(), &config), SessionProfile::Pm);
}
