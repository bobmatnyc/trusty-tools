//! Unit tests for `core::base_facet_index` (#5069).
//!
//! Why: split into a sibling file (declared `#[path = "base_facet_index_tests.rs"]
//! mod tests;`) so `base_facet_index.rs` stays under the 500-SLOC production cap,
//! mirroring `worktree_index_tests.rs`.
//!
//! What: drives [`super::ensure_base_facet_indexed`] against REAL git worktrees
//! built in a hermetic temp dir, so the worktree→base-checkout resolution is
//! proven against git's actual on-disk `gitdir:` pointer rather than a
//! hand-written stand-in. Most tests never touch a daemon
//! (`trusty_common::search_index` refuses daemon writes from a test process,
//! #4255); the three that assert on the wire body stand up their own loopback
//! listener and point discovery at it.
//!
//! Test: `cargo test -p trusty-mpm -- core::base_facet_index`

use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_common::search_index::IndexRegistration;

use super::{BaseFacetOutcome, base_checkout_of, base_facet_options, ensure_base_facet_indexed};

/// Set an env var for the duration of a `#[serial]` test, restoring it on drop.
///
/// Why: the fake-daemon tests redirect daemon discovery and opt out of the
/// #4255 write suppression, and a panic between set and restore would leak
/// process-global state into sibling serial tests.
/// What: snapshots the prior value, sets the new one, restores in `Drop`.
/// Test: used by [`point_discovery_at`].
struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: every test using this guard is tagged `#[serial]`, so no other
        // thread races the set/restore. Restore happens in `Drop`.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see `set` — serialized by `#[serial]`.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// Run `git <args>` inside `dir`, returning whether it succeeded.
fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build a real git repository with one commit, or `None` when git is
/// unavailable in this environment so the tests skip rather than fail.
fn init_repo(base: &Path) -> Option<()> {
    std::fs::create_dir_all(base).ok()?;
    if !git_ok(base, &["init", "--initial-branch=main"]) {
        return None;
    }
    let _ = git_ok(base, &["config", "user.email", "ci@test.invalid"]);
    let _ = git_ok(base, &["config", "user.name", "CI"]);
    std::fs::write(base.join("README.md"), "# fixture\n").ok()?;
    if !git_ok(base, &["add", "."]) {
        return None;
    }
    if !git_ok(base, &["commit", "-m", "seed"]) {
        return None;
    }
    Some(())
}

/// A base clone plus one worktree cut from it, both under a hermetic temp dir.
struct Fixture {
    _dir: tempfile::TempDir,
    base: PathBuf,
    worktree: PathBuf,
}

fn fixture() -> Option<Fixture> {
    let dir = crate::test_support::hermetic_temp_dir();
    let base = dir.path().join("base-repo");
    init_repo(&base)?;
    let worktree = dir.path().join("feature-wt");
    if !git_ok(
        &base,
        &[
            "worktree",
            "add",
            "-b",
            "feature",
            worktree.to_str().expect("utf8 path"),
        ],
    ) {
        return None;
    }
    // git records the CANONICAL repository path in the worktree's `gitdir:`
    // pointer, and on macOS the temp dir reaches it through the `/tmp` →
    // `/private/tmp` symlink. Canonicalising the fixture's own paths keeps the
    // assertions about which facet was chosen, not about which spelling of the
    // same directory each side happened to hold.
    Some(Fixture {
        base: base.canonicalize().ok()?,
        worktree: worktree.canonicalize().ok()?,
        _dir: dir,
    })
}

/// git's own `gitdir:` pointer resolves back to the primary checkout.
///
/// Why: everything else in this module is downstream of getting this path
/// right, and the pointer format is git's, not ours — asserting it against a
/// hand-written `.git` file would only prove our parser agrees with our fixture.
/// Test: this test.
#[test]
fn resolves_the_base_checkout_of_a_real_worktree() {
    let Some(fx) = fixture() else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    assert_eq!(
        base_checkout_of(&fx.worktree),
        Some(fx.base.clone()),
        "a worktree's gitdir pointer must lead back to the checkout it was cut from"
    );
}

