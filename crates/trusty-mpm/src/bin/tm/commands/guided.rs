//! Bare `tm` guided default mode — session picker (#1705, #1708).
//!
//! Why: typing `tm` alone — with no subcommand — should do the most useful
//! thing for the operator's current context. Inside a GitHub-backed git
//! repository with a reachable daemon, the guided default (#1705) queries the
//! daemon for existing sessions bound to the detected `owner/repo`, shows them
//! in a numbered list, and lets the operator resume or launch with a single key.
//! When the daemon is unreachable or the project is not GitHub-backed, the
//! protected fallback (#1724) ensures the live checkout is never touched.
//!
//! What: [`run_guided_default`] orchestrates detection, listing, and dispatch.
//! [`derive_project`] derives the `source_id`, managed workspace, and git root.
//! [`fallback_protected`] is the three-way dispatch for the daemon-unreachable
//! path; it is also the target for non-GitHub projects. [`non_github_refusal_message`]
//! is the pure helper that produces the accurate refusal text for the
//! non-GitHub-remote case (see #1777). The TTY picker is split into a pure
//! `parse_picker_choice` + an I/O driver `run_tty_picker`.
//!
//! Test: unit tests for detection and fallback live in `tests_behavior_b_tests.rs`
//! (#1724 regression suite) and `tests_behavior_c_tests.rs` (#1705 new UX suite);
//! the managed-spawn integration path is exercised by `tests/session_manager_mvp.rs`.

use anyhow::Context as _;
use std::io::IsTerminal as _;

pub(crate) use super::guided_autostart::github_host;
use super::guided_launch::launch_new_session_and_attach;
use super::managed::filter_live_sessions;
use crate::formatters::info_box::DaemonInfo;

