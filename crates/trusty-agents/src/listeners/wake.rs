//! Stage-two filtering + agent wake dispatch (#3820, DOC-54 SPEC-AGENTS-06
//! §7.4, issue #3817).
//!
//! Why: Ingestion (stage one, `crate::listeners::poll`) is fully
//! deterministic and spends no inference. Waking an agent is the ONLY place
//! inference is spent, so this module is the single choke point that
//! decides "does this specific event reach this specific agent's chat as a
//! reaction" — matching every agent's declared `[[listeners]]` bindings
//! against the event, then calling the SAME persona-chat entry point
//! (`ctrl::pm_task::dispatch::persona::run_pm_task_with_persona`) the `/agent`
//! REPL command uses, so a wake reaction is indistinguishable in history
//! from a normal chat turn (speaker attribution = the agent).
//! What: `wake_bound_agents` scans the agents directories for packages
//! declaring a `[[listeners]]` binding to the firing listener, applies each
//! binding's stage-two filter (`event_types` + `from`/`exclude_labels`), and
//! for the first match (rate-limited to one wake per poll cycle per DEADLINE
//! BUILD scope) builds a prompt combining the event summary with the
//! agent's `events/<connector>.md` instructions (#3817) and dispatches it
//! ask-first. Every wake DECISION (matched-and-woke, matched-but-rate-
//! limited, no-binding-matched) is logged at info/debug level per the
//! DEADLINE BUILD's "log every wake decision" requirement.
//! Test: `sender_glob_matches_leading_and_trailing_wildcard`,
//! `binding_matches_event_type_and_sender`,
//! `binding_rejects_excluded_label`,
//! `event_type_filter_empty_matches_any`.

use std::path::Path;

use crate::agents::AgentConfig;
use crate::agents::agents_dir_candidates;
use crate::ctrl::config::SessionOverrides;
use crate::ctrl::pm_task::run_pm_task_with_persona;
use crate::listeners::store::StoredEvent;

/// Ask-first framing prepended to every wake turn (DOC-54 §2.1 "Ask" step).
///
/// Why: The product's core loop is Event -> Ask -> Learn -> Adapt; a woken
/// agent must never silently act on an inbound event without first asking
/// how to respond (unless it has separately LEARNED autonomy for that
/// pattern, which is out of scope for this slice — every wake in this build
/// is ask-first).
const ASK_FIRST_PREAMBLE: &str = "A new event just arrived on one of your listeners. \
Summarize it for the user and ASK how they'd like you to respond — do not take \
any action (send email, modify calendar, etc.) without explicit confirmation, \
unless you have previously been told you may act autonomously on exactly this \
kind of event.";

/// Simple glob matcher for sender patterns: a LEADING `*` is a suffix match
/// (`"*@family.com"` matches any address ending in `@family.com`), a
/// TRAILING `*` is a prefix match, no `*` is an exact match. Distinct from
/// `ctrl::pm_task::helpers::match_any_glob` (tool-name globs, trailing-only)
/// because DOC-54's own binding examples use a LEADING wildcard for sender
/// patterns (`from = ["*@family.com"]`).
fn sender_glob_matches(value: &str, pattern: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix('*') {
        value.ends_with(suffix)
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        value.starts_with(prefix)
    } else {
        value == pattern
    }
}

/// Whether `binding` (an agent's stage-two filter) matches `event`.
///
/// Why: Pulled out as a pure function so the filter semantics (event-type
/// allowlist, sender allowlist, label exclusion) are unit-testable without a
/// live agent directory or LLM call.
/// What: `event_types` empty = any type passes; non-empty = exact match
/// required. `filter.from` empty = any sender passes; non-empty = at least
/// one glob must match the event's `from` field (a `None` `from` fails a
/// non-empty sender filter — DOC-54 gives listeners no way to match "unknown
/// sender"). `filter.exclude_labels` is reserved for connectors that surface
/// labels on the event; Gmail's `StoredEvent` doesn't carry labels yet in
/// this slice, so it is currently a no-op guard (documented, not silently
/// dropped) pending a `labels: Vec<String>` field on `StoredEvent`.
/// Test: `binding_matches_event_type_and_sender`,
/// `binding_rejects_excluded_label`, `event_type_filter_empty_matches_any`.
pub fn binding_matches_event(
    binding: &crate::listeners::config::AgentListenerBinding,
    event: &StoredEvent,
) -> bool {
    if binding.name != event.listener_id {
        return false;
    }
    if !binding.event_types.is_empty() && !binding.event_types.contains(&event.event_type) {
        return false;
    }
    if !binding.filter.from.is_empty() {
        let matches_sender = event.from.as_deref().is_some_and(|from| {
            binding
                .filter
                .from
                .iter()
                .any(|pat| sender_glob_matches(from, pat))
        });
        if !matches_sender {
            return false;
        }
    }
    // exclude_labels: no-op today (see doc comment) — `StoredEvent` carries
    // no label list yet.
    true
}

