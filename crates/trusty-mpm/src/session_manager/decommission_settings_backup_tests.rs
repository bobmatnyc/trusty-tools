//! Decommission's two delete routes and tm's gitignored settings backups
//! (#11267).
//!
//! Why: #11218 lets every route that calls `inspect_dirt_excusing` skip the
//! `.claude/settings.json.bak` and timestamped snapshots the provisioning
//! ledger vouches for byte for byte. Only `tm session delete --project-dir`
//! had tests for it; `decommission --force` (`decommission_force.rs`
//! `keep_reason`) and the owned-workspace route (`decommission_owned.rs`
//! `owned_workspace_keep_reason`) reach the same excusal untested.
//! What: a real worktree or clone whose tracked `.gitignore` ignores
//! `.claude/`, provisioned by the real launch preparation, so the backups reach
//! only the gitignored-file walk. A byte-identical tm copy is removed with the
//! tree; an edited copy, a user backup or an unusable ledger keeps the tree,
//! names the file and deletes nothing.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::decommission_force::{ProvisioningDirt, WorkspaceVerdict, remove_in_project_worktree};
use super::decommission_owned::owned_workspace_keep_reason;
use super::manager::SessionManager;
use super::provisioning_ledger::LEDGER_NAME;
use super::record::ManagedSessionId;
use super::worktree_git_fixture::GitWorktreeFixture;
use super::worktree_ownership_location::{admin_sentinel_path, write_sentinel_bytes};

/// tm's fixed-name settings backup.
const BAK: &str = ".claude/settings.json.bak";

/// Run `git -C <dir> <args>` and assert it succeeded.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The timestamped `settings.json` snapshots in `ws/.claude/`, sorted.
fn snapshots(ws: &Path) -> Vec<PathBuf> {
    use crate::core::standalone::hooks::backup::is_snapshot_of;
    let mut found: Vec<PathBuf> = std::fs::read_dir(ws.join(".claude"))
        .expect("list .claude")
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| is_snapshot_of("settings.json", n))
        })
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// Run the launch preparation a spawn runs on `ws`, under a hermetic home
/// that must outlive the test, and assert both backups are present and
/// gitignored. The hook exe is pinned so the hooks writer runs and snapshots
/// `settings.json` on a runner with no installed `tm`.
fn spawn(ws: &Path) -> tempfile::TempDir {
    let home = crate::test_support::hermetic_temp_dir();
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace_under(home.path(), ws);
    crate::core::session_launch::prepare_session_for_repair_in(
        &fw,
        ws,
        None,
        Some(Path::new(crate::test_support::STABLE_HOOK_EXE)),
        Some(home.path()),
        None,
    )
    .expect("the spawn preparation succeeds");
    assert!(ws.join(BAK).is_file(), "premise: the spawn wrote {BAK}");
    assert!(
        !snapshots(ws).is_empty(),
        "premise: the spawn wrote a timestamped settings snapshot"
    );
    let ignored = Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(["check-ignore", "-q", BAK])
        .status()
        .expect("git check-ignore runs");
    assert!(ignored.success(), "premise: {BAK} is gitignored");
    home
}

/// Every tm backup in `ws`: the fixed `.bak` and each snapshot.
fn backups(ws: &Path) -> Vec<PathBuf> {
    let mut all = snapshots(ws);
    all.push(ws.join(BAK));
    all
}

/// Append bytes tm never wrote to the file at `path`.
fn edit(path: &Path) {
    let mut bytes = std::fs::read(path).expect("read backup");
    bytes.extend_from_slice(b"\n// kept by hand\n");
    std::fs::write(path, bytes).expect("edit backup");
}

/// The gitignored files a kept `reason` lists. The reason's fixed text names
/// `.claude/settings.json.bak` too, so a bare `contains` proves nothing.
fn listed(reason: &str) -> Vec<String> {
    let (_, rest) = reason
        .split_once("no `git status` can show: ")
        .unwrap_or_else(|| panic!("the reason lists gitignored files: {reason}"));
    let list = rest.split_once(')').map_or(rest, |(list, _)| list);
    list.split(", ").map(str::to_string).collect()
}

