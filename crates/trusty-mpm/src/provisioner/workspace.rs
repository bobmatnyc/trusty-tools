//! Git operations shared by the framework-catalog sync.
//!
//! Why: this module used to own trusty-mpm's session-workspace provisioner —
//! it cloned a `repo_url` into a shared base checkout and added a per-session
//! `git worktree` for it. ADR-0055 removed that path (#6000): trusty-mpm
//! clones nothing and creates no worktree on `session_new`'s behalf, and a
//! `repo_url` that is not an existing local directory is refused outright by
//! [`crate::core::local_repo_url::require_local_repo_url`]. What survives here
//! is the [`GitBackend`] trait seam and its two implementations, which
//! `content::catalog_sync` still uses to clone and refresh the framework
//! catalog. The worktrees a session runs in are created by
//! `daemon::managed_routes::inproject`, which has never used this trait.
//! What: [`GitBackend`] (clone, repo detection, remote lookup, fetch-and-reset),
//! the shelling-out [`RealGitBackend`], and the [`FakeGitBackend`] test double.
//! Test: `git_identity_env_applied_to_command`,
//! `git_identity_commit_args_applied_to_command`,
//! `default_identity_produces_plain_git_command`.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Errors produced by a [`GitBackend`] operation.
///
/// Why: callers need structured errors to distinguish a git failure from an
/// I/O one. `#[non_exhaustive]` so a future variant is not a breaking change.
/// What: one variant per failure class.
/// Test: each variant is exercised by the catalog-sync tests.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProvisionError {
    /// The git clone or checkout operation failed.
    #[error("git error: {0}")]
    Git(String),

    /// Directory creation or I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Trait seam over the git operations catalog sync performs.
///
/// Why: catalog sync must be testable without a real git remote or network;
/// the trait lets tests inject a FakeGitBackend.
/// What: clone, inspect, and update operations on a target path.
/// Test: FakeGitBackend in this module's test section.
pub trait GitBackend: Send + Sync {
    /// Clone repo_url at git_ref into target_dir.
    ///
    /// Why: catalog sync calls this to establish the framework catalog checkout.
    /// What: performs git clone --branch git_ref repo_url target_dir (or equivalent).
    /// Test: FakeGitBackend records the call and creates the directory.
    fn clone_repo(
        &self,
        repo_url: &str,
        git_ref: &str,
        target_dir: &Path,
    ) -> Result<(), ProvisionError>;

    /// Return true if `dir` contains a valid git repository.
    ///
    /// Why: catalog sync must distinguish a valid checkout from an absent or
    /// corrupt directory so it can decide whether to clone, update, or re-clone.
    /// What: checks for the presence of a `.git` subdirectory (or `.git` file
    /// for worktrees) inside `dir`.
    /// Test: FakeGitBackend checks for the `.git/` dir it creates during clone.
    fn is_git_repo(&self, dir: &Path) -> bool;

    /// Return the URL of the `origin` remote configured in `dir`.
    ///
    /// Why: catalog sync verifies the existing checkout points at the expected
    /// remote before deciding to update in place vs. re-clone.
    /// What: queries the git configuration of the repository at `dir` for the
    /// `origin` remote URL.
    /// Test: FakeGitBackend reads from the `.git/config` it writes during clone.
    fn remote_url(&self, dir: &Path) -> Result<String, ProvisionError>;

    /// Fetch the specific `git_ref` from `origin` and hard-reset to `FETCH_HEAD`.
    ///
    /// Why: catalog sync updates an existing valid checkout in place so local
    /// drift (manual edits, partial state) can never block the update; using
    /// FETCH_HEAD avoids `origin/<ref>` which only works for branches, not tags
    /// or SHAs.
    /// What: runs `git -C dir fetch origin <git_ref>` then
    /// `git -C dir reset --hard FETCH_HEAD` — works for branches, tags, and SHAs.
    /// Test: FakeGitBackend returns Ok(()) without performing real git operations.
    fn fetch_and_reset(&self, dir: &Path, git_ref: &str) -> Result<(), ProvisionError>;
}

/// Real git backend that shells out to the `git` binary.
///
/// Why: production usage requires actual git operations against real remotes.
/// What: runs `git clone --depth 1 [--branch <ref>] <url> <dir>` as a
/// subprocess. When `git_ref` is blank the `--branch` flag is OMITTED so git
/// uses the remote's default branch (HEAD) — passing `--branch ""` to git
/// would produce `fatal: '' is not a valid branch name` and fail.
/// Test: `default_identity_produces_plain_git_command` and the two
/// `git_identity_*_applied_to_command` tests; catalog sync exercises the git
/// calls themselves against a `FakeGitBackend`.
#[derive(Debug, Clone, Default)]
pub struct RealGitBackend {
    /// Resolved per-project GitHub identity (#2184): env overrides applied to
    /// every git subprocess plus an optional commit author override. The
    /// `Default` (empty) identity reproduces pre-#2184 behaviour exactly — no
    /// env overrides, no `-c user.*` args — so every construction site that
    /// has no project context (`RealGitBackend::default()`, e.g. the
    /// framework catalog sync) is unaffected.
    identity: crate::core::git_identity::GitIdentity,
}