/// Decision returned by [`parse_picker_choice`].
///
/// Why: extracting the parse-and-decide logic from the I/O driver makes it
/// unit-testable without stdin/tmux. The driver calls parse, checks the variant,
/// and shells out only for Resume and LaunchNew.
/// What: four variants cover every valid and invalid input the picker can receive.
/// Test: `guided_picker_bare_enter_no_sessions_launches_new`,
/// `guided_picker_bare_enter_with_sessions_resumes_first`,
/// `guided_picker_q_returns_quit`, `guided_picker_numeric_valid_resumes`,
/// `guided_picker_numeric_launch_new`, `guided_picker_out_of_range_unrecognised`,
/// `guided_picker_non_numeric_unrecognised`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PickerDecision {
    /// Resume the session at 0-based index into the sessions slice.
    Resume(usize),
    /// Launch a brand-new session.
    LaunchNew,
    /// User chose to quit without action.
    Quit,
    /// Input was not recognised; the caller quits cleanly.
    Unrecognised,
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Bare `tm` guided default — detect project, list sessions, and offer a picker.
///
/// Why: operators who type `tm` from inside a project directory should not
/// have to remember `tm session new`, `tm connect`, or `tm attach <name>` —
/// the guided default derives the project identity, shows existing sessions,
/// and lets them resume or launch in one step. The live checkout is NEVER
/// written to; all managed sessions run in the protected base-clone workspace.
/// What: (1) tries the rich picker UX when the daemon is reachable and the
/// CWD is a GitHub-backed git project; (2) falls through to
/// [`fallback_protected`] for every other case (daemon unreachable, non-GitHub
/// remote, non-git directory). Non-TTY piped invocations print project + session
/// info and exit 0 without hanging for input (#1705 AC-7). Subdir launches
/// pass the git root as `repo_url` so the daemon sets `source_id` correctly.
/// Test: `guided_derive_project_returns_none_for_non_git_dir`,
/// `guided_non_tty_gate_returns_false_skips_stdin`,
/// `guided_fallback_never_pollutes_github_git_checkout` (#1724).
pub(crate) async fn run_guided_default(client: &reqwest::Client, url: &str) -> anyhow::Result<()> {
    // #2023 component C: an in-pane relaunch takes priority over EVERYTHING
    // below — project detection, the picker, the daemon-unreachable fallback.
    // `TM_MANAGED_SESSION_ID` (exported by #2023 component B into the pane's
    // shell) only resolves to a known managed session when bare `tm` is
    // literally running inside that session's own pane after its runtime
    // exited; any other case (unset, blank, stale/unknown to the daemon)
    // falls through to the ordinary guided default below.
    if let Some(result) = super::guided_inplace::try_inplace_relaunch(client, url).await {
        return result;
    }

    let cwd = std::env::current_dir().context("cannot resolve current directory")?;
    let workdir = cwd.to_string_lossy().to_string();
    eprintln!("tm: no subcommand — using guided default for {workdir}");

    // Auto-register the git project root as a local path alias (non-fatal, silent).
    // Why: every `tm` invocation from a git directory populates `tm ls` with
    // the project's canonical name so operators can quickly find their projects.
    super::managed_root::try_register_alias(&cwd);

    // Use a mutable URL so that a successful auto-start can update it to the
    // actual bound address discovered via the lock file.
    let mut effective_url = url.to_string();

    // Try the rich UX when CWD is a GitHub-backed git project.
    if let Some((source_id, workspace, git_root)) = derive_project(&cwd) {
        // Pass the git root as repo_url so daemon detects .git at root even
        // when tm is invoked from a subdirectory (#1705 LOW fix).
        let repo_url = git_root.to_string_lossy().to_string();

        // First attempt: daemon may already be up.
        if let Some(r) = try_show_picker(
            client,
            &effective_url,
            &source_id,
            &workspace,
            &repo_url,
            &cwd,
        )
        .await
        {
            return r;
        }

        // Daemon unreachable — try to auto-start it transparently.
        eprintln!("tm: daemon not running — starting it…");
        match super::guided_autostart::ensure_daemon_started(client, &effective_url).await {
            Ok(new_url) => {
                effective_url = new_url;
                // Retry the full picker flow with the freshly-started daemon.
                if let Some(r) = try_show_picker(
                    client,
                    &effective_url,
                    &source_id,
                    &workspace,
                    &repo_url,
                    &cwd,
                )
                .await
                {
                    return r;
                }
                eprintln!("tm: daemon started but sessions still unreachable; falling back");
            }
            Err(e) => eprintln!("tm: auto-start failed ({e}); falling back to offline mode"),
        }
    }

    // Daemon unreachable OR not a GitHub project: protected fallback (#1724).
    // For GitHub projects this redirects to the managed-clone workspace.
    // For non-GitHub git projects it refuses (live-checkout guard).
    // For non-git directories it falls through to the classic `tm launch` path.
    fallback_protected(client, &effective_url, &cwd).await
}

/// Attempt to list sessions and display the interactive picker for a GitHub project.
///
/// Why: the same "list sessions → two-panel-banner → picker" sequence is needed
/// both on the initial attempt and after a successful auto-start. A shared helper
/// keeps `run_guided_default` DRY and avoids divergence between the two call sites.
/// What: calls `list_project_sessions`, filters out decommissioned tombstones
/// (#1809), runs the tty-gate, renders the two-panel daily banner (#1808), then
/// hands off to the picker. Returns `None` when the daemon is unreachable so the
/// caller can try auto-start; returns `Some(result)` once the daemon responded.
/// Test: indirectly covered by guided-default e2e tests; the pure sub-functions
/// (`tty_gate`, `parse_picker_choice`, `is_live_session_state`) are unit-tested
/// independently.
async fn try_show_picker(
    client: &reqwest::Client,
    url: &str,
    source_id: &str,
    workspace: &std::path::Path,
    repo_url: &str,
    cwd: &std::path::Path,
) -> Option<anyhow::Result<()>> {
    // #1809: exclude decommissioned tombstones from the picker by default.
    let sessions = filter_live_sessions(list_project_sessions(client, url, source_id).await.ok()?);
    if !tty_gate(
        std::io::stdin().is_terminal(),
        source_id,
        workspace,
        &sessions,
    ) {
        return Some(Ok(()));
    }
    // #1808: render the same two-panel banner as `tm banner` — version in the
    // title bar, 24-row clipped art, project/workspace fields — no sleep.
    let daemon = DaemonInfo::from_lock_file_with_probe().with_count(sessions.len());
    crate::formatters::banner::print_daily_banner(&cwd.to_string_lossy(), &daemon);
    Some(run_tty_picker(client, url, repo_url, source_id, sessions).await)
}

