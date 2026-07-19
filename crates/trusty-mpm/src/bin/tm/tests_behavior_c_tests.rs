//! Unit tests for the guided-default session-picker UX (#1705).
//!
//! Why: the guided-default must correctly detect GitHub projects, show a
//! readable session list, gracefully degrade for non-TTY callers, and
//! correctly derive the managed workspace path. These properties can be
//! checked without a live daemon.
//! What: tests for `derive_project`, `parse_picker_choice`, `tty_gate`,
//! `print_project_context`, `print_non_tty_hint`, and the fallback path.
//! Test: `cargo test -p trusty-mpm -- tests_behavior_c` runs this suite;
//! no network or tmux required.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::commands::first_run::needs_first_run_clone;

/// Serialises tests that set REPOS_ROOT_ENV via std::env::set_var.
///
/// Why: `std::env::set_var` is not thread-safe across concurrent tests (#1780).
/// Two tests that both manipulate the same env key will race unless they acquire
/// this lock first.
/// What: a module-level Mutex<()>; tests hold it for the duration of their
/// set_var / call / restore cycle so the env change is never visible to another
/// concurrent test.
/// Test: prevents `needs_first_run_clone_returns_none_when_clone_exists` from
/// racing `needs_first_run_clone_returns_some_when_no_clone`.
static ENV_MUTEX: Mutex<()> = Mutex::new(());
use crate::commands::guided::{
    CwdProject, NestedFallbackAction, classify_cwd_project, cwd_owns_git_entry, derive_project,
    fallback_protected, github_host, inplace_self_relaunch_hint, is_github_remote,
    ls_tree_reports_tracked_dir, nested_fallback_action, nested_guard_notice, nested_managed_match,
    non_github_refusal_message, pane_identity_confirmed, print_non_tty_hint, print_project_context,
    tty_gate, untracked_ancestor_message,
};
use crate::commands::guided_launch::spawn_progress_message;
use crate::commands::guided_resume::{ResumeAction, is_zombie, needs_restart, plan_resume};
use crate::commands::managed::{filter_live_sessions, is_live_session_state};
// The picker decision enum + parser moved to the shared `session_picker` module.
use crate::commands::session_picker::{PickerDecision, parse_picker_choice};

// ── parse_picker_choice ───────────────────────────────────────────────────────

#[test]
fn guided_picker_bare_enter_no_sessions_launches_new() {
    // Why: bare Enter with no sessions must launch a new session, not hang.
    assert_eq!(
        parse_picker_choice("", 0, false, &[]),
        PickerDecision::LaunchNew
    );
    assert_eq!(
        parse_picker_choice("  \t", 0, false, &[]),
        PickerDecision::LaunchNew
    );
}

#[test]
fn guided_picker_bare_enter_live_session_resumes_first() {
    // Why: bare Enter when the most-recent session is LIVE (not needing a
    // restart) must resume it directly — attaching to a live pane is safe,
    // it never destroys anything (#2148).
    assert_eq!(
        parse_picker_choice("", 1, false, &[]),
        PickerDecision::Resume(0)
    );
    assert_eq!(
        parse_picker_choice("  ", 3, false, &[]),
        PickerDecision::Resume(0)
    );
}

#[test]
fn guided_picker_bare_enter_stopped_session_requires_confirm() {
    // #2148: bare Enter must NOT silently restart (kill+recreate the tmux pane
    // of) a stopped/errored session — the operator must type the number.
    assert_eq!(
        parse_picker_choice("", 1, true, &[]),
        PickerDecision::ConfirmRestart(0)
    );
    assert_eq!(
        parse_picker_choice("  ", 3, true, &[]),
        PickerDecision::ConfirmRestart(0)
    );
}

#[test]
fn guided_picker_bare_enter_unresumable_session_blocked() {
    // #2595: bare Enter must NEVER resume/restart a session whose workspace
    // is gone for good — no confirmation prompt would help, since the resume
    // is guaranteed to fail (#2577/#2594). This takes priority over
    // `ConfirmRestart` (`first_needs_restart=true` here too — a dead session
    // is ALWAYS stopped/errored, so both flags are set; `Unresumable` must win).
    assert_eq!(
        parse_picker_choice("", 1, true, &[true]),
        PickerDecision::Unresumable(0)
    );
    assert_eq!(
        parse_picker_choice("  ", 3, true, &[true, false, false]),
        PickerDecision::Unresumable(0)
    );
}

#[test]
fn guided_picker_numeric_unresumable_session_blocked() {
    // #2595: an EXPLICIT numeric choice must ALSO be blocked when it targets a
    // dead session — the pre-#2595 comment on the numeric branch ("always
    // restarts/resumes directly") is no longer true for this one case.
    assert_eq!(
        parse_picker_choice("2", 3, false, &[false, true, false]),
        PickerDecision::Unresumable(1)
    );
    // A live session at a DIFFERENT index must still resume normally even
    // when another slot in the same menu is dead.
    assert_eq!(
        parse_picker_choice("1", 3, false, &[false, true, false]),
        PickerDecision::Resume(0)
    );
}

#[test]
fn guided_picker_delete_parses_d_prefix() {
    // #2304: `d<N>` / `d <N>` selects the Nth session (0-based) for deletion.
    assert_eq!(
        parse_picker_choice("d1", 1, false, &[]),
        PickerDecision::Delete(0)
    );
    assert_eq!(
        parse_picker_choice("d2", 3, true, &[]),
        PickerDecision::Delete(1)
    );
    assert_eq!(
        parse_picker_choice("D3\n", 3, false, &[]),
        PickerDecision::Delete(2)
    );
    assert_eq!(
        parse_picker_choice("d 2", 3, false, &[]),
        PickerDecision::Delete(1)
    );
}

#[test]
fn guided_picker_delete_out_of_range_unrecognised() {
    // A `d`-prefixed choice outside 1..=session_count (or non-numeric) is
    // rejected — never falls through to the resume/launch branches.
    assert_eq!(
        parse_picker_choice("d4", 3, false, &[]),
        PickerDecision::Unrecognised
    );
    assert_eq!(
        parse_picker_choice("d0", 3, false, &[]),
        PickerDecision::Unrecognised
    );
    assert_eq!(
        parse_picker_choice("dx", 3, false, &[]),
        PickerDecision::Unrecognised
    );
}

#[test]
fn guided_picker_q_returns_quit() {
    // Why: "q" must quit cleanly without touching tmux or the daemon.
    assert_eq!(
        parse_picker_choice("q", 0, false, &[]),
        PickerDecision::Quit
    );
    assert_eq!(parse_picker_choice("q", 3, true, &[]), PickerDecision::Quit);
}

#[test]
fn guided_picker_q_uppercase_returns_quit() {
    // Why: "Q" must be treated identically to "q" (case-insensitive).
    assert_eq!(
        parse_picker_choice("Q", 2, false, &[]),
        PickerDecision::Quit
    );
    assert_eq!(
        parse_picker_choice("Q\n", 0, false, &[]),
        PickerDecision::Quit
    );
}

#[test]
fn guided_picker_numeric_valid_resumes() {
    // Why: "[N]" where 1 <= N <= session_count must resume the Nth session
    // (0-based) — an EXPLICIT numeric choice always dispatches directly,
    // regardless of `first_needs_restart` (that flag only gates bare Enter).
    assert_eq!(
        parse_picker_choice("1", 1, true, &[]),
        PickerDecision::Resume(0)
    );
    assert_eq!(
        parse_picker_choice("1", 3, true, &[]),
        PickerDecision::Resume(0)
    );
    assert_eq!(
        parse_picker_choice("2", 3, false, &[]),
        PickerDecision::Resume(1)
    );
    assert_eq!(
        parse_picker_choice("3", 3, false, &[]),
        PickerDecision::Resume(2)
    );
    // With newline (as stdin read_line returns)
    assert_eq!(
        parse_picker_choice("2\n", 3, false, &[]),
        PickerDecision::Resume(1)
    );
}

#[test]
fn guided_picker_numeric_launch_new() {
    // Why: "[session_count+1]" must always launch a new session.
    assert_eq!(
        parse_picker_choice("1", 0, false, &[]),
        PickerDecision::LaunchNew
    );
    assert_eq!(
        parse_picker_choice("4", 3, false, &[]),
        PickerDecision::LaunchNew
    );
}

#[test]
fn guided_picker_out_of_range_unrecognised() {
    // Why: a number out of range (>session_count+1) must not silently
    // resume or launch — it must be rejected cleanly.
    assert_eq!(
        parse_picker_choice("5", 3, false, &[]),
        PickerDecision::Unrecognised
    );
    assert_eq!(
        parse_picker_choice("100", 1, false, &[]),
        PickerDecision::Unrecognised
    );
    assert_eq!(
        parse_picker_choice("0", 3, false, &[]),
        PickerDecision::Unrecognised
    );
}

#[test]
fn guided_picker_non_numeric_unrecognised() {
    // Why: arbitrary text input must be rejected without panicking.
    assert_eq!(
        parse_picker_choice("abc", 2, false, &[]),
        PickerDecision::Unrecognised
    );
    assert_eq!(
        parse_picker_choice("exit", 0, false, &[]),
        PickerDecision::Unrecognised
    );
    assert_eq!(
        parse_picker_choice("1a", 3, false, &[]),
        PickerDecision::Unrecognised
    );
}

// ── tty_gate ──────────────────────────────────────────────────────────────────

