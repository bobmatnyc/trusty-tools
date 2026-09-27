//! Tests for the #7771 session-safe reclaim rule. Real git fixtures where the
//! answer comes from git; injected probes for the arms no live process can be
//! made to produce.

use std::path::{Path, PathBuf};

use super::*;
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;
use crate::session_manager::worktree_reclaim_claim::ClaimLiveness;
use crate::session_manager::worktree_reclaim_ownership::SessionOwners;

const REASON: &str = "claude agent agent-a1 (pid 82231 start Thu Sep 24 02:08:36 2026)";
const RECORDED: i64 = 1_790_215_716; // 2026-09-24T02:08:36Z

fn agent_tree(fx: &GitWorktreeFixture, name: &str) -> PathBuf {
    fx.add_worktree_at(&fx.repo.join(".claude").join("worktrees"), name)
}

fn a_dead_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn `true`");
    let pid = child.id();
    child.wait().expect("reap `true`");
    pid
}

fn no_agent(_: &AgentWorktreeOwner) -> AgentDelegationState {
    AgentDelegationState::Unknown
}

fn free_lock(_: &Path) -> LockLiveness {
    LockLiveness::Unlocked
}

fn nobody_inside(_: &Path) -> Option<String> {
    None
}

/// A gate whose every probe permits, with `session` answering (d).
fn gate<'a>(session: &'a dyn Fn(&str) -> SessionEnd) -> OwnerGate<'a> {
    OwnerGate {
        agent_state: &no_agent,
        session_end: session,
        lock: &free_lock,
        cwd_holder: &nobody_inside,
    }
}

#[test]
fn lock_start_reads_the_measured_reason_shape() {
    assert_eq!(parse_lock_start(REASON), Some(RECORDED));
    // A space-padded single-digit day, as ctime writes it.
    let padded = "claude agent agent-a (pid 1 start Mon Sep  1 20:33:51 2026)";
    assert!(parse_lock_start(padded).is_some(), "{padded}");
}

#[test]
fn lock_start_is_none_for_a_reason_without_one() {
    assert_eq!(parse_lock_start("claude agent agent-a (pid 17)"), None);
    assert_eq!(
        parse_lock_start("claude agent agent-a (pid 17 start soon)"),
        None
    );
}

#[test]
fn lock_start_round_trips_its_own_format() {
    let reason = format!(
        "claude agent x (pid 1 start {})",
        format_lock_start(RECORDED)
    );
    assert_eq!(parse_lock_start(&reason), Some(RECORDED));
}

#[test]
fn judge_lock_releases_a_dead_pid() {
    let v = judge_lock(REASON, &|_| Some(false), &|_| Err("unused".into()));
    assert!(matches!(v, LockLiveness::Stale(_)), "{v:?}");
    assert_eq!(v.refusal(), None);
}

#[test]
fn judge_lock_releases_a_reused_pid() {
    let v = judge_lock(REASON, &|_| Some(true), &|_| Ok(RECORDED + 3600));
    assert!(
        matches!(v, LockLiveness::Stale(ref why) if why.contains("reused")),
        "{v:?}"
    );
}

#[test]
fn judge_lock_holds_a_live_matching_pid() {
    let v = judge_lock(REASON, &|_| Some(true), &|_| Ok(RECORDED + 1));
    assert!(matches!(v, LockLiveness::Held(_)), "{v:?}");
    assert!(v.refusal().is_some());
}

/// Every probe failure keeps the tree (ADR-0045): an operator lock, a reason
/// with no pid, an unreadable process table, a reason with no start, and a
/// start time that cannot be read.
#[test]
fn judge_lock_refuses_every_probe_failure() {
    let running = |_: u32| Some(true);
    let at_start = |_: u32| Ok(RECORDED);
    type Alive<'a> = &'a dyn Fn(u32) -> Option<bool>;
    type Start<'a> = &'a dyn Fn(u32) -> Result<i64, String>;
    let cases: [(&str, Alive<'_>, Start<'_>); 5] = [
        ("do not touch", &running, &at_start),
        ("claude agent agent-a1 (started later)", &running, &at_start),
        (REASON, &|_| None, &at_start),
        ("claude agent agent-a1 (pid 82231)", &running, &at_start),
        (REASON, &running, &|_| Err("no entry".into())),
    ];
    for (reason, alive, start) in cases {
        let v = judge_lock(reason, alive, start);
        assert!(matches!(v, LockLiveness::Refused(_)), "{reason}: {v:?}");
    }
}

