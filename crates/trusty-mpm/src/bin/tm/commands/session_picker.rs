//! Shared managed-session picker + fetch helpers.
//!
//! Why: two entry points now surface the interactive session picker — bare `tm`
//! (the project-scoped guided default in `guided.rs`) and the top-level `tm ls`
//! connector (fleet-wide, optionally scoped). Extracting the numbered-menu loop,
//! the pure choice-parser, and the fetch-and-filter step into one module keeps
//! both call sites on a single implementation (no divergence, no duplication) and
//! keeps `guided.rs` under the 500-SLOC production cap.
//!
//! What: [`parse_picker_choice`] is the pure input→decision seam; [`run_tty_picker`]
//! is the I/O driver that renders the menu, reads stdin, and dispatches to
//! resume/launch; [`fetch_live_sessions`] is the single GET+filter path shared by
//! the static `tm session ls` renderer and the picker; [`run_ls_connector`] is the
//! `tm ls` orchestrator that decides between the interactive picker and static
//! output based on the TTY/`--json`/`--all`/session-count gate
//! ([`should_show_picker`]).
//!
//! Test: `parse_picker_choice` unit tests live in `tests_behavior_c_tests.rs`
//! (re-exported through `guided`); the TTY gate is unit-tested by
//! `ls_connector_should_show_picker_*` in `tests_behavior_d_tests.rs`; the I/O
//! path is exercised by the e2e suite and manual smoke tests.

use anyhow::Context as _;
use std::io::IsTerminal as _;

use trusty_mpm::client::{ManagedListResponse, ManagedSessionSummary};

use super::managed::filter_live_sessions;

/// Decision returned by [`parse_picker_choice`].
///
/// Why: extracting the parse-and-decide logic from the I/O driver makes it
/// unit-testable without stdin/tmux. The driver calls parse, checks the variant,
/// and shells out only for Resume and LaunchNew.
/// What: five variants cover every valid and invalid input the picker can
/// receive. [`Self::ConfirmRestart`] is the #2148 safety default: a bare Enter
/// that WOULD have silently restarted (destructively recreated the tmux pane
/// of) a stopped/errored session instead asks for an explicit numeric choice.
/// Test: `guided_picker_bare_enter_no_sessions_launches_new`,
/// `guided_picker_bare_enter_live_session_resumes_first`,
/// `guided_picker_bare_enter_stopped_session_requires_confirm`,
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
    /// Bare Enter was pressed but the 0-based-indexed session at this slot is
    /// stopped/errored — resuming it would KILL and recreate its tmux pane
    /// (or, if the pane already happened to be dead, spawn a fresh one). #2148:
    /// this is no longer an implicit default; the operator must type the
    /// number explicitly to confirm the restart.
    ConfirmRestart(usize),
    /// Delete the session at this 0-based index (#2304). Entered as `d<N>`
    /// (e.g. `d2`) or `d <N>`. The driver runs a confirm prompt — and, for a
    /// running session, a force-confirm — before touching the store, so this
    /// variant only signals intent, never an unconditional destructive action.
    Delete(usize),
}

/// Scope describing which sessions the picker operates over and how to launch new.
///
/// Why: the picker serves both a single-project context (bare `tm`, where a
/// `repo_url` is known so "launch new" can spawn into that project) and a
/// fleet-wide/filtered context (`tm ls`, where sessions may span projects and no
/// single launch target exists). Threading the scope through one struct keeps
/// [`run_tty_picker`] agnostic to its caller.
/// What: `source_id` filters the daemon session list (`None` = every managed
/// session); `repo_url` is the launch-new target (`None` disables launch-new with
/// an actionable hint instead of attaching to the wrong project).
/// Test: constructed by `guided::try_show_picker` and [`run_ls_connector`];
/// behavior is covered by the picker's e2e path.
pub(crate) struct PickerScope {
    /// `owner/repo` slug to filter by, or `None` for every managed session.
    pub(crate) source_id: Option<String>,
    /// Git-root path used as the launch-new target, or `None` to disable it.
    pub(crate) repo_url: Option<String>,
}

