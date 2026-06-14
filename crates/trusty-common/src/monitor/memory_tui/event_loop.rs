//! The memory TUI event loop: poll, render, handle input, drain SSE events.
//!
//! Why: keeping the async I/O — daemon polling, SSE subscription, recall
//! requests, drawer fetches — in a dedicated module separates it from the
//! pure state and rendering, making both easier to test independently.
//! What: [`run_loop`] is the inner loop called by `run_with_url`; the other
//! functions are async helpers for specific operations (polling, recall,
//! SSE event application, drawer fetches).
//! Test: the pure pieces (state, log, rendering helpers) are unit-tested;
//! the async I/O glue is exercised by launching the UI.

use std::time::Instant;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc;

use crate::monitor::memory_client::{MemoryClient, MemoryEvent, RecallHit, resolve_memory_url};
use crate::monitor::memory_tui::MemoryFocus;
use crate::monitor::memory_tui::MemoryTuiState;
use crate::monitor::memory_tui::render::render;
use crate::monitor::memory_tui::state::{DRAWER_PAGE_SIZE, RECALL_TOP_K};
use crate::monitor::memory_tui::view::navigate_down_visible;
use crate::monitor::memory_tui::view::navigate_up_visible;
use crate::monitor::utils::DaemonStatus;

/// Data-refresh interval: how often the daemon is polled.
const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2000);

/// Input-poll interval: how often the keyboard is checked.
const INPUT_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Poll the trusty-memory daemon and fold the result into `state`.
///
/// Why: keeps the per-poll I/O out of the event loop so the loop can re-poll
/// on demand as well as on its timer.
/// What: re-resolves the URL when the daemon is offline, calls `fetch_all`, and
/// updates the status, aggregate stats, palace list, and selection clamp.
/// Test: thin I/O glue; the pure clamp is unit-tested.
pub(crate) async fn poll_daemon(state: &mut MemoryTuiState, client: &mut MemoryClient) {
    if !state.daemon_status.is_online() {
        let resolved = resolve_memory_url();
        if resolved != client.base_url() {
            client.set_base_url(resolved.clone());
            state.base_url = resolved;
        }
    }
    match client.fetch_all().await {
        Ok(data) => {
            state.daemon_status = DaemonStatus::Online {
                version: data.version.clone(),
                uptime_secs: 0,
            };
            state.palaces = data.palaces.clone();
            state.status = Some(data);
            state.clamp_selection();
        }
        Err(e) => {
            state.daemon_status = DaemonStatus::Offline {
                last_error: e.to_string(),
            };
        }
    }
}

/// Run a recall and append the hits to the activity log.
///
/// Why: pressing `[Enter]` in the recall bar runs a memory recall; the
/// operator sees the results inline in the ACTIVITY panel. The recall endpoint
/// is inherently cross-palace, so when a single palace is selected the hits
/// are filtered to that palace; when "All palaces" is selected every hit is
/// shown.
/// What: calls `client.recall`, then — for the "All" selection — appends a
/// daemon-wide `recall "<q>" → N results` summary plus one `palace_id`-scoped
/// `· [palace] snippet` continuation per hit. For a single palace it appends a
/// palace-scoped summary counting only that palace's hits and a continuation
/// per kept hit. An empty query is a no-op; transport errors are logged scoped
/// to the selection.
/// Test: thin I/O glue; result projection is tested in `memory_client`.
async fn run_recall(state: &mut MemoryTuiState, client: &MemoryClient) {
    let query = state.input.trim().to_string();
    if query.is_empty() {
        return;
    }
    let scope = state.selected_id().map(str::to_string);
    match client.recall(&query, RECALL_TOP_K).await {
        Ok(hits) => match &scope {
            // "All palaces": one daemon-wide summary, each hit scoped to its
            // own palace so the per-palace feed still shows it.
            None => {
                state
                    .log
                    .push(format!("recall \"{query}\" (all) → {} results", hits.len()));
                for hit in &hits {
                    let palace = if hit.palace_id.is_empty() {
                        "?"
                    } else {
                        hit.palace_id.as_str()
                    };
                    state
                        .log
                        .push_raw_scoped(palace, format!("  · [{palace}] {}", hit.snippet));
                }
            }
            // A single palace: keep only that palace's hits.
            Some(id) => {
                let kept: Vec<&RecallHit> = hits.iter().filter(|h| h.palace_id == *id).collect();
                state
                    .log
                    .push_scoped(id, format!("recall \"{query}\" → {} results", kept.len()));
                for hit in kept {
                    state
                        .log
                        .push_raw_scoped(id, format!("  · {}", hit.snippet));
                }
            }
        },
        Err(e) => match &scope {
            None => state
                .log
                .push(format!("recall \"{query}\" (all) failed: {e}")),
            Some(id) => state
                .log
                .push_scoped(id, format!("recall \"{query}\" failed: {e}")),
        },
    }
    state.input.clear();
}

