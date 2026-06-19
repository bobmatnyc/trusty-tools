//! Render a chat-core [`CommandResult`] as styled ratatui lines.
//!
//! Why: the coordinator TUI dispatches operator intent through the shared
//! chat-core executor and must show the structured [`CommandResult`] in its
//! output log. Unlike the Telegram adapter (which renders to HTML), the TUI needs
//! plain styled [`Line`]s suitable for a ratatui paragraph — so this is a
//! TUI-local formatter, deliberately NOT the Telegram HTML formatter. Keeping the
//! rendering pure (a `CommandResult` → `Vec<Line>` function with no terminal or
//! daemon) makes the per-variant formatting unit-testable (DOC-13 §9).
//! What: [`render_result`] maps each [`CommandResult`] variant the TUI can
//! produce (sessions list, managed lifecycle, send/answer/activity, attach, get,
//! help, and the error variant) to one or more styled lines. Variants the
//! coordinator cannot currently emit fall through to a compact debug line so the
//! formatter is total.
//! Test: `super::tests` covers representative variants (list, lifecycle, error,
//! send, activity).

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::client::{CommandResult, ManagedSessionView};

/// The style for an error line in the output log.
///
/// Why: errors must read distinctly from normal output; a named style keeps the
/// red emphasis single-sourced between the renderer and its test.
/// What: red, bold.
/// Test: `render_error_is_red`.
fn error_style() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
}

/// The style for an informational/heading line.
///
/// Why: command output headings (e.g. "managed sessions (3)") should stand out
/// from their detail rows; centralising the accent keeps it consistent.
/// What: cyan, bold.
/// Test: covered indirectly by the list/lifecycle render tests.
fn heading_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// Render a [`CommandResult`] into one or more styled output lines.
///
/// Why: the single seam the run loop calls after the executor returns; it turns
/// the structured result into the lines the output log appends. A total match
/// means a new result variant never silently renders nothing.
/// What: maps each variant the coordinator can produce to readable lines —
/// list/detail headings in the accent style, lifecycle/send/answer confirmations
/// as a single line, errors in the error style; the remaining variants (legacy
/// project-session family the coordinator does not invoke) render a compact
/// fallback line so the formatter is exhaustive without bloating the TUI.
/// Test: `render_result_*` in `super::tests`.
pub fn render_result(result: &CommandResult) -> Vec<Line<'static>> {
    match result {
        CommandResult::Error(msg) => {
            vec![Line::from(Span::styled(format!("✗ {msg}"), error_style()))]
        }
        CommandResult::Help(text) => text.lines().map(|l| Line::from(l.to_string())).collect(),
        CommandResult::ManagedSessions(views) => render_managed_sessions(views),
        CommandResult::ManagedSession(view) => render_managed_session(view),
        CommandResult::ManagedSpawned {
            id,
            name,
            state,
            runtime,
            attach_cmd,
        } => vec![
            heading_line(format!("spawned {name} [{state}]")),
            detail_line(format!("id {id} · runtime {runtime}")),
            detail_line(format!("attach: {attach_cmd}")),
        ],
        CommandResult::ManagedSent { id, tmux_name } => {
            vec![ok_line(format!("sent → {tmux_name} ({})", short(id)))]
        }
        CommandResult::ManagedAnswered {
            id,
            answer,
            tmux_name,
        } => vec![ok_line(format!(
            "answered {tmux_name} ({}) → {answer}",
            short(id)
        ))],
        CommandResult::ManagedActivity {
            id,
            state,
            summary,
            runtime_active,
            pending_decision,
            ..
        } => render_activity(id, state, summary, *runtime_active, pending_decision),
        CommandResult::ManagedAttachCmd { id, attach_cmd } => vec![
            heading_line(format!("attach {}", short(id))),
            detail_line(attach_cmd.clone()),
        ],
        CommandResult::ManagedLifecycle {
            id,
            name,
            state,
            action,
        } => vec![ok_line(format!(
            "{action} {name} [{state}] ({})",
            short(id)
        ))],
        // The coordinator does not invoke the legacy project-session/pairing
        // family, but the formatter stays total: render a compact, debug-style
        // fallback so any unexpected variant is still visible rather than blank.
        other => vec![Line::from(format!("{other:?}"))],
    }
}