// ── Project derivation ───────────────────────────────────────────────────────

/// Derive the GitHub `source_id`, managed workspace, and git root from `cwd`.
///
/// Why: the picker needs the `owner/repo` identity (for filtering sessions by
/// `source_id`), the managed workspace path (for display), and the git root
/// path (to pass as `repo_url` so the daemon sets `source_id` correctly even
/// when `tm` is invoked from a subdirectory — #1705 LOW fix).
/// What: finds the git root, reads `remote.origin.url`, guards that it is a
/// GitHub URL, parses `owner/repo` via `parse_github_path`, and returns
/// `(source_id, workspace_path, git_root)` where `source_id = "owner/repo"`,
/// `workspace_path = ~/trusty-mpm-projects/<owner>/<repo>` (the canonical base),
/// and `git_root` is the absolute path of the repository root.
/// Test: `guided_derive_project_returns_none_for_non_git_dir`,
/// `guided_derive_project_rejects_non_github_remote`,
/// `guided_derive_project_accepts_github_https_remote`,
/// `guided_derive_project_returns_some_from_subdir`.
pub(crate) fn derive_project(
    cwd: &std::path::Path,
) -> Option<(String, std::path::PathBuf, std::path::PathBuf)> {
    use trusty_mpm::daemon::managed_routes::inproject;
    let git_root = find_git_root(cwd)?;
    let origin_url = inproject::get_origin_url(&git_root).filter(|u| is_github_remote(u))?;
    let gh = trusty_common::github_path::parse_github_path(&origin_url)?;
    let source_id = format!("{}/{}", gh.owner, gh.repo);
    let workspace = inproject::base_clone_path(&gh.owner, &gh.repo);
    Some((source_id, workspace, git_root))
}

// ── Daemon communication ─────────────────────────────────────────────────────

/// Fetch managed sessions for a project via `GET /api/v1/sessions/managed?source_id`.
///
/// Why: the picker only shows sessions that belong to the current project; the
/// `?source_id=<owner/repo>` filter keeps the response small regardless of how
/// many total sessions the daemon manages.
/// What: GETs the managed-list endpoint with the query param and deserializes
/// the sessions array. Returns `Err` when the daemon is unreachable or the
/// request fails — the caller uses this as a signal to fall back.
/// Test: requires a live daemon; covered by the e2e test suite
/// (`tests/session_manager_mvp.rs`) and integration tests in
/// requires a live daemon; covered by the e2e test suite.
async fn list_project_sessions(
    client: &reqwest::Client,
    base_url: &str,
    source_id: &str,
) -> anyhow::Result<Vec<trusty_mpm::client::ManagedSessionSummary>> {
    let resp = client
        .get(format!("{base_url}/api/v1/sessions/managed"))
        .query(&[("source_id", source_id)])
        .send()
        .await?
        .error_for_status()?;
    let body: trusty_mpm::client::ManagedListResponse = resp.json().await?;
    Ok(body.sessions)
}

// ── Display / TTY-gate helpers ────────────────────────────────────────────────

