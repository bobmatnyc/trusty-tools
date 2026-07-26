//! Gmail history-poll engine (#3820, DOC-54 SPEC-AGENTS-06 §7.1.2/§7.3).
//!
//! Why: DOC-54's recommended Gmail transport is Pub/Sub-pull WHERE a GCP
//! topic exists, falling back to `history.list` polling with a persisted
//! `historyId` cursor everywhere else. This slice ships ONLY the polling
//! fallback (Pub/Sub-pull is explicitly deferred — see the PR body) since it
//! needs zero additional GCP setup and is what the demo's `bob-personal`
//! profile can use today. `spawn_listeners` is the harness-runtime entry
//! point: one background tokio task per `enabled = true` listener with
//! `connector = "gmail"`.
//! What: `poll_once` does ONE cursor-bootstrap-or-fetch cycle: no cursor yet
//! -> bootstrap from `users.getProfile` (baseline to "now", no replay);
//! cursor present -> `history.list(startHistoryId=cursor)`, dedup new
//! message ids against an in-memory set seeded from the durable event log,
//! fetch each new message's summary fields, append a `StoredEvent`
//! (unconditionally — every event is durably stored regardless of wake
//! outcome), and hand the event to `wake::wake_bound_agents` when it's
//! event-type-included. A cycle-local `woke_this_cycle` flag threads through
//! the per-event loop so at most ONE wake actually dispatches per
//! `poll_once` call — every qualifying event after the first is rate-limited
//! (`wake::WakeOutcome::RateLimited`), never dispatched. `run_gmail_poll_loop`
//! wraps that in exponential backoff-with-jitter on transient errors and an
//! immediate cursor reset on `410 GONE`, per DOC-54 §7.3.4. The cursor file
//! itself is written atomically (temp-then-rename) and a parse failure on
//! read is logged, not silently swallowed.
//! Test: `next_backoff_doubles_and_caps`, `is_410_gone_detects_status_text`,
//! `stored_event_from_message_extracts_summary_fields`,
//! `load_cursor_warns_and_defaults_on_malformed_json`,
//! `save_cursor_then_load_cursor_round_trips`. The wake-cycle cap itself is
//! pinned in `wake::tests::gate_wake_caps_to_one_dispatch_per_cycle` (the
//! pure decision `wake_bound_agents` delegates to). The network-calling
//! path is exercised manually against a live `bob-personal` mailbox (see the
//! PR body's proof plan) — no mock Gmail server exists in this crate.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use trusty_gworkspace::api::client::BaseClient;
use trusty_gworkspace::api::services::gmail::history::{get_gmail_profile, list_gmail_history};

use crate::listeners::config::ListenerConfig;
use crate::listeners::store::{EventStore, StoredEvent, events_dir};
use crate::listeners::wake;

const MIN_BACKOFF_SECS: u64 = 2;
const MAX_BACKOFF_SECS: u64 = 300;

/// One entry in `~/.trusty-agents/events/cursor-<listener>.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Cursor {
    history_id: Option<String>,
}

fn cursor_path(listener_name: &str) -> Result<PathBuf> {
    Ok(events_dir()?.join(format!("cursor-{listener_name}.json")))
}

/// Load the persisted cursor for `listener_name`.
///
/// Why (#3820 code-critic MEDIUM-3): a missing cursor file is the expected,
/// silent "first run" case (`Cursor::default()`, no log) — but a PRESENT
/// file that fails to parse (truncated write, disk corruption, a manual
/// edit gone wrong) is an anomaly the operator should see in the log rather
/// than have silently swallowed into "treat as absent," which quietly
/// re-baselines the cursor (and, on a Gmail listener, skips whatever
/// history the corrupt file's cursor would have covered).
/// What: `Cursor::default()` on both a missing file and a read error;
/// `tracing::warn!` + `Cursor::default()` on a parse failure specifically.
async fn load_cursor(listener_name: &str) -> Cursor {
    let Ok(path) = cursor_path(listener_name) else {
        return Cursor::default();
    };
    match tokio::fs::read_to_string(&path).await {
        Ok(raw) => match serde_json::from_str(&raw) {
            Ok(cursor) => cursor,
            Err(e) => {
                tracing::warn!(
                    listener = %listener_name,
                    path = %path.display(),
                    error = %e,
                    "cursor file failed to parse; re-baselining (treating as absent)"
                );
                Cursor::default()
            }
        },
        Err(_) => Cursor::default(),
    }
}

