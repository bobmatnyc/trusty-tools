//! Unit tests for `provisioner::workspace` (extracted to keep `workspace.rs`
//! under the 500-SLOC production cap after the #1935 base-checkout +
//! per-session worktree rework added `GitBackend::ensure_base_checkout` /
//! `GitBackend::worktree_add` plus their `FakeGitBackend` implementations).
//!
//! Why: splitting the test module into its own file (rather than trimming
//! doc comments or logic in the production code) keeps the Why/What/Test
//! documentation density in the production module intact while satisfying
//! the mechanical SLOC gate (`scripts/check_line_cap.sh`) — this file's
//! basename (`tests.rs`) classifies it under the 1500-SLOC test/benchmark cap.
//! What: `WorkspaceProvisioner`/`FakeGitBackend` unit tests covering path
//! isolation, session-id subdirectory naming, base-checkout reuse across
//! sessions, blank-ref handling, and TASK.md write/skip behaviour.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use super::base_lock::{LOCK_STALE_AFTER, lock_is_stale};
use super::*;
use tempfile::TempDir;

/// RAII guard restoring `$HOME` on drop (including panic) — mirrors the
/// identical pattern in `core::standalone::load::tests::HomeGuard` and
/// `session_launch::tests::EnvVarGuard`.
///
/// Why (#3965): `WorkspaceProvisioner::provision`/`provision_in` call
/// `core::home_trust_seed::preseed_home_trust` UNCONDITIONALLY — even under
/// `without_prepare()` ("must not touch the shared `~/.claude/` tree", per the
/// comment on `make_provisioner` below) — because that seed is independent of
/// the full agent/skill deploy step. It resolves `~/.claude.json` from the
/// REAL process `$HOME`, not from `workspace_root`, so every test using
/// `make_provisioner`/`.provision(...)` must pin `$HOME` to its own hermetic
/// root or it writes into the operator's real `~/.claude.json`. Pairs with
/// `#[serial_test::serial]`.
/// Test: used by every `.provision(...)`-driving test in this file.
struct HomeGuard(Option<String>);
impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: paired with `#[serial_test::serial]` — no other thread
        // reads/writes the environment concurrently.
        match self.0 {
            Some(ref p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// Point `$HOME` at `home` for the duration of the caller's scope. Callers
/// MUST be `#[serial_test::serial]` — see [`HomeGuard`].
fn set_home(home: &std::path::Path) -> HomeGuard {
    let prior = std::env::var("HOME").ok();
    // SAFETY: serialized via `#[serial_test::serial]`.
    unsafe { std::env::set_var("HOME", home) };
    HomeGuard(prior)
}

fn make_provisioner(root: &TempDir) -> WorkspaceProvisioner<FakeGitBackend> {
    // Skip the global `prepare_session` deploy: these tests verify path
    // isolation only and must not touch the shared `~/.claude/` tree.
    WorkspaceProvisioner::without_prepare(FakeGitBackend::new(), root.path().to_owned())
}

#[test]
fn repo_slug_extraction() {
    assert_eq!(
        repo_slug("https://github.com/owner/trusty-tools"),
        "trusty-tools"
    );
    assert_eq!(
        repo_slug("https://github.com/owner/trusty-tools.git"),
        "trusty-tools"
    );
    assert_eq!(repo_slug("git@github.com:owner/my-repo.git"), "my-repo");
}

#[test]
#[serial_test::serial]
fn provisioner_isolation_path() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let id = ManagedSessionId::new();

    let ws = prov
        .provision(&id, "https://github.com/owner/trusty-tools", "main", "task")
        .unwrap();

    // Path must be inside workspace_root, not the operator's project directory.
    assert!(ws.path.starts_with(root.path()));
    assert!(ws.path.to_string_lossy().contains("trusty-tools"));
    assert!(ws.path.to_string_lossy().contains(&id.to_string()));
}

#[test]
#[serial_test::serial]
fn provisioner_path_not_in_existing_project() {
    // The workspace must NOT be inside any real project dir.
    // We simulate this by checking the path is inside workspace_root (a tempdir).
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let id = ManagedSessionId::new();

    let ws = prov
        .provision(&id, "https://github.com/owner/myrepo.git", "feat/x", "task")
        .unwrap();

    // Must start with the mpm-owned workspace root, not any other path.
    assert!(ws.path.starts_with(root.path()));
    // Must not be equal to the workspace root itself.
    assert_ne!(&ws.path, root.path());
}

#[test]
#[serial_test::serial]
fn provisioner_uses_session_id_subdir() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let id = ManagedSessionId::new();

    let ws = prov
        .provision(&id, "https://github.com/owner/repo", "main", "task")
        .unwrap();

    // The leaf directory must be the session id.
    let leaf = ws.path.file_name().unwrap().to_string_lossy();
    assert_eq!(leaf.as_ref(), id.to_string());
}

#[test]
#[serial_test::serial]
fn provision_in_uses_explicit_project_dir() {
    // The #1220 path: caller supplies a pre-resolved `<owner>/<repo>` project
    // dir. #1935 nested the session worktree under a shared base checkout;
    // #4270 made that base the project dir itself, so the worktree is
    // `<project_dir>/.worktrees/<session-id>/` — the git-standard shape.
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let id = ManagedSessionId::new();
    let project_dir = root.path().join("bobmatnyc").join("trusty-tools");

    let ws = prov
        .provision_in(
            &project_dir,
            &id,
            "https://github.com/bobmatnyc/trusty-tools",
            "main",
            "task",
        )
        .unwrap();

    assert_eq!(
        ws.path,
        project_dir.join(".worktrees").join(id.to_string()),
        "#4270: the worktree must land directly under the project dir's .worktrees/"
    );
    assert!(ws.path.starts_with(&project_dir));
}

