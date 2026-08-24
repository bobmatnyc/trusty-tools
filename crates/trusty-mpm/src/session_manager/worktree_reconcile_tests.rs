//! Tests for report-only worktree reconciliation (#4207 slice 3, #4288).
//!
//! Why: the claim this module makes is that worktree identity comes from GIT,
//! not from a path shape — and path shape misclassifies 29% of the worktrees on
//! the dogfood machine. A mocked git could not distinguish the two, so every
//! test here drives REAL `git worktree add` inside a `tempfile::TempDir`.
//! There is no env-var gate and no `#[ignore]`: if `git` is unavailable the
//! fixture panics and these tests FAIL, they do not skip.
//! What: the git-derived key and its four adversarial mutations, the
//! three-state classification (including both #4288 landmines), the
//! deepest-first ordering, and the proposed-adoption list.
//! Test: this file IS the test module.

use super::*;

use crate::session_manager::record::ManagedSessionState;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use std::process::Command;

/// Run `git -C <dir> <args>`, panicking with git's own stderr on failure.
///
/// Why: a fixture step that silently no-ops produces a test that passes for the
/// wrong reason. The mutations below (`worktree move`, `remote set-url`,
/// `remote remove`) are the whole point of the AC test — a skipped mutation
/// would leave every assertion trivially true.
fn git_ok(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("`git {}` could not be run: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`git {}` failed in {}: {}",
        args.join(" "),
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn canonical(p: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// A `SessionRecord` in `state`, optionally pointing at `workspace`.
///
/// Why: `SessionRecord` has 22 fields and every test here needs one or two;
/// building them inline would bury the two fields that actually matter.
fn record_at(state: ManagedSessionState, workspace: Option<PathBuf>) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "tm-reconcile-test".into(),
        cwd: PathBuf::from("/tmp"),
        task: "reconcile test".into(),
        state,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: workspace,
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
    }
}

/// Register `worktree` a SECOND time, in the `.base` clone's registry.
///
/// Why: git will not create this state through its own porcelain — a worktree
/// belongs to one repository — but the state is real and #4288 names it as
/// residual 3: a hand-copied administrative directory (`cp -r`) leaves two
/// registries listing one path. Forging the admin directory by hand is the only
/// way to reach it, and it is exactly what `cp -r` produces.
/// What: writes `<repo>/.base/worktrees/<dup>/{gitdir,commondir,HEAD}` pointing
/// at `worktree`, which is all `git worktree list` reads to enumerate an entry.
fn forge_duplicate_registration(repo: &std::path::Path, worktree: &std::path::Path) {
    let admin = repo.join(".base").join("worktrees").join("forged-dup");
    std::fs::create_dir_all(&admin).expect("create the forged admin dir");
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", worktree.join(".git").display()),
    )
    .expect("write gitdir");
    std::fs::write(admin.join("commondir"), "../..\n").expect("write commondir");
    let head = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("resolve HEAD");
    assert!(head.status.success(), "fixture: rev-parse HEAD failed");
    std::fs::write(admin.join("HEAD"), head.stdout).expect("write HEAD");
}

/// The entry for `path` in `report`, or a panic naming what WAS reported.
fn entry_for<'a>(report: &'a ReconcileReport, path: &std::path::Path) -> &'a ReconciledWorktree {
    let want = canonical(path);
    report
        .entries
        .iter()
        .find(|e| e.path == want)
        .unwrap_or_else(|| {
            let got: Vec<_> = report.entries.iter().map(|e| &e.path).collect();
            panic!(
                "{} must appear in the inventory; got {got:?}",
                want.display()
            )
        })
}

// ── the acceptance test (#4288) ──────────────────────────────────────────

