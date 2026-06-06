//! PM task entry points + conversational fast-path helpers.
//!
//! Why: The thin `run_pm_task` / `run_pm_task_with_session` entry points and the
//! pure conversational helpers (name extraction, profile save, persona glob
//! matching) are small and heavily unit-tested, so they're kept apart from the
//! large `run_pm_task_with_history` / `run_pm_task_with_persona` dispatch bodies.
//! What: `run_pm_task`, `run_pm_task_with_session`, `extract_name_from_input`,
//! `save_name_to_profile`, and `match_any_glob`.
//! Test: `ctrl::tests::pm_task_tests` covers `extract_name_from_input` and
//! `match_any_glob`; the dispatch entry points are exercised end-to-end.

use std::path::Path;

use anyhow::Result;

use super::super::config::SessionOverrides;
use super::dispatch::run_pm_task_with_history;

/// Entry point used by the PM actor task loop (`pm_actor_task`).
///
/// Why: Centralises the "single-shot PM round-trip" call site so the actor
/// task doesn't need to know about session ids, history, or overrides.
/// What: Delegates to `run_pm_task_with_session` with `None` session id.
/// Test: Exercised end-to-end via `actor_processes_task_and_shuts_down`.
pub(crate) async fn run_pm_task(project_path: &Path, user_input: &str) -> Result<String> {
    run_pm_task_with_session(project_path, user_input, None).await
}

/// Extract a name from a conversational name-introduction input.
///
/// Why: When the conversational fast path runs without a known user name, the
/// coordinator asks for it. The next turn from the user is typically a short
/// reply ("Bob", "I'm Bob", "My name is Alice"). This helper recognizes those
/// shapes so we can persist the name to `UserProfile` without an LLM round-trip.
/// What: Matches common introduction prefixes ("my name is ", "i'm ", "im ",
/// "i am ", "call me ", "it's ", "its "), or accepts a single bare alphabetic
/// word (2–20 chars) as a name. Returns the title-cased name on match.
/// Test: `extract_name_from_input_*` tests cover positive and negative
/// cases (greetings and task requests must NOT match).
pub(crate) fn extract_name_from_input(input: &str) -> Option<String> {
    fn title_case(word: &str) -> String {
        let mut name = word.to_string();
        if let Some(first) = name.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        name
    }
    fn looks_like_name(word: &str, min: usize, max: usize) -> bool {
        let len = word.chars().count();
        len >= min
            && len <= max
            && word
                .chars()
                .all(|c| c.is_alphabetic() || c == '-' || c == '\'')
    }

    let trimmed = input.trim();
    let lower = trimmed.to_lowercase();
    for prefix in &[
        "my name is ",
        "i'm ",
        "im ",
        "i am ",
        "call me ",
        "it's ",
        "its ",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let word = rest.split_whitespace().next()?;
            // Reject greetings disguised as introductions ("i'm here", "i'm fine").
            const STOP_WORDS: &[&str] = &[
                "here", "fine", "good", "well", "ok", "okay", "back", "ready", "sorry", "the", "a",
                "an", "not",
            ];
            if STOP_WORDS.contains(&word) {
                return None;
            }
            if looks_like_name(word, 2, 40) {
                return Some(title_case(word));
            }
            return None;
        }
    }

    // Single-word input that looks like a name.
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() == 1 {
        let word = words[0];
        // Reject common single-word non-names (greetings, thanks, etc.)
        let lw = word.to_lowercase();
        const NON_NAMES: &[&str] = &[
            "hello", "hi", "hey", "yo", "sup", "thanks", "thank", "ok", "okay", "yes", "no", "yep",
            "nope", "help", "quit", "exit", "stop", "done",
        ];
        if NON_NAMES.contains(&lw.as_str()) {
            return None;
        }
        if word.chars().all(|c| c.is_alphabetic()) && looks_like_name(word, 2, 20) {
            return Some(title_case(word));
        }
    }
    None
}

/// Persist a detected user name to `~/.trusty-agents/user.toml`.
///
/// Why: When the conversational fast path detects a name introduction it must
/// save the name immediately so the next turn's system prompt sees it. Without
/// this, the coordinator keeps re-asking ("What's your name?") in a loop.
/// What: Loads the existing profile (or starts a default one), updates the
/// name only when currently empty (don't clobber a real name with a partial
/// match), and writes the file. Failures are logged but non-fatal — the user
/// still gets a greeting, they just won't be remembered next session.
/// Test: Covered by the `extract_name_from_input_*` unit tests plus an
/// end-to-end check via `cat ~/.trusty-agents/user.toml` after running the binary.
pub(crate) fn save_name_to_profile(name: &str) {
    use crate::identity::user_profile::UserProfile;
    let mut profile = UserProfile::load().unwrap_or_default();
    if profile.name.trim().is_empty() {
        profile.name = name.to_string();
        if profile.created_at.is_empty() {
            profile.created_at = chrono::Utc::now().to_rfc3339();
        }
        match profile.save() {
            Ok(()) => tracing::info!(name = %name, "user name saved to profile"),
            Err(e) => tracing::warn!(error = %e, "failed to save user name"),
        }
    } else {
        tracing::debug!(
            existing = %profile.name,
            detected = %name,
            "profile already has a name; not overwriting"
        );
    }
}

/// Match a tool name against a list of glob patterns (#255).
///
/// Why: Persona TOMLs accept `["mcp_*", "git_log"]` so operators don't have
/// to enumerate every dynamic tool name. A purpose-built matcher avoids
/// pulling in the `glob` crate for two patterns of behavior.
/// What: Returns `true` if `name` exactly equals a pattern, OR a pattern
/// ends with `*` and `name` starts with the pattern's prefix. Empty pattern
/// list returns false (caller treats `None` as "no filter" separately).
/// Test: `match_any_glob_handles_suffix_wildcard`.
pub(crate) fn match_any_glob(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if let Some(prefix) = p.strip_suffix('*') {
            name.starts_with(prefix)
        } else {
            name == p
        }
    })
}

/// Same as `run_pm_task` but tags every emitted event with `session_id` so
/// SSE subscribers can filter to a specific UI task.
///
/// Why: The thin-CLI controller socket (`handle_socket_connection`) and any
/// other future caller can mint a uuid up-front and propagate it through to
/// every downstream emission so the UI's per-task view stays coherent. When
/// `session_id` is `None` we still emit events, just unfiltered.
/// What: Delegates to `run_pm_task_with_history` with an empty history slice
/// and default overrides.
/// Test: Exercised end-to-end via the ctrl integration tests.
pub async fn run_pm_task_with_session(
    project_path: &Path,
    user_input: &str,
    session_id: Option<String>,
) -> Result<String> {
    run_pm_task_with_history(
        project_path,
        user_input,
        &[],
        session_id,
        SessionOverrides::default(),
    )
    .await
}
