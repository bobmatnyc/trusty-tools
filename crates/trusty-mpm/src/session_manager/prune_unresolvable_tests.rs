//! Coverage for the `unresolvable` prune selector and the prune engine's
//! self-exclusion (#6118).
//!
//! Why: #6126 stopped MINTING adoption ghosts — records whose `cwd` is the
//! `/unknown` sentinel and whose `workspace_path` is unset — but nothing could
//! SELECT the ones already in the store. Measured on the reporting host: 23 such
//! records, every state filter selecting 0 of them, `prune-idle` skipping all 23
//! on an `errored` verdict, and the one command that did reach them (`--state
//! all --include-active`) also selecting 32 healthy live sessions including the
//! operator's own. Both halves of that need pinning: the selector must find the
//! ghosts, and it must not find anything else.
//!
//! What: the positive case (a live ghost pane IS selected, with no
//! `--include-active`), the negative case (a healthy Active session with a
//! resolvable workspace is NOT), the partial case (one resolvable coordinate is
//! enough to keep a record), dry-run parity for both this filter and the
//! ephemeral sweep, the invoking-session exclusion under the widest possible
//! filter, and the duplicate-pane behaviour the #6117 race produced.
//! Test: this file IS the test module; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;

use chrono::Utc;

use super::super::record::{
    ManagedSessionId, ManagedSessionState, SessionRecord, UNRESOLVED_PATH_SENTINEL,
};
use super::super::tests::{make_manager, seed_record};
use super::PruneFilter;

/// Seed one record with an explicit `cwd`/`workspace_path`/`tmux_name`, and a
/// LIVE tmux session backing it.
///
/// Why: [`super::super::tests::seed_record`] always writes a real on-disk `cwd`
/// and derives the tmux name from the id, so it cannot express either shape this
/// module needs — the `/unknown` ghost, or two records sharing one pane.
/// What: upserts the record and registers `tmux_name` on the fake driver, so
/// every record seeded here is genuinely live in tmux (which is what made the
/// real ghosts unreachable).
async fn seed_live(
    mgr: &super::SessionManager,
    id: ManagedSessionId,
    tmux_name: &str,
    cwd: PathBuf,
    workspace_path: Option<PathBuf>,
    pane_id: Option<&str>,
) {
    let record = SessionRecord {
        id,
        tmux_name: tmux_name.to_owned(),
        cwd,
        task: "adopted session (unmanaged — workspace path could not be resolved)".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path,
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
        pane_id: pane_id.map(str::to_owned),
        injection_status: Default::default(),
        worktree_owner: None,
        terminal_at: None,
        stop_cause: None,
    };
    mgr.tmux
        .create_session(tmux_name, "/tmp")
        .expect("seed: register live tmux session");
    mgr.store
        .write()
        .await
        .upsert(record)
        .await
        .expect("seed: persist record");
}

/// A ghost exactly as the pre-#6126 adopt path wrote it: `/unknown` cwd, no
/// workspace, `Active`, live pane.
async fn seed_ghost(mgr: &super::SessionManager, id: ManagedSessionId, tmux_name: &str) {
    seed_live(
        mgr,
        id,
        tmux_name,
        PathBuf::from(UNRESOLVED_PATH_SENTINEL),
        None,
        Some("%1"),
    )
    .await;
}

/// The ids a prune outcome selected, sorted so comparisons are order-free.
fn selected(outcome: &super::PruneOutcome) -> Vec<String> {
    let mut ids: Vec<String> = outcome.sessions.iter().map(|s| s.id.clone()).collect();
    ids.sort();
    ids
}

/// ACCEPTANCE 2 (#6118): the ghost IS selected, and its live pane does not save
/// it from the one filter written for it.
///
/// Why: a live pane is the DEFINING property of this class — `is_running`
/// returning `true` is exactly what made all 23 records immune. A selector that
/// respected the liveness gate here would select nothing, which is the state
/// this issue describes.
/// What: seeds one ghost with a live tmux session, prunes `unresolvable` with
/// `include_active = false`, and asserts it was selected and tombstoned.
/// Test: this function IS the test.
#[tokio::test]
async fn unresolvable_filter_selects_a_live_ghost_pane() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let ghost = ManagedSessionId::new();
    seed_ghost(&mgr, ghost, "tm-ghost-live").await;

    // PREMISE: the pane really is live, so the liveness gate really is the thing
    // being bypassed. Without this the test could pass against a dead pane and
    // prove nothing about the class it exists for.
    assert!(
        super::is_running(
            &mgr.get(&ghost).await.expect("ghost record"),
            mgr.tmux.as_ref()
        )
        .expect("probe"),
        "PREMISE: the ghost's tmux session must be live"
    );

    let outcome = mgr
        .prune_managed(PruneFilter::Unresolvable, false, false, None, None)
        .await
        .expect("prune unresolvable");

    assert_eq!(
        selected(&outcome),
        vec![ghost.to_string()],
        "the live ghost must be selected with no --include-active"
    );
    assert_eq!(
        mgr.get(&ghost)
            .await
            .expect("record survives as tombstone")
            .state,
        ManagedSessionState::Decommissioned
    );
}