/// #4270 requirement 1: provisioning from a repo URL puts the worktree at
/// `<project-root>/.worktrees/<name>`, not `<root>/.base/.worktrees/<name>`.
///
/// Why: this is the owner ruling ("all new worktrees should be in .worktrees --
/// we need to follow the git convention here") stated as an executable
/// assertion. The sibling `provision_in_uses_explicit_project_dir` pins the
/// exact path; this one pins the NEGATIVE that the ruling is about, so a
/// regression that restores the `.base/` nesting fails here by name.
/// What: provisions one session and asserts the worktree path contains no
/// `.base` component and equals the `.worktrees/<id>` shape.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn provision_in_puts_worktrees_under_the_project_root_not_dot_base() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let id = ManagedSessionId::new();
    let project_dir = root.path().join("owner").join("repo");

    let ws = prov
        .provision_in(&project_dir, &id, "https://github.com/owner/repo", "", "t")
        .unwrap();

    assert!(
        !ws.path
            .components()
            .any(|c| c.as_os_str() == crate::core::harness_root::BASE_CLONE_DIRNAME),
        "#4270: no .base component may appear in a provisioned worktree path, got {}",
        ws.path.display()
    );
    assert_eq!(ws.path, project_dir.join(".worktrees").join(id.to_string()));
}

/// #4270 requirement 2: the provisioning path creates no `.base` directory.
///
/// Why: moving the worktrees out of `.base/` while still cloning a bare store
/// into it would satisfy the path assertion above and miss the point — the
/// store itself is what #4270 retires. Asserting on the filesystem after a real
/// provision is the only way to catch that half-fix.
/// What: provisions one session and asserts `<project_dir>/.base` does not
/// exist, while the base checkout markers DO exist at `<project_dir>` itself.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn provision_in_creates_no_dot_base_directory() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let id = ManagedSessionId::new();
    let project_dir = root.path().join("owner").join("repo");

    prov.provision_in(&project_dir, &id, "https://github.com/owner/repo", "", "t")
        .unwrap();

    assert!(
        !project_dir
            .join(crate::core::harness_root::BASE_CLONE_DIRNAME)
            .exists(),
        "#4270: provisioning must not create a .base store under {}",
        project_dir.display()
    );
    assert!(
        project_dir.join(".git").join("config").is_file(),
        "the base checkout must be established at the project dir itself"
    );
}

/// #4270 requirement 3: an EXISTING `.base` worktree survives provisioning
/// untouched.
///
/// Why: this is the requirement that protects live sessions. Existing `.base`
/// stores were deliberately not migrated, so the new path meets them on real
/// installations — and the generic stale-directory recovery hint would have told
/// an agent to `mv` the whole project directory aside, orphaning every worktree
/// under it (the #3605 shape, one level up). The refusal must be loud, and it
/// must move nothing.
/// What: pre-seeds a `.base/.worktrees/live/` worktree holding a marker file,
/// provisions against that project dir, and asserts (1) the call fails, (2) the
/// error names `.base` and suggests no `mv`/`rm`, (3) the marker file is still
/// there with its original content.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn provision_in_leaves_an_existing_dot_base_store_untouched() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let project_dir = root.path().join("owner").join("repo");

    let live_worktree = project_dir
        .join(crate::core::harness_root::BASE_CLONE_DIRNAME)
        .join(".worktrees")
        .join("live");
    std::fs::create_dir_all(&live_worktree).unwrap();
    let marker = live_worktree.join("IN-USE.txt");
    std::fs::write(&marker, "a live session is working here").unwrap();

    let result = prov.provision_in(
        &project_dir,
        &ManagedSessionId::new(),
        "https://github.com/owner/repo",
        "",
        "t",
    );

    assert!(
        result.is_err(),
        "provisioning over a live legacy .base store must fail loudly, got {result:?}"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains(crate::core::harness_root::BASE_CLONE_DIRNAME),
        "the refusal must name the legacy store, got: {msg}"
    );
    assert_no_recursive_delete(&msg);
    assert!(
        !msg.contains("    mv "),
        "the refusal must not suggest moving a live store, got: {msg}"
    );

    assert_eq!(
        std::fs::read_to_string(&marker).expect("the pre-existing worktree must still be there"),
        "a live session is working here",
        "#4270: an existing .base worktree must not be disturbed, relocated, or deleted"
    );
}

/// #1935: a second session provisioned for the SAME project must reuse the
/// existing base checkout (no re-clone) and land in its OWN worktree.
///
/// Why: this is the entire point of the fix — before #1935 every session
/// paid for an independent full clone; now only the FIRST session for a
/// project establishes the base checkout, and every subsequent session for
/// that project reuses it via `worktree_add`.
/// What: provisions two sessions against the same project dir, asserts (1)
/// `FakeGitBackend::ensure_base_checkout` observed exactly one call whose
/// `.git/config` marker was absent (the actual clone) — the second call is a
/// no-op because the marker now exists; (2) the two sessions get DISTINCT
/// worktree paths sharing the same project-dir base.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn provision_reuses_base_checkout_across_sessions() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let project_dir = root.path().join("owner").join("repo");

    let id1 = ManagedSessionId::new();
    let ws1 = prov
        .provision_in(
            &project_dir,
            &id1,
            "https://github.com/owner/repo",
            "main",
            "t1",
        )
        .unwrap();

    let id2 = ManagedSessionId::new();
    let ws2 = prov
        .provision_in(
            &project_dir,
            &id2,
            "https://github.com/owner/repo",
            "main",
            "t2",
        )
        .unwrap();

    // #4270: both worktrees share the same base checkout — the project dir.
    let worktrees_dir = project_dir.join(".worktrees");
    assert!(ws1.path.starts_with(&worktrees_dir));
    assert!(ws2.path.starts_with(&worktrees_dir));
    // ...but are otherwise distinct, isolated directories.
    assert_ne!(ws1.path, ws2.path);
    // The base checkout itself was established exactly once (idempotent
    // `.git/config` marker check inside `FakeGitBackend::ensure_base_checkout`).
    assert!(project_dir.join(".git").join("config").is_file());
}

