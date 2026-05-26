//! Cross-process hook activity emit.
//!
//! Why: Claude Code's hook commands (`UserPromptSubmit` → `prompt-context`,
//! `SessionStart` → `inbox-check`) run as ephemeral CLI subprocesses, not
//! inside the long-lived daemon. They cannot call `state.emit` directly
//! because they hold no `AppState`. Prior to this module they had no way
//! to populate the activity feed, which led directly to the user
//! complaint "the TUI activity feed is always empty in a normal Claude
//! Code session" — because in a normal session the only daemon traffic
//! is hooks, and hooks emitted nothing.
//!
//! What: this module exposes [`post_hook_event`] — a best-effort async
//! helper that resolves the running daemon's HTTP address via
//! `trusty_common::read_daemon_addr` and POSTs the hook payload to
//! `POST /api/v1/activity/hook`. Failures are swallowed (warn-logged to
//! stderr) so the hook never fails because of a missing or unresponsive
//! daemon — that contract matches the prompt-context handler's "always
//! exit 0" rule. The receiving daemon side lives in `web.rs` and
//! forwards the payload to `state.emit(DaemonEvent::HookFired { … })`.
//!
//! Test: `post_hook_event_no_daemon_is_noop` (the no-daemon branch);
//! the live-daemon round trip is covered in the prompt-context /
//! inbox-check integration tests.

use crate::{HookType, InjectionKind};
use std::time::Duration;

/// HTTP path for the hook ingestion endpoint.
///
/// Why: kept as a constant so tests can target it without copy-pasting
/// the string. Mounted under `/api/v1/activity/hook` so it sits next to
/// the existing `GET /api/v1/activity` history endpoint (#96).
pub const HOOK_EVENT_PATH: &str = "/api/v1/activity/hook";

/// Connect + total timeout for the hook emit POST.
///
/// Why: hooks run in front of every user prompt; the budget here must be
/// tighter than the prompt-context fetch budget so a slow daemon never
/// adds noticeable latency to the user's typing flow. 1.5 s is enough
/// for a healthy local daemon plus a wide margin and tight enough that
/// a hung daemon doesn't block Claude Code by more than a moment.
const HOOK_EMIT_TIMEOUT: Duration = Duration::from_millis(1500);

/// JSON payload posted to `POST /api/v1/activity/hook`.
///
/// Why: deliberately separate from `DaemonEvent` itself so we can evolve
/// the wire format (add fields, rename) without breaking the SSE consumer
/// schema. The daemon-side handler maps this into the canonical
/// `DaemonEvent::HookFired` variant. Forwards-compatible: serde
/// `#[serde(default)]` on every optional field means a future client can
/// add fields without breaking older daemons.
/// What: serde-encoded as snake_case JSON.
/// Test: round-trip exercised by `post_hook_event_no_daemon_is_noop` (the
/// payload encode is the only thing that runs).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HookEventPayload {
    #[serde(default)]
    pub palace_id: Option<String>,
    #[serde(default)]
    pub palace_name: Option<String>,
    pub hook_type: HookType,
    pub injection_kind: InjectionKind,
    #[serde(default)]
    pub injection_length: u64,
    #[serde(default)]
    pub trigger_prompt_excerpt: String,
    #[serde(default)]
    pub duration_ms: u64,
}

/// Post a hook event to the running daemon, best-effort.
///
/// Why: the contract for every hook handler is "never block the user's
/// prompt because of a daemon problem". This function therefore swallows
/// every error path — no daemon address discovered, HTTP client build
/// error, POST send error, non-2xx response — and warn-logs the failure
/// to stderr so the hook command itself continues to print whatever
/// stdout the user expected.
///
/// What: resolves the daemon address via
/// `trusty_common::read_daemon_addr("trusty-memory")`, builds a short-
/// timeout `reqwest::Client`, POSTs the payload as JSON. Returns `()`
/// regardless of outcome.
///
/// Test: `post_hook_event_no_daemon_is_noop` confirms the no-daemon
/// branch is a no-op; the live-daemon path is exercised by
/// `hook_fired_activity_emit_smoke` in `commands::prompt_context`.
pub async fn post_hook_event(payload: HookEventPayload) {
    // 1. Discover the daemon address. Missing lockfile / discovery error =
    //    daemon is not running. The activity feed is best-effort — silently
    //    return.
    let addr = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(a)) => a,
        Ok(None) => return,
        Err(_) => return,
    };
    let base = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{addr}")
    };
    let url = format!("{base}{HOOK_EVENT_PATH}");

    // 2. Build a tightly-bounded HTTP client. A client-build failure is
    //    a programmer-class problem (no realistic runtime trigger) but we
    //    still degrade rather than panic — the hook must not fail.
    let client = match reqwest::Client::builder()
        .timeout(HOOK_EMIT_TIMEOUT)
        .connect_timeout(HOOK_EMIT_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("hook_emit: build client failed: {e:#}");
            return;
        }
    };

    // 3. Fire and forget. Any error is swallowed with a stderr warn so
    //    operators chasing missing activity rows can find the failure
    //    in `~/Library/Logs/trusty-memory/*.log` (or wherever the daemon
    //    routed stderr) without the hook itself blowing up.
    match client.post(&url).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            tracing::warn!("hook_emit: daemon returned {} for {url}", resp.status());
        }
        Err(e) => {
            tracing::warn!("hook_emit: POST {url} failed: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the hook handlers rely on this function being a no-op when
    /// no daemon is running. A panic / error here would fail the hook
    /// and break every Claude Code prompt on a host where the daemon
    /// was never started.
    /// What: pins a tempdir as the data dir so `read_daemon_addr`
    /// returns `Ok(None)`, then awaits `post_hook_event`. Must return
    /// without panicking.
    /// Test: itself.
    #[tokio::test]
    async fn post_hook_event_no_daemon_is_noop() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: test serialised by env_test_lock.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let payload = HookEventPayload {
            palace_id: None,
            palace_name: None,
            hook_type: HookType::UserPromptSubmit,
            injection_kind: InjectionKind::PromptContext,
            injection_length: 0,
            trigger_prompt_excerpt: String::new(),
            duration_ms: 1,
        };
        // Must not panic / hang.
        post_hook_event(payload).await;
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
    }
}
