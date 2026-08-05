//! Advisory locking + checkout detection for `RealGitBackend::ensure_base_checkout`.
//!
//! Why: split out of `workspace.rs` (which grew past the 500-SLOC production
//! cap once trusty-review's PR #1936 / #1935 findings were fixed) to keep
//! both files under the mechanical `scripts/check_line_cap.sh` gate. Grouping
//! these items together here — rather than trimming their Why/What/Test
//! documentation — keeps the fix's rationale fully documented in place.
//! What: [`is_established_checkout`] replaces the fragile `HEAD.is_file()`
//! idempotency probe (review finding #2) with a real `git rev-parse` check;
//! [`acquire_base_checkout_lock`] (backed by [`BaseCheckoutLock`], a
//! dependency-free marker-file mutex) plus [`lock_is_stale`] serialize the
//! check-and-clone window across concurrent callers to fix the TOCTOU
//! provisioning race (review finding #1). [`stale_base_dir_error`] turns the
//! "the base path is occupied by a broken directory" state into an actionable
//! operator error (issue #1937 item 1), and is shared by BOTH backends;
//! [`fake_is_established_checkout`] / [`write_fake_checkout`] let
//! `FakeGitBackend` mirror the real backend's `rev-parse`-based validity
//! semantics rather than a superficial file-exists probe (issue #1937 item 3).
//! [`stale_base_quarantine_path`] backs that error's non-destructive recovery
//! hint (issue #3605: the hint used to be a literal `rm -rf`).
//!
//! #4270 moved the base checkout from `<project_dir>/.base` (bare) to
//! `<project_dir>` itself (non-bare), converging on the shape the in-project
//! spawn path already produces. The detection predicate moved with it, and
//! [`legacy_base_store_error`] guards the one case that move introduces: a
//! project directory that still holds a legacy `.base` store whose worktrees
//! are live.
//! Test: `ensure_base_checkout_recovers_from_concurrent_race`,
//! `ensure_base_checkout_rejects_stale_directory`,
//! `base_checkout_lock_recovers_stale_lock_marker`,
//! `fake_ensure_base_checkout_rejects_stale_directory`,
//! `stale_base_dir_error_suggests_quarantine_not_deletion`,
//! `provision_in_leaves_an_existing_dot_base_store_untouched`, all in
//! `provisioner/workspace/tests.rs`.

use std::path::{Path, PathBuf};

use super::ProvisionError;

/// Determine whether `dir` is an already-established git checkout ROOT.
///
/// Why: the previous idempotency guard for [`super::RealGitBackend`]'s
/// `ensure_base_checkout` (`base_dir.join("HEAD").is_file()`) only checks for
/// a file literally named `HEAD` at the root of `base_dir` — it never
/// confirms that directory is actually a valid, complete git repository. A
/// directory that merely contains a stray `HEAD` file (e.g. left behind by a
/// clone that crashed mid-flight — git writes `HEAD` early, before the rest
/// of the object database/refs — or any other stale/corrupt artifact) would
/// pass the old check and be silently mistaken for an established shared base
/// (trusty-review finding #2 on PR #1936 / #1935). Asking git itself cannot be
/// fooled by directory layout alone. This helper is also reused to resolve the
/// TOCTOU provisioning race (finding #1): after acquiring
/// [`acquire_base_checkout_lock`], re-running this check tells the caller
/// whether a concurrent caller already finished establishing a valid base
/// while this one was waiting.
///
/// #4270: the base checkout is now the non-bare clone at `<project_dir>`, the
/// same one `daemon::managed_routes::inproject::ensure_base_clone` establishes,
/// so the predicate asks for a WORKING TREE whose root is `dir` itself. The
/// toplevel comparison is load-bearing twice over: a `dir` nested inside some
/// unrelated enclosing repository would otherwise report `true` without holding
/// a clone of its own, and a bare repository (the legacy `.base` shape) reports
/// `false` because it has no working tree at all.
/// What: shells out to `git -C dir rev-parse --show-toplevel` and returns
/// `true` only when the command exits successfully AND the printed toplevel
/// canonicalizes to the same path as `dir`. Returns `false` (never panics) if
/// the subprocess fails to spawn, exits non-zero (not a git working tree),
/// or names a different root.
/// Test: `ensure_base_checkout_recovers_from_concurrent_race`,
/// `ensure_base_checkout_rejects_stale_directory`.
pub(super) fn is_established_checkout(dir: &Path) -> bool {
    use std::process::Command;
    let dir_s = dir.to_string_lossy();
    let out = match Command::new("git")
        .args(["-C", &dir_s, "rev-parse", "--show-toplevel"])
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return false,
    };
    let toplevel = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    match (std::fs::canonicalize(&toplevel), std::fs::canonicalize(dir)) {
        (Ok(top), Ok(here)) => top == here,
        _ => false,
    }
}

