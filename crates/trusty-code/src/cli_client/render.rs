//! Pure, unit-tested formatting for the `tcode` CLI's human-readable output
//! (#2060).
//!
//! Why: every subcommand handler in the binary needs to turn a wire-shaped
//! `Session`/`SessionEventEnvelope`/`TranscriptRecord` into something a
//! terminal user can read. Keeping the formatting logic here (rather than
//! inline `println!`s scattered across the binary's subcommand handlers)
//! makes it independently unit-testable without spawning a subprocess, and
//! keeps each handler a thin "call the client, print the result" wrapper.
//! What: [`render_session_table`] (a fixed-width table for `session list`),
//! [`render_event_line`] (one line per streamed [`SessionEventEnvelope`],
//! used by `attach`/`run-task`), [`render_transcript_human`] (a readable
//! turn-by-turn view for `transcript`), and [`exit_code_for_status`] (maps a
//! terminal `SessionStatus` onto the SAME `run_task::ExitCode` values the
//! legacy in-process `run-task` path already used, so scripts checking `$?`
//! see a consistent contract regardless of which path produced the run).
//! Test: `render::tests::*`.

use crate::events::{Event, SessionEventEnvelope};
use crate::run_task::ExitCode;
use crate::session::{Session, SessionStatus, TranscriptRecord};

/// Render a fixed-width table of sessions for `tcode session list`.
///
/// Why: `session.list`'s raw JSON is unreadable at a glance; operators want
/// id/status/task/agent at one line each.
/// What: one header line + one row per session, columns
/// `ID | STATUS | AGENT | TASK` (task truncated to keep rows terminal-width
/// friendly). An empty list renders a single explanatory line, not an empty
/// string, so scripts piping through a pager see something.
/// Test: `tests::render_session_table_lists_every_session`,
/// `tests::render_session_table_handles_empty_list`.
pub fn render_session_table(sessions: &[Session]) -> String {
    if sessions.is_empty() {
        return "(no sessions)".to_string();
    }
    let mut out =
        String::from("ID                                    STATUS     AGENT           TASK\n");
    for s in sessions {
        let agent = s.agent.as_deref().unwrap_or("-");
        let task_preview = truncate(&s.task, 40);
        out.push_str(&format!(
            "{:<38}{:<11}{:<16}{}\n",
            s.id,
            s.status.as_str(),
            truncate(agent, 15),
            task_preview
        ));
    }
    out.pop(); // drop the trailing newline; callers add their own via println!
    out
}

/// Render one line for a single streamed event, for `attach`/`run-task`.
///
/// Why: a human watching a live run wants a compact, timestamped narration
/// — not the raw internally-tagged JSON.
/// What: matches the handful of variants the M1 cut line actually produces
/// (session lifecycle, tool lifecycle, generic message/log/progress) with a
/// readable one-liner; anything else falls back to `[<kind>]` using the
/// envelope's own `kind` string rather than an exhaustive match on
/// [`Event`], so a future variant added elsewhere never breaks this
/// function's compilation.
/// Test: `tests::render_event_line_covers_key_variants`,
/// `tests::render_event_line_falls_back_for_unknown_kind`.
pub fn render_event_line(envelope: &SessionEventEnvelope) -> String {
    let ts = envelope.at.format("%H:%M:%S");
    let detail = match &envelope.event {
        Event::SessionStarted { project, .. } => format!("session started (project: {project})"),
        Event::SessionStatusChanged { status, .. } => format!("status -> {status}"),
        Event::SessionDone { status, .. } => format!("done ({status})"),
        Event::SessionCancelled { .. } => "cancelled".to_string(),
        Event::ToolStarted {
            tool, args_preview, ..
        } => format!("tool_started  {tool}  {args_preview}"),
        Event::ToolFinished {
            tool,
            success,
            result_preview,
            ..
        } => format!(
            "tool_finished {tool}  {} {result_preview}",
            if *success { "ok" } else { "error" }
        ),
        Event::ToolError { tool, error, .. } => format!("tool_error    {tool}  {error}"),
        Event::Message { text, .. } => text.clone(),
        Event::Log { level, message, .. } => format!("[{level}] {message}"),
        Event::Progress {
            message, percent, ..
        } => match percent {
            Some(p) => format!("{message} ({p:.0}%)"),
            None => message.clone(),
        },
        _ => format!("[{}]", envelope.kind),
    };
    format!("[{ts}] {detail}")
}