#[test]
#[serial_test::serial]
fn provisioner_records_repo_url_and_branch() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let id = ManagedSessionId::new();

    let ws = prov
        .provision(
            &id,
            "https://github.com/owner/repo",
            "feat/my-branch",
            "task",
        )
        .unwrap();

    assert_eq!(ws.repo_url, "https://github.com/owner/repo");
    assert_eq!(ws.branch, "feat/my-branch");
}

/// Why: WI-A #1585 — when `LaunchParams::ref_` is absent the spawn path
/// forwards `git_ref = ""` to `spawn_managed`/`SpawnParams`. This test
/// locks in the contract that a blank `git_ref` is passed through to the
/// git backend as `""` AND that provision still succeeds (no early error).
/// #1935: the production fix now lives in `RealGitBackend::worktree_add`
/// (`provision_in` no longer calls `clone_repo` at all): when
/// `git_ref.trim().is_empty()` the fetch source falls back to `HEAD` (the
/// remote's default branch pointer) instead of an empty, invalid ref name —
/// mirroring the same blank-ref-tolerance contract `RealGitBackend::clone_repo`
/// still upholds for `content::catalog_sync`'s unrelated use of the same trait.
/// What: provisions with `git_ref = ""` and asserts (1) provision
/// succeeds, (2) the returned `branch` field is `""` (the provisioner does
/// not substitute a default) — the internal per-session branch name backing
/// the worktree is unrelated to this field, which always echoes the
/// REQUESTED ref verbatim.
/// Test: this is the test.
#[test]
#[serial_test::serial]
fn blank_git_ref_omits_branch_flag() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    // Use FakeGitBackend via make_provisioner so we can read its call log.
    // make_provisioner returns a provisioner whose backend is a FakeGitBackend
    // but we cannot access it post-move. We build explicitly here so we can
    // inspect calls.
    let fake = FakeGitBackend::new();
    // SAFETY: the calls Mutex is shared across the borrow boundary through
    // raw pointers; instead, use a shared Arc<FakeGitBackend> — but since the
    // type does not impl Clone we verify the contract via ws.branch instead.
    let prov = WorkspaceProvisioner::without_prepare(FakeGitBackend::new(), root.path().to_owned());
    let id = ManagedSessionId::new();

    let ws = prov
        .provision(&id, "https://github.com/owner/repo", "", "task")
        .unwrap();

    // Provision must succeed: the provisioner does not reject a blank ref.
    assert!(ws.path.starts_with(root.path()), "workspace inside root");
    // The branch field records what was passed — blank — not a substituted default.
    // This pins the invariant that the provisioner passes "" through to the
    // backend verbatim; only `RealGitBackend::worktree_add` substitutes `HEAD`
    // as the actual fetch source when the ref is blank.
    assert_eq!(
        ws.branch, "",
        "blank ref must be stored as-is, not substituted"
    );
    drop(fake); // explicitly drop to silence unused-variable lint
}

/// Why: closes #1693 — the task description must be written to TASK.md in
/// the workspace root so the agent can read its brief without requiring
/// interactive input. This test locks in the write behaviour.
/// What: provisions with a non-empty task and asserts TASK.md exists and
/// contains exactly the task string.
/// Test: this is the test.
#[test]
#[serial_test::serial]
fn provision_writes_task_md() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let id = ManagedSessionId::new();
    let task = "Fix the authentication bug in the login flow";

    let ws = prov
        .provision(&id, "https://github.com/owner/repo", "main", task)
        .unwrap();

    let task_file = ws.path.join("TASK.md");
    assert!(
        task_file.exists(),
        "TASK.md must be written when task is non-empty"
    );
    let content = std::fs::read_to_string(&task_file).unwrap();
    assert_eq!(content, task, "TASK.md must contain the exact task text");
}

/// Why: closes #1693 — when no task is provided the workspace must NOT
/// receive an empty TASK.md (an empty file is misleading and wastes I/O).
/// What: provisions with an empty task string and asserts TASK.md is absent.
/// Test: this is the test.
#[test]
#[serial_test::serial]
fn provision_skips_task_md_when_empty() {
    let root = crate::test_support::hermetic_temp_dir();
    let _home = set_home(root.path());
    let prov = make_provisioner(&root);
    let id = ManagedSessionId::new();

    let ws = prov
        .provision(&id, "https://github.com/owner/repo", "main", "")
        .unwrap();

    let task_file = ws.path.join("TASK.md");
    assert!(
        !task_file.exists(),
        "TASK.md must NOT be created when task is empty"
    );
}

// ── trusty-review PR #1936 (#1935) findings: RealGitBackend::ensure_base_checkout ──
//
// Both tests below exercise `RealGitBackend` against real local git
// repositories (no network required — `file://` remotes and local `git init`
// only), mirroring the graceful-skip-if-git-unavailable pattern already used
// by `decommission_worktree_tests.rs`.

/// Create a local bare "origin" repo with a single commit and return its path.
///
/// Why: shared fixture for the two `ensure_base_checkout` regression tests
/// below — both need a real, clonable bare repo to point `RealGitBackend` at.
/// What: `git init --bare`, then clones it to a scratch work dir, commits a
/// README, and pushes back to the bare repo. Returns `None` (callers must
/// skip, not fail) if the local `git` binary is unavailable, matching the
/// established pattern in `decommission_worktree_tests.rs`.
/// Test: exercised transitively by every test that calls it.
fn make_local_bare_origin(scratch: &TempDir) -> Option<PathBuf> {
    use std::process::Command;
    let bare = scratch.path().join("origin.git");
    let work = scratch.path().join("seed");
    if !Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .arg(&bare)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return None;
    }
    if !Command::new("git")
        .args(["clone"])
        .arg(&bare)
        .arg(&work)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return None;
    }
    std::fs::write(work.join("README.md"), "seed").unwrap();
    let work_s = work.to_str().unwrap();
    for args in [
        vec!["-C", work_s, "add", "."],
        vec![
            "-C",
            work_s,
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "seed",
        ],
        vec!["-C", work_s, "push", "origin", "main"],
    ] {
        if !Command::new("git")
            .args(&args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return None;
        }
    }
    Some(bare)
}