#[test]
fn guided_non_tty_gate_returns_false_skips_stdin() {
    // Why: when is_tty=false the function must return false so the caller
    // returns Ok(()) without ever reading from stdin — the core of AC-7.
    // This test exercises the non-TTY branch without any live stdin.
    let result = tty_gate(false, "owner/repo", &PathBuf::from("/ws/owner/repo"), &[]);
    assert!(
        !result,
        "non-TTY gate must return false (no picker) for empty sessions"
    );
}

#[test]
fn guided_tty_gate_returns_true_for_tty() {
    // Why: when is_tty=true the function must return true so the caller
    // proceeds to run_tty_picker.
    let result = tty_gate(true, "owner/repo", &PathBuf::from("/ws/owner/repo"), &[]);
    assert!(
        result,
        "TTY gate must return true so caller runs the picker"
    );
}

#[test]
fn guided_non_tty_gate_returns_false_with_sessions() {
    // Why: the non-TTY branch must work even when sessions are present —
    // ensuring the hint path handles the session list safely.
    let sessions = vec![make_session("tm-api-1", "running", None)];
    let result = tty_gate(false, "owner/repo", &PathBuf::from("/ws"), &sessions);
    assert!(
        !result,
        "non-TTY gate must return false regardless of session count"
    );
}

// ── derive_project ────────────────────────────────────────────────────────────

#[test]
fn guided_derive_project_returns_none_for_non_git_dir() {
    // Why: a plain temp directory (not a git repo) should not yield a project.
    // What: derive_project(temp_dir) must return None.
    let tmp = std::env::temp_dir();
    let non_git = tmp.join("trusty_test_non_git_dir_1705");
    std::fs::create_dir_all(&non_git).ok();
    let result = derive_project(&non_git);
    assert!(
        result.is_none(),
        "expected None for non-git dir, got {result:?}"
    );
}

#[test]
fn guided_derive_project_rejects_non_github_remote() {
    // Why: if the origin is not a GitHub URL, derive_project must return None
    // so the live-checkout guard fires downstream.
    let tmp = tempdir_with_name("trusty_test_non_github_remote_1705");
    let ok = git_init_quiet(&tmp);
    if !ok {
        return; // git unavailable
    }
    git_remote_add(&tmp, "https://gitlab.com/owner/repo.git");
    let result = derive_project(&tmp);
    assert!(
        result.is_none(),
        "expected None for non-GitHub remote (gitlab), got {result:?}"
    );
}

#[test]
fn guided_derive_project_accepts_github_https_remote() {
    // Why: a valid HTTPS GitHub remote must parse correctly and return the
    // expected source_id, a non-empty workspace path, and the git root.
    let tmp = tempdir_with_name("trusty_test_github_https_1705");
    let ok = git_init_with_commit(&tmp);
    if !ok {
        return;
    }
    git_remote_add(&tmp, "https://github.com/owner/my-repo.git");
    let result = derive_project(&tmp);
    match result {
        Some((source_id, workspace, git_root)) => {
            assert_eq!(source_id, "owner/my-repo");
            assert!(
                !workspace.as_os_str().is_empty(),
                "workspace must be non-empty"
            );
            // workspace must be the managed clone path, not the live checkout
            assert_ne!(workspace, tmp, "workspace must differ from live checkout");
            // git_root must resolve to tmp (the repo root). On macOS /var is a
            // symlink to /private/var; git resolves the canonical path, so compare
            // canonicalized forms.
            let canonical_root = git_root.canonicalize().unwrap_or(git_root);
            let canonical_tmp = tmp.canonicalize().unwrap_or(tmp.clone());
            assert_eq!(
                canonical_root, canonical_tmp,
                "git_root must be the repo root"
            );
        }
        None => panic!("expected Some for GitHub HTTPS remote, got None"),
    }
}

#[test]
fn guided_derive_project_accepts_github_ssh_remote() {
    // Why: SSH-style GitHub remotes (`git@github.com:owner/repo.git`) must be
    // detected in the same way as HTTPS remotes.
    let tmp = tempdir_with_name("trusty_test_github_ssh_1705");
    let ok = git_init_with_commit(&tmp);
    if !ok {
        return;
    }
    git_remote_add(&tmp, "git@github.com:owner/my-repo.git");
    let result = derive_project(&tmp);
    match result {
        Some((source_id, _workspace, _git_root)) => {
            assert_eq!(source_id, "owner/my-repo");
        }
        None => panic!("expected Some for GitHub SSH remote, got None"),
    }
}

#[test]
fn guided_derive_project_returns_some_from_subdir() {
    // Why: derive_project must work when called from a subdirectory of a git
    // repo, and the returned git_root must be the repo root (not the subdir)
    // so that `launch_new_session_and_attach` passes the git root as repo_url
    // and the daemon finds .git, sets source_id correctly (#1705 LOW fix).
    // The subdir here is TRACKED (committed) — an untracked ancestor subdir is
    // deliberately rejected as of #2534 (see the dedicated test below).
    let tmp = tempdir_with_name("trusty_test_subdir_1705");
    let ok = git_init_with_commit(&tmp);
    if !ok {
        return;
    }
    git_remote_add(&tmp, "https://github.com/owner/my-repo.git");

    // Create a nested subdirectory, TRACK it, then call derive_project from it.
    let subdir = tmp.join("src").join("lib");
    std::fs::create_dir_all(&subdir).unwrap();
    git_track_dir(&tmp, "src/lib");

    let result = derive_project(&subdir);
    match result {
        Some((source_id, _workspace, git_root)) => {
            assert_eq!(source_id, "owner/my-repo");
            // git_root must be the repo root (tmp), NOT the subdir. On macOS
            // /var is a symlink to /private/var; compare canonical forms.
            let canonical_root = git_root.canonicalize().unwrap_or(git_root);
            let canonical_tmp = tmp.canonicalize().unwrap_or(tmp.clone());
            assert_eq!(
                canonical_root, canonical_tmp,
                "git_root from subdir must be repo root, not the nested dir"
            );
        }
        None => panic!("expected Some when calling derive_project from a subdir"),
    }
}

// ── Ancestor-repo trust guard (#2534) ─────────────────────────────────────────

#[test]
fn guided_ls_tree_reports_tracked_dir_true_for_named_entry() {
    // Why: git prints the queried path (one line) when it names a tracked tree.
    assert!(ls_tree_reports_tracked_dir("CTO\n"));
    assert!(ls_tree_reports_tracked_dir("src/lib\n"));
}

#[test]
fn guided_ls_tree_reports_tracked_dir_false_for_empty() {
    // Why: an untracked (or case-mismatched) path yields empty stdout — the
    // exact discriminator for the APFS `cto` vs tracked `CTO` collision (#2534).
    assert!(!ls_tree_reports_tracked_dir(""));
}

#[test]
fn guided_ls_tree_reports_tracked_dir_false_for_blank_lines() {
    // Why: whitespace-only output must not be mistaken for a tracked entry.
    assert!(!ls_tree_reports_tracked_dir("\n  \n\t\n"));
}

#[test]
fn guided_untracked_ancestor_message_names_root() {
    // Why: the operator must see WHICH enclosing repo was declined.
    let msg = untracked_ancestor_message(std::path::Path::new("/Users/masa/Duetto"));
    assert!(
        msg.contains("/Users/masa/Duetto"),
        "message must name the enclosing git root: {msg}"
    );
    assert!(
        msg.contains("not part of it"),
        "message must explain the untracked-ancestor reason: {msg}"
    );
}

#[test]
fn guided_untracked_ancestor_message_does_not_claim_launch() {
    // Why: the whole point is that we did NOT launch the ancestor project.
    let msg = untracked_ancestor_message(std::path::Path::new("/repo"));
    assert!(
        msg.contains("not launching that project"),
        "message must state the project was not launched: {msg}"
    );
}

#[test]
fn guided_classify_cwd_not_git_for_plain_dir() {
    // Why: a directory outside any git working tree is NotGit.
    let tmp = tempdir_with_name("trusty_test_classify_plain_2534");
    // Skip if the temp dir happens to be inside a git tree (dev machines vary).
    if find_git_root_via_cli(&tmp).is_some() {
        return;
    }
    assert!(matches!(classify_cwd_project(&tmp), CwdProject::NotGit));
}

#[test]
fn guided_classify_cwd_usable_for_repo_root() {
    // Why: the top-level case is trusted unconditionally — behavior unchanged.
    let tmp = tempdir_with_name("trusty_test_classify_root_2534");
    if !git_init_with_commit(&tmp) {
        return;
    }
    let canonical = tmp.canonicalize().unwrap_or_else(|_| tmp.clone());
    match classify_cwd_project(&canonical) {
        CwdProject::Usable(root) => {
            let root = root.canonicalize().unwrap_or(root);
            assert_eq!(root, canonical, "repo root must classify as Usable(root)");
        }
        _ => panic!("repo root must be Usable"),
    }
}

#[test]
fn guided_classify_cwd_usable_for_tracked_subdir() {
    // Why: a committed subdirectory genuinely belongs to the repo — Usable.
    let tmp = tempdir_with_name("trusty_test_classify_tracked_2534");
    if !git_init_with_commit(&tmp) {
        return;
    }
    git_track_dir(&tmp, "src/lib");
    let subdir = tmp.join("src").join("lib");
    match classify_cwd_project(&subdir) {
        CwdProject::Usable(_) => {}
        _ => panic!("tracked subdir must be Usable"),
    }
}