/// The repo-relative form of each path under `ws`, sorted.
fn relative(ws: &Path, paths: &[PathBuf]) -> Vec<String> {
    let mut rel: Vec<String> = paths
        .iter()
        .map(|p| p.strip_prefix(ws).expect("under ws").display().to_string())
        .collect();
    rel.sort();
    rel
}

/// The provisioning ledger in `ws`'s git admin dir, which the spawn wrote.
fn ledger_path(ws: &Path) -> PathBuf {
    let admin = admin_sentinel_path(ws).expect("the admin dir resolves");
    let ledger = admin.parent().expect("admin dir").join(LEDGER_NAME);
    assert!(ledger.is_file(), "premise: the spawn recorded a ledger");
    ledger
}

/// Overwrite the provisioning ledger in `ws`'s git admin dir with non-JSON.
fn corrupt_ledger(ws: &Path) {
    std::fs::write(ledger_path(ws), "not json").expect("corrupt the ledger");
}

/// Drop only the backup records from `ws`'s ledger, as an older ledger has
/// none, so every other provisioning write stays vouched for.
fn forget_backups(ws: &Path) {
    let ledger = ledger_path(ws);
    let mut doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ledger).expect("read ledger")).expect("parse");
    let removed = doc["files"].as_object_mut().expect("files").remove(BAK);
    assert!(removed.is_some(), "premise: the ledger recorded {BAK}");
    doc.as_object_mut()
        .expect("ledger object")
        .remove("settings_snapshots");
    std::fs::write(&ledger, serde_json::to_vec(&doc).expect("encode")).expect("write");
}

// ---- `decommission --force` on an in-project worktree ----------------------

/// A pushed, tm-marked linked worktree whose tracked `.gitignore` ignores
/// `.claude/`, then provisioned by a spawn.
fn spawned_worktree(fx: &GitWorktreeFixture, name: &str) -> (PathBuf, tempfile::TempDir) {
    let wt = fx.add_worktree(name);
    std::fs::write(wt.join(".gitignore"), ".claude/\n").expect("write .gitignore");
    GitWorktreeFixture::commit_all_and_push(&wt, "ignore .claude");
    write_sentinel_bytes(&wt, b"").expect("write the admin-dir marker");
    let home = spawn(&wt);
    (wt, home)
}

/// `decommission --force` of `wt`, as a fresh session with no task.
async fn force(wt: &Path) -> WorkspaceVerdict {
    remove_in_project_worktree(
        &ManagedSessionId::new(),
        None,
        wt,
        ProvisioningDirt::Discard,
    )
    .await
}

/// #11267: tm's own byte-identical gitignored backups do not keep the tree.
#[tokio::test]
async fn force_decommission_removes_a_worktree_holding_only_tm_s_backups_11267() {
    let fx = GitWorktreeFixture::new();
    let (wt, _home) = spawned_worktree(&fx, "decom-force-bak-11267");

    let verdict = force(&wt).await;

    assert!(verdict.removed, "kept: {:?}", verdict.kept_reason);
    assert_eq!(verdict.kept_reason, None);
    assert!(!wt.exists(), "the worktree is reclaimed");
}

/// #11267 failure arm: an edited tm backup holds bytes tm never wrote, so
/// `--force` keeps the tree, says why, and deletes nothing.
#[tokio::test]
async fn force_decommission_keeps_an_edited_settings_backup_11267() {
    let fx = GitWorktreeFixture::new();
    let (wt, _home) = spawned_worktree(&fx, "decom-force-edited-bak-11267");
    edit(&wt.join(BAK));

    let verdict = force(&wt).await;

    assert!(!verdict.removed, "an edited backup must keep the tree");
    let reason = verdict.kept_reason.expect("a kept tree must say why");
    assert_eq!(listed(&reason), [BAK], "{reason}");
    assert!(wt.join(BAK).is_file(), "the edited backup survives");
}