#[test]
fn lock_liveness_of_an_unlocked_tree_is_unlocked() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771open");
    assert_eq!(lock_liveness(&path), LockLiveness::Unlocked);
}

#[test]
fn lock_liveness_releases_a_real_lock_whose_pid_is_gone() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771dead");
    fx.harness_lock_worktree_with_pid(&path, "agent-7771dead", a_dead_pid());
    assert!(matches!(lock_liveness(&path), LockLiveness::Stale(_)));
}

#[test]
fn lock_liveness_holds_a_real_lock_naming_this_process() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771live");
    fx.harness_lock_worktree(&path, "agent-7771live");
    let v = lock_liveness(&path);
    assert!(matches!(v, LockLiveness::Held(_)), "{v:?}");
}

/// #7771 error arms: a path git does not list as a worktree, and a path git
/// cannot be asked about at all, are refused — never read as unlocked.
#[test]
fn lock_liveness_refuses_a_path_git_cannot_place() {
    let fx = GitWorktreeFixture::new();
    let inside = fx.repo.join("not-a-worktree");
    std::fs::create_dir_all(&inside).expect("mkdir");
    let v = lock_liveness(&inside);
    assert!(
        matches!(v, LockLiveness::Refused(ref why) if why.contains("does not list")),
        "{v:?}"
    );
    let outside = tempfile::tempdir().expect("tempdir");
    let v = lock_liveness(outside.path());
    assert!(matches!(v, LockLiveness::Refused(_)), "{v:?}");
    assert!(v.refusal().is_some());
}

#[test]
fn release_stale_lock_unlocks_a_dead_pids_lock() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771unlock");
    fx.harness_lock_worktree_with_pid(&path, "agent-7771unlock", a_dead_pid());
    release_stale_lock(&path).expect("a stale lock is released");
    assert_eq!(lock_liveness(&path), LockLiveness::Unlocked);
}

#[test]
fn release_stale_lock_refuses_a_live_holder() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771keep");
    fx.harness_lock_worktree(&path, "agent-7771keep");
    assert!(release_stale_lock(&path).is_err());
    assert!(matches!(lock_liveness(&path), LockLiveness::Held(_)));
}

#[test]
fn owner_refusal_reclaims_a_tree_with_no_owner_file() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "semver-accept-7771");
    let never = |_: &str| -> SessionEnd { panic!("no owner file: (d) is never asked") };
    assert_eq!(owner_refusal(&path, &gate(&never)), None);
}

#[test]
fn owner_refusal_keeps_a_tree_a_live_pid_locks() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771held");
    let ended = |_: &str| SessionEnd::Ended;
    let held = |_: &Path| LockLiveness::Held("pid 1 runs".into());
    let g = OwnerGate {
        lock: &held,
        ..gate(&ended)
    };
    assert!(owner_refusal(&path, &g).is_some_and(|r| r.contains("pid 1 runs")));
}

#[test]
fn owner_refusal_keeps_a_live_delegations_tree() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771busy");
    GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-7771busy");
    let ended = |_: &str| SessionEnd::Ended;
    let live = |_: &AgentWorktreeOwner| AgentDelegationState::Live;
    let g = OwnerGate {
        agent_state: &live,
        ..gate(&ended)
    };
    assert!(owner_refusal(&path, &g).is_some_and(|r| r.contains("agent-7771busy")));
}

#[test]
fn owner_refusal_keeps_another_live_sessions_tree() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771foreign");
    let owner = GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-7771foreign");
    let live = |_: &str| SessionEnd::Live;
    let why = owner_refusal(&path, &gate(&live)).expect("a live foreign session keeps it");
    assert!(
        why.contains(&owner.parent_session_id.0.to_string()),
        "{why}"
    );
}

#[test]
fn owner_refusal_permits_the_callers_own_agent_tree() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771mine");
    let owner = GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-7771mine");
    let me = owner.parent_session_id.0.to_string();
    let caller = |id: &str| {
        if id == me {
            SessionEnd::Caller
        } else {
            SessionEnd::Live
        }
    };
    assert_eq!(owner_refusal(&path, &gate(&caller)), None);
}

