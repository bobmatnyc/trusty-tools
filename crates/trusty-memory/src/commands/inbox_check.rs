//! Handler for `trusty-memory inbox-check` (issue #99).
//!
//! Why: Claude Code's `SessionStart` hook ingests stdout verbatim and
//! injects it as context for the new session. We use this to deliver
//! inter-project messages that have piled up in the project's palace
//! since the previous session — without polling, IPC, or background
//! workers. The receiver session "picks up the mail" exactly once per
//! delivery.
//!
//! What: a side-effect-only command that:
//!   1. Resolves the calling project's palace slug. Precedence (issue #125):
//!      `--palace` override > stdin JSON `cwd` field > process cwd via
//!      [`crate::messaging::cwd_palace_slug`].
//!   2. Queries the daemon's `GET /api/v1/messages?palace=<slug>&unread_only=true`
//!      endpoint for unread messages.
//!   3. Renders each message into a Markdown injection block and writes
//!      them to stdout in chronological order.
//!   4. Atomically marks each delivered message read via the same HTTP API
//!      (`POST /api/v1/messages/mark_read` with the drawer id).
//!
//! Like `prompt-context`, every error path degrades to exit 0 with empty
//! stdout — failing the SessionStart hook would block the new Claude Code
//! session. The mark-read step is best-effort: if it fails, the next
//! SessionStart will redeliver, which is preferable to silently dropping a
//! message we never confirmed delivery for.
//!
//! Test: `inbox_check_returns_ok_without_daemon` covers the no-daemon
//! branch; the round-trip is exercised by
//! `web::tests::messages_endpoint_round_trip`.

use anyhow::Result;
use serde::Deserialize;
use std::time::{Duration, Instant};

use crate::hook_emit::{post_hook_event, HookEventPayload};
use crate::prompt_log::{PromptLogEntry, PromptLogger};
use crate::{hook_prompt_excerpt, HookType, InjectionKind};

/// Connect + total request timeout. Kept short so a slow/dead daemon can
/// never block a Claude Code session for more than a few seconds.
const HTTP_TIMEOUT: Duration = Duration::from_millis(2500);