#[test]
fn guided_classify_cwd_untracked_for_uncommitted_subdir() {
    // Why: the core #2534 regression — a directory that physically exists inside
    // a repo's working tree but is NOT in its tracked tree (the same shape as the
    // APFS `~/Duetto/cto` case-fold collision) must be UntrackedInsideAncestor,
    // NOT trusted as part of the enclosing repo.
    let tmp = tempdir_with_name("trusty_test_classify_untracked_2534");
    if !git_init_with_commit(&tmp) {
        return;
    }
    // Create a subdir but never `git add` it → not in HEAD's tree.
    let untracked = tmp.join("notes");
    std::fs::create_dir_all(&untracked).unwrap();
    match classify_cwd_project(&untracked) {
        CwdProject::UntrackedInsideAncestor(_) => {}
        CwdProject::Usable(_) => {
            panic!("untracked subdir must NOT be trusted as part of the ancestor repo")
        }
        CwdProject::NotGit => panic!("subdir is inside a git tree — must not be NotGit"),
    }
}

#[test]
fn guided_derive_project_returns_none_for_untracked_ancestor_subdir() {
    // Why: the end-to-end #2534 guarantee — bare `tm` from an untracked directory
    // nested inside a GitHub repo must NOT resolve to that ancestor's project.
    let tmp = tempdir_with_name("trusty_test_derive_untracked_2534");
    if !git_init_with_commit(&tmp) {
        return;
    }
    git_remote_add(&tmp, "https://github.com/owner/my-repo.git");
    let untracked = tmp.join("notes");
    std::fs::create_dir_all(&untracked).unwrap();
    assert!(
        derive_project(&untracked).is_none(),
        "derive_project must return None for an untracked ancestor subdir"
    );
}

// ── #2542: own-repo-wins (cwd's own `.git` beats an ancestor's) ───────────────

#[test]
fn guided_cwd_owns_git_entry_true_for_dir() {
    // Why: a `.git` DIRECTORY is the normal repository marker.
    let tmp = tempdir_with_name("trusty_test_owns_git_dir_2542");
    std::fs::create_dir_all(tmp.join(".git")).unwrap();
    assert!(cwd_owns_git_entry(&tmp));
}

#[test]
fn guided_cwd_owns_git_entry_true_for_pointer_file() {
    // Why: a `.git` FILE is a linked-worktree / submodule gitlink — still a
    // working-tree root. Even a dangling pointer (target moved by a rescue)
    // means "this dir is its own repo", so it must count as owned.
    let tmp = tempdir_with_name("trusty_test_owns_git_file_2542");
    std::fs::write(tmp.join(".git"), b"gitdir: ./moved-away\n").unwrap();
    assert!(cwd_owns_git_entry(&tmp));
}

#[test]
fn guided_cwd_owns_git_entry_false_for_dotgit_prefixed_sibling() {
    // Why: the exact-name guarantee — a `.git`-PREFIXED sibling such as
    // `.git.hotstats-backup-20260713` (a preserved gitdir from a rescue) is NOT
    // a repo marker. A `starts_with(".git")`/glob test would be the bug.
    let tmp = tempdir_with_name("trusty_test_owns_git_prefixed_2542");
    std::fs::create_dir_all(tmp.join(".git.hotstats-backup-20260713")).unwrap();
    assert!(
        !cwd_owns_git_entry(&tmp),
        "a `.git`-prefixed sibling must NOT be treated as owning a repo"
    );
}

#[test]
fn guided_cwd_owns_git_entry_false_when_absent() {
    // Why: a plain directory owns no repo.
    let tmp = tempdir_with_name("trusty_test_owns_git_absent_2542");
    assert!(!cwd_owns_git_entry(&tmp));
}

#[test]
fn guided_classify_cwd_own_git_wins_over_ancestor() {
    // Why: the #2542 differential. `cwd` owns a `.git` entry that is present but
    // transiently INVALID (an empty, partially-reconstructed gitdir — the exact
    // shape a concurrent `git` rescue leaves). `git rev-parse --show-toplevel`
    // walks PAST it up to the enclosing `outer` repo (asserted below), so the
    // pre-fix code classified this as `UntrackedInsideAncestor(outer)` and bare
    // `tm` wrongly reported "inside another repository's working tree". Own-repo
    // -wins must classify it as `Usable(cwd)` — its OWN repo, never the ancestor.
    let outer = tempdir_with_name("trusty_test_own_git_wins_2542");
    if !git_init_with_commit(&outer) {
        return; // git unavailable — skip
    }
    let inner = outer.join("inner");
    std::fs::create_dir_all(inner.join(".git")).unwrap(); // present-but-invalid gitdir

    // Precondition: git's upward walk skips the invalid `inner/.git` and lands
    // on `outer` — the exact behavior the fast-path overrides.
    if let Some(walked) = find_git_root_via_cli(&inner) {
        let walked = walked.canonicalize().unwrap_or(walked);
        let outer_c = outer.canonicalize().unwrap_or_else(|_| outer.clone());
        assert_eq!(
            walked, outer_c,
            "precondition: git walks up to the ancestor"
        );
    }

    match classify_cwd_project(&inner) {
        CwdProject::Usable(root) => {
            let root = root.canonicalize().unwrap_or(root);
            let inner_c = inner.canonicalize().unwrap_or_else(|_| inner.clone());
            assert_eq!(
                root, inner_c,
                "must resolve to cwd's OWN repo, not the ancestor"
            );
        }
        other => panic!("cwd owning a `.git` must be Usable(cwd), got {other:?}"),
    }
}

#[test]
fn guided_classify_cwd_own_git_ignores_dotgit_prefixed_sibling() {
    // Why: task guarantee (a)+(b) — an inner repo with a `.git`-suffixed sibling
    // dir in its ancestor resolves to ITSELF, never the sibling or the ancestor.
    let outer = tempdir_with_name("trusty_test_own_git_sibling_2542");
    if !git_init_with_commit(&outer) {
        return;
    }
    // A `.git`-prefixed backup sibling in the ancestor — must be inert.
    std::fs::create_dir_all(outer.join(".git.backup-20260713")).unwrap();
    let inner = outer.join("inner");
    std::fs::create_dir_all(inner.join(".git")).unwrap();
    match classify_cwd_project(&inner) {
        CwdProject::Usable(root) => {
            let root = root.canonicalize().unwrap_or(root);
            let inner_c = inner.canonicalize().unwrap_or_else(|_| inner.clone());
            assert_eq!(root, inner_c);
        }
        other => panic!("inner repo must be Usable(inner), got {other:?}"),
    }
}

#[test]
fn guided_classify_cwd_own_git_worktree_pointer_file() {
    // Why: a `.git` POINTER FILE (linked worktree / submodule gitlink) is a
    // working-tree root just like a `.git` directory — must be Usable(cwd).
    let outer = tempdir_with_name("trusty_test_own_git_ptr_2542");
    if !git_init_with_commit(&outer) {
        return;
    }
    let inner = outer.join("wt");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join(".git"), b"gitdir: /somewhere/else\n").unwrap();
    match classify_cwd_project(&inner) {
        CwdProject::Usable(root) => {
            let root = root.canonicalize().unwrap_or(root);
            let inner_c = inner.canonicalize().unwrap_or_else(|_| inner.clone());
            assert_eq!(root, inner_c);
        }
        other => panic!("cwd with a `.git` pointer file must be Usable(cwd), got {other:?}"),
    }
}

#[test]
fn guided_classify_cwd_untracked_ancestor_still_refuses_without_own_git() {
    // Why: the #2534 guarantee must survive the #2542 fast-path. An untracked
    // directory that has NO own `.git`, nested in a REAL ancestor repo, must
    // still be `UntrackedInsideAncestor` — the fast-path only fires when cwd
    // owns a `.git`, so this path is unchanged.
    let outer = tempdir_with_name("trusty_test_untracked_still_refuses_2542");
    if !git_init_with_commit(&outer) {
        return;
    }
    let untracked = outer.join("notes");
    std::fs::create_dir_all(&untracked).unwrap();
    assert!(
        !cwd_owns_git_entry(&untracked),
        "precondition: untracked dir owns no `.git`"
    );
    match classify_cwd_project(&untracked) {
        CwdProject::UntrackedInsideAncestor(_) => {}
        other => panic!("#2534 case must still refuse the ancestor, got {other:?}"),
    }
}

/// Local helper: resolve the git working-tree root via the CLI (mirrors the
/// production `find_git_root`) so the NotGit test can skip when a dev machine's
/// temp dir happens to sit inside a git tree.
fn find_git_root_via_cli(dir: &PathBuf) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

// ── print_project_context / print_non_tty_hint ────────────────────────────────

#[test]
fn guided_print_project_context_does_not_panic_no_sessions() {
    // Why: the display helper must not panic when the session list is empty.
    print_project_context(
        "owner/repo",
        &PathBuf::from("/home/user/repos/owner/repo"),
        &[],
    );
}

#[test]
fn guided_print_project_context_does_not_panic_with_sessions() {
    // Why: the display helper must not panic when optional fields are None.
    let sessions = vec![make_session(
        "tm-frontend-1",
        "running",
        Some("2026-06-25T12:00:00Z"),
    )];
    print_project_context(
        "owner/repo",
        &PathBuf::from("/home/user/repos/owner/repo"),
        &sessions,
    );
}

#[test]
fn guided_print_non_tty_hint_does_not_panic_no_sessions() {
    // Why: the non-TTY degradation path must work when there are no sessions.
    print_non_tty_hint("owner/repo", &[]);
}