#[test]
fn owner_refusal_permits_an_ended_sessions_agent_tree() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771gone");
    GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-7771gone");
    let ended = |_: &str| SessionEnd::Ended;
    assert_eq!(owner_refusal(&path, &gate(&ended)), None);
}

#[test]
fn owner_refusal_keeps_a_tree_a_process_stands_in() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771cwd");
    let ended = |_: &str| SessionEnd::Ended;
    let failed = |_: &Path| Some("`lsof` exited 1 while checking for live processes".into());
    let g = OwnerGate {
        cwd_holder: &failed,
        ..gate(&ended)
    };
    assert!(owner_refusal(&path, &g).is_some_and(|r| r.contains("lsof")));
}

#[test]
fn owner_refusal_keeps_a_session_nothing_proves_ended() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771unproven");
    GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-7771unproven");
    let unknown = |_: &str| SessionEnd::Undeterminable("the tmux probe errored".into());
    assert!(owner_refusal(&path, &gate(&unknown)).is_some_and(|r| r.contains("tmux")));
}

/// #7771, #8511: a marker only in the git admin dir attributes the tree — a
/// live foreign session named there keeps it.
#[test]
fn owner_refusal_honours_a_marker_only_in_the_admin_dir() {
    use crate::session_manager::worktree_ownership_location as loc;
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771admin");
    let owner = GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-7771admin");
    assert!(
        !loc::legacy_sentinel_path(&path).exists(),
        "no in-tree marker"
    );
    assert!(
        loc::admin_sentinel_path(&path).is_some_and(|p| p.exists()),
        "the marker sits in the admin dir"
    );
    let live = |_: &str| SessionEnd::Live;
    let why = owner_refusal(&path, &gate(&live)).expect("the admin marker names a live owner");
    assert!(
        why.contains(&owner.parent_session_id.0.to_string()),
        "{why}"
    );
}

/// #8511: a tree not yet migrated keeps its in-tree marker, and that marker
/// still attributes the tree. The one test that writes the legacy path.
#[test]
fn owner_refusal_honours_a_pre_migration_in_tree_marker() {
    use crate::session_manager::worktree_ownership::WorktreeSentinel;
    use crate::session_manager::worktree_ownership_location as loc;
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771legacy");
    let owner = AgentWorktreeOwner {
        agent_id: "agent-7771legacy".to_string(),
        delegation_id: crate::core::agent::DelegationId::new(),
        parent_session_id: crate::core::session::SessionId::new(),
    };
    let bytes = serde_json::to_vec(&WorktreeSentinel::for_agent(owner.clone()))
        .expect("serialize the marker");
    std::fs::write(loc::legacy_sentinel_path(&path), bytes).expect("write the in-tree marker");
    assert!(
        !loc::admin_sentinel_path(&path).is_some_and(|p| p.exists()),
        "no admin marker"
    );
    let live = |_: &str| SessionEnd::Live;
    let why = owner_refusal(&path, &gate(&live)).expect("the in-tree marker names a live owner");
    assert!(
        why.contains(&owner.parent_session_id.0.to_string()),
        "{why}"
    );
}

/// #7771, #8511: a `.git` entry that does not resolve hides the admin dir, so
/// the owner cannot be read — the tree is kept even with every other probe
/// permitting.
///
/// Fails before the strict-reader switch: the tolerant reader answered "names
/// nobody" and the tree was permitted.
#[test]
fn owner_refusal_keeps_a_tree_whose_git_entry_does_not_resolve() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-7771dangling");
    GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-7771dangling");
    std::fs::write(
        path.join(".git"),
        "gitdir: /nonexistent/trusty-7771/admin\n",
    )
    .expect("point .git at nothing");
    let ended = |_: &str| SessionEnd::Ended;
    let why = owner_refusal(&path, &gate(&ended)).expect("an unresolvable .git keeps the tree");
    assert!(why.contains("cannot be read"), "{why}");
}

