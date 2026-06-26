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
//! path; it is also the target for non-GitHub projects. The TTY picker is
//! split into a pure `parse_picker_choice` + an I/O driver `run_tty_picker`.
//!
//! Test: unit tests for detection and fallback live in `tests_behavior_b_tests.rs`
//! (#1724 regression suite) and `tests_behavior_c_tests.rs` (#1705 new UX suite);
//! the managed-spawn integration path is exercised by `tests/session_manager_mvp.rs`.

use anyhow::Context as _;
use serde::Deserialize;

use crate::formatters::banner::tmux_has_session;

/// Response shape for `POST /api/v1/sessions/managed` (subset we need).
///
/// Why: a local type avoids depending on the daemon's internal DTO from the
/// CLI binary crate; the fields we care about are `name` and `state`.
/// What: mirrors `daemon::managed_routes::SpawnResponse` for the two fields
/// the guided default uses.
/// Test: covered indirectly by `launch_new_session_and_attach`.
#[derive(Debug, Deserialize)]
struct SpawnManagedResponse {
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
}

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
/// have to remember `tm sessions new`, `tm connect`, or `tm attach <name>` —
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
    let cwd = std::env::current_dir().context("cannot resolve current directory")?;
    let workdir = cwd.to_string_lossy().to_string();
    eprintln!("tm: no subcommand — using guided default for {workdir}");

    // Try the rich UX when CWD is a GitHub-backed git project.
    if let Some((source_id, workspace, git_root)) = derive_project(&cwd) {
        // Pass the git root as repo_url so daemon detects .git at root even
        // when tm is invoked from a subdirectory (#1705 LOW fix).
        let repo_url = git_root.to_string_lossy().to_string();
        match list_project_sessions(client, url, &source_id).await {
            Ok(sessions) => {
                use std::io::IsTerminal as _;
                // tty_gate prints project context; returns true when stdin is a
                // TTY and the picker should run, false for non-TTY exit.
                if !tty_gate(
                    std::io::stdin().is_terminal(),
                    &source_id,
                    &workspace,
                    &sessions,
                ) {
                    return Ok(());
                }
                // Show the compact info box before the TTY picker.
                let daemon = crate::formatters::info_box::DaemonInfo::from_lock_file()
                    .with_count(sessions.len());
                crate::formatters::info_box::print_info_box(
                    &cwd.to_string_lossy(),
                    &source_id,
                    false,
                    &daemon,
                );
                return run_tty_picker(client, url, &repo_url, &sessions).await;
            }
            Err(e) => {
                eprintln!("tm: daemon unreachable ({e}); falling back");
            }
        }
    }

    // Daemon unreachable OR not a GitHub project: protected fallback (#1724).
    // For GitHub projects this redirects to the managed-clone workspace.
    // For non-GitHub git projects it refuses (live-checkout guard).
    // For non-git directories it falls through to the classic `tm launch` path.
    fallback_protected(client, url, &cwd).await
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
/// `workspace_path = <repos_root>/owner/repo`, and `git_root` is the absolute
/// path of the repository root.
/// Test: `guided_derive_project_returns_none_for_non_git_dir`,
/// `guided_derive_project_rejects_non_github_remote`,
/// `guided_derive_project_accepts_github_https_remote`,
/// `guided_derive_project_returns_some_from_subdir`.
pub(crate) fn derive_project(
    cwd: &std::path::Path,
) -> Option<(String, std::path::PathBuf, std::path::PathBuf)> {
    use trusty_mpm::daemon::managed_routes::inproject;
    let git_root = find_git_root(cwd)?;
    let origin_url = inproject::get_origin_url(&git_root)?;
    if !is_github_remote(&origin_url) {
        return None;
    }
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
        false
    } else {
        true
    }
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
                "tm:   [{}] {}  state={}  last={}",
                i + 1,
                s.name,
                s.state,
                activity
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
/// without requiring the operator to remember session names or UUIDs.
/// What: prints the menu, reads one line from stdin, delegates to
/// [`parse_picker_choice`], and dispatches. Side-effect-free parse logic lives
/// in `parse_picker_choice` and is unit-tested independently.
///   • `Resume(i)` → [`tmux_attach`] the session at index `i`;
///   • `LaunchNew` → [`launch_new_session_and_attach`];
///   • `Quit` / `Unrecognised` → print notice and return `Ok`.
/// Test: `parse_picker_choice` is the testable seam; I/O path is exercised by
/// manual smoke tests and the e2e suite.
async fn run_tty_picker(
    client: &reqwest::Client,
    url: &str,
    repo_url: &str,
    sessions: &[trusty_mpm::client::ManagedSessionSummary],
) -> anyhow::Result<()> {
    eprintln!();
    let new_idx = sessions.len() + 1;
    if sessions.is_empty() {
        eprintln!("tm:   [Enter] launch new session");
        eprintln!("tm:   [q]     quit");
    } else {
        for (i, s) in sessions.iter().enumerate() {
            // Show "restart" for sessions that are stopped/errored — they have no
            // live tmux session and will be restarted via the daemon (#1742).
            let verb = if matches!(s.state.as_str(), "stopped" | "errored") {
                "restart"
            } else {
                "resume"
            };
            eprintln!("tm:   [{}] {} {} ({})", i + 1, verb, s.name, s.state);
        }
        eprintln!("tm:   [{new_idx}] launch new session");
        eprintln!("tm:   [q] quit");
        eprintln!("tm: default: [1] resume/restart most recent");
    }
    eprint!("tm: > ");

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("failed to read choice from stdin")?;

    match parse_picker_choice(&line, sessions.len()) {
        PickerDecision::Quit => {
            eprintln!("tm: quit.");
            Ok(())
        }
        // #1742: route through daemon resume when the session is stopped or its
        // tmux session is absent — never raw-attach a non-live session.
        PickerDecision::Resume(i) => resume_guided_session(client, url, &sessions[i]).await,
        PickerDecision::LaunchNew => launch_new_session_and_attach(client, url, repo_url).await,
        PickerDecision::Unrecognised => {
            eprintln!("tm: unrecognised choice '{}'; quitting.", line.trim());
            Ok(())
        }
    }
}