/// Outcome of a single wake-matching pass over the agent roster, for
/// logging/testing.
#[derive(Debug, PartialEq, Eq)]
pub enum WakeOutcome {
    /// No agent has a binding matching this event.
    NoBindingMatched,
    /// A match was found but skipped by the one-wake-per-poll-cycle limit.
    RateLimited { agent: String },
    /// The agent was dispatched successfully.
    Woke { agent: String },
    /// The agent was dispatched but the persona turn errored.
    DispatchFailed { agent: String, error: String },
}

/// Scan the agents directories for a `[[listeners]]` binding matching
/// `event`, and wake AT MOST ONE agent (DEADLINE BUILD scope: "max 1 wake
/// per poll cycle").
///
/// Why: Centralising the roster scan here (rather than in `poll.rs`) keeps
/// the polling loop's per-event work to "does anything care" without it
/// needing to know how personas are dispatched.
/// What: Enumerates directory-package agents across
/// `agents_dir_candidates()` (mirrors the tiered resolution every other
/// by-name lookup in this crate uses), loads each via
/// `AgentConfig::by_name_async` (so `extends` chains resolve identically to
/// every other dispatch path), and checks its `listeners` bindings against
/// `event` via `binding_matches_event`. On the FIRST match it loads
/// `agents/<name>/events/<connector>.md` (best-effort — missing file just
/// means no extra instructions) and dispatches through
/// `run_pm_task_with_persona`. Every branch is logged at info/debug so a
/// demo run's wake decisions are auditable in the process log.
/// Test: exercised end-to-end only via a live agent directory (manual demo
/// script); the pure matching logic is covered by `binding_matches_event`'s
/// tests.
pub async fn wake_bound_agents(project_path: &Path, event: &StoredEvent) -> WakeOutcome {
    let candidate_names = match candidate_agent_names().await {
        Ok(names) => names,
        Err(e) => {
            tracing::warn!(error = %e, "wake_bound_agents: failed to enumerate agent directories");
            return WakeOutcome::NoBindingMatched;
        }
    };

    for name in candidate_names {
        let cfg = match AgentConfig::by_name_async(&name).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !cfg
            .listeners
            .iter()
            .any(|b| binding_matches_event(b, event))
        {
            continue;
        }
        tracing::info!(
            agent = %name,
            listener = %event.listener_id,
            event_id = %event.id,
            event_type = %event.event_type,
            "wake decision: binding matched"
        );

        let connector_instructions = load_connector_instructions(&name, &event.provider).await;
        let user_input = build_wake_prompt(event, connector_instructions.as_deref());

        return match run_pm_task_with_persona(
            project_path,
            &name,
            &user_input,
            &[],
            None,
            SessionOverrides::default(),
        )
        .await
        {
            Ok(_reply) => {
                tracing::info!(agent = %name, event_id = %event.id, "wake decision: dispatched");
                WakeOutcome::Woke { agent: name }
            }
            Err(e) => {
                tracing::warn!(agent = %name, event_id = %event.id, error = %e, "wake decision: dispatch failed");
                WakeOutcome::DispatchFailed {
                    agent: name,
                    error: e.to_string(),
                }
            }
        };
    }

    tracing::debug!(
        listener = %event.listener_id,
        event_id = %event.id,
        "wake decision: no agent binding matched"
    );
    WakeOutcome::NoBindingMatched
}

