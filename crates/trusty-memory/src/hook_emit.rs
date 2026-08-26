//! Cross-process hook activity emit.
//!
//! Why: Claude Code's hook commands (`UserPromptSubmit` → `prompt-context`,
//! `SessionStart` → `inbox-check`) run as ephemeral CLI subprocesses, not
//! inside the long-lived daemon. They cannot call `state.emit` directly because
//! they hold no `AppState`. Before this module they had no way to populate the
//! activity feed, which led directly to the user complaint "the TUI activity
//! feed is always empty in a normal Claude Code session" — because in a normal
//! session the only daemon traffic is hooks, and hooks emitted nothing.
//!
//! What: [`post_hook_event`] calls the daemon's `hook_fired` method over the
//! socket, through [`crate::client`]. It used to resolve an HTTP address and
//! POST `/api/v1/activity/hook`; ADR-0032 retired that listener, and the route
//! was always a duplicate of the dispatcher method — `transport::rpc::dispatch`
//! has routed `hook_fired` since #149, and it reaches the socket through the
//! router's fallback.
//!
//! Failures stay swallowed (warn-logged to stderr) so the hook never fails
//! because of a missing or unresponsive daemon — that contract matches the
//! prompt-context handler's "always exit 0" rule. This differs deliberately
//! from `trusty-memory note`, which now exits non-zero on a dropped write: a
//! note is the caller's whole purpose, and an activity row is telemetry
//! alongside output the user is waiting for.
//!
//! Test: `post_hook_event_no_daemon_is_noop` (the no-daemon branch);
//! `hook_fired_activity_emit_smoke` (the live round trip, against a daemon on a
//! temp socket); `hook_emit_failure_isolated` in `commands::prompt_context`
//! (the hook still exits 0 when the emit fails).

use crate::{HookType, InjectionKind};
use std::path::Path;
use std::time::Duration;

/// The daemon method that ingests a hook firing.
///
/// Why a constant: this module is the only caller, and the name is what a
/// rename has to change in one place. Routed by
/// [`crate::transport::rpc::dispatch`], not by the folded-method table.
pub const HOOK_EVENT_METHOD: &str = "hook_fired";

/// Connect + total timeout for the hook emit.
///
/// Why: hooks run in front of every user prompt; the budget here must be
/// tighter than the prompt-context fetch budget so a slow daemon never adds
/// noticeable latency to the user's typing flow. 1.5 s is enough for a healthy
/// local daemon plus a wide margin, and tight enough that a hung daemon does
/// not block Claude Code by more than a moment.
const HOOK_EMIT_TIMEOUT: Duration = Duration::from_millis(1500);

/// The params sent with [`HOOK_EVENT_METHOD`].
///
/// Why: deliberately separate from `DaemonEvent` itself so the wire format can
/// evolve (add fields, rename) without breaking the consumer schema. The daemon
/// side (`transport::rpc::HookFiredParams`) maps this into the canonical
/// `DaemonEvent::HookFired` variant. Forwards-compatible: `#[serde(default)]`
/// on every optional field means a newer client can add fields without breaking
/// an older daemon.
/// What: serde-encoded as snake_case JSON.
/// Test: `hook_fired_activity_emit_smoke` round-trips it against a real daemon.
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

/// Emit a hook event to the running daemon, best-effort.
///
/// Why: the contract for every hook handler is "never block the user's prompt
/// because of a daemon problem". This function therefore swallows every error
/// path — socket unresolvable, daemon not running, refused params — and
/// warn-logs the failure to stderr so the hook command still prints whatever
/// stdout the user expected.
///
/// What: derives the socket and calls [`HOOK_EVENT_METHOD`] through
/// [`crate::client`]. Returns `()` regardless of outcome.
///
/// Test: `post_hook_event_no_daemon_is_noop`, `hook_fired_activity_emit_smoke`.
pub async fn post_hook_event(payload: HookEventPayload) {
    let params = match serde_json::to_value(&payload) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("hook_emit: encode payload failed: {e:#}");
            return;
        }
    };
    if let Err(e) =
        crate::client::call_with_timeout(HOOK_EVENT_METHOD, params, HOOK_EMIT_TIMEOUT).await
    {
        // Operators chasing missing activity rows find the reason in
        // `~/Library/Logs/trusty-memory/*.log` (or wherever the daemon routed
        // stderr) without the hook itself blowing up.
        tracing::warn!("hook_emit: {HOOK_EVENT_METHOD} failed: {e:#}");
    }
}

