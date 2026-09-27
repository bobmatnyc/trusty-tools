//! Tests for the prune-worktrees scope, preview rows, and allowlist (#8782).
//!
//! Every repository here is a `GitWorktreeFixture` inside its own temp dir; a
//! second project reaches the scan as an ADOPTED checkout, so no test reads the
//! operator's real workspace, registry, or daemon. Pull-request state comes from
//! an injected index, or from the real `PrIndex::from_gh`, which refuses before
//! spawning `gh` because no fixture carries a GitHub `origin`.

use std::path::{Path, PathBuf};

use super::{PruneScope, WorktreeScope};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_keep_list::KeepList;
use crate::session_manager::worktree_ownership::{AgentDelegationState, AgentWorktreeOwner};
use crate::session_manager::worktree_reclaim::{LiveClaims, PrIndex, ReclaimMode, ReclaimOutcome};
use crate::session_manager::worktree_reclaim_preview::preview_rows;
use crate::session_manager::worktree_reclaim_sweep::{FreshProbes, reclaim_scoped};
use crate::session_manager::worktree_registry::scan_registered_worktrees_in;

/// A merged pull request's end state: one commit, pushed.
fn land(wt: &Path) {
    std::fs::write(wt.join("landed.rs"), "// landed\n").expect("write landed file");
    GitWorktreeFixture::commit_all_and_push(wt, "landed");
}

/// An index naming every branch in `branches` as merged.
fn merged_index(branches: &[&str]) -> PrIndex {
    let rows: Vec<String> = branches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            format!(
                r#"{{"number": {}, "headRefName": "{b}", "state": "MERGED"}}"#,
                i + 1
            )
        })
        .collect();
    PrIndex::from_json(&format!("[{}]", rows.join(",")), 400)
}

