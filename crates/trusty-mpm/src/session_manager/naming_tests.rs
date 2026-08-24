//! Integration tests for the `tm-<leaf>-NN` naming scheme (issue #1955).
//!
//! Why: `session_manager/tests.rs` is at the 1500-SLOC test cap; the
//! #1955 serial-allocation and backward-compat-reconciliation coverage lives
//! here so neither file grows past its limit. Uses `FakeTmuxDriver` from the
//! sibling `tests` module, mirroring the pattern established by
//! `backfill_tests.rs`/`decommission_worktree_tests.rs`. The PURE naming-logic
//! unit tests (leaf derivation, serial gap-reuse, the >99 exhaustion case) live
//! in `crate::core::names`'s own test module — this file covers the same
//! behavior end-to-end through the real [`SessionManager`] API.
//! What: (1) serial reuse after decommission, exercised through
//! `SessionManager::create`; (2) `reconcile_on_boot` recognizing a live
//! `tm-`-prefixed session exactly like the legacy `tmpm-` prefix.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use tempfile::TempDir;

use super::manager::{ManagedTmuxDriver, SessionManager};
use super::record::ManagedSessionState;
use super::tests::FakeTmuxDriver;

/// RAII guard restoring `$HOME` on drop (including panic) — mirrors the
/// identical pattern in `core::session_launch::tests::EnvVarGuard` and
/// siblings (each module needs its own copy; `pub(super)`/module-private
/// visibility does not cross sibling module trees — see those sites' docs).
///
/// Why (#3965): `reconcile_on_boot`'s external-adopt loop, when a pane's cwd
/// resolves, calls `deploy_validate::validate_and_repair(&fw, &workspace,
/// None)` where `fw = FrameworkPaths::for_managed_workspace(&workspace)` —
/// whose root resolves from the REAL process `$HOME` (`core/paths.rs`), not
/// from `workspace`. When the workspace is incomplete (as every bare
/// `TempDir` used by these tests is), that falls through to
/// `preseed_workspace_trust_home`, writing into the operator's real
/// `~/.claude.json`. Pairs with `#[serial_test::serial]`.
/// Test: used by every `reconcile_*` test below that resolves a pane cwd.
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

/// The full `tm-<leaf>-NN` naming convention (issue #1955): a `tm-` prefix
/// AND a trailing two-digit numeric serial.
///
/// Why (#1966 review follow-up, moved from `tests.rs` to keep that file under
/// its 1500-SLOC test cap): checking only the `tm-` prefix would not catch a
/// regression that dropped the `-NN` suffix entirely (e.g. a future change
/// that regressed to the pre-#1955 `tm-<leaf>` form with no serial).
/// What: creates one session and asserts its `tmux_name` has the `tm-` prefix
/// AND that the segment after the last dash is exactly two ASCII digits.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_naming_convention() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/wt1")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    assert!(record.tmux_name.starts_with("tm-"), "has tm- prefix");
    let suffix = record
        .tmux_name
        .rsplit('-')
        .next()
        .expect("name has at least one dash-delimited segment");
    assert_eq!(
        suffix.len(),
        2,
        "serial suffix is exactly 2 digits: {}",
        record.tmux_name
    );
    assert!(
        suffix.bytes().all(|b| b.is_ascii_digit()),
        "serial suffix is numeric: {}",
        record.tmux_name
    );
    // `01`-`99`, never `00` (#1966 review follow-up: a bare length+digit-ness
    // check would also accept "00", which `allocate_serial` never hands out).
    let serial: u8 = suffix.parse().expect("suffix is numeric (checked above)");
    assert!(
        (1..=99).contains(&serial),
        "serial is in 01-99: {}",
        record.tmux_name
    );
}