/// Print project context and decide whether to run the interactive picker.
///
/// Why: a testable seam over `std::io::stdin().is_terminal()`. By injecting
/// `is_tty`, callers in tests can exercise the non-TTY branch without a live
/// stdin — confirming that the branch returns `false` (no picker needed) and
/// thus prevents any attempt to read from stdin (#1705 AC-7 / HIGH-2b).
/// What: always calls [`print_project_context`]. If `is_tty = false`, also
/// calls [`print_non_tty_hint`] and returns `false`. If `is_tty = true`,
/// returns `true` (caller should invoke [`run_tty_picker`]).
/// Test: `guided_non_tty_gate_returns_false_skips_stdin`,
/// `guided_tty_gate_returns_true_for_tty`.
pub(crate) fn tty_gate(
    is_tty: bool,
    source_id: &str,
    workspace: &std::path::Path,
    sessions: &[trusty_mpm::client::ManagedSessionSummary],
) -> bool {
    print_project_context(source_id, workspace, sessions);
    if !is_tty {
        print_non_tty_hint(source_id, sessions);
    }
    is_tty
}

/// Print the detected project and session list to stderr.
///
/// Why: the operator needs to see at a glance which project was detected, where
/// the managed workspace lives (so it is clear the live checkout is untouched),
/// and which sessions are available before being prompted.
/// What: prints the source_id, workspace path, and a numbered session list (or
/// "(none)" when empty). All output goes to stderr (stdout stays clean).
/// Test: `guided_print_project_context_does_not_panic_no_sessions`,
/// `guided_print_project_context_does_not_panic_with_sessions`.
pub(crate) fn print_project_context(
    source_id: &str,
    workspace: &std::path::Path,
    sessions: &[trusty_mpm::client::ManagedSessionSummary],
) {
    eprintln!("tm: project:   {source_id}");
    eprintln!(
        "tm: workspace: {} (live checkout is NOT touched)",
        workspace.display()
    );
    if sessions.is_empty() {
        eprintln!("tm: sessions:  (none)");
    } else {
        eprintln!("tm: sessions:");
        for (i, s) in sessions.iter().enumerate() {
            let activity = s.last_activity_at.as_deref().unwrap_or("—");
            eprintln!(
                "tm:   [{}] {}  state={}  last={activity}",
                i + 1,
                s.name,
                s.state
            );
        }
    }
}

/// Print a non-TTY degradation notice and actionable hints.
///
/// Why: when stdin is not a TTY (CI, pipes, scripts), hanging for input would
/// block the caller forever; print the context and exit cleanly instead (#1705 AC-7).
/// What: emits one-line notice + resume/launch hints to stderr. The caller
/// returns `Ok(())` immediately after.
/// Test: `guided_print_non_tty_hint_does_not_panic_no_sessions`,
/// `guided_print_non_tty_hint_does_not_panic_with_sessions`,
/// `guided_non_tty_gate_returns_false_skips_stdin`.
pub(crate) fn print_non_tty_hint(
    source_id: &str,
    sessions: &[trusty_mpm::client::ManagedSessionSummary],
) {
    eprintln!("tm: (stdin is not a TTY — run `tm` from an interactive terminal to use the picker)");
    if sessions.is_empty() {
        eprintln!("tm: to launch a new session: start the daemon and run `tm` from a TTY");
    } else {
        let n = sessions.len();
        eprintln!("tm: {n} session(s) found for {source_id}");
        eprintln!("tm: to resume: tmux attach-session -t {}", sessions[0].name);
        eprintln!("tm: to launch: run `tm` from an interactive terminal");
    }
}

// ── TTY picker ───────────────────────────────────────────────────────────────

