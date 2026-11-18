//! The `call` pipeline: every gate between an agent request and the daemon.
//!
//! Why: each gate is a refusal the agent cannot route around, and each one
//! fails closed (plan section 4, "Call pipeline"; spike `broker.py:79-167`).
//! What, in order: `request_id` and grant id required; a repeat answers from
//! the grant's journal (`replayed`); policy verdict and argument filter on the
//! grant's pid and bundle; grant scope (live, tool granted, window); lease
//! held by this session with this grant; daemon scoped to the grant; journal
//! room; token gate against the last redacted snapshot of that window; audit
//! `forward` (a failure refuses); the driver call; menu-bar strip of a
//! snapshot; read-back confirmation of a mutating call; journal; audit `done`.
//! A manifest idle-timeout lockout restarts the daemon; a read-only call is
//! then retried once and a mutating one is answered `driver_restarted`, not run.
//! Test: `tests/broker_spike.rs`, `tests/broker_fail_closed.rs`.

use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::audit::summarize_args;
use crate::broker::{Ctx, Inner, refusal};
use crate::driver::{DriverError, is_manifest_idle};
use crate::journal::{self, Outcome};
use crate::launcher::Launcher;
use crate::policy::{self, Verdict};
use crate::redact::redact_reply;
use crate::supervisor::READ_ONLY_TOOLS;

/// Tools that must name a token from the last redacted snapshot.
pub const TOKEN_TOOLS: [&str; 6] = [
    "click",
    "double_click",
    "right_click",
    "scroll",
    "set_value",
    "type_text",
];
/// Mutating tools confirmed by read-back.
pub const CONFIRM_TOOLS: [&str; 9] = [
    "click",
    "double_click",
    "right_click",
    "scroll",
    "set_value",
    "type_text",
    "press_key",
    "hotkey",
    "launch_app",
];
/// Delays of the re-reads after a mutating call; the driver already waits ~1 s in a click.
pub const CONFIRM_POLLS: [Duration; 2] = [Duration::ZERO, Duration::from_millis(250)];
/// Told to the agent when a mutating call is not confirmed.
pub const UNCONFIRMED_HINT: &str = "not confirmed: check with get_window_state, do not retry";

fn window_key(args: &Map<String, Value>) -> Option<(u32, u64)> {
    let pid = u32::try_from(args.get("pid")?.as_u64()?).ok()?;
    Some((pid, args.get("window_id")?.as_u64()?))
}

/// The token a call names: `element_token`, else `snapshot_id:element_index`.
fn named_token(args: &Map<String, Value>) -> Option<String> {
    if let Some(t) = args.get("element_token").and_then(Value::as_str) {
        return Some(t.to_owned());
    }
    let snap = args.get("snapshot_id")?;
    let index = args.get("element_index")?;
    let text = |v: &Value| v.as_str().map_or_else(|| v.to_string(), str::to_owned);
    Some(format!("{}:{}", text(snap), text(index)))
}

