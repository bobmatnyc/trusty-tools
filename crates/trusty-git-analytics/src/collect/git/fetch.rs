//! Remote fetch helper used prior to revwalk.
//!
//! Performs a non-interactive `git fetch` against a configured remote so
//! the local clone has the latest commits before [`super::GitCollector`]
//! walks history. The fetch is best-effort: authentication or network
//! failures are logged and downgraded to `Ok(())` so collection can
//! proceed on whatever the local clone already has.
//!
//! ## Authentication strategy
//!
//! - SSH agent (if running and has a usable key)
//! - SSH key files at `~/.ssh/id_ed25519` then `~/.ssh/id_rsa`
//! - **No interactive prompts** — SSH BatchMode-equivalent. We never ask
//!   for a password or passphrase, because the binary is typically run
//!   in CI / background tasks where stdin is unavailable.

use std::path::PathBuf;

use git2::{Cred, CredentialType, FetchOptions, RemoteCallbacks, Repository};
use tracing::{info, warn};

use crate::collect::collector::{FetchOutcome, PerRepoFetch};
use crate::collect::errors::Result;

/// Fetch all refs from the configured remote before walking.
///
/// Why: backwards-compatible soft-fail wrapper; callers that do not need
/// per-repo outcome tracking use this.
/// What: returns `Ok(())` whether the fetch succeeds or fails so that
/// collection can always proceed on whatever the local clone has.
/// Test: covered indirectly by extractor tests that call `GitCollector::collect`.
pub fn fetch_remote(repo: &Repository, remote_name: &str) -> Result<()> {
    fetch_remote_with_outcome(repo, remote_name).map(|_| ())
}

/// Fetch all refs from the configured remote and return a typed outcome.
///
/// Why: `fetch_remote` discards success/failure information; this variant
/// surfaces it as a [`FetchOutcome`] so the CLI can print an end-of-run
/// summary table (requirement #334).
/// What: attempts `git fetch <remote_name>`.  Returns:
///   - `FetchOutcome::Success` on a clean fetch,
///   - `FetchOutcome::Skipped` when the named remote does not exist
///     (local-only repos are a supported case — not an error),
///   - `FetchOutcome::Failed` for any auth/transport/git2 error.
///
/// Test: unit test `tests::fetch_outcome_skipped_for_local_repo` in this
/// module verifies the Skipped path; Failed and Success paths are exercised
/// by integration tests against real git fixtures.
pub fn fetch_remote_with_outcome(repo: &Repository, remote_name: &str) -> Result<FetchOutcome> {
    let mut remote = match repo.find_remote(remote_name) {
        Ok(r) => r,
        Err(e) => {
            // Most common: local-only repo with no remotes configured.
            info!(
                remote = remote_name,
                error = %e,
                "no matching remote; skipping fetch (local-only repo)"
            );
            return Ok(FetchOutcome::Skipped {
                reason: "no remote configured".to_string(),
            });
        }
    };

    info!("Fetching from remote '{}'", remote_name);

    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(|url, username_from_url, allowed_types| {
        non_interactive_credentials(url, username_from_url, allowed_types)
    });

    let mut fetch_options = FetchOptions::new();
    fetch_options.remote_callbacks(callbacks);

    // Fetch with default refspecs (empty slice == use configured refspecs).
    match remote.fetch(&[] as &[&str], Some(&mut fetch_options), None) {
        Ok(()) => {
            info!(remote = remote_name, "fetch succeeded");
            Ok(FetchOutcome::Success {
                remote: remote_name.to_string(),
            })
        }
        Err(e) => {
            // git2 lumps auth, transport, and certificate problems together;
            // treat anything in the auth/transport family as a soft failure
            // but record the error string so the end-of-run summary is useful.
            let kind = if is_auth_or_transport_error(&e) {
                "auth/transport"
            } else {
                "git"
            };
            warn!(
                remote = remote_name,
                error = %e,
                kind,
                "fetch failed — continuing with local refs"
            );
            Ok(FetchOutcome::Failed {
                remote: remote_name.to_string(),
                error: e.to_string(),
            })
        }
    }
}

/// Build a [`PerRepoFetch`] by running a tracked fetch for a named repository.
///
/// Why: wraps `fetch_remote_with_outcome` with the repo display name so the
/// returned value is ready to push into [`crate::collect::collector::CollectionStats::fetch_outcomes`].
/// What: opens the repo, runs `fetch_remote_with_outcome`, and packages the
/// result with `repo_name`.  If opening the repo fails, returns a Failed
/// outcome rather than propagating the error (consistent with the soft-fail
/// policy for all per-repo operations).
/// Test: covered by the integration test in `collector::tests`.
pub fn fetch_and_record(repo: &Repository, repo_name: &str, remote_name: &str) -> PerRepoFetch {
    match fetch_remote_with_outcome(repo, remote_name) {
        Ok(outcome) => PerRepoFetch {
            repo: repo_name.to_string(),
            outcome,
        },
        Err(e) => PerRepoFetch {
            repo: repo_name.to_string(),
            outcome: FetchOutcome::Failed {
                remote: remote_name.to_string(),
                error: e.to_string(),
            },
        },
    }
}

