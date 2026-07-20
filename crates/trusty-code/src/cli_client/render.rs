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
//! turn-by-turn view for `transcript`), [`render_workstream_table`] (a
//! tmux-`list-sessions`-style table for `tcode workstream list`, #3296), and
//! [`exit_code_for_status`] (maps a terminal `SessionStatus` onto the SAME
//! `run_task::ExitCode` values the legacy in-process `run-task` path already
//! used, so scripts checking `$?` see a consistent contract regardless of
//! which path produced the run).
//! Test: `render::tests::*`.

use crate::events::{Event, SessionEventEnvelope};
use crate::run_task::ExitCode;
use crate::session::{Session, SessionStatus, TranscriptRecord};
use crate::workstreams::{WorkstreamId, WorkstreamState};

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
        // (UI Phase 1) `agent` prefixes every tool line so an attached CLI
        // shows WHO made each call — the same attribution the SPA renders.
        Event::ToolStarted {
            agent,
            tool,
            args_preview,
            ..
        } => format!("tool_started  [{agent}] {tool}  {args_preview}"),
        Event::ToolFinished {
            agent,
            tool,
            success,
            result_preview,
            ..
        } => format!(
            "tool_finished [{agent}] {tool}  {} {result_preview}",
            if *success { "ok" } else { "error" }
        ),
        Event::ToolError {
            agent, tool, error, ..
        } => format!("tool_error    [{agent}] {tool}  {error}"),
        Event::SearchPerformed {
            agent,
            lane,
            query,
            hit_count,
            latency_ms,
            ..
        } => format!(
            "search        [{agent}] {lane}  \"{query}\"  {} hits  {latency_ms}ms",
            hit_count.map_or_else(|| "?".to_string(), |c| c.to_string())
        ),
        Event::MemoryRecalled {
            agent,
            query,
            results,
            ..
        } => format!(
            "recall        [{agent}] \"{query}\"  {} injected / {} recalled",
            results.iter().filter(|r| r.injected).count(),
            results.len()
        ),
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