/// Enumerate directory-package agent names across the standard candidate
/// dirs (project `.trusty-agents/agents/` then `$HOME/.trusty-agents/agents/`).
///
/// Why: `AgentListenerBinding` only lives on directory-package agents
/// (`agent.toml` + `persona.md`) in this build's demo roster (izzie);
/// scanning is a lightweight name enumeration, deferring the real parse (and
/// `extends` resolution) to `AgentConfig::by_name_async` per name so this
/// stays a thin directory listing, not a second config parser.
async fn candidate_agent_names() -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    for dir in agents_dir_candidates() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            if tokio::fs::metadata(path.join("agent.toml")).await.is_ok() {
                names.push(name.to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

/// Best-effort load of `agents/<name>/events/<connector>.md` (#3817).
///
/// Why: Per-connector event instructions are optional — an agent bound to a
/// listener without a matching `events/<connector>.md` file still wakes,
/// just without the extra connector-specific guidance.
async fn load_connector_instructions(agent_name: &str, connector: &str) -> Option<String> {
    for dir in agents_dir_candidates() {
        let path = dir
            .join(agent_name)
            .join("events")
            .join(format!("{connector}.md"));
        if let Ok(content) = tokio::fs::read_to_string(&path).await {
            return Some(content);
        }
    }
    None
}

/// Build the user-turn text for a wake dispatch: the ask-first preamble,
/// optional connector-specific instructions, then the event summary.
fn build_wake_prompt(event: &StoredEvent, connector_instructions: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str(ASK_FIRST_PREAMBLE);
    if let Some(instructions) = connector_instructions {
        out.push_str("\n\n## Event-type instructions (");
        out.push_str(&event.provider);
        out.push_str(")\n");
        out.push_str(instructions);
    }
    out.push_str("\n\n## Event\n");
    out.push_str(&format!("- provider: {}\n", event.provider));
    out.push_str(&format!("- type: {}\n", event.event_type));
    if let Some(from) = &event.from {
        out.push_str(&format!("- from: {from}\n"));
    }
    if let Some(subject) = &event.subject {
        out.push_str(&format!("- subject: {subject}\n"));
    }
    if let Some(snippet) = &event.snippet {
        out.push_str(&format!("- snippet: {snippet}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listeners::config::{AgentBindingFilter, AgentListenerBinding};

    fn sample_event() -> StoredEvent {
        StoredEvent {
            id: "gmail-personal:m1".to_string(),
            listener_id: "gmail-personal".to_string(),
            provider: "gmail".to_string(),
            event_type: "message.received".to_string(),
            ts: "2026-07-24T12:00:00Z".to_string(),
            from: Some("dad@family.com".to_string()),
            subject: Some("Dinner Sunday?".to_string()),
            snippet: Some("Want to come over?".to_string()),
            included: true,
        }
    }

    #[test]
    fn sender_glob_matches_leading_and_trailing_wildcard() {
        assert!(sender_glob_matches("dad@family.com", "*@family.com"));
        assert!(!sender_glob_matches("dad@work.com", "*@family.com"));
        assert!(sender_glob_matches("bob@duetto.com", "bob@*"));
        assert!(sender_glob_matches(
            "exact@example.com",
            "exact@example.com"
        ));
    }

    #[test]
    fn binding_matches_event_type_and_sender() {
        let binding = AgentListenerBinding {
            name: "gmail-personal".to_string(),
            event_types: vec!["message.received".to_string()],
            filter: AgentBindingFilter {
                from: vec!["*@family.com".to_string()],
                exclude_labels: vec![],
            },
        };
        assert!(binding_matches_event(&binding, &sample_event()));
    }

    #[test]
    fn binding_rejects_wrong_listener_name() {
        let binding = AgentListenerBinding {
            name: "calendar-personal".to_string(),
            ..Default::default()
        };
        assert!(!binding_matches_event(&binding, &sample_event()));
    }

    #[test]
    fn binding_rejects_non_matching_sender() {
        let binding = AgentListenerBinding {
            name: "gmail-personal".to_string(),
            event_types: vec![],
            filter: AgentBindingFilter {
                from: vec!["*@duetto.com".to_string()],
                exclude_labels: vec![],
            },
        };
        assert!(!binding_matches_event(&binding, &sample_event()));
    }

    #[test]
    fn binding_rejects_excluded_label() {
        // Documented no-op today (StoredEvent carries no labels) — pinned so
        // a future `labels` field addition has a test to update, not a
        // silent behavior change.
        let binding = AgentListenerBinding {
            name: "gmail-personal".to_string(),
            event_types: vec![],
            filter: AgentBindingFilter {
                from: vec![],
                exclude_labels: vec!["PROMOTIONS".to_string()],
            },
        };
        assert!(binding_matches_event(&binding, &sample_event()));
    }

    #[test]
    fn event_type_filter_empty_matches_any() {
        let binding = AgentListenerBinding {
            name: "gmail-personal".to_string(),
            event_types: vec![],
            filter: AgentBindingFilter::default(),
        };
        assert!(binding_matches_event(&binding, &sample_event()));
    }
}
