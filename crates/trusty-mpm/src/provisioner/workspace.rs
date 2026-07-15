//! Workspace provisioner implementation.
//!
//! Why: each managed session needs an isolated workspace so that agents, skills,
//! and configuration deployed there do not collide with the operator's live
//! checkout or with other concurrent sessions on the same repo. Before #1935
//! every session did a full, independent `git clone` of the target repo —
//! wasteful (duplicates the whole object database per session) and at odds
//! with this repo's own worktree-discipline convention (see root `CLAUDE.md`
//! §"Parallel Worktree Discipline": fetch `origin`, then `git worktree add` off
//! it). #1935 replaces the per-session clone with ONE persistent, shared base
//! checkout per project plus a per-session `git worktree add`, mirroring the
//! same shared-base-plus-worktrees pattern already proven by the in-project
//! spawn path (`daemon::managed_routes::inproject`).
//! What: [`WorkspaceProvisioner`] accepts (repo_url, ref, task, session_id),
//! ensures a persistent bare base checkout exists at
//! `<project_dir>/.base/` (cloning once via [`GitBackend::ensure_base_checkout`]),
//! then fetches `git_ref` fresh from `origin` and adds an isolated per-session
//! `git worktree` at `<project_dir>/.base/.worktrees/<session-id>/` via
//! [`GitBackend::worktree_add`]. It then calls `prepare_session` to deploy
//! agents/skills into that worktree, and returns a [`PreparedWorkspace`] with
//! the worktree path, repo_url, and the REQUESTED branch/ref (not the internal
//! per-session branch name backing the worktree).
//! Test: `provisioner_isolation_path`, `provisioner_path_not_in_existing_project`,
//! `provisioner_uses_session_id_subdir`, `provision_reuses_base_checkout_across_sessions`.

use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::{debug, info};

use crate::session_manager::ManagedSessionId;

/// Errors produced by the workspace provisioner.
///
/// Why: callers need structured errors to distinguish git failures from
/// prepare_session failures and I/O errors.
/// What: one variant per failure class.
/// Test: each variant is exercised by WorkspaceProvisioner unit tests.
#[derive(Debug, Error)]
pub enum ProvisionError {
    /// The git clone or checkout operation failed.
    #[error("git error: {0}")]
    Git(String),

    /// Directory creation or I/O failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The prepare_session step failed.
    #[error("session preparation failed: {0}")]
    PrepareSession(String),
}

/// Directory name (relative to a project directory) holding the persistent,
/// shared base git checkout that every session's worktree branches off of (#1935).
///
/// Why: naming this segment `.base` (rather than reusing the project
/// directory itself as the checkout root) guarantees it is EMPTY the first
/// time a project is provisioned under the new scheme — even on an existing
/// installation whose project directory already holds pre-#1935 per-session
/// full-clone subdirectories — so establishing the base checkout never needs
/// an old-layout migration step.
/// What: `".base"`.
/// Test: `provision_in_uses_explicit_project_dir`,
/// `provision_reuses_base_checkout_across_sessions`.
const BASE_CHECKOUT_DIRNAME: &str = ".base";

/// Describes an isolated workspace after provisioning.
///
/// Why: the caller (SessionManager) needs the workspace path to pass as cwd
/// to the RuntimeAdapter, and the repo_url + branch for storage in SessionRecord
/// so the calling agentic process can correlate sessions with GitHub artifacts.
/// What: bundles the three values returned after a successful provision().
/// Test: asserted by provisioner unit tests.
#[derive(Debug, Clone)]
pub struct PreparedWorkspace {
    /// Absolute path to the provisioned workspace directory.
    pub path: PathBuf,
    /// Repository URL that was cloned.
    pub repo_url: String,
    /// Git branch or ref that was checked out.
    pub branch: String,
}

