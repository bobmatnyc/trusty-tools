//! The resume verify pass: re-read what a resume relies on (#9390).
//!
//! Why: two of 13 relaunches on 2026-10-07 resumed from snapshots whose PR
//! facts were wrong within 35 minutes. PR C made a pause leave a
//! `ws:<session>/resume` slot holding identifiers and re-read commands; this
//! pass runs those commands when a catch-up resolves a snapshot, so the resume
//! starts from live values and is told which recorded ones are stale.
//! What: [`resume_verify`] reads the slot of the session that owns the resolved
//! snapshot over the memory socket, runs each `verify` line that
//! [`AllowedCommand::parse`] accepts through the injected [`VerifyRunner`] in
//! the project directory, and reports per command what was claimed, what came
//! back and a verdict: `verified`, `stale`, or `unverified:<reason>`. It only
//! reports: it never edits a slot, never retires a fact, and runs nothing the
//! allowlist refuses. Every failure is `unverified`, never `verified`, and never
//! fails the catch-up. Each command is bounded by
//! [`ResumeVerifyHost::command_timeout`] and the whole pass by
//! [`ResumeVerifyHost::pass_timeout`].
//! Test: `resume_verify_tests.rs`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::memory_checked::{self, protocol_mismatch};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use trusty_common::catchup::session_log;
use trusty_common::memory_rpc::MemoryRpcError;

use crate::core::memory_forget::{FACT_KEY_LIST_LIMIT, LIST_METHOD, match_fact_key_drawer};
use crate::core::memory_verbs::{MemoryVerbOptions, resolve_palace_for};
use crate::daemon::catchup_superseded::{recorded_branch, recorded_commit};
use crate::daemon::pause_resume_slot::slot_key;
use crate::daemon::resume_verify_runner::{RunOutcome, SystemRunner, VerifyRunner};
use crate::daemon::verify_allowlist::{AllowedCommand, MAX_PRS};

/// The most `verify` lines one pass looks at: what a pause can write.
pub(crate) const MAX_LINES: usize = MAX_PRS + 1;

/// The default bound on one command. `gh pr view` is one API call.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(4);

/// The default bound on the whole pass, slot read included.
const PASS_TIMEOUT: Duration = Duration::from_secs(10);

/// The bound on the slot read, within the pass's remaining budget.
const SLOT_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// How long past the deadline the outer guard waits; the inner bounds stop
/// first, so the guard only fires on a step that ignored them.
const GUARD_GRACE: Duration = Duration::from_millis(250);

/// How many hex digits of a refused line's SHA-256 the report carries.
const LINE_HASH_HEX: usize = 12;

/// What the verify pass needs beyond the catch-up's own arguments.
///
/// Why: the runner is the process boundary, injected so no test spawns `gh`;
/// the bounds are fields so a test can prove a timeout without waiting seconds.
#[derive(Clone)]
pub(crate) struct ResumeVerifyHost {
    /// Runs each allowlisted argv.
    pub(crate) runner: Arc<dyn VerifyRunner>,
    /// A palace named explicitly; `None` resolves it from the project, as the
    /// pause's slot write does.
    pub(crate) palace: Option<String>,
    /// The bound on one command.
    pub(crate) command_timeout: Duration,
    /// The bound on the whole pass.
    pub(crate) pass_timeout: Duration,
}

impl ResumeVerifyHost {
    /// Real `gh` and `git`, the project's own palace, the default bounds.
    pub(crate) fn system() -> Self {
        Self {
            runner: Arc::new(SystemRunner::default()),
            palace: None,
            command_timeout: COMMAND_TIMEOUT,
            pass_timeout: PASS_TIMEOUT,
        }
    }
}