#[test]
fn guided_print_non_tty_hint_does_not_panic_with_sessions() {
    // Why: the non-TTY hint must print the session name for a resume hint.
    let sessions = vec![make_session("tm-api-2", "stopped", None)];
    print_non_tty_hint("owner/repo", &sessions);
}

// ── fallback_protected in non-git dir ────────────────────────────────────────

#[tokio::test]
async fn guided_fallback_non_git_dir_calls_launch_path() {
    // Why: for a non-git directory, fallback_protected should call launch()
    // which will fail (daemon not running) rather than returning a
    // "live git checkout protected" error.
    let tmp = tempdir_with_name("trusty_test_fallback_nongit_1705");
    let client = reqwest::Client::new();
    let result = fallback_protected(&client, "http://127.0.0.1:19999", &tmp).await;
    // The function should NOT return the live-checkout protection error.
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(
            !msg.contains("live git checkout"),
            "non-git dir should NOT trigger live-checkout guard; got: {msg}"
        );
    }
}

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Create (or replace) a temp directory with the given name under the OS temp dir.
fn tempdir_with_name(name: &str) -> PathBuf {
    let tmp = std::env::temp_dir().join(name);
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp).expect("create temp dir");
    tmp
}

/// Run `git init -q` in `dir`. Returns true on success, false if git unavailable.
fn git_init_quiet(dir: &PathBuf) -> bool {
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `git init` + minimal identity config + empty initial commit.
/// Returns true on success, false if git is unavailable in this environment.
fn git_init_with_commit(dir: &PathBuf) -> bool {
    if !git_init_quiet(dir) {
        return false;
    }
    // Configure a minimal identity so `git commit` doesn't fail.
    for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
        std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(dir)
            .status()
            .ok();
    }
    // Empty initial commit so the repo has a valid HEAD ref.
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init", "-q"])
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Add `remote.origin` with the given URL.
fn git_remote_add(dir: &PathBuf, url: &str) {
    std::process::Command::new("git")
        .args(["remote", "add", "origin", url])
        .current_dir(dir)
        .status()
        .ok();
}

/// Create `<repo>/<rel>/.keep`, stage it, and commit so `<rel>` becomes a
/// tracked directory in the repo's HEAD tree. Used to distinguish a genuinely
/// tracked subdir from an untracked ancestor subdir (#2534).
fn git_track_dir(repo: &PathBuf, rel: &str) {
    let dir = repo.join(rel);
    std::fs::create_dir_all(&dir).expect("create tracked subdir");
    std::fs::write(dir.join(".keep"), b"").expect("write .keep");
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .status()
        .ok();
    std::process::Command::new("git")
        .args(["commit", "-m", "track", "-q"])
        .current_dir(repo)
        .status()
        .ok();
}

/// Construct a minimal `ManagedSessionSummary` for tests.
fn make_session(
    name: &str,
    state: &str,
    last_activity_at: Option<&str>,
) -> trusty_mpm::client::ManagedSessionSummary {
    trusty_mpm::client::ManagedSessionSummary {
        id: format!("{name}-id"),
        name: name.to_string(),
        state: state.to_string(),
        workspace_path: None,
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: last_activity_at.map(str::to_owned),
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
        stale_assets: false,
        attached: false,
    }
}

// ── needs_restart (#1742) ─────────────────────────────────────────────────────
// `needs_restart` is now state-only: it returns true iff the daemon's /resume
// endpoint can accept the session (Stopped/Errored only). The active-but-tmux-
// absent case is handled by `is_zombie` below.

#[test]
fn guided_resume_needs_restart_stopped() {
    // Why: stopped state means no live runtime; daemon restart is required (#1742).
    // tmux liveness is irrelevant — the daemon makes the decision based on state.
    assert!(needs_restart("stopped"), "stopped state must need restart");
}

#[test]
fn guided_resume_needs_restart_errored() {
    // Why: errored sessions are resumable through the daemon (Stopped|Errored are
    // the only accepted states for /resume); direct attach would fail.
    assert!(needs_restart("errored"), "errored state must need restart");
}

#[test]
fn guided_resume_no_restart_active() {
    // Why: active state — daemon's /resume rejects it with 409; not the restart path.
    // (The active-but-tmux-absent case is caught by is_zombie before we'd POST.)
    assert!(
        !needs_restart("active"),
        "active state must not need daemon restart"
    );
}

#[test]
fn guided_resume_no_restart_provisioning() {
    // Why: provisioning state — daemon is already setting up the session.
    assert!(
        !needs_restart("provisioning"),
        "provisioning must not need restart"
    );
}

#[test]
fn guided_resume_no_restart_decommissioned() {
    // Why: decommissioned sessions have no workspace; /resume returns 409.
    // is_zombie handles the absent-tmux case before we'd attempt a POST.
    assert!(
        !needs_restart("decommissioned"),
        "decommissioned must not need restart"
    );
}

// ── is_zombie (#1742 adversarial follow-up) ───────────────────────────────────
// A zombie is a session whose daemon state is NOT stopped/errored (i.e., active
// or provisioning) but whose tmux session has disappeared. The daemon's /resume
// endpoint would return 409 for these — leading to a permanent dead end. The
// correct recovery is: `tm session stop <id>` then `tm` again.

#[test]
fn guided_resume_is_zombie_active_no_tmux() {
    // Why: active + tmux absent is the canonical zombie case — daemon thinks it's
    // running but tmux is gone. We must bail with an actionable message, not POST.
    assert!(
        is_zombie("active", false),
        "active + no tmux must be detected as zombie"
    );
}

#[test]
fn guided_resume_is_zombie_provisioning_no_tmux() {
    // Why: provisioning + tmux absent is also a zombie (daemon is setting up a
    // session whose tmux vanished). Same actionable bail applies.
    assert!(
        is_zombie("provisioning", false),
        "provisioning + no tmux must be detected as zombie"
    );
}

#[test]
fn guided_resume_not_zombie_stopped_no_tmux() {
    // Why: stopped + no tmux is NOT a zombie — it is the normal restart case where
    // the daemon's /resume will recreate the tmux session. Must not bail.
    assert!(
        !is_zombie("stopped", false),
        "stopped + no tmux is a restart case, not a zombie"
    );
}

#[test]
fn guided_resume_not_zombie_errored_no_tmux() {
    // Why: errored + no tmux is also a restart case, not a zombie.
    assert!(
        !is_zombie("errored", false),
        "errored + no tmux is a restart case, not a zombie"
    );
}

#[test]
fn guided_resume_not_zombie_active_with_tmux() {
    // Why: active + tmux live is the happy-path attach case — no zombie, no restart.
    assert!(
        !is_zombie("active", true),
        "active + live tmux is the normal attach path, not a zombie"
    );
}

// ── plan_resume (#2001 zombie auto-reconcile) ─────────────────────────────────
// `plan_resume` is the pure branch-selection seam that drives resume_guided_session.
// It composes is_zombie + needs_restart into the three concrete actions the I/O
// driver takes. The zombie case must now select ReconcileThenRestart (auto-stop
// then restart) rather than bailing — the operator does nothing.

#[test]
fn guided_resume_plan_active_live_tmux_attaches() {
    // Why: active state with a live tmux pane is the happy path — attach directly,
    // no daemon round-trip, no stop, no resume.
    assert_eq!(
        plan_resume("active", true),
        ResumeAction::Attach,
        "active + live tmux must attach directly"
    );
}

#[test]
fn guided_resume_plan_stopped_restarts() {
    // Why: stopped state must go straight to the daemon /resume restart path —
    // NOT reconcile (there is nothing to stop) and NOT a bare attach.
    assert_eq!(
        plan_resume("stopped", false),
        ResumeAction::Restart,
        "stopped must select the plain Restart path"
    );
}

#[test]
fn guided_resume_plan_errored_restarts() {
    // Why: errored is resumable via /resume just like stopped.
    assert_eq!(
        plan_resume("errored", false),
        ResumeAction::Restart,
        "errored must select the plain Restart path"
    );
}

#[test]
fn guided_resume_plan_active_no_tmux_reconciles_then_restarts() {
    // Why (#2001): the canonical zombie — daemon says active but tmux is gone. The
    // fix is to auto-stop (reset the record to Stopped) THEN restart, so the plan
    // must be ReconcileThenRestart, not a bail and not a plain Restart (a bare
    // /resume would 409 because the record is still active).
    assert_eq!(
        plan_resume("active", false),
        ResumeAction::ReconcileThenRestart,
        "active + no tmux must reconcile (auto-stop) then restart"
    );
}

#[test]
fn guided_resume_plan_provisioning_no_tmux_reconciles_then_restarts() {
    // Why (#2001): provisioning + tmux gone is also a zombie and follows the same
    // auto-stop-then-restart recovery.
    assert_eq!(
        plan_resume("provisioning", false),
        ResumeAction::ReconcileThenRestart,
        "provisioning + no tmux must reconcile then restart"
    );
}

#[test]
fn plan_resume_refuses_terminal_states() {
    // code-critic CRITICAL: a terminal tombstone must NEVER be resumed —
    // neither via the zombie-reconcile path (tmux_live=false, which used to
    // resurrect a Deleted record) NOR via a bare Attach (tmux_live=true, the
    // "force-deleted-while-live" variant). Both must resolve to Terminal.
    for state in ["deleted", "decommissioned"] {
        for tmux_live in [false, true] {
            assert_eq!(
                plan_resume(state, tmux_live),
                ResumeAction::Terminal,
                "{state} (tmux_live={tmux_live}) must be refused as Terminal, never resumed"
            );
        }
    }
}

#[test]
fn is_zombie_false_for_terminal_states() {
    // A terminal tombstone with no live tmux must NOT read as a resurrectable
    // zombie (the exact chain that resurrected a Deleted session).
    assert!(!is_zombie("deleted", false), "deleted is never a zombie");
    assert!(
        !is_zombie("decommissioned", false),
        "decommissioned is never a zombie"
    );
}

#[test]
fn guided_resume_plan_stopped_with_stale_tmux_still_restarts() {
    // Why: a stopped record whose stale tmux pane is somehow still alive is NOT a
    // zombie (needs_restart is true) — it takes the plain Restart path (the daemon
    // kills the stale pane). Guards the branch ordering in plan_resume.
    assert_eq!(
        plan_resume("stopped", true),
        ResumeAction::Restart,
        "stopped + stale live tmux must still take the Restart path, not reconcile"
    );
}

// ── is_github_remote (Change 2: SSH alias support) ───────────────────────────

#[test]
fn is_github_remote_accepts_github_com_ssh() {
    // Why: `git@github.com:o/r.git` is the canonical SSH GitHub URL.
    assert!(
        is_github_remote("git@github.com:o/r.git"),
        "github.com SSH must be recognised as GitHub"
    );
}

#[test]
fn is_github_remote_accepts_github_com_https() {
    // Why: `https://github.com/o/r.git` is the canonical HTTPS GitHub URL.
    assert!(
        is_github_remote("https://github.com/o/r.git"),
        "github.com HTTPS must be recognised as GitHub"
    );
}

#[test]
fn is_github_remote_accepts_github_hyphen_alias() {
    // Why: `git@github-duetto:duettoresearch/aria.git` is the real-world repro
    // case from the issue. Multi-account SSH aliases use `github-<name>`.
    assert!(
        is_github_remote("git@github-duetto:duettoresearch/aria.git"),
        "github-<alias> SSH remote must be recognised as GitHub"
    );
}

#[test]
fn is_github_remote_accepts_github_alias_ssh_url_style() {
    // Why: `ssh://git@github-work/o/r` uses scheme-URL form with an alias host.
    assert!(
        is_github_remote("ssh://git@github-work/o/r"),
        "ssh:// github-<alias> must be recognised as GitHub"
    );
}

#[test]
fn is_github_remote_accepts_github_underscore_alias() {
    // Why: some operators use underscores in their SSH config host aliases
    // (e.g. `github_personal`). The rule covers `-` and `_` separators.
    assert!(
        is_github_remote("git@github_personal:user/repo.git"),
        "github_<alias> SSH remote must be recognised as GitHub"
    );
}

#[test]
fn is_github_remote_rejects_gitlab() {
    // Why: GitLab URLs must NEVER be treated as GitHub to avoid an unexpected
    // managed-clone redirect for non-GitHub projects.
    assert!(
        !is_github_remote("git@gitlab.com:o/r.git"),
        "gitlab.com must NOT be recognised as GitHub"
    );
}

#[test]
fn is_github_remote_rejects_bitbucket() {
    // Why: Bitbucket is not GitHub; the guard must block it.
    assert!(
        !is_github_remote("https://bitbucket.org/o/r"),
        "bitbucket.org must NOT be recognised as GitHub"
    );
}

#[test]
fn is_github_remote_rejects_githubusercontent() {
    // Why: `raw.githubusercontent.com` contains `github` but is a content
    // delivery host, not a clone remote. The host does NOT start with
    // `github-` or `github_`, and it is not `github.com`, so it must be
    // blocked. (Cloning from this host would fail anyway, but blocking it
    // prevents a misleading redirect attempt.)
    assert!(
        !is_github_remote("https://raw.githubusercontent.com/o/r/file"),
        "githubusercontent.com must NOT be recognised as GitHub"
    );
}

#[test]
fn is_github_remote_accepts_github_com_https_with_port() {
    // Why: `https://github.com:443/o/r.git` is a valid remote URL (explicit
    // port). The old substring-match handled this; the host-based approach
    // regressed on it because `split('/').next()` returned `"github.com:443"`.
    // This is the regression guard for the port-stripping fix.
    assert!(
        is_github_remote("https://github.com:443/o/r.git"),
        "https://github.com:443/… must be recognised as GitHub (port stripped)"
    );
}

#[test]
fn is_github_remote_rejects_gitea_with_github_in_path() {
    // Why: a self-hosted Gitea whose URL happens to mention "github" in the
    // path (e.g. a mirror) must not be treated as GitHub.
    assert!(
        !is_github_remote("https://gitea.example.com/mirrors/github-fork.git"),
        "gitea host with github in path must NOT match"
    );
}

// ── github_host extraction ────────────────────────────────────────────────────

#[test]
fn github_host_extracts_scp_style() {
    // Why: the most common GitHub remote form is scp-style `git@HOST:path`.
    assert_eq!(
        github_host("git@github-duetto:duettoresearch/aria.git"),
        "github-duetto"
    );
    assert_eq!(github_host("git@github.com:owner/repo.git"), "github.com");
}

#[test]
fn github_host_extracts_https() {
    // Why: HTTPS remotes use scheme-URL form `https://HOST/path`.
    assert_eq!(
        github_host("https://github.com/owner/repo.git"),
        "github.com"
    );
    assert_eq!(github_host("https://gitlab.com/o/r"), "gitlab.com");
}

#[test]
fn github_host_extracts_ssh_url_with_user() {
    // Why: `ssh://git@HOST/path` is the RFC-compliant SSH URL form.
    assert_eq!(github_host("ssh://git@github-work/o/r"), "github-work");
}

// ── derive_project accepts SSH alias remote ──────────────────────────────────

#[test]
fn guided_derive_project_accepts_github_ssh_alias() {
    // Why: `derive_project` uses `is_github_remote` internally. With the
    // SSH-alias fix, a repo whose origin is `git@github-duetto:owner/repo.git`
    // must return Some with the correct source_id.
    let tmp = tempdir_with_name("trusty_test_github_alias_derive_1705");
    let ok = git_init_with_commit(&tmp);
    if !ok {
        return;
    }
    git_remote_add(&tmp, "git@github-duetto:duettoresearch/aria.git");
    let result = derive_project(&tmp);
    match result {
        Some((source_id, _workspace, _git_root)) => {
            assert_eq!(
                source_id, "duettoresearch/aria",
                "source_id must be parsed correctly from alias remote"
            );
        }
        None => panic!("expected Some for GitHub SSH alias remote, got None"),
    }
}

// ── fallback_protected with SSH alias does not hit GitHub-remote refusal ─────

#[tokio::test]
#[serial_test::serial]
async fn guided_fallback_does_not_refuse_github_ssh_alias_remote() {
    // Why: before this fix, `git@github-duetto:duettoresearch/aria.git`
    // triggered the "Auto-protected managed clones require a GitHub remote"
    // error because `is_github_remote` only matched `github.com`. The SSH alias
    // host `github-duetto` was not recognised, so `fallback_protected` fell
    // into the non-GitHub refusal path. This test locks in the fix.
    // What: creates a real git repo, sets the SSH-alias remote, and calls
    // `fallback_protected`. Asserts the result is NOT the GitHub-remote
    // refusal error. (It may still be an Err from the clone attempt failing
    // due to the daemon being unreachable — that is expected and acceptable.)
    // Deliberately uses a fixture-only host/owner/repo (NOT the real
    // `github-duetto` alias from the regression report above) — a developer
    // machine may have a real, reachable SSH config entry with that exact
    // name (as this repo's own maintainer's machine does), which would let
    // `ensure_base_clone` actually succeed and make this test flaky/order-
    // dependent on local SSH config instead of hermetic (discovered while
    // fixing the nested-tmux attach bug, #1873).
    // Test: this is the test; annotated `serial` because it may set REPOS_ROOT.
    let dir = tempdir_with_name("trusty_test_github_alias_fallback_1705");
    let ok = std::process::Command::new("git")
        .arg("init")
        .current_dir(&dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!(
            "guided_fallback_does_not_refuse_github_ssh_alias_remote: git unavailable, skipping"
        );
        return;
    }
    let _ = std::process::Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            "git@github-test-fixture-alias-nonexistent:acmetest/repo-fixture.git",
        ])
        .current_dir(&dir)
        .status();

    // Point REPOS_ROOT at a tempdir so we don't pollute the real repos root
    // and to make `ensure_base_clone` fail fast (base/.git absent → clone
    // attempt → network failure → Err, which is what we want to assert on).
    let repos_root = tempfile::tempdir().unwrap();
    let repos_root_key = trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV;
    let prev = std::env::var(repos_root_key).ok();
    unsafe { std::env::set_var(repos_root_key, repos_root.path()) };

    let client = reqwest::Client::new();
    let result = fallback_protected(&client, "http://127.0.0.1:1", &dir).await;

    unsafe {
        match prev {
            Some(v) => std::env::set_var(repos_root_key, v),
            None => std::env::remove_var(repos_root_key),
        }
    }

    // The call must fail (daemon unreachable + clone fails), but NOT with the
    // "requires a GitHub remote" refusal message.
    assert!(
        result.is_err(),
        "fallback must Err (daemon unreachable / clone fails)"
    );
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        !err_msg.contains("auto-managed clones require a GitHub remote"),
        "SSH alias remote must NOT trigger GitHub-remote refusal; got: {err_msg}"
    );
    // Also confirm no framework files landed in the live checkout.
    assert!(
        !dir.join("CLAUDE.md").exists(),
        "CLAUDE.md must NOT appear in the live checkout"
    );
    assert!(
        !dir.join(".mcp.json").exists(),
        ".mcp.json must NOT appear in the live checkout"
    );
}