/// Parse one line of picker input into a [`PickerDecision`].
///
/// Why: separating parse-and-decide from the I/O driver makes the dispatch
/// logic unit-testable without needing a real stdin, tmux, or daemon.
/// What: `session_count` is the number of existing sessions in the menu (the
/// menu slot `session_count + 1` is always "launch new").
///   • `"q"` / `"Q"` → `Quit`
///   • empty / whitespace → `Resume(0)` when `session_count > 0`, else `LaunchNew`
///   • `N` (1..=session_count) → `Resume(N-1)` (0-based index)
///   • `session_count + 1` → `LaunchNew`
///   • anything else → `Unrecognised`
/// Test: `guided_picker_bare_enter_no_sessions_launches_new`,
/// `guided_picker_bare_enter_with_sessions_resumes_first`,
/// `guided_picker_q_returns_quit`, `guided_picker_q_uppercase_returns_quit`,
/// `guided_picker_numeric_valid_resumes`, `guided_picker_numeric_launch_new`,
/// `guided_picker_out_of_range_unrecognised`,
/// `guided_picker_non_numeric_unrecognised`.
pub(crate) fn parse_picker_choice(line: &str, session_count: usize) -> PickerDecision {
    let choice = line.trim();
    if choice.eq_ignore_ascii_case("q") {
        return PickerDecision::Quit;
    }
    if choice.is_empty() {
        return if session_count > 0 {
            PickerDecision::Resume(0)
        } else {
            PickerDecision::LaunchNew
        };
    }
    if let Ok(n) = choice.parse::<usize>() {
        if n >= 1 && n <= session_count {
            return PickerDecision::Resume(n - 1);
        }
        if n == session_count + 1 {
            return PickerDecision::LaunchNew;
        }
    }
    PickerDecision::Unrecognised
}

/// Interactive numbered picker (TTY mode only).
///
/// Why: a simple numbered menu is the lowest-friction way to resume or launch
/// without requiring the operator to remember session names or UUIDs. After a
/// detach, the picker is redisplayed rather than exiting to the shell — the
/// common "pick → Ctrl-b d → pick again" flow stays in one command.
/// What: loops: print menu, read one line, dispatch, then re-fetch the session
/// list so the next iteration shows current state. Exits cleanly on `Quit`,
/// EOF (Ctrl-D), or unrecognised input; propagates attach/launch errors.
///   • `Resume(i)` → [`resume_guided_session`] which handles daemon restart
///     when needed and then calls [`tmux_attach`] internally;
///   • `LaunchNew` → [`launch_new_session_and_attach`];
///   • `Quit` / EOF / `Unrecognised` → print notice and return `Ok`.
/// Test: `parse_picker_choice` is the testable seam; I/O path is exercised by
/// manual smoke tests and the e2e suite.
async fn run_tty_picker(
    client: &reqwest::Client,
    url: &str,
    repo_url: &str,
    source_id: &str,
    mut sessions: Vec<trusty_mpm::client::ManagedSessionSummary>,
) -> anyhow::Result<()> {
    loop {
        eprintln!();
        let new_idx = sessions.len() + 1;
        if sessions.is_empty() {
            eprintln!("tm:   [Enter] launch new session");
            eprintln!("tm:   [q]     quit");
        } else {
            for (i, s) in sessions.iter().enumerate() {
                // Show "restart" for sessions that are stopped/errored — they have no
                // live tmux session and will be restarted via the daemon (#1742).
                let stopped = matches!(s.state.as_str(), "stopped" | "errored");
                let verb = if stopped { "restart" } else { "resume" };
                eprintln!("tm:   [{}] {} {} ({})", i + 1, verb, s.name, s.state);
            }
            eprintln!("tm:   [{new_idx}] launch new session");
            eprintln!("tm:   [q] quit");
            eprintln!("tm: default: [1] resume/restart most recent");
        }
        eprint!("tm: > ");

        let mut line = String::new();
        let n = std::io::stdin()
            .read_line(&mut line)
            .context("failed to read choice from stdin")?;
        if n == 0 {
            break;
        } // EOF (Ctrl-D): exit cleanly.

        match parse_picker_choice(&line, sessions.len()) {
            PickerDecision::Quit => {
                eprintln!("tm: quit.");
                break;
            }
            // #1742: route through daemon resume when the session is stopped or its
            // tmux session is absent — never raw-attach a non-live session.
            PickerDecision::Resume(i) => {
                super::guided_resume::resume_guided_session(client, url, &sessions[i]).await?
            }
            PickerDecision::LaunchNew => {
                launch_new_session_and_attach(client, url, repo_url).await?
            }
            PickerDecision::Unrecognised => {
                eprintln!("tm: unrecognised choice '{}'; quitting.", line.trim());
                break;
            }
        }

        // Detached or session ended — re-fetch the list before redisplaying.
        // #1809: apply the same tombstone filter on the re-fetched list.
        let r = list_project_sessions(client, url, source_id).await?;
        sessions = filter_live_sessions(r);
    }
    Ok(())
}