/// One row of `workstream.list`'s wire response, as needed to render
/// [`render_workstream_table`] (#3296, DOC-48 §5.4).
///
/// Why: the daemon's `workstream.list` response nests each record as
/// `crate::workstreams::protocol::WorkstreamView` — private to that module,
/// and carrying fields (`metadata`, `created_at`) the table never displays.
/// This is the CLI's own minimal read-only projection of that wire shape;
/// serde's default "ignore unknown fields" behavior means it deserializes
/// straight out of the same JSON without the protocol module needing to
/// expose anything new.
/// What: `id`/`state` reuse the library's own [`WorkstreamId`]/
/// [`WorkstreamState`] types (both already `pub` from `crate::workstreams`)
/// so the table can never disagree with the domain model on what "active"
/// means; `session_ids` is only read for its length (live session count).
/// Test: `tests::render_workstream_table_marks_the_active_row`.
#[derive(Debug, serde::Deserialize)]
pub struct WorkstreamRow {
    pub id: WorkstreamId,
    #[serde(default)]
    pub name: String,
    pub state: WorkstreamState,
    #[serde(default)]
    pub session_ids: Vec<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Render a tmux-`list-sessions`-style table of workstreams for
/// `tcode workstream list` (DOC-48 §5.4, AC-5.1, issue #3296).
///
/// Why: rhymes with `tm`'s session-listing conventions (fixed-width columns,
/// a leading marker for "the one that's current") WITHOUT importing from
/// `trusty-mpm` — this crate has no dependency on it, so the shape is
/// reinvented small and pure here. The leading `*` marker mirrors tmux's own
/// `list-sessions` attached-session convention, which DOC-48 §5.3 already
/// cites as the mental model for workstream observation ("tmux-attach-like
/// behavior").
/// What: one header line + one row per workstream: a leading `*` on the
/// active workstream (blank otherwise), an 8-hex-char id prefix (DISPLAY
/// ONLY — `tcode workstream get`/`activate`/`close` still require the full
/// id; see `crate::cli::workstream`'s module docs in the `tcode` binary for
/// why prefix resolution isn't implemented client-side), the computed
/// state, the live session count, a humanized `updated_at` age, and the
/// name (`(untitled)` when empty, DOC-48 §5.1: "Name is initially empty").
/// An empty list renders one explanatory line instead of a bare header.
/// Test: `tests::render_workstream_table_marks_the_active_row`,
/// `tests::render_workstream_table_handles_empty_list`.
pub fn render_workstream_table(rows: &[WorkstreamRow]) -> String {
    if rows.is_empty() {
        return "(no workstreams)".to_string();
    }
    let mut out = String::from("  ID        STATE   SESSIONS  UPDATED   NAME\n");
    for r in rows {
        let marker = if r.state == WorkstreamState::Active {
            '*'
        } else {
            ' '
        };
        let id_prefix: String = r.id.to_string().chars().take(8).collect();
        let name = if r.name.is_empty() {
            "(untitled)"
        } else {
            &r.name
        };
        out.push_str(&format!(
            "{marker} {:<9} {:<7} {:<9} {:<9} {}\n",
            id_prefix,
            r.state.to_string(),
            r.session_ids.len(),
            humanize_age(r.updated_at),
            truncate(name, 40),
        ));
    }
    out.pop(); // drop the trailing newline; callers add their own via println!
    out
}

/// Render a compact "time ago" string for a past UTC timestamp.
///
/// Why: `updated_at`'s raw ISO-8601 timestamp is hard to scan at a glance;
/// an operator wants "how stale is this workstream" in one short token.
/// What: buckets the age into `just now` (<60s), `<N>m ago` (<1h), `<N>h ago`
/// (<24h), `<N>d ago` (<30d), or `<N>w ago` beyond that. Never panics on a
/// future timestamp (clock skew) — `signed_duration_since` is clamped to a
/// minimum of zero seconds.
/// Test: `tests::humanize_age_buckets_common_durations`.
fn humanize_age(at: chrono::DateTime<chrono::Utc>) -> String {
    let secs = chrono::Utc::now()
        .signed_duration_since(at)
        .num_seconds()
        .max(0);
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else if secs < 30 * 86_400 {
        format!("{}d ago", secs / 86_400)
    } else {
        format!("{}w ago", secs / (7 * 86_400))
    }
}

/// Map a terminal `SessionStatus` onto the CLI process exit code.
///
/// Why: `run-task` via the thin client and the legacy in-process
/// `run-task` must agree on what a script checking `$?` sees.
/// What: `Finished` -> `ExitCode::Success`; `Cancelled`/`Failed` ->
/// `ExitCode::RunFailure`; `DeadlineExceeded` (#2207) -> its own distinct
/// `ExitCode::DeadlineExceeded`, so a script can tell "timed out" from
/// "errored" the same way the legacy path's `RunReport::exit` does;
/// `Created`/`Running` (should never be passed here — the caller only calls
/// this once a run has reached a terminal status) also map to `RunFailure`
/// defensively rather than panicking.
/// Test: `tests::exit_code_for_status_maps_terminal_states`.
pub fn exit_code_for_status(status: SessionStatus) -> ExitCode {
    match status {
        SessionStatus::Finished => ExitCode::Success,
        SessionStatus::Cancelled | SessionStatus::Failed => ExitCode::RunFailure,
        SessionStatus::DeadlineExceeded => ExitCode::DeadlineExceeded,
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
            binding: crate::binding::ProjectBinding::None,
            status,
            created_at: Utc::now(),
            mode: None,
            workstream_id: None,
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
            agent: "python-engineer".to_string(),
            agent_id: "eng-1".to_string(),
            tool: "bash".to_string(),
            call_id: "c1".to_string(),
            args_preview: "echo hi".to_string(),
        }));
        assert!(line.contains("tool_started"));
        assert!(line.contains("bash"));
        assert!(line.contains("echo hi"));
        assert!(
            line.contains("[python-engineer]"),
            "an attached CLI must show WHO made the call: {line}"
        );

        // (UI Phase 1) The two structured retrieval events render their own
        // lines rather than degrading to the generic `[kind]` fallback.
        let line = render_event_line(&envelope(Event::SearchPerformed {
            session_id: "s-1".to_string(),
            agent: "python-engineer".to_string(),
            agent_id: "eng-1".to_string(),
            lane: "lexical".to_string(),
            query: "where is auth".to_string(),
            hit_count: Some(3),
            hits: vec![],
            latency_ms: 42,
        }));
        assert!(line.contains("[python-engineer]"), "{line}");
        assert!(line.contains("lexical"), "the REAL routed lane: {line}");
        assert!(line.contains("3 hits"), "{line}");

        let line = render_event_line(&envelope(Event::MemoryRecalled {
            session_id: "s-1".to_string(),
            agent: "pm".to_string(),
            agent_id: "pm-1".to_string(),
            query: "pkce".to_string(),
            results: vec![
                crate::events::RecalledMemory {
                    score: 0.9,
                    injected: true,
                    text: "injected memory".to_string(),
                    run_id: None,
                },
                crate::events::RecalledMemory {
                    score: 0.41,
                    injected: false,
                    text: "held-back memory".to_string(),
                    run_id: None,
                },
            ],
        }));
        assert!(
            line.contains("1 injected / 2 recalled"),
            "the injected/held-back split is the point: {line}"
        );

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
                ran_test_command: false,
                usage: TokenUsage::new(10, 5, 0, 0),
            }],
            usage: TokenUsage::new(10, 5, 0, 0),
            cost_usd: Some(0.01),
            mode: Some(crate::mode::HarnessMode::DailyDriver),
            compaction_events: 0,
            goals: vec![],
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
            mode: None,
            compaction_events: 0,
            goals: vec![],
        };
        let out = render_transcript_human(&record);
        assert!(out.contains("s-1"));
        assert!(out.contains("has not run"));
    }

    #[test]
    fn render_workstream_table_marks_the_active_row() {
        use uuid::Uuid;

        let active_id =
            WorkstreamId(Uuid::parse_str("a1b2c3d4-e5f6-4748-9a0b-1c2d3e4f5a6b").unwrap());
        let idle_id =
            WorkstreamId(Uuid::parse_str("b2c3d4e5-f6a1-4748-9a0b-1c2d3e4f5a6b").unwrap());
        let rows = vec![
            WorkstreamRow {
                id: active_id,
                name: "Token rotation hardening".to_string(),
                state: WorkstreamState::Active,
                session_ids: vec!["s-1".to_string(), "s-2".to_string()],
                updated_at: Utc::now(),
            },
            WorkstreamRow {
                id: idle_id,
                name: String::new(),
                state: WorkstreamState::Idle,
                session_ids: vec![],
                updated_at: Utc::now() - chrono::Duration::hours(2),
            },
        ];
        let table = render_workstream_table(&rows);
        assert!(
            table.contains('*'),
            "active row must carry the marker: {table}"
        );
        assert!(
            table.contains("a1b2c3d4"),
            "expected the 8-char id prefix: {table}"
        );
        assert!(table.contains("active"));
        assert!(table.contains("Token rotation hardening"));
        assert!(
            table.contains("(untitled)"),
            "an empty name must render a placeholder: {table}"
        );
        assert!(
            table.contains("2h ago"),
            "expected a humanized age: {table}"
        );
        assert!(
            table.contains('2'),
            "active row's session count must be 2: {table}"
        );
    }

    #[test]
    fn render_workstream_table_handles_empty_list() {
        assert_eq!(render_workstream_table(&[]), "(no workstreams)");
    }

    #[test]
    fn humanize_age_buckets_common_durations() {
        assert_eq!(humanize_age(Utc::now()), "just now");
        assert_eq!(
            humanize_age(Utc::now() - chrono::Duration::minutes(5)),
            "5m ago"
        );
        assert_eq!(
            humanize_age(Utc::now() - chrono::Duration::hours(3)),
            "3h ago"
        );
        assert_eq!(
            humanize_age(Utc::now() - chrono::Duration::days(2)),
            "2d ago"
        );
        assert_eq!(
            humanize_age(Utc::now() - chrono::Duration::days(45)),
            "6w ago"
        );
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
        assert_eq!(
            exit_code_for_status(SessionStatus::DeadlineExceeded).code(),
            ExitCode::DeadlineExceeded.code()
        );
    }
}