/// Directory name of the LEGACY per-project bare base store retired by #4270.
///
/// Why: one literal, shared by the two guards that must recognise a legacy
/// store and refuse to touch it. Re-exported from [`crate::core::harness_root`]
/// rather than respelled, so the reader that maps a `.base` common-dir back to
/// its project and the writer that refuses to clobber it can never disagree.
/// What: `.base`.
/// Test: `provision_in_leaves_an_existing_dot_base_store_untouched`.
pub(super) use crate::core::harness_root::BASE_CLONE_DIRNAME as LEGACY_BASE_DIRNAME;

/// Build the refusal for a project directory holding git or trusty-mpm state —
/// or `None` when it holds none.
///
/// Why (#4270): provisioning now clones the base into `<project_dir>` itself,
/// and `git clone` refuses a non-empty destination. Before #4270 `base_dir` was
/// `<project_dir>/.base`, which for an in-project-shaped project does not
/// exist, so [`stale_base_dir_error`] returned `None` and its `mv` hint was
/// unreachable there. Pointing `base_dir` at the project directory made that
/// hint reachable — aimed at the directory holding `.worktrees/<id>` for every
/// live session, and at `.base` with every worktree beneath it. That is the
/// #3605 string with a bigger target, so the whole class is diverted here to a
/// refusal that suggests nothing which moves or deletes anything.
///
/// Reaching this on a HEALTHY repository is the case that makes it matter:
/// [`is_established_checkout`] reports `false` for a transient git spawn
/// failure, a missing `git` binary, and a dubious-ownership rejection alike, so
/// a momentary hiccup lands here with `.git` present. Naming that state and
/// stopping is right in every one of those; suggesting a rename is not.
/// What: returns `Some(ProvisionError::Git(..))` naming `project_dir` and the
/// protected entry [`crate::core::harness_root::protected_state_in`] found;
/// `None` otherwise, leaving the caller on its normal path.
/// Test: `provision_in_leaves_an_existing_dot_base_store_untouched`,
/// `stale_base_dir_error_never_offers_to_move_a_dir_holding_live_worktrees`.
fn protected_dir_error(project_dir: &Path) -> Option<ProvisionError> {
    let found = crate::core::harness_root::protected_state_in(project_dir)?;
    Some(ProvisionError::Git(format!(
        "cannot establish the base checkout at {project}: it holds {found}, so it \
         already carries git or trusty-mpm state.\n\
         \n\
         Anything under it may be in use — a `{worktrees}/<id>` worktree, or a \
         `{legacy}` store owning worktrees of its own — and one of those sessions may \
         be live right now. trusty-mpm will not move, rename, or delete any of it, and \
         neither should you while a session is running.\n\
         \n\
         If this project has a `{legacy}` store, retiring it is a manual step for once \
         no session needs it (#4270). If `.git` is present, this path expected git to \
         recognise a checkout here and it did not — check that `git -C {project} \
         rev-parse --show-toplevel` succeeds before retrying.",
        project = project_dir.display(),
        legacy = LEGACY_BASE_DIRNAME,
        worktrees = crate::session_manager::decommission::WORKTREES_DIRNAME,
    )))
}