// ── needs_first_run_clone (#1780) ─────────────────────────────────────────────

/// Why: a non-directory path (URL, non-existent path) must return None — no git
/// operation is attempted; the check is a fast-path guard.
/// Test: itself.
#[test]
fn needs_first_run_clone_returns_none_for_non_dir() {
    assert!(needs_first_run_clone("https://github.com/owner/repo.git").is_none());
    assert!(needs_first_run_clone("/nonexistent/path/that/does/not/exist").is_none());
    assert!(needs_first_run_clone("").is_none());
}

/// Why: when the base clone directory already exists, the fn must return None
/// so the "first run" message is NOT emitted on subsequent `tm` invocations.
/// Test: itself. Marked `#[serial_test::serial]` so it cannot run concurrently
/// with other env-mutating tests (especially b-tests that also mutate
/// TRUSTY_MPM_REPOS_ROOT without holding ENV_MUTEX).
#[test]
#[serial_test::serial]
fn needs_first_run_clone_returns_none_when_clone_exists() {
    use std::process::Command;
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path();

    // Init git and add a GitHub origin.
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !git(&["init"]) {
        return; // no git on runner
    }
    git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:owner/already-cloned.git",
    ]);

    // Simulate the base clone already being present by creating the expected dir.
    let repos_env_key = trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV;
    let tmp_repos = tempfile::TempDir::new().unwrap();
    let base = tmp_repos.path().join("owner").join("already-cloned");
    std::fs::create_dir_all(base.join(".git")).unwrap();

    let prev = std::env::var(repos_env_key).ok();
    let result = {
        let _env_guard = ENV_MUTEX.lock().unwrap();
        unsafe { std::env::set_var(repos_env_key, tmp_repos.path()) };
        let r = needs_first_run_clone(&dir.to_string_lossy());
        unsafe {
            match prev {
                Some(v) => std::env::set_var(repos_env_key, v),
                None => std::env::remove_var(repos_env_key),
            }
        }
        r
    };
    assert!(
        result.is_none(),
        "base clone exists → must return None (no first-run message)"
    );
}