/// Reconciliation keys on git's registry, and survives every mutation that
/// breaks the alternative anchors (#4288 acceptance criteria).
///
/// Why: #4288's whole resolution rests on one claim — that
/// `(git-common-dir, admin-dir name)` is the only anchor that survives a
/// directory move, a remote rename, and a remote removal, and that no
/// "workspace_path not in `git worktree list` => orphan" rule may exist. This
/// test is the mechanical form of that claim. Its fixture is the decisive
/// counterexample from the issue: two worktrees under the SAME path prefix
/// (`<proj>/.base/.worktrees/`), registered in two DIFFERENT registries.
///
/// It goes RED when: a path-shape rule maps `B` to `.base`; only one anchor is
/// probed so `B` is missing; the key is path-derived (the `root` failure #4269
/// names) so a move changes it; the key derives from `RepoIdentity`, the origin
/// URL, or `project_index_id` so a remote change moves it; or any rule treats a
/// `workspace_path` absent from `git worktree list` as an orphan.
#[test]
fn reconcile_keys_on_the_git_registry_not_the_path() {
    let fx = GitWorktreeFixture::new();

    // A is registered in the `.base` BARE clone's registry; B is registered in
    // the PROJECT checkout's registry — and both live under the identical
    // `<proj>/.base/.worktrees/` prefix.
    let a = fx.add_base_clone_worktree("A");
    let base_worktrees = fx.repo.join(".base").join(".worktrees");
    let b = fx.add_worktree_at(&base_worktrees, "B");

    let base_common = canonical(&fx.repo.join(".base"));
    let repo_common = canonical(&fx.repo.join(".git"));
    assert_ne!(
        base_common, repo_common,
        "the two registries must genuinely differ, or this fixture proves nothing"
    );

    // ── baseline ─────────────────────────────────────────────────────────
    let key_a = worktree_key(&a).expect("A must have a git-derived key");
    let key_b = worktree_key(&b).expect("B must have a git-derived key");
    assert_eq!(
        key_a.common_dir, base_common,
        "A is registered in the .base clone; its common-dir must be .base"
    );
    assert_eq!(
        key_b.common_dir, repo_common,
        "B lives under the .base/ prefix but is registered in the PROJECT \
         checkout — a path-shape rule gets this one wrong"
    );
    assert_eq!(key_a.admin_dir.as_deref(), Some("A"));
    assert_eq!(key_b.admin_dir.as_deref(), Some("B"));
    assert_ne!(key_a, key_b);

    let report = reconcile_worktrees(&fx.repos_root, &[], &[], Utc::now());
    let admitted: Vec<PathBuf> = report
        .entries
        .iter()
        .filter(|e| e.admission == Some(Admission::Admitted))
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(
        admitted.len(),
        2,
        "exactly A and B are reclaim candidates; got {admitted:?}"
    );
    assert!(admitted.contains(&canonical(&a)), "got {admitted:?}");
    assert!(admitted.contains(&canonical(&b)), "got {admitted:?}");
    assert_eq!(report.counts.admitted, 2);
    // In-scope item 2: the EXCLUDED set is reported too — here the project's
    // main checkout and the bare `.base` record, each with its own reason.
    assert_eq!(
        report.counts.excluded, 2,
        "the excluded set must be REPORTED, not silently dropped; got {:?}",
        report.entries
    );
    assert_eq!(
        entry_for(&report, &fx.repo).admission,
        Some(Admission::MainCheckout)
    );
    assert_eq!(
        entry_for(&report, &fx.repo.join(".base")).admission,
        Some(Admission::Bare)
    );

    // ── mutation 1: `git worktree move` ──────────────────────────────────
    let elsewhere = fx.repo.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("create move destination parent");
    let a2 = elsewhere.join("A2");
    git_ok(
        &fx.repo.join(".base"),
        &[
            "worktree",
            "move",
            a.to_str().expect("utf8 A"),
            a2.to_str().expect("utf8 A2"),
        ],
    );
    assert!(
        a2.is_dir() && !a.exists(),
        "the move must really have happened"
    );
    assert_eq!(
        worktree_key(&a2).expect("A2 must still have a key"),
        key_a,
        "the key must survive a worktree move — a path-derived key does not"
    );

    // ── mutation 2: origin rename, then origin removal ───────────────────
    git_ok(
        &fx.repo,
        &["remote", "set-url", "origin", "/nonexistent/renamed.git"],
    );
    assert_eq!(
        worktree_key(&b).expect("B keeps its key"),
        key_b,
        "renaming origin must not move the key (it moves RepoIdentity/origin-URL anchors)"
    );
    git_ok(&fx.repo, &["remote", "remove", "origin"]);
    assert_eq!(
        worktree_key(&b).expect("B keeps its key with no remote at all"),
        key_b,
        "removing origin must not move the key — the anchor that vanishes here \
         is the origin URL, which is why it is not the anchor"
    );

    // ── mutation 3: the `tm-trusty-tools-01` landmine ────────────────────
    // A plain directory inside a live checkout, named by a SessionRecord.
    let fake = fx.repo.join(".worktrees").join("fake");
    std::fs::create_dir_all(&fake).expect("create the fake worktree dir");
    std::fs::write(fake.join("f"), b"the only copy of somebody's work\n").expect("write file");
    assert!(
        worktree_key(&fake).is_none(),
        "a plain directory inside a checkout has NO git-derived key — its \
         `rev-parse --show-toplevel` resolves to the enclosing checkout"
    );

    let rec = record_at(ManagedSessionState::Active, Some(fake.clone()));
    let report = reconcile_worktrees(&fx.repos_root, std::slice::from_ref(&rec), &[], Utc::now());
    let fake_entry = entry_for(&report, &fake);
    assert_eq!(
        fake_entry.state,
        ReconcileState::Unknown,
        "reason was: {}",
        fake_entry.reason
    );
    assert!(
        fake_entry.admission.is_none(),
        "git registers no such worktree, so there is no admission verdict"
    );
    assert_eq!(report.counts.record_only, 1);
    let orphaned: Vec<PathBuf> = report
        .entries
        .iter()
        .filter(|e| e.state == ReconcileState::Orphaned)
        .map(|e| e.path.clone())
        .collect();
    assert!(
        !orphaned.contains(&canonical(&fake)),
        "a `workspace_path` absent from `git worktree list` must NEVER be an \
         orphan — that rule deletes files inside a live checkout; got {orphaned:?}"
    );
    assert!(fake.join("f").exists(), "reconciliation must write nothing");
}