impl<L: Launcher> Inner<L> {
    /// Run one `call` request for `ctx`.
    ///
    /// Postcondition: the driver saw the call only if every gate passed and
    /// the `forward` line was written; a reply with `ok: true` and a
    /// `confirmation` says `confirmed: true` only on a changed read-back
    /// fingerprint or a driver `effect: confirmed`.
    /// Test: `broker_click_and_type_drive_fixture`, `every_gate_refusal_never_reaches_driver`.
    pub(crate) fn call(
        &mut self,
        req: &Map<String, Value>,
        mut rec: Map<String, Value>,
        ctx: &Ctx,
        now: f64,
        started: Instant,
    ) -> Value {
        let tool = req.get("tool").cloned().unwrap_or(Value::Null);
        rec.insert("tool".into(), tool.clone());
        let rid = match journal::request_id(&Value::Object(req.clone())) {
            Ok(id) => id.to_owned(),
            Err(e) => return self.refuse(rec, e.code(), "every call needs a request_id"),
        };
        let Some(grant_id) = req.get("grant_id").and_then(Value::as_str) else {
            return self.refuse(rec, "grant_required", "a call needs a grant_id");
        };
        rec.insert("grant".into(), json!(grant_id));
        if let Some(entry) = self.journals.get(grant_id).and_then(|j| j.replay(&rid)) {
            // A retry after a lost ack is answered from the journal, never re-run.
            let reply = entry.reply.clone();
            rec.insert("replayed".into(), json!(true));
            return self.audited(rec, reply);
        }
        let Some(tool) = tool.as_str().map(str::to_owned) else {
            return self.refuse(rec, "bad_request", "tool must be a string");
        };
        let args = req.get("args").cloned().unwrap_or_else(|| json!({}));
        let Some(args_map) = args.as_object().cloned() else {
            return self.refuse(rec, "bad_args", "args must be an object");
        };
        rec.insert("args".into(), summarize_args(&args_map));
        let grant = match self.grants.live(grant_id, now) {
            Ok(g) => g.clone(),
            Err(e) => return self.refuse(rec, e.code(), &e.to_string()),
        };
        // 1. Policy verdict and argument filter, on the grant's app.
        let rule = match policy::check_call(&tool, &args, &grant.policy_scope()) {
            Ok(rule) => rule,
            Err(e) => return self.refuse(rec, e.code, &e.detail),
        };
        let verdict = match rule.verdict {
            Verdict::Allow => "ALLOW",
            Verdict::Filter => "FILTER",
            Verdict::Deny => return self.refuse(rec, "denied_tool", &tool),
        };
        rec.insert("verdict".into(), json!(verdict));
        // 2. Grant scope, per app.
        if !grant.tools.contains(&tool) {
            return self.refuse(rec, "tool_not_granted", &tool);
        }
        if args_map
            .get("window_id")
            .is_some_and(|w| w.as_u64() != Some(grant.scope.window_id))
        {
            return self.refuse(
                rec,
                "window_out_of_scope",
                "window_id is not the granted window",
            );
        }
        // 3. Lease, then the daemon's scope.
        if let Err(e) = self.lease.check(&ctx.session, grant_id, now) {
            return self.refuse(rec, e.code(), &e.to_string());
        }
        if let Err(e) = self.ensure_scope(grant.scope) {
            return self.refuse(rec, "rescope_failed", &e);
        }
        let journal = self.journals.entry(grant_id.to_owned()).or_default();
        if let Err(e) = journal.reserve(&rid) {
            return self.refuse(rec, e.code(), &e.to_string());
        }
        // 4. Token gate against the last redacted snapshot of this window.
        let key = window_key(&args_map);
        if TOKEN_TOOLS.contains(&tool.as_str()) {
            let snap = key.and_then(|k| self.snaps.get(&k));
            let detail = match (snap, named_token(&args_map)) {
                (None, _) => Some("take get_window_state of this pid and window_id first"),
                (Some((tokens, _)), Some(t)) if tokens.contains(&t) => None,
                (Some(_), _) => Some("token is not in the last redacted snapshot of this window"),
            };
            if let Some(detail) = detail {
                return self.refuse(rec, "token_not_exposed", detail);
            }
        }
        // 5. Audit `forward`; a failure refuses before the daemon is called.
        let mut forward = rec.clone();
        forward.insert("phase".into(), json!("forward"));
        if let Err(e) = self.audit.write(&forward) {
            return refusal("audit_failed", &e.to_string());
        }
        self.grants.touch(grant_id, now);
        // 6. The driver call, then strip, confirm, journal, audit `done`.
        let (mut reply, outcome) = self.drive(&tool, &args, grant.scope);
        if reply["ok"] == json!(true) {
            if tool == "get_window_state" {
                let red = redact_reply(&reply["result"]);
                rec.insert("redacted".into(), json!(red.removed));
                reply["result"] = red.result;
                if let Some(k) = key {
                    self.snaps.insert(k, (red.tokens, red.fingerprint));
                }
            } else if CONFIRM_TOOLS.contains(&tool.as_str()) {
                let confirmation = self.confirm(key, &reply["result"]);
                let confirmed = confirmation["confirmed"] == json!(true);
                rec.insert("confirmed".into(), json!(confirmed));
                reply["confirmation"] = confirmation;
                if !confirmed {
                    reply["hint"] = json!(UNCONFIRMED_HINT);
                }
            }
        }
        let journal = self.journals.entry(grant_id.to_owned()).or_default();
        if let Err(e) = journal.record(&rid, reply.clone(), outcome) {
            reply = json!({"ok": false, "code": e.code(), "error": e.to_string(), "result": reply});
        }
        rec.insert("phase".into(), json!("done"));
        rec.insert("ok".into(), reply["ok"].clone());
        rec.insert("ms".into(), json!(started.elapsed().as_millis() as u64));
        self.audited(rec, reply)
    }