/// Persist the cursor for `listener_name`, atomically.
///
/// Why (#3820 code-critic MEDIUM-3): a direct `tokio::fs::write` to the
/// live cursor path can be interrupted mid-write (process kill, power
/// loss) leaving a truncated/malformed file — exactly the corruption case
/// `load_cursor`'s parse-failure branch above now has to warn about
/// instead of silently absorbing. Writing to a sibling `.tmp` path then
/// renaming over the target makes the update atomic on POSIX filesystems:
/// a crash mid-write leaves either the OLD cursor file intact or the fully-
/// written NEW one, never a torn one.
/// What: Write-then-rename; the temp file's name reuses the target's file
/// name with a `.tmp` suffix appended (not a `with_extension` swap, so a
/// `cursor-gmail-personal.json` target's temp file is
/// `cursor-gmail-personal.json.tmp`, unambiguous even if a listener name
/// itself contained a dot).
async fn save_cursor(listener_name: &str, cursor: &Cursor) -> Result<()> {
    let path = cursor_path(listener_name)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let content = serde_json::to_string_pretty(cursor).context("serialize cursor")?;
    let mut tmp_name = path.clone().into_os_string();
    tmp_name.push(".tmp");
    let tmp_path = PathBuf::from(tmp_name);
    tokio::fs::write(&tmp_path, &content)
        .await
        .with_context(|| format!("write temp cursor {}", tmp_path.display()))?;
    tokio::fs::rename(&tmp_path, &path)
        .await
        .with_context(|| format!("rename temp cursor into place {}", path.display()))?;
    Ok(())
}

/// Spawn one background polling task per enabled `connector = "gmail"`
/// listener in `listeners`. Non-gmail / disabled listeners are logged and
/// skipped (DOC-54 §7.5: `google-calendar` is a documented-but-unimplemented
/// connector in this slice).
///
/// Why: The harness-runtime entry point — called once from the API server's
/// `serve_with_config` bootstrap so listeners only run inside the long-lived
/// server process, never a one-shot CLI invocation.
/// What: Fire-and-forget `tokio::task::spawn` per listener, mirroring the
/// existing background-docs-index-build pattern in `api/server/routes.rs`.
pub fn spawn_listeners(listeners: Vec<ListenerConfig>, project_path: PathBuf) {
    for cfg in listeners {
        if !cfg.enabled {
            tracing::debug!(listener = %cfg.name, "listener disabled; skipping");
            continue;
        }
        match cfg.connector.as_str() {
            "gmail" => {
                let project_path = project_path.clone();
                tracing::info!(listener = %cfg.name, interval_secs = cfg.poll_interval_secs, "starting gmail listener poll loop");
                tokio::task::spawn(run_gmail_poll_loop(cfg, project_path));
            }
            other => {
                tracing::warn!(
                    listener = %cfg.name,
                    connector = %other,
                    "listener connector not yet implemented; skipping (deferred — see PR body)"
                );
            }
        }
    }
}

/// Long-running poll loop for one Gmail listener. Never returns under
/// normal operation; a `BaseClient::new()` construction failure logs and
/// exits (nothing to retry — that's a local config problem, not transient).
async fn run_gmail_poll_loop(cfg: ListenerConfig, project_path: PathBuf) {
    let client = match BaseClient::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(listener = %cfg.name, error = %e, "gmail listener: failed to construct BaseClient; not starting");
            return;
        }
    };
    // Seed dedup from the durable log so a process restart doesn't replay
    // events already appended (DOC-54 §7.3.3).
    let dedup = Arc::new(Mutex::new(
        EventStore::recent_ids(500).await.unwrap_or_default(),
    ));
    let mut backoff = Duration::from_secs(MIN_BACKOFF_SECS);

    loop {
        match poll_once(&client, &cfg, &dedup, &project_path).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(listener = %cfg.name, new_events = n, "gmail listener: poll cycle found new events");
                }
                backoff = Duration::from_secs(MIN_BACKOFF_SECS);
            }
            Err(e) if is_410_gone(&e) => {
                tracing::warn!(listener = %cfg.name, "gmail listener: cursor invalid (410 GONE); re-baselining");
                if let Ok(path) = cursor_path(&cfg.name) {
                    let _ = tokio::fs::remove_file(&path).await;
                }
                backoff = Duration::from_secs(MIN_BACKOFF_SECS);
            }
            Err(e) => {
                tracing::warn!(listener = %cfg.name, error = %e, backoff_secs = backoff.as_secs(), "gmail listener: poll error; backing off");
                tokio::time::sleep(jittered(backoff)).await;
                backoff = next_backoff(backoff);
                continue;
            }
        }
        tokio::time::sleep(Duration::from_secs(cfg.poll_interval_secs)).await;
    }
}