/// Trait seam over git operations used by the provisioner and catalog sync.
///
/// Why: both the workspace provisioner and catalog sync must be testable without
/// a real git remote or network; the trait lets tests inject a FakeGitBackend.
/// What: clone, inspect, and update operations on a target path.
/// Test: FakeGitBackend in this module's test section.
pub trait GitBackend: Send + Sync {
    /// Clone repo_url at git_ref into target_dir.
    ///
    /// Why: the provisioner calls this to create an isolated checkout.
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

    /// Ensure a persistent, shared base checkout exists at `base_dir` (#1935).
    ///
    /// Why: every managed session used to pay for a full, independent clone.
    /// A single shared base checkout per project lets every session's git
    /// worktree share one object database, cloned only once.
    /// What: no-op (idempotent) when `base_dir` already looks like an
    /// established git directory; otherwise clones `repo_url` into `base_dir`.
    /// `RealGitBackend` clones `--bare` (see its impl doc for why); callers
    /// must not assume a working tree exists at `base_dir` itself — only
    /// [`worktree_add`](Self::worktree_add) produces checked-out working trees.
    /// Test: `FakeGitBackend` records the call and writes fake bare-checkout
    /// markers (`HEAD` + a `bare = true` `config`) so a second call is
    /// recognised as already-established (no re-clone), while a stale directory
    /// carrying only a stray `HEAD` is rejected — matching the real backend.
    fn ensure_base_checkout(&self, repo_url: &str, base_dir: &Path) -> Result<(), ProvisionError>;

    /// Fetch `git_ref` fresh from `origin` and add an isolated worktree for it.
    ///
    /// Why: sessions must branch off the LATEST `git_ref`, not whatever commit
    /// the base happened to be at when it was first cloned — mirroring this
    /// repo's own worktree-discipline convention ("always fetch origin, branch
    /// off origin/<ref>"). Fetching directly into a session-unique local branch
    /// (rather than mutating the shared `FETCH_HEAD`) also means concurrent
    /// session creation against the same base never races on a shared ref.
    /// What: runs `git -C base_dir fetch origin "+<src>:refs/heads/<branch_name>"`
    /// (where `<src>` is `git_ref`, or `HEAD` when `git_ref` is blank — mirroring
    /// `RealGitBackend::clone_repo`'s blank-ref handling) followed by
    /// `git -C base_dir worktree add <worktree_path> <branch_name>`. Works for
    /// branches, tags, and commit SHAs alike (each becomes its own dedicated
    /// local branch, so this never conflicts with a branch checked out
    /// elsewhere). `branch_name` is caller-chosen so it can double as the
    /// worktree-removal branch cleanup key (see
    /// `session_manager::decommission::remove_session_worktree`).
    /// Test: `FakeGitBackend` records the call and creates `worktree_path` on
    /// disk so callers that write into it (e.g. `TASK.md`) succeed.
    fn worktree_add(
        &self,
        base_dir: &Path,
        git_ref: &str,
        worktree_path: &Path,
        branch_name: &str,
    ) -> Result<(), ProvisionError>;
}

/// Real git backend that shells out to the `git` binary.
///
/// Why: production usage requires actual git operations against real remotes.
/// What: runs `git clone --depth 1 [--branch <ref>] <url> <dir>` as a
/// subprocess. When `git_ref` is blank the `--branch` flag is OMITTED so git
/// uses the remote's default branch (HEAD) — passing `--branch ""` to git
/// would produce `fatal: '' is not a valid branch name` and fail.
/// Test: used in the `#[ignore]` integration test only; unit tests use
/// `FakeGitBackend`. The empty-ref contract is locked in by
/// `blank_git_ref_omits_branch_flag` in `workspace.rs` tests.
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
    /// Construct a backend bound to a resolved per-project [`GitIdentity`]
    /// (#2184).
    ///
    /// Why: the daemon's `spawn_managed` path resolves ONE identity per spawn
    /// (via `core::git_identity::resolve_for_config`) and must apply it to
    /// every git subprocess the provisioner runs for that session.
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
        let mut cmd = std::process::Command::new("git");
        cmd.args(self.identity.commit_config_args());
        cmd.envs(self.identity.env.iter().cloned());
        cmd
    }
}