/// The `resume_verify` field of the catch-up response.
///
/// What: `null` when no snapshot resolved. Otherwise an object with
/// `fact_key` (the slot read, or `null` when none could be named), `status`
/// (`ran`, or `unverified:<reason>` when no command could be read: one of
/// `no_session_owner`, `invalid_session_id`, `no_palace`,
/// `memory_unreachable`, `memory_protocol`, `memory_error`, `slot_unresolved`,
/// `no_verify_command`, `pass_timeout`), `detail` on an unverified status, and
/// `checks`: one entry per `verify` line with `command`, `claimed`, `returned`
/// and `verdict`, for at most [`MAX_LINES`] lines, then one `over_line_cap`
/// summary counting the `skipped` rest. A refused line's entry has a `null`
/// `command` and carries only `line_len` and `line_sha256`, never its text.
/// Test: `a_resume_reads_the_live_values_and_reports_the_stale_ones`,
/// `an_unreachable_memory_daemon_is_unverified_and_catchup_still_answers`,
/// `the_whole_pass_is_bounded`,
/// `the_checks_list_stops_at_the_line_cap_with_one_summary`,
/// `a_refused_line_reports_its_length_and_hash_never_its_text`.
pub(crate) async fn resume_verify(
    host: &ResumeVerifyHost,
    socket: &Path,
    project_dir: &Path,
    snapshot: Option<&Path>,
) -> Value {
    let Some(snapshot) = snapshot else {
        return Value::Null;
    };
    let deadline = Instant::now() + host.pass_timeout;
    let pass = verify_pass(host, socket, project_dir, snapshot, deadline);
    match tokio::time::timeout(host.pass_timeout + GUARD_GRACE, pass).await {
        Ok(report) => report,
        Err(_) => unverified(None, "pass_timeout", "the verify pass outlived its bound"),
    }
}

/// The pass itself; see [`resume_verify`].
async fn verify_pass(
    host: &ResumeVerifyHost,
    socket: &Path,
    project_dir: &Path,
    snapshot: &Path,
    deadline: Instant,
) -> Value {
    let Some(owner) = snapshot_owner(project_dir, snapshot) else {
        return unverified(
            None,
            "no_session_owner",
            "no session is recorded as its writer",
        );
    };
    let Some(fact_key) = slot_key(&owner) else {
        return unverified(None, "invalid_session_id", &owner);
    };
    let verify = match read_slot_verify(host, socket, project_dir, &fact_key, deadline).await {
        Ok(Some(verify)) => verify,
        Ok(None) => return unverified(Some(&fact_key), "no_verify_command", "the slot has none"),
        Err((reason, detail)) => return unverified(Some(&fact_key), reason, &detail),
    };
    let markdown = std::fs::read_to_string(snapshot).unwrap_or_default();
    let claims = Claims {
        branch: recorded_branch(&markdown),
        commit: recorded_commit(&markdown),
    };
    let mut checks = Vec::new();
    let mut lines = verify.split('\n');
    // #9390: the slot is writer-controlled; past the cap one summary, then stop.
    for line in lines.by_ref().take(MAX_LINES) {
        let check = match AllowedCommand::parse(line) {
            Some(command) => run_check(host, &command, project_dir, &claims, deadline).await,
            None => refused(line, "unverified:not_allowlisted"),
        };
        checks.push(check);
    }
    let skipped = lines.count();
    if skipped > 0 {
        checks.push(json!({
            "command": null,
            "claimed": null,
            "returned": null,
            "verdict": "unverified:over_line_cap",
            "skipped": skipped,
        }));
    }
    json!({ "fact_key": fact_key, "status": "ran", "checks": checks })
}

/// The session that wrote `snapshot`: its `pause` line in the session log, or
/// failing that the `<session-id>/` directory it sits in.
fn snapshot_owner(project_dir: &Path, snapshot: &Path) -> Option<String> {
    let sessions_dir = project_dir.join(".trusty-mpm").join("sessions");
    let relative = snapshot.strip_prefix(&sessions_dir).ok()?;
    let recorded = relative.to_str().and_then(|rel| {
        session_log::read_log(&sessions_dir)
            .into_iter()
            .rev()
            .find(|e| e.event == session_log::EVENT_PAUSE && e.snapshot == rel)
            .map(|e| e.session_id)
    });
    let mut parts = relative.components();
    let from_dir = match (parts.next(), parts.next(), parts.next()) {
        (Some(dir), Some(_file), None) => dir.as_os_str().to_str().map(str::to_string),
        _ => None,
    };
    recorded.or(from_dir)
}

