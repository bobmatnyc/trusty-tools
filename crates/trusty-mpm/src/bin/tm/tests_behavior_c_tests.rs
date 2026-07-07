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
    PickerDecision, derive_project, fallback_protected, github_host, is_github_remote,
    nested_managed_match, non_github_refusal_message, parse_picker_choice, print_non_tty_hint,
    print_project_context, tty_gate,
};
use crate::commands::guided_launch::spawn_progress_message;
use crate::commands::guided_resume::{ResumeAction, is_zombie, needs_restart, plan_resume};
use crate::commands::managed::{filter_live_sessions, is_live_session_state};

// ── parse_picker_choice ───────────────────────────────────────────────────────

#[test]
fn guided_picker_bare_enter_no_sessions_launches_new() {
    // Why: bare Enter with no sessions must launch a new session, not hang.
    assert_eq!(parse_picker_choice("", 0), PickerDecision::LaunchNew);
    assert_eq!(parse_picker_choice("  \t", 0), PickerDecision::LaunchNew);
}

#[test]
fn guided_picker_bare_enter_with_sessions_resumes_first() {
    // Why: bare Enter when sessions exist must resume the most-recent session
    // (index 0) — the documented default for sessions-present mode.
    assert_eq!(parse_picker_choice("", 1), PickerDecision::Resume(0));
    assert_eq!(parse_picker_choice("  ", 3), PickerDecision::Resume(0));
}

#[test]
fn guided_picker_q_returns_quit() {
    // Why: "q" must quit cleanly without touching tmux or the daemon.
    assert_eq!(parse_picker_choice("q", 0), PickerDecision::Quit);
    assert_eq!(parse_picker_choice("q", 3), PickerDecision::Quit);
}

#[test]
fn guided_picker_q_uppercase_returns_quit() {
    // Why: "Q" must be treated identically to "q" (case-insensitive).
    assert_eq!(parse_picker_choice("Q", 2), PickerDecision::Quit);
    assert_eq!(parse_picker_choice("Q\n", 0), PickerDecision::Quit);
}

#[test]
fn guided_picker_numeric_valid_resumes() {
    // Why: "[N]" where 1 <= N <= session_count must resume the Nth session (0-based).
    assert_eq!(parse_picker_choice("1", 1), PickerDecision::Resume(0));
    assert_eq!(parse_picker_choice("1", 3), PickerDecision::Resume(0));
    assert_eq!(parse_picker_choice("2", 3), PickerDecision::Resume(1));
    assert_eq!(parse_picker_choice("3", 3), PickerDecision::Resume(2));
    // With newline (as stdin read_line returns)
    assert_eq!(parse_picker_choice("2\n", 3), PickerDecision::Resume(1));
}

#[test]
fn guided_picker_numeric_launch_new() {
    // Why: "[session_count+1]" must always launch a new session.
    assert_eq!(parse_picker_choice("1", 0), PickerDecision::LaunchNew);
    assert_eq!(parse_picker_choice("4", 3), PickerDecision::LaunchNew);
}

#[test]
fn guided_picker_out_of_range_unrecognised() {
    // Why: a number out of range (>session_count+1) must not silently
    // resume or launch — it must be rejected cleanly.
    assert_eq!(parse_picker_choice("5", 3), PickerDecision::Unrecognised);
    assert_eq!(parse_picker_choice("100", 1), PickerDecision::Unrecognised);
    assert_eq!(parse_picker_choice("0", 3), PickerDecision::Unrecognised);
}

#[test]
fn guided_picker_non_numeric_unrecognised() {
    // Why: arbitrary text input must be rejected without panicking.
    assert_eq!(parse_picker_choice("abc", 2), PickerDecision::Unrecognised);
    assert_eq!(parse_picker_choice("exit", 0), PickerDecision::Unrecognised);
    assert_eq!(parse_picker_choice("1a", 3), PickerDecision::Unrecognised);
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
    let tmp = tempdir_with_name("trusty_test_subdir_1705");
    let ok = git_init_with_commit(&tmp);
    if !ok {
        return;
    }
    git_remote_add(&tmp, "https://github.com/owner/my-repo.git");

    // Create a nested subdirectory and call derive_project from it.
    let subdir = tmp.join("src").join("lib");
    std::fs::create_dir_all(&subdir).unwrap();

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