/// #11267 failure arm: a user's backup beside tm's keeps the tree under
/// `--force`; tm's own backups are not named.
#[tokio::test]
async fn force_decommission_keeps_a_user_backup_beside_tm_s_11267() {
    let fx = GitWorktreeFixture::new();
    let (wt, _home) = spawned_worktree(&fx, "decom-force-user-bak-11267");
    let mine = ".claude/settings.json.20990101T000000Z.bak";
    std::fs::write(wt.join(mine), "{}\n").expect("write a user backup");

    let verdict = force(&wt).await;

    assert!(!verdict.removed, "a user backup must keep the tree");
    let reason = verdict.kept_reason.expect("a kept tree must say why");
    // Only the user's file is listed; tm's own backups stay excused.
    assert_eq!(listed(&reason), [mine], "{reason}");
    assert!(wt.join(mine).is_file(), "the user backup survives");
}

/// #11267 failure arm: a ledger with no record of the backups vouches for
/// none, so `--force` keeps the tree and lists exactly tm's backups.
#[tokio::test]
async fn force_decommission_keeps_backups_the_ledger_never_recorded_11267() {
    let fx = GitWorktreeFixture::new();
    let (wt, _home) = spawned_worktree(&fx, "decom-force-unrecorded-bak-11267");
    forget_backups(&wt);
    let all = backups(&wt);

    let verdict = force(&wt).await;

    assert!(!verdict.removed, "an unrecorded backup must keep the tree");
    let reason = verdict.kept_reason.expect("a kept tree must say why");
    let mut named = listed(&reason);
    named.sort();
    assert_eq!(named, relative(&wt, &all), "{reason}");
    for path in &all {
        assert!(path.is_file(), "{} survives", path.display());
    }
}

/// #11267 failure arm: a corrupt ledger vouches for no backup, so `--force`
/// keeps the tree and every backup survives.
#[tokio::test]
async fn force_decommission_keeps_tm_s_backups_when_the_ledger_is_corrupt_11267() {
    let fx = GitWorktreeFixture::new();
    let (wt, _home) = spawned_worktree(&fx, "decom-force-corrupt-ledger-11267");
    corrupt_ledger(&wt);

    let verdict = force(&wt).await;

    assert!(!verdict.removed, "no ledger vouches for nothing");
    assert!(verdict.kept_reason.is_some(), "a kept tree must say why");
    for path in backups(&wt) {
        assert!(path.is_file(), "{} survives", path.display());
    }
}

// ---- the SM-owned workspace route -------------------------------------------

/// A clean, pushed clone whose tracked `.gitignore` ignores `.claude/`, at
/// `<repos_root>/owner/clones/<name>`, then provisioned by a spawn.
fn spawned_clone(fx: &GitWorktreeFixture, name: &str) -> (PathBuf, tempfile::TempDir) {
    std::fs::write(fx.repo.join(".gitignore"), ".claude/\n").expect("write .gitignore");
    git(&fx.repo, &["add", ".gitignore"]);
    git(&fx.repo, &["commit", "-q", "-m", "ignore .claude"]);
    let ws = fx.repos_root.join("owner").join("clones").join(name);
    std::fs::create_dir_all(ws.parent().expect("parent")).expect("mkdir clones");
    let (src, dst) = (fx.repo.to_string_lossy(), ws.to_string_lossy());
    git(&fx.repos_root, &["clone", "-q", &src, &dst]);
    let home = spawn(&ws);
    (ws, home)
}

/// Why a plain (`Refuse`) or `--force` (`Discard`) decommission keeps `ws`.
fn keep(ws: &Path, policy: ProvisioningDirt) -> Option<String> {
    owned_workspace_keep_reason(ws, &ManagedSessionId::new(), None, policy)
}

/// Decommission the SM-owned `ws` through the plain route; `true` when removed.
async fn decommission_owned(managed_root: &Path, ws: &Path) -> bool {
    let store = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(
        store.path(),
        crate::session_manager::tests::fake_with_pane(),
    )
    .await
    .expect("manager");
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "regression: #11267 settings backups".into(),
            Some(ws.to_path_buf()),
            None,
            Some(ws.to_path_buf()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            true,
        )
        .await
        .expect("create");
    let (_, removed) = mgr
        .decommission_with_root(&record.id, managed_root, None)
        .await
        .expect("a refusal is not an error");
    removed
}