/// Append a streamed `/sse` event to the activity log, scoped to its palace.
///
/// Why: the SSE task forwards [`MemoryEvent`]s through a channel; the event
/// loop drains them and this turns each into a human-readable log entry. The
/// drawer events concern one palace, so they are tagged with its id and the
/// per-palace activity feed keeps only its own events.
/// What: `DreamCompleted` records a daemon-wide header plus an indented
/// merge/prune/compact line; `DrawerAdded` / `DrawerDeleted` record a single
/// line each scoped to `palace_id`; `PalaceCreated` records a daemon-wide line
/// (the new palace has no id yet on the wire).
/// Test: `test_log_append_dream`, `test_apply_memory_event`.
pub fn apply_memory_event(state: &mut MemoryTuiState, event: MemoryEvent) {
    match event {
        MemoryEvent::DreamCompleted {
            merged,
            pruned,
            compacted,
        } => {
            state.log.push("SSE: dream_completed");
            state.log.push_raw(format!(
                "  merged: {merged}  pruned: {pruned}  compacted: {compacted}"
            ));
        }
        MemoryEvent::DrawerAdded {
            palace_id,
            drawer_count,
            content_preview,
        } => {
            // Prefer a content preview when the daemon provided one; fall
            // back to the legacy "(<count>)" format so older daemons still
            // render a useful line.
            let line = if content_preview.is_empty() {
                format!("SSE: drawer added → {palace_id} ({drawer_count})")
            } else {
                format!("SSE: drawer added → {palace_id} ({drawer_count}): \"{content_preview}\"")
            };
            state.log.push_scoped(&palace_id, line);
        }
        MemoryEvent::DrawerDeleted {
            palace_id,
            drawer_count,
        } => {
            state.log.push_scoped(
                &palace_id,
                format!("SSE: drawer deleted → {palace_id} ({drawer_count})"),
            );
        }
        MemoryEvent::PalaceCreated { name } => {
            state.log.push(format!("SSE: palace created → {name}"));
        }
    }
}

/// Fetch the drawer page for the current selection and fold the result into
/// [`MemoryTuiState::drawer_list`].
///
/// Why: the activity panel needs a live page slice for whichever palace is
/// selected; isolating the fetch keeps the event loop free of per-trigger
/// branching and makes the loading / error transitions easy to reason about.
/// What: when no single palace is selected, clears the drawer slice and
/// returns. Otherwise issues `client.list_drawers` for the stored offset and
/// either replaces `drawers` or records the error. Always flips
/// `loading = false` so the renderer drops the in-flight badge.
/// Test: thin I/O glue; pure projection is tested in `memory_client`.
pub(crate) async fn fetch_drawer_page(state: &mut MemoryTuiState, client: &MemoryClient) {
    let Some(palace_id) = state.selected_id().map(str::to_string) else {
        // "All palaces" or no selection — clear the drawer slice; the panel
        // falls back to the aggregate activity log.
        state.drawer_list.palace_id = None;
        state.drawer_list.drawers.clear();
        state.drawer_list.offset = 0;
        state.drawer_list.loading = false;
        state.drawer_list.last_error = None;
        return;
    };

    state.drawer_list.palace_id = Some(palace_id.clone());
    state.drawer_list.loading = true;
    match client
        .list_drawers(&palace_id, DRAWER_PAGE_SIZE, state.drawer_list.offset)
        .await
    {
        Ok(rows) => {
            state.drawer_list.drawers = rows;
            state.drawer_list.last_error = None;
        }
        Err(e) => {
            state.drawer_list.last_error = Some(e.to_string());
            state.drawer_list.drawers.clear();
        }
    }
    state.drawer_list.loading = false;
}

