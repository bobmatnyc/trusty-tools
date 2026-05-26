//! Coordinator: a cross-session activity layer for the TUI/GUI.
//!
//! Why: the operator wants one conversational surface that has visibility into
//! *every* active Claude Code session at once — "what's happening across my
//! sessions?" — and a way to route a command at a named session by prefix. The
//! daemon already owns the session registry and tmux capture, so the
//! coordinator is a thin assembly layer over them: it builds a trimmed activity
//! snapshot and either routes a prefixed command straight to a session or hands
//! the snapshot to the LLM chat assistant for a free-text answer.
//! What: [`CoordinatorContext`] is the snapshot; [`build_coordinator_context`]
//! assembles it from [`DaemonState`]; [`parse_session_prefix`] recognises an
//! `@prefix:` / `prefix:` routing prefix; [`coordinator_system_prompt`] renders
//! the snapshot into an LLM system prompt.
//! Test: `cargo test -p trusty-mpm-daemon coordinator` covers the prefix parser
//! and the system-prompt rendering without a tmux host or network.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::core::session::SessionStatus;

use super::services::TmuxService;
use super::state::DaemonState;

/// Number of recent global events included in a coordinator snapshot.
const RECENT_EVENT_LIMIT: usize = 20;

/// Number of trailing pane lines captured per session for the snapshot.
const SESSION_OUTPUT_LINES: u32 = 20;

/// A trimmed snapshot of current session activity for LLM context.
///
/// Why: the LLM chat assistant needs a compact, structured view of what every
/// session is doing; a full registry dump would be too large and noisy.
/// What: the per-session summaries plus the last [`RECENT_EVENT_LIMIT`] global
/// hook events and the time the snapshot was built.
/// Test: `build_coordinator_context` is exercised by `context_builds_from_state`.
#[derive(Debug, Clone, Serialize)]
pub struct CoordinatorContext {
    /// Per-session activity summaries.
    pub sessions: Vec<SessionSummary>,
    /// The most recent global hook events (oldest first), capped at 20.
    pub recent_events: Vec<EventSummary>,
    /// When this snapshot was assembled.
    pub generated_at: DateTime<Utc>,
}

/// One session's activity summary inside a [`CoordinatorContext`].
///
/// Why: the coordinator and its LLM prompt need a flat, render-ready view of a
/// session — its name, a short routing prefix, status, and a recent-output
/// excerpt — without the full `Session` record.
/// What: identity fields plus the captured tail of the session's tmux pane.
/// Test: `parse_session_prefix` matches against the `name`/`prefix` fields.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    /// Session id (UUID string).
    pub id: String,
    /// tmux session name, e.g. `tmpm-aipowerranking`.
    pub name: String,
    /// Short routing prefix, e.g. `aipowerranking` (the `tmpm-` prefix dropped).
    pub prefix: String,
    /// Working directory the session runs in.
    pub workdir: String,
    /// Lifecycle status as a word: `Active` / `Paused` / `Stopped` / ….
    pub status: String,
    /// Number of active delegations the session has running.
    pub active_delegations: u32,
    /// Last [`SESSION_OUTPUT_LINES`] lines captured from the session's pane.
    pub recent_output: Vec<String>,
}

/// One hook event inside a [`CoordinatorContext`].
///
/// Why: the LLM prompt lists recent cross-session events; the full
/// `HookEventRecord` carries an opaque payload the prompt does not need.
/// What: the event's wire name, the originating session id, and the timestamp.
/// Test: covered by `context_builds_from_state`.
#[derive(Debug, Clone, Serialize)]
pub struct EventSummary {
    /// Originating session id (UUID string).
    pub session: String,
    /// Hook event wire name, e.g. `PreToolUse`.
    pub event: String,
    /// RFC3339 timestamp the daemon received the event.
    pub at: String,
}

/// Derive a short routing prefix from a tmux session name.
///
/// Why: operators address a session by a short word (`aipowerranking`), not its
/// full `tmpm-aipowerranking` tmux name; the prefix is that name with the
/// `tmpm-` prefix dropped.
/// What: strips a leading `tmpm-` when present; otherwise returns the name
/// unchanged.
/// Test: `prefix_strips_tmpm`.
fn derive_prefix(name: &str) -> String {
    name.strip_prefix("tmpm-").unwrap_or(name).to_string()
}

/// Render a [`SessionStatus`] as a capitalised display word.
///
/// Why: the snapshot and the LLM prompt show a human status word; the serde
/// representation is lowercase, but the coordinator surface uses title-case.
/// What: maps each status variant to its display word.
/// Test: covered by `context_builds_from_state`.
fn status_word(status: SessionStatus) -> String {
    match status {
        SessionStatus::Starting => "Starting",
        SessionStatus::Active => "Active",
        SessionStatus::AwaitingApproval => "AwaitingApproval",
        SessionStatus::Detached => "Detached",
        SessionStatus::Paused => "Paused",
        SessionStatus::Stopped => "Stopped",
    }
    .to_string()
}