/// Server payload schema for one decoded message.
///
/// Why: deserialise the daemon's `GET /api/v1/messages` response into
/// something we can render. The shape mirrors
/// [`crate::messaging::Message`] but we keep it local to the command so a
/// future on-wire change to the daemon can be absorbed without leaking
/// dependencies.
/// What: `id` carries the drawer UUID we POST back to `mark_read`;
/// `formatted` carries the Markdown block built by `to_injection_block` so
/// the CLI doesn't have to know the rendering rules. (Both fields are
/// optional in the JSON for forward compatibility with daemons that don't
/// pre-render.)
/// Test: indirectly via `web::tests::messages_endpoint_round_trip`.
#[derive(Deserialize)]
struct ServerMessage {
    id: String,
    #[serde(default)]
    formatted: Option<String>,
    // Raw fields, used as a fallback if `formatted` is absent.
    #[serde(default)]
    from_palace: Option<String>,
    #[serde(default)]
    to_palace: Option<String>,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    sent_at: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

/// Entry point for `trusty-memory inbox-check`.
///
/// Why: SessionStart hook. Same fail-soft contract as
/// [`crate::commands::prompt_context::handle_prompt_context`] — every
/// failure path exits 0 silently with no stdout so the user's session
/// start is never blocked.
/// What:
///   1. Resolves the recipient palace slug from cwd (or explicit `--palace`).
///   2. Fetches unread messages via the daemon's HTTP API.
///   3. Prints the formatted Markdown blocks to stdout.
///   4. POSTs back to mark each delivered message read.
///
/// `palace` overrides the cwd-derived slug; useful for test rigs and for
/// projects whose repo basename does not match their preferred palace.
/// Test: `inbox_check_returns_ok_without_daemon`.
pub async fn handle_inbox_check(palace: Option<String>) -> Result<()> {
    let start = Instant::now();
    // SessionStart hooks deliver session metadata (JSON) on stdin. Capture
    // it best-effort for the log; never block on stdin reads.
    let trigger_prompt = read_stdin_best_effort();

    // Resolve recipient palace eagerly so the log entry can carry it on
    // every failure path. `palace` (the explicit override) takes precedence;
    // then a stdin-provided `cwd` (issue #125 — the JSON Claude Code pipes
    // into the SessionStart hook reflects the user's actual cwd, which
    // differs from the hook process cwd when the hook was registered from a
    // different directory); then the process cwd; finally `"<unknown>"`.
    let recipient = palace
        .clone()
        .or_else(|| palace_slug_from_stdin_cwd(&trigger_prompt))
        .or_else(|| crate::messaging::cwd_palace_slug().ok())
        .unwrap_or_else(|| "<unknown>".to_string());

    let injection = run_inbox_fetch(&trigger_prompt, &recipient, start).await;

    // Submission-logging Part A: emit a `HookFired` activity event so the
    // dashboard / TUI feed sees this SessionStart invocation. Best-effort.
    emit_hook_event(&trigger_prompt, &injection, &recipient, start).await;

    Ok(())
}

/// Internal helper that performs the actual inbox fetch + print + log
/// pipeline.
///
/// Why: split out of `handle_inbox_check` so the wrapper can emit the
/// activity event for *every* exit path (no daemon, empty inbox, real
/// messages) without duplicating the emit call at every return.
/// What: same logic as the prior monolithic handler — but returns the
/// rendered injection (empty string when nothing was emitted) so the
/// caller can include the size in the activity event payload.
/// Test: `inbox_check_returns_ok_without_daemon`,
/// `inbox_check_logs_attempt_without_daemon` (unchanged paths).
async fn run_inbox_fetch(trigger_prompt: &str, recipient: &str, start: Instant) -> String {
    // Resolve daemon address — missing = exit silently (but still log).
    let addr = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(addr)) => addr,
        _ => {
            log_entry(trigger_prompt, "", 0, recipient, start);
            return String::new();
        }
    };
    let base = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{addr}")
    };

    let client = match reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            log_entry(trigger_prompt, "", 0, recipient, start);
            return String::new();
        }
    };

    // Fetch unread messages.
    let list_url = format!("{base}/api/v1/messages?palace={recipient}&unread_only=true");
    let resp = match client.get(&list_url).send().await {
        Ok(r) => r,
        Err(_) => {
            log_entry(trigger_prompt, "", 0, recipient, start);
            return String::new();
        }
    };
    if !resp.status().is_success() {
        log_entry(trigger_prompt, "", 0, recipient, start);
        return String::new();
    }
    let messages: Vec<ServerMessage> = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            log_entry(trigger_prompt, "", 0, recipient, start);
            return String::new();
        }
    };
    if messages.is_empty() {
        log_entry(trigger_prompt, "", 0, recipient, start);
        return String::new();
    }

    // Render. We buffer the injection into a string so the same content the
    // user sees lands on stdout AND in the log file (issue #105). Writing
    // to stdout still uses `println!` (single syscall per block) so the
    // ordering relative to the hook caller is preserved.
    let mut injection = String::new();
    injection.push_str(&format!(
        "# Inter-project inbox (trusty-memory, palace `{recipient}`)\n\n"
    ));
    for m in &messages {
        let block = match &m.formatted {
            Some(s) => s.clone(),
            None => render_fallback(m),
        };
        injection.push_str(&block);
        injection.push('\n');
        injection.push('\n');
    }
    // One write to stdout — the hook reads the entire stream.
    print!("{injection}");

    // Mark each delivered message read. Best-effort: a failed ack means the
    // next SessionStart will redeliver, which is safer than silently
    // dropping a message we never confirmed.
    let mark_url = format!("{base}/api/v1/messages/mark_read");
    for m in &messages {
        let body = serde_json::json!({"palace": recipient, "drawer_id": m.id});
        let _ = client.post(&mark_url).json(&body).send().await;
    }

    log_entry(trigger_prompt, &injection, messages.len(), recipient, start);
    injection
}

/// Emit a `HookFired` activity event for the SessionStart hook firing.
///
/// Why: same rationale as `commands::prompt_context::emit_hook_event` —
/// the activity feed needs to see every hook firing so a normal Claude
/// Code session populates the feed instead of leaving it empty.
/// What: builds a `HookEventPayload` carrying the recipient palace
/// slug, the rendered injection length, a short excerpt of the stdin
/// payload (typically just session metadata, but harmless), and the
/// elapsed duration. Best-effort.
/// Test: covered by the daemon-side test
/// `hook_activity_endpoint_appends_to_activity_log`.
async fn emit_hook_event(trigger_prompt: &str, injection: &str, recipient: &str, start: Instant) {
    let palace_id = if recipient == "<unknown>" || recipient.is_empty() {
        None
    } else {
        Some(recipient.to_string())
    };
    let payload = HookEventPayload {
        palace_id: palace_id.clone(),
        palace_name: palace_id,
        hook_type: HookType::SessionStart,
        injection_kind: InjectionKind::InboxCheck,
        injection_length: injection.len() as u64,
        trigger_prompt_excerpt: hook_prompt_excerpt(trigger_prompt),
        duration_ms: start.elapsed().as_millis() as u64,
    };
    post_hook_event(payload).await;
}