    /// The daemon call and its error mapping.
    ///
    /// Why: no driver failure may read as success, and a mutating call must
    /// never run twice.
    /// What: `isError` -> `driver_refused`; a mutating call cut by an ended
    /// session -> `outcome_unknown` (journaled as such); the manifest
    /// idle-timeout lockout -> restart, then a read-only retry once or
    /// `driver_restarted` for a mutating call (the daemon refused it before
    /// running it); any other failure -> `driver_error`.
    /// Test: `tool_error_is_driver_refused`, `mutating_call_cut_by_session_end_is_outcome_unknown`,
    /// `idle_timeout_lockout_restarts_and_retries_read_only`,
    /// `idle_timeout_lockout_does_not_rerun_mutating_call`.
    fn drive(
        &mut self,
        tool: &str,
        args: &Value,
        scope: crate::manifest::LeaseScope,
    ) -> (Value, Outcome) {
        let mut result = self.supervisor.call(tool, args);
        if result.as_ref().err().is_some_and(is_manifest_idle) {
            self.snaps.clear();
            self.scope_ready = false;
            if let Err(e) = self.supervisor.restart(scope) {
                return (
                    refusal("driver_error", &format!("restart after idle lockout: {e}")),
                    Outcome::Known,
                );
            }
            self.scope_ready = true;
            if !READ_ONLY_TOOLS.contains(&tool) {
                let detail = "the daemon was locked out by its idle timeout and restarted; \
                              the call did not run; take get_window_state, then retry with a new request_id";
                return (refusal("driver_restarted", detail), Outcome::Known);
            }
            result = self.supervisor.call(tool, args);
        }
        match result {
            Ok(result) => (json!({"ok": true, "result": result}), Outcome::Known),
            Err(DriverError::ToolError { message, result }) => (
                json!({"ok": false, "code": "driver_refused", "error": message, "result": result}),
                Outcome::Known,
            ),
            Err(e @ DriverError::SessionRestarted { .. }) => {
                (refusal("outcome_unknown", &e.to_string()), Outcome::Unknown)
            }
            Err(e) => (refusal("driver_error", &e.to_string()), Outcome::Known),
        }
    }

    /// Confirm a mutating call by re-reading its window.
    ///
    /// Why: the driver's `effect: unverifiable` is not evidence the call worked
    /// (GO-STOP.md:76-87).
    /// What: re-snapshots at each [`CONFIRM_POLLS`] delay until the redacted
    /// fingerprint differs from the pre-call one. A read-back failure, no
    /// pre-call snapshot or no window in the args is `confirmed: false`,
    /// whatever the driver's `effect`.
    /// Postcondition: `confirmed` is true only when the fingerprint changed
    /// from a known one, or the driver itself said `effect: confirmed`.
    /// Test: `broker_click_confirmation_by_readback_button`,
    /// `broker_click_confirmation_by_readback_inert`, `unverifiable_effect_is_not_confirmed`,
    /// `readback_failure_is_not_confirmed`.
    fn confirm(&mut self, key: Option<(u32, u64)>, result: &Value) -> Value {
        let effect = result
            .pointer("/structuredContent/effect")
            .cloned()
            .unwrap_or(Value::Null);
        let by_driver = effect == json!("confirmed");
        let Some((pid, window)) = key else {
            return json!({"confirmed": false, "driver_effect": effect, "method": "state_readback",
                "state_changed": false, "readback_error": "no pid and window_id to read back"});
        };
        let before = self
            .snaps
            .get(&(pid, window))
            .map(|s| s.1.clone())
            .unwrap_or_default();
        let mut after = before.clone();
        for delay in CONFIRM_POLLS {
            std::thread::sleep(delay);
            let snap = self.supervisor.call(
                "get_window_state",
                &json!({"pid": pid, "window_id": window, "include_screenshot": false}),
            );
            let snap = match snap {
                Ok(s) => s,
                Err(e) => {
                    // The call already ran; it is reported unconfirmed, never as failed.
                    self.snaps.remove(&(pid, window));
                    return json!({"confirmed": false, "driver_effect": effect, "method": "state_readback",
                        "state_changed": false, "readback_error": e.to_string()});
                }
            };
            let red = redact_reply(&snap);
            after = red.fingerprint.clone();
            self.snaps
                .insert((pid, window), (red.tokens, red.fingerprint));
            if after != before {
                break;
            }
        }
        let changed = !before.is_empty() && !after.is_empty() && after != before;
        json!({"confirmed": changed || by_driver, "driver_effect": effect,
            "method": "state_readback", "state_changed": changed})
    }
}
