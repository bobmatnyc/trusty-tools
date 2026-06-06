//! Local Ollama fast-path for qualifying ctrl turns (#319).
//!
//! Why: Frequent ctrl turns (TM status, project lists, simple chat) don't
//! benefit from a remote LLM round-trip. Routing them to a locally-running
//! ollama instance gives sub-second feedback and zero token cost when ollama
//! is available. The decision must be made cheaply (no network on the hot
//! path) and must not silently degrade behavior when ollama is missing.
//! What: Two pure functions plus a cached availability probe:
//!   - `is_ollama_available(host)` — short-timeout health check.
//!   - `is_ollama_available_cached(host)` — once-per-process memoized probe.
//!   - `qualifies_for_local_inference(intent, message)` — routing predicate.
//! Test: `tests::*` cover the predicate matrix; the probe is exercised
//! manually via the `/local test` REPL command.

use std::sync::OnceLock;
use std::time::Duration;

use crate::intent::IntentClass;

/// Cached result of the per-process ollama probe.
///
/// Why: `is_ollama_available` opens a TCP connection and waits for a 200 from
/// `/api/tags` — fine on startup but unacceptable on every turn. We probe
/// once and remember the answer for the rest of the process; users who start
/// ollama mid-session can re-probe via `/local test`.
/// What: `OnceLock<bool>` initialized lazily by `is_ollama_available_cached`.
static OLLAMA_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Probe an ollama server's `/api/tags` endpoint.
///
/// Why: Health-check before routing local — if ollama isn't running we want a
/// clean fall-through to the remote model instead of a 500 on the next turn.
/// 500 ms timeout keeps the hot path responsive when the user has nothing
/// listening on the port.
/// What: GETs `<host>/api/tags`, returns `true` on a 2xx response. Any error
/// (timeout, connection refused, non-2xx) returns `false` without panicking.
/// Test: `is_ollama_available_returns_false_for_bad_host`.
pub async fn is_ollama_available(host: &str) -> bool {
    let url = format!("{}/api/tags", host.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Memoized variant of `is_ollama_available`.
///
/// Why: The hot path (every ctrl turn) cannot afford a 500 ms TCP probe.
/// Caching the first result means subsequent turns pay nothing. When the
/// probe fails, logs a warn! so the user knows local inference is enabled
/// but unreachable — graceful degradation via `fallback_on_error`.
/// What: On first call, awaits `is_ollama_available(host)` and stores the
/// result in `OLLAMA_AVAILABLE`. Every subsequent call returns the cached bool.
/// Test: Manual via `/local` (changes to ollama state after process start are
/// not reflected — by design; user can restart trusty-agents to re-probe).
pub async fn is_ollama_available_cached(host: &str) -> bool {
    if let Some(b) = OLLAMA_AVAILABLE.get() {
        return *b;
    }
    let result = is_ollama_available(host).await;
    if !result {
        tracing::warn!(
            host = %host,
            "Local inference enabled but Ollama not reachable at {} — will use remote fallback",
            host
        );
    }
    // First writer wins; concurrent probes both compute but only one stores.
    let _ = OLLAMA_AVAILABLE.set(result);
    result
}

/// Force re-probe of ollama availability (used by `/local test`).
///
/// Why: After the user starts ollama mid-session, `/local test` should pick
/// up the new state without restarting trusty-agents. Since `OnceLock` doesn't
/// expose a setter once initialized, we expose a separate non-cached probe
/// for the test command. The cached value remains whatever the first probe
/// observed — `/local test` is purely diagnostic.
/// What: Just delegates to `is_ollama_available`. Kept as a named function
/// so callers can grep for the explicit "I want a fresh probe" intent.
/// Test: Same as `is_ollama_available`.
pub async fn probe_ollama_now(host: &str) -> bool {
    is_ollama_available(host).await
}

/// TM-related substring patterns that signal a routing-eligible query.
///
/// Why: TM (tmux session manager) data is local — there's no point paying a
/// remote LLM round-trip to answer "what sessions are running?". The token
/// list captures the high-frequency phrasings; matching is case-insensitive
/// substring so we catch grammatical variants without a regex.
/// What: Lowercased tokens; if any appear as substrings in the user message
/// the query is treated as TM-related.
const TM_PATTERNS: &[&str] = &[
    "session",
    "sessions",
    "tmux",
    "project",
    "projects",
    "what's running",
    "what is running",
    "pause",
    "resume",
    "list",
    "status",
    "attach",
    "monitor",
    "harness",
];

/// Decide whether a classified intent qualifies for the local fast-path.
///
/// Why: Local models (qwen3:30b et al.) are great for short conversational
/// turns and tool-only TM queries — they're poor at multi-step implementation
/// reasoning. Gating per-intent keeps the user experience consistent: real
/// work always reaches the remote model.
/// What:
///   - `Conversational` → always local (chat is exactly what local excels at).
///   - `Research` → local only when the message smells like a TM query;
///     general research stays remote.
///   - `Implementation` → never local (full reasoning needed remotely).
/// Test: `tests::qualifies_*` covers the matrix.
pub fn qualifies_for_local_inference(intent: &IntentClass, message: &str) -> bool {
    let lower = message.to_lowercase();
    let is_tm_query = TM_PATTERNS.iter().any(|p| lower.contains(p));
    match intent {
        IntentClass::Conversational => true,
        IntentClass::Research => is_tm_query,
        IntentClass::Implementation => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualifies_conversational_always_true() {
        assert!(qualifies_for_local_inference(
            &IntentClass::Conversational,
            "hello"
        ));
        assert!(qualifies_for_local_inference(
            &IntentClass::Conversational,
            "what can you help me with?"
        ));
    }

    #[test]
    fn qualifies_research_only_for_tm_queries() {
        assert!(qualifies_for_local_inference(
            &IntentClass::Research,
            "what sessions are running?"
        ));
        assert!(qualifies_for_local_inference(
            &IntentClass::Research,
            "list my projects"
        ));
        assert!(!qualifies_for_local_inference(
            &IntentClass::Research,
            "explain how OAuth works"
        ));
        assert!(!qualifies_for_local_inference(
            &IntentClass::Research,
            "research best databases for time-series"
        ));
    }

    #[test]
    fn qualifies_implementation_always_false() {
        assert!(!qualifies_for_local_inference(
            &IntentClass::Implementation,
            "write a python script"
        ));
        assert!(!qualifies_for_local_inference(
            &IntentClass::Implementation,
            "list all the sessions and write a report"
        ));
    }

    #[tokio::test]
    async fn is_ollama_available_returns_false_for_bad_host() {
        // 1.2.3.4:1 is guaranteed unreachable; the 500 ms timeout keeps this
        // test fast even when the system network stack is slow to fail.
        let ok = is_ollama_available("http://1.2.3.4:1").await;
        assert!(!ok);
    }
}