impl PickerScope {
    /// Build a single-project scope (bare `tm` guided default).
    ///
    /// Why: the guided default always knows both the project slug and the git
    /// root, so both fields are always populated.
    /// What: returns a scope filtered to `source_id` with `repo_url` as the
    /// launch target.
    /// Test: exercised by `guided::try_show_picker`.
    pub(crate) fn project(source_id: &str, repo_url: &str) -> Self {
        Self {
            source_id: Some(source_id.to_string()),
            repo_url: Some(repo_url.to_string()),
        }
    }
}

/// GET the raw managed-session list body, optionally scoped by `source_id`.
///
/// Why: the `--json` passthrough needs the byte-for-byte daemon response while
/// the table/picker paths need the deserialized form — both must issue exactly
/// one GET. Returning the raw text lets each caller decide.
/// What: GETs `/api/v1/sessions/managed`, appending `?source_id=` when a filter
/// is active, and returns the response body as a `String` after status checks.
/// Test: HTTP round-trip covered by `tests/session_manager_mvp.rs`.
pub(crate) async fn fetch_managed_raw(
    client: &reqwest::Client,
    url: &str,
    source_id: Option<&str>,
) -> anyhow::Result<String> {
    let endpoint = format!("{url}/api/v1/sessions/managed");
    let mut req = client.get(&endpoint);
    if let Some(sid) = source_id {
        req = req.query(&[("source_id", sid)]);
    }
    Ok(req.send().await?.error_for_status()?.text().await?)
}

/// Parse a raw managed-session list body into a scoped `Vec` for display.
///
/// Why: the static table and the picker share one filtering/sorting policy so
/// the two views never diverge (#1809/#1841).
/// What: deserializes `ManagedListResponse`; when `all` is false, drops
/// decommissioned tombstones via [`filter_live_sessions`]; when `all` is true,
/// keeps every session but stable-sorts tombstones to the end.
/// Test: `picker_filter_excludes_decommissioned_keeps_active`,
/// `ls_source_id_filter_selects_correct_slug` in `tests_behavior_c_tests.rs`.
pub(crate) fn parse_scoped_sessions(
    raw: &str,
    all: bool,
) -> anyhow::Result<Vec<ManagedSessionSummary>> {
    let fetched = serde_json::from_str::<ManagedListResponse>(raw)?.sessions;
    let sessions = if all {
        let mut s = fetched;
        s.sort_by_key(|sess| u8::from(sess.state == "decommissioned"));
        s
    } else {
        filter_live_sessions(fetched)
    };
    Ok(sessions)
}

/// Fetch and filter managed sessions in one call — the shared picker fetch path.
///
/// Why: the interactive picker (both call sites) needs the deserialized, filtered
/// session list; combining the GET and the parse keeps re-fetch-after-detach a
/// single call and guarantees identical scoping to the static list.
/// What: [`fetch_managed_raw`] then [`parse_scoped_sessions`]. The picker always
/// requests live-only (`all = false`) — a decommissioned tombstone is never a
/// resume target.
/// Test: HTTP path in `tests/session_manager_mvp.rs`; the parse/filter seam is
/// unit-tested via `parse_scoped_sessions`.
pub(crate) async fn fetch_live_sessions(
    client: &reqwest::Client,
    url: &str,
    source_id: Option<&str>,
    all: bool,
) -> anyhow::Result<Vec<ManagedSessionSummary>> {
    let raw = fetch_managed_raw(client, url, source_id).await?;
    parse_scoped_sessions(&raw, all)
}

