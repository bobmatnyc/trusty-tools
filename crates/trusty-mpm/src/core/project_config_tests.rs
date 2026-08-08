//! Schema and loader tests for the committed project-level config (#5207).
//!
//! Why: the two contracts worth pinning are (a) an unrecognised key is an
//! ERROR, which is the whole point of putting `deny_unknown_fields` on this
//! struct, and (b) an absent file is NOT an error, since almost no project has
//! one.
//! What: parse-level cases against [`super::ProjectLevelConfig::from_toml`] and
//! disk-level cases against [`super::ProjectLevelConfig::load`] /
//! [`super::load_or_report`], all hermetic under a `TempDir`.

use std::path::Path;

use tempfile::TempDir;

use super::{PROJECT_CONFIG_FILE, ProjectConfigError, ProjectLevelConfig, load_or_report};

/// Write a project config into a fresh temp project directory.
fn project_with(body: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join(PROJECT_CONFIG_FILE), body).expect("write config");
    dir
}

/// Why (owner ruling 4): the file must be a ROOT-LEVEL dotfile, not a member of
/// the machine-local `.trusty-mpm/` directory that projects gitignore wholesale.
/// Pinning the name keeps a later refactor from quietly moving a file whose
/// entire purpose is to be committed into a directory that cannot be.
#[test]
fn project_config_path_is_a_root_dotfile() {
    assert_eq!(PROJECT_CONFIG_FILE, ".trusty-mpm.toml");
    assert!(
        !PROJECT_CONFIG_FILE.contains('/'),
        "the project config must sit at the project root, not under a subdirectory"
    );
}

/// Why: the primary setting this surface exists to carry must round-trip.
#[test]
fn project_config_parses_worktree() {
    let cfg = ProjectLevelConfig::from_toml("worktree = false\n", Path::new("t.toml"))
        .expect("valid config");
    assert_eq!(cfg.worktree, Some(false));
    assert_eq!(cfg.default_model, None);
}

/// Why: an empty (or comment-only) file is a legitimate state — it overrides
/// nothing and must not be an error.
#[test]
fn project_config_empty_file_is_all_none() {
    let cfg = ProjectLevelConfig::from_toml("# nothing yet\n", Path::new("t.toml"))
        .expect("an empty config is valid");
    assert_eq!(cfg, ProjectLevelConfig::default());
}

/// Why (#5207, the `deny_unknown_fields` proof): before this change a
/// misspelled key was accepted and silently ignored, so an operator who wrote
/// `worktre = false` got worktrees anyway with no signal at all. THIS is the
/// test that fails without `#[serde(deny_unknown_fields)]`.
#[test]
fn project_config_rejects_unknown_key() {
    let err = ProjectLevelConfig::from_toml("worktre = false\n", Path::new("t.toml"))
        .expect_err("a misspelled key must be rejected, not silently ignored");

    let ProjectConfigError::Malformed { source, .. } = &err else {
        panic!("expected a Malformed error, got: {err:?}");
    };
    let msg = source.to_string();
    assert!(
        msg.contains("worktre"),
        "the error must name the offending key so it can be fixed: {msg}"
    );
}

/// Why: an unknown key is rejected even when every OTHER key is valid — a
/// partially-good file must not half-apply, because a typo means the author's
/// intent is unknown rather than partially known.
#[test]
fn project_config_rejects_unknown_key_alongside_valid_ones() {
    let err = ProjectLevelConfig::from_toml(
        "worktree = false\nmodel_default = \"opus\"\n",
        Path::new("t.toml"),
    )
    .expect_err("one bad key invalidates the file");
    assert!(matches!(err, ProjectConfigError::Malformed { .. }));
}

/// Why: a right-named but wrongly-typed value is the other half of "rejected,
/// not silently ignored" — `worktree = "false"` is a string, not a bool.
#[test]
fn project_config_rejects_wrong_type() {
    let err = ProjectLevelConfig::from_toml("worktree = \"false\"\n", Path::new("t.toml"))
        .expect_err("a string is not a bool");
    assert!(matches!(err, ProjectConfigError::Malformed { .. }));
}

/// Why: almost no project has this file; its absence is the common case and
/// must be `Ok(None)` rather than an error.
#[test]
fn project_config_absent_is_none() {
    let dir = TempDir::new().expect("tempdir");
    assert_eq!(
        ProjectLevelConfig::load(dir.path()).expect("absent is not an error"),
        None
    );
}

/// Why: the loader must actually read the canonical filename from disk — a
/// path-join typo would make every project config invisible.
#[test]
fn project_config_reads_from_disk() {
    let dir = project_with("worktree = false\ndefault_model = \"opus\"\n");
    let cfg = ProjectLevelConfig::load(dir.path())
        .expect("valid config")
        .expect("file is present");
    assert_eq!(cfg.worktree, Some(false));
    assert_eq!(cfg.default_model.as_deref(), Some("opus"));
}

/// Why: a file that exists but does not parse must be an `Err` from the
/// fallible loader, so a future `tm doctor` check can report it.
#[test]
fn project_config_load_surfaces_a_bad_file() {
    let dir = project_with("worktre = false\n");
    let err = ProjectLevelConfig::load(dir.path()).expect_err("a bad file must not be Ok");
    assert!(matches!(err, ProjectConfigError::Malformed { .. }));
}

/// Why: the spawn path uses the lenient wrapper, which must degrade to "no
/// project layer" rather than propagate. A committed file is shared by the whole
/// team, so one bad push must not brick everyone's session launches.
#[test]
fn load_or_report_returns_none_for_unknown_key() {
    let dir = project_with("worktre = false\n");
    assert_eq!(
        load_or_report(dir.path()),
        None,
        "a rejected file must contribute nothing"
    );
}

/// Why: the lenient wrapper's ordinary path — absent file, no layer, no noise.
#[test]
fn load_or_report_returns_none_when_absent() {
    let dir = TempDir::new().expect("tempdir");
    assert_eq!(load_or_report(dir.path()), None);
}

/// Why: the lenient wrapper must still return a GOOD file's values; degrading
/// on the error path is only correct if the success path works.
#[test]
fn load_or_report_returns_a_valid_config() {
    let dir = project_with("worktree = true\n");
    assert_eq!(
        load_or_report(dir.path()).and_then(|c| c.worktree),
        Some(true)
    );
}