/// Serial reuse end-to-end (issue #1955 worked example): sessions `-01`,
/// `-02`, `-03` exist for a project; `-02` is decommissioned; the NEXT session
/// created for that project must reuse `-02`, not jump to `-04`.
///
/// Why: this is the ticket's acceptance example, exercised through the real
/// [`SessionManager::create`] path (not just the pure `names` unit tests) so
/// the store-filtering behavior in `names_for_serial_allocation` is covered
/// end-to-end.
/// What: creates three sessions for the same repo-derived leaf, marks the
/// second `Decommissioned` directly in the store (and kills its tmux session,
/// mirroring what real decommission does), creates a fourth, and asserts it
/// reused the `-02` serial.
/// Test: this function IS the test.
#[tokio::test]
async fn manager_serial_reuses_decommissioned_gap() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();
    let repo_url = Some("https://github.com/acme/gap-test".to_string());

    let r1 = mgr
        .create(
            "t1".into(),
            Some(PathBuf::from("/tmp/g1")),
            None,
            None,
            repo_url.clone(),
            None,
        )
        .await
        .expect("create r1");
    let r2 = mgr
        .create(
            "t2".into(),
            Some(PathBuf::from("/tmp/g2")),
            None,
            None,
            repo_url.clone(),
            None,
        )
        .await
        .expect("create r2");
    let r3 = mgr
        .create(
            "t3".into(),
            Some(PathBuf::from("/tmp/g3")),
            None,
            None,
            repo_url.clone(),
            None,
        )
        .await
        .expect("create r3");

    assert_eq!(r1.tmux_name, "tm-gap-test-01");
    assert_eq!(r2.tmux_name, "tm-gap-test-02");
    assert_eq!(r3.tmux_name, "tm-gap-test-03");

    // Decommission r2: flip its store state and kill its tmux session (mirrors
    // what the real `decommission` path does to the tmux host).
    {
        let mut store = mgr.store.write().await;
        let mut rec = store.get(&r2.id).await.expect("get r2");
        rec.state = ManagedSessionState::Decommissioned;
        store.upsert(rec).await.expect("upsert decommissioned r2");
    }
    fake.kill_session(&r2.tmux_name).expect("kill r2 tmux");

    let r4 = mgr
        .create(
            "t4".into(),
            Some(PathBuf::from("/tmp/g4")),
            None,
            None,
            repo_url.clone(),
            None,
        )
        .await
        .expect("create r4");
    assert_eq!(
        r4.tmux_name, "tm-gap-test-02",
        "decommissioned serial 02 must be reused, got {}",
        r4.tmux_name
    );
}

/// Live tmux sessions created under the NEW `tm-` prefix (#1955) must be
/// recognized and adopted by boot reconciliation exactly like the legacy
/// `tmpm-` prefix — the daemon must never treat a `tm-<leaf>-NN` session as
/// foreign just because the prefix changed.
///
/// Why: `reconcile_on_boot` used to hardcode `starts_with("tmpm-")`; after
/// #1955 it must use `is_managed_session_name` so sessions created under the
/// new scheme are adopted, not ignored (which would leave them untracked and
/// vulnerable to the orphan-GC).
/// What: seeds a live `tm-`-prefixed tmux session unknown to the store, runs
/// reconcile, and asserts it appears in `external_adopted` as `Active`.
///
/// #6118: the pane resolves a working directory. Adoption now requires that —
/// a pane whose cwd will not resolve is declined, covered by
/// `reconcile_declines_to_adopt_a_pane_whose_cwd_cannot_be_resolved` — so this
/// test uses [`PaneCwdTmux`] rather than the shared `FakeTmuxDriver`, whose
/// `get_pane_cwd` is the trait default (`None`).
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn manager_reconcile_adopts_new_prefix_session() {
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    // #3965: `#[serial]` + `$HOME` override — see `HomeGuard` above.
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(PaneCwdTmux {
        alive: vec!["tm-trusty-tools-01".into()],
        pane_cwd: Some(workspace.path().to_path_buf()),
    });

    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();

    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");
    assert!(
        report
            .external_adopted
            .contains(&"tm-trusty-tools-01".to_string()),
        "a live tm-prefixed session must be externally adopted; report: {report:?}"
    );

    let listed = mgr.list().await;
    let adopted = listed
        .iter()
        .find(|r| r.tmux_name == "tm-trusty-tools-01")
        .expect("adopted record present");
    assert_eq!(adopted.state, ManagedSessionState::Active);
}

/// Minimal `ManagedTmuxDriver` test double whose `get_pane_cwd` is
/// controllable, mirroring `lifecycle.rs`'s local `StubTmux` pattern rather
/// than extending the shared `tests::FakeTmuxDriver` (already near its
/// 1500-SLOC test-file cap).
///
/// Why (#2158): the adopted-session cwd-resolution fix needs a driver whose
/// `get_pane_cwd` returns a controllable value; every other trait method is
/// unused by these two tests and panics if called, so a wiring mistake fails
/// loudly instead of silently passing.
/// What: `list_sessions` returns `alive`; `get_pane_cwd` returns `pane_cwd`
/// for every session name (both fields set at construction).
/// Test: used by `reconcile_resolves_adopted_session_cwd_from_pane` and
/// `reconcile_flags_unresolvable_adopted_session_as_unmanaged` below.
struct PaneCwdTmux {
    alive: Vec<String>,
    pane_cwd: Option<PathBuf>,
}