impl RealGitBackend {
    /// Construct a backend bound to a resolved per-project [`GitIdentity`](crate::core::git_identity::GitIdentity)
    /// (#2184).
    ///
    /// Why: a caller resolves ONE identity per project (via
    /// `core::git_identity::resolve_for_config`) and must apply it to every git
    /// subprocess this backend runs.
    /// What: stores `identity`; every `GitBackend` method below applies it via
    /// [`Self::command`].
    /// Test: `git_identity_env_applied_to_command`,
    /// `git_identity_commit_args_applied_to_command`.
    pub fn new(identity: crate::core::git_identity::GitIdentity) -> Self {
        Self { identity }
    }

    /// Build a `git` [`std::process::Command`] with this backend's resolved
    /// identity applied.
    ///
    /// Why: every git subprocess this backend spawns must apply the SAME
    /// identity (env overrides for auth, `-c user.*` for commit authorship);
    /// centralising the construction means no call site can diverge or forget
    /// one half of the identity.
    /// What: `-c user.name=…`/`-c user.email=…` (when set) are added BEFORE
    /// any subcommand args (git accepts `-c` overrides only in that
    /// position); the resolved env overrides are applied via `Command::envs`.
    /// An empty (`Default`) identity produces a plain `git` command
    /// byte-for-byte equivalent to the pre-#2184 `Command::new("git")`.
    /// Test: `git_identity_env_applied_to_command`,
    /// `git_identity_commit_args_applied_to_command`.
    fn command(&self) -> std::process::Command {
        // #7171: through the shared entry point so the catalog-sync clone/
        // fetch this backend runs cannot trigger a background
        // `git maintenance run --auto` against the shared object store.
        let mut cmd = trusty_common::git::command();
        cmd.args(self.identity.commit_config_args());
        // #6668: clear the inherited identity before applying the bound one —
        // a credential helper reads GH_TOKEN ahead of GH_CONFIG_DIR.
        for key in &self.identity.env_remove {
            cmd.env_remove(key);
        }
        cmd.envs(self.identity.env.iter().cloned());
        cmd
    }
}

impl GitBackend for RealGitBackend {
    fn clone_repo(
        &self,
        repo_url: &str,
        git_ref: &str,
        target_dir: &Path,
    ) -> Result<(), ProvisionError> {
        // When `git_ref` is blank omit `--branch` entirely so git clones the
        // remote's default branch (HEAD). An empty `--branch ""` arg causes
        // git to fail with "not a valid branch name" rather than falling back.
        let dir_str = target_dir.to_string_lossy().into_owned();
        // `--progress` forces git to report byte/object progress even though
        // stderr is a pipe (not a TTY); `clone_with_progress` streams it as
        // `CloningRepo` stage detail (#2605).
        let mut args: Vec<&str> = vec!["clone", "--progress", "--depth", "1"];
        if !git_ref.trim().is_empty() {
            args.push("--branch");
            args.push(git_ref);
        }
        args.push(repo_url);
        args.push(&dir_str);
        // Use the destination's parent as cwd so a deleted inherited cwd cannot
        // cause git to fail with "fatal: Unable to read current working directory".
        let cwd = target_dir.parent().unwrap_or(std::path::Path::new("/"));
        // #2184: applies the resolved per-project identity (env overrides +
        // commit args) to this clone; a `Default` identity is a no-op.
        let mut cmd = self.command();
        cmd.args(&args).current_dir(cwd);
        let outcome = super::clone_progress::clone_with_progress(cmd)
            .map_err(|e| ProvisionError::Git(format!("git clone exec failed: {e}")))?;
        if outcome.success {
            Ok(())
        } else {
            Err(ProvisionError::Git(format!(
                "git clone failed: {}",
                outcome.stderr
            )))
        }
    }

    fn is_git_repo(&self, dir: &Path) -> bool {
        // `.git` can be a directory (normal clone) or a file (worktree).
        let dot_git = dir.join(".git");
        dot_git.is_dir() || dot_git.is_file()
    }

