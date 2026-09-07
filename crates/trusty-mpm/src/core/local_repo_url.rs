//! The one rule deciding whether a `session_new` `repo_url` names something
//! trusty-mpm can run a session on (ADR-0055, #6000).
//!
//! Why: trusty-mpm used to clone a remote `repo_url` and add a worktree for it
//! on `session_new`'s behalf. ADR-0055 removed that path, so a `repo_url` that
//! is not already a directory on the daemon host has nowhere to route. Every
//! entry point — the `session_new` MCP tool, the daemon's spawn RPC, and the
//! `tm session new` CLI — must refuse it with the same words, so the rule and
//! its message live here rather than being restated per surface.
//! What: [`is_local_workdir`], the detection heuristic itself, and
//! [`require_local_repo_url`], which turns a failing `repo_url` into the typed
//! [`NonLocalRepoUrl`] error naming ADR-0055 and the supported form.
//! Test: `local_repo_url_accepts_an_existing_directory`,
//! `local_repo_url_rejects_a_remote_url`,
//! `non_local_repo_url_message_names_adr_0055_and_the_remedy` below.

use std::path::Path;

/// A `repo_url` that is not an existing local directory (ADR-0055).
///
/// Why: the three `session_new` entry points each surface errors differently
/// (JSON string, HTTP body, rendered CLI line), so the refusal has to be a
/// value they can each carry rather than a string one of them formats. Typed
/// so a caller can match on it instead of substring-matching a message.
/// What: carries the rejected `repo_url`; its `Display` names ADR-0055, the
/// supported form, and the two-step remedy.
/// Test: `non_local_repo_url_message_names_adr_0055_and_the_remedy`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "repo_url {repo_url:?} is not an existing local directory. trusty-mpm no longer clones a \
     repository or creates a worktree for a session (ADR-0055): the only supported form is an \
     ABSOLUTE path to a directory that already exists on the daemon host. Clone the repository \
     yourself, then pass that path — e.g. `git clone <url> <dir>` followed by \
     `tm session new <dir>`."
)]
pub struct NonLocalRepoUrl {
    /// The `repo_url` as the caller supplied it.
    pub repo_url: String,
}

/// Whether `s` names an EXISTING local directory usable as a session workspace
/// directly, i.e. without a git clone (#1433).
///
/// Why: this used to select between the local-path branch and the clone branch.
/// Since ADR-0055 removed the clone branch it selects between running and
/// refusing, which is why it and the refusal now live in one module.
/// What: returns `true` iff `s` is an ABSOLUTE path that is a directory.
/// `is_dir()` already implies existence in a single `stat` syscall (a missing
/// path is not a dir) and follows symlinks, so a separate `exists()` probe is
/// redundant.
/// Test: `is_local_workdir_detects_absolute_dir`,
/// `is_local_workdir_rejects_url_relative_and_missing` in tests/local_spawn.rs.
pub fn is_local_workdir(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && p.is_dir()
}

/// Refuse a `repo_url` that is not an existing local directory (ADR-0055).
///
/// Why: decision B of ADR-0055 — with the clone path gone, a non-local
/// `repo_url` must fail loudly and name the remedy rather than degrade into a
/// different placement. Called before any side effect at every entry point.
/// What: `Ok(())` when [`is_local_workdir`] accepts `repo_url`, else the typed
/// [`NonLocalRepoUrl`].
/// Test: `local_repo_url_accepts_an_existing_directory`,
/// `local_repo_url_rejects_a_remote_url`.
pub fn require_local_repo_url(repo_url: &str) -> Result<(), NonLocalRepoUrl> {
    if is_local_workdir(repo_url) {
        return Ok(());
    }
    Err(NonLocalRepoUrl {
        repo_url: repo_url.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_repo_url_accepts_an_existing_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_string_lossy().into_owned();
        assert!(require_local_repo_url(&path).is_ok());
    }

    #[test]
    fn local_repo_url_rejects_a_remote_url() {
        let err = require_local_repo_url("https://github.com/owner/repo.git")
            .expect_err("a remote URL has no local directory to run in");
        assert_eq!(err.repo_url, "https://github.com/owner/repo.git");
    }

    /// The refusal must be actionable on its own — an operator reading only the
    /// error has to learn WHY it cannot work and WHAT to do instead.
    #[test]
    fn non_local_repo_url_message_names_adr_0055_and_the_remedy() {
        let msg = require_local_repo_url("git@github.com:owner/repo.git")
            .expect_err("ssh-style remote is not local")
            .to_string();
        assert!(msg.contains("ADR-0055"), "message must cite the ADR: {msg}");
        assert!(
            msg.contains("tm session new"),
            "message must name the two-step remedy: {msg}"
        );
    }
}