/// Render the managed-session list as a heading plus one row per session.
///
/// Why: `/sessions` (and `/list`) produce a list the operator scans; a heading
/// with the count and one compact row each is the readable shape.
/// What: a `managed sessions (N)` heading then `• <short-id> <name> [state]` per
/// session (with a `· decision: …` suffix when one is pending); an explicit
/// "(none)" line when the list is empty.
/// Test: `render_result_lists_managed_sessions`.
fn render_managed_sessions(views: &[ManagedSessionView]) -> Vec<Line<'static>> {
    let mut lines = vec![heading_line(format!("managed sessions ({})", views.len()))];
    if views.is_empty() {
        lines.push(detail_line("(none)".to_string()));
        return lines;
    }
    for v in views {
        let mut text = format!("• {} {} [{}]", short(&v.id), v.name, v.state);
        if let Some(decision) = &v.pending_decision {
            text.push_str(&format!(" · decision: {decision}"));
        }
        lines.push(Line::from(text));
    }
    lines
}

/// Render one managed session's record (`/get`).
///
/// Why: the detail view shows identity + lifecycle and the pending decision the
/// session may be blocked on.
/// What: a heading line then detail lines for the workspace/branch and any
/// pending decision.
/// Test: `render_result_renders_managed_session`.
fn render_managed_session(v: &ManagedSessionView) -> Vec<Line<'static>> {
    let mut lines = vec![heading_line(format!(
        "{} {} [{}]",
        short(&v.id),
        v.name,
        v.state
    ))];
    if let Some(branch) = &v.branch {
        lines.push(detail_line(format!("branch {branch}")));
    }
    if let Some(ws) = &v.workspace_path {
        lines.push(detail_line(format!("workspace {ws}")));
    }
    if let Some(decision) = &v.pending_decision {
        lines.push(detail_line(format!("pending: {decision}")));
    }
    lines
}

/// Render a managed-session activity snapshot (`/activity`).
///
/// Why: the operator wants the classified state, a one-line summary, the runtime
/// liveness, and any surfaced decision at a glance.
/// What: a heading with the state/runtime, the summary line, and a pending
/// decision line when present.
/// Test: `render_result_renders_activity`.
fn render_activity(
    id: &str,
    state: &str,
    summary: &str,
    runtime_active: bool,
    pending_decision: &Option<String>,
) -> Vec<Line<'static>> {
    let runtime = if runtime_active { "alive" } else { "down" };
    let mut lines = vec![
        heading_line(format!(
            "activity {} [{state}] runtime {runtime}",
            short(id)
        )),
        detail_line(summary.to_string()),
    ];
    if let Some(decision) = pending_decision {
        lines.push(detail_line(format!("pending: {decision}")));
    }
    lines
}

/// Truncate an id to its short 8-char form for compact output.
///
/// Why: the output log echoes ids; the full UUID is noise, the 8-hex prefix is
/// enough to identify a session and matches the list's left column.
/// What: the first 8 characters of `id`.
/// Test: covered via the send/lifecycle render tests.
fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Build an accent (cyan/bold) heading line.
fn heading_line(text: String) -> Line<'static> {
    Line::from(Span::styled(text, heading_style()))
}

/// Build a dimmed detail line.
fn detail_line(text: String) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {text}"),
        Style::default().fg(Color::DarkGray),
    ))
}

/// Build a green "✓"-prefixed confirmation line for a successful mutation.
fn ok_line(text: String) -> Line<'static> {
    Line::from(Span::styled(
        format!("✓ {text}"),
        Style::default().fg(Color::Green),
    ))
}