/// Why: the first `tm` invocation returns Some when the clone directory is absent,
/// giving the caller the project id and path to emit a "cloning…" message before
/// the blocking daemon request. This is the primary FIX 2 path (#1780).
/// Test: itself. Marked `#[serial_test::serial]` so it cannot run concurrently
/// with other env-mutating tests (especially b-tests that also mutate
/// TRUSTY_MPM_REPOS_ROOT without holding ENV_MUTEX).
#[test]
#[serial_test::serial]
fn needs_first_run_clone_returns_some_when_no_clone() {
    use std::process::Command;
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path();

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if !git(&["init"]) {
        return;
    }
    git(&[
        "remote",
        "add",
        "origin",
        "git@github.com:myorg/my-new-project.git",
    ]);

    // Point repos root at an empty temp dir so the base clone definitely does NOT exist.
    let repos_env_key = trusty_mpm::daemon::managed_routes::inproject::REPOS_ROOT_ENV;
    let tmp_repos = tempfile::TempDir::new().unwrap();
    let prev = std::env::var(repos_env_key).ok();
    let result = {
        let _env_guard = ENV_MUTEX.lock().unwrap();
        unsafe { std::env::set_var(repos_env_key, tmp_repos.path()) };
        let r = needs_first_run_clone(&dir.to_string_lossy());
        unsafe {
            match prev {
                Some(v) => std::env::set_var(repos_env_key, v),
                None => std::env::remove_var(repos_env_key),
            }
        }
        r
    };
    let (proj, _path) = result.expect("must return Some for a first-run scenario");
    assert_eq!(proj, "myorg/my-new-project");
}

// ── spawn_progress_message (#1904) ────────────────────────────────────────────
// The blocking managed-spawn POST previously left the operator with zero
// feedback for the whole (potentially multi-minute) first-run clone; a spinner
// now wraps the call, and this is the pure message-formatting helper behind it.

#[test]
fn spawn_progress_message_first_run() {
    // Why: the first-run case must name the project and the destination path so
    // the operator understands why the wait is long.
    let path = PathBuf::from("/home/user/trusty-mpm-projects/owner/repo");
    let msg = spawn_progress_message(Some(&("owner/repo".to_string(), path.clone())));
    assert!(msg.contains("owner/repo"));
    assert!(msg.contains(&path.display().to_string()));
    assert!(msg.contains("first run"));
}

#[test]
fn spawn_progress_message_reuse() {
    // Why: a non-first-run launch (worktree already cloned) should show the
    // generic launching message, not the cloning-specific one.
    let msg = spawn_progress_message(None);
    assert_eq!(msg, "tm: launching new session…");
}

// ── non_github_refusal_message (#1777) ───────────────────────────────────────
// These tests verify the message helper introduced to fix the misleading
// "daemon unreachable" wording shown when the actual reason for refusal is
// "not a GitHub remote". The helper is pure — no stderr capture needed.

#[test]
fn guided_non_github_refusal_message_does_not_mention_daemon_or_start() {
    // Why: the old message wrongly said "daemon unreachable" even when the
    // daemon is running. The new message must never reference daemon
    // reachability or the `tm start` command (#1777).
    // What: call the helper with a GitLab remote and assert forbidden phrases
    // are absent.
    // Test: this is the test.
    let msg = non_github_refusal_message("git@gitlab.com:org/repo.git");
    assert!(
        !msg.contains("daemon unreachable"),
        "refusal message must NOT blame the daemon: {msg}"
    );
    assert!(
        !msg.contains("tm start"),
        "refusal message must NOT instruct to start the daemon: {msg}"
    );
    assert!(
        !msg.contains("Start the daemon"),
        "refusal message must NOT mention starting the daemon: {msg}"
    );
}

#[test]
fn guided_non_github_refusal_message_explains_github_only_policy() {
    // Why: operators need to understand the actual reason — `tm` auto-manages
    // GitHub repositories only, not Gitea/GitLab/bare-SSH remotes.
    // What: asserts the message names "GitHub" as the requirement.
    // Test: this is the test.
    let msg = non_github_refusal_message("https://gitea.example.com/org/repo.git");
    assert!(
        msg.contains("GitHub"),
        "refusal must name GitHub as the auto-management requirement: {msg}"
    );
    assert!(
        msg.to_lowercase().contains("auto-manag"),
        "refusal must mention auto-management scope: {msg}"
    );
}

#[test]
fn guided_non_github_refusal_message_includes_detected_remote() {
    // Why: showing the detected remote URL in the message lets the operator
    // immediately confirm which remote triggered the refusal — useful when
    // running `tm` in a repo with multiple or aliased remotes.
    // What: asserts the passed-in remote string appears verbatim in the output.
    // Test: this is the test.
    let remote = "git@gitlab.com:myorg/myrepo.git";
    let msg = non_github_refusal_message(remote);
    assert!(
        msg.contains(remote),
        "refusal must echo the detected remote ({remote}): {msg}"
    );
}

#[test]
fn guided_non_github_refusal_message_reassures_live_checkout_untouched() {
    // Why: the operator may be anxious that `tm` modified their working tree.
    // The message must clearly state the live checkout was not touched.
    // What: asserts the reassurance phrase appears in the output.
    // Test: this is the test.
    let msg = non_github_refusal_message("https://bitbucket.org/org/repo.git");
    assert!(
        msg.contains("live checkout"),
        "refusal must reassure that the live checkout was not touched: {msg}"
    );
    assert!(
        msg.contains("not touched"),
        "refusal must use the phrase 'not touched': {msg}"
    );
}

// ── #1809: decommissioned-tombstone filter ────────────────────────────────────

#[test]
fn picker_filter_live_state_excludes_decommissioned() {
    // Why (#1809): `is_live_session_state` is the canonical predicate for
    // "should this session appear in the picker / sessions list by default?".
    // Test: concrete state → expected bool, not derived from the same expression.
    assert!(
        !is_live_session_state("decommissioned"),
        "decommissioned must be excluded from default view"
    );
    // Active sessions must always be visible.
    assert!(
        is_live_session_state("active"),
        "active must be included in default view"
    );
    // Stopped/errored sessions can still be resumed — they must show.
    assert!(
        is_live_session_state("stopped"),
        "stopped must be included in default view"
    );
    assert!(
        is_live_session_state("errored"),
        "errored must be included in default view"
    );
    // Provisioning sessions are in-flight — they must show.
    assert!(
        is_live_session_state("provisioning"),
        "provisioning must be included in default view"
    );
}

#[test]
fn is_live_session_state_excludes_deleted() {
    // code-critic CRITICAL: a `--deleted--` tombstone is TERMINAL — it must be
    // excluded from the default picker/list exactly like `decommissioned`, so it
    // is never offered as a resume target (which would resurrect it).
    assert!(
        !is_live_session_state("deleted"),
        "deleted must be excluded from the default picker/list view"
    );
    // Both terminal tombstones are excluded; every live state is kept.
    assert!(!is_live_session_state("decommissioned"));
    assert!(is_live_session_state("active"));
    assert!(is_live_session_state("stopped"));
}