/// The registration targets the BASE checkout, not the worktree (#5069).
///
/// Why: this is the defect. Before this module every registration trusty-mpm
/// made named a worktree, so no index anywhere carried vectors for the repo and
/// a semantic query from a worktree had nothing to route to. Asserting on the
/// derived index id — which is the checkout's directory name — is what catches a
/// regression that re-points this at `worktree_path`, because such a version
/// still "registers something" and still returns an `Unconfirmed` outcome.
/// What: the daemon write is suppressed under the test harness (#4255), so the
/// outcome is `RegistrationUnconfirmed { reason: SkippedUnderTest }` — which
/// proves the call reached `ensure_project_indexed_reporting` past both guards —
/// and its `index_id` is the base checkout's, never the worktree's.
/// Test: this test. `#[serial]` because the sibling wire test sets
/// `TRUSTY_ALLOW_PRODUCTION_STATE` process-globally; overlapping it would let
/// this call attempt a real POST and report `NotConfirmed` instead.
#[test]
#[serial_test::serial]
fn base_facet_targets_the_base_checkout_not_the_worktree() {
    let Some(fx) = fixture() else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    let expected = trusty_common::derive_index_id(&trusty_common::resolve_project_root(&fx.base));
    let worktree_id =
        trusty_common::derive_index_id(&trusty_common::resolve_project_root(&fx.worktree));
    assert_ne!(
        expected, worktree_id,
        "fixture precondition: the two facets must derive different ids"
    );

    match ensure_base_facet_indexed(&fx.worktree) {
        BaseFacetOutcome::RegistrationUnconfirmed { index_id, reason } => {
            assert_eq!(index_id, expected, "the base facet's id is the CHECKOUT's");
            assert_eq!(reason, IndexRegistration::SkippedUnderTest);
        }
        other => panic!("expected an unconfirmed base-facet registration, got {other:?}"),
    }
}

/// The base facet keeps its vector lane — the whole point of the module.
///
/// Why: `worktree_index` passes `skip_vector: true` two files away. A change
/// that makes "all trusty-mpm index registrations" consistent by copying that
/// flag here would delete semantic search for the repo entirely while every
/// other test in this file still passes.
/// Test: this test.
#[test]
fn base_facet_keeps_the_vector_lane_on() {
    let opts = base_facet_options();
    assert!(
        !opts.skip_vector,
        "the base checkout owns the embedding lane (#5060 ruling); skip_vector must stay false"
    );
    assert!(
        !opts.allow_sensitive_path,
        "the daemon's sensitive-path denylist stays armed (#2914)"
    );
}

/// A plain clone has no base facet to derive — it IS one.
///
/// Why: a permissive version that walked upward from any directory would
/// register the enclosing repository of whatever path it was handed, making the
/// operator's opt-in decision for them (#767). Refusing at the shape check keeps
/// the input to real worktrees.
/// Test: this test.
#[test]
fn plain_clone_is_not_a_base_facet_source() {
    let dir = crate::test_support::hermetic_temp_dir();
    let clone = dir.path().join("plain-clone");
    std::fs::create_dir_all(clone.join(".git")).expect("mkdir .git");

    assert_eq!(
        ensure_base_facet_indexed(&clone),
        BaseFacetOutcome::NotAWorktree
    );
}

/// A stale `gitdir:` pointer is REPORTED, never silently treated as success.
///
/// Why: the failure this issue is about was invisible — nothing observed that no
/// base facet existed, so a search returning only lexical rows was
/// indistinguishable from a repo with nothing to find. Every refusal here has to
/// be a distinct value a caller (and this test) can see.
/// What: a `.git` file whose pointer names a repository that does not exist. The
/// tail parses as a worktree pointer, so only the primary-checkout check can
/// reject it.
/// Test: this test.
#[test]
fn unresolvable_gitdir_pointer_is_reported_not_registered() {
    let dir = crate::test_support::hermetic_temp_dir();
    let wt = dir.path().join("orphan-wt");
    std::fs::create_dir_all(&wt).expect("mkdir");
    std::fs::write(
        wt.join(".git"),
        "gitdir: /nonexistent-5069/base/.git/worktrees/orphan-wt\n",
    )
    .expect("write gitlink");

    assert_eq!(base_checkout_of(&wt), None);
    assert_eq!(
        ensure_base_facet_indexed(&wt),
        BaseFacetOutcome::BaseCheckoutUnresolved
    );
}

