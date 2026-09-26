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

use super::decommission_force::{ProvisioningDirt, worktree_kind};
use super::decommission_owned::{discarded_entries, owned_workspace_keep_reason};
use super::manager::SessionManager;
use super::provisioning_ledger::{LEDGER_NAME, load, record, snapshot};
use super::record::ManagedSessionId;
use super::tests::FakeTmuxDriver;
use super::worktree_git_fixture::GitWorktreeFixture;
use super::worktree_ownership::{AgentWorktreeOwner, sentinel_payload_bytes, write_agent_sentinel};
use super::worktree_ownership_location::write_sentinel_bytes;
use super::worktree_safety::is_worktree_root;
use crate::core::agent::DelegationId;
use crate::core::session::SessionId;

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

/// #8663 critic round 2: a linked worktree whose kind probe fails is kept
/// under both policies, not treated as "not linked". A newline in its path
/// splits `git rev-parse`'s one-path-per-line answer, so the probe cannot
/// parse it while `is_worktree_root` still says yes. Fails at 08fa38d77, which
/// skipped the lock gate and removed this clean, locked tree.
#[test]
fn owned_worktree_whose_probe_fails_is_kept() {
    let fx = GitWorktreeFixture::new();
    let wt = fx.repo.join(".worktrees").join("probe\n8663");
    let wt_arg = wt.to_str().expect("utf8 worktree path");
    git(
        &fx.repo,
        &["worktree", "add", "-q", "-b", "session/probe-8663", wt_arg],
    );
    fx.lock_worktree(&wt);
    let me = ManagedSessionId::new();
    write_sentinel_bytes(&wt, &sentinel_payload_bytes(me)).expect("write the owner marker");
    assert!(is_worktree_root(&wt).expect("git answers"), "precondition");
    let probe = worktree_kind(&wt).expect_err("precondition: the probe fails");

    for policy in [ProvisioningDirt::Refuse, ProvisioningDirt::Discard] {
        let reason = owned_workspace_keep_reason(&wt, &me, policy)
            .unwrap_or_else(|| panic!("a tree the probe cannot read is kept under {policy:?}"));
        assert!(reason.contains(&probe), "{reason}");
        assert!(reason.contains("nothing was removed"), "{reason}");
    }
}

/// #8663 critic round 2: `--force` on an owned worktree holding only
/// provisioning dirt is declined when its marker names another session or an
/// agent. Fails when the `--force declined` gate is removed, since the
/// provisioning excuse alone would clear the tree.
#[test]
fn force_on_owned_worktree_of_another_session_is_kept() {
    let fx = GitWorktreeFixture::new();
    let me = ManagedSessionId::new();
    let other = fx.add_worktree("owned-8663-other-session");
    write_sentinel_bytes(&other, &sentinel_payload_bytes(ManagedSessionId::new()))
        .expect("write another session's marker");
    let agent = fx.add_worktree("owned-8663-agent");
    let owner = AgentWorktreeOwner {
        agent_id: "agent-synthetic-8663".to_string(),
        delegation_id: DelegationId::new(),
        parent_session_id: SessionId::new(),
    };
    write_agent_sentinel(&agent, owner).expect("write the agent marker");

    for (wt, names) in [(&other, "names session"), (&agent, "agent-synthetic-8663")] {
        std::fs::write(wt.join("CLAUDE.md"), "# tm\n").expect("provisioning dirt");
        let reason = owned_workspace_keep_reason(wt, &me, ProvisioningDirt::Discard)
            .expect("--force is declined");
        assert!(reason.contains("--force declined"), "{reason}");
        assert!(reason.contains(names), "{reason}");
    }
}

/// #8663 critic round 2: a user edit to `settings.json` between launches,
/// which the next launch rewrites in place and backs up to
/// `settings.json.bak`, is ledgered under neither name. Fails at 08fa38d77,
/// which recorded both new hashes because they had changed.
#[test]
fn ledger_does_not_adopt_a_settings_edit_the_next_launch_rewrites() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "ledger-settings-edit");
    provision(&ws);
    let settings = ws.join(".claude/settings.json");
    std::fs::write(&settings, "{\"user\": true}\n").expect("user edit");

    let before = snapshot(&ws);
    std::fs::copy(&settings, ws.join(".claude/settings.json.bak")).expect("statusline backup");
    std::fs::write(&settings, "{\"tm\": 2}\n").expect("tm rewrite");
    assert!(record(&ws, &before).expect("second launch"));

    let ledger = load(&ws).expect("the ledger reads back");
    assert!(
        !ledger.files.contains_key(".claude/settings.json"),
        "{ledger:?}"
    );
    assert!(
        !ledger.files.contains_key(".claude/settings.json.bak"),
        "{ledger:?}"
    );
    assert!(kept(&ws).contains(".claude/settings.json"), "{}", kept(&ws));
}

/// #8663 critic round 2: a relaunch that rewrites tm's own ledgered bytes, or
/// a tracked `CLAUDE.md` still at its `HEAD` content, is ledgered, and the
/// clone is still reclaimed.
#[test]
fn ledger_adopts_a_relaunch_rewrite_of_tm_or_head_bytes() {
    let fx = GitWorktreeFixture::new();
    std::fs::write(fx.repo.join("CLAUDE.md"), "# committed\n").expect("CLAUDE.md");
    git(&fx.repo, &["add", "CLAUDE.md"]);
    git(&fx.repo, &["commit", "-q", "-m", "track CLAUDE.md"]);
    let ws = clone(&fx, "ledger-relaunch-tm");
    // The first launch overwrites the committed `CLAUDE.md`: the `HEAD` rule.
    provision(&ws);
    let settings = ws.join(".claude/settings.json");

    let before = snapshot(&ws);
    std::fs::copy(&settings, ws.join(".claude/settings.json.bak")).expect("statusline backup");
    std::fs::write(&settings, "{\"tm\": 2}\n").expect("tm rewrite");
    assert!(record(&ws, &before).expect("second launch"));

    let ledger = load(&ws).expect("the ledger reads back");
    for rel in [
        "CLAUDE.md",
        ".claude/settings.json",
        ".claude/settings.json.bak",
    ] {
        assert!(ledger.files.contains_key(rel), "{rel}: {ledger:?}");
    }
    assert_eq!(
        owned_workspace_keep_reason(&ws, &ManagedSessionId::new(), ProvisioningDirt::Refuse),
        None
    );
}

/// #8663 critic round 2: the resume path's hook merge rewrites
/// `settings.json` without recording the ledger, so the clone is kept. This
/// drives the real merge (`ensure_project_hooks_with`) over a ledgered clone.
#[tokio::test]
async fn resume_hook_merge_keeps_a_ledgered_clone() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "ledger-resume");
    provision(&ws);
    let settings = ws.join(".claude/settings.json");
    let launched = std::fs::read(&settings).expect("read settings");

    let base = tempfile::tempdir().expect("framework base");
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace_under(base.path(), &ws);
    let exe = Path::new(crate::test_support::STABLE_HOOK_EXE);
    crate::core::session_launch::ensure_project_hooks_with(&fw, &ws, Some(exe))
        .expect("the resume merge");
    assert_ne!(
        std::fs::read(&settings).expect("reread"),
        launched,
        "precondition"
    );

    assert!(kept(&ws).contains(".claude/settings.json"), "{}", kept(&ws));
    assert!(!plain_decommission(&fx.repos_root, &ws).await);
    assert!(settings.exists());
}
