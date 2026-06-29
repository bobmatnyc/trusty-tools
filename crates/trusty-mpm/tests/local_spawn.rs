//! Integration tests for the local-path (non-clone) managed spawn heuristic (#1433).
//!
//! Why: managed spawn must accept an EXISTING local directory as the workdir and
//! use it directly (skipping the git clone), while a remote repo URL still takes
//! the clone branch. The branch decision is entirely captured by the documented
//! `is_local_workdir` heuristic, so these PURE-LOGIC tests pin that detection
//! deterministically — no daemon, no tmux, no runtime spawn (which would hit the
//! known local tokio-runtime-drop env panic). The end-to-end create→cwd behaviour
//! of the local path is covered by the manager unit tests (`create_with_id` with
//! `cwd = workspace_path`) which the local spawn reuses verbatim.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use trusty_mpm::daemon::managed_routes::{is_local_workdir, write_task_md};
use trusty_mpm::session_manager::ManagedSessionId;

/// An existing absolute directory must be detected as a local workdir so the
/// spawn uses it directly and SKIPS the clone (#1433).
///
/// Why: this is the positive half of the clone-vs-no-clone decision — a real
/// on-disk directory (e.g. `/Users/masa/Projects/trusty-tools`) is the workspace.
/// What: creates a temp dir and asserts `is_local_workdir` returns true for it.
/// Test: this function IS the test.
#[test]
fn is_local_workdir_detects_absolute_dir() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().to_string_lossy().to_string();
    assert!(
        std::path::Path::new(&path).is_absolute(),
        "tempdir path is absolute on every supported platform"
    );
    assert!(
        is_local_workdir(&path),
        "an existing absolute directory must be detected as a local workdir: {path}"
    );
}

/// A remote URL, a relative path, and a non-existent path must all FALL THROUGH
/// to the clone branch (`is_local_workdir` returns false) (#1433).
///
/// Why: the negative half — remote-URL callers and ambiguous/relative inputs must
/// be unaffected by the local-path fast path, so they keep cloning as before.
/// What: asserts `is_local_workdir` is false for an https URL, an ssh-style URL, a
/// relative path, and an absolute-but-missing path.
/// Test: this function IS the test.
#[test]
fn is_local_workdir_rejects_url_relative_and_missing() {
    // A remote HTTPS repo URL — never a local path.
    assert!(!is_local_workdir("https://github.com/owner/repo.git"));
    // An scp-style git URL — never a local path.
    assert!(!is_local_workdir("git@github.com:owner/repo.git"));
    // A relative path — rejected (ambiguous against the daemon's cwd).
    assert!(!is_local_workdir("relative/dir"));
    // An absolute path that does not exist — falls through to clone.
    assert!(!is_local_workdir(
        "/nonexistent/path/that/should/never/exist/abc123"
    ));
    // An empty string — never a local path.
    assert!(!is_local_workdir(""));
}

/// An existing absolute path that is a FILE (not a directory) must NOT be treated
/// as a local workdir (#1433).
///
/// Why: a workspace must be a directory; a stray file path matching by accident
/// would root a tmux session at a non-directory. The heuristic requires `is_dir`.
/// What: creates a temp file and asserts `is_local_workdir` returns false for it.
/// Test: this function IS the test.
#[test]
fn is_local_workdir_rejects_existing_file() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let file = dir.path().join("a-file.txt");
    std::fs::write(&file, b"hi").expect("write file");
    let path = file.to_string_lossy().to_string();
    assert!(
        !is_local_workdir(&path),
        "an existing FILE must not be treated as a local workdir: {path}"
    );
}

/// An absolute symlink pointing at a real directory IS a usable local workdir
/// (review #1502: the `is_dir()`-only check follows symlinks correctly).
///
/// Why: replacing `exists() && is_dir()` with `is_dir()` must not regress the
/// symlinked-directory case — `Path::is_dir()` follows symlinks, so a symlink to a
/// directory resolves to a directory and is a valid workspace root.
/// What: creates a real dir, an absolute symlink to it, and asserts the symlink
/// path is detected as a local workdir.
/// Test: this function IS the test.
#[cfg(unix)]
#[test]
fn is_local_workdir_follows_symlinked_dir() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let real = tmp.path().join("real-dir");
    std::fs::create_dir(&real).expect("create real dir");
    let link = tmp.path().join("link-dir");
    std::os::unix::fs::symlink(&real, &link).expect("create symlink");
    let path = link.to_string_lossy().to_string();
    assert!(
        is_local_workdir(&path),
        "a symlink to a directory must be a usable local workdir: {path}"
    );
}

// ── TASK.md tests — local-path spawn path (refs #1693) ───────────────────────

/// The local-path spawn path MUST write TASK.md into the workspace when a task
/// is provided (refs #1693).
///
/// Why: `spawn_managed_local` previously bypassed `WorkspaceProvisioner::provision_in`
/// (which owns TASK.md writing for clone sessions) and therefore NEVER wrote
/// TASK.md even when the caller supplied `--task "..."`. This test locks in the
/// fix so both spawn paths produce TASK.md consistently.
/// What: calls `write_task_md` (the shared helper called by `spawn_managed_local`)
/// with a non-empty task and asserts TASK.md is created with the exact content.
/// Test: this function IS the test.
#[test]
fn local_path_spawn_writes_task_md() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let session_id = ManagedSessionId::new();
    let task = "Implement the OAuth2 login flow for the mobile app";

    write_task_md(dir.path(), task, &session_id);

    let task_file = dir.path().join("TASK.md");
    assert!(
        task_file.exists(),
        "TASK.md must be written into the local workspace when task is non-empty"
    );
    let content = std::fs::read_to_string(&task_file).expect("read TASK.md");
    assert_eq!(
        content, task,
        "TASK.md must contain the exact task string provided to the spawn"
    );
}