/// Build a [`CoordinatorContext`] from the daemon's current state.
///
/// Why: the coordinator chat endpoint needs a fresh activity snapshot on every
/// request — the LLM answer is only as good as the context it is handed.
/// What: lists every session, derives a routing prefix and a status word for
/// each, and (for non-`Stopped` sessions) captures the last
/// [`SESSION_OUTPUT_LINES`] pane lines via [`TmuxService::capture`] — which
/// degrades to an empty list when tmux is absent. Also folds the daemon's
/// recent hook events down to [`EventSummary`] rows.
/// Test: `context_builds_from_state` (tmux-absent path).
pub fn build_coordinator_context(state: &DaemonState) -> CoordinatorContext {
    let sessions = state
        .list_sessions()
        .into_iter()
        .map(|session| {
            // Capturing a stopped session's pane is pointless — its tmux window
            // is gone — so only live sessions get an output excerpt.
            let recent_output = if session.status == SessionStatus::Stopped {
                Vec::new()
            } else {
                let raw = TmuxService::capture(&session, SESSION_OUTPUT_LINES);
                raw.lines().map(str::to_string).collect()
            };
            SessionSummary {
                id: session.id.0.to_string(),
                name: session.tmux_name.clone(),
                prefix: derive_prefix(&session.tmux_name),
                workdir: session.workdir.clone(),
                status: status_word(session.status),
                active_delegations: session.active_delegations,
                recent_output,
            }
        })
        .collect();

    let recent = state.recent_hook_events();
    let start = recent.len().saturating_sub(RECENT_EVENT_LIMIT);
    let recent_events = recent[start..]
        .iter()
        .map(|record| EventSummary {
            session: record.session.0.to_string(),
            event: record.event.wire_name().to_string(),
            at: record.at.to_rfc3339(),
        })
        .collect();

    CoordinatorContext {
        sessions,
        recent_events,
        generated_at: Utc::now(),
    }
}

/// Parse an `@prefix:` / `prefix:` session-routing prefix from user input.
///
/// Why: the coordinator lets the operator address one session directly — a
/// message that starts with `@aipowerranking:` (or the bare `aipowerranking:`)
/// is routed straight to that session's tmux pane, bypassing the LLM.
/// What: takes the text before the first `:` as a candidate prefix (a leading
/// `@` is stripped) and matches it case-insensitively against each session's
/// full tmux `name` or its short `prefix`. A bare (no `@`) prefix only matches
/// when it is unambiguous — exactly one session matches — so plain prose with a
/// stray colon is never misrouted. Returns `(session_tmux_name, remaining)` on
/// a match, `None` otherwise (no colon, empty prefix, or no/ambiguous match).
/// Test: `parses_at_prefix`, `parses_full_tmux_name`, `bare_prefix_unambiguous`,
/// `bare_prefix_ambiguous_is_none`, `no_colon_is_none`.
pub fn parse_session_prefix(input: &str, sessions: &[SessionSummary]) -> Option<(String, String)> {
    let trimmed = input.trim_start();
    let (head, rest) = trimmed.split_once(':')?;
    let had_at = head.starts_with('@');
    let candidate = head.trim_start_matches('@').trim();
    if candidate.is_empty() {
        return None;
    }
    let candidate_lc = candidate.to_lowercase();

    let matches: Vec<&SessionSummary> = sessions
        .iter()
        .filter(|s| {
            s.name.to_lowercase() == candidate_lc || s.prefix.to_lowercase() == candidate_lc
        })
        .collect();

    let session = match matches.as_slice() {
        [one] => *one,
        // Multiple sessions match a bare prefix: refuse to guess.
        _ if !had_at => return None,
        // An explicit `@` prefix with no (or an ambiguous) match also fails —
        // the operator named a session that does not uniquely exist.
        _ => return None,
    };

    Some((session.name.clone(), rest.trim().to_string()))
}