/// Decide whether a guided resume must restart the session through the daemon.
///
/// Why: a stopped/errored managed session has no live tmux session; calling
/// `tmux attach-session` against it fails with "can't find session" (#1742).
/// Both the daemon-recorded state and the live tmux liveness are checked
/// independently — either condition alone requires the daemon restart path.
/// What: returns `true` when `state` is `"stopped"` or `"errored"`, OR when
/// `tmux_live` is `false`. Returns `false` only when the session is in an active
/// state AND its tmux session is confirmed live.
/// Test: `guided_resume_needs_restart_*` in `tests_behavior_c_tests.rs`.
pub(crate) fn needs_restart(state: &str, tmux_live: bool) -> bool {
    !tmux_live || matches!(state, "stopped" | "errored")
}

/// Restart (if needed) then attach to a managed session from the guided picker.
///
/// Why: the guided picker must handle stopped sessions gracefully (#1742). A
/// direct `tmux attach-session` against a stopped session exits with failure
/// ("can't find session"); this function first asks the daemon to restart the
/// tmux session when the liveness or state check shows it is absent, then
/// attaches. On daemon errors a clear, actionable message is printed and `Err`
/// is returned — the raw tmux failure is never surfaced.
/// What: (1) calls `tmux_has_session` to check liveness; (2) if restart is
/// needed, POSTs `{url}/api/v1/sessions/managed/{id}/resume`; (3) on daemon
/// error (404/409/network) prints an actionable hint and returns `Err`; (4) on
/// success (or no restart needed), delegates to `tmux_attach`.
/// Test: `needs_restart` is the testable pure seam; the I/O path is exercised by
/// the e2e suite and manual smoke tests.
async fn resume_guided_session(
    client: &reqwest::Client,
    url: &str,
    session: &trusty_mpm::client::ManagedSessionSummary,
) -> anyhow::Result<()> {
    let tmux_live = tmux_has_session(&session.name);
    if needs_restart(&session.state, tmux_live) {
        eprintln!(
            "tm: session '{}' is {} (tmux session {}); restarting via daemon…",
            session.name,
            session.state,
            if tmux_live { "present" } else { "absent" },
        );
        let resp = match client
            .post(format!(
                "{url}/api/v1/sessions/managed/{}/resume",
                session.id
            ))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "tm: daemon unreachable — cannot restart session '{}': {e}",
                    session.name
                );
                eprintln!("tm: start the daemon with `tm start`, then run `tm` again.");
                anyhow::bail!("daemon unreachable; cannot restart stopped session: {e}");
            }
        };
        match resp.status() {
            reqwest::StatusCode::NOT_FOUND => {
                eprintln!(
                    "tm: session '{}' not found on daemon; it may have been decommissioned.",
                    session.name
                );
                eprintln!(
                    "tm: run `tm sessions ls` to see current sessions, \
                     or press [Enter] to launch a new one."
                );
                anyhow::bail!("session '{}' not found on daemon", session.name);
            }
            reqwest::StatusCode::CONFLICT => {
                let msg = resp.text().await.unwrap_or_default();
                eprintln!("tm: cannot restart session '{}': {}", session.name, msg);
                eprintln!("tm: run `tm sessions ls` to see the current state.");
                anyhow::bail!("cannot restart session '{}': {msg}", session.name);
            }
            s if !s.is_success() => {
                eprintln!(
                    "tm: daemon returned {s} restarting session '{}'; cannot attach.",
                    session.name
                );
                eprintln!("tm: start the daemon with `tm start`, then run `tm` again.");
                anyhow::bail!("daemon returned {s} restarting session '{}'", session.name);
            }
            _ => {
                eprintln!("tm: session restarted — attaching…");
            }
        }
    }
    tmux_attach(&session.name)
}

