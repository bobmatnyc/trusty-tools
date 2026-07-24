//! Append-only JSONL event store + per-event-type include/exclude state
//! (#3820, DOC-54 SPEC-AGENTS-06 §7.4).
//!
//! Why: The Events pane (#3818) must show every event that passed stage-one
//! (listener-level) filtering, tagged by listener, REGARDLESS of whether any
//! agent is bound to it — and let the user flip an event TYPE between
//! included/excluded without losing history. A plain append-only JSONL file
//! is the simplest structure that satisfies "durable, newest-first list,
//! survives restarts" for a single-node desktop harness; a sled/redb store
//! would be over-engineering for the event volumes an eventstream listener
//! demo produces (dozens, not millions, of rows).
//! What: [`StoredEvent`] is the normalized on-disk record. [`EventStore`]
//! appends events, reads them back (newest first), and persists per-
//! event-type `included` state to a sibling `filters.json`. Both live under
//! `~/.trusty-agents/events/`.
//! Test: `append_and_read_round_trips`, `dedup_seed_loads_recent_ids`,
//! `filter_toggle_persists_and_applies_default_included`,
//! `read_events_returns_newest_first`.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One normalized event surfaced by a listener onto the harness event bus.
///
/// Why: Every connector (Gmail today, Calendar/Slack later) emits wildly
/// different native payloads; the Events pane and the agent-wake path both
/// need one small, stable shape rather than connector-specific structs.
/// What: `id` is the STABLE IDEMPOTENT id used for dedup (DOC-54 §7.3.3) —
/// `"{listener_id}:{provider_message_id}"`. `from`/`subject`/`snippet` are
/// the summary fields DEADLINE-BUILD item 3 asks for; all `Option` since
/// not every future connector/event type has all three (Calendar events
/// have no `from`, for instance).
/// Test: `append_and_read_round_trips`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEvent {
    pub id: String,
    pub listener_id: String,
    pub provider: String,
    pub event_type: String,
    pub ts: String,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub snippet: Option<String>,
    /// Snapshot of the include/exclude state AT INGEST TIME. The
    /// authoritative, current state is `EventStore::is_included`
    /// (evaluated live from `filters.json` against `event_type`) — this
    /// field is a convenience for API responses that shouldn't need a
    /// second lookup, and is refreshed by `EventStore::read_events`.
    pub included: bool,
}

/// Root directory for listener event persistence: `~/.trusty-agents/events/`.
///
/// Why: Centralised so tests can override via `$HOME` exactly like
/// `GlobalConfig::config_path` does, and so every reader/writer agrees on
/// the layout.
pub fn events_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine $HOME directory")?;
    Ok(home.join(".trusty-agents").join("events"))
}

fn events_log_path() -> Result<PathBuf> {
    Ok(events_dir()?.join("events.jsonl"))
}

fn filters_path() -> Result<PathBuf> {
    Ok(events_dir()?.join("filters.json"))
}

/// Append-only event log + include/exclude filter state.
///
/// Why: A thin, stateless-between-calls wrapper (no in-memory cache) keeps
/// this correct across the polling engine (a background tokio task) and the
/// API handlers (request-scoped) writing/reading the SAME files without
/// coordinating through shared process state — the filesystem is the source
/// of truth. Read/write volumes here are low enough (demo-scale event
/// counts, on a local disk) that re-reading per call is not a measurable
/// cost, and it trades a small amount of I/O for zero cross-task locking
/// complexity, matching the code-reduction-first principle.
/// Test: `append_and_read_round_trips`.
pub struct EventStore;