/// Parse one line of picker input into a [`PickerDecision`].
///
/// Why: separating parse-and-decide from the I/O driver makes the dispatch
/// logic unit-testable without needing a real stdin, tmux, or daemon. Folding
/// `first_needs_restart` in here (rather than deciding safety in the I/O
/// driver) keeps the destructive-default guard (#2148) exhaustively
/// unit-testable alongside every other picker-input case.
/// What: `session_count` is the number of existing sessions in the menu (the
/// menu slot `session_count + 1` is always "launch new"); `first_needs_restart`
/// is true when the session at index 0 is `stopped`/`errored` — i.e. resuming
/// it goes through the daemon's restart path, which can recreate its tmux pane
/// (see [`super::guided_resume::needs_restart`]).
///   • `"q"` / `"Q"` → `Quit`
///   • empty / whitespace, `session_count == 0` → `LaunchNew`
///   • empty / whitespace, `session_count > 0`, session 0 is live → `Resume(0)`
///   • empty / whitespace, `session_count > 0`, session 0 needs a restart →
///     `ConfirmRestart(0)` (#2148: no longer an implicit destructive default)
///   • `N` (1..=session_count) → `Resume(N-1)` (0-based index) — an EXPLICIT
///     numeric choice always restarts/resumes directly, confirm or not
///   • `session_count + 1` → `LaunchNew`
///   • `d<N>` / `d <N>` (1..=session_count) → `Delete(N-1)` (#2304); the driver
///     still runs a confirm/force-confirm prompt before deleting
///   • anything else → `Unrecognised`
/// Test: `guided_picker_bare_enter_no_sessions_launches_new`,
/// `guided_picker_bare_enter_live_session_resumes_first`,
/// `guided_picker_bare_enter_stopped_session_requires_confirm`,
/// `guided_picker_q_returns_quit`, `guided_picker_q_uppercase_returns_quit`,
/// `guided_picker_numeric_valid_resumes`, `guided_picker_numeric_launch_new`,
/// `guided_picker_out_of_range_unrecognised`,
/// `guided_picker_non_numeric_unrecognised`.
pub(crate) fn parse_picker_choice(
    line: &str,
    session_count: usize,
    first_needs_restart: bool,
) -> PickerDecision {
    let choice = line.trim();
    if choice.eq_ignore_ascii_case("q") {
        return PickerDecision::Quit;
    }
    // #2304: `d<N>` / `d <N>` deletes the session at menu slot N. Parsed before
    // the numeric-resume branch so the `d` prefix is unambiguous.
    if let Some(rest) = choice.strip_prefix(['d', 'D']) {
        if let Ok(n) = rest.trim().parse::<usize>()
            && n >= 1
            && n <= session_count
        {
            return PickerDecision::Delete(n - 1);
        }
        return PickerDecision::Unrecognised;
    }
    if choice.is_empty() {
        if session_count == 0 {
            return PickerDecision::LaunchNew;
        }
        return if first_needs_restart {
            PickerDecision::ConfirmRestart(0)
        } else {
            PickerDecision::Resume(0)
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
/// list (via [`fetch_live_sessions`], honoring the scope) so the next iteration
/// shows current state. Exits cleanly on `Quit`, EOF (Ctrl-D), or unrecognised
/// input; propagates attach/launch errors.
///   • `Resume(i)` → [`super::guided_resume::resume_guided_session`] which
///     handles daemon restart when needed and then attaches internally;
///   • `LaunchNew` → [`super::guided_launch::launch_new_session_and_attach`] when
///     the scope carries a `repo_url`; otherwise prints an actionable hint and
///     redisplays the menu (fleet-wide `tm ls` has no single launch target);
///   • `ConfirmRestart(i)` (#2148) → print a one-line "type the number to
///     confirm" notice and redisplay the SAME menu — no daemon round-trip;
///   • `Delete(i)` (#2304) → [`super::picker_delete::confirm_and_delete`] runs a
///     confirm (force-confirm for a running session) then the managed→local
///     routed delete; redisplay the re-fetched menu afterwards;
///   • `Quit` / EOF / `Unrecognised` → print notice and return `Ok`.
/// Test: `parse_picker_choice` is the testable seam; I/O path is exercised by
/// manual smoke tests and the e2e suite.
pub(crate) async fn run_tty_picker(
    client: &reqwest::Client,
    url: &str,
    scope: &PickerScope,
    mut sessions: Vec<ManagedSessionSummary>,
) -> anyhow::Result<()> {
    loop {
        eprintln!();
        let new_idx = sessions.len() + 1;
        // #2148: bare Enter must not silently restart (kill+recreate the tmux
        // pane of) a stopped/errored session — only used to pick the menu's
        // default hint and to gate `parse_picker_choice`'s bare-Enter branch.
        let first_needs_restart = sessions
            .first()
            .map(|s| super::guided_resume::needs_restart(&s.state))
            .unwrap_or(false);
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
            eprintln!("tm:   [d<N>] delete session N (e.g. d1)");
            eprintln!("tm:   [q] quit");
            if first_needs_restart {
                // #2148: no implicit destructive default — an explicit [1] is required.
                eprintln!("tm: [Enter] does NOT restart — type [1] to confirm the restart");
            } else {
                eprintln!("tm: default: [1] resume most recent");
            }
        }
        eprint!("tm: > ");

        let mut line = String::new();
        let n = std::io::stdin()
            .read_line(&mut line)
            .context("failed to read choice from stdin")?;
        if n == 0 {
            break;
        } // EOF (Ctrl-D): exit cleanly.

        match parse_picker_choice(&line, sessions.len(), first_needs_restart) {
            PickerDecision::Quit => {
                eprintln!("tm: quit.");
                break;
            }
            // #1742: route through daemon resume when the session is stopped or its
            // tmux session is absent — never raw-attach a non-live session.
            PickerDecision::Resume(i) => {
                super::guided_resume::resume_guided_session(client, url, &sessions[i]).await?
            }
            PickerDecision::LaunchNew => match scope.repo_url.as_deref() {
                Some(repo) => {
                    super::guided_launch::launch_new_session_and_attach(client, url, repo).await?
                }
                None => {
                    // Fleet-wide `tm ls` has no single launch target — steer the
                    // operator to the project directory or an explicit spawn.
                    eprintln!(
                        "tm: launch-new is unavailable from this list view — run `tm` from a \
                         project directory, or `tm session new <repo-url>`."
                    );
                    continue;
                }
            },
            // #2148: bare Enter hit a session that needs a restart — ask for an
            // explicit choice instead of silently destroying its pane. No daemon
            // round-trip; redisplay the SAME menu.
            PickerDecision::ConfirmRestart(i) => {
                eprintln!(
                    "tm: '{}' requires a restart (its pane is gone or will be recreated) — \
                     bare Enter no longer does this automatically.",
                    sessions[i].name
                );
                eprintln!(
                    "tm: type [{}] to confirm the restart, or [q] to quit.",
                    i + 1
                );
                continue;
            }
            // #2304: delete the selected session. The confirm/force-confirm
            // prompt and the managed→local routing live in `picker_delete`; this
            // arm just runs it, then falls through to the re-fetch so the menu
            // reflects the removal. A cancel/refusal is a no-op re-fetch.
            PickerDecision::Delete(i) => {
                super::picker_delete::confirm_and_delete(client, url, &sessions[i]).await?;
            }
            PickerDecision::Unrecognised => {
                eprintln!("tm: unrecognised choice '{}'; quitting.", line.trim());
                break;
            }
        }

        // Detached or session ended — re-fetch the list before redisplaying.
        // #1809: the shared fetch path applies the same live-only tombstone filter.
        sessions = fetch_live_sessions(client, url, scope.source_id.as_deref(), false).await?;
    }
    Ok(())
}

/// Decide whether `tm ls` should open the interactive picker or print statically.
///
/// Why: a testable seam that folds every gate into one pure decision so the
/// non-TTY / `--json` / `--all` / empty-list branches are unit-testable without a
/// live terminal or daemon. Requiring BOTH stdin and stdout to be TTYs is the
/// anti-hang guarantee: a piped input (would EOF) or a piped output (must stay a
/// clean pipeable table) both fall through to static output.
/// What: returns `true` only when stdin AND stdout are TTYs, neither `--json` nor
/// `--all` was requested, and at least one session exists. `--all` forces static
/// output because its purpose is the forensic full list (including
/// decommissioned tombstones), not connecting.
/// Test: `ls_connector_should_show_picker_*` in `tests_behavior_d_tests.rs`.
pub(crate) fn should_show_picker(
    stdin_tty: bool,
    stdout_tty: bool,
    json: bool,
    all: bool,
    session_count: usize,
) -> bool {
    stdin_tty && stdout_tty && !json && !all && session_count > 0
}

/// `tm ls` — the interactive managed-session connector (top-level).
///
/// Why: bare `tm ls` should do the most useful thing for connecting to the
/// managed fleet: on a real terminal it opens the session picker; piped or
/// scripted it degrades to the same static, pipeable list as `tm session ls`.
/// What: resolves the scope (`--current` derives `owner/repo` from the cwd git
/// remote, mirroring `tm session ls`); routes `--json`, `--all`, or any non-TTY
/// invocation straight to the static [`super::managed::session_ls`] renderer
/// (preserving its raw `--json` passthrough byte-for-byte); otherwise fetches the
/// live sessions once and either renders the static table (0 sessions) or opens
/// [`run_tty_picker`] (≥1 session). Launch-new inside the picker targets the cwd
/// project only when it is a GitHub-backed git checkout.
/// Test: parse tests `cli_parses_ls_*` and the gate tests
/// `ls_connector_should_show_picker_*` in `tests_behavior_d_tests.rs`.
pub(crate) async fn run_ls_connector(
    client: &reqwest::Client,
    url: &str,
    json: bool,
    source_id: Option<String>,
    current: bool,
    all: bool,
) -> anyhow::Result<()> {
    // `--current` derives the source_id from the cwd git remote, exactly like
    // `tm session ls --current`. `--source-id` and `--current` are mutually
    // exclusive at the clap layer, so at most one branch supplies a filter.
    let sid: Option<String> = if current {
        super::session::derive_source_id_from_cwd()
    } else {
        source_id
    };

    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();

    // Cheap pre-gate: `--json`, `--all`, or any non-interactive stream never
    // fetches for the picker — delegate straight to the static renderer, which
    // owns the raw `--json` passthrough and the `--all` tombstone sort.
    if json || all || !stdin_tty || !stdout_tty {
        return super::managed::session_ls(client, url, json, sid.as_deref(), all).await;
    }

    // Interactive stream: fetch the live sessions once. On any fetch error
    // (daemon unreachable, HTTP failure) fall back to the static renderer so the
    // operator sees the same actionable error rather than a bare picker crash.
    let sessions = match fetch_live_sessions(client, url, sid.as_deref(), false).await {
        Ok(s) => s,
        Err(_) => {
            return super::managed::session_ls(client, url, false, sid.as_deref(), false).await;
        }
    };

    if !should_show_picker(stdin_tty, stdout_tty, json, all, sessions.len()) {
        // 0 sessions on a TTY: print the static "no managed sessions" line rather
        // than an empty picker.
        super::managed::render_session_table(&sessions, sid.as_deref());
        return Ok(());
    }

    // ≥1 session on a real terminal → the interactive picker. Launch-new targets
    // the cwd project only when it is a GitHub-backed git checkout.
    let repo_url = std::env::current_dir()
        .ok()
        .and_then(|cwd| super::guided::derive_project(&cwd))
        .map(|(_sid, _workspace, git_root)| git_root.to_string_lossy().to_string());
    let scope = PickerScope {
        source_id: sid,
        repo_url,
    };
    run_tty_picker(client, url, &scope, sessions).await
}