    fn remote_url(&self, dir: &Path) -> Result<String, ProvisionError> {
        let dir_s = dir.to_string_lossy();
        let out = self
            .command()
            .args(["-C", &dir_s, "remote", "get-url", "origin"])
            .output()
            .map_err(|e| ProvisionError::Git(format!("git remote get-url exec failed: {e}")))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(ProvisionError::Git(format!(
                "git remote get-url failed: {stderr}"
            )))
        }
    }

    fn fetch_and_reset(&self, dir: &Path, git_ref: &str) -> Result<(), ProvisionError> {
        let dir_s = dir.to_string_lossy();
        // Fetch the specific ref so tags and SHAs work correctly.
        // `git fetch origin <ref>` writes FETCH_HEAD; `git reset --hard FETCH_HEAD`
        // then works for branches, tags, and commit SHAs alike — unlike
        // `origin/<ref>` which only resolves for branches.
        let fetch = self
            .command()
            .args(["-C", &dir_s, "fetch", "origin", git_ref])
            .output()
            .map_err(|e| ProvisionError::Git(format!("git fetch exec failed: {e}")))?;
        if !fetch.status.success() {
            let stderr = String::from_utf8_lossy(&fetch.stderr);
            return Err(ProvisionError::Git(format!("git fetch failed: {stderr}")));
        }
        let reset = self
            .command()
            .args(["-C", &dir_s, "reset", "--hard", "FETCH_HEAD"])
            .output()
            .map_err(|e| ProvisionError::Git(format!("git reset exec failed: {e}")))?;
        if reset.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&reset.stderr);
            Err(ProvisionError::Git(format!("git reset failed: {stderr}")))
        }
    }
}

/// Fake git backend for unit tests.
///
/// Why: unit tests must not require a real git remote or network.
/// What: records clone calls and creates the target directory to simulate a checkout.
/// Use `new()` for a permissive fake; use `new_strict()` to simulate real `git clone`
/// exit-128 failures when the target directory already exists.
/// Test: used by the catalog-sync idempotency tests.
pub struct FakeGitBackend {
    /// Calls recorded for assertions.
    pub calls: std::sync::Mutex<Vec<(String, String, PathBuf)>>,
    /// When true, clone_repo returns an error if target_dir already exists,
    /// mirroring real `git clone` exit 128 behaviour.
    strict: bool,
}

impl FakeGitBackend {
    /// Construct a new FakeGitBackend with an empty call log (permissive mode).
    ///
    /// Why: tests need a fresh call log for each test case.
    /// What: initialises the Mutex-guarded vec with `strict = false`.
    /// Test: used in every provisioner unit test.
    pub fn new() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            strict: false,
        }
    }

    /// Construct a FakeGitBackend that fails `clone_repo` when the target already exists.
    ///
    /// Why: the idempotency regression test (`catalog_sync_second_sync_succeeds`)
    /// must fail against pre-fix unconditional-clone code and pass only when
    /// `ensure_repo` routes the second call to the update path instead of re-cloning.
    /// What: same as `new()` but `strict = true`; clone_repo returns
    /// `ProvisionError::Git` (simulating exit 128) if the target directory exists.
    /// Test: catalog_sync_second_sync_succeeds.
    pub fn new_strict() -> Self {
        Self {
            calls: std::sync::Mutex::new(Vec::new()),
            strict: true,
        }
    }
}

impl Default for FakeGitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl GitBackend for FakeGitBackend {
    fn clone_repo(
        &self,
        repo_url: &str,
        git_ref: &str,
        target_dir: &Path,
    ) -> Result<(), ProvisionError> {
        self.calls.lock().unwrap().push((
            repo_url.to_owned(),
            git_ref.to_owned(),
            target_dir.to_owned(),
        ));
        // In strict mode, mirror real `git clone` exit 128: fail when target exists.
        // This makes catalog_sync_second_sync_succeeds a genuine regression guard —
        // it fails against pre-fix unconditional-clone code and passes only when
        // ensure_repo routes the second call to fetch_and_reset instead of clone.
        if self.strict && target_dir.exists() {
            return Err(ProvisionError::Git(format!(
                "git clone failed (exit 128): destination path '{}' already exists and is not an empty directory",
                target_dir.display()
            )));
        }
        // Simulate a clone: create a minimal .git/config so is_git_repo and
        // remote_url work correctly in subsequent calls (e.g. second sync).
        let git_dir = target_dir.join(".git");
        std::fs::create_dir_all(&git_dir)?;
        let config = format!(
            "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = {repo_url}\n"
        );
        std::fs::write(git_dir.join("config"), config)?;
        Ok(())
    }

    fn is_git_repo(&self, dir: &Path) -> bool {
        dir.join(".git").is_dir()
    }

    fn remote_url(&self, dir: &Path) -> Result<String, ProvisionError> {
        // Read the URL from the fake .git/config written during clone_repo.
        let config_path = dir.join(".git").join("config");
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| ProvisionError::Git(format!("no .git/config: {e}")))?;
        let mut in_origin = false;
        for line in content.lines() {
            let t = line.trim();
            if t == "[remote \"origin\"]" {
                in_origin = true;
            } else if t.starts_with('[') {
                in_origin = false;
            } else if in_origin && let Some(url) = t.strip_prefix("url = ") {
                return Ok(url.to_owned());
            }
        }
        Err(ProvisionError::Git(
            "no origin remote in .git/config".to_owned(),
        ))
    }

    fn fetch_and_reset(&self, _dir: &Path, _git_ref: &str) -> Result<(), ProvisionError> {
        // Fake: always succeeds — no network or filesystem operation needed.
        Ok(())
    }
}

#[cfg(test)]
mod tests;