// ── the key, in isolation ────────────────────────────────────────────────

/// The `show-toplevel` assertion is what rejects the landmine shape (#4288).
///
/// Why: without it, `rev-parse --git-common-dir` run inside an ordinary
/// subdirectory of a checkout happily answers with the ENCLOSING repository's
/// common dir, manufacturing a perfectly plausible key for a directory that is
/// not a worktree at all.
#[test]
fn key_is_none_for_a_plain_directory_inside_a_checkout() {
    let fx = GitWorktreeFixture::new();
    let plain = fx.repo.join("docs").join("notes");
    std::fs::create_dir_all(&plain).expect("create plain dir");
    assert!(worktree_key(&plain).is_none());
    // Control: a real worktree in the same repository DOES get a key, so the
    // `None` above cannot be blamed on the fixture or a missing git.
    let wt = fx.add_worktree("real-one");
    assert!(worktree_key(&wt).is_some());
}

/// Two registries listing one path keep BOTH verdicts, and neither is
/// reclaimable (#4382 review).
///
/// Why: the inventory grouped scanned rows into a map keyed on path alone, so a
/// second registry's claim silently overwrote the first — last writer wins, no
/// trace. A disagreement between two registries about one path is precisely the
/// state this slice exists to surface, so losing one side is the one outcome
/// that cannot be allowed. Both rows now survive, each naming the registry that
/// produced it, and the ambiguity resolves the same way an unanswerable probe
/// does: UNKNOWN, never ORPHANED.
///
/// Non-vacuity: `uncontested` is an otherwise identical worktree with an
/// identical session record, and it IS proposed for adoption — so the contested
/// path's absence from that list is attributable to the contest and nothing
/// else.
#[test]
fn two_registries_claiming_one_path_keep_both_verdicts() {
    let fx = GitWorktreeFixture::new();
    fx.add_base_clone_worktree("seed");
    let contested = fx.add_worktree("claimed-twice");
    forge_duplicate_registration(&fx.repo, &contested);
    let uncontested = fx.add_worktree("claimed-once");
    let records = vec![
        record_at(ManagedSessionState::Active, Some(contested.clone())),
        record_at(ManagedSessionState::Active, Some(uncontested.clone())),
    ];

    let report = reconcile_worktrees(&fx.repos_root, &records, &[], Utc::now());
    let want = canonical(&contested);
    let rows: Vec<&ReconciledWorktree> = report.entries.iter().filter(|e| e.path == want).collect();
    assert_eq!(
        rows.len(),
        2,
        "both registries' claims must survive; got {rows:?}"
    );

    let mut got_roots: Vec<PathBuf> = rows
        .iter()
        .filter_map(|r| r.registry_root.clone())
        .collect();
    got_roots.sort();
    let mut want_roots = vec![canonical(&fx.repo), canonical(&fx.repo.join(".base"))];
    want_roots.sort();
    assert_eq!(
        got_roots, want_roots,
        "each row must name the registry that produced it"
    );
    assert!(
        rows.iter()
            .all(|r| r.admission == Some(Admission::Admitted)),
        "both verdicts must be preserved, not collapsed; got {rows:?}"
    );
    assert!(
        rows.iter()
            .all(|r| r.contested && r.state == ReconcileState::Unknown),
        "an ambiguous path is never reclaimable; got {rows:?}"
    );
    assert!(rows.iter().all(|r| r.reason.contains("contested")));
    assert_eq!(report.counts.orphaned, 0);

    let proposed: Vec<PathBuf> = report
        .proposed_adoptions
        .iter()
        .map(|p| p.path.clone())
        .collect();
    assert_eq!(
        proposed,
        vec![canonical(&uncontested)],
        "the uncontested twin IS proposed, the contested path is not; got {proposed:?}"
    );
}

