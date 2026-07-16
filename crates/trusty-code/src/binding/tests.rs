//! Unit tests for [`crate::binding`] — the three binding states (§4.2).
//!
//! Why: this module is the correction the spec makes to the design proposal
//! ("binds the moment work touches files in a git repo"). The non-git case is
//! therefore not an edge case here — it is the load-bearing assertion, and
//! `non_git_dir_is_bound_and_indexes` is the test that would fail if anyone
//! reintroduced the git-only gate.
//! What: covers `resolve`'s five outcomes, the bound/git/index predicates, the
//! agents-dir fallback, and the wire shape.
//! Test: this file.

use super::*;
use std::process::Command;

/// Initialise a real git repo in `dir` (the classification shells out to git,
/// so a fake `.git` directory would not be honest here).
fn init_git_repo(dir: &Path) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("init")
        .output()
        .expect("git init must run")
        .status
        .success();
    assert!(ok, "git init must succeed");
}

/// `resolve(None)` is projectless — the supported state, NOT an error (AC-2.1).
#[test]
fn resolve_none_is_projectless() {
    let binding = ProjectBinding::resolve(None).expect("projectless must resolve, never error");
    assert_eq!(binding, ProjectBinding::None);
    assert!(!binding.is_bound(), "projectless must not be bound");
    assert!(!binding.is_git(), "projectless has no git affordances");
    assert_eq!(binding.root(), None, "projectless has no root");
}

/// A plain (non-git) directory binds as `Directory` — a BOUND state (AC-2.2).
#[test]
fn resolve_plain_dir_binds_as_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let binding =
        ProjectBinding::resolve(Some(tmp.path().to_path_buf())).expect("a plain dir must bind");
    assert!(
        matches!(binding, ProjectBinding::Directory(_)),
        "a non-git dir must bind as Directory, got {binding:?}"
    );
}

/// A git worktree binds as `GitRepo` and reports git affordances.
#[test]
fn resolve_git_repo_binds_as_git_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    init_git_repo(tmp.path());
    let binding =
        ProjectBinding::resolve(Some(tmp.path().to_path_buf())).expect("a git repo must bind");
    assert!(
        matches!(binding, ProjectBinding::GitRepo(_)),
        "a git worktree must bind as GitRepo, got {binding:?}"
    );
    assert!(binding.is_git(), "a git repo must offer git affordances");
    assert!(binding.is_bound());
    assert!(binding.should_index());
}

/// **The correction, asserted.** A non-git directory is BOUND and INDEXES; it is
/// not projectless and must not be treated as such (AC-2.2/AC-2.3, #2728/#2747).
/// This is the test that fails if the git-only binding gate is reintroduced.
#[test]
fn non_git_dir_is_bound_and_indexes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let binding = ProjectBinding::resolve(Some(tmp.path().to_path_buf())).expect("must bind");

    assert!(
        binding.is_bound(),
        "a non-git dir MUST bind — binding is not gated on .git (#2728/#2747)"
    );
    assert!(
        binding.should_index(),
        "a non-git dir MUST index — indexing is not gated on .git (AC-2.3)"
    );
    assert!(
        !binding.is_git(),
        "a non-git dir must NOT claim git affordances — they are hidden, not faked"
    );
    assert_ne!(
        binding,
        ProjectBinding::None,
        "a non-git dir is NOT projectless"
    );
}

/// Projectless is the ONLY state that does not index (AC-2.1).
#[test]
fn projectless_does_not_index() {
    assert!(
        !ProjectBinding::None.should_index(),
        "projectless has no project to index"
    );
}

/// A `Some(path)` that does not exist is a caller error, never a silent
/// downgrade to projectless — omitting a project and naming a bad one are
/// different facts and must not collapse into one.
#[test]
fn resolve_rejects_missing_path() {
    let missing = PathBuf::from("/definitely/not/a/real/path/xyzzy-42");
    let err = ProjectBinding::resolve(Some(missing.clone()))
        .expect_err("a nonexistent path must not resolve");
    assert_eq!(err, BindingError::NotFound(missing));
}

/// A `Some(path)` naming a FILE is a caller error (the old untyped label would
/// have accepted this happily and bound nothing).
#[test]
fn resolve_rejects_file_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("not-a-dir.txt");
    std::fs::write(&file, "x").expect("write");
    let err = ProjectBinding::resolve(Some(file)).expect_err("a file path must not resolve");
    assert!(
        matches!(err, BindingError::NotADirectory(_)),
        "expected NotADirectory, got {err:?}"
    );
}

/// `root()` returns `None` exactly for projectless, and `Some` for both bound
/// states — the git/non-git split must not affect whether a root exists.
#[test]
fn root_is_none_only_when_projectless() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = ProjectBinding::Directory(tmp.path().to_path_buf());
    let git = ProjectBinding::GitRepo(tmp.path().to_path_buf());

    assert_eq!(ProjectBinding::None.root(), None);
    assert_eq!(dir.root(), Some(tmp.path()));
    assert_eq!(git.root(), Some(tmp.path()));
}