#[test]
fn picker_filter_excludes_decommissioned_keeps_active() {
    // Why (#1809): `filter_live_sessions` must drop decommissioned tombstones and
    // retain all other states. We construct a mixed slice and assert concrete counts
    // and membership — not the same expression used to compute the filter.
    let sessions: Vec<trusty_mpm::client::ManagedSessionSummary> =
        serde_json::from_value(serde_json::json!([
            { "id": "a1", "name": "sess-active",        "state": "active" },
            { "id": "b2", "name": "sess-dead-1",        "state": "decommissioned" },
            { "id": "c3", "name": "sess-stopped",       "state": "stopped" },
            { "id": "d4", "name": "sess-dead-2",        "state": "decommissioned" },
            { "id": "e5", "name": "sess-provisioning",  "state": "provisioning" },
        ]))
        .expect("test data must deserialize");

    let filtered = filter_live_sessions(sessions);

    // Exactly 3 of the 5 sessions survive the filter.
    assert_eq!(
        filtered.len(),
        3,
        "filter must keep exactly 3 live sessions (active, stopped, provisioning)"
    );
    // Active session must be present.
    assert!(
        filtered.iter().any(|s| s.state == "active"),
        "active session must survive filter"
    );
    // Stopped session must be present (can be resumed).
    assert!(
        filtered.iter().any(|s| s.state == "stopped"),
        "stopped session must survive filter"
    );
    // Provisioning session must be present (in-flight).
    assert!(
        filtered.iter().any(|s| s.state == "provisioning"),
        "provisioning session must survive filter"
    );
    // Neither decommissioned session must appear.
    assert!(
        !filtered.iter().any(|s| s.state == "decommissioned"),
        "decommissioned tombstones must be excluded"
    );
}

#[test]
fn picker_filter_all_live_sessions_unchanged() {
    // Why: when no sessions are decommissioned, `filter_live_sessions` must
    // return all sessions unchanged — no unexpected truncation.
    let sessions: Vec<trusty_mpm::client::ManagedSessionSummary> =
        serde_json::from_value(serde_json::json!([
            { "id": "x1", "name": "sess-a", "state": "active" },
            { "id": "x2", "name": "sess-b", "state": "stopped" },
            { "id": "x3", "name": "sess-c", "state": "errored" },
        ]))
        .expect("test data must deserialize");

    let filtered = filter_live_sessions(sessions);
    assert_eq!(
        filtered.len(),
        3,
        "all-live input must pass through unchanged (3 sessions)"
    );
}

#[test]
fn picker_filter_all_decommissioned_returns_empty() {
    // Why: if every session is decommissioned, the picker must show an empty list
    // (not crash or return some sessions).
    let sessions: Vec<trusty_mpm::client::ManagedSessionSummary> =
        serde_json::from_value(serde_json::json!([
            { "id": "z1", "name": "old-1", "state": "decommissioned" },
            { "id": "z2", "name": "old-2", "state": "decommissioned" },
        ]))
        .expect("test data must deserialize");

    let filtered = filter_live_sessions(sessions);
    assert!(
        filtered.is_empty(),
        "all-decommissioned input must produce empty list"
    );
}

// ── #1808: daily banner uses two-panel renderer ───────────────────────────────

#[test]
fn daily_banner_two_panel_version_in_title_bar_not_content() {
    // Why (#1808): the daily `tm` banner must use `render_two_panel_banner`
    // so the version appears in the title bar (first line, starts with ╭) and
    // NOT as a separate content row. The compact `render_welcome_panel` path
    // always puts `"trusty-mpm vX.Y.Z"` in the first content row, which is the
    // old behaviour we are replacing.
    // What: builds WelcomeData (same shape the daily banner path uses) and checks
    // the two-panel output for the invariants that distinguish the new path.
    use crate::formatters::banner::two_panel::{render_two_panel_banner, strip_ansi};
    use crate::formatters::info_box::{DaemonInfo, WelcomeData};

    colored::control::set_override(false);
    let data = WelcomeData {
        project: "owner/repo".to_string(),
        workspace: "/home/alice/trusty-mpm-projects/owner/repo".to_string(),
        user: "alice".to_string(),
        reconnecting: false,
        session_name: String::new(),
        daemon: DaemonInfo::default(),
        recent_commits: vec![],
        memory_status: "(not detected)".to_string(),
        search_status: "(not detected)".to_string(),
        review_status: "(not detected)".to_string(),
    };

    let version = env!("CARGO_PKG_VERSION");
    let banner =
        render_two_panel_banner(&data, 120, false).expect("120-col terminal must produce banner");
    let bare = strip_ansi(&banner);

    // 1. Version appears exactly once — in the title bar, never in content rows.
    let count = bare.matches(version).count();
    assert_eq!(
        count, 1,
        "version must appear exactly once (title bar only); found {count}"
    );

    // 2. First line (title bar) starts with ╭ and contains the version.
    let first = bare.lines().next().unwrap_or("");
    assert!(
        first.starts_with('╭'),
        "title bar must start with ╭: {first:?}"
    );
    assert!(
        first.contains(version),
        "title bar must contain the version: {first:?}"
    );

    // 3. Content rows must NOT contain the version string as a standalone line.
    // (The title bar is line 0; content rows follow.)
    for (i, line) in bare.lines().enumerate().skip(1) {
        // Content lines must not reproduce the version outside the border.
        let inner = line.trim_start_matches('│').trim_end_matches('│').trim();
        assert!(
            !inner.starts_with(&format!("trusty-mpm v{version}")),
            "content row {i} must not carry the version line: {line:?}"
        );
    }
    colored::control::unset_override();
}

// ── nested_managed_match (#2157 item 4) ──────────────────────────────────────
// The nested-session guard's pure decision: does any known managed record
// belong to the pane bare `tm` is currently running inside? Matched either by
// tmux session name (the primary signal — works even when the env var was
// never exported into THIS particular pane) or by TM_MANAGED_SESSION_ID
// (belt-and-suspenders).

#[test]
fn nested_managed_match_by_session_name() {
    let sessions = vec![make_session("tm-proj-01", "active", None)];
    let matched = nested_managed_match(Some("tm-proj-01"), None, &sessions);
    assert_eq!(matched.map(|s| s.name.as_str()), Some("tm-proj-01"));
}

#[test]
fn nested_managed_match_by_env_id() {
    let sessions = vec![make_session("tm-proj-01", "active", None)];
    // make_session sets id = "<name>-id".
    let matched = nested_managed_match(None, Some("tm-proj-01-id"), &sessions);
    assert_eq!(matched.map(|s| s.name.as_str()), Some("tm-proj-01"));
}

#[test]
fn nested_managed_match_none_when_no_match() {
    let sessions = vec![make_session("tm-proj-01", "active", None)];
    // Neither the session name nor the env id matches any record — e.g. a
    // plain terminal opened outside any managed tmux session.
    let matched = nested_managed_match(Some("some-other-session"), Some("unrelated-id"), &sessions);
    assert!(matched.is_none());
}

#[test]
fn nested_managed_match_none_when_both_inputs_absent() {
    // The "not inside tmux" case: the guard's I/O wrapper passes None for
    // both keys, which must never spuriously match any record.
    let sessions = vec![make_session("tm-proj-01", "active", None)];
    let matched = nested_managed_match(None, None, &sessions);
    assert!(matched.is_none());
}

#[test]
fn nested_managed_match_finds_record_missing_from_source_id_filtered_list() {
    // #2157 items 4+5 interplay: the guard fetches the UNFILTERED session
    // list specifically so it can still find a record whose source_id write
    // never landed (item 5's failure mode) — this record would be invisible
    // to a `?source_id=` filtered fetch, but the guard must still catch it by
    // tmux session name.
    let mut orphaned = make_session("tm-orphan-02", "active", None);
    orphaned.source_id = None;
    let sessions = vec![orphaned];
    let matched = nested_managed_match(Some("tm-orphan-02"), None, &sessions);
    assert!(
        matched.is_some(),
        "must match by session name regardless of source_id"
    );
}

/// Build a [`make_session`] summary with an explicit `created_at` (RFC3339),
/// for tests that need to control recency ordering.
fn make_session_at(
    name: &str,
    state: &str,
    created_at: &str,
) -> trusty_mpm::client::ManagedSessionSummary {
    let mut s = make_session(name, state, None);
    s.created_at = Some(created_at.to_string());
    s
}

#[test]
fn nested_managed_match_prefers_live_over_recycled_decommissioned_name() {
    // #2790 code-critic HIGH: tmux session names are RECYCLED after
    // decommission. A stale Decommissioned tombstone sharing a name with a
    // genuinely LIVE session must never win the match — liveness takes STRICT
    // precedence over recency (not just "usually more recent"). Proven here by
    // giving the tombstone a LATER created_at than the live record — an
    // adversarial ordering the pre-#2790 plain `.find()` would have been
    // vulnerable to depending on iteration/list order.
    let mut decommissioned =
        make_session_at("tm-proj-01", "decommissioned", "2026-03-01T00:00:00Z");
    decommissioned.id = "old-id".to_string();
    let mut live = make_session_at("tm-proj-01", "active", "2026-01-01T00:00:00Z");
    live.id = "new-id".to_string();
    let sessions = vec![decommissioned, live];

    let matched = nested_managed_match(Some("tm-proj-01"), None, &sessions);
    assert_eq!(
        matched.map(|s| s.id.as_str()),
        Some("new-id"),
        "a live record must win over a decommissioned one sharing the same \
         recycled name, even when the tombstone's created_at is later"
    );
}