/// Read the hook's stdin into a string, capped at 64 KiB.
///
/// Why (issue #105): SessionStart hooks may forward session metadata JSON via
/// stdin; capturing it lets the log entry record what triggered the
/// invocation. Failures or absent stdin (e.g. running the command in a TTY
/// for manual testing) degrade to an empty string.
/// What: reads up to 64 KiB synchronously; checks `is_terminal` first to
/// avoid blocking on an interactive stdin.
/// Test: not unit-tested (process stdin is hard to mock); covered indirectly.
fn read_stdin_best_effort() -> String {
    use std::io::Read;
    const STDIN_CAP_BYTES: usize = 64 * 1024;
    let stdin = std::io::stdin();
    if std::io::IsTerminal::is_terminal(&stdin) {
        return String::new();
    }
    let mut buf = String::new();
    let _ = stdin
        .lock()
        .take(STDIN_CAP_BYTES as u64)
        .read_to_string(&mut buf);
    buf
}

/// Parse a SessionStart hook stdin payload and derive the palace slug from
/// the JSON's `cwd` field (issue #125).
///
/// Why: Claude Code pipes the session metadata as JSON; the `cwd` field
/// reflects the user's actual working directory at session start. The hook
/// process inherits its cwd from wherever it was registered (often a fixed
/// install root), so the stdin-provided `cwd` is the authoritative source
/// for the recipient palace slug.
/// What: best-effort JSON parse; returns `Some(slug)` only when the payload
/// is a JSON object carrying a non-empty `cwd` string AND
/// `cwd_palace_slug_at` succeeds for that path.
/// Test: `palace_slug_from_stdin_cwd_uses_stdin_path`.
fn palace_slug_from_stdin_cwd(stdin_payload: &str) -> Option<String> {
    if stdin_payload.trim().is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(stdin_payload).ok()?;
    let cwd = value.get("cwd")?.as_str()?;
    if cwd.is_empty() {
        return None;
    }
    crate::messaging::cwd_palace_slug_at(std::path::Path::new(cwd)).ok()
}

/// Append one log entry to the enriched-prompt log, swallowing failures.
fn log_entry(
    trigger_prompt: &str,
    injection: &str,
    unread_count: usize,
    palace: &str,
    start: Instant,
) {
    let logger = PromptLogger::from_env();
    let entry = PromptLogEntry::new(
        "SessionStart",
        "inbox-check-messages",
        palace,
        trigger_prompt,
        injection,
    )
    .with_unread_messages_count(unread_count)
    .with_duration_ms(start.elapsed().as_millis() as u64);
    logger.log(entry);
}