/// Invoke `tmux attach-session -t <name>` and await exit.
///
/// Why: resuming a session means handing the terminal over to tmux.
/// What: shells out to `tmux attach-session -t <name>` and waits; returns
/// `Err` if tmux exits with a non-zero status.
/// Test: exercised indirectly by the picker flow; mocked in unit tests via
/// process stubs.
fn tmux_attach(name: &str) -> anyhow::Result<()> {
    eprintln!("tm: attaching to session '{name}'");
    let status = std::process::Command::new("tmux")
        .args(["attach-session", "-t", name])
        .status()
        .context("failed to invoke tmux")?;
    if !status.success() {
        anyhow::bail!("tmux attach-session exited with failure");
    }
    Ok(())
}

/// POST a new managed session to the daemon and attach to it.
///
/// Why: "launch new" in the picker must use the daemon's protected managed-clone
/// spawn path — NEVER write framework files into the live checkout (#1724).
/// What: POSTs `{"repo_url": repo_url, "ref": "HEAD", "task": ""}` to
/// `/api/v1/sessions/managed`. `repo_url` MUST be the git working-tree root
/// (not a subdirectory), so the daemon finds `.git` and sets `source_id`
/// correctly — enabling future `list_project_sessions` to show this session.
/// The daemon provisions a per-session worktree in the base clone and returns
/// the session name. Then attaches via [`tmux_attach`].
/// Test: covered by the `POST /api/v1/sessions/managed` integration tests in
/// `tests/session_manager_mvp.rs`.
async fn launch_new_session_and_attach(
    client: &reqwest::Client,
    url: &str,
    repo_url: &str,
) -> anyhow::Result<()> {
    eprintln!("tm: launching new session…");
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed"))
        .json(&serde_json::json!({
            "repo_url": repo_url,
            "ref": "HEAD",
            "task": "",
        }))
        .send()
        .await
        .context("managed spawn POST failed")?;

    if !resp.status().is_success() {
        anyhow::bail!("daemon returned {} for managed spawn", resp.status());
    }
    let body: SpawnManagedResponse = resp
        .json()
        .await
        .context("failed to parse managed spawn response")?;
    if body.name.is_empty() {
        anyhow::bail!("daemon returned empty session name from managed spawn");
    }
    eprintln!("tm: session '{}' created ({})", body.name, body.state);
    tmux_attach(&body.name)
}

// ── Fallback (daemon-unreachable / non-GitHub) path ─────────────────────────

/// Return true when the remote URL targets github.com (any transport).
///
/// Why: `parse_github_path` from `trusty-common` accepts *any* git remote URL,
/// not just GitHub ones. We need a host-specific guard so that non-GitHub git
/// projects (Gitea, GitLab, bare SSH, etc.) are refused rather than having a
/// clone attempt made against an unrecognised host (#1724 residual gap).
/// What: case-insensitive substring match for `"github.com"` in the raw URL —
/// catches both HTTPS (`https://github.com/…`) and SSH (`git@github.com:…`)
/// forms without adding a URL-parsing dependency.
/// Known limitation: a hostname such as `github.mycompany.com` does NOT match
/// since `"github.mycompany.com".contains("github.com")` is false. URLs like
/// `https://notgithub.com/a/b.git` also do not match. Both cases fall through
/// to the non-GitHub refusal path — safe but not redirected.
/// Test: `guided_fallback_blocks_non_github_git_checkout` verifies a Gitea URL
/// is NOT treated as GitHub; `guided_fallback_never_pollutes_github_git_checkout`
/// verifies a real GitHub URL is recognised.
fn is_github_remote(url: &str) -> bool {
    url.to_ascii_lowercase().contains("github.com")
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
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout);
    let root = root.trim();
    if root.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(root))
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
        let remote_desc = origin_url.as_deref().unwrap_or("(no remote configured)");
        eprintln!(
            "tm: daemon unreachable — refusing to deploy into live git checkout.\n\
             tm: Auto-protected managed clones require a GitHub remote \
             (detected remote: {remote_desc}).\n\
             tm: Start the daemon with `tm start`, then run `tm` again."
        );
        anyhow::bail!(
            "daemon unreachable: live git checkout at '{}' is protected — \
             auto-managed clones require a GitHub remote; \
             start the daemon with `tm start` first",
            git_root.display()
        );
    }

    // Not inside a git working tree: classic tm launch path.
    super::launch::launch(client, url, None, None).await
}

/// Redirect the guided-default fallback to the protected managed-clone workspace.
///
/// Why: when the daemon is unreachable and the current directory is a GitHub-backed
/// git project, framework files must go into the managed-clone workspace
/// (`~/trusty-tools/repos/<owner>/<repo>/worktrees/<session-id>/`), never
/// into the operator's live checkout (#1724).
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
    let worktree = match inproject::create_session_worktree(&base, &session_id) {
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