/// A `gitdir:` pointer outside a `worktrees/` directory resolves to nothing.
///
/// Why: a submodule's `.git` is also a FILE, and its pointer names
/// `<common>/modules/<name>`. Stripping two components blindly would hand back
/// the superproject and register an index for it. The `worktrees` tail check is
/// what separates the two shapes.
/// Test: this test.
#[test]
fn gitdir_outside_a_worktrees_directory_resolves_to_nothing() {
    let dir = crate::test_support::hermetic_temp_dir();
    let base = dir.path().join("super");
    std::fs::create_dir_all(base.join(".git")).expect("mkdir .git");
    let sub = base.join("vendor").join("dep");
    std::fs::create_dir_all(&sub).expect("mkdir sub");
    std::fs::write(
        sub.join(".git"),
        format!("gitdir: {}/.git/modules/dep\n", base.display()),
    )
    .expect("write gitlink");

    assert_eq!(base_checkout_of(&sub), None);
}

/// A loopback stand-in for the trusty-search daemon.
///
/// Why: three tests below need the bytes a registration puts on the wire, and
/// one `ensure_project_indexed_reporting` call makes three requests (create,
/// status, reindex) across two clients — a single-connection listener captures
/// the first and leaves the rest in the accept backlog. Looping also lets the
/// two call-site tests observe BOTH registrations a worktree path triggers and
/// pick out the one they care about.
/// What: binds an ephemeral loopback port, answers every request `200 {}` (an
/// empty status body is not fresh, so the reindex trigger proceeds as it would
/// in production), and forwards the body of each `POST /indexes` to the returned
/// channel. The accept loop is detached and ends when the receiver is gone.
/// Test: used by `base_facet_posts_the_base_checkout_with_the_vector_lane_on`,
/// `session_launch_registers_the_base_facet_for_a_worktree`,
/// `worktree_creation_registers_the_base_facet`.
fn fake_daemon() -> (std::net::SocketAddr, std::sync::mpsc::Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        while let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
            );
            let _ = stream.flush();
            if request.starts_with("POST /indexes ")
                && tx
                    .send(request.split("\r\n\r\n").nth(1).unwrap_or("").to_string())
                    .is_err()
            {
                return;
            }
        }
    });
    (addr, rx)
}

/// Redirect daemon discovery at `addr` and opt out of the #4255 suppression.
///
/// Why: both env vars are process-global, so every caller is `#[serial]`, and
/// returning the guards keeps them alive for the caller's whole body.
/// What: writes `<data_dir>/trusty-search/http_addr` (the on-disk contract
/// `resolve_daemon_base_url` reads) and sets the two variables. Opting out of
/// the write suppression is safe here precisely because discovery now points at
/// the test's own loopback socket, never the operator's daemon.
/// Test: used by the three fake-daemon tests.
fn point_discovery_at(data_dir: &Path, addr: std::net::SocketAddr) -> (EnvVarGuard, EnvVarGuard) {
    let search_data_dir = data_dir.join("trusty-search");
    std::fs::create_dir_all(&search_data_dir).expect("mkdir");
    std::fs::write(search_data_dir.join("http_addr"), addr.to_string()).expect("write http_addr");
    (
        EnvVarGuard::set(
            trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV,
            &data_dir.to_string_lossy(),
        ),
        EnvVarGuard::set(trusty_common::test_harness::ALLOW_PRODUCTION_ENV, "1"),
    )
}

/// Wait for a create-index POST naming `root`, ignoring any others.
///
/// Why: a worktree path registers TWO indexes — its own and the base facet —
/// and the detached threads decide the order. Filtering by `root_path` makes
/// the assertion about which facet was registered rather than about which
/// thread won.
/// What: drains the channel until a body's `root_path` matches, or the deadline
/// passes. `None` means no such POST arrived.
/// Test: used by the three fake-daemon tests.
fn wait_for_create_index(
    rx: &std::sync::mpsc::Receiver<String>,
    root: &Path,
) -> Option<serde_json::Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        let body = rx.recv_timeout(remaining).ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
        if parsed.get("root_path").and_then(serde_json::Value::as_str)
            == Some(root.to_string_lossy().as_ref())
        {
            return Some(parsed);
        }
    }
    None
}