impl ManagedTmuxDriver for PaneCwdTmux {
    fn create_session(
        &self,
        _name: &str,
        _workdir: &str,
    ) -> Result<(), super::manager::ManagedError> {
        unimplemented!("not exercised by PaneCwdTmux tests")
    }
    fn kill_session(&self, _name: &str) -> Result<(), super::manager::ManagedError> {
        unimplemented!("not exercised by PaneCwdTmux tests")
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), super::manager::ManagedError> {
        unimplemented!("not exercised by PaneCwdTmux tests")
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, super::manager::ManagedError> {
        unimplemented!("not exercised by PaneCwdTmux tests")
    }
    fn list_sessions(&self) -> Result<Vec<String>, super::manager::ManagedError> {
        Ok(self.alive.clone())
    }
    fn get_pane_cwd(&self, _name: &str) -> Option<PathBuf> {
        self.pane_cwd.clone()
    }
}

/// Why (#2158): an adopted session whose pane cwd resolves to a real,
/// existing directory must carry that directory as BOTH `cwd` and
/// `workspace_path` — never the permanent `/unknown` stub — so it can be
/// validated/auto-repaired like any other managed workspace.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn reconcile_resolves_adopted_session_cwd_from_pane() {
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    // #3965: `#[serial]` + `$HOME` override — see `HomeGuard` above.
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(PaneCwdTmux {
        alive: vec!["tm-resolvable-01".into()],
        pane_cwd: Some(workspace.path().to_path_buf()),
    });

    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    mgr.reconcile_on_boot(false).await.expect("reconcile");

    let listed = mgr.list().await;
    let adopted = listed
        .iter()
        .find(|r| r.tmux_name == "tm-resolvable-01")
        .expect("adopted record present");
    assert_eq!(adopted.cwd, workspace.path());
    assert_eq!(adopted.workspace_path.as_deref(), Some(workspace.path()));
    assert_eq!(adopted.task, "adopted session");
}

/// A `ManagedTmuxDriver` whose `get_pane_cwd` fails the first `flaky_for`
/// calls and succeeds afterwards, counting every call.
///
/// Why (#6118 review): every other fixture here fixes `pane_cwd` for its
/// lifetime, so the case that actually matters — a probe that fails once and
/// then works — had no coverage at all. That is the case where declining kills
/// a live pane: `TmuxDriver::pane_current_path` reports spawn failure, a
/// non-zero exit and empty output all as `None`, and a declined pane is
/// orphan-GC input.
/// What: `list_sessions` returns `alive`; `get_pane_cwd` returns `None` for the
/// first `flaky_for` calls and `Some(pane_cwd)` after, incrementing `calls`
/// every time. `pane_cwd: None` makes it permanently unresolvable, which is how
/// the retry BOUND is asserted.
/// Test: `flaky_cwd_probe_still_adopts_within_one_reconcile`,
/// `unresolvable_cwd_probe_is_retried_a_bounded_number_of_times`.
struct FlakyPaneCwdTmux {
    alive: Vec<String>,
    pane_cwd: Option<PathBuf>,
    flaky_for: usize,
    calls: std::sync::atomic::AtomicUsize,
    /// What `get_pane_id` reports — `flaky_for: 0` plus a distinct value here
    /// makes this a plain fixture standing in for a DIFFERENT physical pane
    /// under a reused tmux name.
    pane_id: Option<String>,
}

impl ManagedTmuxDriver for FlakyPaneCwdTmux {
    fn create_session(
        &self,
        _name: &str,
        _workdir: &str,
    ) -> Result<(), super::manager::ManagedError> {
        unimplemented!("not exercised by FlakyPaneCwdTmux tests")
    }
    fn kill_session(&self, _name: &str) -> Result<(), super::manager::ManagedError> {
        unimplemented!("not exercised by FlakyPaneCwdTmux tests")
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), super::manager::ManagedError> {
        unimplemented!("not exercised by FlakyPaneCwdTmux tests")
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, super::manager::ManagedError> {
        unimplemented!("not exercised by FlakyPaneCwdTmux tests")
    }
    fn list_sessions(&self) -> Result<Vec<String>, super::manager::ManagedError> {
        Ok(self.alive.clone())
    }
    fn get_pane_cwd(&self, _name: &str) -> Option<PathBuf> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n < self.flaky_for {
            return None;
        }
        self.pane_cwd.clone()
    }
    fn get_pane_id(&self, _name: &str) -> Option<String> {
        self.pane_id.clone()
    }
}