fn no_agents(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

/// Run the reclaim loop over `fx` (plus `adopted`) under `scope`.
fn run(
    fx: &GitWorktreeFixture,
    adopted: &[PathBuf],
    index_for: &dyn Fn(&Path) -> PrIndex,
    mode: ReclaimMode,
    scope: &WorktreeScope,
) -> ReclaimOutcome {
    reclaim_scoped(
        &fx.repos_root,
        &FreshProbes {
            prove: &crate::session_manager::worktree_reclaim_landed::reclaim_landed_proof,
            launched_from: &[],
            keep_list: &KeepList::default,
            agent_state: &no_agents,
            in_use_now: &|| Some(LiveClaims::default()),
            index_for,
        },
        mode,
        adopted,
        scope,
    )
}

fn project(fx: &GitWorktreeFixture) -> WorktreeScope {
    WorktreeScope::from_request(fx.repo.to_str(), None).expect("fixture project resolves")
}

/// The scan admits only the scoped project's worktrees; the empty scope admits
/// both projects'.
#[test]
fn a_project_scope_admits_only_that_projects_worktrees() {
    let (a, b) = (GitWorktreeFixture::new(), GitWorktreeFixture::new());
    let wt_a = a.add_worktree("scope-a-8782");
    let wt_b = b.add_worktree("scope-b-8782");
    let adopted = vec![b.repo.clone()];
    let paths = |scope: &WorktreeScope| -> Vec<PathBuf> {
        scan_registered_worktrees_in(&a.repos_root, &adopted, scope)
            .into_iter()
            .map(|s| s.path)
            .collect()
    };
    let everything = paths(&WorktreeScope::all());
    assert!(
        everything.contains(&wt_a) && everything.contains(&wt_b),
        "{everything:?}"
    );
    let scoped = paths(&project(&a));
    assert!(scoped.contains(&wt_a), "{scoped:?}");
    assert!(
        !scoped.contains(&wt_b),
        "another project leaked into the scope: {scoped:?}"
    );
}

/// 🔴 #8782: a `--force` run scoped to one project leaves another project's
/// merged, clean worktree on disk.
///
/// Fails when `WorktreeScope::admits` ignores the project: the daemon-global
/// scan reaches the adopted project and removes its tree.
#[test]
fn a_scoped_force_run_leaves_another_projects_merged_worktree() {
    let (a, b) = (GitWorktreeFixture::new(), GitWorktreeFixture::new());
    let wt_a = a.add_worktree("force-a-8782");
    let wt_b = b.add_worktree("force-b-8782");
    land(&wt_a);
    land(&wt_b);
    let index = |_: &Path| merged_index(&["session/force-a-8782", "session/force-b-8782"]);
    let out = run(
        &a,
        std::slice::from_ref(&b.repo),
        &index,
        ReclaimMode::Remove,
        &project(&a),
    );
    assert_eq!(out.removed, vec![wt_a.clone()], "{out:?}");
    assert!(!wt_a.exists());
    assert!(
        wt_b.exists(),
        "a worktree outside the scope was removed: {out:?}"
    );
}

/// `only_paths` admits exactly the listed paths.
#[test]
fn an_allowlist_admits_only_listed_paths() {
    let fx = GitWorktreeFixture::new();
    let listed = fx.add_worktree("listed-8782");
    let other = fx.add_worktree("unlisted-8782");
    let only = vec![listed.to_string_lossy().into_owned()];
    let scope = WorktreeScope::from_request(None, Some(&only)).expect("scope");
    let scanned: Vec<PathBuf> = scan_registered_worktrees_in(&fx.repos_root, &[], &scope)
        .into_iter()
        .map(|s| s.path)
        .collect();
    assert_eq!(scanned, vec![listed], "only the listed path is in scope");
    assert!(!scanned.contains(&other));
}

/// 🔴 #8782: a `--force` run removes nothing its preview did not list, even a
/// tree that became reclaimable after the preview.
///
/// Fails when `WorktreeScope::admits` ignores `only`.
#[test]
fn a_force_run_removes_nothing_its_preview_did_not_list() {
    let fx = GitWorktreeFixture::new();
    let previewed = fx.add_worktree("previewed-8782");
    let later = fx.add_worktree("later-8782");
    land(&previewed);
    land(&later);
    let index = |_: &Path| merged_index(&["session/previewed-8782", "session/later-8782"]);
    let only = vec![previewed.to_string_lossy().into_owned()];
    let scope = WorktreeScope::from_request(fx.repo.to_str(), Some(&only)).expect("scope");
    let out = run(&fx, &[], &index, ReclaimMode::Remove, &scope);
    assert_eq!(out.removed, vec![previewed], "{out:?}");
    assert!(
        later.exists(),
        "a path the preview did not list was removed"
    );
}

/// 🔴 #8782 (acceptance 2 and 4): the preview lists every path the pass would
/// remove, `--force` with that list removes exactly those paths, and a merged
/// tree holding an uncommitted file or an unpushed commit is in neither.
#[test]
fn the_preview_lists_exactly_what_force_removes() {
    let fx = GitWorktreeFixture::new();
    let clean = fx.add_worktree("clean-8782");
    let dirty = fx.add_worktree("dirty-8782");
    let unpushed = fx.add_worktree("unpushed-8782");
    for wt in [&clean, &dirty, &unpushed] {
        land(wt);
    }
    std::fs::write(dirty.join("wip.rs"), "// uncommitted\n").expect("dirty the tree");
    GitWorktreeFixture::commit_unpushed(&unpushed);
    let index = |_: &Path| {
        merged_index(&[
            "session/clean-8782",
            "session/dirty-8782",
            "session/unpushed-8782",
        ])
    };
    let scope = project(&fx);

    let report = run(&fx, &[], &index, ReclaimMode::Report, &scope);
    let preview = preview_rows(&report.survey);
    let listed: Vec<String> = preview.reclaim.iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        listed,
        vec![clean.to_string_lossy().into_owned()],
        "{preview:?}"
    );
    assert!(
        preview
            .reclaim
            .iter()
            .all(|r| r.project == fx.repo.to_string_lossy() && r.reason.contains("PR #")),
        "every row names its project and reason: {preview:?}"
    );
    assert!(report.removed.is_empty(), "a preview removed something");

    let force = WorktreeScope::from_request(fx.repo.to_str(), Some(&listed)).expect("scope");
    let out = run(&fx, &[], &index, ReclaimMode::Remove, &force);
    let removed: Vec<String> = out
        .removed
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    assert_eq!(removed, listed, "--force removed a different set: {out:?}");
    assert!(dirty.exists(), "an uncommitted file was destroyed");
    assert!(unpushed.exists(), "an unpushed commit was destroyed");
}