// #1935 review findings (PR #1936): bare-checkout detection + the advisory
// provisioning lock live in the `base_lock` submodule to keep this file under
// the 500-SLOC production cap — see that module's doc comment for the full
// rationale behind each item.
use base_lock::{
    acquire_base_checkout_lock, base_checkout_lock_path, fake_is_established_bare_checkout,
    is_established_bare_checkout, stale_base_dir_error, write_fake_bare_checkout,
};

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

    /// Why: this method used to treat `base_dir.join("HEAD").is_file()` as
    /// proof of an established bare checkout (trusty-review finding #2 on
    /// PR #1936 / #1935). A non-bare git repo ALSO has a root-level `HEAD`
    /// file, so a stale non-bare directory (e.g. left over from a pre-#1935
    /// full-clone install sharing this path) would be silently accepted as a
    /// valid base, and a later `git worktree add` against it would then fail
    /// with `'<branch>' is already checked out`. It also unconditionally
    /// attempted `git clone --bare` on a cache miss with no protection
    /// against two sessions provisioning the SAME project for the first time
    /// concurrently (finding #1): both observe "no base yet" and both start
    /// cloning into the identical target directory. Note that "recover by
    /// re-checking after the clone fails" is NOT sufficient here — real `git
    /// clone --bare` is not safe against a concurrent writer into the exact
    /// same directory; two interleaved clones can corrupt each other's
    /// partial checkout (observed empirically: colliding on copying
    /// `hooks/commit-msg.sample`) rather than cleanly failing with "already
    /// exists". Only mutual exclusion around the whole check-and-clone window
    /// avoids that.
    /// What: replaces the fragile file-existence probe with
    /// [`is_established_bare_checkout`], which asks git itself via
    /// `rev-parse --is-bare-repository` (returns `false`, never panics, for a
    /// non-bare repo or a non-repo directory) — this alone fixes finding #2.
    /// For finding #1, wraps the clone in [`acquire_base_checkout_lock`]: a
    /// dependency-free `OpenOptions::create_new` marker-file mutex (no
    /// file-locking crate such as `fs2`/`fs4` is currently a workspace
    /// dependency) that serializes concurrent callers for the SAME
    /// `base_dir`, with stale-lock recovery so a crashed process can never
    /// permanently deadlock future provisioning. After acquiring the lock,
    /// re-checks `is_established_bare_checkout` once more (another caller may
    /// have finished cloning while this one was waiting) before cloning. If the
    /// path is then found occupied by a non-empty stale/broken (non-bare)
    /// directory, returns an actionable [`stale_base_dir_error`] naming the
    /// exact path and the `rm -rf` command to clear it — rather than a cryptic
    /// clone failure or a destructive auto-delete (issue #1937 item 1).
    /// Test: `ensure_base_checkout_recovers_from_concurrent_race` (spawns
    /// real threads racing on the same `base_dir`, asserts every one returns
    /// `Ok` and exactly one valid bare checkout results),
    /// `ensure_base_checkout_rejects_stale_non_bare_directory` (pre-seeds a
    /// non-bare repo at `base_dir` and asserts a loud error, not silent
    /// reuse), both in `provisioner/workspace/tests.rs`.
    fn ensure_base_checkout(&self, repo_url: &str, base_dir: &Path) -> Result<(), ProvisionError> {
        // A bare clone has no working tree of its own, so it can never conflict
        // with a session worktree checking out the same branch (a non-bare
        // clone's own checked-out branch would collide with a worktree trying
        // to check out that same branch elsewhere). Bare-plus-worktrees is the
        // standard git pattern for exactly this "one shared base, many
        // isolated worktrees" shape (verified empirically before landing this:
        // `git clone --bare`, then repeated `git fetch origin +<ref>:refs/heads/<x>`
        // + `git worktree add` against it, works cleanly for branches, tags,
        // and SHAs alike).
        if is_established_bare_checkout(base_dir) {
            debug!(path = %base_dir.display(), "base checkout already present, reusing");
            return Ok(());
        }
        if let Some(parent) = base_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Serialize the check-and-clone window across concurrent callers
        // racing to provision the SAME project for the first time (finding
        // #1). The guard's `Drop` impl removes the marker file when this
        // function returns via any path (success, error, or early return).
        let lock_path = base_checkout_lock_path(base_dir);
        let _lock = acquire_base_checkout_lock(&lock_path)?;

        // Re-check now that the lock is held: another caller may have
        // completed the clone while this one was waiting for the lock.
        if is_established_bare_checkout(base_dir) {
            debug!(
                path = %base_dir.display(),
                "base checkout completed by a concurrent caller while waiting for the lock; reusing"
            );
            return Ok(());
        }

        // The path is confirmed NOT a valid bare checkout. If it is a non-empty
        // stale/broken directory (crashed mid-clone leftover, or a pre-#1935
        // non-bare clone) a `git clone --bare` here would fail with an opaque
        // "destination path already exists" message. Surface an actionable
        // error naming the exact path + the `rm -rf` command instead (issue
        // #1937 item 1). We do NOT auto-delete: removing operator data is too
        // destructive for an advisory recovery path.
        if let Some(err) = stale_base_dir_error(base_dir) {
            return Err(err);
        }

        // Use the destination's parent as cwd so a deleted inherited cwd cannot
        // cause git to fail with "fatal: Unable to read current working directory".
        let cwd = base_dir.parent().unwrap_or(std::path::Path::new("/"));
        // `--progress` + `clone_with_progress` streams byte/object percentages
        // as `CloningRepo` stage detail for the (long, on a large repo) base
        // clone — the dominant path for the in-project spawn (#2605).
        let mut cmd = self.command();
        cmd.args(["clone", "--bare", "--progress", repo_url])
            .arg(base_dir)
            .current_dir(cwd);
        let outcome = super::clone_progress::clone_with_progress(cmd)
            .map_err(|e| ProvisionError::Git(format!("git clone --bare exec failed: {e}")))?;
        if outcome.success {
            info!(url = %repo_url, dest = %base_dir.display(), "base checkout cloned");
            Ok(())
        } else {
            Err(ProvisionError::Git(format!(
                "git clone --bare failed: {}",
                outcome.stderr
            )))
        }
    }

    fn worktree_add(
        &self,
        base_dir: &Path,
        git_ref: &str,
        worktree_path: &Path,
        branch_name: &str,
    ) -> Result<(), ProvisionError> {
        let base_s = base_dir.to_string_lossy();
        // Blank git_ref means "the remote's default branch" — mirror clone_repo's
        // blank-ref handling by fetching `HEAD` (the remote's default branch
        // pointer) rather than an empty (invalid) ref name.
        let src_ref = if git_ref.trim().is_empty() {
            "HEAD"
        } else {
            git_ref
        };
        // Fetch directly into a session-unique local branch ref rather than the
        // shared FETCH_HEAD: two sessions provisioning against the SAME base
        // concurrently would otherwise race on which fetch's FETCH_HEAD the
        // other session's `worktree add` observes. The leading `+` allows a
        // non-fast-forward overwrite in case a retry reuses `branch_name`.
        let refspec = format!("+{src_ref}:refs/heads/{branch_name}");
        let fetch = self
            .command()
            .args(["-C", &base_s, "fetch", "origin", &refspec])
            .output()
            .map_err(|e| ProvisionError::Git(format!("git fetch exec failed: {e}")))?;
        if !fetch.status.success() {
            let stderr = String::from_utf8_lossy(&fetch.stderr);
            return Err(ProvisionError::Git(format!("git fetch failed: {stderr}")));
        }

        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let out = self
            .command()
            .args(["-C", &base_s, "worktree", "add"])
            .arg(worktree_path)
            .arg(branch_name)
            .output()
            .map_err(|e| ProvisionError::Git(format!("git worktree add exec failed: {e}")))?;
        if out.status.success() {
            info!(
                base = %base_dir.display(),
                worktree = %worktree_path.display(),
                branch = %branch_name,
                "per-session worktree created"
            );
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(ProvisionError::Git(format!(
                "git worktree add failed (exit {}): {stderr}",
                out.status
            )))
        }
    }
}