/// #6118 review, CRITICAL: one transient probe failure must not cost a live
/// pane.
///
/// Why: declining hands the pane to the orphan-GC, which kills an idle-shell
/// pane with no live child after two 60-second sweeps — and `reconcile_on_boot`
/// runs once per daemon process, so a single flaky `get_pane_cwd` would be that
/// pane's only hearing until the next daemon restart. The retry in
/// `resolve_adoptable_cwd` has to close that inside one reconcile pass.
/// What: a probe that fails once then succeeds; asserts ONE `reconcile_on_boot`
/// adopts the pane normally — real cwd, real workspace, nothing declined.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn flaky_cwd_probe_still_adopts_within_one_reconcile() {
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(FlakyPaneCwdTmux {
        alive: vec!["tm-flaky-01".into()],
        pane_cwd: Some(workspace.path().to_path_buf()),
        flaky_for: 1,
        calls: std::sync::atomic::AtomicUsize::new(0),
        pane_id: None,
    });

    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();
    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert!(
        report.adoption_declined.is_empty(),
        "a probe that recovers on retry must not be declined; report: {report:?}"
    );
    let listed = mgr.list().await;
    assert_eq!(
        listed.len(),
        1,
        "the pane must be adopted within one reconcile pass: {listed:?}"
    );
    assert_eq!(listed[0].tmux_name, "tm-flaky-01");
    assert_eq!(listed[0].cwd, workspace.path());
    assert_eq!(listed[0].workspace_path.as_deref(), Some(workspace.path()));
    assert!(
        fake.calls.load(std::sync::atomic::Ordering::SeqCst) >= 2,
        "the first probe must have been retried"
    );
}

/// The retry is bounded — a genuinely unresolvable pane is declined, not
/// probed forever.
///
/// Why: the retry exists to survive a flake, not to hang boot on a pane that
/// will never resolve. Boot holds the store write lock while it runs.
/// What: a probe that never succeeds; asserts the pane is declined and that
/// `get_pane_cwd` was called exactly three times (`CWD_PROBE_BACKOFF.len() + 1`).
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn unresolvable_cwd_probe_is_retried_a_bounded_number_of_times() {
    let dir = TempDir::new().unwrap();
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(FlakyPaneCwdTmux {
        alive: vec!["tm-never-01".into()],
        pane_cwd: None,
        flaky_for: usize::MAX,
        calls: std::sync::atomic::AtomicUsize::new(0),
        pane_id: None,
    });

    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();
    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert_eq!(report.adoption_declined, vec!["tm-never-01".to_string()]);
    assert_eq!(mgr.list().await.len(), 0);
    assert_eq!(
        fake.calls.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "the probe must be attempted exactly 3 times, then give up"
    );
}

/// #6118: a live pane whose cwd will not resolve is NOT adopted.
///
/// Why: #2158 adopted it as an `Active` record with a `/unknown` cwd and an
/// `unmanaged` note in `task`, and that record could never be retired. The
/// `tm ls` auto-prune keeps any record whose tmux name is live, and the
/// orphan-GC keeps any pane a registry names — so the act of adopting the pane
/// is what made the pane unreapable and the record permanent. 55 of the 103
/// records in the reporting store were these. Declining hands the pane back to
/// the orphan-GC, which already knows how to reap an idle one safely.
/// What: one live pane, `get_pane_cwd` returning `None`; asserts no record was
/// written, the name is reported in `adoption_declined`, and it is absent from
/// `external_adopted`.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn reconcile_declines_to_adopt_a_pane_whose_cwd_cannot_be_resolved() {
    let dir = TempDir::new().unwrap();
    // #3965: `#[serial]` + `$HOME` override — see `HomeGuard` above. A declined
    // pane never reaches `validate_and_repair`, but pin `$HOME` anyway so a
    // future refactor cannot silently reopen the real-`$HOME` write.
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(PaneCwdTmux {
        alive: vec!["tm-unresolvable-01".into()],
        pane_cwd: None,
    });

    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert_eq!(
        mgr.list().await.len(),
        0,
        "a pane with no resolvable cwd must leave no record behind"
    );
    assert!(
        report
            .adoption_declined
            .contains(&"tm-unresolvable-01".to_string()),
        "the declined pane must be named in the report; report: {report:?}"
    );
    assert!(
        report.external_adopted.is_empty(),
        "nothing was adopted; report: {report:?}"
    );
}