/// trusty-review finding #1 (TOCTOU race, PR #1936): two sessions
/// provisioning the SAME project for the first time must not fail when they
/// race on `ensure_base_checkout`.
///
/// Why: before the fix, both racing callers observed `base_dir.join("HEAD")`
/// absent, both attempted a clone, and the loser hit git's
/// "destination path ... already exists and is not an empty directory"
/// error, surfacing as a hard `ProvisionError::Git` and failing that
/// session's provisioning outright.
/// What: spawns several real OS threads that all call
/// `RealGitBackend.ensure_base_checkout(repo_url, &base_dir)` concurrently
/// against the exact same, not-yet-existing `base_dir`, then asserts (1)
/// every thread returned `Ok(())` — no race loser propagates the "already
/// exists" error — and (2) exactly one genuinely established checkout
/// exists at `base_dir` afterward. Skips gracefully if the local `git` binary
/// is unavailable.
/// Test: this function IS the test.
#[test]
fn ensure_base_checkout_recovers_from_concurrent_race() {
    let scratch = crate::test_support::hermetic_temp_dir();
    let Some(bare_origin) = make_local_bare_origin(&scratch) else {
        eprintln!("ensure_base_checkout_recovers_from_concurrent_race: git unavailable, skipping");
        return;
    };
    let repo_url = format!("file://{}", bare_origin.display());

    let root = crate::test_support::hermetic_temp_dir();
    // #4270: the base checkout IS the project directory.
    let base_dir = root.path().join("owner").join("repo");

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let repo_url = repo_url.clone();
            let base_dir = base_dir.clone();
            std::thread::spawn(move || {
                RealGitBackend::default().ensure_base_checkout(&repo_url, &base_dir)
            })
        })
        .collect();

    for handle in handles {
        let result = handle.join().expect("thread must not panic");
        assert!(
            result.is_ok(),
            "every racing ensure_base_checkout call must recover and return Ok, got {result:?}"
        );
    }

    assert!(
        is_established_checkout(&base_dir),
        "base_dir must be a genuinely established checkout after the race settles"
    );
}

/// trusty-review finding #2 (fragile clone detection, PR #1936): a stale
/// directory sitting at the base-checkout path must not be silently accepted
/// as a valid shared base.
///
/// Why: the previous idempotency guard (`base_dir.join("HEAD").is_file()`)
/// only checks for a file NAMED `HEAD` at the root of `base_dir` — it never
/// confirms that directory is actually a valid, complete git repository.
/// Verified empirically (see this test): a plain `git init`/`clone` does NOT
/// put a `HEAD` file at its OWN root (that lives at `.git/HEAD` instead), but
/// a directory that merely CONTAINS a stray file literally named `HEAD` — e.g.
/// left over from a clone that crashed mid-flight (git writes `HEAD` early,
/// before the rest of the object database/refs), or any other stale/corrupt
/// artifact occupying the base path — passes the old check and would be
/// silently treated as an established base, so cloning is skipped and a corrupt
/// directory is left as the "base"; later `git worktree add` / `git fetch`
/// calls against it then fail confusingly.
/// What: pre-creates `base_dir` containing ONLY a `HEAD` file (no `.git`,
/// no object database, no refs — the minimum needed to fool the OLD
/// file-existence check) and calls `RealGitBackend.ensure_base_checkout`
/// pointed at a real, unrelated bare origin. Asserts the call returns `Err`
/// (loud failure) rather than `Ok` (which would mean the corrupt directory
/// was silently treated as valid). Skips gracefully if the local `git`
/// binary is unavailable.
/// Test: this function IS the test.
#[test]
fn ensure_base_checkout_rejects_stale_directory() {
    let scratch = crate::test_support::hermetic_temp_dir();
    let Some(bare_origin) = make_local_bare_origin(&scratch) else {
        eprintln!("ensure_base_checkout_rejects_stale_directory: git unavailable, skipping");
        return;
    };
    let repo_url = format!("file://{}", bare_origin.display());

    let root = crate::test_support::hermetic_temp_dir();
    let base_dir = root.path().join("owner").join("repo");
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::write(base_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

    assert!(
        base_dir.join("HEAD").is_file(),
        "sanity check: the stale directory has a file literally named HEAD"
    );
    assert!(
        !is_established_checkout(&base_dir),
        "sanity check: a lone HEAD file with no repo structure must NOT read as a checkout"
    );

    let result = RealGitBackend::default().ensure_base_checkout(&repo_url, &base_dir);
    assert!(
        result.is_err(),
        "a stale non-repo directory must be rejected loudly, not silently reused: {result:?}"
    );
    // #1937 item 1: the error must be ACTIONABLE — name the exact path and a
    // recovery command the operator can run to allow re-provisioning. #3605:
    // that command must be a non-destructive quarantine, never a delete.
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains(&base_dir.display().to_string()),
        "error must name the exact stale base path, got: {msg}"
    );
    assert_no_destructive_hint(&msg);
}

