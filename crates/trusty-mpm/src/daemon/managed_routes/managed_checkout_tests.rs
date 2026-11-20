//! Unit tests for [`super`] — launch placement and the worktree-request denial.
//!
//! Why: placement is a two-line decision with a large blast radius (a session
//! running in the wrong tree), so each branch is pinned directly rather than
//! inferred from a spawn integration test.
//! What: the equality branch, the redirect branch, the provisioning-failure
//! branch, and both arms of [`super::deny_worktree_fallback`].
//! Test: this module IS the test suite for `super`.
//!
//! The placement tests mutate `TRUSTY_MPM_REPOS_ROOT`, so each takes the
//! crate-wide `env_test_lock` that `trusty_tools_config` and `inproject`
//! already share.

use super::*;

/// Set `TRUSTY_MPM_REPOS_ROOT` for the duration of a test, restoring the prior
/// value (or absence) on drop — panic-safe.
struct ReposRootGuard(Option<std::ffi::OsString>);

impl ReposRootGuard {
    fn set(dir: &Path) -> Self {
        let prev = std::env::var_os(super::super::inproject::REPOS_ROOT_ENV);
        // SAFETY: every caller holds `env_test_lock`, so only one thread
        // mutates the process environment at a time.
        unsafe { std::env::set_var(super::super::inproject::REPOS_ROOT_ENV, dir) };
        Self(prev)
    }
}

impl Drop for ReposRootGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var(super::super::inproject::REPOS_ROOT_ENV, v),
                None => std::env::remove_var(super::super::inproject::REPOS_ROOT_ENV),
            }
        }
    }
}

fn gh() -> GithubPath {
    trusty_common::github_path::parse_github_path("https://github.com/an-owner/a-repo")
        .expect("fixture URL parses")
}

const ORIGIN: &str = "https://github.com/an-owner/a-repo";

/// The managed checkout is exactly the in-project base clone path.
///
/// Why: the redirect target and the base clone the worktree branch establishes
/// must be ONE directory. Asserting the composed path (rather than that the two
/// helpers agree) is what would catch a future rewrite that recomputes the join.
/// Test: this function IS the test.
#[test]
fn managed_checkout_is_the_base_clone_path() {
    let _lock = crate::core::trusty_tools_config::env_test_lock();
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let _env = ReposRootGuard::set(tmp.path());

    assert_eq!(
        managed_checkout_for(&gh()),
        tmp.path().join("an-owner").join("a-repo"),
        "the managed checkout is <workspace-root>/<owner>/<repo>"
    );
}

/// A launch FROM the managed checkout stays there, and provisions nothing.
///
/// Why: the owner's rule has two halves, and this is the one that must not
/// regress into a redundant clone or a rename of a tree that is already correct.
/// What: calls `resolve_placement` with `launch_dir == managed` for a path that
/// does not exist on disk, and asserts the result is that path AND that nothing
/// was created — proof no provisioning ran.
/// Test: this function IS the test.
#[test]
fn managed_launch_dir_is_left_alone() {
    let _lock = crate::core::trusty_tools_config::env_test_lock();
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let _env = ReposRootGuard::set(tmp.path());
    let managed = tmp.path().join("an-owner").join("a-repo");

    let placement =
        resolve_placement(&managed, &gh(), ORIGIN).expect("equality branch never fails");

    assert_eq!(placement, managed);
    assert!(
        !managed.exists(),
        "the equality branch must not provision anything: {}",
        managed.display()
    );
}

/// A launch from an UNMANAGED directory runs in the managed checkout instead,
/// and the unmanaged directory is untouched.
///
/// Why: this is the defect. A launch from the operator's own clone used to run
/// there, inheriting whatever that tree carried.
/// What: pre-creates `<managed>/.git` so `ensure_base_clone` takes its
/// idempotent reuse path (no network, no clone), launches from an unrelated
/// directory, and asserts the placement is the managed checkout while the launch
/// directory gains no new entries.
/// Test: this function IS the test.
#[test]
fn unmanaged_launch_dir_redirects_to_managed() {
    let _lock = crate::core::trusty_tools_config::env_test_lock();
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let _env = ReposRootGuard::set(tmp.path());

    let managed = tmp.path().join("an-owner").join("a-repo");
    std::fs::create_dir_all(managed.join(".git")).expect("seed a base clone");

    let unmanaged = tempfile::TempDir::new().expect("unmanaged temp dir");
    let placement = resolve_placement(unmanaged.path(), &gh(), ORIGIN).expect("redirect succeeds");

    assert_eq!(
        placement, managed,
        "an unmanaged launch directory must switch to the managed checkout"
    );
    assert_eq!(
        std::fs::read_dir(unmanaged.path())
            .expect("read unmanaged dir")
            .count(),
        0,
        "the unmanaged launch directory must never be written to"
    );
}