/// #6118 + #6117 together: reconciling the same unresolvable pane over and over
/// never accumulates records.
///
/// Why: this is the shape the reporting store was in — a pane re-encountered on
/// every daemon boot, each pass free to write another row for it. The count
/// after N passes must be the count after one.
/// What: three reconcile passes over one unresolvable pane; asserts the store
/// is still empty and every pass declined it.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn repeated_reconcile_of_one_unresolvable_pane_stays_at_zero_records() {
    let dir = TempDir::new().unwrap();
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(PaneCwdTmux {
        alive: vec!["tm-ghosty-01".into()],
        pane_cwd: None,
    });

    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    for pass in 1..=3 {
        let report = mgr.reconcile_on_boot(false).await.expect("reconcile");
        assert_eq!(
            report.adoption_declined,
            vec!["tm-ghosty-01".to_string()],
            "pass {pass} must decline the pane; report: {report:?}"
        );
        assert_eq!(
            mgr.list().await.len(),
            0,
            "pass {pass} must leave the store empty"
        );
    }
}

/// #6118 reclaim path: a declined pane is one the orphan-GC can reap.
///
/// Why: declining adoption is only a fix if something else can then retire the
/// pane. The orphan-GC's criterion is "managed prefix, absent from BOTH
/// registries, idle shell, no live child" — and adoption is what used to put
/// the pane in the managed registry. This asserts the whole chain: after a
/// reconcile pass that declined it, the pane's name is absent from
/// `known_tmux_names` (what the GC's protected set is built from), and
/// `classify_session` returns `ReapCandidate` for an idle `sh` pane.
/// What: reconcile with an unresolvable pane, feed the resulting
/// `known_tmux_names` into `TrackedNames::managed`, classify an idle pane.
/// Test: this function IS the test.
#[cfg(feature = "daemon")]
#[tokio::test]
#[serial_test::serial]
async fn a_declined_pane_is_reapable_by_the_orphan_gc() {
    use crate::daemon::orphan_gc::{
        AlwaysIdleProbe, GcDecision, PaneInfo, TrackedNames, classify_session,
    };

    let dir = TempDir::new().unwrap();
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(PaneCwdTmux {
        alive: vec!["tm-leaked-01".into()],
        pane_cwd: None,
    });

    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    mgr.reconcile_on_boot(false).await.expect("reconcile");

    let managed = mgr.known_tmux_names().await.expect("known names");
    assert!(
        !managed.contains("tm-leaked-01"),
        "a declined pane must not be in the orphan-GC's protected set: {managed:?}"
    );

    let decision = classify_session(
        &PaneInfo {
            session_name: "tm-leaked-01".into(),
            pane_current_command: "sh".into(),
            pane_pid: None,
            pane_id: None,
        },
        &TrackedNames {
            managed,
            ..Default::default()
        },
        &AlwaysIdleProbe,
    );
    assert_eq!(
        decision,
        GcDecision::ReapCandidate,
        "an idle leaked pane the daemon declined to adopt must be reapable"
    );
}

/// #3396 regression: a LIVE tmux session unknown to the store by NAME must
/// NOT be externally-adopted as a second identity when its resolved pane cwd
/// is the SAME literal workspace an EXISTING, non-`Decommissioned` record
/// already tracks.
///
/// Why: this is the duplicate-record defect's creation path — a tmux session
/// renamed (or replaced) out from under its original managed record, then
/// rediscovered by `reconcile_on_boot`'s external-adopt loop under its new
/// live name, minted a completely separate `SessionRecord` for the exact same
/// worktree because the loop only ever checked `tmux_name` membership, never
/// the resolved workspace path against records the store already tracks —
/// the same class of bug `decide_native_registration` (#3599) fixed for the
/// native-process discovery path. The fix must skip adoption instead of
/// minting a duplicate.
/// What: seeds a `Stopped` record for `workspace` under tmux_name
/// `tm-known-01` (NOT live), then runs reconcile with a live tmux session
/// `tm-crossed-01` (a different name, unknown to the store) whose pane cwd
/// resolves to the SAME `workspace` directory. Asserts: no second record was
/// minted (`mgr.list()` still has exactly one entry), and `tm-crossed-01`
/// never appears in `report.external_adopted`.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn reconcile_skips_external_adopt_when_workspace_already_tracked() {
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    // #3965: `#[serial]` + `$HOME` override — see `HomeGuard` above. This
    // case's crossed-workspace `continue` also short-circuits before
    // `validate_and_repair` runs today, but pin `$HOME` anyway for the same
    // future-proofing reason as the sibling test above.
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(PaneCwdTmux {
        alive: vec!["tm-crossed-01".into()],
        pane_cwd: Some(workspace.path().to_path_buf()),
    });

    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    let existing = super::record::SessionRecord {
        id: super::record::ManagedSessionId::new(),
        tmux_name: "tm-known-01".into(),
        cwd: workspace.path().to_path_buf(),
        task: "existing task".into(),
        state: ManagedSessionState::Stopped,
        created_at: chrono::Utc::now(),
        last_activity_at: None,
        workspace_path: Some(workspace.path().to_path_buf()),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
        worktree_owner: None,
        terminal_at: None,
        stop_cause: None,
    };
    let existing_id = existing.id;
    mgr.store.write().await.upsert(existing).await.unwrap();

    let report = mgr.reconcile_on_boot(false).await.expect("reconcile");

    assert!(
        !report
            .external_adopted
            .contains(&"tm-crossed-01".to_string()),
        "a live session resolving to an already-tracked workspace must never be \
         externally adopted; report: {report:?}"
    );

    let listed = mgr.list().await;
    assert_eq!(
        listed.len(),
        1,
        "no second record must be minted for the same workspace; records: {listed:?}"
    );
    assert_eq!(
        listed[0].id, existing_id,
        "the original record must be the only one"
    );
    assert!(
        !listed.iter().any(|r| r.tmux_name == "tm-crossed-01"),
        "no record must carry the crossed tmux_name"
    );
}

