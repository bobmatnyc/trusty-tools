//! `tm ls` — the top-level managed-session connector.
//!
//! Why: split out of `session_picker.rs`, which sits at the 500-SLOC
//! production cap. The picker module owns the interactive loop and its pure
//! input→decision seam; deciding whether `tm ls` should even OPEN that loop —
//! scope resolution, the TTY/`--json`/`--all` gate, and the static-renderer
//! fallbacks — is a separate responsibility with one call site (`main.rs`),
//! so it moves rather than making the picker file grow.
//!
//! What: [`should_show_picker`] is the pure gate; [`run_ls_connector`] is the
//! orchestrator that applies it. Everything either touches — fetching,
//! filtering, sorting — still lives in [`super::session_picker`] and is called
//! from here, so there is one implementation of each.
//!
//! #7224: the interactive branch now opens the full-screen
//! [`super::session_tui`] rather than the line-based numbered picker. The
//! picker itself is unchanged and still serves bare `tm`
//! (`guided::try_show_picker`); what moved is only which surface `tm ls`
//! reaches. `--plain` is the new opt-out back to the static table.
//!
//! Test: the gate is unit-tested by `ls_connector_should_show_picker_*` in
//! `tests_behavior_d_tests.rs`; the orchestrator's argument parsing is covered
//! by `cli_parses_ls_*` in the same file, and its I/O path by the e2e suite.

use std::io::IsTerminal as _;

use super::session_picker::{PickerScope, SessionFilter, SessionSortArg, fetch_live_sessions};
use super::session_picker_order::{filter_sessions_by_term, sort_sessions};

/// Decide whether `tm ls` should open the interactive picker or print statically.
///
/// Why: a testable seam that folds every gate into one pure decision so the
/// non-TTY / `--json` / `--all` / `--attached` / empty-list branches are
/// unit-testable without a live terminal or daemon. Requiring BOTH stdin and
/// stdout to be TTYs is the anti-hang guarantee: a piped input (would EOF) or a
/// piped output (must stay a clean pipeable table) both fall through to static
/// output.
/// What: returns `true` only when stdin AND stdout are TTYs, none of `--json`,
/// `--all`, or `--attached` was requested, and at least one session exists.
/// `--all` forces static output because its purpose is the forensic full list
/// (including decommissioned tombstones), not connecting; `--attached` forces it
/// because the sessions it keeps are exactly the ones a client is already on, so
/// the picker's connect action has nothing left to do.
///
/// #7224: `plain` is the operator's explicit "give me the table, not the
/// interactive surface" — the escape hatch for a TTY invocation whose output is
/// meant to be READ rather than acted on. It sits with `json`/`all`/`attached`
/// rather than being a separate gate because they all answer the same question:
/// is this invocation a listing, or a connect?
/// Test: `ls_connector_should_show_picker_*` in `tests_behavior_d_tests.rs`.
pub(crate) fn should_show_picker(
    stdin_tty: bool,
    stdout_tty: bool,
    json: bool,
    all: bool,
    attached: bool,
    plain: bool,
    session_count: usize,
) -> bool {
    stdin_tty && stdout_tty && !json && !all && !attached && !plain && session_count > 0
}

/// Does this invocation print the static table and return before any TUI?
///
/// Why (#7224): raw mode is the one thing `tm ls` must never enter when its
/// output is not a terminal — a piped `tm ls | head` that switched the tty to
/// raw mode would leave the operator's shell without echo. Naming that branch
/// makes "a piped or `--plain` run never reaches the TUI" a unit test rather
/// than something to establish by reading the function. It is also the sole
/// path to [`super::managed::session_ls`] from here, so every static invocation
/// produces one renderer's bytes.
/// What: true for `--json`, `--all`, `--attached`, `--plain`, or either stream
/// not being a terminal. The complement is NOT [`should_show_picker`] — that
/// one additionally requires a non-empty fleet, because zero sessions on a TTY
/// prints the static "no managed sessions" line from a later branch instead.
/// Test: `plain_and_non_tty_reach_the_same_static_renderer`,
/// `ls_connector_should_show_picker_plain_static`.
pub(crate) fn prints_static_table(
    stdin_tty: bool,
    stdout_tty: bool,
    json: bool,
    all: bool,
    attached: bool,
    plain: bool,
) -> bool {
    json || all || attached || plain || !stdin_tty || !stdout_tty
}