/// [`post_hook_event`] against an explicit socket.
///
/// Why it is public: a test drives a daemon it bound on a temp path, and a
/// fixture that had to mutate the real data directory to be testable would not
/// be a test of this function.
///
/// Test: `hook_fired_activity_emit_smoke`.
pub async fn post_hook_event_at(socket: &Path, payload: HookEventPayload) {
    let params = match serde_json::to_value(&payload) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("hook_emit: encode payload failed: {e:#}");
            return;
        }
    };
    if let Err(e) =
        crate::client::call_at(socket, HOOK_EVENT_METHOD, params, HOOK_EMIT_TIMEOUT).await
    {
        tracing::warn!("hook_emit: {HOOK_EVENT_METHOD} failed: {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> HookEventPayload {
        HookEventPayload {
            palace_id: Some("alpha".to_string()),
            palace_name: Some("alpha".to_string()),
            hook_type: HookType::UserPromptSubmit,
            injection_kind: InjectionKind::PromptContext,
            injection_length: 256,
            trigger_prompt_excerpt: "test prompt".to_string(),
            duration_ms: 12,
        }
    }

    /// Why: the hook handlers rely on this function being a no-op when no
    /// daemon is running. A panic or an error here would fail the hook and
    /// break every Claude Code prompt on a host where the daemon was never
    /// started.
    /// What: pins a tempdir as the data dir so the derived socket path exists
    /// nowhere and the dial is refused, then awaits `post_hook_event`. Must
    /// return without panicking.
    /// Test: itself.
    #[tokio::test]
    async fn post_hook_event_no_daemon_is_noop() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: test serialised by env_test_lock.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let mut payload = sample_payload();
        payload.palace_id = None;
        payload.palace_name = None;
        // Must not panic / hang.
        post_hook_event(payload).await;
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
    }

    /// Why: every hook firing must produce an activity-feed row tagged
    /// `source=hook`, so a normal Claude Code session — which triggers hooks
    /// and nothing else — stops leaving the TUI feed empty. This test existed
    /// against the retired `POST /api/v1/activity/hook` route and went with it;
    /// nothing covered the emit path end to end afterwards, which is the gap
    /// (#6286 review, finding 2). The unit tests on each side can both pass
    /// while the two disagree about the method name or the params shape.
    /// What: binds a daemon on a temp socket, emits one event through the real
    /// [`post_hook_event_at`], flushes, and reads the row back through
    /// `memory.activity` filtered to `source=hook`.
    /// Test: itself.
    #[cfg(feature = "daemon")]
    #[tokio::test]
    async fn hook_fired_activity_emit_smoke() {
        let daemon = crate::test_daemon::TestDaemon::start().await;

        post_hook_event_at(daemon.socket(), sample_payload()).await;
        // #232: `emit` fire-and-forgets the redb append on the blocking pool.
        daemon.state().flush_activity_writes().await;

        let page = crate::client::call_at(
            daemon.socket(),
            "memory.activity",
            serde_json::json!({ "source": "hook", "limit": 10 }),
            Duration::from_secs(10),
        )
        .await
        .expect("memory.activity answers");

        let entries = page["entries"].as_array().expect("entries array");
        assert!(
            !entries.is_empty(),
            "expected at least one hook activity row, got {page}"
        );
        let first = &entries[0];
        assert_eq!(first["source"], "hook");
        assert_eq!(first["event_type"], "hook_fired");
        assert_eq!(first["palace_id"], "alpha");
        assert_eq!(first["payload"]["hook_type"], "UserPromptSubmit");
        assert_eq!(first["payload"]["injection_kind"], "prompt-context");
    }

    /// Why: a hook must complete even when the emit fails, and there are two
    /// distinct ways it can. `post_hook_event_no_daemon_is_noop` covers the
    /// transport half — nothing is listening. This covers the other: a daemon
    /// that IS listening and REFUSES the call, which is what a params-shape
    /// disagreement or an internal error looks like and which no test covered
    /// after the retired route took `hook_emit_failure_isolated` with it (#6286
    /// review, finding 2). A refusal arriving as a typed error rather than a
    /// dial failure takes a different arm through this function.
    /// What: serves a socket whose every method answers an error, emits against
    /// it, and asserts the call returns rather than panicking or propagating.
    /// Test: itself.
    #[tokio::test]
    async fn hook_emit_failure_isolated() {
        use std::sync::Arc;
        use trusty_common::uds::server::{
            serve_until, RpcError, RpcFallback, RpcRouter, RpcServeOptions,
        };

        /// Refuses every method, the way a daemon that cannot accept the event
        /// does.
        struct AlwaysRefuses;

        #[async_trait::async_trait]
        impl RpcFallback for AlwaysRefuses {
            async fn call(
                &self,
                method: &str,
                _params: serde_json::Value,
            ) -> Result<serde_json::Value, RpcError> {
                Err(RpcError::internal(format!("{method} is refused")))
            }
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("refusing.sock");
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
        let router = Arc::new(RpcRouter::new().fallback(AlwaysRefuses));
        tokio::spawn(async move {
            serve_until(&listener, router, RpcServeOptions::default(), async {
                let _ = shutdown.await;
            })
            .await;
        });

        // Returns `()` — the assertion is that it returns at all, having
        // swallowed the daemon's refusal into a warn rather than propagating.
        post_hook_event_at(&socket, sample_payload()).await;

        let _ = stop.send(());
    }
}
