//! Async event loop: daemon polling, SSE reindex streaming, keyboard handling.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::monitor::dashboard::format_count;
use crate::monitor::search_client::{ReindexEvent, SearchClient, resolve_search_url};
use crate::monitor::tui_common::{enter_tui, leave_tui};
use crate::monitor::utils::DaemonStatus;

use super::nav::{navigate_down_visible, navigate_up_visible, new_log_lines_since};
use super::render::{SearchFocus, render};
use super::state::SearchTuiState;

/// Data-refresh interval: how often the daemon is polled.
const REFRESH_INTERVAL: Duration = Duration::from_millis(2000);

/// Input-poll interval: how often the keyboard is checked.
const INPUT_POLL: Duration = Duration::from_millis(50);

/// Number of results requested per search query.
const SEARCH_TOP_K: usize = 5;

/// One reindex SSE event tagged with the index it concerns.
///
/// Why: a reindex is started per-index, but the streamed [`ReindexEvent`]s
/// carry no index id; pairing each with `index_id` lets the activity log scope
/// the line so the per-index feed (and the "All" merge) stay correct.
/// What: the index id the reindex targets and the streamed event.
/// Test: `test_apply_reindex_event` exercises the scoped logging.
#[derive(Debug, Clone)]
pub struct ScopedReindexEvent {
    /// The index this reindex event belongs to.
    pub index_id: String,
    /// The streamed reindex progress event.
    pub event: ReindexEvent,
}

/// Apply one streamed reindex event to the activity log, scoped to its index.
///
/// Why: the reindex SSE task forwards [`ScopedReindexEvent`]s through a
/// channel; the event loop drains them and this turns each into a
/// human-readable, index-scoped log line so the per-index activity feed shows
/// only its own reindex progress.
/// What: `Started` / `Progress` / `Complete` / `Failed` each map to a distinct
/// timestamped line tagged with `scoped.index_id`, with progress carrying a
/// percent-complete figure.
/// Test: `test_apply_reindex_event`.
pub fn apply_reindex_event(state: &mut SearchTuiState, scoped: ScopedReindexEvent) {
    let id = scoped.index_id;
    match scoped.event {
        ReindexEvent::Started { total_files } => {
            state
                .log
                .push_scoped(&id, format!("reindex started: {total_files} files"));
        }
        ReindexEvent::Progress {
            indexed,
            total_files,
        } => {
            let pct = indexed
                .saturating_mul(100)
                .checked_div(total_files)
                .unwrap_or(0);
            state
                .log
                .push_scoped(&id, format!("indexing: {indexed}/{total_files} ({pct}%)"));
        }
        ReindexEvent::Complete {
            total_chunks,
            status,
        } => {
            state.log.push_scoped(
                &id,
                format!("reindex {status}: {} chunks", format_count(total_chunks)),
            );
        }
        ReindexEvent::Failed(message) => {
            state
                .log
                .push_scoped(&id, format!("reindex error: {message}"));
        }
    }
}

/// Run the trusty-search monitor TUI.
///
/// Why: the single entry point the `monitor tui` subcommand of `trusty-search`
/// calls.
/// What: resolves the daemon URL from the service lock file and delegates to
/// [`run_with_url`].
/// Test: the pure pieces are unit-tested; this thin glue is exercised by
/// launching the UI.
pub async fn run() -> anyhow::Result<()> {
    run_with_url(resolve_search_url()).await
}

/// Run the search TUI against an explicit daemon URL.
///
/// Why: separated from [`run`] so a future CLI flag can override the resolved
/// address, and so terminal setup/teardown lives in one place.
/// What: builds the client and state, enters raw mode + the alternate screen,
/// runs [`run_loop`], and unconditionally restores the terminal even on error.
/// Test: terminal glue is exercised by launching the UI.
pub async fn run_with_url(base_url: String) -> anyhow::Result<()> {
    let mut client = SearchClient::new(base_url.clone());
    let mut state = SearchTuiState::new(base_url);

    let mut terminal = enter_tui()?;
    let result = run_loop(&mut terminal, &mut state, &mut client).await;
    leave_tui(&mut terminal)?;
    result
}

/// Poll the trusty-search daemon and fold the result into `state`.
///
/// Why: keeps the per-poll I/O out of the event loop so the loop can re-poll
/// on demand as well as on its timer.
/// What: re-resolves the URL when the daemon is offline (it may have rebound a
/// fresh port), calls `fetch_all`, and updates the status, index list, and
/// selection clamp.
/// Test: thin I/O glue; the pure clamp is unit-tested.
async fn poll_daemon(state: &mut SearchTuiState, client: &mut SearchClient) {
    if !state.daemon_status.is_online() {
        let resolved = resolve_search_url();
        if resolved != client.base_url() {
            client.set_base_url(resolved.clone());
            state.base_url = resolved;
        }
    }
    match client.fetch_all().await {
        Ok(data) => {
            state.daemon_status = DaemonStatus::Online {
                version: data.version,
                uptime_secs: data.uptime_secs,
            };
            state.indexes = data.indexes;
            state.clamp_selection();
        }
        Err(e) => {
            state.daemon_status = DaemonStatus::Offline {
                last_error: e.to_string(),
            };
        }
    }

    let tail = client.logs_tail(50).await;
    if state.log_first_poll {
        state.log_watermark = tail.last().cloned();
        state.log_first_poll = false;
    } else {
        let new = new_log_lines_since(&tail, state.log_watermark.as_deref());
        for line in new {
            state.log.push(line.clone());
        }
        if let Some(last) = tail.last() {
            state.log_watermark = Some(last.clone());
        }
    }
}