/// The `Session.project` label is DERIVED from the binding's root — it can no
/// longer be an independent value that disagrees with the path.
#[test]
fn label_is_derived_from_root() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let binding = ProjectBinding::Directory(tmp.path().to_path_buf());
    assert_eq!(binding.label(), Some(tmp.path().display().to_string()));
    assert_eq!(
        ProjectBinding::None.label(),
        None,
        "projectless must have no label"
    );
}

/// A bound binding resolves agents from the PROJECT's `.claude/agents`.
#[test]
fn agents_dir_uses_project_root_when_bound() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let agents = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents).expect("mkdir");
    let binding = ProjectBinding::Directory(tmp.path().to_path_buf());
    assert_eq!(binding.agents_dir(), agents);
}

/// A projectless daemon still needs agent configs; it falls back to the
/// USER-level `~/.claude/agents` rather than the process CWD, which would
/// silently bind to a directory the operator never chose.
#[test]
fn projectless_agents_dir_is_user_level() {
    let Some(home) = dirs::home_dir() else {
        // No home dir on this machine: the documented degraded path.
        assert_eq!(
            ProjectBinding::None.agents_dir(),
            PathBuf::from(".claude").join("agents")
        );
        return;
    };
    let dir = ProjectBinding::None.agents_dir();
    assert!(
        dir.starts_with(&home),
        "projectless agents_dir must be user-level ({}), got {}",
        home.display(),
        dir.display()
    );
    assert!(
        !dir.starts_with(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/nonexistent"))),
        "projectless must NOT implicitly bind agents to the process CWD"
    );
}

/// The wire shape carries an explicit `state` discriminant for all three states,
/// so the SPA never has to infer projectless from a null path.
#[test]
fn wire_shape_round_trips_each_state() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().display().to_string();

    let none = ProjectBinding::None.to_json();
    assert_eq!(none["state"], STATE_PROJECTLESS);
    assert!(none["root"].is_null(), "projectless must have a null root");

    let dir = ProjectBinding::Directory(tmp.path().to_path_buf()).to_json();
    assert_eq!(dir["state"], STATE_DIRECTORY);
    assert_eq!(dir["root"], root);

    let git = ProjectBinding::GitRepo(tmp.path().to_path_buf()).to_json();
    assert_eq!(git["state"], STATE_GIT_REPO);
    assert_eq!(git["root"], root);
}

/// `Serialize` must agree with `to_json` — `Session` derives `Serialize`, so a
/// drift here would silently change the `session.*` wire shape.
#[test]
fn serialize_matches_to_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let binding = ProjectBinding::GitRepo(tmp.path().to_path_buf());
    let via_serde = serde_json::to_value(&binding).expect("serialize");
    assert_eq!(via_serde, binding.to_json());
}

/// `is_git_worktree` fails open to `false` for a nonexistent path — a missing
/// directory means "no git affordances", never a panic.
#[test]
fn is_git_worktree_fails_open_for_missing_path() {
    assert!(!is_git_worktree(Path::new("/definitely/not/here/xyzzy-42")));
}

/// Deserialization must reconstruct from the explicit `state` discriminant and
/// NOT re-probe the filesystem: a `GitRepo` read back on a host that lacks the
/// repo must stay a `GitRepo`, or the round-trip is lossy.
#[test]
fn deserialize_round_trips_without_touching_disk() {
    let phantom = PathBuf::from("/definitely/not/here/xyzzy-42");
    for original in [
        ProjectBinding::None,
        ProjectBinding::Directory(phantom.clone()),
        ProjectBinding::GitRepo(phantom.clone()),
    ] {
        let json = serde_json::to_value(&original).expect("serialize");
        let back: ProjectBinding = serde_json::from_value(json).expect("deserialize");
        assert_eq!(
            back, original,
            "round-trip must preserve the state even for a path that does not exist on this host"
        );
    }
}

/// A bound state without a `root`, or an unknown discriminant, is a hard error
/// — never a silent downgrade to projectless.
#[test]
fn deserialize_rejects_bound_state_without_root() {
    let err = serde_json::from_value::<ProjectBinding>(serde_json::json!({"state": "git_repo"}))
        .expect_err("a bound state with no root must not deserialize");
    assert!(
        err.to_string().contains("requires a root"),
        "unexpected error: {err}"
    );

    let err = serde_json::from_value::<ProjectBinding>(
        serde_json::json!({"state": "bogus", "root": "/x"}),
    )
    .expect_err("an unknown state must not deserialize");
    assert!(
        err.to_string().contains("unknown project binding state"),
        "unexpected error: {err}"
    );
}

/// `Default` must be projectless — a `Session` deserialized from an older
/// payload with no `binding` field reads back as projectless rather than
/// inventing a binding.
#[test]
fn default_is_projectless() {
    assert_eq!(ProjectBinding::default(), ProjectBinding::None);
}