// ── three-state classification ───────────────────────────────────────────

/// A `Stopped` record's workspace is LIVE, never ORPHANED (#4288).
///
/// Why: session `2eb72dca-…` was measured RUNNING in tmux pane `%981` while
/// `sessions.json` recorded `state: "stopped"`, holding 12 modified tracked
/// files, 31 untracked files and 1 unpushed commit. A record's state is
/// bookkeeping, not a liveness signal. This is the reconcile-side sibling of
/// `prune_spares_a_stopped_records_workspace`; it goes RED the moment anyone
/// adds a state filter to the record scan in `reconcile_worktrees`.
#[test]
fn stopped_records_workspace_is_live_not_orphaned() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("stopped-but-running");
    // Make it otherwise fully reclaimable, so ONLY the record veto spares it.
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    let rec = record_at(ManagedSessionState::Stopped, Some(wt.clone()));

    let report = reconcile_worktrees(&fx.repos_root, std::slice::from_ref(&rec), &[], Utc::now());
    let entry = entry_for(&report, &wt);
    assert_eq!(
        entry.state,
        ReconcileState::Live,
        "reason: {}",
        entry.reason
    );
    assert!(entry.reason.contains("workspace_path"), "{}", entry.reason);
    assert_eq!(entry.sessions, vec![rec.id]);
    assert_eq!(
        report.counts.orphaned, 0,
        "nothing may be orphaned here; got {:?}",
        report.entries
    );
}

/// The control case: a clean, admitted worktree whose sentinel owner is
/// provably gone IS classified ORPHANED.
///
/// Why: without this, every other test in this file could pass by classifying
/// literally everything UNKNOWN — a report that is safe and useless.
#[test]
fn clean_ownerless_admitted_worktree_is_orphaned() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("genuinely-stale");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);

    let report = reconcile_worktrees(&fx.repos_root, &[], &[], Utc::now());
    let entry = entry_for(&report, &wt);
    assert_eq!(
        entry.state,
        ReconcileState::Orphaned,
        "reason: {}",
        entry.reason
    );
    assert_eq!(report.counts.orphaned, 1);
    assert!(wt.exists(), "report-only: nothing may be removed");
}