#[test]
fn session_end_resolves_a_claude_id_through_its_alias() {
    let owners = SessionOwners::observed([
        ("managed-a".to_string(), ClaimLiveness::SessionGone),
        ("managed-b".to_string(), ClaimLiveness::Live),
    ])
    .with_aliases([
        ("claude-a".to_string(), "managed-a".to_string()),
        ("claude-b".to_string(), "managed-b".to_string()),
    ])
    .with_caller(Some("managed-c".to_string()));
    assert_eq!(owners.session_end("claude-a"), SessionEnd::Ended);
    assert_eq!(owners.session_end("claude-b"), SessionEnd::Live);
    assert_eq!(owners.session_end("managed-c"), SessionEnd::Caller);
    assert!(matches!(
        owners.session_end("claude-z"),
        SessionEnd::Undeterminable(_)
    ));
}

/// #7771 critic: one Claude session resumed into a second managed record. A
/// dead record listed LAST must not outvote a live one; `Ended` needs every
/// record gone, and an unrecorded alias leaves the answer undeterminable.
///
/// Fails at 4b1f480af: the alias map kept the last pair, so `claude-r` was
/// `Ended` and a live parent's finished agent tree was reclaimable.
#[test]
fn session_end_keeps_a_claude_id_any_live_record_holds() {
    let owners = SessionOwners::observed([
        ("managed-live".to_string(), ClaimLiveness::Live),
        ("managed-gone".to_string(), ClaimLiveness::SessionGone),
        ("managed-gone-2".to_string(), ClaimLiveness::SessionGone),
    ])
    .with_aliases([
        ("claude-r".to_string(), "managed-live".to_string()),
        ("claude-r".to_string(), "managed-gone".to_string()),
        ("claude-e".to_string(), "managed-gone".to_string()),
        ("claude-e".to_string(), "managed-gone-2".to_string()),
        ("claude-u".to_string(), "managed-gone".to_string()),
        ("claude-u".to_string(), "managed-unrecorded".to_string()),
    ]);
    assert_eq!(owners.session_end("claude-r"), SessionEnd::Live);
    assert_eq!(owners.session_end("claude-e"), SessionEnd::Ended);
    assert!(matches!(
        owners.session_end("claude-u"),
        SessionEnd::Undeterminable(_)
    ));
}

/// #7771 critic: a Claude id aliased to the caller's own record AND to another
/// live record is `Live`, not `Caller` — the caller may not take a tree a
/// second live session still holds. Pins `Live` above `Caller` in `strictness`.
#[test]
fn session_end_ranks_a_live_alias_above_the_caller_alias() {
    let owners = SessionOwners::observed([
        ("managed-caller".to_string(), ClaimLiveness::Live),
        ("managed-other".to_string(), ClaimLiveness::Live),
    ])
    .with_aliases([
        ("claude-shared".to_string(), "managed-caller".to_string()),
        ("claude-shared".to_string(), "managed-other".to_string()),
    ])
    .with_caller(Some("managed-caller".to_string()));
    assert_eq!(owners.session_end("managed-caller"), SessionEnd::Caller);
    assert_eq!(owners.session_end("claude-shared"), SessionEnd::Live);
}

#[test]
fn session_end_refuses_an_unread_map() {
    let owners = SessionOwners::default();
    assert!(matches!(
        owners.session_end("anyone"),
        SessionEnd::Undeterminable(_)
    ));
}

/// #8301: the CLI names only its own session, so another session's agent tree
/// is kept by `tm pr merge`.
#[tokio::test]
async fn cli_tree_gate_keeps_another_sessions_agent_tree() {
    use crate::core::pr_cleanup::{CallerOwnership, ClaimOwnership};
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "agent-8301foreign");
    GitWorktreeFixture::stamp_agent_sentinel(&path, "agent-8301foreign");
    let why = CallerOwnership::new(vec!["my-session".to_string()])
        .tree_gate(&path)
        .await
        .expect_err("another session's tree is not this run's");
    assert!(why.contains("nothing proves"), "{why}");
}

#[test]
fn host_tree_gate_permits_an_unowned_unlocked_tree() {
    let fx = GitWorktreeFixture::new();
    let path = agent_tree(&fx, "handmade-8301");
    let never = |_: &str| SessionEnd::Undeterminable("unused".into());
    assert_eq!(
        crate::core::pr_cleanup::host_tree_gate(&path, &never),
        Ok(())
    );
}