/// #11267: tm's own byte-identical gitignored backups keep the owned clone
/// under neither policy, and a plain decommission deletes it.
#[tokio::test]
async fn owned_decommission_removes_a_clone_holding_only_tm_s_backups_11267() {
    let fx = GitWorktreeFixture::new();
    let (ws, _home) = spawned_clone(&fx, "owned-bak-11267");

    assert_eq!(keep(&ws, ProvisioningDirt::Refuse), None);
    assert_eq!(keep(&ws, ProvisioningDirt::Discard), None);
    assert!(decommission_owned(&fx.repos_root, &ws).await);
    assert!(!ws.exists(), "the owned clone is reclaimed");
}

/// #11267 failure arm: an edited tm backup keeps the owned clone under both
/// policies, the reason names it, and a plain decommission deletes nothing.
#[tokio::test]
async fn owned_decommission_keeps_an_edited_settings_backup_11267() {
    let fx = GitWorktreeFixture::new();
    let (ws, _home) = spawned_clone(&fx, "owned-edited-bak-11267");
    let edited = backups(&ws);
    for path in &edited {
        edit(path);
    }

    for policy in [ProvisioningDirt::Refuse, ProvisioningDirt::Discard] {
        let reason = keep(&ws, policy).expect("an edited backup keeps the clone");
        let want = format!("{} gitignored file(s)", edited.len());
        assert!(reason.contains(&want), "{policy:?}: {reason}");
        let mut named = listed(&reason);
        named.sort();
        assert_eq!(named, relative(&ws, &edited), "{policy:?}: {reason}");
    }
    assert!(!decommission_owned(&fx.repos_root, &ws).await);
    for path in &edited {
        assert!(path.is_file(), "{} survives", path.display());
    }
}

/// #11267 failure arm: a user's backup beside tm's keeps the owned clone, and
/// only the user's file is named.
#[tokio::test]
async fn owned_decommission_keeps_a_user_backup_beside_tm_s_11267() {
    let fx = GitWorktreeFixture::new();
    let (ws, _home) = spawned_clone(&fx, "owned-user-bak-11267");
    let mine = ".claude/settings.json.20990101T000000Z.bak";
    std::fs::write(ws.join(mine), "{}\n").expect("write a user backup");

    for policy in [ProvisioningDirt::Refuse, ProvisioningDirt::Discard] {
        let reason = keep(&ws, policy).expect("a user backup keeps the clone");
        assert!(
            reason.contains("1 gitignored file(s)"),
            "{policy:?}: {reason}"
        );
        assert_eq!(listed(&reason), [mine], "{policy:?}: {reason}");
    }
    assert!(!decommission_owned(&fx.repos_root, &ws).await);
    assert!(ws.join(mine).is_file(), "the user backup survives");
}

/// #11267 failure arm: a ledger with no record of the backups vouches for
/// none, so the owned clone is kept under both policies and lists exactly
/// tm's backups.
#[tokio::test]
async fn owned_decommission_keeps_backups_the_ledger_never_recorded_11267() {
    let fx = GitWorktreeFixture::new();
    let (ws, _home) = spawned_clone(&fx, "owned-unrecorded-bak-11267");
    forget_backups(&ws);
    let all = backups(&ws);

    for policy in [ProvisioningDirt::Refuse, ProvisioningDirt::Discard] {
        let reason = keep(&ws, policy).expect("an unrecorded backup keeps the clone");
        let mut named = listed(&reason);
        named.sort();
        assert_eq!(named, relative(&ws, &all), "{policy:?}: {reason}");
    }
    assert!(!decommission_owned(&fx.repos_root, &ws).await);
    for path in &all {
        assert!(path.is_file(), "{} survives", path.display());
    }
}

/// #11267 failure arm: a corrupt ledger vouches for no backup, so the owned
/// clone is kept and every backup survives.
#[tokio::test]
async fn owned_decommission_keeps_tm_s_backups_when_the_ledger_is_corrupt_11267() {
    let fx = GitWorktreeFixture::new();
    let (ws, _home) = spawned_clone(&fx, "owned-corrupt-ledger-11267");
    corrupt_ledger(&ws);

    for policy in [ProvisioningDirt::Refuse, ProvisioningDirt::Discard] {
        assert!(keep(&ws, policy).is_some(), "{policy:?}: no ledger vouches");
    }
    assert!(!decommission_owned(&fx.repos_root, &ws).await);
    for path in backups(&ws) {
        assert!(path.is_file(), "{} survives", path.display());
    }
}