/// A live cwd at or under a worktree vetoes reclamation (#4288).
#[test]
fn an_observed_live_cwd_vetoes_reclamation() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("has-a-live-pane");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    let inside = canonical(&wt).join("crates");

    let report = reconcile_worktrees(&fx.repos_root, &[], &[inside], Utc::now());
    let entry = entry_for(&report, &wt);
    assert_eq!(
        entry.state,
        ReconcileState::Live,
        "reason: {}",
        entry.reason
    );
    assert_eq!(report.counts.orphaned, 0);
}

/// A sentinel naming an unregistered owner is LIVE while it is still young
/// (#3649 creation race), and ORPHANED only once it ages out.
///
/// Why: both sentinel write sites write the sentinel BEFORE the owning record
/// is persisted. Without the grace window a sweep landing in that gap reads a
/// brand-new worktree as ownerless.
#[test]
fn young_absent_sentinel_owner_is_live_not_orphaned() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("mid-provisioning");
    let payload = serde_json::to_vec(&super::super::worktree_ownership::WorktreeSentinel {
        owner_session_id: Some(ManagedSessionId::new()),
        created_at: Utc::now(),
        agent: None,
    })
    .expect("serialize sentinel");
    std::fs::write(
        wt.join(super::super::decommission::WORKTREE_SENTINEL_FILE),
        payload,
    )
    .expect("write sentinel");

    let now = Utc::now();
    let fresh = reconcile_worktrees(&fx.repos_root, &[], &[], now);
    assert_eq!(entry_for(&fresh, &wt).state, ReconcileState::Live);

    // Same sentinel, clock advanced past the grace window.
    let later = now + OWNERLESS_GRACE + chrono::Duration::minutes(1);
    let aged = reconcile_worktrees(&fx.repos_root, &[], &[], later);
    assert_eq!(entry_for(&aged, &wt).state, ReconcileState::Orphaned);
}

/// A worktree holding unsaved work is UNKNOWN, never ORPHANED (#4091 gate,
/// inherited unchanged).
#[test]
fn a_dirty_ownerless_worktree_is_unknown_not_orphaned() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("has-unsaved-work");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&wt);
    std::fs::write(wt.join("README.md"), "edited but never committed\n").expect("dirty it");

    let report = reconcile_worktrees(&fx.repos_root, &[], &[], Utc::now());
    let entry = entry_for(&report, &wt);
    assert_eq!(
        entry.state,
        ReconcileState::Unknown,
        "reason: {}",
        entry.reason
    );
    assert!(entry.dirty.is_some(), "the dirt reason must be reported");
    assert_eq!(report.counts.orphaned, 0);
}

/// A bare clone reports its `Bare` verdict, not an instruction to repair
/// something that is not broken (#4382 review).
///
/// Why: a bare repository HAS no working tree, so `rev-parse --show-toplevel`
/// exiting 128 is git answering correctly. The row was falling into the generic
/// no-key branch and printing "repair the registry (`git worktree repair`)"
/// while discarding the `Admission::Bare` verdict it already held. Every managed
/// project has a `.base` clone, so that wrong line would appear on one of the
/// most common rows in the report — and on the real machine, where ~95% of rows
/// are UNKNOWN, the reason line is the only text distinguishing them.
///
/// The same applies to a worktree whose directory is gone: git already said why
/// there is no key, and it is not a repair.
#[test]
fn a_bare_clone_reports_its_bare_verdict_not_a_repair_instruction() {
    let fx = GitWorktreeFixture::new();
    fx.add_base_clone_worktree("seed");
    let base = fx.repo.join(".base");
    let vanished = fx.add_worktree("directory-removed");
    std::fs::remove_dir_all(&vanished).expect("remove the worktree directory");

    let report = reconcile_worktrees(&fx.repos_root, &[], &[], Utc::now());

    let bare = entry_for(&report, &base);
    assert_eq!(
        bare.admission,
        Some(Admission::Bare),
        "the Bare verdict must survive, not be discarded"
    );
    assert!(
        bare.key.is_none(),
        "a bare repo has no working tree, so it has no key — that is the point"
    );
    assert_eq!(bare.state, ReconcileState::Unknown);
    assert!(
        bare.reason.contains("bare repository record"),
        "the reason must name the real verdict; got {}",
        bare.reason
    );
    assert!(
        !bare.reason.contains("worktree repair"),
        "a bare clone is not a broken registry; got {}",
        bare.reason
    );

    // The directory-is-gone row lands in the same branch for the same reason.
    let gone = report
        .entries
        .iter()
        .find(|e| e.path.ends_with("directory-removed"))
        .unwrap_or_else(|| panic!("the removed worktree must still be reported"));
    assert!(
        matches!(
            gone.admission,
            Some(Admission::Prunable | Admission::Unresolvable)
        ),
        "got {:?}",
        gone.admission
    );
    assert!(
        !gone.reason.contains("worktree repair"),
        "a directory git already reports as gone is not a repair job; got {}",
        gone.reason
    );
}