fn next_backoff(current: Duration) -> Duration {
    let doubled = current.saturating_mul(2);
    let cap = Duration::from_secs(MAX_BACKOFF_SECS);
    if doubled > cap { cap } else { doubled }
}

/// Add up to 20% jitter to a backoff duration so multiple listeners don't
/// retry in lockstep.
fn jittered(d: Duration) -> Duration {
    let jitter_ms = (d.as_millis() as u64 / 5).max(1);
    let extra = fastrand_like(jitter_ms);
    d + Duration::from_millis(extra)
}

/// Tiny dependency-free jitter source (avoids pulling in a `rand` call for
/// one non-cryptographic offset); seeded from the current time's
/// sub-second nanos.
fn fastrand_like(bound_ms: u64) -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    if bound_ms == 0 { 0 } else { nanos % bound_ms }
}

/// Whether `e` (or anything in its `anyhow` context chain) represents an
/// HTTP 410 Gone response.
///
/// Why: `anyhow::Error::to_string()` (via `Display`) shows only the
/// OUTERMOST `.context(...)` message, not the full chain — `BaseClient`'s
/// underlying `"google api error 410 Gone: ..."` would be invisible to a
/// naive `to_string().contains("410")` check once wrapped by
/// `.context("list_gmail_history")`. Walking `e.chain()` inspects every
/// layer.
/// Test: `is_410_gone_detects_status_text`.
fn is_410_gone(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| cause.to_string().contains("410"))
}

/// One poll cycle: bootstrap-or-fetch, append new events, advance the
/// cursor. Returns the number of new events appended.
async fn poll_once(
    client: &BaseClient,
    cfg: &ListenerConfig,
    dedup: &Arc<Mutex<HashSet<String>>>,
    project_path: &Path,
) -> Result<usize> {
    let account = cfg.identity.as_deref();
    let cursor = load_cursor(&cfg.name).await;

    let Some(history_id) = cursor.history_id else {
        // No cursor yet: baseline to "now" via the profile's current
        // historyId (DOC-54 §7.1.2 — never replay the whole mailbox).
        let profile = get_gmail_profile(client, account)
            .await
            .context("get_gmail_profile (cursor bootstrap)")?;
        let history_id = profile
            .get("historyId")
            .and_then(|v| v.as_str())
            .context("gmail profile response missing historyId")?
            .to_string();
        save_cursor(
            &cfg.name,
            &Cursor {
                history_id: Some(history_id),
            },
        )
        .await?;
        tracing::info!(listener = %cfg.name, "gmail listener: cursor bootstrapped");
        return Ok(0);
    };

    let label = cfg.filter.single_gmail_label();
    let resp = list_gmail_history(client, account, &history_id, label)
        .await
        .context("list_gmail_history")?;

    let next_history_id = resp
        .get("historyId")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut new_count = 0usize;
    // #3820 code-critic CRITICAL fix: cycle-local gate enforcing "max one
    // wake per poll cycle" — every qualifying event in THIS `poll_once` call
    // after the first is rate-limited (see `wake::gate_wake`), never
    // dispatched. Every event is still appended to the store above
    // regardless of this flag — only the LLM dispatch is gated.
    let mut woke_this_cycle = false;
    if let Some(history) = resp.get("history").and_then(|v| v.as_array()) {
        for record in history {
            let Some(added) = record.get("messagesAdded").and_then(|v| v.as_array()) else {
                continue;
            };
            for entry in added {
                let Some(msg) = entry.get("message") else {
                    continue;
                };
                let Some(msg_id) = msg.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let event_id = format!("{}:{}", cfg.name, msg_id);
                {
                    let mut seen = dedup.lock().await;
                    if seen.contains(&event_id) {
                        continue;
                    }
                    seen.insert(event_id.clone());
                }

                let event = match fetch_event_summary(client, account, &cfg.name, msg_id, &event_id)
                    .await
                {
                    Ok(ev) => ev,
                    Err(e) => {
                        tracing::warn!(listener = %cfg.name, message_id = %msg_id, error = %e, "gmail listener: failed to fetch message summary; skipping");
                        continue;
                    }
                };

                if let Err(e) = EventStore::append(&event).await {
                    tracing::warn!(listener = %cfg.name, error = %e, "gmail listener: failed to persist event");
                }
                new_count += 1;

                let included = EventStore::is_event_type_included(&event.event_type).await;
                // Mirror onto the live harness event bus (#3820) so the GUI's
                // Events pane updates in real time via the SAME SSE/Tauri
                // bridge every other event type already uses — see
                // `crate::events::Event::ListenerEventReceived`'s doc comment.
                crate::events::publish(crate::events::Event::ListenerEventReceived {
                    listener_id: event.listener_id.clone(),
                    provider: event.provider.clone(),
                    event_type: event.event_type.clone(),
                    summary: listener_event_summary(&event),
                    included,
                });

                if included {
                    let outcome =
                        wake::wake_bound_agents(project_path, &event, woke_this_cycle).await;
                    if matches!(outcome, wake::WakeOutcome::Woke { .. }) {
                        woke_this_cycle = true;
                    }
                    tracing::info!(listener = %cfg.name, event_id = %event.id, outcome = ?outcome, "wake decision recorded");
                } else {
                    tracing::debug!(listener = %cfg.name, event_id = %event.id, "event type excluded; no wake attempted");
                }
            }
        }
    }

    if let Some(next_id) = next_history_id {
        save_cursor(
            &cfg.name,
            &Cursor {
                history_id: Some(next_id),
            },
        )
        .await?;
    }

    Ok(new_count)
}