/// Build the timestamped sibling path a stale base directory should be
/// QUARANTINED to (moved aside), rather than deleted.
///
/// Why: the recovery hint in [`stale_base_dir_error`] is read and executed
/// verbatim by autonomous agents, so it must name a concrete destination —
/// an agent handed `mv <path> <somewhere-you-choose>` will improvise, and an
/// agent handed a shell-substitution (`$(date …)`) may mangle it. Emitting a
/// fully-resolved path keeps the suggested command copy-pasteable while
/// keeping it non-destructive. The timestamp suffix means repeated recoveries
/// never collide, so an earlier quarantine is never clobbered by a later one.
/// What: `<base_dir>.stale-<UTC YYYYmmdd-HHMMSS>`, a sibling of `base_dir`
/// (same parent, so the `mv` is a cheap same-filesystem rename).
/// Test: `stale_base_dir_error_suggests_quarantine_not_deletion`.
pub(super) fn stale_base_quarantine_path(base_dir: &Path) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let mut name = base_dir.as_os_str().to_os_string();
    name.push(format!(".stale-{stamp}"));
    PathBuf::from(name)
}

/// Build an actionable error for a base-checkout path occupied by a broken
/// directory — or return `None` when the path is safe to clone into.
///
/// Why: `ensure_base_checkout` correctly refuses to reuse a base path that is
/// not a valid checkout (a leftover from a crashed mid-clone, or any other
/// stale directory), but the resulting failure used to be an opaque
/// `git clone` "destination path already exists" message that gives the
/// operator no recovery path (issue #1937 item 1). This error is, however,
/// consumed almost exclusively by an autonomous agent, which executes a
/// suggested command verbatim — so the message's own recovery hint is a
/// privileged instruction channel, not advice. The original hint was a literal
/// `rm -rf <path>` and is the most plausible trigger of the 2026-07-21 incident
/// (issue #3605) in which `<project>/.base` was destroyed and ~70 worktrees were
/// orphaned machine-wide, each then emitting phantom git-discovery test
/// failures. `.base` is SHARED by every worktree of the project, so deleting it
/// is never a local, recoverable act. The hint is therefore QUARANTINE
/// (`mv` aside to [`stale_base_quarantine_path`]) — matching this crate's
/// existing corrupt-state convention (`core::mcp_config`,
/// `core::standalone::trust_seed` both rename rather than delete) — and the
/// message states the shared-ownership blast radius explicitly, which is the
/// context whose absence made the destructive hint look safe to follow.
/// What: returns `None` when `base_dir` is absent or empty (either is safe —
/// `git clone` accepts a missing or empty target); otherwise returns
/// `Some(ProvisionError::Git(..))` whose message names the exact path, warns
/// that the path is shared across sessions, and gives a single non-destructive
/// `mv <path> <path>.stale-<timestamp>` command. It never emits a recursive
/// delete. A whole class is answered ahead of that, via [`protected_dir_error`]:
/// a directory holding git or trusty-mpm state gets a refusal that suggests
/// moving nothing (#4270), because the `mv` hint aimed at a project directory
/// would rename live worktrees out from under their sessions. Only foreign
/// debris — no `.git`, no `.base`, no `.worktrees` — still gets the quarantine
/// hint. Callers MUST invoke this only AFTER confirming the path is not already
/// a valid checkout (via [`is_established_checkout`]) so a healthy base is
/// never flagged.
/// Test: `ensure_base_checkout_rejects_stale_directory`,
/// `fake_ensure_base_checkout_rejects_stale_directory` (both assert
/// the message names the path and carries the quarantine hint), and
/// `stale_base_dir_error_suggests_quarantine_not_deletion` (asserts the message
/// contains NO destructive command — the #3605 regression guard).
pub(super) fn stale_base_dir_error(base_dir: &Path) -> Option<ProvisionError> {
    // A missing dir (NotFound) or an empty dir (no entries) is fine: `git clone`
    // clones cleanly into either. Only a NON-empty dir that is not a
    // valid checkout is the stale/broken state worth reporting. Any OTHER
    // read_dir error (e.g. permission-denied) must NOT be swallowed as "absent":
    // treating an unreadable directory as non-empty forces an actionable error
    // here instead of letting `git clone` fail later with an opaque
    // message — exactly the failure mode this helper exists to prevent.
    let non_empty = match std::fs::read_dir(base_dir) {
        Ok(mut entries) => entries.next().is_some(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    };
    if !non_empty {
        return None;
    }
    // #4270: a directory holding git or trusty-mpm state is occupied, but
    // nothing about it is stale — and the quarantine hint below must never be
    // aimed at it. Only foreign debris reaches the `mv` suggestion.
    if let Some(err) = protected_dir_error(base_dir) {
        return Some(err);
    }
    Some(ProvisionError::Git(format!(
        "base checkout path {path} exists but is not a valid git checkout \
         (likely a leftover from a crashed clone, or a stale directory).\n\
         \n\
         WARNING: this path is the SHARED git base for EVERY trusty-mpm worktree of \
         this project, and other sessions may be using it right now. Destroying it \
         orphans every existing worktree, which then fails with phantom \
         git-discovery errors (issue #3605). Do NOT recursively delete it.\n\
         \n\
         trusty-mpm will not modify it automatically. Recover by QUARANTINING it — \
         move it aside, never delete it — then retry provisioning:\n\
         \x20   mv {path} {quarantine}\n\
         \n\
         The quarantined copy stays intact so an orphaned worktree can be repointed \
         at it, or so it can be inspected before anyone decides to discard it.",
        path = base_dir.display(),
        quarantine = stale_base_quarantine_path(base_dir).display()
    )))
}

/// Determine whether `dir` is an already-established (fake) checkout,
/// mirroring [`is_established_checkout`]'s intent for `FakeGitBackend`.
///
/// Why: `FakeGitBackend::ensure_base_checkout` used to treat a lone
/// `dir.join("HEAD").is_file()` as proof of an established base — the exact
/// superficial file-existence probe the real backend abandoned in favour of
/// asking git (issue #1937 item 3). A future test author using the fake to
/// simulate a stale base (a stray `HEAD` file with no repository structure)
/// would have gotten a false-positive "already established" pass, hiding the
/// very stale-directory bug a real-backend test would catch. This check
/// restores that fidelity in the filesystem-light fake.
/// What: returns `true` only when the `.git/config` file
/// [`write_fake_checkout`] writes exists — the same marker
/// `FakeGitBackend::is_git_repo` and `clone_repo` already use, and the
/// filesystem-light stand-in for the real backend's working-tree probe. A
/// directory containing only a stray `HEAD` (the stale-mid-clone shape) reads
/// as NOT established, matching the real backend.
/// Test: `fake_ensure_base_checkout_rejects_stale_directory`,
/// `provision_reuses_base_checkout_across_sessions`.
pub(super) fn fake_is_established_checkout(dir: &Path) -> bool {
    dir.join(".git").join("config").is_file()
}

/// Write the minimal structural markers that make a directory read as an
/// established (fake) checkout.
///
/// Why: `FakeGitBackend::ensure_base_checkout` must leave behind enough state
/// that [`fake_is_established_checkout`] recognises it on the next call
/// (idempotent reuse across sessions) while still being distinguishable from a
/// stale directory that merely holds a stray `HEAD` file (issue #1937 item 3).
/// What: creates `dir/.git` (and parents) and writes a `config` naming
/// `repo_url` as `origin`, matching what `FakeGitBackend::clone_repo` produces
/// so `is_git_repo`/`remote_url` answer consistently for a faked base clone.
/// Returns any I/O error as [`ProvisionError::Io`].
/// Test: `provision_reuses_base_checkout_across_sessions` (second provision is a
/// no-op reuse), `fake_ensure_base_checkout_rejects_stale_directory`.
pub(super) fn write_fake_checkout(dir: &Path, repo_url: &str) -> Result<(), ProvisionError> {
    let git_dir = dir.join(".git");
    std::fs::create_dir_all(&git_dir)?;
    std::fs::write(
        git_dir.join("config"),
        format!("[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = {repo_url}\n"),
    )?;
    Ok(())
}

/// Maximum time a caller will wait to acquire the base-checkout lock before
/// giving up.
///
/// Why: a real clone of a large repository can legitimately take a while;
/// this must be generous enough not to spuriously fail a normal (non-racing)
/// first provision on a slow network, while still bounding how long a
/// session-spawn request can block.
/// What: 60 seconds.
/// Test: `ensure_base_checkout_recovers_from_concurrent_race` completes well
/// within this window against a local, near-instant clone.
pub(super) const LOCK_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Interval between retries while polling for the base-checkout lock.
///
/// Why: short enough that a waiting caller notices the lock's release
/// quickly, long enough not to busy-spin the CPU.
/// What: 50 milliseconds.
/// Test: covered indirectly by every test that exercises the lock.
const LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Age after which an existing lock marker file is treated as abandoned.
///
/// Why: the lock marker is a plain file, not a kernel-tracked `flock` — if
/// the process holding it crashes (killed, panicked, power loss) mid-clone,
/// the marker file is never cleaned up by a `Drop` impl that never ran. A
/// purely time-bound marker would otherwise deadlock every future
/// provisioning attempt for that project forever.
/// What: 5 minutes — comfortably longer than [`LOCK_ACQUIRE_TIMEOUT`] so a
/// live, slow-but-progressing clone is never mistaken for an abandoned one.
/// Test: `base_checkout_lock_recovers_stale_lock_marker`.
pub(super) const LOCK_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(300);

/// Compute the sibling marker-file path used to serialize concurrent
/// `ensure_base_checkout` calls for the same `base_dir`.
///
/// Why: the lock must live NEXT TO `base_dir` (not inside it), since
/// `base_dir` itself may not exist yet when the lock is first acquired.
/// What: `<base_dir's parent>/<base_dir's file name>.lock`, e.g.
/// `<repos_root>/<owner>/<repo>.lock` for a `<owner>/<repo>` project dir.
/// Test: exercised transitively by every `ensure_base_checkout` test.
pub(super) fn base_checkout_lock_path(base_dir: &Path) -> PathBuf {
    match (base_dir.parent(), base_dir.file_name()) {
        (Some(parent), Some(name)) => {
            let mut lock_name = name.to_os_string();
            lock_name.push(".lock");
            parent.join(lock_name)
        }
        _ => base_dir.with_extension("lock"),
    }
}

/// RAII guard holding the base-checkout provisioning lock.
///
/// Why: guarantees the marker file is removed when `ensure_base_checkout`
/// returns via ANY path (success, error, or early return) — a manual
/// remove-on-every-branch approach is error-prone and easy to miss on a new
/// return path added later.
/// What: wraps the locked path; `Drop` best-effort removes the marker file
/// (a failure to remove is not actionable here and must not panic in a
/// destructor).
/// Test: `ensure_base_checkout_recovers_from_concurrent_race` (the lock is
/// released promptly enough for every racing thread to eventually proceed).
pub(super) struct BaseCheckoutLock {
    path: PathBuf,
}

impl Drop for BaseCheckoutLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Acquire the advisory base-checkout provisioning lock at `lock_path`.
///
/// Why: resolves the TOCTOU race (#1935 review finding #1) at its root by
/// serializing the whole check-and-clone window, rather than trying to
/// detect and recover from corruption after the fact — a real `git clone`
/// is not safe against a concurrent writer into the same target
/// directory (empirically observed failure mode without this lock: two
/// interleaved clones can corrupt each other's partial checkout, e.g.
/// colliding on copying `hooks/commit-msg.sample`, rather than cleanly
/// failing with "already exists").
/// What: retries `OpenOptions::new().write(true).create_new(true)` — atomic
/// at the filesystem level, failing with `AlreadyExists` iff another caller
/// currently holds the lock — for up to [`LOCK_ACQUIRE_TIMEOUT`], sleeping
/// [`LOCK_POLL_INTERVAL`] between attempts. A lock file older than
/// [`LOCK_STALE_AFTER`] is treated as abandoned (left behind by a crashed
/// holder) and force-removed before retrying, so a crash mid-clone can never
/// permanently deadlock future provisioning attempts for the same project.
/// Returns a [`BaseCheckoutLock`] guard that releases the lock on drop.
/// Test: `ensure_base_checkout_recovers_from_concurrent_race`,
/// `base_checkout_lock_recovers_stale_lock_marker`.
pub(super) fn acquire_base_checkout_lock(
    lock_path: &Path,
) -> Result<BaseCheckoutLock, ProvisionError> {
    let deadline = std::time::Instant::now() + LOCK_ACQUIRE_TIMEOUT;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_) => {
                return Ok(BaseCheckoutLock {
                    path: lock_path.to_owned(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // KNOWN, ACCEPTED RACE (issue #1937 item 2): staleness is judged
                // purely by the marker file's mtime crossing `LOCK_STALE_AFTER`.
                // On a filesystem with low-resolution timestamps, or under
                // significant wall-clock skew (e.g. NTP step, VM clock drift),
                // it is theoretically possible for this caller to compute a
                // still-live holder's marker as "stale" and force-remove it,
                // after which BOTH callers could hold the lock at once. We
                // accept this window deliberately: this is an ADVISORY lock, not
                // a correctness-critical mutex — the only thing it guards is a
                // rare first-provision `git clone` collision (finding #1),
                // and `LOCK_STALE_AFTER` (5 min) is set comfortably longer than
                // `LOCK_ACQUIRE_TIMEOUT` (60 s) precisely so a live,
                // slow-but-progressing clone is never mistaken for an abandoned
                // one under normal conditions. A dependency-free, kernel-tracked
                // alternative (e.g. `flock`) or a PID-liveness check would close
                // the window, but the added complexity is not justified for this
                // low-probability, non-destructive edge case (worst outcome: the
                // git-clone collision this lock exists to prevent, which was
                // already tolerable before the lock landed).
                if lock_is_stale(lock_path) {
                    // Best-effort: if the remove races with the true holder's
                    // own release, the next loop iteration's create_new simply
                    // retries — no harm done either way.
                    let _ = std::fs::remove_file(lock_path);
                    continue;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(ProvisionError::Git(format!(
                        "timed out after {:?} waiting for base-checkout lock at {}",
                        LOCK_ACQUIRE_TIMEOUT,
                        lock_path.display()
                    )));
                }
                std::thread::sleep(LOCK_POLL_INTERVAL);
            }
            Err(e) => return Err(ProvisionError::Io(e)),
        }
    }
}

/// Determine whether the lock marker at `lock_path` is old enough to be
/// treated as abandoned by a crashed holder.
///
/// Why: split out of [`acquire_base_checkout_lock`] so the staleness policy
/// is independently testable and readable in isolation.
/// What: returns `true` iff the file's modified time is more than
/// [`LOCK_STALE_AFTER`] in the past. Returns `false` (never panics) if the
/// file vanished, its metadata can't be read, or the clock query fails —
/// treating an unreadable lock as NOT stale is the conservative, safe default
/// (it just means this caller waits and retries rather than force-clearing
/// state it can't reason about).
/// Test: `base_checkout_lock_recovers_stale_lock_marker`.
pub(super) fn lock_is_stale(lock_path: &Path) -> bool {
    std::fs::metadata(lock_path)
        .and_then(|meta| meta.modified())
        .and_then(|modified| {
            modified
                .elapsed()
                .map_err(|e| std::io::Error::other(e.to_string()))
        })
        .map(|age| age > LOCK_STALE_AFTER)
        .unwrap_or(false)
}
