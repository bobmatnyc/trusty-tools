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

use trusty_mpm::daemon::managed_routes::is_local_workdir;

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