/// A managed checkout that cannot be established is an ERROR, not a fallback.
///
/// Why: returning the launch directory when provisioning fails is a failure
/// reported as success — the exact shape this whole module exists to remove.
/// What: seeds `<managed>/.worktrees` with no top-level `.git`, which
/// `migrate_old_layout_aside` refuses (#4270), so `ensure_base_clone` errors
/// with no network involved. Asserts `Err`, and that the message names the
/// unmanaged directory the session is being refused in.
/// Test: this function IS the test.
#[test]
fn provisioning_failure_is_an_error_not_a_fallback() {
    let _lock = crate::core::trusty_tools_config::env_test_lock();
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let _env = ReposRootGuard::set(tmp.path());

    let managed = tmp.path().join("an-owner").join("a-repo");
    std::fs::create_dir_all(managed.join(".worktrees")).expect("seed protected state");

    let unmanaged = tempfile::TempDir::new().expect("unmanaged temp dir");
    let err = resolve_placement(unmanaged.path(), &gh(), ORIGIN)
        .expect_err("a base clone that cannot be established must fail the launch");

    assert!(
        err.contains(&unmanaged.path().display().to_string()),
        "the refusal must name the unmanaged directory it will not run in: {err}"
    );
}

/// An explicit worktree request that cannot be honoured returns `Err`.
///
/// Why: the fall-through this replaces spawned a session with no worktree and
/// reported success.
/// Test: this function IS the test.
#[test]
fn deny_worktree_fallback_errors_when_a_worktree_was_requested() {
    let id = ManagedSessionId::new();
    let err = deny_worktree_fallback(&id, true, "base clone unavailable")
        .expect_err("an unhonourable worktree request must abort the spawn");

    assert!(err.contains("base clone unavailable"), "{err}");
    assert!(
        err.contains(&id.to_string()),
        "the error must name the session: {err}"
    );
}

/// With no worktree requested, the historical fall-through is preserved.
///
/// Why: `spawn_managed_local` provisions its own managed clone, so continuing
/// is correct there; narrowing the fix to the explicit-request case is what
/// keeps this change's blast radius to the defect.
/// Test: this function IS the test.
#[test]
fn deny_worktree_fallback_permits_the_fallback_when_none_was_requested() {
    let id = ManagedSessionId::new();
    assert!(
        deny_worktree_fallback(&id, false, "base clone unavailable").is_ok(),
        "no worktree was requested, so the local-path fallback stays available"
    );
}

/// Copies every regular file under `from` into `to`.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read dir").flatten() {
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().expect("file type");
        if kind.is_dir() {
            copy_tree(&src, &dst);
        } else if kind.is_file() {
            std::fs::copy(&src, &dst).expect("copy");
        }
    }
}

/// A trusty-tools checkout at `root` carrying the repository's `content/`.
fn content_checkout(root: &Path) {
    let repo = crate::core::content_source::test_support::repo_root();
    copy_tree(&repo.join("content"), &root.join("content"));
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
}

/// A managed checkout with a `.git` directory and a worktree admin entry
/// `fresh` whose `commondir` points back at it.
fn managed_with_admin(managed: &Path) {
    let admin = managed.join(".git/worktrees/fresh");
    std::fs::create_dir_all(&admin).expect("admin dir");
    std::fs::write(admin.join("commondir"), "../..\n").expect("commondir");
}

/// Writes a worktree `.git` file at `worktree` naming `admin`.
fn worktree_marker(worktree: &Path, admin: &Path) {
    std::fs::create_dir_all(worktree).expect("worktree dir");
    let line = format!("gitdir: {}\n", admin.display());
    std::fs::write(worktree.join(".git"), line).expect(".git file");
}

/// Capture every WARN line `body` logs.
fn captured<T>(body: impl FnOnce() -> T) -> (T, Vec<String>) {
    use tracing_subscriber::layer::SubscriberExt;

    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let out = tracing::subscriber::with_default(subscriber, body);
    (out, buffer.tail(64))
}