/// The slot's `verify` text, `Ok(None)` when it holds none.
///
/// What: lists the palace with `memory_list` (the only read that reports a
/// drawer's slot and command) and picks the live occupant through
/// [`match_fact_key_drawer`], the matcher `tm memory forget --fact-key` uses.
/// Errors are `(reason, detail)`.
async fn read_slot_verify(
    host: &ResumeVerifyHost,
    socket: &Path,
    project_dir: &Path,
    fact_key: &str,
    deadline: Instant,
) -> Result<Option<String>, (&'static str, String)> {
    let opts = MemoryVerbOptions {
        palace: host.palace.clone(),
        socket: None,
        cwd: Some(project_dir.to_path_buf()),
    };
    let palace = match resolve_palace_for(true, &opts) {
        Ok(Some(palace)) => palace,
        Ok(None) => return Err(("no_palace", "nothing resolved".to_string())),
        Err(e) => return Err(("no_palace", e.to_string())),
    };
    let params = json!({ "palace": palace, "limit": FACT_KEY_LIST_LIMIT, "full": true });
    let budget = SLOT_READ_TIMEOUT.min(deadline.saturating_duration_since(Instant::now()));
    let listing = memory_checked::call_with_timeout(socket, LIST_METHOD, params, budget)
        .await
        .map_err(|e| match e.downcast_ref::<MemoryRpcError>() {
            Some(_) => ("memory_error", format!("{e:#}")),
            // #9538: a daemon outside the supported protocol is named, not "unreachable".
            None if protocol_mismatch(&e).is_some() => ("memory_protocol", format!("{e:#}")),
            None => ("memory_unreachable", format!("{e:#}")),
        })?;
    let now = chrono::Utc::now();
    let drawer = match_fact_key_drawer(fact_key, &palace, &listing, FACT_KEY_LIST_LIMIT, now)
        .map_err(|e| ("slot_unresolved", e.to_string()))?;
    Ok(drawer
        .get("verify")
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// What the resolved snapshot recorded, for the commands to be judged against.
struct Claims {
    /// The `Branch:` it was paused on.
    branch: Option<String>,
    /// The short `Last commit:` id, which was that branch's tip at the pause.
    commit: Option<String>,
}

impl Claims {
    /// The recorded tip of `branch`, only when the snapshot was paused on it.
    fn commit_of(&self, branch: &str) -> Option<&str> {
        (self.branch.as_deref() == Some(branch))
            .then_some(self.commit.as_deref())
            .flatten()
    }
}

/// Run one allowlisted command within the remaining budget and judge it.
async fn run_check(
    host: &ResumeVerifyHost,
    command: &AllowedCommand,
    cwd: &Path,
    claims: &Claims,
    deadline: Instant,
) -> Value {
    let claimed = match command {
        AllowedCommand::GhPrView(n) => json!({ "pr": n, "state": "OPEN" }),
        AllowedCommand::GitRevParse(branch) => match claims.commit_of(branch) {
            Some(commit) => json!({ "branch": branch, "commit": commit }),
            None => Value::Null,
        },
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return check(
            command.line(),
            claimed,
            Value::Null,
            "unverified:pass_timeout",
        );
    }
    let args = command.args();
    let run = host.runner.run(command.program(), &args, cwd);
    let (returned, verdict) = match tokio::time::timeout(host.command_timeout.min(remaining), run)
        .await
    {
        Err(_) => (Value::Null, "unverified:timeout".to_string()),
        Ok(RunOutcome::Missing) => (
            Value::Null,
            format!("unverified:{}_missing", command.program()),
        ),
        Ok(RunOutcome::SpawnFailed(_)) => (Value::Null, "unverified:spawn_failed".to_string()),
        Ok(RunOutcome::Exited { code: None, .. }) => (Value::Null, "unverified:exit_signal".into()),
        Ok(RunOutcome::Exited {
            code: Some(0),
            stdout,
        }) => judge(command, claims, &stdout),
        Ok(RunOutcome::Exited {
            code: Some(code), ..
        }) => (Value::Null, format!("unverified:exit_{code}")),
    };
    check(command.line(), claimed, returned, &verdict)
}

/// Judge a command's successful output against the snapshot's claims.
///
/// What: `gh` must answer a JSON object with a 40- or 64-hex `headRefOid` and a
/// `state`; `OPEN` is `verified`, `MERGED` or `CLOSED` is `stale`, anything
/// else is `unverified:unexpected_state`. The slot records no head (PR C), so
/// the head is reported as the live value, not compared. `git` must print one
/// object id; it is `verified` when it begins with the snapshot's recorded
/// commit, `stale` when it does not, and `unverified:no_claim` when the
/// snapshot was not paused on that branch.
fn judge(command: &AllowedCommand, claims: &Claims, stdout: &str) -> (Value, String) {
    match command {
        AllowedCommand::GhPrView(_) => {
            let parsed: Option<(String, String)> =
                serde_json::from_str::<Value>(stdout).ok().and_then(|v| {
                    let head = v.get("headRefOid")?.as_str()?.to_string();
                    let state = v.get("state")?.as_str()?.to_string();
                    is_object_id(&head).then_some((head, state))
                });
            let Some((head, state)) = parsed else {
                return (Value::Null, "unverified:unparseable_json".to_string());
            };
            let verdict = match state.as_str() {
                "OPEN" => "verified",
                "MERGED" | "CLOSED" => "stale",
                _ => "unverified:unexpected_state",
            };
            (
                json!({ "headRefOid": head, "state": state }),
                verdict.to_string(),
            )
        }
        AllowedCommand::GitRevParse(branch) => {
            let tip = stdout.trim();
            if !is_object_id(tip) {
                return (Value::Null, "unverified:unparseable_output".to_string());
            }
            let verdict = match claims.commit_of(branch) {
                None => "unverified:no_claim",
                Some(c) if tip.starts_with(&c.to_ascii_lowercase()) => "verified",
                Some(_) => "stale",
            };
            (json!({ "commit": tip }), verdict.to_string())
        }
    }
}

/// A full lowercase SHA-1 or SHA-256 object id.
fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// One `checks` entry.
fn check(command: String, claimed: Value, returned: Value, verdict: &str) -> Value {
    json!({ "command": command, "claimed": claimed, "returned": returned, "verdict": verdict })
}

/// The entry for a line that never ran: its byte length and short hash only.
///
/// Why: the line is store-controlled text, and echoing it into the PM's
/// context is a prompt-injection channel (#9390, critic C2).
/// What: `command` is `null`; `line_len` is the byte length and `line_sha256`
/// the first [`LINE_HASH_HEX`] hex digits of the line's SHA-256.
/// Test: `a_refused_line_reports_its_length_and_hash_never_its_text`.
fn refused(line: &str, verdict: &str) -> Value {
    let mut hash = format!("{:x}", Sha256::digest(line.as_bytes()));
    hash.truncate(LINE_HASH_HEX);
    json!({
        "command": null,
        "claimed": null,
        "returned": null,
        "verdict": verdict,
        "line_len": line.len(),
        "line_sha256": hash,
    })
}

/// A pass that read no command: `status: unverified:<reason>`, logged.
fn unverified(fact_key: Option<&str>, reason: &str, detail: &str) -> Value {
    tracing::warn!("session_context_catchup: resume verify unverified ({reason}): {detail}");
    json!({
        "fact_key": fact_key,
        "status": format!("unverified:{reason}"),
        "detail": detail,
        "checks": [],
    })
}

#[cfg(test)]
#[path = "resume_verify_tests.rs"]
mod tests;