#[test]
fn nested_managed_match_falls_back_to_decommissioned_when_no_live_candidate() {
    // The legitimate #2777 repro: the ONLY record sharing this tmux session
    // name IS the decommissioned session itself (its name has not yet been
    // recycled by a new session) — the guard must still match it so the
    // in-place revive path can run.
    let sessions = vec![make_session("tm-apex-01", "decommissioned", None)];
    let matched = nested_managed_match(Some("tm-apex-01"), None, &sessions);
    assert_eq!(
        matched.map(|s| s.name.as_str()),
        Some("tm-apex-01"),
        "must fall back to the decommissioned record when no live candidate \
         shares its name"
    );
}

#[test]
fn nested_managed_match_prefers_most_recent_live_among_multiple() {
    // Belt-and-suspenders: when MULTIPLE non-decommissioned candidates somehow
    // share a name (should not normally happen, but the guard must still be
    // deterministic), the most recently created one wins — mirroring
    // `capture_pane_by_tmux_name`'s `max_by_key(created_at)` convention.
    let mut older = make_session_at("tm-proj-02", "active", "2026-01-01T00:00:00Z");
    older.id = "older-id".to_string();
    let mut newer = make_session_at("tm-proj-02", "active", "2026-02-01T00:00:00Z");
    newer.id = "newer-id".to_string();
    let sessions = vec![older, newer];

    let matched = nested_managed_match(Some("tm-proj-02"), None, &sessions);
    assert_eq!(matched.map(|s| s.id.as_str()), Some("newer-id"));
}

// ── pane_identity_confirmed (#2456 review finding 1, ROUND 2) ────────────────
// The cross-pane-hijack guard: `nested_managed_match` alone only proves the
// tmux SESSION matches (every window/pane in that session shares the same
// session name) — it is NOT proof the CURRENT pane is the one bound to the
// matched record. A ROUND-1 fix compared the process-level
// `TM_MANAGED_SESSION_ID` env var; that was EMPIRICALLY DISPROVEN (live tmux
// 3.6b) — tmux's session-scoped `set-environment` (used by the runtime-exit
// healing step) is inherited into the process env of every NEW pane/window
// created in that session AFTERWARD, so an env-var comparison can be
// satisfied by a genuinely different, unrelated pane. `pane_identity_confirmed`
// now compares tmux's own stable `pane_id` (never inherited across panes)
// instead.

#[test]
fn pane_identity_confirmed_true_when_pane_id_matches_record() {
    // THIS pane's own tmux pane_id equals the SAME record's captured
    // pane_id — genuinely the pane bound to that record; safe to relaunch.
    assert!(pane_identity_confirmed(Some("%5"), Some("%5")));
}

#[test]
fn pane_identity_confirmed_false_when_current_pane_id_absent() {
    // The CURRENT pane's tmux query failed (or we are not inside tmux at
    // all) — cannot confirm identity; must refuse to drive the in-place
    // relaunch here, even if the record DOES have a pane_id.
    assert!(!pane_identity_confirmed(None, Some("%5")));
}

#[test]
fn pane_identity_confirmed_false_when_record_pane_id_absent() {
    // A legacy record (created before #2453) never had a pane_id captured —
    // `None` must be treated as "identity unconfirmed", never an implicit
    // match, regardless of what the current pane's own id is.
    assert!(!pane_identity_confirmed(Some("%5"), None));
}

#[test]
fn pane_identity_confirmed_false_when_pane_ids_differ() {
    // Both resolved, but for genuinely DIFFERENT panes — must still refuse.
    assert!(!pane_identity_confirmed(Some("%7"), Some("%5")));
}

#[test]
fn pane_identity_confirmed_false_when_inherited_env_but_different_pane_id() {
    // #2456 review finding 1's ROUND-2 exact hijack scenario, reproduced at
    // the pane_id layer: a session whose runtime-exit healing has fired once
    // (so `TM_MANAGED_SESSION_ID` is now poisoned into the tmux SESSION
    // environment) gets a second window opened afterward. That new window's
    // process env INHERITS the healed session's id — an env-var-only gate
    // would wrongly treat this as belonging to the healed session's record.
    // Modeled here directly at the pane_id layer (env inheritance is a tmux
    // process-env mechanism, not observable from pure Rust — the point this
    // test proves is that pane_id comparison does NOT share that weakness):
    // the healed record's `pane_id` is `"%5"` (the ORIGINAL pane), but the
    // NEW sibling window bare `tm` is actually invoked from resolves to a
    // DIFFERENT pane_id, `"%9"` — even though (in the real system) both
    // panes' process env would read the SAME inherited
    // `TM_MANAGED_SESSION_ID`. The gate must reject this pane_id mismatch.
    let healed_record_pane_id = Some("%5");
    let sibling_window_current_pane_id = Some("%9");
    assert!(!pane_identity_confirmed(
        sibling_window_current_pane_id,
        healed_record_pane_id
    ));
}

// ── nested_fallback_action (#2777 decommissioned-relaunch dead-end fix) ──────
// When the nested-session guard matched by tmux SESSION name but could NOT
// confirm pane identity, the pre-#2777 fallback ALWAYS reconnected
// (switch-client) — a no-op dead-end when the session is DEAD (the operator is
// already inside the session being "reconnected" to, and nothing live is there).
// `nested_fallback_action` splits dead states (relaunch in place) from
// possibly-live states (reconnect a sibling window).

#[test]
fn nested_fallback_action_relaunches_dead_states() {
    // The reported bug: bare `tm` inside a `decommissioned` session must NOT
    // switch-client to itself. Stopped/errored share the "no live runtime" trait
    // and must relaunch in place too rather than take the destructive daemon
    // /resume path a plain reconnect would reach.
    for state in ["decommissioned", "stopped", "errored"] {
        assert_eq!(
            nested_fallback_action(state),
            NestedFallbackAction::RelaunchInPlace,
            "dead state {state:?} must relaunch in place, not switch-client to self"
        );
    }
}

#[test]
fn nested_fallback_action_reconnects_live_states() {
    // A possibly-live session (its runtime may be in a genuinely different
    // sibling window of the same tmux session) must keep the switch-client
    // reconnect — that is NOT a self no-op, it brings the live pane forward.
    for state in ["active", "provisioning", "running", "some-future-state"] {
        assert_eq!(
            nested_fallback_action(state),
            NestedFallbackAction::Reconnect,
            "possibly-live state {state:?} must reconnect (default), not relaunch"
        );
    }
}

// ── nested_guard_notice (sibling-window hijack fix, follow-up to #2456) ─────

#[test]
fn nested_guard_notice_never_says_refusing() {
    // Regression guard: the old wording ("…refusing to launch a nested
    // session here.") read as a hard failure even when the immediately
    // following reconcile-relaunch succeeded. The reworded notice must never
    // contain that alarming phrasing.
    let msg = nested_guard_notice("my-session");
    assert!(
        !msg.to_lowercase().contains("refus"),
        "nested_guard_notice must not read as a refusal: {msg:?}"
    );
}

#[test]
fn nested_guard_notice_mentions_reconnect_target() {
    // The notice replaces two previously-separate lines (the refusal line and
    // a standalone "reconnecting to '{name}' instead…" line) with one — the
    // session name must still be present so the operator knows where they are
    // being reconnected.
    let msg = nested_guard_notice("my-session");
    assert!(
        msg.contains("my-session"),
        "nested_guard_notice must name the session being reconnected to: {msg:?}"
    );
    assert!(
        msg.contains("reconnecting"),
        "nested_guard_notice must still communicate the reconnect action: {msg:?}"
    );
}

#[test]
fn inplace_self_relaunch_hint_suggests_managed_resume() {
    // #2794: the honest self-relaunch hint must point at the MANAGED relaunch
    // `tm sessions resume <managed-id>` — re-spawning the runtime with the
    // tm-owned CLAUDE_CONFIG_DIR, roster, and persona intact — keyed on the
    // managed session id (`record.id`), NOT the claude conversation id.
    let mut record = make_session("tm-cto-01", "active", None);
    // A claude_session_id must NOT leak into the hint — the managed id is what
    // `tm sessions resume` takes.
    record.claude_session_id = Some("bfc69db6-cb98-4aa7-a07d-89fd69ba710b".to_string());
    let hint = inplace_self_relaunch_hint(&record);
    assert!(
        hint.contains(&format!("tm sessions resume {}", record.id)),
        "hint must suggest the managed `tm sessions resume <id>`: {hint:?}"
    );
    assert!(
        hint.contains("already inside this session's pane"),
        "hint must explain why a reconnect would be a no-op: {hint:?}"
    );
}

#[test]
fn inplace_self_relaunch_hint_never_suggests_bare_claude() {
    // #2794 regression guard: a bare `claude` (or `claude --resume <id>`)
    // launches OUTSIDE the managed session, dropping the tm-owned config —
    // the hint must NEVER suggest it, with or without a claude_session_id.
    for sid in [None, Some("abc123".to_string()), Some("   ".to_string())] {
        let mut record = make_session("tm-cto-01", "active", None);
        record.claude_session_id = sid.clone();
        let hint = inplace_self_relaunch_hint(&record);
        assert!(
            !hint.contains("claude"),
            "hint must never mention a bare `claude` (loses managed config); \
             sid={sid:?}: {hint:?}"
        );
        assert!(
            hint.contains("tm sessions resume"),
            "hint must always point at the managed resume; sid={sid:?}: {hint:?}"
        );
    }
}