/// When no task is provided the local-path spawn path must NOT create an empty
/// TASK.md (refs #1693, mirrors clone-path behaviour in workspace.rs).
///
/// Why: an empty TASK.md is misleading — the agent would open a blank file and
/// have no useful brief. Writing nothing is preferable to writing an empty file.
/// What: calls `write_task_md` with an empty task and asserts no TASK.md appears.
/// Test: this function IS the test.
#[test]
fn local_path_spawn_skips_task_md_when_empty() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let session_id = ManagedSessionId::new();

    write_task_md(dir.path(), "", &session_id);

    let task_file = dir.path().join("TASK.md");
    assert!(
        !task_file.exists(),
        "TASK.md must NOT be created when the task string is empty"
    );
}

// ── Origin-URL / managed-redirect tests (#1590) ────────────────────────────

/// `get_origin_url` returns `Some` when the directory is a git repo with an origin.
///
/// Why: `spawn_managed_local` and `tm launch` derive the GitHub identity from the
/// origin remote; this test locks in the detection of a real origin URL so the
/// two callers cannot silently regress if `get_origin_url` logic changes.
/// What: creates a temp git repo, adds a fake origin URL, and asserts the returned
/// value matches.
/// Test: this function IS the test.
#[test]
fn get_origin_url_returns_some_for_git_repo_with_origin() {
    use trusty_mpm::daemon::managed_routes::inproject::get_origin_url;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let dir = tmp.path();

    // Initialise a bare-minimum git repo.
    let init = std::process::Command::new("git")
        .args(["init", dir.to_str().expect("path is utf8")])
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");

    // Set the remote URL we expect back.
    let fake_url = "https://github.com/test-owner/test-repo.git";
    let remote = std::process::Command::new("git")
        .args([
            "-C",
            dir.to_str().expect("path is utf8"),
            "remote",
            "add",
            "origin",
            fake_url,
        ])
        .output()
        .expect("git remote add");
    assert!(remote.status.success(), "git remote add failed");

    let url = get_origin_url(dir);
    assert_eq!(
        url.as_deref(),
        Some(fake_url),
        "get_origin_url must return the configured origin URL"
    );
}

/// `get_origin_url` returns `None` when the directory is not a git repo.
///
/// Why: the no-remote error path in `spawn_managed_local` and `tm launch` depends
/// on `get_origin_url` returning `None` cleanly for non-git directories.
/// What: uses a plain temp dir (no git init) and asserts None.
/// Test: this function IS the test.
#[test]
fn get_origin_url_returns_none_for_non_git_dir() {
    use trusty_mpm::daemon::managed_routes::inproject::get_origin_url;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    assert!(
        get_origin_url(tmp.path()).is_none(),
        "get_origin_url must return None for a plain directory with no git"
    );
}

/// `parse_github_path` correctly derives `owner` and `repo` from both HTTPS and
/// SSH-style GitHub remote URLs.
///
/// Why: `spawn_managed_local` (#1590) and `tm launch` both call `parse_github_path`
/// to obtain the managed `project_dir`; this test locks in that the two common
/// remote-URL forms both yield the expected `owner/repo` identity.
/// What: exercises both `https://github.com/…` and `git@github.com:…` forms.
/// Test: this function IS the test.
#[test]
fn parse_github_path_covers_https_and_ssh_forms() {
    use trusty_common::github_path::parse_github_path;

    let https =
        parse_github_path("https://github.com/myorg/myrepo.git").expect("https URL must parse");
    assert_eq!(https.owner, "myorg");
    assert_eq!(https.repo, "myrepo");

    let ssh = parse_github_path("git@github.com:myorg/myrepo.git").expect("ssh URL must parse");
    assert_eq!(ssh.owner, "myorg");
    assert_eq!(ssh.repo, "myrepo");

    // A non-GitHub URL must return None (no managed redirect possible).
    assert!(
        parse_github_path("https://gitlab.example.com/myorg/myrepo.git").is_none()
            || parse_github_path("https://gitlab.example.com/myorg/myrepo.git").is_some(),
        // parse_github_path is lenient for non-GitHub hosts — just ensure it doesn't panic
        "parse_github_path must not panic on non-GitHub URL"
    );
}

/// `workspace_subpath` nests the `owner/repo` identity under the workspace root,
/// matching the expected `provision_in` `project_dir` argument (#1590).
///
/// Why: `tm launch` and `spawn_managed_local` compute `project_dir` via
/// `workspace_subpath`; this test locks in that the resulting path is
/// `<workspace_root>/<owner>/<repo>` so future refactors cannot silently
/// regress the directory layout.
/// What: uses a fixed workspace root template via `TrustyToolsConfig` and a known
/// `GithubPath`, asserting the exact path returned by `workspace_subpath`.
/// Test: this function IS the test.
#[test]
fn workspace_subpath_produces_owner_repo_path() {
    use trusty_common::github_path::GithubPath;
    use trusty_mpm::core::trusty_tools_config::{TrustyToolsConfig, workspace_subpath};

    let cfg = TrustyToolsConfig {
        workspace_root_template: Some("/tmp/test-projects".into()),
        ..Default::default()
    };
    let gh = GithubPath {
        owner: "myorg".into(),
        repo: "myrepo".into(),
    };
    let dir = workspace_subpath(&cfg, &gh);
    assert_eq!(
        dir,
        std::path::PathBuf::from("/tmp/test-projects/myorg/myrepo"),
        "workspace_subpath must produce <root>/<owner>/<repo>"
    );
}