/// Render a `TranscriptRecord` as a readable turn-by-turn view for
/// `tcode transcript` (human mode; `--json` bypasses this entirely and
/// pretty-prints the raw result).
///
/// Why: a bare JSON dump of the transcript is hard to scan; a human wants
/// "who said what, with which tools" plus the cost/usage summary.
/// What: one block per turn (`role` header, prose if any, tool calls if
/// any), then a trailing usage/cost summary line. A never-run session (empty
/// `turns`) renders a single explanatory line rather than an empty string.
/// Test: `tests::render_transcript_human_lists_turns_and_totals`,
/// `tests::render_transcript_human_handles_never_run_session`.
pub fn render_transcript_human(record: &TranscriptRecord) -> String {
    if record.turns.is_empty() {
        return format!(
            "session {} has not run a task yet (no transcript)",
            record.session_id
        );
    }
    let mut out = String::new();
    for turn in &record.turns {
        out.push_str(&format!("--- {} ({}) ---\n", turn.role, turn.model));
        if !turn.text.is_empty() {
            out.push_str(&turn.text);
            out.push('\n');
        }
        if !turn.tool_calls.is_empty() {
            out.push_str(&format!("tool calls: {}\n", turn.tool_calls.join(", ")));
        }
    }
    out.push_str(&format!(
        "\ntotal: {} prompt + {} completion tokens, cost: {}\n",
        record.usage.prompt_tokens,
        record.usage.completion_tokens,
        record
            .cost_usd
            .map(|c| format!("${c:.4}"))
            .unwrap_or_else(|| "unknown".to_string())
    ));
    out.pop();
    out
}

/// Map a terminal `SessionStatus` onto the CLI process exit code.
///
/// Why: `run-task` via the thin client and the legacy in-process
/// `run-task` must agree on what a script checking `$?` sees.
/// What: `Finished` -> `ExitCode::Success`; `Cancelled`/`Failed` ->
/// `ExitCode::RunFailure`; `Created`/`Running` (should never be passed here
/// — the caller only calls this once a run has reached a terminal status)
/// also map to `RunFailure` defensively rather than panicking.
/// Test: `tests::exit_code_for_status_maps_terminal_states`.
pub fn exit_code_for_status(status: SessionStatus) -> ExitCode {
    match status {
        SessionStatus::Finished => ExitCode::Success,
        SessionStatus::Cancelled | SessionStatus::Failed => ExitCode::RunFailure,
        SessionStatus::Created | SessionStatus::Running => ExitCode::RunFailure,
    }
}

/// Truncate `s` to at most `max` chars, appending `…` when it was longer.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::perf::TokenUsage;
    use crate::run_task::TurnRecord;
    use chrono::Utc;

    fn session(id: &str, status: SessionStatus) -> Session {
        Session {
            id: id.to_string(),
            task: "do the thing".to_string(),
            agent: Some("pm".to_string()),
            project: None,
            status,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn render_session_table_lists_every_session() {
        let sessions = vec![
            session("s-1", SessionStatus::Running),
            session("s-2", SessionStatus::Finished),
        ];
        let table = render_session_table(&sessions);
        assert!(table.contains("s-1"));
        assert!(table.contains("running"));
        assert!(table.contains("s-2"));
        assert!(table.contains("finished"));
    }

    #[test]
    fn render_session_table_handles_empty_list() {
        assert_eq!(render_session_table(&[]), "(no sessions)");
    }

    fn envelope(event: Event) -> SessionEventEnvelope {
        SessionEventEnvelope::new("s-1".to_string(), 1, Utc::now(), event)
    }

    #[test]
    fn render_event_line_covers_key_variants() {
        let line = render_event_line(&envelope(Event::ToolStarted {
            session_id: "s-1".to_string(),
            tool: "bash".to_string(),
            call_id: "c1".to_string(),
            args_preview: "echo hi".to_string(),
        }));
        assert!(line.contains("tool_started"));
        assert!(line.contains("bash"));
        assert!(line.contains("echo hi"));

        let line = render_event_line(&envelope(Event::SessionDone {
            session_id: "s-1".to_string(),
            status: "finished".to_string(),
        }));
        assert!(line.contains("done (finished)"));
    }

    #[test]
    fn render_event_line_falls_back_for_unknown_kind() {
        let line = render_event_line(&envelope(Event::Ping));
        assert!(line.contains("[ping]"));
    }

    #[test]
    fn render_transcript_human_lists_turns_and_totals() {
        let record = TranscriptRecord {
            session_id: "s-1".to_string(),
            turns: vec![TurnRecord {
                role: "pm".to_string(),
                model: "openai/gpt-4o-mini".to_string(),
                text: "done".to_string(),
                tool_calls: vec!["delegate_to_agent".to_string()],
                usage: TokenUsage::new(10, 5, 0, 0),
            }],
            usage: TokenUsage::new(10, 5, 0, 0),
            cost_usd: Some(0.01),
        };
        let out = render_transcript_human(&record);
        assert!(out.contains("pm"));
        assert!(out.contains("done"));
        assert!(out.contains("delegate_to_agent"));
        assert!(out.contains("$0.0100"));
    }

    #[test]
    fn render_transcript_human_handles_never_run_session() {
        let record = TranscriptRecord {
            session_id: "s-1".to_string(),
            turns: vec![],
            usage: TokenUsage::default(),
            cost_usd: None,
        };
        let out = render_transcript_human(&record);
        assert!(out.contains("s-1"));
        assert!(out.contains("has not run"));
    }

    #[test]
    fn exit_code_for_status_maps_terminal_states() {
        assert_eq!(
            exit_code_for_status(SessionStatus::Finished).code(),
            ExitCode::Success.code()
        );
        assert_eq!(
            exit_code_for_status(SessionStatus::Cancelled).code(),
            ExitCode::RunFailure.code()
        );
        assert_eq!(
            exit_code_for_status(SessionStatus::Failed).code(),
            ExitCode::RunFailure.code()
        );
    }
}
