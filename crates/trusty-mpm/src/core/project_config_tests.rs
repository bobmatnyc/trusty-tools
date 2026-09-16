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
    staged_declaration_changes_documents_only,
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

/// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
///
/// Why: a fixture step that silently no-ops produces a test that passes for the
/// wrong reason — and every #7905 row below turns on whether a blob really
/// reached `HEAD` or the index.
fn git_ok(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("fixture: `git {}` could not be run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "fixture: `git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A real, committed git checkout — the only shape #7905 can be tested in.
///
/// Why (#7905 review, CRITICAL 1): [`documents_only_at`] reads
/// `HEAD:.trusty-mpm.toml`, so a bare `tempdir` with a file written into it
/// answers `false` no matter what the file says. Every row below needs a
/// repository with a real commit in it, or it would be asserting that git is
/// absent rather than that the declaration is.
fn committed_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git_ok(dir.path(), &["init", "--initial-branch=main"]);
    git_ok(dir.path(), &["config", "user.email", "t@example.com"]);
    git_ok(dir.path(), &["config", "user.name", "t"]);
    std::fs::write(dir.path().join("README.md"), "# t\n").expect("seed");
    git_ok(dir.path(), &["add", "README.md"]);
    git_ok(dir.path(), &["commit", "-m", "seed"]);
    dir
}

/// Write `raw` to the declaration, then stage and commit it.
fn commit_declaration(dir: &Path, raw: &str) {
    std::fs::write(dir.join(PROJECT_CONFIG_FILE), raw).expect("write the declaration");
    git_ok(dir, &["add", PROJECT_CONFIG_FILE]);
    git_ok(dir, &["commit", "-m", "declare"]);
}

/// 🔴 #7905: only a COMMITTED, parseable declaration saying `true` widens the
/// write boundary.
///
/// Why: [`documents_only_at`] is the single reader both ADR-0044 rules consult,
/// and its `true` REMOVES a deny. So every way of not-saying-true has to answer
/// `false`: no declaration at all, a `false` value, and another key entirely. A
/// single wrong `true` here would open a shared checkout to source writes.
#[test]
fn documents_only_at_reads_a_committed_declaration() {
    let dir = committed_repo();

    assert!(
        !documents_only_at(dir.path()),
        "no declaration declares nothing"
    );

    commit_declaration(dir.path(), "documents_only = true\n");
    assert!(
        documents_only_at(dir.path()),
        "a committed declaration says so"
    );

    commit_declaration(dir.path(), "documents_only = false\n");
    assert!(!documents_only_at(dir.path()), "`false` is not a grant");

    commit_declaration(dir.path(), "worktree = false\n");
    assert!(
        !documents_only_at(dir.path()),
        "another project key is not this one"
    );
}

/// 🔴 #7905 review, CRITICAL 1: an UNCOMMITTED declaration grants nothing.
///
/// Why: the reported escalation verbatim. `.trusty-mpm.toml` is not source
/// under `is_source_code_path`, so ADR-0044 admits a write to it — which means
/// an agent refused a source write could simply write the declaration and
/// retry. Two tool calls, and the boundary was off. The three rows here are the
/// three uncommitted spellings that attack reaches for: untracked, staged but
/// not committed, and an uncommitted edit to a tracked file that did not carry
/// the key.
/// Test: itself.
#[test]
fn documents_only_at_ignores_an_uncommitted_declaration() {
    // 1. Untracked — the critic's exact sequence.
    let dir = committed_repo();
    std::fs::write(
        dir.path().join(PROJECT_CONFIG_FILE),
        "documents_only = true\n",
    )
    .expect("write");
    assert!(
        !documents_only_at(dir.path()),
        "an untracked declaration is not the repository's word"
    );

    // 2. Staged but not committed.
    git_ok(dir.path(), &["add", PROJECT_CONFIG_FILE]);
    assert!(
        !documents_only_at(dir.path()),
        "staging is not committing; `HEAD` still carries no declaration"
    );

    // 3. An uncommitted EDIT to a tracked declaration that lacked the key.
    let dir = committed_repo();
    commit_declaration(dir.path(), "worktree = false\n");
    std::fs::write(
        dir.path().join(PROJECT_CONFIG_FILE),
        "worktree = false\ndocuments_only = true\n",
    )
    .expect("write");
    assert!(
        !documents_only_at(dir.path()),
        "an uncommitted edit to a tracked file is not the repository's word"
    );
}

/// 🔴 #7905 fail-closed: a declaration that cannot be TRUSTED is not a
/// declaration.
///
/// Why: `from_toml` rejects a file wholesale when any key fails
/// `deny_unknown_fields` — a typo means the author's intent is unknown, not
/// partially known. A relaxation read out of such a file would be a grant
/// nobody wrote. The last row is the one only the committed-blob read can
/// answer: a directory git cannot be asked about at all.
#[test]
fn documents_only_at_is_false_for_an_undeclared_or_unreadable_checkout() {
    // A real `documents_only = true` sitting beside a misspelled key: the file
    // is rejected as a whole, so the grant does not survive.
    for raw in [
        "documents_only = true\nwrktree = false\n",
        "documents_only = \"yes\"\n",
        "this is not toml = = =\n",
    ] {
        let dir = committed_repo();
        commit_declaration(dir.path(), raw);
        assert!(
            !documents_only_at(dir.path()),
            "a blob that does not parse grants nothing: {raw:?}"
        );
    }

    // Not a repository at all: git cannot be asked, so nothing is declared.
    let bare = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        bare.path().join(PROJECT_CONFIG_FILE),
        "documents_only = true\n",
    )
    .expect("write");
    assert!(
        !documents_only_at(bare.path()),
        "a directory git cannot answer for declares nothing"
    );
}

/// 🔴 #7905 review, CRITICAL 2: staging the declaration is detected, in both
/// directions.
///
/// Why: closing CRITICAL 1 alone leaves the other half of the loop open — the
/// declaration would simply be committed, from the main checkout, as an
/// ordinary documents commit. This is the predicate the ADR-0049 commit gate
/// uses to refuse that commit, so it has to fire on introducing the key, on
/// flipping it, and on retracting it: each changes a security-relevant
/// declaration and each belongs in a reviewed pull request.
/// Test: itself.
#[test]
fn staged_declaration_change_is_detected_in_both_directions() {
    // Introducing it, with nothing at HEAD.
    let dir = committed_repo();
    std::fs::write(
        dir.path().join(PROJECT_CONFIG_FILE),
        "documents_only = true\n",
    )
    .expect("write");
    git_ok(dir.path(), &["add", PROJECT_CONFIG_FILE]);
    assert!(
        staged_declaration_changes_documents_only(dir.path()),
        "introducing the key is a change"
    );

    // Flipping it, and then retracting it, against a HEAD that carries `true`.
    for raw in ["documents_only = false\n", "worktree = false\n"] {
        let dir = committed_repo();
        commit_declaration(dir.path(), "documents_only = true\n");
        std::fs::write(dir.path().join(PROJECT_CONFIG_FILE), raw).expect("write");
        git_ok(dir.path(), &["add", PROJECT_CONFIG_FILE]);
        assert!(
            staged_declaration_changes_documents_only(dir.path()),
            "flipping or retracting the key is a change: {raw:?}"
        );
    }
}

/// #7905 review, CRITICAL 2's bound: an edit that spares the key is not a
/// declaration change.
///
/// Why: the rule has to be about the KEY, not about the file. A project editing
/// an unrelated setting in its `.trusty-mpm.toml` is doing ordinary
/// configuration work, and refusing that from the main checkout would be a new
/// deny #7905 never asked for.
/// Test: itself.
#[test]
fn staged_declaration_edit_that_leaves_the_key_alone_is_not_a_change() {
    let dir = committed_repo();
    commit_declaration(dir.path(), "documents_only = true\nworktree = false\n");
    std::fs::write(
        dir.path().join(PROJECT_CONFIG_FILE),
        "documents_only = true\nworktree = true\n",
    )
    .expect("write");
    git_ok(dir.path(), &["add", PROJECT_CONFIG_FILE]);
    assert!(
        !staged_declaration_changes_documents_only(dir.path()),
        "editing a neighbouring key leaves the declaration where it was"
    );

    // And the same in a project that never declared: a brand-new config file
    // carrying no `documents_only` key changes nothing either.
    let dir = committed_repo();
    std::fs::write(dir.path().join(PROJECT_CONFIG_FILE), "worktree = false\n").expect("write");
    git_ok(dir.path(), &["add", PROJECT_CONFIG_FILE]);
    assert!(
        !staged_declaration_changes_documents_only(dir.path()),
        "a new config file that does not declare is an ordinary documents commit"
    );
}