/// Fake git backend for unit tests.
///
/// Why: unit tests must not require a real git remote or network.
/// What: records clone calls and creates the target directory to simulate a checkout.
/// Use `new()` for a permissive fake; use `new_strict()` to simulate real `git clone`
/// exit-128 failures when the target directory already exists.
/// Test: used by every WorkspaceProvisioner unit test and catalog_sync_idempotent tests.
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

    fn ensure_base_checkout(&self, repo_url: &str, base_dir: &Path) -> Result<(), ProvisionError> {
        self.calls.lock().unwrap().push((
            repo_url.to_owned(),
            "ensure_base_checkout".to_owned(),
            base_dir.to_owned(),
        ));
        // Idempotent reuse, mirroring `RealGitBackend`'s
        // `rev-parse --is-bare-repository` semantics (issue #1937 item 3):
        // recognise an already-established base ONLY when it looks like a valid
        // (fake) bare checkout, not merely when a stray `HEAD` file exists.
        // This is the observable contract
        // `provision_reuses_base_checkout_across_sessions` pins down.
        if fake_is_established_bare_checkout(base_dir) {
            return Ok(());
        }
        // Mirror `RealGitBackend`: a non-empty directory that is NOT a valid
        // bare checkout is a stale/broken artifact — reject it loudly (with the
        // same actionable message) rather than silently reuse or clobber it, so
        // a fake-backend stale-directory test catches what a real one would.
        if let Some(err) = stale_base_dir_error(base_dir) {
            return Err(err);
        }
        // Simulate `git clone --bare`: write the structural markers that make
        // `fake_is_established_bare_checkout` recognise this as a valid base.
        write_fake_bare_checkout(base_dir)?;
        Ok(())
    }

    fn worktree_add(
        &self,
        base_dir: &Path,
        git_ref: &str,
        worktree_path: &Path,
        branch_name: &str,
    ) -> Result<(), ProvisionError> {
        self.calls.lock().unwrap().push((
            git_ref.to_owned(),
            branch_name.to_owned(),
            worktree_path.to_owned(),
        ));
        // Simulate a checked-out worktree: create the directory and a `.git`
        // file pointing back at the (fake) base, mirroring what a real
        // `git worktree add` produces, so downstream code that only checks
        // for directory existence (TASK.md write, prepare_session, etc.)
        // behaves the same as it would against a real worktree.
        std::fs::create_dir_all(worktree_path)?;
        std::fs::write(
            worktree_path.join(".git"),
            format!("gitdir: {}/worktrees/{branch_name}\n", base_dir.display()),
        )?;
        Ok(())
    }
}