impl EventStore {
    /// Append one event to the JSONL log. Creates `~/.trusty-agents/events/`
    /// if absent.
    ///
    /// Why: Append-only means a torn write can corrupt at most the LAST
    /// line, never earlier history — acceptable for a local demo store; a
    /// production-grade store would fsync + checksum, out of scope here.
    /// What: Serializes `event` as one JSON line, appended with `\n`.
    /// Test: `append_and_read_round_trips`.
    pub async fn append(event: &StoredEvent) -> Result<()> {
        let path = events_log_path()?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create events dir {}", parent.display()))?;
        }
        let mut line = serde_json::to_string(event).context("serialize StoredEvent")?;
        line.push('\n');
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .with_context(|| format!("failed to open events log {}", path.display()))?;
        file.write_all(line.as_bytes())
            .await
            .context("append event line")?;
        Ok(())
    }

    /// Read all events, newest first, with `included` refreshed from the
    /// current `filters.json` state (so a filter toggle retroactively
    /// re-labels past rows, matching the Events pane's "excluded rows stay
    /// visible, muted" spec — DOC-54 §7.4 / issue #3818).
    ///
    /// Why: `limit` bounds the response for a long-lived demo/dev process;
    /// `None` returns everything (fine at demo scale).
    /// What: Missing log file returns an empty `Vec`, never an error (a
    /// listener that has never fired yet is a valid, common state).
    /// Test: `read_events_returns_newest_first`,
    /// `filter_toggle_persists_and_applies_default_included`.
    pub async fn read_events(limit: Option<usize>) -> Result<Vec<StoredEvent>> {
        let path = events_log_path()?;
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let filters = Self::load_filters().await.unwrap_or_default();
        let mut events: Vec<StoredEvent> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| match serde_json::from_str::<StoredEvent>(l) {
                Ok(mut ev) => {
                    ev.included = is_included(&filters, &ev.event_type);
                    Some(ev)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "skipping malformed event log line");
                    None
                }
            })
            .collect();
        events.reverse(); // newest first
        if let Some(n) = limit {
            events.truncate(n);
        }
        Ok(events)
    }

    /// Seed a dedup set from the most recent `n` persisted event ids.
    ///
    /// Why: The polling engine's in-memory dedup cache (DOC-54 §7.3.3) must
    /// not replay events already appended by a PRIOR process run — seeding
    /// from the tail of the durable log on startup closes that gap cheaply
    /// without needing a separate dedup index file.
    /// What: Reads at most `n` most-recent ids (newest-first order from
    /// `read_events`).
    /// Test: `dedup_seed_loads_recent_ids`.
    pub async fn recent_ids(n: usize) -> Result<HashSet<String>> {
        let events = Self::read_events(Some(n)).await?;
        Ok(events.into_iter().map(|e| e.id).collect())
    }

    /// Load the persisted event-type -> included map. Missing file = empty
    /// map (every type defaults to included — see [`is_included`]).
    pub async fn load_filters() -> Result<std::collections::HashMap<String, bool>> {
        let path = filters_path()?;
        match tokio::fs::read_to_string(&path).await {
            Ok(raw) => {
                serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Set (and persist) the included/excluded state for one event type.
    ///
    /// Why: Backs `POST /api/listener-events/filter` — the Events pane's
    /// per-type include/exclude toggle (#3818).
    /// What: Read-modify-write of `filters.json`.
    /// Test: `filter_toggle_persists_and_applies_default_included`.
    pub async fn set_filter(event_type: &str, included: bool) -> Result<()> {
        let path = filters_path()?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create events dir {}", parent.display()))?;
        }
        let mut filters = Self::load_filters().await.unwrap_or_default();
        filters.insert(event_type.to_string(), included);
        let content = serde_json::to_string_pretty(&filters).context("serialize filters")?;
        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    /// Whether `event_type` is currently included, per persisted filter
    /// state (default `true` — new event types are included until a user
    /// explicitly excludes them).
    ///
    /// Why: Both the API list handler and the agent-wake stage-two check
    /// (`crate::listeners::wake`) need the SAME answer for "is this type
    /// currently on" — centralising avoids the two drifting.
    /// Test: `filter_toggle_persists_and_applies_default_included`.
    pub async fn is_event_type_included(event_type: &str) -> bool {
        let filters = Self::load_filters().await.unwrap_or_default();
        is_included(&filters, event_type)
    }
}

fn is_included(filters: &std::collections::HashMap<String, bool>, event_type: &str) -> bool {
    filters.get(event_type).copied().unwrap_or(true)
}

#[cfg(test)]
// Every test here holds `HOME_LOCK` across `.await` points DELIBERATELY —
// that's the whole point of the lock (serialize `$HOME`-mutating tests
// across the crate, see `crate::test_env`'s module doc). Matches the
// existing `#[allow(clippy::await_holding_lock)]` in
// `mcp::config::tests::mod` for the identical pattern.
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;
    use crate::test_env::HOME_LOCK;

    fn set_test_home(dir: &std::path::Path) {
        // SAFETY: caller holds `HOME_LOCK` for the duration of the test (see
        // `crate::test_env`'s module doc) so no other thread observes `HOME`
        // mid-mutation.
        unsafe {
            std::env::set_var("HOME", dir);
        }
    }

    fn sample_event(id: &str, event_type: &str) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            listener_id: "gmail-personal".to_string(),
            provider: "gmail".to_string(),
            event_type: event_type.to_string(),
            ts: "2026-07-24T12:00:00Z".to_string(),
            from: Some("someone@example.com".to_string()),
            subject: Some("Hello".to_string()),
            snippet: Some("snippet text".to_string()),
            included: true,
        }
    }

    #[tokio::test]
    async fn append_and_read_round_trips() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        set_test_home(tmp.path());
        let ev = sample_event("gmail-personal:msg1", "message.received");
        EventStore::append(&ev).await.unwrap();
        let events = EventStore::read_events(None).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "gmail-personal:msg1");
        assert_eq!(events[0].subject.as_deref(), Some("Hello"));
    }

    #[tokio::test]
    async fn read_events_returns_newest_first() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        set_test_home(tmp.path());
        EventStore::append(&sample_event("id1", "message.received"))
            .await
            .unwrap();
        EventStore::append(&sample_event("id2", "message.received"))
            .await
            .unwrap();
        let events = EventStore::read_events(None).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "id2", "newest appended must be first");
        assert_eq!(events[1].id, "id1");
    }

    #[tokio::test]
    async fn dedup_seed_loads_recent_ids() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        set_test_home(tmp.path());
        EventStore::append(&sample_event("id1", "message.received"))
            .await
            .unwrap();
        EventStore::append(&sample_event("id2", "message.received"))
            .await
            .unwrap();
        let ids = EventStore::recent_ids(10).await.unwrap();
        assert!(ids.contains("id1"));
        assert!(ids.contains("id2"));
    }

    #[tokio::test]
    async fn filter_toggle_persists_and_applies_default_included() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        set_test_home(tmp.path());
        // Default (no filters.json yet): included.
        assert!(EventStore::is_event_type_included("message.received").await);

        EventStore::set_filter("message.received", false)
            .await
            .unwrap();
        assert!(!EventStore::is_event_type_included("message.received").await);

        // A never-toggled type stays included.
        assert!(EventStore::is_event_type_included("event.created").await);

        // read_events reflects the retroactive exclusion.
        EventStore::append(&sample_event("id1", "message.received"))
            .await
            .unwrap();
        let events = EventStore::read_events(None).await.unwrap();
        assert!(!events[0].included);
    }
}