/// Fetch the full memory detail for the drawer-detail modal (issue #215).
///
/// Why: when the operator presses `Enter` in the drawer pane the modal must
/// open with the verbatim drawer body. The activity-panel rows only carry
/// the truncated snippet, so we re-fetch the drawer list from the daemon —
/// which serialises every drawer's full `content` — and store the result in
/// `state.drawer_detail_memories`.
/// What: when no palace is selected, leaves the modal closed and returns.
/// Otherwise issues `client.fetch_drawer_detail` for the current scope and
/// either replaces the memories or records the failure on the log. Always
/// flips `drawer_detail_loading = false` so the modal drops its in-flight
/// label.
/// Test: thin I/O glue; pure projection is tested via `parse_memory_details`.
async fn fetch_drawer_detail(state: &mut MemoryTuiState, client: &MemoryClient) {
    let Some(palace_id) = state.selected_id().map(str::to_string) else {
        // No single palace selected — close the modal so it can't render
        // stale memories from a previous scope.
        state.close_drawer_detail();
        return;
    };
    state.drawer_detail_loading = true;
    // Use a generous limit so the modal can show the entire drawer page (the
    // pane page size is 20, but the modal lets the operator scroll through
    // every memory the daemon returns).
    match client.fetch_drawer_detail(&palace_id, 50).await {
        Ok(memories) => {
            state.drawer_detail_memories = memories;
            // Clamp the selected index to the loaded set in case the page
            // shrank between key-press and fetch completion.
            if state.drawer_detail_idx >= state.drawer_detail_memories.len() {
                state.drawer_detail_idx = state.drawer_detail_memories.len().saturating_sub(1);
            }
        }
        Err(e) => {
            // Surface the error on the activity log so the operator sees why
            // the modal stayed empty. The modal itself shows a `Loading…`
            // placeholder until either a fetch succeeds or it is closed.
            state
                .log
                .push_scoped(&palace_id, format!("drawer detail fetch failed: {e}"));
            state.drawer_detail_memories.clear();
        }
    }
    state.drawer_detail_loading = false;
}