/// Provisions isolated workspaces for managed agent sessions.
///
/// Why: each managed session must live in a directory that is entirely owned
/// by the session manager and never overlaps with the operator's project checkouts
/// or other concurrent sessions on the same repo.
/// What: clones the repository via a GitBackend into
/// ~/.trusty-mpm/workspaces/<project-slug>/<session-id>/, calls prepare_session
/// to deploy agents and write config files, and returns a PreparedWorkspace.
/// Test: provisioner_isolation_path, provisioner_path_not_in_existing_project.
pub struct WorkspaceProvisioner<G: GitBackend> {
    git: G,
    /// Root directory for all provisioned workspaces (~/.trusty-mpm/workspaces/).
    workspace_root: PathBuf,
    /// When false, `provision` skips the `prepare_session` deploy step.
    ///
    /// Why: `prepare_session` deploys agents/skills into the provisioned
    /// workspace's `.claude/` tree (issue #1931 — never the shared real
    /// `~/.claude/`); unit tests that only verify path isolation must not
    /// perform that filesystem side-effect (it races with other tests).
    /// Production always sets this to true via [`Self::new`].
    prepare: bool,
}

impl<G: GitBackend> WorkspaceProvisioner<G> {
    /// Construct a provisioner with the given git backend and workspace root.
    ///
    /// Why: the workspace root is injectable so tests can use a tempdir.
    /// What: stores git and workspace_root; `prepare` defaults to true so the
    /// agent/skill deploy runs in production. No I/O at construction time.
    /// Test: used in every provisioner unit test via `make_provisioner`.
    pub fn new(git: G, workspace_root: PathBuf) -> Self {
        Self {
            git,
            workspace_root,
            prepare: true,
        }
    }