/// ACCEPTANCE 1 (#6118): a HEALTHY Active session with a resolvable workspace is
/// never selected.
///
/// Why: this is the whole difference between the new selector and the measured
/// `--state all --include-active` sweep, which took 32 healthy sessions along
/// with the 23 ghosts. Remove the staleness predicate from `matches_filter` —
/// make the `Unresolvable` arm `true`, as `All` is — and this test goes red on
/// the healthy session while the control below keeps proving the fixture is
/// otherwise selectable.
/// What: seeds one healthy Active session (real on-disk cwd and workspace) and
/// one ghost, prunes `unresolvable`, and asserts the selection is the ghost
/// alone and the healthy session is still `Active`.
/// Test: this function IS the test.
#[tokio::test]
async fn healthy_active_session_is_never_selected_by_the_unresolvable_filter() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let healthy = ManagedSessionId::new();
    seed_record(&mgr, &dir, healthy, ManagedSessionState::Active, false).await;
    // CONTROL: a ghost in the same store, so a selector that has stopped
    // selecting ANYTHING cannot pass this test by selecting nothing.
    let ghost = ManagedSessionId::new();
    seed_ghost(&mgr, ghost, "tm-ghost-beside-healthy").await;

    let outcome = mgr
        .prune_managed(PruneFilter::Unresolvable, false, false, None, None)
        .await
        .expect("prune unresolvable");

    assert_eq!(
        selected(&outcome),
        vec![ghost.to_string()],
        "CONTROL: the ghost must be selected and the healthy session must not"
    );
    assert_eq!(
        mgr.get(&healthy).await.expect("healthy record").state,
        ManagedSessionState::Active,
        "a healthy Active session must be untouched"
    );
}

/// One resolvable coordinate keeps the record (#6118).
///
/// Why: the predicate is deliberately an AND. A record whose `cwd` is the
/// sentinel but which still names a `workspace_path` has somewhere a caller can
/// go, so it is not this class — and a record naming a real directory on an
/// unmounted volume must never be reachable here at all, which is what keeps
/// this selector clear of the mass-tombstone hazard a probe-based rule carries.
/// What: seeds a record with `/unknown` cwd but a real workspace path, prunes
/// `unresolvable`, and asserts nothing was selected.
/// Test: this function IS the test.
#[tokio::test]
async fn unresolvable_filter_keeps_a_record_that_still_names_a_workspace() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;

    let half = ManagedSessionId::new();
    seed_live(
        &mgr,
        half,
        "tm-half-resolvable",
        PathBuf::from(UNRESOLVED_PATH_SENTINEL),
        Some(dir.path().join("still-here")),
        None,
    )
    .await;

    let outcome = mgr
        .prune_managed(PruneFilter::Unresolvable, false, false, None, None)
        .await
        .expect("prune unresolvable");
    assert!(
        outcome.sessions.is_empty(),
        "a record still naming a workspace is not unresolvable: {:?}",
        selected(&outcome)
    );
    assert_eq!(
        mgr.get(&half).await.expect("record").state,
        ManagedSessionState::Active
    );
}

/// `--include-active` neither enables nor widens this filter (#6118).
///
/// Why: the flag is a BLANKET lift of the liveness gate, and needing it would
/// mean typing the flag whose default behaviour is the footgun. Asserting the
/// two selections are IDENTICAL pins both halves at once: the filter works
/// without it, and passing it pulls in nothing extra.
/// What: seeds a ghost plus a healthy Active session, dry-runs `unresolvable`
/// with `include_active` false and then true, and compares selections.
/// Test: this function IS the test.
#[tokio::test]
async fn unresolvable_filter_needs_no_include_active() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let ghost = ManagedSessionId::new();
    seed_ghost(&mgr, ghost, "tm-ghost-flagless").await;
    seed_record(
        &mgr,
        &dir,
        ManagedSessionId::new(),
        ManagedSessionState::Active,
        false,
    )
    .await;

    let without = mgr
        .prune_managed(PruneFilter::Unresolvable, true, false, None, None)
        .await
        .expect("dry-run without the flag");
    let with = mgr
        .prune_managed(PruneFilter::Unresolvable, true, true, None, None)
        .await
        .expect("dry-run with the flag");

    assert_eq!(selected(&without), vec![ghost.to_string()]);
    assert_eq!(
        selected(&without),
        selected(&with),
        "--include-active must not change what `unresolvable` selects"
    );
}