/// Fetch a new message's headers/snippet and build its `StoredEvent`.
async fn fetch_event_summary(
    client: &BaseClient,
    account: Option<&str>,
    listener_id: &str,
    message_id: &str,
    event_id: &str,
) -> Result<StoredEvent> {
    let msg = trusty_gworkspace::api::services::gmail::messages::get_gmail_message_content(
        client,
        serde_json::json!({ "message_id": message_id, "account": account }),
    )
    .await
    .context("get_gmail_message_content")?;
    Ok(stored_event_from_message(listener_id, event_id, &msg))
}

/// Fold a `StoredEvent`'s summary fields into one glanceable line for the
/// live event bus (`Event::ListenerEventReceived::summary`).
fn listener_event_summary(event: &StoredEvent) -> String {
    match (&event.from, &event.subject) {
        (Some(from), Some(subject)) => format!("{from}: {subject}"),
        (Some(from), None) => from.clone(),
        (None, Some(subject)) => subject.clone(),
        (None, None) => event
            .snippet
            .clone()
            .unwrap_or_else(|| event.event_type.clone()),
    }
}

/// Pure extraction of summary fields from a Gmail `messages.get(format=full)`
/// JSON response — split out for unit testing without a live client.
fn stored_event_from_message(listener_id: &str, event_id: &str, msg: &Value) -> StoredEvent {
    let headers = msg
        .get("payload")
        .and_then(|p| p.get("headers"))
        .and_then(|h| h.as_array())
        .cloned()
        .unwrap_or_default();
    let header = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|h| {
                h.get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .and_then(|h| h.get("value").and_then(|v| v.as_str()))
            .map(String::from)
    };
    StoredEvent {
        id: event_id.to_string(),
        listener_id: listener_id.to_string(),
        provider: "gmail".to_string(),
        event_type: "message.received".to_string(),
        ts: chrono::Utc::now().to_rfc3339(),
        from: header("From"),
        subject: header("Subject"),
        snippet: msg
            .get("snippet")
            .and_then(|v| v.as_str())
            .map(String::from),
        included: true,
    }
}

