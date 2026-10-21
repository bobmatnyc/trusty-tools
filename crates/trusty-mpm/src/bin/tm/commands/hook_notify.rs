//! `tm hook`'s push of a `Notification` to a configured inbox (#8392), and the
//! single best-effort daemon POST every non-`SubagentStop` event takes.
//!
//! Why: a fleet supervisor wants the "session needs user action" signal pushed
//! to it. Nothing names the supervisor, so the target is configuration only
//! (`TRUSTY_MPM_NOTIFY_INBOX`, else `[notification_hook] inbox`). A hook that
//! blocks or fails stalls the user's session, so the push is bounded and every
//! failure is one stderr line and exit 0. The POST moved here from
//! `commands::misc`, which sits over the 500-SLOC cap, so the hook grew no lines
//! there.
//! What: [`forward_notification`] resolves the target and appends one JSON line
//! to `<inbox>/events.jsonl` on a worker thread the hook waits on for at most
//! [`FORWARD_TIMEOUT`]; [`post_best_effort`] is the fire-and-forget POST.
//! Test: `tests` below; `tm_hook_notification_8392` drives the real binary.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use trusty_mpm::client::DaemonClient;

use super::tmux_attach::tmux_pane_id_from_env;
use trusty_mpm::core::standalone::hooks::notification::{
    INBOX_EVENTS_FILE, NOTIFICATION_EVENT, NOTIFY_INBOX_ENV, NotificationHookConfig,
    PUSHED_MESSAGE_FIELD, PushTarget, resolve_push_target,
};

/// The longest `tm hook` waits on the inbox append: one second.
///
/// Why: the registered hook timeout is 5 s and the daemon POST after this
/// takes up to 2 s, so 1 s leaves 2 s of headroom. A local append takes
/// microseconds; only a stuck target (a FIFO with no reader, a hung mount)
/// reaches the bound.
pub(crate) const FORWARD_TIMEOUT: Duration = Duration::from_secs(1);

/// Push a `Notification` to the configured inbox, if one is configured.
///
/// Why (#8392): see the module doc. Returns nothing because no outcome may
/// change the hook's exit status.
/// What: no-op for any other event and for an unset target. Otherwise builds
/// the inbox line (the prototype `notify-supervisor.sh` shape the Architect's
/// poller reads) and runs [`forward_to_target`]; a failure prints one
/// `trusty-mpm: Notification forward failed` line to stderr. #9167: a
/// completed append is named on `payload`, the daemon POST's, by
/// [`record_push`].
/// Test: `tm_hook_notification_8392::a_notification_reaches_the_daemon_and_the_inbox`,
/// `tm_hook_notification_8392::a_stuck_inbox_costs_one_logged_failure_and_exit_zero`,
/// `tm_hook_message_events_9167::a_notification_push_reaches_the_bus_with_the_inbox_line_digest_9167`.
pub(crate) fn forward_notification(
    event: &str,
    stdin: Option<&Value>,
    cwd: &str,
    payload: &mut Value,
) {
    if event != NOTIFICATION_EVENT {
        return;
    }
    let env_value = std::env::var_os(NOTIFY_INBOX_ENV).filter(|v| !v.is_empty());
    let config = if env_value.is_some() {
        NotificationHookConfig::default()
    } else {
        trusty_mpm::core::config::MpmConfig::load_default().notification_hook
    };
    let target = resolve_push_target(env_value, &config);
    let context = LineContext {
        payload: stdin.cloned().unwrap_or(Value::Null),
        cwd: cwd.to_string(),
        project_dir: std::env::var("CLAUDE_PROJECT_DIR").ok(),
        // #8392: only a `%N` pane id is kept; tmux prefix-matches anything else.
        tmux_pane: tmux_pane_id_from_env(std::env::var("TMUX_PANE").ok()),
    };
    match forward_to_target(&target, context, FORWARD_TIMEOUT) {
        Ok(line) => {
            if let PushTarget::Inbox(dir) = &target {
                record_push(payload, dir, line);
            }
        }
        Err(reason) => eprintln!("trusty-mpm: Notification forward failed (#8392): {reason}"),
    }
}