// ── #6117: one adopted record per tmux pane, however often adoption runs ────

/// The adopted id derives from the tmux name, so it is the same every time.
///
/// Why: this is the whole mechanism behind #6117. Two adopters that never saw
/// each other's write must still land on one store key.
/// Test: this function IS the test.
#[test]
fn adopted_id_is_stable_for_one_tmux_name() {
    let a = super::record::ManagedSessionId::for_adopted_tmux_name("tm-stable-01");
    let b = super::record::ManagedSessionId::for_adopted_tmux_name("tm-stable-01");
    assert_eq!(a, b, "the same tmux name must derive the same id");
}

/// Two different panes still get two different ids.
///
/// Why: a name-derived id would be worthless if it collapsed distinct panes.
/// Test: this function IS the test.
#[test]
fn adopted_id_differs_per_tmux_name() {
    let a = super::record::ManagedSessionId::for_adopted_tmux_name("tm-one-01");
    let b = super::record::ManagedSessionId::for_adopted_tmux_name("tm-two-01");
    assert_ne!(a, b, "distinct tmux names must derive distinct ids");
}

/// #6117 regression: an adopter that never saw the first adopter's record
/// targets the SAME store key.
///
/// Why: `reconcile_on_boot` builds its known-names set once, before the adopt
/// loop, and a concurrent daemon holds its own copy. Both then see the pane as
/// unknown, and the store is keyed by id — so a random id per adoption meant
/// both writes survived. The reporting store carried 11 such pairs, several
/// written 30-60 ms apart. A single-threaded test cannot stage that
/// interleaving through the public API, so it asserts the property that makes
/// the interleaving harmless: whichever adopter writes, and in whatever order,
/// the record identity for one tmux name is the same one. Pre-fix the second
/// adoption returns a fresh random id here, which is the assertion that fails.
/// What: adopts `tm-raced-01`, removes the record so the next pass is as blind
/// as a concurrent daemon's snapshot, adopts again, and compares ids.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn adopting_the_same_pane_twice_writes_one_record() {
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(PaneCwdTmux {
        alive: vec!["tm-raced-01".into()],
        pane_cwd: Some(workspace.path().to_path_buf()),
    });

    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    mgr.reconcile_on_boot(false).await.expect("first reconcile");
    let first = mgr.list().await;
    assert_eq!(first.len(), 1, "first adoption wrote one record");

    // Blind the second pass exactly as a concurrent adopter's stale snapshot
    // would: it does not know this pane is already tracked.
    mgr.store
        .write()
        .await
        .remove(&first[0].id)
        .await
        .expect("drop the record the second adopter never saw");

    mgr.reconcile_on_boot(false)
        .await
        .expect("second reconcile");

    let after = mgr.list().await;
    assert_eq!(after.len(), 1, "one pane, one record: {after:?}");
    assert_eq!(
        after[0].id, first[0].id,
        "re-adopting one tmux name must land on the same store key, so two \
         concurrent adopters overwrite each other instead of both surviving"
    );
}

