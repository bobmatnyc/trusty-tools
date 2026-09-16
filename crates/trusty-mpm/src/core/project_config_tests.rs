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

use super::{
    PROJECT_CONFIG_FILE, ProjectConfigError, ProjectLevelConfig, documents_only_at, load_or_report,
};

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

/// Why (#5814): `agent_worktree` is a SEPARATE key from `worktree` — a file
/// that sets one must leave the other undecided, or the dispatched-agent opt-out
/// would silently relocate the project's sessions as well.
#[test]
fn project_config_parses_agent_worktree() {
    let cfg = ProjectLevelConfig::from_toml("agent_worktree = false\n", Path::new("t.toml"))
        .expect("valid config");
    assert_eq!(cfg.agent_worktree, Some(false));
    assert_eq!(
        cfg.worktree, None,
        "the session-placement key must stay undecided"
    );

    let both = ProjectLevelConfig::from_toml(
        "worktree = true\nagent_worktree = false\n",
        Path::new("t.toml"),
    )
    .expect("valid config");
    assert_eq!(both.worktree, Some(true));
    assert_eq!(both.agent_worktree, Some(false));
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

// ─── #7422: the `[session]` MCP-server / plugin allowlists ────────────────

/// Both allowlists parse, and they are independent of each other.
#[test]
fn project_config_parses_session_scope() {
    let raw = "[session]\nmcp_servers = [\"slack-mcp\"]\nplugins = [\"aws-core\"]\n";
    let cfg = ProjectLevelConfig::from_toml(raw, Path::new("/p/.trusty-mpm.toml")).unwrap();
    let session = cfg.session.expect("the table parses");
    assert_eq!(session.mcp_servers, Some(vec!["slack-mcp".to_owned()]));
    assert_eq!(session.plugins, Some(vec!["aws-core".to_owned()]));
}

/// An absent `[session]` table is deny-all, not an error.
#[test]
fn project_config_session_defaults_to_none() {
    let cfg = ProjectLevelConfig::from_toml("worktree = true\n", Path::new("/p")).unwrap();
    assert_eq!(cfg.session, None);
}

/// A misspelled key inside `[session]` silently denies a server the operator
/// meant to allow, so it fails loudly like every other key here.
#[test]
fn project_config_rejects_unknown_session_key() {
    let raw = "[session]\nmcp_server = [\"slack-mcp\"]\n";
    let err = ProjectLevelConfig::from_toml(raw, Path::new("/p")).unwrap_err();
    assert!(
        matches!(err, ProjectConfigError::Malformed { .. }),
        "expected a Malformed error, got {err:?}"
    );
}

/// #7688: the prompt-self-improvement toggle parses as a top-level scalar,
/// exactly like `worktree` and `agent_worktree`.
#[test]
fn project_config_parses_prompt_self_improvement() {
    let cfg = ProjectLevelConfig::from_toml(
        "prompt_self_improvement = true\n",
        Path::new("/p/.trusty-mpm.toml"),
    )
    .unwrap();
    assert_eq!(cfg.prompt_self_improvement, Some(true));

    let off = ProjectLevelConfig::from_toml("prompt_self_improvement = false\n", Path::new("/p"))
        .unwrap();
    assert_eq!(off.prompt_self_improvement, Some(false));
}

/// An absent key declines to decide, so the host layer answers — it does NOT
/// mean "off".
#[test]
fn project_config_prompt_self_improvement_defaults_to_none() {
    let cfg = ProjectLevelConfig::from_toml("worktree = true\n", Path::new("/p")).unwrap();
    assert_eq!(cfg.prompt_self_improvement, None);
}

/// It is a top-level key, not a member of `[session]` — that table is two
/// allowlists and nothing else.
#[test]
fn project_config_rejects_prompt_self_improvement_inside_session() {
    let raw = "[session]\nprompt_self_improvement = true\n";
    let err = ProjectLevelConfig::from_toml(raw, Path::new("/p")).unwrap_err();
    assert!(
        matches!(err, ProjectConfigError::Malformed { .. }),
        "expected a Malformed error, got {err:?}"
    );
}

/// This repository's own committed config turns the flag on (owner ruling
/// 2026-09-12), and it must keep parsing against the schema that reads it.
#[test]
fn this_repositorys_project_config_enables_prompt_self_improvement() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(PROJECT_CONFIG_FILE);
    let raw = std::fs::read_to_string(&path).expect("the committed project config is readable");
    let cfg = ProjectLevelConfig::from_toml(&raw, &path).expect("it parses");
    assert_eq!(cfg.prompt_self_improvement, Some(true));
}

/// #7905: the key parses, and an absent key is not `false`.
#[test]
fn project_config_parses_documents_only() {
    let on = ProjectLevelConfig::from_toml("documents_only = true\n", Path::new("/p")).unwrap();
    assert_eq!(on.documents_only, Some(true));

    let off = ProjectLevelConfig::from_toml("documents_only = false\n", Path::new("/p")).unwrap();
    assert_eq!(off.documents_only, Some(false));

    let absent = ProjectLevelConfig::from_toml("worktree = true\n", Path::new("/p")).unwrap();
    assert_eq!(absent.documents_only, None);
}

/// 🔴 #7905: only a parseable file saying `true` widens the write boundary.
///
/// Why: [`documents_only_at`] is the single reader both ADR-0044 rules consult,
/// and its `true` REMOVES a deny. So every way of not-saying-true has to answer
/// `false`: an absent file, a `false` value, a file that fails
/// `deny_unknown_fields`, and one that is not TOML at all. A single wrong
/// `true` here would open a shared checkout to source writes.
#[test]
fn documents_only_at_reads_a_declared_checkout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(PROJECT_CONFIG_FILE);

    assert!(!documents_only_at(dir.path()), "no file declares nothing");

    std::fs::write(&path, "documents_only = true\n").expect("write");
    assert!(
        documents_only_at(dir.path()),
        "a declared repository says so"
    );

    std::fs::write(&path, "documents_only = false\n").expect("write");
    assert!(!documents_only_at(dir.path()), "`false` is not a grant");

    std::fs::write(&path, "worktree = false\n").expect("write");
    assert!(
        !documents_only_at(dir.path()),
        "another project key is not this one"
    );
}

/// 🔴 #7905 fail-closed: a declaration that cannot be TRUSTED is not a
/// declaration.
///
/// Why: `load_or_report` rejects a file wholesale when any key fails
/// `deny_unknown_fields` — a typo means the author's intent is unknown, not
/// partially known. A relaxation read out of such a file would be a grant
/// nobody wrote.
#[test]
fn documents_only_at_is_false_for_an_undeclared_or_unreadable_checkout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(PROJECT_CONFIG_FILE);

    // A real `documents_only = true` sitting beside a misspelled key: the file
    // is rejected as a whole, so the grant does not survive.
    std::fs::write(&path, "documents_only = true\nwrktree = false\n").expect("write");
    assert!(
        !documents_only_at(dir.path()),
        "a file that fails deny_unknown_fields grants nothing"
    );

    std::fs::write(&path, "documents_only = \"yes\"\n").expect("write");
    assert!(
        !documents_only_at(dir.path()),
        "a wrong type grants nothing"
    );

    std::fs::write(&path, "this is not toml = = =\n").expect("write");
    assert!(
        !documents_only_at(dir.path()),
        "an unparseable file grants nothing"
    );
}