    /// Construct a provisioner that skips the `prepare_session` deploy step.
    ///
    /// Why: unit tests exercise path isolation without performing the global
    /// `~/.claude/` agent deploy that `prepare_session` does.
    /// What: identical to [`Self::new`] but with `prepare = false`.
    /// Test: used by the provisioner unit tests in this module.
    #[doc(hidden)]
    pub fn without_prepare(git: G, workspace_root: PathBuf) -> Self {
        Self {
            git,
            workspace_root,
            prepare: false,
        }
    }

    /// Provision an isolated workspace for the given session.
    ///
    /// Why: each session needs a fresh, isolated git worktree so agents and
    /// config never collide across sessions or with the operator's live
    /// project, without paying for a full independent clone per session (#1935).
    /// What: derives the project directory
    /// (workspace_root/<project-slug>/), delegates to [`Self::provision_in`]
    /// which ensures a shared base checkout and adds a per-session worktree at
    /// `<project-slug>/.base/.worktrees/<session-id>/`, runs prepare_session
    /// inside it, and returns a PreparedWorkspace with path, repo_url, and branch.
    /// Test: provisioner_isolation_path, provisioner_uses_session_id_subdir.
    pub fn provision(
        &self,
        session_id: &ManagedSessionId,
        repo_url: &str,
        git_ref: &str,
        task: &str,
    ) -> Result<PreparedWorkspace, ProvisionError> {
        let project_slug = repo_slug(repo_url);
        let project_dir = self.workspace_root.join(&project_slug);
        self.provision_in(&project_dir, session_id, repo_url, git_ref, task)
    }