/// ACCEPTANCE 3 (#6118): the dry run reports exactly what the real run does.
///
/// Why: a 2026-08-03 finding claimed prune's dry run misreported its outcome.
/// The class it named is fixed (`PruneAction::DecommissionedWorktreeRetained`,
/// pinned by `prune_reports_dirty_worktree_retained` and
/// `prune_dry_run_reports_without_mutating`), but nothing asserted the two
/// selections are the SAME selection — which is the property an operator relies
/// on when they preview a destructive sweep before running it.
/// What: dry-runs against the fixture, asserts nothing moved, then runs for real
/// against the SAME fixture and asserts an identical id set and identical
/// per-record actions.
/// Test: this function IS the test.
#[tokio::test]
async fn unresolvable_dry_run_selects_exactly_what_the_real_run_does() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let a = ManagedSessionId::new();
    let b = ManagedSessionId::new();
    seed_ghost(&mgr, a, "tm-ghost-parity-a").await;
    seed_ghost(&mgr, b, "tm-ghost-parity-b").await;
    seed_record(
        &mgr,
        &dir,
        ManagedSessionId::new(),
        ManagedSessionState::Active,
        false,
    )
    .await;

    let preview = mgr
        .prune_managed(PruneFilter::Unresolvable, true, false, None, None)
        .await
        .expect("dry run");
    assert!(
        preview.dry_run,
        "the outcome must report itself as a dry run"
    );
    // NON-VACUITY: parity between two EMPTY selections is not parity. Pin the
    // preview to both ghosts, so a selector that stops selecting anything fails
    // here instead of passing by symmetry.
    assert_eq!(
        selected(&preview).len(),
        2,
        "both ghosts must be previewed, or the comparison below proves nothing: {:?}",
        selected(&preview)
    );
    // A preview mutates nothing — otherwise the "same fixture" claim below is
    // false and the comparison is meaningless.
    for id in [a, b] {
        assert_eq!(
            mgr.get(&id).await.expect("record").state,
            ManagedSessionState::Active,
            "the dry run must not have touched {id}"
        );
    }

    let real = mgr
        .prune_managed(PruneFilter::Unresolvable, false, false, None, None)
        .await
        .expect("real run");

    assert_eq!(
        selected(&preview),
        selected(&real),
        "the preview and the real run must select the same records"
    );
    let actions = |o: &super::PruneOutcome| {
        let mut v: Vec<String> = o
            .sessions
            .iter()
            .map(|s| format!("{} {}", s.id, s.action.as_str()))
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        actions(&preview),
        actions(&real),
        "the preview must predict the same action per record"
    );
}

/// ACCEPTANCE 4 (#6118): `decommission-ephemeral`'s new preview selects exactly
/// what its real sweep does.
///
/// Why: the verb had no `--dry-run` at all, so its selection could not be
/// inspected before it ran. Routing both through one call differing in one
/// boolean is what makes them agree; this asserts they do, and that the preview
/// leaves the fleet alone.
/// What: seeds one ephemeral and one durable session, previews, asserts both
/// records survive and only the ephemeral was reported, then sweeps for real and
/// compares.
/// Test: this function IS the test.
#[tokio::test]
async fn ephemeral_dry_run_selects_exactly_what_the_real_run_does() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let eph = ManagedSessionId::new();
    let durable = ManagedSessionId::new();
    seed_record(&mgr, &dir, eph, ManagedSessionState::Active, true).await;
    seed_record(&mgr, &dir, durable, ManagedSessionState::Active, false).await;

    let preview = mgr.sweep_all_ephemeral(true).await.expect("preview");
    assert_eq!(selected(&preview), vec![eph.to_string()]);
    assert_eq!(
        mgr.get(&eph).await.expect("record").state,
        ManagedSessionState::Active,
        "the preview must not tear the ephemeral session down"
    );

    let real = mgr.sweep_all_ephemeral(false).await.expect("real sweep");
    assert_eq!(
        selected(&preview),
        selected(&real),
        "the ephemeral preview and sweep must select the same records"
    );
    assert_eq!(
        mgr.get(&durable).await.expect("record").state,
        ManagedSessionState::Active,
        "a durable session is unreachable by this path"
    );
}