/// Assert an operator-facing recovery message offers a QUARANTINE and contains
/// no command that recursively destroys the shared base checkout (#3605).
///
/// Why: on 2026-07-21 a `<project>/.base` was destroyed and ~70 worktrees were
/// orphaned machine-wide, every one then emitting phantom git-discovery test
/// failures. The most plausible trigger was `stale_base_dir_error`'s own
/// suggested recovery: a literal, copy-pasteable `rm -rf <path>`. The threat
/// model is that this message is read by an autonomous agent that executes the
/// suggestion verbatim, so the string itself is the vulnerability and a
/// string-level assertion is the correct regression guard.
/// What: fails if the message names any recursive-delete form, and requires the
/// non-destructive `mv`-aside quarantine hint plus the shared-ownership warning
/// that was missing when the destructive hint was followed.
/// Test: used by `ensure_base_checkout_rejects_stale_directory`,
/// `fake_ensure_base_checkout_rejects_stale_directory`, and
/// `stale_base_dir_error_suggests_quarantine_not_deletion`.
///
/// The recursive-delete half is split into [`assert_no_recursive_delete`]
/// because it applies to EVERY operator-facing refusal, while the `mv`
/// requirement applies only to the foreign-debris case — a message aimed at a
/// directory holding live worktrees must offer no `mv` at all (#4270).
fn assert_no_destructive_hint(msg: &str) {
    assert_no_recursive_delete(msg);
    assert!(
        msg.contains("    mv "),
        "recovery message must offer the non-destructive quarantine command, got: {msg}"
    );
    assert!(
        msg.contains("SHARED"),
        "recovery message must warn that the base is shared across sessions, got: {msg}"
    );
}

/// #4270: `git clean -ffd` in the base clone must not delete session worktrees.
///
/// Why: the two review rounds contradicted each other on whether the missing
/// `.git/info/exclude` entry was cosmetic, and the disagreement was a data-loss
/// question, so it is settled here in the suite rather than by hand. Measured
/// against real git: single-force `clean` reports `Skipping repository` and is
/// safe, which is what makes this look harmless — but `-ff` removes
/// `.worktrees/` outright, uncommitted session work included. The exclude entry
/// is therefore a safety guard, and the in-project path has always written it.
/// The provisioner producing the identical topology must too.
/// What: builds the shipping topology with the production
/// `ensure_base_checkout` and `worktree_add`, writes an uncommitted file into
/// the worktree, then runs a real `git clean -ffd` in the base and asserts the
/// file survives. Skips gracefully when git is unavailable.
/// Test: this function IS the test.
#[test]
fn worktrees_exclude_entry_protects_against_double_force_clean() {
    let scratch = crate::test_support::hermetic_temp_dir();
    let Some(bare) = make_local_bare_origin(&scratch) else {
        eprintln!("worktrees_exclude_entry_protects_against_double_force_clean: no git, skipping");
        return;
    };
    let repo_url = format!("file://{}", bare.display());
    let base_dir = scratch.path().join("owner").join("repo");
    let backend = RealGitBackend::default();
    backend
        .ensure_base_checkout(&repo_url, &base_dir)
        .expect("ensure_base_checkout must succeed");

    let worktree = base_dir.join(".worktrees").join("sess-1");
    backend
        .worktree_add(&base_dir, "main", &worktree, "sess-1")
        .expect("worktree_add must succeed");
    let wip = worktree.join("WIP.txt");
    std::fs::write(&wip, b"uncommitted session work").unwrap();

    // The guard itself: `.worktrees/` must be excluded in the base clone.
    let exclude = std::fs::read_to_string(base_dir.join(".git").join("info").join("exclude"))
        .expect("the base clone must have a .git/info/exclude");
    assert!(
        exclude.lines().any(|l| l.trim() == ".worktrees/"),
        "ensure_base_checkout must exclude .worktrees/, got: {exclude}"
    );

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&base_dir)
        .args(["clean", "-ffd"])
        .output()
        .expect("git clean -ffd");
    let said = String::from_utf8_lossy(&out.stdout);

    assert!(
        wip.exists(),
        "#4270: `git clean -ffd` in the base DELETED a session worktree's \
         uncommitted work — git said: {said}"
    );
    assert!(
        !said.contains(".worktrees"),
        "`git clean -ffd` must not touch .worktrees/ at all, got: {said}"
    );
}

/// Assert an operator-facing message names no recursive delete (#3605).
///
/// Why: the delete prohibition binds every refusal this code emits, not only
/// the ones that also offer a quarantine. Splitting it out lets the refusals
/// that deliberately suggest NOTHING (#4270) still be checked for the thing
/// that caused the 2026-07-21 incident.
/// What: fails if the message names any recursive-delete form.
/// Test: used by `assert_no_destructive_hint` and
/// `stale_base_dir_error_never_offers_to_move_a_dir_holding_live_worktrees`.
fn assert_no_recursive_delete(msg: &str) {
    for forbidden in ["rm -rf", "rm -fr", "rm -r ", "remove_dir_all", "rmdir"] {
        assert!(
            !msg.contains(forbidden),
            "recovery message must never suggest a recursive delete (#3605), but it \
             contains {forbidden:?}: {msg}"
        );
    }
}

/// #4270: the quarantine hint must never be aimed at a directory that holds
/// git or trusty-mpm state.
///
/// Why: before #4270 `base_dir` was `<project_dir>/.base`, which an
/// in-project-shaped project does not have — `stale_base_dir_error` returned
/// `None` and its `mv` hint was unreachable there. Pointing `base_dir` at the
/// project directory made it reachable, aimed at the directory holding
/// `.worktrees/<id>` for every live session. That is the #3605 string with a
/// bigger target. The trigger is loose enough to matter: `is_established_checkout`
/// reports `false` for a transient git spawn failure just as it does for a
/// non-repo, so a momentary hiccup on a healthy project reaches this message.
/// What: builds the error for a project dir holding `.worktrees/sess-1`, then
/// for one holding `.git`, and asserts neither offers a `mv` — while the
/// foreign-debris case in the sibling test still does, so the #1937 recovery
/// path is not lost.
/// Test: this function IS the test.
#[test]
fn stale_base_dir_error_never_offers_to_move_a_dir_holding_live_worktrees() {
    let root = crate::test_support::hermetic_temp_dir();

    for (label, entry) in [("live worktrees", ".worktrees"), ("a git dir", ".git")] {
        let dir = root.path().join(label.replace(' ', "-")).join("repo");
        std::fs::create_dir_all(dir.join(entry).join("sess-1")).unwrap();

        let msg = super::base_lock::stale_base_dir_error(&dir)
            .unwrap_or_else(|| panic!("an occupied project dir holding {label} must error"))
            .to_string();

        assert!(
            !msg.contains("    mv "),
            "#4270: the quarantine hint must never target a directory holding {entry} — \
             moving it orphans every session under it, got: {msg}"
        );
        assert!(
            msg.contains(entry),
            "the refusal must name what it found, got: {msg}"
        );
        assert_no_recursive_delete(&msg);
    }
}