/// The memory TUI event loop: poll, render, handle input, drain SSE events.
///
/// Why: kept separate from `run_with_url` so terminal setup/teardown wraps it
/// cleanly.
/// What: polls the daemon immediately and spawns the `/sse` subscription task,
/// then renders every frame while polling the keyboard every 50 ms; re-polls on
/// the 2 s timer and drains SSE events via `try_recv`. `[d]` triggers a dream
/// cycle, `[Enter]` runs a recall; `Tab`, arrows, `?`, `q`/`Esc`, and `Ctrl-C`
/// behave per [`KEY_HINT`].
/// Test: the pure pieces (state, log, rendering helpers) are unit-tested.
pub(crate) async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut ratatui::Terminal<B>,
    state: &mut MemoryTuiState,
    client: &mut MemoryClient,
) -> anyhow::Result<()> {
    poll_daemon(state, client).await;
    let mut last_poll = Instant::now();

    // Subscribe to the daemon's /sse stream on a background task.
    let (sse_tx, mut sse_rx) = mpsc::channel::<MemoryEvent>(64);
    let sse_client = client.clone();
    tokio::spawn(async move {
        sse_client.sse_stream(sse_tx).await;
    });

    // Issue #184: every time the palace selection changes, refresh the
    // drawer panel. Tracking the previously-shown scope avoids re-fetching
    // on every render tick.
    let mut last_drawer_scope: Option<String> = None;

    loop {
        terminal.draw(|f| render(f, state))?;
        // `terminal.draw` requires `state` mutably (the renderer scrolls the
        // palace list); the closure reborrows it for the rest of the loop.

        // Drain any SSE events the subscription task produced since last frame.
        while let Ok(event) = sse_rx.try_recv() {
            apply_memory_event(state, event);
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
            // Ctrl-C always quits, regardless of focus or the help overlay.
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
            // Issue #215: drawer-detail modal owns the keyboard while open —
            // `Esc`/`q` close it; `↑`/`↓` scroll its body; everything else is
            // swallowed so the underlying UI never reacts under the modal.
            if state.drawer_detail_open {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => state.close_drawer_detail(),
                    KeyCode::Up => {
                        state.drawer_detail_scroll = state.drawer_detail_scroll.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        state.drawer_detail_scroll = state.drawer_detail_scroll.saturating_add(1);
                    }
                    _ => {}
                }
                continue;
            }
            match (state.focus, key.code) {
                // Filter-active bindings come first — they capture characters,
                // backspace, Esc, and Enter before the general List handlers.
                (MemoryFocus::List, KeyCode::Esc) if state.filter_active => {
                    // Keep the filter text so the user can re-activate.
                    state.filter_active = false;
                }
                (MemoryFocus::List, KeyCode::Enter) if state.filter_active => {
                    state.filter_active = false;
                }
                (MemoryFocus::List, KeyCode::Backspace) if state.filter_active => {
                    state.filter.pop();
                    state.clamp_to_visible();
                }
                (MemoryFocus::List, KeyCode::Char(c)) if state.filter_active => {
                    state.filter.push(c);
                    state.clamp_to_visible();
                }
                // Tab is a no-op while the filter is active — otherwise it
                // would steal focus away from the list and break filter input.
                (MemoryFocus::List, KeyCode::Tab) if state.filter_active => {}
                (_, KeyCode::Char('?')) => state.show_help = true,
                // Issue #215: Tab cycles through every focusable zone.
                (_, KeyCode::Tab) => state.cycle_focus(),
                // Esc on the drawer pane returns focus to the palace list
                // (with the drawer cursor cleared); on every other zone Esc
                // still quits, matching the legacy behaviour.
                (MemoryFocus::DrawerPane, KeyCode::Esc) => {
                    state.focus = MemoryFocus::List;
                    state.drawer_cursor = 0;
                }
                (_, KeyCode::Esc) => return Ok(()),
                // List-focus bindings.
                (MemoryFocus::List, KeyCode::Char('q')) => return Ok(()),
                (MemoryFocus::List, KeyCode::Up) => navigate_up_visible(state),
                (MemoryFocus::List, KeyCode::Down) => navigate_down_visible(state),
                // Drawer-page navigation in the ACTIVITY panel — only when a
                // single palace is selected. `←` previous page, `→` next.
                (MemoryFocus::List, KeyCode::Left) if state.selected_id().is_some() => {
                    state.drawer_list.prev_page();
                    fetch_drawer_page(state, client).await;
                    state.clamp_drawer_cursor();
                }
                (MemoryFocus::List, KeyCode::Right) if state.selected_id().is_some() => {
                    state.drawer_list.next_page();
                    fetch_drawer_page(state, client).await;
                    state.clamp_drawer_cursor();
                }
                (MemoryFocus::List, KeyCode::Char('/')) => {
                    state.filter_active = true;
                    state.filter.clear();
                }
                (MemoryFocus::List, KeyCode::Char('s')) => {
                    state.sort_key = state.sort_key.next();
                }
                (MemoryFocus::List, KeyCode::Char('g')) => {
                    state.group_by_project = !state.group_by_project;
                }
                (MemoryFocus::List, KeyCode::Char('d')) => {
                    let now = Instant::now();
                    if !state.dream_backoff.ready(now) {
                        let remaining = state.dream_backoff.remaining(now);
                        tracing::debug!(
                            "dream cycle suppressed by backoff: {}s remaining",
                            remaining.as_secs()
                        );
                        // Only echo the cooldown once per quiet period — log a
                        // single hint line the first time the operator hits
                        // [d] inside the window, then stay silent on repeats.
                    } else {
                        state.log.push("dream cycle triggered");
                        match client.dream_run().await {
                            Ok(stats) => {
                                state.log.push_raw(format!(
                                    "  merged: {}  pruned: {}  compacted: {}",
                                    stats.merged, stats.pruned, stats.compacted
                                ));
                                state.dream_backoff.record_success();
                            }
                            Err(e) => {
                                let should_log = state.dream_backoff.record_failure(Instant::now());
                                if should_log {
                                    let next = state.dream_backoff.remaining(Instant::now());
                                    state.log.push(format!(
                                        "dream failed: {e} (next attempt in {}s)",
                                        next.as_secs()
                                    ));
                                } else {
                                    tracing::debug!(
                                        "dream failed (suppressed, {} consecutive failures): {e}",
                                        state.dream_backoff.consecutive_failures()
                                    );
                                }
                            }
                        }
                        poll_daemon(state, client).await;
                        last_poll = Instant::now();
                    }
                }
                // DrawerPane bindings (issue #215). `↑`/`↓` move the drawer
                // cursor through the current page; `Enter` opens the detail
                // modal for the highlighted drawer; `←`/`→` continue to do
                // page navigation so the operator can step through pages
                // without switching focus back to the list.
                (MemoryFocus::DrawerPane, KeyCode::Up) => {
                    state.drawer_cursor_up();
                }
                (MemoryFocus::DrawerPane, KeyCode::Down) => {
                    state.drawer_cursor_down();
                }
                (MemoryFocus::DrawerPane, KeyCode::Left) if state.selected_id().is_some() => {
                    state.drawer_list.prev_page();
                    fetch_drawer_page(state, client).await;
                    state.clamp_drawer_cursor();
                }
                (MemoryFocus::DrawerPane, KeyCode::Right) if state.selected_id().is_some() => {
                    state.drawer_list.next_page();
                    fetch_drawer_page(state, client).await;
                    state.clamp_drawer_cursor();
                }
                (MemoryFocus::DrawerPane, KeyCode::Enter)
                    if !state.drawer_list.drawers.is_empty()
                        && state.drawer_cursor < state.drawer_list.drawers.len() =>
                {
                    state.drawer_detail_open = true;
                    state.drawer_detail_idx = state.drawer_cursor;
                    state.drawer_detail_scroll = 0;
                    state.drawer_detail_memories.clear();
                    fetch_drawer_detail(state, client).await;
                }
                (MemoryFocus::DrawerPane, KeyCode::Char('q')) => return Ok(()),
                // Input-focus bindings.
                (MemoryFocus::Input, KeyCode::Enter) => {
                    run_recall(state, client).await;
                }
                (MemoryFocus::Input, KeyCode::Backspace) => {
                    state.input.pop();
                }
                (MemoryFocus::Input, KeyCode::Char(c)) => state.input.push(c),
                _ => {}
            }
        }

        if last_poll.elapsed() >= REFRESH_INTERVAL {
            poll_daemon(state, client).await;
            // Refresh the drawer page in lock-step with the daemon poll so
            // new drawers appear in the activity panel without needing a
            // key press (issue #184: "Real-time updates when new drawers
            // are added while viewing").
            if state.selected_id().is_some() {
                fetch_drawer_page(state, client).await;
                state.clamp_drawer_cursor();
            }
            last_poll = Instant::now();
        }

        // Detect a palace-selection change after key handling and refresh
        // the drawer slice. Comparing the stored scope means we only fire
        // the fetch on real changes, not on every render tick.
        let current_scope = state.selected_id().map(str::to_string);
        if current_scope != last_drawer_scope {
            state.drawer_list.reset_for(current_scope.clone());
            fetch_drawer_page(state, client).await;
            // Issue #215: palace change resets the drawer cursor; the modal
            // (if open) should also close since its memories belong to the
            // previous scope.
            state.drawer_cursor = 0;
            state.close_drawer_detail();
            last_drawer_scope = current_scope;
        }
    }
}