/// 🔴 ACCEPTANCE 5 (#6118): no prune path selects the INVOKING session.
///
/// Why: the measured `--state all --include-active` sweep selected 55 sessions,
/// the operator's own current session among them. A tidy-up that takes down the
/// terminal it was typed in is an outage. This is asserted under the WIDEST
/// filter and the widest flag — `All` with `include_active` — because a guard
/// that only holds on the narrow selector protects nothing.
/// What: seeds two live Active sessions, prunes `All` with `include_active` and
/// `invoker = Some(mine)`, and asserts the peer was taken and the invoker was
/// not — including that the invoker's record is still `Active`.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_never_selects_the_invoking_session() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let mine = ManagedSessionId::new();
    let peer = ManagedSessionId::new();
    seed_record(&mgr, &dir, mine, ManagedSessionState::Active, false).await;
    seed_record(&mgr, &dir, peer, ManagedSessionState::Active, false).await;

    let outcome = mgr
        .prune_managed(PruneFilter::All, false, true, None, Some(mine))
        .await
        .expect("prune all");

    assert_eq!(
        selected(&outcome),
        vec![peer.to_string()],
        "CONTROL: the peer must be taken (so the sweep really ran) and the invoker spared"
    );
    assert_eq!(
        mgr.get(&mine).await.expect("invoker record").state,
        ManagedSessionState::Active,
        "the invoking session must still be running"
    );
}

/// ACCEPTANCE 6 (#6118): two records sharing one pane are BOTH tombstoned, and
/// the pane is reclaimed once.
///
/// Why: 3 of the 23 measured ghosts were double-registered under a second id by
/// the #6117 race, so the duplicate case is real data this selector will meet.
/// The behaviour is worth pinning either way it lands: `decommission`'s
/// `graceful_terminate_runtime` self-guards on `session_exists`, so the second
/// record's teardown finds the pane already gone and is a no-op rather than an
/// error — which is why both rows clear in one pass instead of one clearing and
/// one failing.
/// What: seeds two ghosts sharing one `tmux_name` and one `pane_id`, prunes
/// `unresolvable`, and asserts both are selected, both are tombstoned, and the
/// tmux session is gone.
/// Test: this function IS the test.
#[tokio::test]
async fn unresolvable_duplicates_sharing_a_pane_are_both_tombstoned() {
    let dir = crate::test_support::hermetic_temp_dir();
    let (mgr, _fake) = make_manager(&dir).await;
    let first = ManagedSessionId::new();
    let second = ManagedSessionId::new();
    seed_ghost(&mgr, first, "tm-ghost-dup").await;
    // The #6117 shape: a second id for the SAME pane and the same tmux name.
    seed_ghost(&mgr, second, "tm-ghost-dup").await;

    let outcome = mgr
        .prune_managed(PruneFilter::Unresolvable, false, false, None, None)
        .await
        .expect("prune unresolvable");

    let mut expected = vec![first.to_string(), second.to_string()];
    expected.sort();
    assert_eq!(
        selected(&outcome),
        expected,
        "both duplicate records must be selected in one pass"
    );
    for id in [first, second] {
        assert_eq!(
            mgr.get(&id).await.expect("record").state,
            ManagedSessionState::Decommissioned,
            "{id} must be tombstoned"
        );
    }
    assert!(
        !mgr.tmux
            .session_exists_checked("tm-ghost-dup")
            .expect("probe"),
        "the shared pane must be reclaimed"
    );
}

/// `PruneFilter::parse` accepts the new spelling and still rejects garbage
/// (#6118).
///
/// Why: `--state active` was rejected outright when an operator went looking for
/// this class, and the error message is the only place the supported values are
/// listed. A new variant that parses but is absent from that message is a
/// selector nobody can find.
/// Test: this function IS the test.
#[test]
fn unresolvable_filter_parses_and_is_listed_in_the_error() {
    assert_eq!(
        PruneFilter::parse("unresolvable").expect("parses"),
        PruneFilter::Unresolvable
    );
    assert_eq!(PruneFilter::Unresolvable.as_str(), "unresolvable");
    let err = PruneFilter::parse("active").expect_err("`active` is still not a filter");
    assert!(
        err.contains("unresolvable"),
        "the rejection must name the new filter: {err}"
    );
    assert!(
        PruneFilter::Unresolvable.selects_regardless_of_liveness(),
        "this is the one filter that ignores liveness"
    );
    for f in [
        PruneFilter::Ephemeral,
        PruneFilter::Stopped,
        PruneFilter::Decommissioned,
        PruneFilter::Deleted,
        PruneFilter::All,
    ] {
        assert!(
            !f.selects_regardless_of_liveness(),
            "{f} must keep its liveness gate"
        );
    }
}
