//! The `PreToolUse` stdout envelope every `tm hook` decision is printed in.
//!
//! Why: two shapes share one JSON envelope and one protocol rule — stdout may
//! carry exactly one object — and they are printed from four modules
//! (`pm_guard`, `divert_check`, and the rules they call). Keeping both builders
//! in one place is what stops a second, subtly different envelope appearing at
//! a new print site; splitting them out of `pm_guard.rs` is also what kept that
//! file under the 500-SLOC cap when #7172 added a rule to it.
//!
//! What: [`build_pretooluse_deny_response`] blocks a tool call;
//! [`build_pretooluse_context_response`] speaks to the agent and decides
//! nothing. Neither ever emits an explicit `allow` — see each doc for why.
//!
//! Test: the `#[cfg(test)]` suite below.

/// Build the `hookSpecificOutput.permissionDecision = "deny"` JSON body.
///
/// Why: this is the exact shape Claude Code parses on a `PreToolUse` hook's
/// `exit 0` stdout to block a tool call. Confirmed against the live Claude Code
/// hooks reference (<https://code.claude.com/docs/en/hooks>, confirmed
/// 2026-07-03): a deny is
/// `{"hookSpecificOutput":{"hookEventName":"PreToolUse",
/// "permissionDecision":"deny","permissionDecisionReason":"…"}}`, and the
/// documented ALLOW form is simply exit 0 with no JSON — which is why
/// [`crate::commands::pm_guard::pm_guard`] prints nothing on ALLOW rather than emitting a
/// `permissionDecision:"allow"` object. Mirrors
/// `hook_rewrite::build_pretooluse_rewrite_response` (same `hookSpecificOutput`
/// envelope, different decision payload).
/// What: returns the deny `serde_json::Value`; its `Display` impl is compact
/// single-line JSON, so `println!("{}", …)` satisfies the protocol's "stdout
/// must contain only the JSON object" rule.
/// Test: `build_pretooluse_deny_response_has_expected_shape`.
pub(crate) fn build_pretooluse_deny_response(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason
        }
    })
}

/// Build a `hookSpecificOutput.additionalContext` body — a message TO the agent
/// that changes no decision.
///
/// Why (#4837 review, HIGH): the agent-cost warning is addressed to the agent
/// ("wrap up and report back"), but the only channel it had was an audit POST
/// to the daemon, so no agent ever read it — the graduated warn-then-stop
/// response did not exist from the agent's side. `additionalContext` is the
/// channel that fixes it: Claude Code injects the string next to the tool
/// result, and the object carries **no** `permissionDecision`, so the call
/// still goes through the normal permission flow. Per the hooks reference
/// (<https://code.claude.com/docs/en/hooks>, confirmed 2026-08-04): "Exit code
/// 0 with no output means the hook has no decision to report, so the tool call
/// continues through the normal permission flow… staying silent doesn't approve
/// it", and `additionalContext` is listed for `PreToolUse` as appearing "next
/// to the tool result". That is precisely the property the author needed and
/// could not find: context reaches the agent, and an explicit `allow` — which
/// WOULD bypass the permission flow — is never emitted.
/// What: returns the `serde_json::Value` whose compact `Display` is the single
/// stdout object; mirrors [`build_pretooluse_deny_response`]'s envelope.
/// Test: `build_pretooluse_context_response_carries_no_decision`.
pub(crate) fn build_pretooluse_context_response(context: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "additionalContext": context
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_pretooluse_deny_response_has_expected_shape() {
        let v = build_pretooluse_deny_response("nope");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(v["hookSpecificOutput"]["permissionDecisionReason"], "nope");
        // Display is compact single-line JSON (protocol: stdout is only the object).
        assert!(!v.to_string().contains('\n'));
    }

    #[test]
    fn build_pretooluse_context_response_carries_no_decision() {
        // #4837 review HIGH: this is the channel that reaches the AGENT. The
        // absence of `permissionDecision` is the whole safety property — with
        // one, the object would approve the call and bypass the permission
        // flow, which is exactly what the author declined to do.
        let v = build_pretooluse_context_response("wrap up");
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["additionalContext"], "wrap up");
        assert!(
            v["hookSpecificOutput"].get("permissionDecision").is_none(),
            "an explicit decision here would bypass the permission flow"
        );
        assert!(!v.to_string().contains('\n'));
    }
}
