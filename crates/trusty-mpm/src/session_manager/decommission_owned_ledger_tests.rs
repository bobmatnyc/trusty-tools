//! Tests for the provisioning ledger, lock and `--force` gates on an
//! SM-owned workspace (#8663 critic round 1).
//!
//! Why: a plain decommission kept every freshly provisioned owned clone,
//! because tm's own `.gitignore`, `CLAUDE.md` and `.claude/settings.json`
//! writes read as dirt, and a locked owned worktree was removed under
//! `--force`. These tests drive real git and a real `SessionManager`.
//! What: a real clone provisioned through `snapshot` → tm's writes →
//! `record`, then `owned_workspace_keep_reason` and `decommission_with_root`.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::decommission_force::ProvisioningDirt;
use super::decommission_owned::{discarded_entries, owned_workspace_keep_reason};
use super::manager::SessionManager;
use super::provisioning_ledger::{LEDGER_NAME, record, snapshot};
use super::record::ManagedSessionId;
use super::tests::FakeTmuxDriver;
use super::worktree_git_fixture::GitWorktreeFixture;
use super::worktree_ownership::sentinel_payload_bytes;
use super::worktree_ownership_location::write_sentinel_bytes;

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

/// A real clone of the fixture repo, which tracks `.gitignore`, at
/// `<repos_root>/owner/clones/<name>`, clean and pushed.
fn clone(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    std::fs::write(fx.repo.join(".gitignore"), "target/\n").expect("write .gitignore");
    git(&fx.repo, &["add", ".gitignore"]);
    git(&fx.repo, &["commit", "-q", "-m", "track .gitignore"]);
    let ws = fx.repos_root.join("owner").join("clones").join(name);
    std::fs::create_dir_all(ws.parent().expect("parent")).expect("mkdir clones");
    let (src, dst) = (fx.repo.to_string_lossy(), ws.to_string_lossy());
    git(&fx.repos_root, &["clone", "-q", &src, &dst]);
    ws
}

/// Make tm's provisioning writes in `ws` the way a launch does: snapshot,
/// write `CLAUDE.md` and `.claude/settings.json`, append the scaffold block
/// to `.gitignore`, then record the ledger.
fn provision(ws: &Path) {
    let before = snapshot(ws);
    std::fs::write(ws.join("CLAUDE.md"), "# Project Instructions\n").expect("CLAUDE.md");
    std::fs::create_dir_all(ws.join(".claude")).expect("mkdir .claude");
    std::fs::write(ws.join(".claude/settings.json"), "{}\n").expect("settings.json");
    crate::core::scaffold_gitignore::ensure_scaffold_gitignored(ws).expect("scaffold block");
    assert!(record(ws, &before).expect("record the ledger"));
}

/// Decommission the owned `ws` through the plain route; `true` when removed.
async fn plain_decommission(managed_root: &Path, ws: &Path) -> bool {
    let store = crate::test_support::hermetic_temp_dir();
    let mgr = SessionManager::new(store.path(), FakeTmuxDriver::new())
        .await
        .expect("manager");
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "regression: #8663 ledger".into(),
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

/// The reason a plain decommission keeps `ws`.
fn kept(ws: &Path) -> String {
    owned_workspace_keep_reason(ws, &ManagedSessionId::new(), ProvisioningDirt::Refuse)
        .expect("the workspace is kept")
}

/// #8663: a clone holding only ledger-matching provisioning dirt is removed
/// by a plain decommission, and the ledger lives outside the work tree.
/// Fails at cc6cb57d0, which kept it for ` M .gitignore` and `?? CLAUDE.md`.
#[tokio::test]
async fn ledger_excuses_only_provisioning_dirt_on_a_clone() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "ledger-clean");
    provision(&ws);
    assert!(ws.join(".git").join(LEDGER_NAME).is_file(), "admin dir");

    assert_eq!(
        owned_workspace_keep_reason(&ws, &ManagedSessionId::new(), ProvisioningDirt::Refuse),
        None
    );
    assert!(plain_decommission(&fx.repos_root, &ws).await);
    assert!(!ws.exists(), "the provisioned clone is reclaimed");
}

/// #8663: an edit to `CLAUDE.md` after provisioning keeps the clone.
#[tokio::test]
async fn ledger_keeps_a_clone_with_an_edited_claude_md() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "ledger-edited");
    provision(&ws);
    std::fs::write(ws.join("CLAUDE.md"), "# Project Instructions\nmy notes\n").expect("edit");

    assert!(kept(&ws).contains("CLAUDE.md"), "{}", kept(&ws));
    assert!(!plain_decommission(&fx.repos_root, &ws).await);
    assert!(ws.join("CLAUDE.md").exists());
}

/// #8663: a `.gitignore` line tm did not append keeps the clone.
#[test]
fn ledger_keeps_a_clone_whose_gitignore_gained_a_user_line() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "ledger-gitignore");
    provision(&ws);
    let mut body = std::fs::read_to_string(ws.join(".gitignore")).expect("read");
    body.push_str("secrets.env\n");
    std::fs::write(ws.join(".gitignore"), body).expect("append");

    assert!(kept(&ws).contains(".gitignore"), "{}", kept(&ws));
}

/// #8663: with no ledger — a clone provisioned before it existed — nothing is
/// excused.
#[test]
fn ledger_missing_keeps_a_clone_with_provisioning_dirt() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "ledger-missing");
    provision(&ws);
    std::fs::remove_file(ws.join(".git").join(LEDGER_NAME)).expect("drop the ledger");

    assert!(kept(&ws).contains("CLAUDE.md"), "{}", kept(&ws));
}

/// #8663: a later launch that does not rewrite `CLAUDE.md` does not adopt an
/// edit someone made to it in between.
#[test]
fn ledger_does_not_carry_an_edit_made_between_launches() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "ledger-relaunch");
    provision(&ws);
    std::fs::write(ws.join("CLAUDE.md"), "# edited by an agent\n").expect("edit");
    let before = snapshot(&ws);
    assert!(record(&ws, &before).expect("second launch"));

    assert!(kept(&ws).contains("CLAUDE.md"), "{}", kept(&ws));
}

/// #8663: a `git worktree lock` keeps an owned worktree under both policies,
/// `--force` included. Fails at cc6cb57d0, which removed a clean locked tree.
#[test]
fn locked_owned_worktree_is_kept_even_with_force() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.add_worktree("owned-8663-locked");
    let me = ManagedSessionId::new();
    write_sentinel_bytes(&wt, &sentinel_payload_bytes(me)).expect("write the owner marker");
    fx.lock_worktree(&wt);

    for policy in [ProvisioningDirt::Refuse, ProvisioningDirt::Discard] {
        let reason = owned_workspace_keep_reason(&wt, &me, policy)
            .unwrap_or_else(|| panic!("a locked worktree is kept under {policy:?}"));
        assert!(reason.contains("locked"), "{reason}");
    }
}

/// #8663: the `--discard-dirty` log names every entry with its file count,
/// up to 50, then `+N more`.
#[test]
fn discarded_entries_names_every_entry_up_to_the_cap() {
    let entries: Vec<(String, usize)> = (0..52).map(|i| (format!("e{i}"), 1 + i % 2)).collect();
    let line = discarded_entries(&entries);
    assert!(line.starts_with("`e0` (1 file), `e1` (2 files)"), "{line}");
    assert!(line.contains("`e49` (2 files)"), "{line}");
    assert!(!line.contains("`e50`"), "{line}");
    assert!(line.ends_with("+2 more"), "{line}");
}