/// 🔴 #8782 (acceptance 3): a repository with no `origin` remote is kept, and
/// the preview names it as UNKNOWN with the reason, never as reclaimable.
///
/// Uses the real `PrIndex::from_gh`, which refuses before spawning `gh` because
/// the repository to ask about cannot be established. Fails when
/// `worktree_reclaim_preview::is_unknown` returns false: the row is dropped
/// from the preview and the tree is reported nowhere.
#[test]
fn a_repository_without_origin_is_kept_and_reported_unknown() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("no-origin-8782");
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&fx.repo)
        .args(["remote", "remove", "origin"])
        .output()
        .expect("git runs");
    assert!(out.status.success(), "fixture: remove origin");
    let scope = project(&fx);

    let report = run(&fx, &[], &PrIndex::from_gh, ReclaimMode::Report, &scope);
    let preview = preview_rows(&report.survey);
    assert!(preview.reclaim.is_empty(), "{preview:?}");
    let unknown: Vec<&str> = preview.unknown.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(unknown, vec![wt.to_string_lossy()], "{preview:?}");
    assert!(preview.unknown[0].reason.contains("origin"), "{preview:?}");

    let removed = run(&fx, &[], &PrIndex::from_gh, ReclaimMode::Remove, &scope);
    assert!(removed.removed.is_empty(), "{removed:?}");
    assert!(wt.exists(), "a tree whose PR state is unknown was removed");
}

/// #8782: a failed pull-request lookup — here the fixture's `origin` is a
/// filesystem path no GitHub slug can be built from — is also UNKNOWN and kept.
#[test]
fn a_failed_pr_lookup_is_kept_and_reported_unknown() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("lookup-fails-8782");
    land(&wt);
    let scope = project(&fx);
    let report = run(&fx, &[], &PrIndex::from_gh, ReclaimMode::Report, &scope);
    let preview = preview_rows(&report.survey);
    assert!(preview.reclaim.is_empty(), "{preview:?}");
    assert_eq!(preview.unknown.len(), 1, "{preview:?}");
    let out = run(&fx, &[], &PrIndex::from_gh, ReclaimMode::Remove, &scope);
    assert!(out.removed.is_empty() && wt.exists(), "{out:?}");
}

/// The orphan sweep's scan is bounded by the same scope.
#[test]
fn an_orphan_sweep_scoped_to_one_project_spares_another() {
    let (a, b) = (GitWorktreeFixture::new(), GitWorktreeFixture::new());
    let wt_a = a.add_worktree("orphan-a-8782");
    let wt_b = b.add_worktree("orphan-b-8782");
    let adopted = vec![b.repo.clone()];
    let none = std::collections::HashSet::new();
    let found = crate::session_manager::prune::find_orphaned_worktrees_in(
        &a.repos_root,
        &none,
        &adopted,
        &project(&a),
    );
    assert!(found.paths.contains(&wt_a), "{found:?}");
    assert!(
        !found.paths.contains(&wt_b),
        "another project's tree is a candidate: {found:?}"
    );
    assert_eq!(
        found.registry_roots.get(&wt_a),
        Some(&a.repo),
        "the scan carries each candidate's registry: {found:?}"
    );
}

/// A project root that does not resolve is refused, never read as "no scope".
#[test]
fn a_project_root_that_does_not_resolve_is_refused() {
    let err = WorktreeScope::from_request(Some("/nonexistent/8782/project"), None)
        .expect_err("an unresolvable project must not widen to every project");
    assert!(err.contains("#8782"), "{err}");
}

#[test]
fn the_scope_echo_names_the_project_and_the_allowlist_size() {
    let fx = GitWorktreeFixture::new();
    let orphan = vec!["/a".to_string(), "/b".to_string()];
    let merged = vec!["/c".to_string()];
    let echo = PruneScope::from_request(fx.repo.to_str(), Some(&orphan), Some(&merged))
        .expect("scope")
        .echo(false);
    assert_eq!(echo["project_root"], fx.repo.to_string_lossy().as_ref());
    assert_eq!(echo["project_known"], false);
    assert_eq!(echo["only_orphan_paths"], 2);
    assert_eq!(echo["only_merged_paths"], 1);
    let all = PruneScope::default().echo(true);
    assert!(
        all["project_root"].is_null()
            && all["only_orphan_paths"].is_null()
            && all["only_merged_paths"].is_null(),
        "{all}"
    );
}

/// 🔴 #8782: each pass is bounded by what the preview listed for THAT pass.
///
/// Fails when both passes share one allowlist: the orphan scope then admits
/// the path the preview listed only for the merged-PR pass, and the reverse.
#[test]
fn a_per_pass_allowlist_bounds_each_pass_by_its_own_rows() {
    let fx = GitWorktreeFixture::new();
    let orphan = fx.add_worktree("orphan-only-8782");
    let merged = fx.add_worktree("merged-only-8782");
    let scope = PruneScope::from_request(
        fx.repo.to_str(),
        Some(&[orphan.to_string_lossy().into_owned()]),
        Some(&[merged.to_string_lossy().into_owned()]),
    )
    .expect("scope");
    let paths = |scope: &WorktreeScope| -> Vec<PathBuf> {
        scan_registered_worktrees_in(&fx.repos_root, &[], scope)
            .into_iter()
            .map(|s| s.path)
            .collect()
    };
    assert_eq!(paths(&scope.orphan), vec![orphan.clone()]);
    assert_eq!(paths(&scope.merged), vec![merged.clone()]);
}