/// Run a search and append the hits to the activity log.
///
/// Why: pressing `[Enter]` in the query bar runs a hybrid search; the operator
/// sees the results inline in the ACTIVITY panel. When "All indexes" is
/// selected the search fans out across every registered index.
/// What: with a single index selected, calls `client.search` for it and logs a
/// `search "<q>" → N results` summary plus one indented continuation line per
/// hit, all tagged with that index's id. With "All" selected it iterates every
/// index, logging each index's per-index summary tagged to that index and a
/// final daemon-wide total line. An empty query is a no-op; transport errors
/// are logged.
/// Test: thin I/O glue; result projection is tested in `search_client`.
async fn run_search(state: &mut SearchTuiState, client: &SearchClient) {
    let query = state.input.trim().to_string();
    if query.is_empty() {
        return;
    }

    if state.is_all_selected() {
        run_search_all(state, client, &query).await;
    } else if let Some(id) = state.selected_id().map(str::to_string) {
        run_search_one(state, client, &id, &query).await;
    } else {
        state.log.push("search: no index selected");
    }
    state.input.clear();
}

/// Run a search against one index and append the hits to the log.
///
/// Why: the single-index path of [`run_search`]; isolating it keeps the
/// fan-out loop in [`run_search_all`] terse.
/// What: calls `client.search(id, …)`, appends an `id`-scoped summary line and
/// one indented `path:line  snippet` continuation per hit. A transport error
/// is logged as an `id`-scoped failure line.
/// Test: thin I/O glue; result projection is tested in `search_client`.
async fn run_search_one(state: &mut SearchTuiState, client: &SearchClient, id: &str, query: &str) {
    match client.search(id, query, SEARCH_TOP_K).await {
        Ok(hits) => {
            state
                .log
                .push_scoped(id, format!("search \"{query}\" → {} results", hits.len()));
            for hit in &hits {
                state
                    .log
                    .push_raw_scoped(id, format!("  {}:{}  {}", hit.file, hit.line, hit.snippet));
            }
        }
        Err(e) => state
            .log
            .push_scoped(id, format!("search \"{query}\" failed: {e}")),
    }
}

/// Fan a search out across every index and append a merged summary.
///
/// Why: the "All indexes" selector lets an operator run one query over the
/// whole machine's corpus; the activity feed then shows each index's hit count
/// (tagged so the per-index view still works) plus a daemon-wide total.
/// What: snapshots the index ids, then for each calls `client.search`,
/// appending an `id`-scoped `· <id>: N results` line. A daemon-wide
/// `search "<q>" (all) → T results across K indexes` total line closes the
/// burst. With no indexes registered it logs a single note.
/// Test: thin I/O glue; the single-index search is unit-tested in
/// `search_client`.
async fn run_search_all(state: &mut SearchTuiState, client: &SearchClient, query: &str) {
    let ids: Vec<String> = state.indexes.iter().map(|i| i.id.clone()).collect();
    if ids.is_empty() {
        state.log.push("search (all): no indexes registered");
        return;
    }
    state
        .log
        .push(format!("search \"{query}\" (all) → {} indexes", ids.len()));
    let mut total = 0usize;
    for id in &ids {
        match client.search(id, query, SEARCH_TOP_K).await {
            Ok(hits) => {
                total += hits.len();
                state
                    .log
                    .push_raw_scoped(id, format!("  · {id}: {} results", hits.len()));
            }
            Err(e) => state
                .log
                .push_raw_scoped(id, format!("  · {id}: failed: {e}")),
        }
    }
    state.log.push(format!(
        "search \"{query}\" (all) → {total} results across {} indexes",
        ids.len()
    ));
}

