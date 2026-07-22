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
/// Test: this function IS the test.
#[tokio::test]
async fn manager_reconcile_adopts_new_prefix_session() {
    let dir = TempDir::new().unwrap();
    let fake = FakeTmuxDriver::new();
    fake.seeded_names
        .lock()
        .unwrap()
        .push("tm-trusty-tools-01".into());

    let mgr = SessionManager::new(dir.path(), fake.clone()).await.unwrap();

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
async fn reconcile_resolves_adopted_session_cwd_from_pane() {
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
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

/// Why (#2158): an adopted session whose pane cwd cannot be resolved (the
/// driver returns `None`, or resolves to a path that does not exist) must
/// keep the `/unknown` sentinel AND carry a task string that clearly flags it
/// as unmanaged — never an indistinguishable-from-normal "adopted session".
/// Test: this function IS the test.
#[tokio::test]
async fn reconcile_flags_unresolvable_adopted_session_as_unmanaged() {
    let dir = TempDir::new().unwrap();
    let fake = std::sync::Arc::new(PaneCwdTmux {
        alive: vec!["tm-unresolvable-01".into()],
        pane_cwd: None,
    });

    let mgr = SessionManager::new(dir.path(), fake).await.unwrap();
    mgr.reconcile_on_boot(false).await.expect("reconcile");

    let listed = mgr.list().await;
    let adopted = listed
        .iter()
        .find(|r| r.tmux_name == "tm-unresolvable-01")
        .expect("adopted record present");
    assert_eq!(adopted.cwd, PathBuf::from("/unknown"));
    assert!(adopted.workspace_path.is_none());
    assert!(
        adopted.task.contains("unmanaged"),
        "task must clearly flag the record as unmanaged, got: {}",
        adopted.task
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
async fn reconcile_skips_external_adopt_when_workspace_already_tracked() {
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
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