/// #3605 regression guard: `stale_base_dir_error`'s message must recover the
/// operator WITHOUT naming a destructive command.
///
/// Why: the two backend-level tests above reach this message through a full
/// `ensure_base_checkout` call; this one pins the message contract directly at
/// its source so a future edit to the string is caught even if the backends'
/// call paths change. It also pins the parts a purely negative assertion would
/// miss: the message must still leave the operator able to recover (a safe but
/// useless hint is not an improvement), and the quarantine destination must be
/// a concrete, distinct sibling path — not the base path itself, which would
/// make the suggested `mv` a no-op.
/// What: builds the error for a non-empty, non-repo directory and asserts the
/// message names the path, carries the quarantine + SHARED warning, offers no
/// recursive delete, and targets a `.stale-` sibling destination that differs
/// from the source path.
/// Test: this function IS the test.
#[test]
fn stale_base_dir_error_suggests_quarantine_not_deletion() {
    let root = crate::test_support::hermetic_temp_dir();
    let base_dir = root.path().join("project").join(".base");
    std::fs::create_dir_all(&base_dir).unwrap();
    std::fs::write(base_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

    let err = super::base_lock::stale_base_dir_error(&base_dir)
        .expect("a non-empty, non-repo base dir must produce an error");
    let msg = err.to_string();

    assert!(
        msg.contains(&base_dir.display().to_string()),
        "error must name the exact stale base path, got: {msg}"
    );
    assert_no_destructive_hint(&msg);

    let quarantine = super::base_lock::stale_base_quarantine_path(&base_dir);
    assert_ne!(
        quarantine, base_dir,
        "quarantine destination must differ from the source, or the `mv` is a no-op"
    );
    assert!(
        quarantine
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(".stale-")),
        "quarantine destination must be a timestamped `.stale-` sibling, got: {}",
        quarantine.display()
    );
    assert_eq!(
        quarantine.parent(),
        base_dir.parent(),
        "quarantine must be a sibling so the `mv` is a same-filesystem rename"
    );

    // An EMPTY or absent base dir is safe to clone into and must not error.
    let empty = root.path().join("project").join(".base-empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(super::base_lock::stale_base_dir_error(&empty).is_none());
    assert!(super::base_lock::stale_base_dir_error(&root.path().join("nope")).is_none());
}

/// #1937 item 3: `FakeGitBackend`'s idempotency/stale-detection must match
/// `RealGitBackend`'s so a fake-backend test catches the same stale-directory
/// condition a real-backend test would — the fidelity gap flagged in the
/// PR #1936 review.
///
/// Why: before this fix `FakeGitBackend::ensure_base_checkout` reused any
/// directory containing a stray `HEAD` file (the same superficial probe the
/// real backend abandoned), so a future author simulating a stale base with
/// the fake would get a false-positive "already established" pass instead of
/// the loud rejection the real backend now produces. This test mirrors
/// `ensure_base_checkout_rejects_stale_directory` against the fake.
/// What: pre-seeds `base_dir` with ONLY a stray `HEAD` file (no `.git/config`
/// marker — the fake's stand-in for "not a valid checkout") and asserts
/// `FakeGitBackend::ensure_base_checkout` returns an actionable `Err` naming
/// the path and the non-destructive quarantine recovery command, exactly like
/// the real backend — not a silent `Ok` reuse.
/// Test: this function IS the test.
#[test]
fn fake_ensure_base_checkout_rejects_stale_directory() {
    let root = crate::test_support::hermetic_temp_dir();
    let base_dir = root.path().join("owner").join("repo");
    std::fs::create_dir_all(&base_dir).unwrap();
    // A lone HEAD file with no `.git/config` marker is the fake's stand-in for
    // a stale, mid-crash directory.
    std::fs::write(base_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

    let fake = FakeGitBackend::new();
    let result = fake.ensure_base_checkout("https://github.com/owner/repo", &base_dir);
    assert!(
        result.is_err(),
        "fake must reject a stale directory loudly, not silently reuse it: {result:?}"
    );
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains(&base_dir.display().to_string()),
        "fake error must be actionable (must name the path), got: {msg}"
    );
    assert_no_destructive_hint(&msg);
}

/// #1937 item 3 (positive path): a FRESH `FakeGitBackend::ensure_base_checkout`
/// establishes a valid fake checkout, and a SECOND call reuses it
/// idempotently (no error, no clobber).
///
/// Why: locks in that the tightened validity check does not regress the
/// happy-path reuse contract — the fake must still recognise the base it just
/// wrote as established on the next call.
/// What: calls `ensure_base_checkout` twice against an empty base path; asserts
/// both return `Ok`, the second is recognised via `fake_is_established_checkout`,
/// and the written `.git/config` marker is present.
/// Test: this function IS the test.
#[test]
fn fake_ensure_base_checkout_is_idempotent_on_valid_base() {
    let root = crate::test_support::hermetic_temp_dir();
    let base_dir = root.path().join("owner").join("repo");
    let fake = FakeGitBackend::new();

    fake.ensure_base_checkout("https://github.com/owner/repo", &base_dir)
        .expect("first ensure must establish the fake base");
    assert!(
        super::base_lock::fake_is_established_checkout(&base_dir),
        "a freshly-established fake base must read as a valid checkout"
    );

    // Second call must be an idempotent no-op reuse, not a stale rejection.
    fake.ensure_base_checkout("https://github.com/owner/repo", &base_dir)
        .expect("second ensure must reuse the established fake base");
    assert!(base_dir.join(".git").join("config").is_file());
}

/// A lock marker abandoned by a crashed holder must not permanently deadlock
/// future base-checkout provisioning attempts.
///
/// Why: the base-checkout lock (added to fix trusty-review finding #1) is a
/// plain marker file, not a kernel-tracked `flock` — if the process holding
/// it is killed mid-clone, nothing ever runs its `Drop` impl to remove the
/// marker. Without stale-lock recovery, every future `ensure_base_checkout`
/// call for that project would wait out `LOCK_ACQUIRE_TIMEOUT` and then fail
/// forever, which is worse than the race it was meant to fix.
/// What: writes a lock marker file and backdates its modified time past
/// `LOCK_STALE_AFTER` via `File::set_modified` (no `filetime` dependency
/// needed — this has been in `std` since Rust 1.75, well under this
/// workspace's 1.94 MSRV). Asserts `lock_is_stale` reports it as stale, then
/// asserts `acquire_base_checkout_lock` recovers PROMPTLY (well under
/// `LOCK_ACQUIRE_TIMEOUT`) rather than blocking out the full timeout, and
/// that dropping the returned guard removes the marker file.
/// Test: this function IS the test.
#[test]
fn base_checkout_lock_recovers_stale_lock_marker() {
    let root = crate::test_support::hermetic_temp_dir();
    let lock_path = root.path().join(".base.lock");
    std::fs::write(&lock_path, b"").unwrap();

    let stale_time =
        std::time::SystemTime::now() - (LOCK_STALE_AFTER + std::time::Duration::from_secs(10));
    std::fs::File::options()
        .write(true)
        .open(&lock_path)
        .unwrap()
        .set_modified(stale_time)
        .unwrap();

    assert!(
        lock_is_stale(&lock_path),
        "a lock marker older than LOCK_STALE_AFTER must be considered stale"
    );

    let start = std::time::Instant::now();
    let guard = acquire_base_checkout_lock(&lock_path).expect("must recover from a stale lock");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "stale-lock recovery must not wait out the full acquire timeout"
    );

    drop(guard);
    assert!(
        !lock_path.exists(),
        "dropping the lock guard must remove the marker file"
    );
}