/// Name a completed push on the daemon payload (#9167).
///
/// Why: only this process knows the append completed and what it wrote; the
/// daemon turns the field into a `hook_push` `message` event.
/// What: sets [`PUSHED_MESSAGE_FIELD`] to `{"to": <inbox>, "text": <line>}`
/// when `line` is `Some`; leaves `payload` untouched otherwise.
/// Test: `a_completed_append_returns_the_line_it_wrote_9167`.
pub(crate) fn record_push(payload: &mut Value, inbox: &Path, line: Option<String>) {
    if let Some(line) = line {
        payload[PUSHED_MESSAGE_FIELD] = serde_json::json!({
            "to": inbox.display().to_string(),
            "text": line,
        });
    }
}

/// What the inbox line is built from, moved onto the worker thread.
pub(crate) struct LineContext {
    /// The hook's stdin JSON.
    pub(crate) payload: Value,
    /// The hook process's cwd, used when the payload carries none.
    pub(crate) cwd: String,
    /// `CLAUDE_PROJECT_DIR`, the project root Claude Code reports.
    pub(crate) project_dir: Option<String>,
    /// `TMUX_PANE`, when the session runs in tmux and it is a `%N` pane id.
    pub(crate) tmux_pane: Option<String>,
}

/// Append one line for `context` to the target, waiting at most `timeout`.
///
/// Why: the bound has to cover the open as well as the write — opening a FIFO
/// with no reader blocks forever — so the whole append runs on a detached
/// thread and the hook stops waiting at the bound. Process exit reaps it.
/// What: `Ok(None)` for [`PushTarget::Unset`]; `Ok(Some(line))`, the line
/// written without its newline, for a completed append (#9167); `Err` with a
/// reason for a relative target (the value is not echoed), an append error, or
/// no answer within `timeout`. The inbox directory is never created.
/// Test: `an_unset_target_forwards_nothing`, `a_missing_inbox_is_an_error_not_a_mkdir`,
/// `a_fifo_with_no_reader_times_out`, `a_completed_append_returns_the_line_it_wrote_9167`.
pub(crate) fn forward_to_target(
    target: &PushTarget,
    context: LineContext,
    timeout: Duration,
) -> Result<Option<String>, String> {
    let dir = match target {
        PushTarget::Unset => return Ok(None),
        PushTarget::NotAbsolute => {
            return Err("the configured inbox is not an absolute path; not forwarded".into());
        }
        PushTarget::Inbox(dir) => dir.clone(),
    };
    let file = dir.join(INBOX_EVENTS_FILE);
    let (tx, rx) = std::sync::mpsc::channel();
    let worker_file = file.clone();
    std::thread::spawn(move || {
        let tmux_session = context.tmux_pane.as_deref().and_then(tmux_session_of);
        let line = inbox_line(&context, tmux_session, chrono::Utc::now());
        let _ = tx.send(append_line(&worker_file, &line).map(|()| line));
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(line)) => Ok(Some(line)),
        Ok(Err(e)) => Err(format!("{}: {e}", file.display())),
        Err(_) => Err(format!(
            "{}: no answer within {} ms",
            file.display(),
            timeout.as_millis()
        )),
    }
}

/// Append `line` plus a newline to `file` in one write, creating the file but
/// never its directory.
fn append_line(file: &Path, line: &str) -> std::io::Result<()> {
    let mut out = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(file)?;
    out.write_all(format!("{line}\n").as_bytes())
}

/// The tmux session that owns `pane`, or `None`; `None` without running tmux
/// when `pane` is not a `%N` pane id.
///
/// Test: `only_a_percent_n_tmux_pane_is_a_tmux_target`.
fn tmux_session_of(pane: &str) -> Option<String> {
    // #8392: the bare `-t pane` below is exact only because this gate admits
    // an immutable `%N` and nothing else (tmux-exact-targets allowlist row).
    let pane = tmux_pane_id_from_env(Some(pane.to_owned()))?;
    let out = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", &pane, "#S"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !name.is_empty()).then_some(name)
}