/// The search TUI event loop: poll, render, handle input, drain reindex events.
///
/// Why: kept separate from [`run_with_url`] so terminal setup/teardown wraps it
/// cleanly.
/// What: polls the daemon immediately, then renders every frame while polling
/// the keyboard every 50 ms; re-polls on the 2 s timer. `[r]` spawns an SSE
/// reindex task whose events are drained via `try_recv`; `[Enter]` runs a
/// search; `Tab`, arrows, `?`, `q`/`Esc`, and `Ctrl-C` behave per [`KEY_HINT`].
/// Test: the pure pieces (state, log, rendering helpers) are unit-tested.
async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut SearchTuiState,
    client: &mut SearchClient,
) -> anyhow::Result<()> {
    poll_daemon(state, client).await;
    let mut last_poll = Instant::now();

    let (reindex_tx, mut reindex_rx) = mpsc::channel::<ScopedReindexEvent>(64);

    loop {
        terminal.draw(|f| render(f, state))?;

        while let Ok(event) = reindex_rx.try_recv() {
            apply_reindex_event(state, event);
        }

        let key = if event::poll(INPUT_POLL)? {
            match event::read()? {
                Event::Key(key) => Some(key),
                _ => None,
            }
        } else {
            None
        };
        if let Some(key) = key
            && key.kind != KeyEventKind::Release
        {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }
            if state.show_help {
                if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc) {
                    state.show_help = false;
                } else if key.code == KeyCode::Char('q') {
                    return Ok(());
                }
                continue;
            }
            match (state.focus, key.code) {
                (SearchFocus::List, KeyCode::Esc) if state.filter_active => {
                    state.filter_active = false;
                }
                (SearchFocus::List, KeyCode::Enter) if state.filter_active => {
                    state.filter_active = false;
                }
                (SearchFocus::List, KeyCode::Backspace) if state.filter_active => {
                    state.filter.pop();
                    state.clamp_to_visible();
                }
                (SearchFocus::List, KeyCode::Char(c)) if state.filter_active => {
                    state.filter.push(c);
                    state.clamp_to_visible();
                }
                (SearchFocus::List, KeyCode::Tab) if state.filter_active => {}
                (_, KeyCode::Char('?')) => state.show_help = true,
                (_, KeyCode::Tab) => state.toggle_focus(),
                (_, KeyCode::Esc) => return Ok(()),
                (SearchFocus::List, KeyCode::Char('q')) => return Ok(()),
                (SearchFocus::List, KeyCode::Up) => navigate_up_visible(state),
                (SearchFocus::List, KeyCode::Down) => navigate_down_visible(state),
                (SearchFocus::List, KeyCode::Char('/')) => {
                    state.filter_active = true;
                    state.filter.clear();
                }
                (SearchFocus::List, KeyCode::Char('s')) => {
                    state.sort_key = state.sort_key.next();
                }
                (SearchFocus::List, KeyCode::Char('g')) => {
                    state.group_by_project = !state.group_by_project;
                }
                (SearchFocus::List, KeyCode::Char('r')) => {
                    let targets: Vec<String> = if state.is_all_selected() {
                        state.indexes.iter().map(|i| i.id.clone()).collect()
                    } else {
                        state
                            .selected_id()
                            .map(str::to_string)
                            .into_iter()
                            .collect()
                    };
                    if targets.is_empty() {
                        if state.is_all_selected() {
                            state.log.push("reindex (all): no indexes registered");
                        } else {
                            state.log.push("reindex: no index selected");
                        }
                    } else {
                        if state.is_all_selected() {
                            state
                                .log
                                .push(format!("reindex triggered: all {} indexes", targets.len()));
                        }
                        for id in targets {
                            state
                                .log
                                .push_scoped(&id, format!("reindex triggered: {id}"));
                            spawn_reindex(client.clone(), reindex_tx.clone(), id);
                        }
                    }
                }
                (SearchFocus::Input, KeyCode::Enter) => {
                    run_search(state, client).await;
                    poll_daemon(state, client).await;
                    last_poll = Instant::now();
                }
                (SearchFocus::Input, KeyCode::Backspace) => {
                    state.input.pop();
                }
                (SearchFocus::Input, KeyCode::Char(c)) => state.input.push(c),
                _ => {}
            }
        }

        if last_poll.elapsed() >= REFRESH_INTERVAL {
            poll_daemon(state, client).await;
            last_poll = Instant::now();
        }
    }
}

/// Spawn a background task streaming one index's reindex into `out`.
///
/// Why: `SearchClient::reindex_stream` emits bare [`ReindexEvent`]s, but the
/// event loop needs each tagged with its index id; this bridges a per-index
/// inner channel onto the loop's [`ScopedReindexEvent`] channel so several
/// indexes can reindex concurrently (the "All" fan-out) without losing track
/// of which event belongs to which index.
/// What: spawns the SSE streaming task plus a forwarding task that wraps every
/// [`ReindexEvent`] in a [`ScopedReindexEvent`] carrying `index_id`.
/// Test: side-effect-only (spawns tasks); the scoped event handling is
/// unit-tested via `test_apply_reindex_event`.
fn spawn_reindex(client: SearchClient, out: mpsc::Sender<ScopedReindexEvent>, index_id: String) {
    let (inner_tx, mut inner_rx) = mpsc::channel::<ReindexEvent>(64);
    let stream_id = index_id.clone();
    tokio::spawn(async move {
        client.reindex_stream(&stream_id, inner_tx).await;
    });
    tokio::spawn(async move {
        while let Some(event) = inner_rx.recv().await {
            let scoped = ScopedReindexEvent {
                index_id: index_id.clone(),
                event,
            };
            if out.send(scoped).await.is_err() {
                break;
            }
        }
    });
}
