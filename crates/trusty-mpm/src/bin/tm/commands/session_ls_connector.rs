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
//! filtering, sorting, and the picker loop itself — still lives in
//! [`super::session_picker`] and is called from here, so there is one
//! implementation of each.
//!
//! Test: the gate is unit-tested by `ls_connector_should_show_picker_*` in
//! `tests_behavior_d_tests.rs`; the orchestrator's argument parsing is covered
//! by `cli_parses_ls_*` in the same file, and its I/O path by the e2e suite.

use std::io::IsTerminal as _;

use super::session_picker::{
    PickerScope, SessionFilter, SessionSortArg, fetch_live_sessions, filter_sessions_by_term,
    run_tty_picker, sort_sessions,
};

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
/// Test: `ls_connector_should_show_picker_*` in `tests_behavior_d_tests.rs`.
pub(crate) fn should_show_picker(
    stdin_tty: bool,
    stdout_tty: bool,
    json: bool,
    all: bool,
    attached: bool,
    session_count: usize,
) -> bool {
    stdin_tty && stdout_tty && !json && !all && !attached && session_count > 0
}

/// `tm ls` — the interactive managed-session connector (top-level).
///
/// Why: bare `tm ls` should do the most useful thing for connecting to the
/// managed fleet: on a real terminal it opens the session picker; piped or
/// scripted it degrades to the same static, pipeable list as `tm session ls`.
/// What: resolves the scope (`--current` derives `owner/repo` from the cwd git
/// remote, mirroring `tm session ls`); routes `--json`, `--all`, `--attached`, or
/// any non-TTY invocation straight to the static [`super::managed::session_ls`]
/// renderer (preserving its raw `--json` passthrough byte-for-byte); otherwise
/// fetches the live sessions once and either renders the static table (0
/// sessions) or opens [`run_tty_picker`] (≥1 session). Launch-new inside the
/// picker targets the cwd project only when it is a GitHub-backed git checkout.
/// `attached` is a pure listing filter and never reaches the picker — see
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
    if json || all || attached || !stdin_tty || !stdout_tty {
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

    if !should_show_picker(stdin_tty, stdout_tty, json, all, attached, sessions.len()) {
        // 0 sessions on a TTY: print the static "no managed sessions" line rather
        // than an empty picker.
        super::managed_render::render_session_table(&sessions, sid.as_deref());
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
        sort,
        term,
    };
    run_tty_picker(client, url, &scope, sessions).await
}
