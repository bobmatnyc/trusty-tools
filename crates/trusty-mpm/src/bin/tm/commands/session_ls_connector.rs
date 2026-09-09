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
//! [`super::session_tui`] rather than the line-based numbered picker.
//! `--plain` is the opt-out back to the static table.
//!
//! #7224: bare `tm` reaches that same TUI, through this module rather than a
//! second copy of the gate. [`bare_tm_opens_session_tui`] is the decision and
//! [`run_bare_tm_surface`] the dispatch; `guided::try_show_picker` calls the
//! latter where it used to call `run_tty_picker` directly. The numbered picker
//! is still bare `tm`'s fallback — it is the surface that can launch a NEW
//! session, which the TUI cannot, so an empty fleet or a dumb terminal keeps
//! it.
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
///
/// #7224: `term` is the `TERM` value, injected rather than read here so the gate
/// stays pure. A TTY is not enough — `TERM=dumb` under a real pty (`script`, an
/// Emacs shell buffer, a CI pty) is two TTYs with no cursor addressing, and the
/// TUI would smear its redraw down the screen after raw mode had already taken
/// the operator's echo away. The predicate is
/// [`trusty_mpm::tui::terminal::term_supports_raw_mode`], shared with `tm f`'s
/// `interactive_filter_allowed` so the two surfaces cannot drift.
/// Test: `ls_connector_should_show_picker_*` and
/// `ls_connector_dumb_term_reaches_the_static_renderer` in
/// `tests_behavior_d_ls_connector_tests.rs`.
// #7224: the eighth input is `TERM`. The alternative to the arity is a flags
// struct built at the one call site and destructured here, which hides the same
// operands behind a type without removing one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn should_show_picker(
    stdin_tty: bool,
    stdout_tty: bool,
    json: bool,
    all: bool,
    attached: bool,
    plain: bool,
    term: Option<&str>,
    session_count: usize,
) -> bool {
    // #7224: defined as the complement of [`prints_static_table`] rather than a
    // second spelling of the same operands, so a gate added to one can never be
    // missing from the other — which is how the `TERM` check came to be in
    // `tm f` and not here.
    !prints_static_table(stdin_tty, stdout_tty, json, all, attached, plain, term)
        && session_count > 0
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
/// What: true for `--json`, `--all`, `--attached`, `--plain`, either stream not
/// being a terminal, or (#7224) a `TERM` that cannot address the cursor. The
/// complement is NOT [`should_show_picker`] — that one additionally requires a
/// non-empty fleet, because zero sessions on a TTY prints the static "no managed
/// sessions" line from a later branch instead.
/// Test: `plain_and_non_tty_reach_the_same_static_renderer`,
/// `ls_connector_should_show_picker_plain_static`,
/// `ls_connector_dumb_term_reaches_the_static_renderer`.
pub(crate) fn prints_static_table(
    stdin_tty: bool,
    stdout_tty: bool,
    json: bool,
    all: bool,
    attached: bool,
    plain: bool,
    term: Option<&str>,
) -> bool {
    json
        || all
        || attached
        || plain
        || !stdin_tty
        || !stdout_tty
        // #7224: a dumb terminal takes the SAME early return a pipe does, so the
        // TUI is unreachable before any fetch rather than after one.
        || !trusty_mpm::tui::terminal::term_supports_raw_mode(term)
}

/// `tm ls` — the interactive managed-session connector (top-level).
///
/// Why: bare `tm ls` should do the most useful thing for connecting to the
/// managed fleet: on a real terminal it opens the session TUI; piped,
/// scripted, or `--plain` it degrades to the same static, pipeable list as
/// `tm session ls`.
/// What: resolves the scope (`--current` derives `owner/repo` from the cwd git
/// remote, mirroring `tm session ls`); routes `--json`, `--all`, `--attached`, a
/// `TERM` that cannot address the cursor (#7224), or
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
    // #7224: renamed from `term` — the new `term` below is the TERM variable,
    // and two different meanings under one name is how a gate gets miswired.
    filter: Option<SessionFilter>,
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
    // #7224: read at this I/O boundary and injected into both pure gates below,
    // mirroring `run_f_command`. The gates stay unit-testable without a live
    // terminal, and `src/bin/tm/**` keeps its no-process-global-env posture
    // (#5544) — this is a read, and it never leaves the boundary.
    let term_var = std::env::var("TERM").ok();
    let term = term_var.as_deref();

    // Cheap pre-gate: `--json`, `--all`, `--attached`, or any non-interactive
    // stream never fetches for the picker — delegate straight to the static
    // renderer, which owns the raw `--json` passthrough, the `--all` tombstone
    // sort, and the `-a` attached-only filter. `sort`/`filter` ride along (the
    // static renderer applies them; `--json` ignores them, matching `--all`'s
    // existing "no effect on --json" precedent).
    // #7224: `--plain` joins the list, so the static renderer below is reached
    // by the SAME branch a piped invocation takes — byte-identical output, not
    // a second formatting path.
    if prints_static_table(stdin_tty, stdout_tty, json, all, attached, plain, term) {
        return super::managed::session_ls(
            client,
            url,
            json,
            sid.as_deref(),
            all,
            attached,
            sort,
            filter,
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
                filter,
                no_prune,
            )
            .await;
        }
    };
    let mut sessions = filter_sessions_by_term(sessions, filter.as_ref());
    sort_sessions(&mut sessions, sort);

    if !should_show_picker(
        stdin_tty,
        stdout_tty,
        json,
        all,
        attached,
        plain,
        term,
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
        term: filter,
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

/// Does bare `tm` open the session TUI on this invocation?
///
/// Why (#7224): the owner ruling is that bare `tm` reaches the same surface
/// `tm ls` reaches. Re-spelling the gate here is the defect #7224 round 2 just
/// removed — the `TERM` check lived in `tm f` and not in `tm ls`, so one
/// surface entered raw mode where the other refused. The body is therefore one
/// delegation to [`should_show_picker`], and a gate added there reaches bare
/// `tm` without a second edit.
/// What: `json`, `all`, `attached`, and `plain` are pinned `false` because bare
/// `tm` has no grammar that could set them — it takes no flags and no
/// positionals (a leading token parses as `cli::Command::External`), so
/// `tm recent` and `tm alpha <filter>` are not forms that exist. The one
/// operand bare `tm` alone carries is `managed_pane`: inside a tm-managed pane,
/// bare `tm` is that pane's relaunch verb, so a full-screen surface must never
/// take the pane over there. `tm ls` in the same pane still opens the TUI and
/// reads the same variable for its self-delete guard — two different questions
/// about one fact, not a divergent gate. The operand is a `bool` for the same
/// reason [`super::guided_outside_git::route_bare_tm`]'s is: what counts as a
/// managed pane is
/// [`guided_inplace::resolve_env_managed_session_id`](super::guided_inplace::resolve_env_managed_session_id),
/// the one implementation of that lookup, and re-deciding it here would be a
/// second one.
/// Test: `bare_tm_opens_the_session_tui_on_two_ttys_with_a_capable_term`,
/// `bare_tm_never_opens_the_tui_inside_a_managed_pane`,
/// `bare_tm_and_ls_refuse_the_tui_on_the_same_inputs`.
pub(crate) fn bare_tm_opens_session_tui(
    stdin_tty: bool,
    stdout_tty: bool,
    term: Option<&str>,
    managed_pane: bool,
    session_count: usize,
) -> bool {
    if managed_pane {
        return false;
    }
    should_show_picker(
        stdin_tty,
        stdout_tty,
        false,
        false,
        false,
        false,
        term,
        session_count,
    )
}

/// Open bare `tm`'s managed-session surface: the TUI, or the numbered picker.
///
/// Why (#7224): `guided::try_show_picker` used to call
/// [`super::session_picker::run_tty_picker`] unconditionally, so the surface
/// the owner asked for was reachable from `tm ls` and from nothing else.
/// Putting the choice HERE rather than in `guided.rs` is what keeps the gate
/// single: this module already owns "which surface does a managed-session
/// listing open", and `guided.rs` sits one line under the 500-SLOC cap.
/// What: reads `TERM` and the managed-session id at this I/O boundary and
/// injects them into [`bare_tm_opens_session_tui`], mirroring
/// [`run_ls_connector`] — the gate stays pure and `src/bin/tm/**` keeps its
/// no-process-global-env posture (#5544), which these reads never violate. The
/// id comes from
/// [`guided_inplace::resolve_env_managed_session_id`](super::guided_inplace::resolve_env_managed_session_id),
/// the process-env-then-tmux-env chain `try_inplace_relaunch` and
/// `try_outside_git` already ask, rather than a third reading of the variable.
/// The picker is the fallback, not a lesser one: it is the only surface with a
/// launch-new action, which is exactly what an empty fleet needs, and it reads
/// whole lines instead of raw-mode keys, which is exactly what a `TERM` with no
/// cursor addressing needs.
/// Test: the choice is `bare_tm_opens_the_session_tui_*` and
/// `bare_tm_never_opens_the_tui_inside_a_managed_pane`; the two surfaces it
/// dispatches to carry their own.
pub(crate) async fn run_bare_tm_surface(
    client: &reqwest::Client,
    url: &str,
    scope: &mut PickerScope,
    sessions: Vec<trusty_mpm::client::ManagedSessionSummary>,
) -> anyhow::Result<()> {
    let self_session_id = super::guided_inplace::resolve_env_managed_session_id();
    let term_var = std::env::var("TERM").ok();
    if bare_tm_opens_session_tui(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        term_var.as_deref(),
        self_session_id.is_some(),
        sessions.len(),
    ) {
        // `self_session_id` is `None` on this branch by construction — the gate
        // above refuses a managed pane — and is still threaded through so the
        // TUI's self-delete guard reads the same operand `tm ls` hands it.
        let self_tmux_name = super::tmux_attach::current_tmux_session_name();
        return super::session_tui::run_session_tui(
            client,
            url,
            scope,
            sessions,
            self_session_id,
            self_tmux_name,
        )
        .await;
    }
    super::session_picker::run_tty_picker(client, url, scope, sessions).await
}