// ── Guided resume/restart ────────────────────────────────────────────────────
//
// The resume/restart flow (zombie auto-reconcile, daemon /resume, attach) lives
// in the sibling `guided_resume` module (#2001) to keep both files under the
// 500-SLOC production cap. The picker calls
// `super::guided_resume::resume_guided_session`; the pure seams
// (`needs_restart`, `is_zombie`, `plan_resume`, `ResumeAction`) are unit-tested
// there directly.

// ── Fallback (daemon-unreachable / non-GitHub) path ─────────────────────────

/// Build the user-facing refusal notice for a non-GitHub-remote refusal.
///
/// Why: the non-GitHub-remote branch of [`fallback_protected`] previously
/// printed "daemon unreachable" wording, which is wrong when the daemon IS
/// running — the refusal is purely because `tm` only auto-manages GitHub
/// repositories. Extracting the message into a pure helper makes it
/// unit-testable without capturing stderr.
/// What: returns a three-line notice that names the non-GitHub remote as the
/// reason for refusal and reassures the operator that the live checkout was
/// not modified. The caller prints via `eprintln!` and then bails.
/// Test: `guided_non_github_refusal_message_*` in `tests_behavior_c_tests.rs`.
pub(crate) fn non_github_refusal_message(remote_desc: &str) -> String {
    format!(
        "tm: not auto-managing this project — `tm` auto-manages GitHub repositories only.\n\
         tm: detected non-GitHub remote: {remote_desc}.\n\
         tm: your live checkout was not touched. \
         (Use an explicit `tm` subcommand to work here manually.)"
    )
}

/// Return true when the remote URL targets GitHub (any transport or SSH alias).
///
/// Why: `parse_github_path` from `trusty-common` accepts *any* git remote URL,
/// not just GitHub ones. We need a host-specific guard so that non-GitHub git
/// projects (Gitea, GitLab, bare SSH, etc.) are refused rather than having a
/// clone attempt made against an unrecognised host (#1724 residual gap). We
/// also need to recognise GitHub repos accessed via multi-account SSH host
/// aliases (e.g. `git@github-duetto:owner/repo.git` via `~/.ssh/config`).
/// What: extracts the host via [`github_host`], then accepts it when:
///   (a) host == `"github.com"` (the canonical GitHub host), OR
///   (b) host starts with `"github-"` or `"github_"` (SSH alias convention —
///       e.g. `github-duetto`, `github-work`, `github_personal`).
/// Decision — GitHub Enterprise (`github.mycompany.com`): NOT matched. GHE
/// uses a dot separator after `github`, and the broader managed-clone
/// infrastructure is not tested against GHE; erring on the side of safety
/// avoids an unexpected redirect for GHE users.
/// Does NOT match: `githubusercontent.com` (the `u` after `github` is
/// alphanumeric, so neither rule fires), `gitlab.com`, `bitbucket.org`, or
/// any other host.
/// Test: `is_github_remote_*` unit tests in `tests_behavior_c_tests.rs`;
/// `guided_fallback_never_pollutes_github_git_checkout` (real `github.com` URL);
/// `guided_fallback_blocks_non_github_git_checkout` (Gitea URL must be blocked).
pub(crate) fn is_github_remote(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let host = github_host(&lower);
    host == "github.com" || host.starts_with("github-") || host.starts_with("github_")
}