/// Assert a captured create-index body registers the base facet with vectors.
fn assert_base_facet_body(body: &serde_json::Value) {
    assert_eq!(
        body.get("skip_vector"),
        Some(&serde_json::Value::Bool(false)),
        "the base facet is the one index that embeds; got {body:?}"
    );
    assert_eq!(
        body.get("allow_sensitive_path"),
        Some(&serde_json::Value::Bool(false)),
        "the daemon's denylist stays armed (#2914); got {body:?}"
    );
}

/// The `POST /indexes` that reaches the daemon names the base checkout and
/// leaves the vector lane on (#5069).
///
/// Why: every outcome-value test could stay correct while the request on the
/// wire changed. This one asserts on the bytes the daemon would actually
/// receive — the technique `session_launch::tests_search_index` uses for the
/// `allow_sensitive_path` invariant — so both halves of the fix (which root,
/// which lane) are pinned where they are observable.
/// Test: this test. `#[serial]` because `point_discovery_at` sets two
/// process-global env vars.
#[test]
#[serial_test::serial]
fn base_facet_posts_the_base_checkout_with_the_vector_lane_on() {
    let Some(fx) = fixture() else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    let data_dir = tempfile::tempdir().expect("data dir");
    let (addr, rx) = fake_daemon();
    let _guards = point_discovery_at(data_dir.path(), addr);

    let _ = ensure_base_facet_indexed(&fx.worktree);

    let expected_root = trusty_common::resolve_project_root(&fx.base);
    let body = wait_for_create_index(&rx, &expected_root)
        .expect("the fake daemon must have received a create-index POST for the BASE checkout");
    assert_base_facet_body(&body);
}

/// Session launch in a worktree registers the base facet (#5069).
///
/// Why: this is the call site that reaches the worktrees which already exist —
/// they never run the creation-time path again, so deleting this one line would
/// leave the live worktrees exactly as broken as before while every
/// module-level test in this file still passed.
/// What: drives the real `session_launch::register_project_index` against the
/// fake daemon and requires a create-index POST naming the BASE checkout. The
/// worktree's own registration also arrives; `wait_for_create_index` filters it
/// out by root.
/// Test: this test.
#[test]
#[serial_test::serial]
fn session_launch_registers_the_base_facet_for_a_worktree() {
    let Some(fx) = fixture() else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    let data_dir = tempfile::tempdir().expect("data dir");
    let (addr, rx) = fake_daemon();
    let _guards = point_discovery_at(data_dir.path(), addr);

    let _ = crate::core::session_launch::register_project_index(&fx.worktree);

    let expected_root = trusty_common::resolve_project_root(&fx.base);
    let body = wait_for_create_index(&rx, &expected_root)
        .expect("launching a session in a worktree must also register its base facet");
    assert_base_facet_body(&body);
}

/// Worktree creation registers the base facet (#5069).
///
/// Why: the other call site. `index_new_worktree_in_background` is what both
/// worktree-minting paths invoke, so this covers `provisioner::workspace` and
/// `daemon::managed_routes::inproject` at their shared entry point.
/// What: as above, through the background entry point — which returns
/// immediately, so the assertion waits on the daemon rather than on a join.
/// Test: this test.
#[test]
#[serial_test::serial]
fn worktree_creation_registers_the_base_facet() {
    let Some(fx) = fixture() else {
        eprintln!("skipping: git unavailable in this environment");
        return;
    };
    let data_dir = tempfile::tempdir().expect("data dir");
    let (addr, rx) = fake_daemon();
    let _guards = point_discovery_at(data_dir.path(), addr);

    crate::core::worktree_index::index_new_worktree_in_background(fx.worktree.clone());

    let expected_root = trusty_common::resolve_project_root(&fx.base);
    let body = wait_for_create_index(&rx, &expected_root)
        .expect("creating a worktree must also register its base facet");
    assert_base_facet_body(&body);
}
