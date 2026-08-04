//! Tests for the #3766 conversational fast-path local-failure recovery policy.
//!
//! Live symptom: with `local_inference.enabled = false` (Bob's 2026-07-24
//! "we should not be using ollama for anything right now" directive),
//! `local_qualifies` is false on EVERY turn, so `effective_model` falls back to
//! `pm_cfg.agent.model` — which for ctrl was itself `ollama/qwen3:30b`. The old
//! arm was gated `local_qualifies && fallback_on_error`, so it never fired and
//! the raw transport error reached the user as their assistant's reply:
//!
//! ```text
//! conversational handler failed: raw chat completion POST failed: error
//! sending request for url (http://localhost:11434/v1/chat/completions):
//! client error (Connect): tcp connect error: Connection refused (os error 61)
//! ```
//!
//! The fix gates recovery on the FAILURE MODE (a transport-level failure of a
//! model that resolved to a LOCAL adapter), never on a local-inference
//! preference — so no setting can switch error recovery off.

use super::{LocalFailureAction, is_local_model, local_failure_action};

const HOST: &str = "http://localhost:11434";
/// The call failed at the transport level (`Connection refused`).
const TRANSPORT: bool = true;

#[test]
fn local_model_detects_ollama_prefix() {
    assert!(is_local_model("ollama/qwen3:30b"));
    assert!(!is_local_model("claude-sonnet-4-6"));
    assert!(!is_local_model("anthropic/claude-sonnet-4-6"));
    assert!(!is_local_model(
        "bedrock/us.anthropic.claude-3-5-haiku-20241022-v1:0"
    ));
}

/// The owner's live path: local inference DISABLED, so `local_qualifies` was
/// false and `effective_model` fell through to ctrl's own local slug. Recovery
/// must happen anyway — this is the case that regressed.
#[test]
fn owner_path_local_inference_disabled_recovers() {
    let action = local_failure_action("ollama/qwen3:30b", "ollama/qwen3:30b", TRANSPORT, HOST);
    let LocalFailureAction::Explain(msg) = action else {
        panic!("expected Explain, got {action:?}");
    };
    assert!(
        msg.contains("ollama/qwen3:30b"),
        "must name the model: {msg}"
    );
    assert!(msg.contains(HOST), "must name the unreachable host: {msg}");
    assert!(msg.contains("ollama serve"), "must name the remedy: {msg}");
    assert!(
        msg.contains("[agent] model"),
        "must name the config key to change: {msg}"
    );
    assert!(
        !msg.contains("Connection refused") && !msg.contains("os error"),
        "must not leak transport-error text to the user: {msg}"
    );
}

/// Both gate states crossed with a local transport failure must recover. The
/// old code recovered in neither reliably: `enabled=false` skipped the arm
/// entirely, and `enabled=true` retried the same local slug. Recovery is now
/// structurally independent of the gate — the preference is not even an input.
#[test]
fn recovery_is_independent_of_local_inference_gate_state() {
    // `enabled = false` (the owner's config): `local_qualifies` is false, so
    // `effective_model` falls through to the agent's OWN model — local here.
    let disabled = local_failure_action("ollama/qwen3:30b", "ollama/qwen3:30b", TRANSPORT, HOST);
    // `enabled = true`: `effective_model` is `local_inference.model`, while the
    // agent's configured model stays remote.
    let enabled = local_failure_action("ollama/qwen3:30b", "claude-sonnet-4-6", TRANSPORT, HOST);

    // The recovery SHAPE differs (one has a remote target, one does not), but
    // neither may propagate a raw transport error to the user.
    assert!(
        matches!(disabled, LocalFailureAction::Explain(_)),
        "gate disabled must recover, got {disabled:?}"
    );
    assert!(
        matches!(enabled, LocalFailureAction::RetryRemote(_)),
        "gate enabled must recover, got {enabled:?}"
    );
    for (label, action) in [("disabled", &disabled), ("enabled", &enabled)] {
        assert_ne!(
            *action,
            LocalFailureAction::Propagate,
            "gate state {label} must never propagate a raw transport error"
        );
    }
}

/// Whenever a fallback IS taken, its target must be a genuinely DIFFERENT,
/// non-local model — never the endpoint that just failed.
#[test]
fn enabled_local_route_falls_back_to_a_distinct_remote_model() {
    for (failed, configured) in [
        ("ollama/qwen3:30b", "claude-sonnet-4-6"),
        ("ollama/llama3", "anthropic/claude-sonnet-4-6"),
        (
            "ollama/qwen3:30b",
            "bedrock/us.anthropic.claude-3-5-haiku-20241022-v1:0",
        ),
    ] {
        let action = local_failure_action(failed, configured, TRANSPORT, HOST);
        let LocalFailureAction::RetryRemote(target) = action else {
            panic!("expected RetryRemote for {failed} -> {configured}, got {action:?}");
        };
        assert_ne!(target, failed, "fallback must not reuse the failed model");
        assert!(
            !is_local_model(&target),
            "fallback target must be remote, got {target}"
        );
    }
}

/// Regression: the old fallback retried `pm_cfg.agent.model` verbatim, which
/// for a locally-configured agent is the SAME slug that just failed — a retry
/// that cannot succeed by construction.
#[test]
fn local_route_will_not_retry_the_same_local_slug() {
    for configured in ["ollama/qwen3:30b", "ollama/llama3"] {
        let action = local_failure_action("ollama/qwen3:30b", configured, TRANSPORT, HOST);
        assert!(
            matches!(action, LocalFailureAction::Explain(_)),
            "must explain rather than retry a local target, got {action:?}"
        );
    }
}

/// A local model that ANSWERED with an error (429, 401, malformed request) is
/// not a transport failure — that is a real error and must not be masked by a
/// silent remote retry.
#[test]
fn non_transport_local_failure_propagates() {
    let action = local_failure_action("ollama/qwen3:30b", "claude-sonnet-4-6", false, HOST);
    assert_eq!(action, LocalFailureAction::Propagate);
}

/// With ctrl's model fixed to remote Sonnet, a genuine remote failure still
/// propagates — this fix must not swallow real errors.
#[test]
fn fixed_ctrl_config_propagates_remote_failures() {
    let action = local_failure_action("claude-sonnet-4-6", "claude-sonnet-4-6", TRANSPORT, HOST);
    assert_eq!(action, LocalFailureAction::Propagate);
}