    /// Provision an isolated workspace under an explicit project directory.
    ///
    /// Why: #1220 nests session workspaces under
    /// `~/trusty-mpm-projects/<owner>/<repo>/`, where the `<owner>/<repo>`
    /// project home is resolved by the caller from the target repo's GitHub
    /// remote (see [`crate::core::trusty_tools_config`]). The legacy
    /// [`Self::provision`] derives a single-segment slug from the URL; this
    /// variant lets the caller supply the pre-resolved two-segment project
    /// directory. #1935: rather than a full clone per session, both variants
    /// now share ONE persistent base checkout per project
    /// (`<project_dir>/.base/`, established once via
    /// [`GitBackend::ensure_base_checkout`]) plus a per-session `git worktree`
    /// (`<project_dir>/.base/.worktrees/<session-id>/`, added fresh every call
    /// via [`GitBackend::worktree_add`]) — see the module doc for the full
    /// rationale. Nesting worktrees under the NEW `.base/` directory (rather
    /// than reusing `project_dir` as the checkout root directly) means an
    /// existing installation's pre-#1935 per-session full clones sitting
    /// directly under `project_dir` can never collide with the fresh base
    /// checkout: `.base/` is guaranteed empty on first use, so no migration
    /// step is required.
    /// What: computes `base_dir = project_dir/.base` and
    /// `workspace_path = base_dir/.worktrees/<session_id>`; ensures the base
    /// checkout exists, fetches `git_ref` fresh from `origin` into an isolated
    /// worktree at `workspace_path`, writes the session-manager worktree
    /// ownership sentinel (so `session_manager::decommission` can safely
    /// `git worktree remove --force` it later), runs `prepare_session` (unless
    /// `prepare` is false), and returns the [`PreparedWorkspace`] — `branch` is
    /// the REQUESTED `git_ref`, not the internal per-session branch name
    /// backing the worktree (that internal name is `session_id` itself, chosen
    /// so `decommission`'s existing branch cleanup — which deletes a branch
    /// named after the worktree's leaf directory — finds and removes it).
    /// Test: `provision_in_uses_explicit_project_dir`,
    /// `provision_reuses_base_checkout_across_sessions`.
    pub fn provision_in(
        &self,
        project_dir: &Path,
        session_id: &ManagedSessionId,
        repo_url: &str,
        git_ref: &str,
        task: &str,
    ) -> Result<PreparedWorkspace, ProvisionError> {
        let base_dir = project_dir.join(BASE_CHECKOUT_DIRNAME);
        let workspace_path = base_dir
            .join(crate::session_manager::decommission::WORKTREES_DIRNAME)
            .join(session_id.to_string());
        // The branch backing this worktree is named after the session id
        // itself (not e.g. "session/<id>") so that
        // `session_manager::decommission::remove_session_worktree`'s branch
        // cleanup step — which deletes a branch named after the worktree's
        // leaf directory name — targets the right ref.
        let branch_name = session_id.to_string();

        debug!(
            session = %session_id,
            base = %base_dir.display(),
            path = %workspace_path.display(),
            repo = %repo_url,
            git_ref = %git_ref,
            "provisioning workspace"
        );

        // Announce the clone/checkout stage (issue #1904). No-op unless a
        // `daemon::managed_routes::lifecycle::spawn_managed` caller wrapped
        // this call in `provisioning_stage::scoped` — see that module's doc
        // for why this seam does not take a `DaemonState` parameter.
        crate::core::provisioning_stage::emit(
            crate::core::provisioning_stage::ProvisioningStage::CloningRepo,
        );
        self.git.ensure_base_checkout(repo_url, &base_dir)?;
        self.git
            .worktree_add(&base_dir, git_ref, &workspace_path, &branch_name)?;

        // Item 5 mirror (#1845 / #1935): write the SM ownership sentinel so
        // `remove_session_worktree` can confirm this worktree was TM-created
        // before ever running `git worktree remove --force` against it.
        // Best-effort: a failure here is a non-fatal warning — the worktree
        // was created successfully and decommission's naming-convention
        // fallback still recognises `.worktrees/<id>` paths.
        let sentinel =
            workspace_path.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE);
        if let Err(e) = std::fs::write(&sentinel, b"") {
            tracing::warn!(
                session = %session_id,
                path = %workspace_path.display(),
                "failed to write worktree ownership sentinel (non-fatal): {e}"
            );
        }

        // Write the task description into TASK.md at the workspace root so the
        // agent can read it as its initial brief (closes #1693).
        if !task.is_empty() {
            let task_file = workspace_path.join("TASK.md");
            if let Err(e) = std::fs::write(&task_file, task) {
                tracing::warn!(
                    session = %session_id,
                    path = %task_file.display(),
                    "failed to write TASK.md: {e}"
                );
            }
        }

        // Best-effort: pre-seed workspace trust + renderer-upsell dismissal into
        // ~/.claude.json so the session starts without blocking startup prompts.
        // Non-fatal: a seed failure must never abort provisioning (closes #1696).
        if let Err(e) = crate::core::home_trust_seed::preseed_home_trust(&workspace_path) {
            tracing::warn!(
                session = %session_id,
                path = %workspace_path.display(),
                "home trust pre-seed failed (non-fatal): {e}"
            );
        }

        if !self.prepare {
            return Ok(PreparedWorkspace {
                path: workspace_path,
                repo_url: repo_url.to_owned(),
                branch: git_ref.to_owned(),
            });
        }