#[cfg(test)]
// The cursor tests below deliberately hold `HOME_LOCK` across `.await`
// points via `crate::test_env::lock_home()` (the whole point of the lock —
// see `crate::test_env`'s module doc); `lock_home()` also marks this
// thread as the current holder for `listeners::store::events_dir`'s
// per-caller recurrence guard (issue #3922 follow-up), which these tests
// reach via `events_dir()`/`cursor_path()`.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;

    #[test]
    fn next_backoff_doubles_and_caps() {
        let start = Duration::from_secs(MIN_BACKOFF_SECS);
        let doubled = next_backoff(start);
        assert_eq!(doubled, Duration::from_secs(MIN_BACKOFF_SECS * 2));

        let near_cap = Duration::from_secs(MAX_BACKOFF_SECS - 1);
        let capped = next_backoff(near_cap);
        assert_eq!(capped, Duration::from_secs(MAX_BACKOFF_SECS));

        let already_at_cap = Duration::from_secs(MAX_BACKOFF_SECS);
        assert_eq!(
            next_backoff(already_at_cap),
            Duration::from_secs(MAX_BACKOFF_SECS)
        );
    }

    #[test]
    fn is_410_gone_detects_status_text() {
        use anyhow::Context;
        // Wrapped with an outer `.context(...)` exactly like `poll_once`
        // does, so this pins the chain-walking behavior (not just the
        // outermost message).
        let e: anyhow::Error =
            Err::<(), _>(anyhow::anyhow!("google api error 410 Gone: cursor expired"))
                .context("list_gmail_history")
                .unwrap_err();
        assert!(is_410_gone(&e));

        let other: anyhow::Error = Err::<(), _>(anyhow::anyhow!(
            "google api error 500 Internal Server Error: oops"
        ))
        .context("list_gmail_history")
        .unwrap_err();
        assert!(!is_410_gone(&other));
    }

    #[test]
    fn stored_event_from_message_extracts_summary_fields() {
        let msg = serde_json::json!({
            "snippet": "Want to come over Sunday?",
            "payload": {
                "headers": [
                    { "name": "From", "value": "dad@family.com" },
                    { "name": "Subject", "value": "Dinner Sunday?" },
                ]
            }
        });
        let event = stored_event_from_message("gmail-personal", "gmail-personal:m1", &msg);
        assert_eq!(event.id, "gmail-personal:m1");
        assert_eq!(event.provider, "gmail");
        assert_eq!(event.event_type, "message.received");
        assert_eq!(event.from.as_deref(), Some("dad@family.com"));
        assert_eq!(event.subject.as_deref(), Some("Dinner Sunday?"));
        assert_eq!(event.snippet.as_deref(), Some("Want to come over Sunday?"));
    }

    fn set_test_home(dir: &std::path::Path) {
        // SAFETY: caller holds `HOME_LOCK` for the duration of the test (via
        // `crate::test_env::lock_home()`, see `crate::test_env`'s module
        // doc) so no other thread observes `HOME` mid-mutation.
        unsafe {
            std::env::set_var("HOME", dir);
        }
    }

    /// #3820 code-critic MEDIUM-3: `save_cursor` writes atomically
    /// (temp-then-rename) and `load_cursor` reads back exactly what was
    /// written.
    #[tokio::test]
    async fn save_cursor_then_load_cursor_round_trips() {
        // `lock_home()` (not the raw `HOME_LOCK.lock()`) also marks this
        // thread as the holder for `listeners::store::events_dir`'s
        // per-caller recurrence guard (issue #3922 follow-up) — this test
        // reaches that function via `events_dir()`/`cursor_path()` below.
        let _guard = crate::test_env::lock_home();
        let tmp = tempfile::tempdir().unwrap();
        set_test_home(tmp.path());

        let cursor = Cursor {
            history_id: Some("12345".to_string()),
        };
        save_cursor("gmail-personal", &cursor).await.unwrap();

        let loaded = load_cursor("gmail-personal").await;
        assert_eq!(loaded.history_id.as_deref(), Some("12345"));

        // The temp file must not be left behind after a successful rename.
        let tmp_path = events_dir().unwrap().join("cursor-gmail-personal.json.tmp");
        assert!(
            !tmp_path.exists(),
            "temp cursor file should be renamed away"
        );
    }

    /// #3820 code-critic MEDIUM-3: a present-but-malformed cursor file must
    /// NOT be silently treated identically to "no cursor" without a trace —
    /// this pins the fallback BEHAVIOR (defaults to no cursor, so the next
    /// poll re-baselines); the `tracing::warn!` emission itself isn't
    /// asserted here (no `tracing-test` dependency in this crate), but the
    /// distinct code path is exercised.
    #[tokio::test]
    async fn load_cursor_warns_and_defaults_on_malformed_json() {
        let _guard = crate::test_env::lock_home();
        let tmp = tempfile::tempdir().unwrap();
        set_test_home(tmp.path());

        let path = cursor_path("gmail-personal").unwrap();
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, b"{not valid json").await.unwrap();

        let loaded = load_cursor("gmail-personal").await;
        assert!(
            loaded.history_id.is_none(),
            "malformed cursor file must fall back to no-cursor, not panic or propagate"
        );
    }

    #[tokio::test]
    async fn load_cursor_defaults_when_file_absent() {
        let _guard = crate::test_env::lock_home();
        let tmp = tempfile::tempdir().unwrap();
        set_test_home(tmp.path());

        let loaded = load_cursor("never-seen-listener").await;
        assert!(loaded.history_id.is_none());
    }
}