/// #8782: a scoped run from a checkout the scan never reaches is reported as
/// such, so `total: 0` is not read as "nothing to prune".
#[test]
fn project_known_is_false_for_a_checkout_the_daemon_does_not_scan() {
    let (a, b) = (GitWorktreeFixture::new(), GitWorktreeFixture::new());
    assert!(
        project(&a).project_known(&a.repos_root, &[]),
        "a walk project"
    );
    assert!(
        !project(&b).project_known(&a.repos_root, &[]),
        "b is neither under a's repos root nor adopted"
    );
    assert!(
        project(&b).project_known(&a.repos_root, std::slice::from_ref(&b.repo)),
        "an adopted checkout is scanned"
    );
    assert!(WorktreeScope::all().project_known(&a.repos_root, &[]));
}

#[test]
fn project_root_for_a_linked_worktree_is_its_main_checkout() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("root-8782");
    assert_eq!(super::project_root_for(&wt), Some(fx.repo.clone()));
}

#[test]
fn orphan_rows_name_the_owning_checkout() {
    let wt = PathBuf::from("/r/a/.worktrees/row-8782");
    let outcome = crate::session_manager::prune::OrphanSweepOutcome {
        removed: vec![wt.clone()],
        registry_roots: [(wt.clone(), PathBuf::from("/r/a"))].into(),
        ..Default::default()
    };
    let rows = crate::daemon::managed_routes::prune::orphan_rows(&outcome);
    assert_eq!(rows[0]["path"], "/r/a/.worktrees/row-8782");
    assert_eq!(rows[0]["project"], "/r/a");
    assert!(
        rows[0]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("orphaned") && r.contains("no unsaved work"))
    );
}

/// 🔴 #8782: under `--discard-dirty` the preview names the unsaved work each
/// removal destroys, and never says "no unsaved work" for such a tree.
///
/// Fails when `orphan_rows` ignores `discarded_dirty`, or when the sweep stops
/// recording the force-discard verdict: the dirty row then reads "holds no
/// unsaved work". The `Skip` control keeps the dirty tree out of the rows.
#[tokio::test]
async fn the_orphan_preview_names_unsaved_work_that_discard_dirty_destroys() {
    use crate::session_manager::{DirtyWorktreePolicy, SessionManager};
    let store = tempfile::tempdir().expect("store dir");
    let mgr = SessionManager::new(
        store.path(),
        crate::session_manager::tests::FakeTmuxDriver::new(),
    )
    .await
    .expect("manager");
    let fx = GitWorktreeFixture::new();
    let (dirty, clean) = (fx.add_worktree("dirty-8782"), fx.add_worktree("clean-8782"));
    for wt in [&dirty, &clean] {
        GitWorktreeFixture::stamp_reclaimable_sentinel(wt);
    }
    std::fs::write(dirty.join("wip.rs"), "// uncommitted\n").expect("dirty the tree");
    let scope = project(&fx);
    let sweep =
        |policy| mgr.prune_orphaned_worktrees_in(&fx.repos_root, &[], true, policy, &[], &scope);
    let reason_of = |rows: &[serde_json::Value], wt: &Path| -> Option<String> {
        rows.iter()
            .find(|r| r["path"] == wt.to_string_lossy().as_ref())
            .and_then(|r| r["reason"].as_str().map(str::to_owned))
    };

    let forced = sweep(DirtyWorktreePolicy::ForceDiscard)
        .await
        .expect("sweep");
    let rows = crate::daemon::managed_routes::prune::orphan_rows(&forced);
    let dirty_reason = reason_of(&rows, &dirty).expect("the dirty tree is previewed");
    assert!(
        dirty_reason.contains("holds unsaved work")
            && dirty_reason.contains("discarded (--discard-dirty)")
            && !dirty_reason.contains("no unsaved work"),
        "{dirty_reason}"
    );
    let clean_reason = reason_of(&rows, &clean).expect("the clean tree is previewed");
    assert!(clean_reason.contains("no unsaved work"), "{clean_reason}");

    let kept = sweep(DirtyWorktreePolicy::Skip).await.expect("sweep");
    let rows = crate::daemon::managed_routes::prune::orphan_rows(&kept);
    assert!(reason_of(&rows, &dirty).is_none(), "{rows:?}");
    assert!(dirty.exists() && clean.exists(), "a dry run removed a tree");
}