/// Detect the git working-tree root at any depth by shelling to git.
///
/// Why: `cwd.join(".git").exists()` only fires when `cwd` IS the repo root.
/// Operators commonly run `tm` from a nested subdirectory (e.g. `~/project/src/`);
/// the shallow check would miss the repo, bypass the live-checkout guard, and
/// allow `launch(None)` to write framework files into that subdir (#1724 gap).
/// What: runs `git -C <cwd> rev-parse --show-toplevel`; returns the absolute
/// repo root on success, `None` when cwd is not inside any git working tree
/// (plain directory or bare repo). Bare repos (`--git-dir` succeeds but
/// `--show-toplevel` does not) are treated as non-git — deploying into a bare
/// repo has no live-checkout to protect.
/// Test: `guided_fallback_blocks_github_git_from_subdirectory` calls
/// `fallback_protected` from a nested subdir and asserts the protection fires.
fn find_git_root(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let root = String::from_utf8_lossy(&out.stdout);
    let root = root.trim();
    (!root.is_empty()).then_some(std::path::PathBuf::from(root))
}

/// Daemon-unreachable fallback that protects ALL live git checkouts (#1724).
///
/// Why: the guided default MUST NOT deploy framework files into ANY git
/// project's live checkout, even when the daemon is down. The invariant is:
/// if cwd is inside a git working tree (at ANY depth), it is never written to.
/// What: three-way dispatch on cwd:
///   (1) Inside a GitHub-backed git working tree → delegate to
///       [`redirect_to_managed_clone`] which provisions (or reuses) the protected
///       base clone and per-session worktree, then calls `launch()` against THAT
///       workspace, never the live checkout. Uses the repo ROOT (not cwd) to
///       read the origin remote — correct even from subdirectories.
///   (2) Inside a non-GitHub git working tree (non-GitHub remote or no remote)
///       → return an actionable `Err`; the live checkout is never touched.
///   (3) Not inside any git working tree (plain directory or bare repo) →
///       classic `tm launch` path; there is no live checkout to protect,
///       consistent with the daemon's local-path fast path.
/// Test: `guided_fallback_never_pollutes_github_git_checkout`,
/// `guided_fallback_blocks_non_github_git_checkout`,
/// `guided_fallback_blocks_github_git_from_subdirectory`, and
/// `guided_fallback_redirect_success_worktree_not_live_checkout` in
/// `tests_behavior_b_tests.rs`.
pub(crate) async fn fallback_protected(
    client: &reqwest::Client,
    url: &str,
    cwd: &std::path::Path,
) -> anyhow::Result<()> {
    if let Some(git_root) = find_git_root(cwd) {
        // cwd is inside a git working tree (at any depth) — protect it.
        // Read the origin remote from the REPO ROOT so subdirectory calls
        // find the same remote as top-level calls would.
        //
        // NOTE: `parse_github_path` accepts *any* remote URL (not just GitHub);
        // we therefore gate on `is_github_remote` (github.com substring) to
        // distinguish GitHub from Gitea/GitLab/bare-SSH remotes.
        let origin_url = trusty_mpm::daemon::managed_routes::inproject::get_origin_url(&git_root);

        if let Some(ref raw_url) = origin_url
            && is_github_remote(raw_url)
        {
            // GitHub project: redirect deploy to the protected managed clone.
            return redirect_to_managed_clone(client, url, cwd, raw_url).await;
        }

        // Non-GitHub remote or no remote: refuse to write to the live tree.
        // NOTE: the daemon's reachability is irrelevant here — this branch is
        // reached even when the daemon is UP. The real reason for refusal is
        // that `tm` only auto-manages GitHub repositories (#1777).
        let remote_desc = origin_url.as_deref().unwrap_or("(no remote configured)");
        eprintln!("{}", non_github_refusal_message(remote_desc));
        anyhow::bail!(
            "not auto-managing: live git checkout at '{}' is a non-GitHub repository — \
             auto-managed clones require a GitHub remote; \
             use an explicit `tm` subcommand to work here manually",
            git_root.display()
        );
    }

    // Not inside a git working tree: print a helpful note and exit cleanly (#1839).
    eprintln!("{}", super::misc::NON_GIT_FALLBACK_HINT);
    Ok(())
}