// ── the inventory shape ──────────────────────────────────────────────────

/// The inventory is the UNION of git's registries and `sessions.json`.
///
/// Why: on the dogfood machine only 3 of 118 registered worktrees have a
/// matching session record, and `sessions.json` holds 31 `workspace_path`s
/// pointing at things git does not list. Either source alone reports a
/// fictional picture.
#[test]
fn report_unions_registry_and_session_records() {
    let fx = GitWorktreeFixture::new();
    let registered = fx.add_worktree("registered-only");
    let record_only = fx.repo.join("not-a-worktree");
    std::fs::create_dir_all(&record_only).expect("create record-only dir");
    let rec = record_at(ManagedSessionState::Active, Some(record_only.clone()));

    let report = reconcile_worktrees(&fx.repos_root, std::slice::from_ref(&rec), &[], Utc::now());
    assert_eq!(
        entry_for(&report, &registered).admission,
        Some(Admission::Admitted)
    );
    assert!(entry_for(&report, &record_only).admission.is_none());
    assert_eq!(report.counts.admitted, 1);
    assert_eq!(report.counts.record_only, 1);
    assert_eq!(report.counts.excluded, 1, "the project's main checkout");
    assert_eq!(
        report.counts.total,
        report.counts.live + report.counts.orphaned + report.counts.unknown,
        "every row carries exactly one state"
    );
    assert_eq!(report.counts.total, 3);
}

/// A nested worktree precedes its container, and the container is flagged
/// (#4288 in-scope item 4).
///
/// Why: nine registered worktrees on the dogfood machine sit inside another
/// registered worktree. A parent-first order removes the parent's directory and
/// takes its children with it, leaving dangling registry entries behind.
#[test]
fn entries_are_ordered_deepest_first() {
    let fx = GitWorktreeFixture::new();
    let parent = fx.add_worktree("container");
    let child = fx.add_nested_worktree(&parent, ".claude/worktrees", "nested");

    let report = reconcile_worktrees(&fx.repos_root, &[], &[], Utc::now());
    let idx = |p: &std::path::Path| {
        let want = canonical(p);
        report
            .entries
            .iter()
            .position(|e| e.path == want)
            .unwrap_or_else(|| panic!("{} must be in the inventory", want.display()))
    };
    assert!(
        idx(&child) < idx(&parent),
        "the nested worktree must precede its container"
    );
    assert!(entry_for(&report, &parent).contains_other_entry);
    assert!(!entry_for(&report, &child).contains_other_entry);
}

// ── categories are descriptive only ──────────────────────────────────────