/// Build a credential without prompting the user.
///
/// Tries, in order:
/// 1. SSH agent (if `allowed_types` permits)
/// 2. `~/.ssh/id_ed25519`
/// 3. `~/.ssh/id_rsa`
/// 4. Default credential helper (HTTPS)
///
/// Returns a git2 error if none of these succeed — the caller will turn
/// that into a logged warning and continue.
fn non_interactive_credentials(
    _url: &str,
    username_from_url: Option<&str>,
    allowed_types: CredentialType,
) -> std::result::Result<Cred, git2::Error> {
    let username = username_from_url.unwrap_or("git");

    if allowed_types.contains(CredentialType::SSH_KEY) {
        // 1. SSH agent first.
        if let Ok(cred) = Cred::ssh_key_from_agent(username) {
            return Ok(cred);
        }

        // 2. Explicit key files (ed25519 preferred, rsa fallback).
        if let Some(home) = home_dir() {
            for key_name in &["id_ed25519", "id_rsa"] {
                let private_key = home.join(".ssh").join(key_name);
                if private_key.exists() {
                    // git2 needs a passphrase parameter even for unencrypted
                    // keys; we pass None to force non-interactive behavior.
                    if let Ok(cred) = Cred::ssh_key(username, None, private_key.as_path(), None) {
                        return Ok(cred);
                    }
                }
            }
        }
    }

    if allowed_types.contains(CredentialType::DEFAULT) {
        if let Ok(cred) = Cred::default() {
            return Ok(cred);
        }
    }

    Err(git2::Error::from_str(
        "no non-interactive credentials available (tried SSH agent, ~/.ssh/id_ed25519, ~/.ssh/id_rsa)",
    ))
}

/// Best-effort home-directory lookup using `$HOME`.
///
/// We deliberately avoid pulling in the `home` / `dirs` crate for one
/// path lookup; `$HOME` is set on every supported platform.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Classify an error as an auth/transport failure suitable for soft-failing.
fn is_auth_or_transport_error(e: &git2::Error) -> bool {
    matches!(
        e.class(),
        git2::ErrorClass::Ssh
            | git2::ErrorClass::Http
            | git2::ErrorClass::Net
            | git2::ErrorClass::Callback
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: local-only repos (no remotes) are a valid configuration; the
    /// fetch path must return Skipped rather than an error.
    /// What: creates a temp in-memory git repo with no remotes, calls
    /// `fetch_remote_with_outcome`, and asserts the Skipped variant.
    /// Test: this test itself.
    #[test]
    fn fetch_outcome_skipped_for_local_repo() {
        let td = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(td.path()).expect("init");
        // No remote configured → expect Skipped.
        let outcome =
            fetch_remote_with_outcome(&repo, "origin").expect("no error expected from soft-fail");
        assert!(
            matches!(outcome, FetchOutcome::Skipped { .. }),
            "expected Skipped, got {outcome:?}"
        );
    }

    /// Why: ensure that `fetch_and_record` wraps the outcome with the correct
    /// repo name without panicking.
    /// What: uses the same local-only repo and verifies PerRepoFetch.repo field.
    /// Test: this test itself.
    #[test]
    fn fetch_and_record_sets_repo_name() {
        let td = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(td.path()).expect("init");
        let prf = fetch_and_record(&repo, "my-repo", "origin");
        assert_eq!(prf.repo, "my-repo");
        assert!(
            matches!(prf.outcome, FetchOutcome::Skipped { .. }),
            "expected Skipped, got {:?}",
            prf.outcome
        );
    }

    /// Why: the end-of-run fetch summary must include `Failed` entries for
    /// repos whose remote exists but cannot be reached.  This test proves that
    /// `fetch_remote_with_outcome` returns a `Failed` variant (not a panic or
    /// an `Err`) when the remote's URL is unreachable.
    /// What: creates two git repos, adds repo-b as an "origin" remote on
    /// repo-a, then deletes repo-b so the URL is valid-looking but the target
    /// directory is gone.  The fetch attempt against that dead path must
    /// produce `FetchOutcome::Failed`.
    /// Test: this test itself — covers the `Failed` branch of
    /// `fetch_remote_with_outcome` and by extension `fetch_and_record`.
    #[test]
    fn fetch_remote_with_outcome_returns_failed_for_dead_remote() {
        let td_remote = tempfile::tempdir().expect("tempdir for remote");
        // Initialise a bare remote repo so git2 can point an origin at a valid path.
        let _remote = git2::Repository::init_bare(td_remote.path()).expect("init bare");

        let td_local = tempfile::tempdir().expect("tempdir for local");
        let local = git2::Repository::init(td_local.path()).expect("init local");

        // Set the origin remote to the path of the bare repo, then remove it
        // to make the URL dead without being syntactically invalid.
        let remote_url = td_remote.path().to_str().expect("valid utf8").to_string();
        local
            .remote("origin", &remote_url)
            .expect("add remote origin");
        // Drop td_remote explicitly to delete the directory.
        drop(td_remote);
        // Reopen local after remote dir is gone.
        let local2 = git2::Repository::open(td_local.path()).expect("reopen local");

        let outcome = fetch_remote_with_outcome(&local2, "origin")
            .expect("no Err — soft-fail policy means we return Ok(Failed)");
        assert!(
            matches!(outcome, FetchOutcome::Failed { .. }),
            "expected Failed for dead remote path, got {outcome:?}"
        );

        // Also verify the fetch_and_record wrapper preserves the repo name.
        let local3 = git2::Repository::open(td_local.path()).expect("reopen local 3");
        let prf = fetch_and_record(&local3, "dead-remote-repo", "origin");
        assert_eq!(prf.repo, "dead-remote-repo");
        assert!(
            matches!(prf.outcome, FetchOutcome::Failed { .. }),
            "expected Failed in PerRepoFetch, got {:?}",
            prf.outcome
        );
    }
}