/// Fallback renderer used when the daemon does not pre-format messages.
///
/// Why: defends against an older daemon that returns the raw envelope
/// fields without a `formatted` key. The block shape matches
/// `Message::to_injection_block` so the rendered session context is
/// indistinguishable from the server-side format.
/// What: synthesises the `## Message from <from>` heading plus metadata
/// and body using whichever optional fields are present, substituting
/// `"<unknown>"` when a field is absent.
/// Test: indirectly via the integration tests; defensive fallback only.
fn render_fallback(m: &ServerMessage) -> String {
    let from = m.from_palace.as_deref().unwrap_or("<unknown>");
    let to = m.to_palace.as_deref().unwrap_or("<unknown>");
    let purpose = m.purpose.as_deref().unwrap_or("<unknown>");
    let sent_at = m.sent_at.as_deref().unwrap_or("<unknown>");
    let content = m.content.as_deref().unwrap_or("<missing body>");
    format!(
        "## Message from {from} (purpose: {purpose})\n\
         _sent {sent_at} → {to}_\n\
         \n\
         {content}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (issue #125): when Claude Code invokes the SessionStart hook, the
    /// stdin JSON carries a `cwd` field reflecting the user's actual cwd.
    /// The hook process cwd may not match (it inherits from wherever the
    /// hook was registered). `palace_slug_from_stdin_cwd` must derive the
    /// slug from the stdin payload, not the process cwd.
    /// What: build a JSON payload with a tempdir `cwd`; assert the derived
    /// slug matches `cwd_palace_slug_at(tempdir)` and reflects the tempdir
    /// basename rather than the process cwd basename.
    /// Test: itself.
    #[test]
    fn palace_slug_from_stdin_cwd_uses_stdin_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let project = tmp.path().join("inbox-stdin-project");
        std::fs::create_dir_all(&project).expect("create project dir");
        let payload = serde_json::json!({
            "hook_event_name": "SessionStart",
            "cwd": project.to_string_lossy(),
        })
        .to_string();

        let expected =
            crate::messaging::cwd_palace_slug_at(&project).expect("derive slug from stdin cwd");
        let got = palace_slug_from_stdin_cwd(&payload).expect("slug from stdin");
        assert_eq!(got, expected);
        assert!(
            got.contains("inbox-stdin-project"),
            "expected slug derived from stdin path, got {got:?}"
        );
    }

    /// Why: empty stdin, non-JSON stdin, or JSON without a `cwd` field must
    /// return `None` so the caller falls back to the next resolution layer.
    /// What: exercise each negative path.
    /// Test: itself.
    #[test]
    fn palace_slug_from_stdin_cwd_returns_none_on_bad_input() {
        assert_eq!(palace_slug_from_stdin_cwd(""), None);
        assert_eq!(palace_slug_from_stdin_cwd("not json"), None);
        assert_eq!(palace_slug_from_stdin_cwd("{\"foo\":\"bar\"}"), None);
        assert_eq!(palace_slug_from_stdin_cwd("{\"cwd\":\"\"}"), None);
    }

    /// Why: the hook is wired into every Claude Code session start; failing
    /// it would block the session opening. Without a running daemon
    /// `read_daemon_addr` returns `None`, and we must degrade silently.
    /// What: pin a tempdir as the data directory, then call the handler
    /// with an unreachable daemon and assert it returns `Ok(())`.
    #[tokio::test]
    async fn inbox_check_returns_ok_without_daemon() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: tests serialise on `TRUSTY_DATA_DIR_OVERRIDE` by
        // convention; we only mutate inside this test's scope.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let res = handle_inbox_check(Some("test-palace".to_string())).await;
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            res.is_ok(),
            "missing daemon lockfile must degrade to Ok(()), got {res:?}"
        );
    }

    /// Why (issue #105): the SessionStart hook must record its invocation
    /// even when no daemon is running, so we can see "session opened, no
    /// inbox to check" in the JSONL stream.
    /// What: pin a tempdir as the data dir; call the handler with an
    /// explicit palace so `cwd_palace_slug` is not consulted; assert a
    /// single log entry tagged `inbox-check-messages` lands under the logs
    /// directory.
    /// Test: itself.
    #[tokio::test]
    async fn inbox_check_logs_attempt_without_daemon() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: env mutation is scoped to this test.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
            std::env::remove_var(crate::prompt_log::ENV_ENABLED);
            std::env::remove_var(crate::prompt_log::ENV_DIR);
            std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
        }
        let res = handle_inbox_check(Some("explicit-palace".to_string())).await;
        let logs_dir = trusty_common::resolve_data_dir("trusty-memory")
            .expect("resolve data dir")
            .join("logs");
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(res.is_ok());
        // Filter by FILE_PREFIX so unrelated daemon log files (stdout.log,
        // stderr.log) under the same data root don't trip the count.
        let files: Vec<_> = std::fs::read_dir(&logs_dir)
            .expect("logs dir should exist")
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("enriched-prompts."))
            })
            .collect();
        assert_eq!(
            files.len(),
            1,
            "expected one enriched-prompts log file, got {files:?}"
        );
        let content = std::fs::read_to_string(&files[0]).expect("read log");
        let line = content.lines().next().expect("at least one line");
        let parsed: crate::prompt_log::PromptLogEntry =
            serde_json::from_str(line).expect("parse JSONL");
        assert_eq!(parsed.hook_type, "SessionStart");
        assert_eq!(parsed.injection_kind, "inbox-check-messages");
        assert_eq!(parsed.palace, "explicit-palace");
    }
}