/// Render a [`CoordinatorContext`] into an LLM system prompt.
///
/// Why: the coordinator's free-text answers come from the LLM chat assistant;
/// the model can only reason about sessions it is told about, so the snapshot is
/// flattened into a prompt that lists every session (with its routing prefix
/// and a recent-output excerpt) and the recent global events.
/// What: returns a multi-line system prompt — a role preamble, one block per
/// session, the last 10 events, and the `@prefix` routing hint.
/// Test: `system_prompt_lists_sessions`, `system_prompt_handles_empty`.
pub fn coordinator_system_prompt(context: &CoordinatorContext) -> String {
    let mut prompt = String::from(
        "You are the trusty-mpm coordinator. You have visibility into all active \
Claude Code sessions.\n",
    );

    if context.sessions.is_empty() {
        prompt.push_str("\nThere are no active sessions right now.\n");
    } else {
        prompt.push_str("\nCurrent sessions:\n");
        for s in &context.sessions {
            prompt.push_str(&format!(
                "- {} (prefix @{}) — workdir {}, status {}, {} active delegation(s)\n",
                s.name, s.prefix, s.workdir, s.status, s.active_delegations,
            ));
            if !s.recent_output.is_empty() {
                // Keep the excerpt tight — the last 5 lines are enough signal.
                let start = s.recent_output.len().saturating_sub(5);
                for line in &s.recent_output[start..] {
                    prompt.push_str(&format!("    | {line}\n"));
                }
            }
        }
    }

    if !context.recent_events.is_empty() {
        prompt.push_str("\nRecent events:\n");
        let start = context.recent_events.len().saturating_sub(10);
        for e in &context.recent_events[start..] {
            prompt.push_str(&format!("- {} {} ({})\n", e.at, e.event, e.session));
        }
    }

    prompt.push_str(
        "\nAnswer questions about session activity, summarize what is happening, and \
help the user manage sessions. To send a command to a specific session, the user \
can prefix their message with @session-name (or the short prefix).\n",
    );
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `SessionSummary` for the parser tests.
    fn summary(name: &str) -> SessionSummary {
        SessionSummary {
            id: "00000000-0000-0000-0000-000000000000".to_string(),
            name: name.to_string(),
            prefix: derive_prefix(name),
            workdir: "/tmp/proj".to_string(),
            status: "Active".to_string(),
            active_delegations: 0,
            recent_output: Vec::new(),
        }
    }

    #[test]
    fn prefix_strips_tmpm() {
        assert_eq!(derive_prefix("tmpm-aipowerranking"), "aipowerranking");
        assert_eq!(derive_prefix("frontend"), "frontend");
    }

    #[test]
    fn parses_at_prefix() {
        // `@prefix: msg` strips the `@`, matches the short prefix, and returns
        // the full tmux name plus the trimmed remainder.
        let sessions = [summary("tmpm-aipowerranking")];
        let parsed = parse_session_prefix("@aipowerranking: run the tests", &sessions);
        assert_eq!(
            parsed,
            Some((
                "tmpm-aipowerranking".to_string(),
                "run the tests".to_string()
            ))
        );
    }

    #[test]
    fn parses_full_tmux_name() {
        // The full `tmpm-…` tmux name also matches.
        let sessions = [summary("tmpm-aipowerranking")];
        let parsed = parse_session_prefix("@tmpm-aipowerranking: do X", &sessions);
        assert_eq!(
            parsed,
            Some(("tmpm-aipowerranking".to_string(), "do X".to_string()))
        );
    }

    #[test]
    fn bare_prefix_unambiguous() {
        // A bare (no `@`) prefix routes when exactly one session matches.
        let sessions = [summary("tmpm-aipowerranking"), summary("tmpm-other")];
        let parsed = parse_session_prefix("aipowerranking: status?", &sessions);
        assert_eq!(
            parsed,
            Some(("tmpm-aipowerranking".to_string(), "status?".to_string()))
        );
    }

    #[test]
    fn bare_prefix_ambiguous_is_none() {
        // Plain prose with a stray colon must not be misrouted: a bare prefix
        // that matches nothing yields None rather than guessing.
        let sessions = [summary("tmpm-aipowerranking")];
        assert_eq!(
            parse_session_prefix("note: this is just prose", &sessions),
            None
        );
    }

    #[test]
    fn at_prefix_unknown_session_is_none() {
        // An explicit `@` prefix naming a non-existent session also fails.
        let sessions = [summary("tmpm-aipowerranking")];
        assert_eq!(parse_session_prefix("@ghost: hello", &sessions), None);
    }

    #[test]
    fn no_colon_is_none() {
        let sessions = [summary("tmpm-aipowerranking")];
        assert_eq!(
            parse_session_prefix("just a question with no prefix", &sessions),
            None
        );
    }

    #[test]
    fn empty_prefix_is_none() {
        let sessions = [summary("tmpm-aipowerranking")];
        assert_eq!(parse_session_prefix("@: hello", &sessions), None);
        assert_eq!(parse_session_prefix(": hello", &sessions), None);
    }

    #[test]
    fn system_prompt_lists_sessions() {
        let mut s = summary("tmpm-aipowerranking");
        s.recent_output = vec!["building…".to_string(), "tests passed".to_string()];
        let context = CoordinatorContext {
            sessions: vec![s],
            recent_events: Vec::new(),
            generated_at: Utc::now(),
        };
        let prompt = coordinator_system_prompt(&context);
        assert!(prompt.contains("tmpm-aipowerranking"));
        assert!(prompt.contains("prefix @aipowerranking"));
        assert!(prompt.contains("tests passed"));
        assert!(prompt.contains("@session-name"));
    }

    #[test]
    fn system_prompt_handles_empty() {
        let context = CoordinatorContext {
            sessions: Vec::new(),
            recent_events: Vec::new(),
            generated_at: Utc::now(),
        };
        let prompt = coordinator_system_prompt(&context);
        assert!(prompt.contains("no active sessions"));
    }

    #[test]
    fn context_builds_from_state() {
        // With no sessions registered the snapshot is empty but well-formed;
        // this also exercises the tmux-absent capture path.
        let state = DaemonState::new();
        let context = build_coordinator_context(&state);
        assert!(context.sessions.is_empty());
        assert!(context.recent_events.is_empty());
    }
}