/// #6117 review: a reused tmux name derives a reused id, so the record written
/// under it must carry nothing from the pane that held the name before.
///
/// Why: a name-derived id is only safe if the id is not a handle onto state
/// that outlives the record. Adoption can reach a previously-used id only after
/// the earlier record LEFT the store — a terminal tombstone keeps its name in
/// the known-name set and blocks re-adoption outright, so the reachable case is
/// removal (retention, `tm sessions prune`, a manual delete). `decommission`
/// clears no PID-registry entry and no correlation, so this pins what a
/// re-adopted record actually sees.
/// What: adopts `tm-recycled-01`, loads it with per-id residue (correlation,
/// deliverable, claude session, scrollback, activity), registers a PID under
/// its derived id, removes the record, then re-adopts the SAME name backed by a
/// different pane and workspace. Asserts the id is reused, every residual field
/// is clear, the record tracks the NEW pane, and that adoption neither reads nor
/// writes the PID registry — `SessionRecord` carries no PID field, so a stale
/// entry cannot reach it.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn a_reused_adopted_id_carries_no_residual_state() {
    let dir = TempDir::new().unwrap();
    let first_ws = TempDir::new().unwrap();
    let second_ws = TempDir::new().unwrap();
    let _home = set_home(dir.path());

    let first_pane = std::sync::Arc::new(FlakyPaneCwdTmux {
        alive: vec!["tm-recycled-01".into()],
        pane_cwd: Some(first_ws.path().to_path_buf()),
        flaky_for: 0,
        calls: std::sync::atomic::AtomicUsize::new(0),
        pane_id: Some("%11".into()),
    });
    let mgr = SessionManager::new(dir.path(), first_pane).await.unwrap();
    mgr.reconcile_on_boot(false).await.expect("first reconcile");

    let listed = mgr.list().await;
    assert_eq!(listed.len(), 1);
    let reused_id = listed[0].id;
    assert_eq!(
        reused_id,
        super::record::ManagedSessionId::for_adopted_tmux_name("tm-recycled-01")
    );

    // Load the record with every per-id field a later adoption could inherit.
    {
        let mut store = mgr.store.write().await;
        let mut rec = store.get(&reused_id).await.expect("get adopted");
        rec.deliverable_id = Some(crate::deliverable::record::DeliverableId::new());
        rec.claude_session_id = Some("claude-abc".into());
        rec.scrollback_path = Some(dir.path().join("scrollback.log"));
        rec.last_activity_at = Some(chrono::Utc::now());
        rec.correlation.branch = Some("feature/old".into());
        store.upsert(rec).await.expect("seed residue");
    }

    // A PID recorded under the derived id, the residue `decommission` never
    // clears. It must stay inert, not become the new record's.
    let pids = crate::core::pid_registry::PidRegistry::new(dir.path().join("pids"));
    pids.register(&reused_id.to_string(), 4242)
        .expect("register pid");

    // Removal is the only path that frees the name for re-adoption.
    mgr.store
        .write()
        .await
        .remove(&reused_id)
        .await
        .expect("remove record");

    // A DIFFERENT physical pane now answers to the same tmux name.
    let second_pane = std::sync::Arc::new(FlakyPaneCwdTmux {
        alive: vec!["tm-recycled-01".into()],
        pane_cwd: Some(second_ws.path().to_path_buf()),
        flaky_for: 0,
        calls: std::sync::atomic::AtomicUsize::new(0),
        pane_id: Some("%22".into()),
    });
    let mgr2 = SessionManager::new(dir.path(), second_pane).await.unwrap();
    mgr2.reconcile_on_boot(false)
        .await
        .expect("second reconcile");

    let after = mgr2.list().await;
    assert_eq!(after.len(), 1, "one record for one pane: {after:?}");
    let fresh = &after[0];
    assert_eq!(fresh.id, reused_id, "the derived id is reused by design");
    assert_eq!(
        fresh.pane_id.as_deref(),
        Some("%22"),
        "the record must track the NEW pane, not the one that held the name"
    );
    assert_eq!(fresh.workspace_path.as_deref(), Some(second_ws.path()));
    assert_eq!(
        fresh.deliverable_id, None,
        "deliverable link must not carry over"
    );
    assert_eq!(
        fresh.claude_session_id, None,
        "claude session must not carry over"
    );
    assert_eq!(
        fresh.scrollback_path, None,
        "scrollback must not carry over"
    );
    assert_eq!(fresh.last_activity_at, None, "activity must not carry over");
    assert_eq!(
        fresh.correlation,
        Default::default(),
        "correlation must not carry over: {:?}",
        fresh.correlation
    );

    // The stale PID entry is untouched by adoption — neither consumed nor
    // cleared. `SessionRecord` has no PID field, so it cannot reach the record;
    // reaping it belongs to the PID sweep, not here.
    let entries = pids.entries().expect("read pid registry");
    assert_eq!(entries.len(), 1, "adoption must not write PID entries");
    assert_eq!(entries[0].session_id, reused_id.to_string());
    assert_eq!(entries[0].pid, 4242);
}