/// The inbox line: the fields the prototype `notify-supervisor.sh` wrote.
///
/// Why: the Architect's `fleet-poll.py` and `tm-fleet-check` read
/// `type`, `message`, `tmux_session` and `display` from these lines, so the
/// shape is the contract #8436 P3 consumes.
/// What: `ts` (UTC, second precision), `session_id`, `cwd`, `project` (basename
/// of `CLAUDE_PROJECT_DIR`, else of `cwd`), `tmux_session`, `tmux_pane`, `type`
/// (`notification_type`), `message`, and `display` = `[<tmux session or
/// project>]: <message>`.
/// Test: `the_inbox_line_carries_the_prototype_fields`.
pub(crate) fn inbox_line(
    context: &LineContext,
    tmux_session: Option<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let field = |key: &str| context.payload.get(key).and_then(Value::as_str);
    let cwd = field("cwd").unwrap_or(&context.cwd).to_string();
    let base = |p: &str| {
        Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
    };
    let project = context
        .project_dir
        .as_deref()
        .and_then(base)
        .or_else(|| base(&cwd));
    let message = field("message");
    let label = tmux_session
        .clone()
        .or_else(|| project.clone())
        .unwrap_or_else(|| "unknown-session".to_string());
    serde_json::json!({
        "ts": now.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "session_id": field("session_id"),
        "cwd": cwd,
        "project": project,
        "tmux_session": tmux_session,
        "tmux_pane": context.tmux_pane,
        "type": field("notification_type"),
        "message": message,
        "display": format!("[{label}]: {}", message.unwrap_or_default()),
    })
    .to_string()
}

/// One fire-and-forget `POST /hooks` of `body` to the daemon socket.
///
/// Why: failing the hook would block the user's prompt, so every failure —
/// daemon down, socket absent, refused address — is dropped. #6288: `url` is
/// the `unix:` locator `main` resolved; the post never opens TCP.
/// What: one 2 s socket exchange through [`DaemonClient::with_client`]. A
/// refusal sentinel is refused before any I/O.
/// Test: `tm_hook_notification_8392::a_down_daemon_does_not_block_the_hook`,
/// `tm_client_socket_cutover_6288::a_hook_reaches_the_daemon_over_the_socket_only`.
pub(crate) async fn post_best_effort(client: &reqwest::Client, url: &str, body: &Value) {
    let _ = DaemonClient::with_client(client.clone(), url)
        .post("/hooks")
        .json(body)
        .timeout(Duration::from_secs(2))
        .send()
        .await;
}

/// Deliver a `SessionStart` or `SessionEnd` over the daemon socket (#8531,
/// #8980).
///
/// Why: only the socket proves the sender's pid, and the daemon binds the
/// session to the `claude` above that pid — the identity a later
/// `tm repair delegation` is checked against. #8980: a `SessionEnd` stales the
/// session's live delegations only when it arrives over the socket from under
/// that bound `claude`. #6288: there is no HTTP fallback any more; an absent
/// socket drops the event, as any other hook failure is dropped.
/// What: [`post_best_effort`], whose transport is the socket.
/// Test: `tm_hook_session_start_8531::a_session_start_over_the_socket_binds_its_claude`,
/// `tm_hook_session_start_8531::a_session_start_with_no_socket_never_falls_back_to_http`,
/// `tm_hook_session_end_8980::the_owners_session_end_over_the_socket_stales_its_records`.
pub(crate) async fn post_lifecycle_via_socket(client: &reqwest::Client, url: &str, body: &Value) {
    post_best_effort(client, url, body).await;
}

#[cfg(test)]
#[path = "hook_notify_tests.rs"]
mod tests;
