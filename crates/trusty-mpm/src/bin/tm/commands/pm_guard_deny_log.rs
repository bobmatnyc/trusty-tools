//! What every `tm hook --pm-guard` deny leaves behind: a denial record and an
//! audit POST (#8722).
//!
//! Why: a deny used to reach only the agent (the stdout JSON) and the daemon's
//! `/hooks` audit, so `list_recent_errors` and `preview_bug_report` never saw
//! one. Every deny site already called [`audit_denied_tool`]; making it take
//! the check slug and the call's [`DenyContext`] means the compiler, not a
//! review, makes a new rule record what it refused. Split out of `pm_guard.rs`,
//! which sits at the 500-SLOC cap.
//! What: [`DenyContext`] names the refused call once per hook run;
//! [`record_deny`] appends it to the denial store the bug pipeline reads;
//! [`audit_denied_tool`] records, then POSTs the audit.
//! Test: `tests/tm_hook_pm_guard_deny_capture.rs` end to end; the record shape
//! in `trusty_mpm::daemon::bug_report::denials`.

use std::path::PathBuf;

use trusty_common::error_capture::TRUSTY_NO_BUG_CAPTURE_ENV;
use trusty_mpm::daemon::bug_report::{
    DeniedCall, append_denial, denial_record, pm_guard_denials_path,
};

/// The ceiling [`audit_denied_tool`] can spend before a deny reaches stdout.
///
/// Why (#7975): named so `hook_stdin::PM_GUARD_STDIN_TIMEOUT` can be chosen
/// against it — read budget plus this must clear the registered hook timeout.
/// Test: `guard_read_budget_leaves_the_audit_post_inside_the_hook_timeout`.
pub(crate) const AUDIT_POST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Payload fields naming what a refused call would have acted on, in order.
const SUBJECT_FIELDS: &[&str] = &["command", "file_path", "notebook_path", "subagent_type"];

/// The refused tool call, as the hook read it (#8722).
#[derive(Debug, Clone)]
pub(crate) struct DenyContext<'a> {
    pub(crate) url: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) tool_name: &'a str,
    /// The first of [`SUBJECT_FIELDS`] the `tool_input` carries, else empty.
    pub(crate) command: &'a str,
    /// The payload's `cwd`, else the hook's own working directory.
    pub(crate) cwd: String,
    /// An explicit denial store — a test's temp file; `None` is the ambient one.
    pub(crate) store: Option<PathBuf>,
}

impl<'a> DenyContext<'a> {
    /// Read the call's identity out of a `PreToolUse` payload.
    ///
    /// A payload that could not be read at all is passed as `Value::Null`: every
    /// field is then empty and `cwd` is the hook's own directory.
    pub(crate) fn from_payload(url: &'a str, payload: &'a serde_json::Value) -> Self {
        let text = |v: Option<&'a serde_json::Value>| v.and_then(|v| v.as_str()).unwrap_or("");
        let input = payload.get("tool_input");
        let command = SUBJECT_FIELDS
            .iter()
            .find_map(|key| input.and_then(|i| i.get(*key)).and_then(|v| v.as_str()))
            .unwrap_or("");
        let cwd = payload
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|p| p.display().to_string())
            })
            .unwrap_or_default();
        Self {
            url,
            session_id: text(payload.get("session_id")),
            tool_name: text(payload.get("tool_name")),
            command,
            cwd,
            store: None,
        }
    }
}

/// Append one deny to the denial store; never changes the decision.
///
/// Why (#8722): the record is what makes a refusal reportable, but losing one
/// must not turn a deny into an allow — so a failure is reported on stderr and
/// the caller still prints its deny.
/// What: skipped when `TRUSTY_NO_BUG_CAPTURE` is set non-empty; otherwise
/// appends [`denial_record`] to the context's store, or the ambient one.
/// Returns whether a record was written.
/// Test: `a_deny_whose_record_cannot_be_written_still_denies_and_says_so`.
pub(crate) fn record_deny(ctx: &DenyContext<'_>, check: &str, reason: &str) -> bool {
    if std::env::var_os(TRUSTY_NO_BUG_CAPTURE_ENV).is_some_and(|v| !v.is_empty()) {
        return false;
    }
    let Some(path) = ctx.store.clone().or_else(pm_guard_denials_path) else {
        eprintln!("tm pm-guard: could not record deny `{check}`: no data directory");
        return false;
    };
    let call = DeniedCall {
        check,
        tool: ctx.tool_name,
        command: ctx.command,
        cwd: &ctx.cwd,
        session_id: ctx.session_id,
        reason,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match append_denial(&path, &denial_record(&call, now)) {
        Ok(()) => true,
        Err(err) => {
            eprintln!(
                "tm pm-guard: could not record deny `{check}` to {}: {err}",
                path.display()
            );
            false
        }
    }
}

/// Record a denied tool call, then POST its best-effort audit.
///
/// Why: the one call every deny site makes before printing its deny, so a new
/// rule cannot print a deny that leaves no record (#8722).
/// What: [`record_deny`], then POSTs `{session_id, event:"PreToolUse",
/// payload:{cwd, tool, pm_guard_decision:"deny", pm_guard_reason}}` to
/// `<url>/hooks` under [`AUDIT_POST_TIMEOUT`], dropping any error.
/// Test: `pm_guard_records_every_deny_where_list_recent_errors_reads`.
pub(crate) async fn audit_denied_tool(ctx: &DenyContext<'_>, check: &str, reason: &str) {
    record_deny(ctx, check, reason);
    post_deny_audit(ctx, reason).await;
}

/// The audit POST half of [`audit_denied_tool`], for a caller that records first.
pub(crate) async fn post_deny_audit(ctx: &DenyContext<'_>, reason: &str) {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_default();
    let body = serde_json::json!({
        "session_id": ctx.session_id,
        "event": "PreToolUse",
        "payload": {
            "cwd": cwd,
            "tool": ctx.tool_name,
            "pm_guard_decision": "deny",
            "pm_guard_reason": reason,
        }
    });
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(AUDIT_POST_TIMEOUT)
        .build()
    else {
        return;
    };
    let _ = client
        .post(format!("{}/hooks", ctx.url))
        .json(&body)
        .send()
        .await;
}