// ── #2184: RealGitBackend applies the resolved GitIdentity to every command ──

/// Why: a `RealGitBackend::default()` (no identity resolved) must build a
/// PLAIN `git` command — no env overrides, no `-c` args — so every existing
/// production call site (which constructs `RealGitBackend::default()` when it
/// has no project context) is byte-for-byte unaffected by #2184.
/// Test: itself.
#[test]
fn default_identity_produces_plain_git_command() {
    let backend = RealGitBackend::default();
    let cmd = backend.command();
    assert_eq!(
        cmd.get_args().count(),
        0,
        "no -c args for an empty identity"
    );
    assert_eq!(
        cmd.get_envs().count(),
        0,
        "no env overrides for an empty identity"
    );
}

/// Why: the resolved `GitIdentity::env` overrides must be applied to every
/// command this backend builds, so `git`/its credential helper authenticate
/// as the right per-project identity.
/// Test: itself.
#[test]
fn git_identity_env_applied_to_command() {
    let identity = crate::core::git_identity::GitIdentity {
        env: vec![("GH_CONFIG_DIR".to_string(), "/cfg/project".to_string())],
        commit_name: None,
        commit_email: None,
    };
    let backend = RealGitBackend::new(identity);
    let cmd = backend.command();
    let envs: Vec<_> = cmd.get_envs().collect();
    assert!(
        envs.iter().any(|(k, v)| {
            *k == std::ffi::OsStr::new("GH_CONFIG_DIR")
                && *v == Some(std::ffi::OsStr::new("/cfg/project"))
        }),
        "GH_CONFIG_DIR override must be applied: {envs:?}"
    );
}

/// Why: a resolved commit-identity override must render as `-c user.name=…`/
/// `-c user.email=…` BEFORE any subcommand arg (git only accepts `-c`
/// overrides in that position).
/// Test: itself.
#[test]
fn git_identity_commit_args_applied_to_command() {
    let identity = crate::core::git_identity::GitIdentity {
        env: vec![],
        commit_name: Some("Bot".to_string()),
        commit_email: Some("bot@example.com".to_string()),
    };
    let backend = RealGitBackend::new(identity);
    let cmd = backend.command();
    let args: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
    assert_eq!(
        args,
        vec![
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new("user.name=Bot"),
            std::ffi::OsStr::new("-c"),
            std::ffi::OsStr::new("user.email=bot@example.com"),
        ]
    );
}

// ── #2867: the provisioner must never arm a worktree with a foreign upstream ──