/// A launch from a linked worktree of the managed checkout runs there and
/// composes its PM prompt from the worktree's own `content/`.
///
/// Why: the defect — the session composed from the stale managed checkout.
/// What: marks the worktree's `identity.md` with a token the managed copy
/// lacks, resolves the placement, composes the prompt through the calls
/// `build_prompt_file_in` makes, and asserts the token is present.
/// Test: this function IS the test.
#[test]
#[serial_test::serial]
fn a_worktree_of_the_managed_checkout_composes_from_its_own_content() {
    let _lock = crate::core::trusty_tools_config::env_test_lock();
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let _env = ReposRootGuard::set(tmp.path());
    let managed = tmp.path().join("an-owner").join("a-repo");
    content_checkout(&managed);
    managed_with_admin(&managed);
    let worktree = managed.join(".claude/worktrees/fresh");
    worktree_marker(&worktree, &managed.join(".git/worktrees/fresh"));
    content_checkout(&worktree);
    let identity = worktree.join("content/instructions/sections/identity.md");
    let mut text = std::fs::read_to_string(&identity).expect("identity.md");
    text.push_str("\nWORKTREE-ONLY-MARKER-D3I\n");
    std::fs::write(&identity, text).expect("mark identity.md");

    let placement = resolve_placement(&worktree, &gh(), ORIGIN).expect("placement");
    let content = crate::core::content_source::framework_content_for(&placement).expect("content");
    let prompt = crate::core::session_launch::build_system_prompt_for_profile(
        &content,
        &placement,
        None,
        false,
        crate::core::session_profile::SessionProfile::Pm,
    );
    assert!(
        prompt.contains("WORKTREE-ONLY-MARKER-D3I"),
        "the prompt must carry the worktree's content change"
    );
    assert_eq!(placement, worktree, "a linked worktree is managed");
}

/// A worktree whose git admin directory is gone falls back to the managed
/// checkout with a warning.
#[test]
fn a_pruned_worktree_falls_back_to_the_managed_checkout() {
    let _lock = crate::core::trusty_tools_config::env_test_lock();
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let _env = ReposRootGuard::set(tmp.path());
    let managed = tmp.path().join("an-owner").join("a-repo");
    std::fs::create_dir_all(managed.join(".git")).expect("seed a base clone");
    let worktree = managed.join(".claude/worktrees/gone");
    worktree_marker(&worktree, &managed.join(".git/worktrees/gone"));

    let (placement, lines) = captured(|| resolve_placement(&worktree, &gh(), ORIGIN));

    assert_eq!(placement.expect("fallback"), managed);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("admin directory is missing")),
        "{lines:#?}"
    );
}

/// A launch directory that no longer exists falls back to the managed
/// checkout with a warning.
#[test]
fn a_removed_worktree_falls_back_to_the_managed_checkout() {
    let _lock = crate::core::trusty_tools_config::env_test_lock();
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let _env = ReposRootGuard::set(tmp.path());
    let managed = tmp.path().join("an-owner").join("a-repo");
    managed_with_admin(&managed);
    let worktree = managed.join(".claude/worktrees/fresh");

    let (placement, lines) = captured(|| resolve_placement(&worktree, &gh(), ORIGIN));

    assert_eq!(placement.expect("fallback"), managed);
    assert!(
        lines.iter().any(|l| l.contains("does not exist")),
        "{lines:#?}"
    );
}

/// A linked worktree of a different repository is not managed: the launch
/// redirects to the managed checkout exactly as before.
#[test]
fn a_foreign_worktree_redirects_to_the_managed_checkout() {
    let _lock = crate::core::trusty_tools_config::env_test_lock();
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let _env = ReposRootGuard::set(tmp.path());
    let managed = tmp.path().join("an-owner").join("a-repo");
    std::fs::create_dir_all(managed.join(".git")).expect("seed a base clone");
    let foreign = tempfile::TempDir::new().expect("foreign");
    managed_with_admin(foreign.path());
    let worktree = foreign.path().join("wt");
    worktree_marker(&worktree, &foreign.path().join(".git/worktrees/fresh"));

    let placement = resolve_placement(&worktree, &gh(), ORIGIN).expect("redirect");

    assert_eq!(placement, managed);
}