        // Run prepare_session inside the isolated workspace. Agent/skill
        // deployment is best-effort: the critical guarantee of this method is the
        // ISOLATED CHECKOUT. If the framework is not installed (or deploy fails
        // for any reason) we log and continue so a session can still start — the
        // operator can run `tm install` / `tm catalog sync` to populate agents.
        //
        // #1931: use `for_managed_workspace(&workspace_path)`, NOT `default()` —
        // this workspace IS the harness cwd for the spawned session, so deployed
        // agents/skills must land in `<workspace_path>/.claude/{agents,skills}`
        // (where Claude Code's project-skill discovery looks), not the real
        // `$HOME/.claude`. Mirrors the sibling fix in
        // `daemon::managed_routes::lifecycle::spawn_managed_inproject`.
        let fw = crate::core::paths::FrameworkPaths::for_managed_workspace(&workspace_path);
        // Thread the cloned-from `repo_url` so the trusty-memory MCP injection
        // pins `env.TRUSTY_MEMORY_PALACE` to the project's `owner-repo` slug
        // (issue #1605). Without it the injector would fall back to deriving the
        // palace from the throwaway `<session-id>` workspace basename — the
        // WRONG palace for a cloned session.
        match crate::core::session_launch::prepare_session_with_repo_url(
            &fw,
            &workspace_path,
            Some(repo_url),
        ) {
            Ok(report) => {
                info!(
                    session = %session_id,
                    deployed = report.deploy.deployed.len(),
                    path = %workspace_path.display(),
                    "workspace provisioned and session prepared"
                );
                // Issue #2149: `prepare_session_with_repo_url` no longer aborts
                // on a roster-deploy failure, so a broken agent/skill catalog
                // would otherwise only show up as a suspiciously low
                // `deployed` count above. Surface it loudly and explicitly —
                // this is the gap that previously shipped a session with no
                // roster AND no trusty-mpm identity.
                for err in &report.roster_errors {
                    tracing::error!(
                        session = %session_id,
                        path = %workspace_path.display(),
                        "roster provisioning gap (session still launches with its \
                         trusty-mpm identity): {err}"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    session = %session_id,
                    path = %workspace_path.display(),
                    "workspace provisioned but session prep failed (best-effort): {e}"
                );
            }
        }

        // DOC-28 §5/§7 Phase 2 (R3, epic #1855): auto-seed the trusty-mpm
        // identity prompt-fact the first time a workspace is provisioned for a
        // managed session. Gated on `self.prepare` (not a dedicated config
        // toggle, per the owner's "don't over-engineer" call) so the
        // lightweight `without_prepare` test provisioner never performs this
        // network I/O, exactly like the `prepare_session` step above.
        // Idempotency is enforced inside the helper via a kg_query guard, and
        // it is entirely fail-open: any daemon-unreachable/parse error is
        // logged and swallowed, never propagated here.
        if self.prepare {
            // Discovery-first (issue #2030): resolves TRUSTY_MEMORY_URL when
            // set, else the daemon's actual discovered bound address, never
            // a hardcoded port.
            let memory_url =
                trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable();
            let override_value = trusty_common::palace_override_from_env();
            match trusty_common::derive_palace_id(
                &workspace_path,
                Some(repo_url),
                override_value.as_deref(),
            ) {
                Some(palace_id) => {
                    super::identity_seed::seed_identity_prompt_fact_blocking(
                        &memory_url,
                        &palace_id,
                    );
                }
                None => {
                    tracing::debug!(
                        session = %session_id,
                        "identity-seed: skipping — no palace could be derived for this workspace"
                    );
                }
            }
        }

        Ok(PreparedWorkspace {
            path: workspace_path,
            repo_url: repo_url.to_owned(),
            branch: git_ref.to_owned(),
        })
    }
}

/// Derive a filesystem-safe slug from a repository URL.
///
/// Why: the workspace path encodes the project identity so multiple sessions
/// on the same repo are grouped under one project directory.
/// What: extracts the repo name from the URL (last path component), strips
/// the .git suffix, and lowercases the result.
/// Test: `repo_slug_extraction` in tests.
fn repo_slug(repo_url: &str) -> String {
    let name = repo_url
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("unknown");
    name.trim_end_matches(".git").to_lowercase()
}

mod base_lock;

#[cfg(test)]
mod tests;