/// The descriptive category never gates the three-state verdict (#4288).
///
/// Why: the category is derived partly from path shape, and path shape
/// misclassifies 29% of registry membership. If a consumer ever promoted it to
/// a gate, the whole issue's finding would be re-introduced through the report.
/// The assertion runs in BOTH directions: two entries with DIFFERENT
/// categories reach the SAME state, and two entries with the SAME category
/// reach DIFFERENT states. Either alone could be satisfied by accident.
#[test]
fn categories_are_descriptive_and_do_not_gate_state() {
    let fx = GitWorktreeFixture::new();
    // Same evidence (none), different categories.
    let agent = fx.add_worktree_at(&fx.repo.join(".claude").join("worktrees"), "agent-x");
    let plain = fx.add_worktree("plain-no-sentinel");
    // Both carry owner EVIDENCE, so both are `SessionOwned` — but one is
    // reclaimable and one is vetoed.
    let stale = fx.add_worktree("sentinel-ownerless");
    GitWorktreeFixture::stamp_reclaimable_sentinel(&stale);
    let owned = fx.add_worktree("owned-one");
    let rec = record_at(ManagedSessionState::Active, Some(owned.clone()));

    let report = reconcile_worktrees(&fx.repos_root, std::slice::from_ref(&rec), &[], Utc::now());
    let category = |p: &std::path::Path| entry_for(&report, p).category;
    let state = |p: &std::path::Path| entry_for(&report, p).state;

    assert_eq!(category(&agent), WorktreeCategory::HarnessAgent);
    assert_eq!(category(&plain), WorktreeCategory::Unclassified);
    assert_eq!(category(&stale), WorktreeCategory::SessionOwned);
    assert_eq!(category(&owned), WorktreeCategory::SessionOwned);
    assert_eq!(category(&fx.repo), WorktreeCategory::ProjectCheckout);

    // Different categories, identical state — the label decided nothing.
    assert_eq!(state(&agent), ReconcileState::Unknown);
    assert_eq!(state(&plain), ReconcileState::Unknown);
    // Identical category, different states — the EVIDENCE decided, not the label.
    assert_eq!(state(&stale), ReconcileState::Orphaned);
    assert_eq!(state(&owned), ReconcileState::Live);
}

// ── proposed adoptions (names only, writes nothing) ──────────────────────

/// Adoptions are NAMED with their evidence, and nothing is written (#4288
/// item 3, open decision (b)).
#[test]
fn proposed_adoptions_name_owner_and_evidence() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("adoptable");
    let rec = record_at(ManagedSessionState::Active, Some(wt.clone()));

    let report = reconcile_worktrees(&fx.repos_root, std::slice::from_ref(&rec), &[], Utc::now());
    assert_eq!(
        report.proposed_adoptions.len(),
        1,
        "got {:?}",
        report.proposed_adoptions
    );
    let proposal = &report.proposed_adoptions[0];
    assert_eq!(proposal.path, canonical(&wt));
    assert_eq!(proposal.inferred_owner, rec.id);
    assert!(
        proposal.evidence.contains("workspace_path"),
        "the evidence must name the observation; got {}",
        proposal.evidence
    );
    assert!(
        !wt.join(crate::session_manager::decommission::WORKTREE_SENTINEL_FILE)
            .exists(),
        "naming an adoption must not WRITE a sentinel — this slice writes nothing"
    );
}

/// A worktree containing another inventory entry is never proposed for
/// adoption (#4288 in-scope item 4).
///
/// Why: adopting a container is how a later reclaim takes its children's
/// directories with it. The sibling worktree in this test has identical
/// evidence and IS proposed, so the refusal is attributable to the nesting and
/// nothing else.
#[test]
fn proposed_adoptions_refuse_a_worktree_containing_another_entry() {
    let fx = GitWorktreeFixture::new();
    let container = fx.add_worktree("has-a-nested-worktree");
    fx.add_nested_worktree(&container, ".claude/worktrees", "nested");
    let sibling = fx.add_worktree("has-nothing-nested");
    let container_rec = record_at(ManagedSessionState::Active, Some(container.clone()));
    let sibling_rec = record_at(ManagedSessionState::Active, Some(sibling.clone()));
    let records = vec![container_rec, sibling_rec];

    let report = reconcile_worktrees(&fx.repos_root, &records, &[], Utc::now());
    let proposed: Vec<PathBuf> = report
        .proposed_adoptions
        .iter()
        .map(|p| p.path.clone())
        .collect();
    assert_eq!(proposed, vec![canonical(&sibling)], "got {proposed:?}");
    assert!(entry_for(&report, &container).contains_other_entry);
}