/// Requirement 3 of the fix: nothing changes for a resolvable pane.
///
/// Why: the #6118 decline and the #6117 id change both sit in the adopt loop,
/// so the ordinary path needs a guard that says it still behaves as before —
/// one record, `Active`, real cwd and workspace, and stable across passes.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn repeated_reconcile_of_one_resolvable_pane_keeps_one_record() {
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let _home = set_home(dir.path());
    let fake = std::sync::Arc::new(PaneCwdTmux {
        alive: vec!["tm-steady-01".into()],
        pane_cwd: Some(workspace.path().to_path_buf()),
    });

    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    for _ in 0..3 {
        mgr.reconcile_on_boot(false).await.expect("reconcile");
    }

    let listed = mgr.list().await;
    assert_eq!(listed.len(), 1, "three passes, one record: {listed:?}");
    assert_eq!(listed[0].tmux_name, "tm-steady-01");
    assert_eq!(listed[0].state, ManagedSessionState::Active);
    assert_eq!(listed[0].cwd, workspace.path());
    assert_eq!(listed[0].workspace_path.as_deref(), Some(workspace.path()));
    assert_eq!(listed[0].task, "adopted session");
}

// ── #3692: create-path final safety net + suffixed-length cap ───────────────

/// A pre-reserved name gone STALE (another session claimed it between
/// reservation and create) is auto-suffixed by `create_with_resolved_name`'s
/// final pre-tmux-create dedupe — never creating a second session under the
/// taken name (#3692 review HIGH-2).
///
/// Why: `create_with_reserved_name` trusts a name reserved BEFORE clone/
/// worktree provisioning ran; that window is the create path's documented
/// TOCTOU. Pre-fix, this scenario produced two live sessions sharing one
/// name — the literal #3692 defect, via the create path.
/// What: registers a live tmux session + Active record under `tm-stale-01`,
/// then calls `create_with_reserved_name` with that same (now stale) reserved
/// name and asserts the created session/record landed on `tm-stale-02`.
/// Test: this function IS the test.
#[tokio::test]
async fn create_with_reserved_name_suffixes_stale_reservation() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

    // The reservation went stale: someone else now LIVES at tm-stale-01.
    let first = mgr
        .create(
            "squatter".into(),
            Some(PathBuf::from("/tmp/wt1")),
            Some("stale".into()),
            None,
            None,
            None,
        )
        .await
        .expect("first create");
    assert_eq!(first.tmux_name, "tm-stale-01");

    let second = mgr
        .create_with_reserved_name(
            super::record::ManagedSessionId::new(),
            "tm-stale-01".into(),
            "stale reservation".into(),
            Some(PathBuf::from("/tmp/wt2")),
            None,
            None,
            None,
            Default::default(),
            false,
            false,
        )
        .await
        .expect("a stale reservation must auto-suffix, never collide");

    assert_eq!(
        second.tmux_name, "tm-stale-02",
        "the stale reserved name must be suffixed to the next free ordinal"
    );
    assert!(
        fake.session_exists("tm-stale-02"),
        "the tmux session must be created under the SUFFIXED name"
    );
    // The squatter is untouched.
    assert_eq!(
        mgr.get(&first.id).await.expect("get first").tmux_name,
        "tm-stale-01"
    );
}

/// `dedupe_name_against` never returns a name over the 64-char cap
/// `validate_session_name` enforces on the bare candidate (#3692 review LOW):
/// a suffix appended to a near-cap name must truncate-and-retry, not silently
/// overflow.
///
/// Test: this function IS the test.
#[tokio::test]
async fn dedupe_name_against_caps_suffixed_length_at_64() {
    let long = "x".repeat(64); // exactly at the validate_session_name cap
    let taken: std::collections::HashSet<String> = [long.clone()].into();

    let result = SessionManager::dedupe_name_against(&long, &taken);

    assert!(
        result.chars().count() <= 64,
        "suffixed result must stay within the 64-char cap, got {} chars: {result}",
        result.chars().count()
    );
    assert_ne!(result, long, "the taken candidate must still be suffixed");
}