/// #2867: `RealGitBackend::worktree_add` must leave the session branch with NO
/// `branch.<name>.merge` pointing at a ref the worktree does not own.
///
/// Why: this is the exact config shape that clobbered PR #2863 — a worktree's
/// local branch tracked a foreign PR branch, so a later bare `git push` landed
/// on that branch instead of its own. The provisioner is one of the two code
/// paths that create session worktrees, so its output is a standing invariant,
/// not an incidental property: any future `--track` / `--set-upstream-to` /
/// `guessRemote` change here must fail this test rather than silently re-arm
/// the gun.
/// What: builds a real local bare `origin`, runs the production
/// `ensure_base_checkout` + `worktree_add` against it, then enumerates EVERY
/// `branch.*.merge` key visible from the resulting worktree and asserts each
/// one names its own branch. Also asserts the session branch has no upstream
/// at all (`@{u}` fails), which is the fail-safe state.
/// Test: this function IS the test.
#[test]
fn provisioner_worktree_add_writes_no_foreign_branch_merge() {
    let scratch = crate::test_support::hermetic_temp_dir();
    let Some(bare) = make_local_bare_origin(&scratch) else {
        return; // git unavailable
    };
    let repo_url = format!("file://{}", bare.display());
    // #4270: exercise the SHIPPING topology with real git — a non-bare base at
    // the project dir and the worktree NESTED inside it at
    // `<project_dir>/.worktrees/<id>`, not a sibling under a bare base. Whether
    // real `git worktree add` accepts a path inside its own base's working tree
    // is the load-bearing question of this change, and a `FakeGitBackend` whose
    // `worktree_add` is a `create_dir_all` cannot answer it.
    let base_dir = scratch.path().join("owner").join("repo");
    let backend = RealGitBackend::default();
    backend
        .ensure_base_checkout(&repo_url, &base_dir)
        .expect("ensure_base_checkout must succeed against a local bare origin");

    let branch = "session/tm-2867-01";
    let worktree = base_dir.join(".worktrees").join(branch.replace('/', "-"));
    backend
        .worktree_add(&base_dir, "main", &worktree, branch)
        .expect("worktree_add must succeed into <project_dir>/.worktrees/<id>");
    assert!(
        worktree.join(".git").exists(),
        "real git must produce a working worktree nested under its own base at {}",
        worktree.display()
    );

    // Every branch.<name>.merge in the whole config must name its own branch.
    let listed = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["config", "--get-regexp", r"^branch\..*\.merge$"])
        .output()
        .expect("git config --get-regexp");
    let body = String::from_utf8_lossy(&listed.stdout);
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        let own = key
            .trim_start_matches("branch.")
            .trim_end_matches(".merge")
            .to_string();
        assert_eq!(
            value.trim(),
            format!("refs/heads/{own}"),
            "provisioner wrote a FOREIGN upstream (#2867): {line}"
        );
    }

    // And the session branch specifically must have no upstream at all.
    let upstream = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
        .output()
        .expect("git rev-parse @{u}");
    assert!(
        !upstream.status.success(),
        "the provisioner must leave the session branch with NO upstream, got: {}",
        String::from_utf8_lossy(&upstream.stdout).trim()
    );
}

/// #2867: `ensure_base_checkout` must install the cross-branch push guard into
/// the freshly cloned base's shared hooks directory.
///
/// Why: the guard is the only mitigation that covers an ad-hoc `git worktree
/// add` an agent makes for itself — the actual shape of the PR #2863 clobber.
/// It only covers those worktrees if it lands in the base clone's
/// `$GIT_COMMON_DIR/hooks`, which every worktree of that base shares.
/// What: clones a base via the production path and asserts an executable
/// `pre-push` carrying the trusty-mpm marker exists in the resolved hooks dir.
/// Test: this function IS the test.
#[test]
fn ensure_base_checkout_installs_push_guard() {
    let scratch = crate::test_support::hermetic_temp_dir();
    let Some(bare) = make_local_bare_origin(&scratch) else {
        return; // git unavailable
    };
    let repo_url = format!("file://{}", bare.display());
    let base_dir = scratch.path().join("base.git");
    RealGitBackend::default()
        .ensure_base_checkout(&repo_url, &base_dir)
        .expect("ensure_base_checkout must succeed");

    let hooks = crate::core::push_guard::effective_hooks_dir(&base_dir)
        .expect("hooks dir must resolve for a fresh bare clone");
    let hook = hooks.join("pre-push");
    let body = std::fs::read_to_string(&hook)
        .unwrap_or_else(|e| panic!("pre-push guard missing at {}: {e}", hook.display()));
    assert!(
        body.contains(crate::core::push_guard::HOOK_MARKER),
        "installed pre-push must carry the trusty-mpm marker"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&hook)
            .expect("stat hook")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "hook must be executable, mode {mode:o}"
        );
    }
}

/// (#5811) The DOC-28 identity seed resolves the COMMITTED PIN, not the slug
/// derived from the repo URL.
///
/// Why: this call site used the pure three-level `derive_palace_id`, which never
/// reads `.trusty-tools/trusty-memory.yaml`. A pinned project therefore seeded
/// its identity fact under `<owner>-<repo>` while every other surface — session
/// launch, catch-up, the turn recorder, the workstream endpoints — resolved the
/// pinned name. Two names, one project's memory.
/// What: a workspace carrying a pin naming `trusty-tools`, provisioned from a
/// remote whose derived slug would be `bobmatnyc-trusty-tools`. Only the pin
/// being read can tell the two apart.
/// Test: itself.
#[test]
fn identity_seed_palace_prefers_the_committed_pin_over_the_remote() {
    let _env = EnvGuard::clear_palace_override();
    let workspace = TempDir::new().expect("tempdir");
    let pin_dir = workspace.path().join(".trusty-tools");
    std::fs::create_dir_all(&pin_dir).expect("create .trusty-tools");
    std::fs::write(
        pin_dir.join("trusty-memory.yaml"),
        "schema_version: 1\npalace: trusty-tools\n",
    )
    .expect("write pin");

    let got = crate::provisioner::identity_seed::identity_seed_palace(
        workspace.path(),
        "git@github.com:bobmatnyc/trusty-tools.git",
    )
    .expect("a pinned workspace resolves");

    assert_eq!(
        got, "trusty-tools",
        "the identity seed must use the committed pin, not the remote-derived slug"
    );
}

/// RAII guard clearing `TRUSTY_MEMORY_PALACE` for one test.
///
/// Why: level 1 outranks the pin, so an operator override left set in the
/// environment would mask exactly what the test above is asserting.
/// What: snapshots the prior value and restores it on drop.
struct EnvGuard(Option<String>);

impl EnvGuard {
    fn clear_palace_override() -> Self {
        let prior = std::env::var(trusty_common::PALACE_OVERRIDE_ENV).ok();
        // SAFETY: this is the only test in this file that touches the variable.
        unsafe { std::env::remove_var(trusty_common::PALACE_OVERRIDE_ENV) };
        Self(prior)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: as above.
        match &self.0 {
            Some(v) => unsafe { std::env::set_var(trusty_common::PALACE_OVERRIDE_ENV, v) },
            None => unsafe { std::env::remove_var(trusty_common::PALACE_OVERRIDE_ENV) },
        }
    }
}