/// `tm ls` — the interactive managed-session connector (top-level).
///
/// Why: bare `tm ls` should do the most useful thing for connecting to the
/// managed fleet: on a real terminal it opens the session TUI; piped,
/// scripted, or `--plain` it degrades to the same static, pipeable list as
/// `tm session ls`.
/// What: resolves the scope (`--current` derives `owner/repo` from the cwd git
/// remote, mirroring `tm session ls`); routes `--json`, `--all`, `--attached`, or
/// any non-TTY invocation straight to the static [`super::managed::session_ls`]
/// renderer (preserving its raw `--json` passthrough byte-for-byte); otherwise
/// fetches the live sessions once and either renders the static table (0
/// sessions) or opens the session TUI
/// ([`super::session_tui::run_session_tui`], #7224) for one or more.
/// `attached` is a pure listing filter and never reaches the TUI — see
/// [`should_show_picker`] — so the static renderer is the single place it is
/// applied.
/// Test: parse tests `cli_parses_ls_*` and the gate tests
/// `ls_connector_should_show_picker_*` in `tests_behavior_d_tests.rs`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_ls_connector(
    client: &reqwest::Client,
    url: &str,
    json: bool,
    source_id: Option<String>,
    current: bool,
    all: bool,
    attached: bool,
    plain: bool,
    sort: SessionSortArg,
    term: Option<SessionFilter>,
    no_prune: bool,
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

    // Cheap pre-gate: `--json`, `--all`, `--attached`, or any non-interactive
    // stream never fetches for the picker — delegate straight to the static
    // renderer, which owns the raw `--json` passthrough, the `--all` tombstone
    // sort, and the `-a` attached-only filter. `sort`/`term` ride along (the
    // static renderer applies them; `--json` ignores them, matching `--all`'s
    // existing "no effect on --json" precedent).
    // #7224: `--plain` joins the list, so the static renderer below is reached
    // by the SAME branch a piped invocation takes — byte-identical output, not
    // a second formatting path.
    if prints_static_table(stdin_tty, stdout_tty, json, all, attached, plain) {
        return super::managed::session_ls(
            client,
            url,
            json,
            sid.as_deref(),
            all,
            attached,
            sort,
            term,
            // #5950: the operator's explicit "this read must not mutate".
            no_prune,
        )
        .await;
    }

    // Interactive stream: fetch the live sessions once. On any fetch error
    // (daemon unreachable, HTTP failure) fall back to the static renderer so the
    // operator sees the same actionable error rather than a bare picker crash.
    let sessions = match fetch_live_sessions(client, url, sid.as_deref(), false).await {
        Ok(s) => s,
        Err(_) => {
            return super::managed::session_ls(
                client,
                url,
                false,
                sid.as_deref(),
                false,
                false,
                sort,
                term,
                no_prune,
            )
            .await;
        }
    };
    let mut sessions = filter_sessions_by_term(sessions, term.as_ref());
    sort_sessions(&mut sessions, sort);

    if !should_show_picker(
        stdin_tty,
        stdout_tty,
        json,
        all,
        attached,
        plain,
        sessions.len(),
    ) {
        // 0 sessions on a TTY: print the static "no managed sessions" line rather
        // than an empty picker.
        super::managed_render::render_session_table(&sessions, sid.as_deref());
        return Ok(());
    }

    // ≥1 session on a real terminal → the interactive TUI (#7224). Launch-new
    // targets the cwd project only when it is a GitHub-backed git checkout; the
    // TUI has no launch-new action of its own, so `repo_url` rides along only
    // for the scope the shared ordering helper reads.
    let repo_url = std::env::current_dir()
        .ok()
        .and_then(|cwd| super::guided::derive_project(&cwd))
        .map(|(_sid, _workspace, git_root)| git_root.to_string_lossy().to_string());
    // #3552: mutable — the scope carries the live sort/filter/pin state the
    // ordering helper reads on every frame, not just the CLI grammar's starting
    // view.
    let mut scope = PickerScope {
        source_id: sid,
        repo_url,
        sort,
        term,
        selected_id: None,
    };
    // #7224: both "self" signals are read HERE, at the I/O boundary, and
    // injected — `src/bin/tm/**` is ratcheted against process-global env access
    // (#5544), and injecting them is also what makes the delete guard testable.
    let self_session_id = std::env::var("TM_MANAGED_SESSION_ID").ok();
    let self_tmux_name = super::tmux_attach::current_tmux_session_name();
    super::session_tui::run_session_tui(
        client,
        url,
        &mut scope,
        sessions,
        self_session_id,
        self_tmux_name,
    )
    .await
}