/// Redirect the guided-default fallback to the protected managed-clone workspace.
///
/// Why: when the daemon is unreachable and the current directory is a GitHub-backed
/// git project, framework files must go into the managed-clone workspace
/// (`~/trusty-mpm-projects/<owner>/<repo>/.worktrees/<session-id>/`), never
/// into the operator's live checkout (#1724, #1803).
/// What: (1) parses `owner/repo` from `origin_url`; (2) ensures the protected
/// base clone exists (`ensure_base_clone` is idempotent — returns immediately when
/// the clone already exists); (3) creates a per-session git worktree inside the
/// base clone; (4) calls `launch()` with the worktree path as the target directory.
/// If any step fails (unparseable URL, clone error, worktree error), the function
/// returns `Err` with an actionable message — the live checkout is never touched.
/// Test: `guided_fallback_never_pollutes_github_git_checkout`.
async fn redirect_to_managed_clone(
    client: &reqwest::Client,
    url: &str,
    cwd: &std::path::Path,
    origin_url: &str,
) -> anyhow::Result<()> {
    use trusty_mpm::daemon::managed_routes::inproject;
    use trusty_mpm::session_manager::ManagedSessionId;

    // Parse owner/repo from the GitHub remote URL.
    let Some(gh) = trusty_common::github_path::parse_github_path(origin_url) else {
        eprintln!(
            "tm: cannot determine GitHub project from remote URL '{origin_url}'.\n\
             Start the daemon first with `tm start`, then run `tm` again."
        );
        anyhow::bail!(
            "daemon unreachable: cannot parse GitHub remote URL as owner/repo — run `tm start` first"
        );
    };

    // Ensure the protected base clone exists. Idempotent: returns Ok immediately
    // when the clone is already present; clones once on first invocation.
    let base = inproject::base_clone_path(&gh.owner, &gh.repo);
    eprintln!(
        "tm: daemon unreachable — redirecting to protected managed clone\n\
         tm: base clone: {}",
        base.display()
    );
    if let Err(e) = inproject::ensure_base_clone(origin_url, &base) {
        eprintln!(
            "tm: could not set up base clone for {}/{}: {e}\n\
             Start the daemon first with `tm start`, then run `tm` again.",
            gh.owner, gh.repo
        );
        anyhow::bail!("failed to set up managed base clone: {e}");
    }

    // Create a per-session worktree branched from the base clone.
    let session_id = ManagedSessionId::new();
    // NOTE (#2032): this daemon-unreachable fallback path is NOT the managed
    // SessionManager spawn flow — it has no session manager to resolve a
    // semantic tmux name from, so it deliberately keeps the pre-#2032
    // UUID-named worktree here. Only the daemon's `spawn_managed_inproject`
    // path (`daemon::managed_routes::lifecycle`) uses the new semantic-name
    // worktree layout.
    let worktree = match inproject::create_session_worktree(&base, &session_id.to_string()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "tm: could not create per-session worktree: {e}\n\
                 Start the daemon first with `tm start`, then run `tm` again."
            );
            anyhow::bail!("failed to create session worktree: {e}");
        }
    };

    eprintln!(
        "tm: launching in protected workspace (live checkout at {} is untouched)\n\
         tm: session worktree: {}",
        cwd.display(),
        worktree.display()
    );

    // Launch in the session worktree — not the live checkout.
    let dir = worktree.to_string_lossy().to_string();
    super::launch::launch(client, url, Some(dir), None).await
}
