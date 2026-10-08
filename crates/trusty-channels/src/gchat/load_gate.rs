//! The routes-file load gate (#9448 E1): `routes.toml` takes effect only when
//! the bytes parsed are exactly the bytes committed at `HEAD`.
//!
//! Why: a route change must go through a reviewed commit. The commit is not
//! the approval; the gate only keeps an unreviewed edit from taking effect.
//! `git status` is not that check: `--assume-unchanged`, `--skip-worktree`, a
//! clean filter or `core.fsmonitor` each hide an edit from it, and a file
//! read before the check can change between the read and the check.
//! What: [`check_committed`] hashes the caller's bytes with
//! `git hash-object --no-filters --stdin` and compares the result with
//! `git rev-parse HEAD:./<file>`. Every `GIT_*` variable is removed from the
//! git environment so the caller's environment cannot point the check at
//! another repository. Any git failure refuses the load.
//! Test: `load_gate_refuses_untracked_modified_and_staged`,
//! `load_gate_refuses_edits_hidden_by_assume_unchanged_or_skip_worktree`,
//! `load_gate_checks_the_bytes_read_not_the_file_after`.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use crate::gchat::error::RouteError;

/// Refuse unless `bytes` equal the blob committed at `HEAD` for `path`.
///
/// Why: see the module doc. The caller passes the bytes it will parse, so
/// the check and the parse see the same content (#9448 review).
/// What: runs git in the file's directory. No commit, a path absent from
/// `HEAD`, bytes that differ from the committed blob, or git missing each
/// return [`RouteError::NotCommitted`]. `--no-filters` hashes the raw bytes,
/// so a file a clean filter or line-ending conversion would rewrite is
/// refused rather than trusted.
/// Test: `load_gate_refuses_untracked_modified_and_staged`,
/// `load_gate_refuses_edits_hidden_by_assume_unchanged_or_skip_worktree`,
/// `load_gate_checks_the_bytes_read_not_the_file_after`.
pub fn check_committed(path: &Path, bytes: &[u8]) -> Result<(), RouteError> {
    let refuse = |reason: String| RouteError::NotCommitted {
        path: path.to_path_buf(),
        reason,
    };
    let (dir, file) = match (path.parent(), path.file_name()) {
        (Some(d), Some(f)) => (d, f),
        _ => return Err(refuse("path has no parent directory".into())),
    };
    let mut spec = OsString::from("HEAD:./");
    spec.push(file);
    let committed = git(dir)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(&spec)
        .output()
        .map_err(|e| refuse(format!("cannot run git: {e}")))?;
    if !committed.status.success() {
        return Err(refuse(
            "not committed at HEAD (commit it through a reviewed PR)".into(),
        ));
    }
    let read = hash_bytes(dir, bytes).map_err(|e| refuse(format!("cannot run git: {e}")))?;
    if !read.status.success() {
        return Err(refuse("git hash-object failed".into()));
    }
    if object_id(&read) != object_id(&committed) {
        return Err(refuse(
            "content differs from the version committed at HEAD".into(),
        ));
    }
    Ok(())
}

/// `git hash-object --no-filters --stdin` over `bytes`, in `dir`'s repo (so
/// the repository's object format applies).
fn hash_bytes(dir: &Path, bytes: &[u8]) -> std::io::Result<Output> {
    let mut child = git(dir)
        .args(["hash-object", "--no-filters", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Dropping the handle after the write closes stdin, so git sees EOF.
    let written = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(bytes),
        None => Err(std::io::Error::other("git stdin unavailable")),
    };
    let output = child.wait_with_output()?;
    written.map(|()| output)
}

fn object_id(output: &Output) -> &[u8] {
    output.stdout.trim_ascii()
}

/// A git command in `dir` with every `GIT_*` variable removed.
fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd
}
