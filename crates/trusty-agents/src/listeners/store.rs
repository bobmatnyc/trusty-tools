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
///
/// (#3922 recurrence guard) `#[cfg(test)]`-only: panics if a test reaches
/// this `$HOME`-reading production path without holding
/// `crate::test_env::HOME_LOCK` for the duration of the call. Why: this
/// function used to be the ONLY seam `EventStore`'s tests had for
/// isolation, so every test that wanted its own directory had to mutate the
/// process-global `$HOME` under `HOME_LOCK` — but nothing stopped some
/// OTHER test elsewhere in the crate from mutating `$HOME` under a
/// DIFFERENT (or no) exclusion mechanism and racing it (issue #3922: a
/// `listeners::store` test lost an appended event to exactly this race
/// against `llm::http::tests`, which sandboxes `$HOME` under
/// `#[serial_test::serial]` alone — a `std::sync::Mutex` can't be held
/// across `.await`, so it never took `HOME_LOCK` at all — reproduced
/// locally at 2/5 under deliberately maximised overlap, matching the CI
/// failure's `ids.contains(..)` assertion shape byte-for-byte). The fix:
/// `EventStore::*_at(dir, ..)` now lets a test inject its directory
/// directly and never call this function at all — every test in THIS
/// module was migrated to that seam and no longer touches `$HOME`.
/// `listeners::poll`'s cursor tests and a few API/handler-level tests still
/// call the plain `$HOME`-sandboxing path directly (no `AppState`/handler
/// seam to inject through yet); they hold `HOME_LOCK` correctly, so this
/// guard passes for them (a held lock makes `try_lock` fail, i.e. "busy" —
/// exactly what this checks for). A held lock isn't proof the CURRENT
/// thread is the holder, only that the crate's exclusion convention was
/// followed; on the other hand `try_lock` succeeding removes ALL doubt —
/// nobody is honouring the convention right now, so this is exactly the
/// #3922 shape and must fail loud rather than silently race. Production
/// builds are entirely unaffected — this whole check compiles out.
/// Test: `listeners::store::tests::*` (migrated off `$HOME` entirely, so
/// never reach this branch); `listeners::poll::tests::*` (still sandbox via
/// `HOME_LOCK` and must keep passing under this guard).
pub fn events_dir() -> Result<PathBuf> {
    #[cfg(test)]
    if crate::test_env::HOME_LOCK.try_lock().is_ok() {
        panic!(
            "listeners::store::events_dir(): about to resolve $HOME in a test with \
             `crate::test_env::HOME_LOCK` NOT held (issue #3922) — either hold the lock for \
             the duration of this test (see `listeners::poll`'s cursor tests) or, preferably, \
             use `EventStore::*_at(dir, ..)` with an injected tempdir so this test never \
             touches the process-global $HOME at all (see `listeners::store::tests`)."
        );
    }
    let home = dirs::home_dir().context("could not determine $HOME directory")?;
    Ok(home.join(".trusty-agents").join("events"))
}

fn events_log_path_at(dir: &std::path::Path) -> PathBuf {
    dir.join("events.jsonl")
}

fn filters_path_at(dir: &std::path::Path) -> PathBuf {
    dir.join("filters.json")
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
    /// What: Resolves the write target via `events_dir()` (production:
    /// `$HOME`-derived; see that function's `#[cfg(test)]` guard, issue
    /// #3922) and delegates to [`Self::append_at`].
    /// Test: `append_and_read_round_trips`.
    pub async fn append(event: &StoredEvent) -> Result<()> {
        Self::append_at(&events_dir()?, event).await
    }

    /// `append`, with the target directory injected directly instead of
    /// resolved from `$HOME` (issue #3922).
    ///
    /// Why: `append`'s only prior seam for test isolation was mutating the
    /// process-global `$HOME` under `crate::test_env::HOME_LOCK` — but a
    /// lock only excludes callers that ALSO take it, and at least one other
    /// test module in this crate (`llm::http::tests`, which can't hold a
    /// `std::sync::Mutex` guard across `.await` and relies on `#[serial]`
    /// instead) mutates `$HOME` without ever taking `HOME_LOCK`, so the two
    /// groups could — and, reproduced locally, DID — race each other's
    /// writes into the wrong directory. Taking `dir` as a parameter removes
    /// the shared mutable global from the equation entirely for every
    /// caller that uses this seam: two concurrently-running calls with
    /// different `dir`s can never race, no lock required.
    /// What: identical body to the pre-#3922 `append`, just reading `dir`
    /// instead of re-deriving it from `events_dir()` each call.
    /// Test: every test in `listeners::store::tests` now calls this instead
    /// of sandboxing `$HOME`.
    pub(crate) async fn append_at(dir: &std::path::Path, event: &StoredEvent) -> Result<()> {
        let path = events_log_path_at(dir);
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
    /// What: Resolves the read target via `events_dir()` and delegates to
    /// [`Self::read_events_at`]. Missing log file returns an empty `Vec`,
    /// never an error (a listener that has never fired yet is a valid,
    /// common state).
    /// Test: `read_events_returns_newest_first`,
    /// `filter_toggle_persists_and_applies_default_included`.
    pub async fn read_events(limit: Option<usize>) -> Result<Vec<StoredEvent>> {
        Self::read_events_at(&events_dir()?, limit).await
    }

    /// `read_events`, with the source directory injected directly (issue
    /// #3922) — see [`Self::append_at`]'s docs for why this seam exists.
    pub(crate) async fn read_events_at(
        dir: &std::path::Path,
        limit: Option<usize>,
    ) -> Result<Vec<StoredEvent>> {
        let path = events_log_path_at(dir);
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let filters = Self::load_filters_at(dir).await.unwrap_or_default();
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
        Self::recent_ids_at(&events_dir()?, n).await
    }

    /// `recent_ids`, with the source directory injected directly (issue
    /// #3922) — see [`Self::append_at`]'s docs for why this seam exists.
    /// Test: `dedup_seed_loads_recent_ids`.
    pub(crate) async fn recent_ids_at(dir: &std::path::Path, n: usize) -> Result<HashSet<String>> {
        let events = Self::read_events_at(dir, Some(n)).await?;
        Ok(events.into_iter().map(|e| e.id).collect())
    }

    /// Load the persisted event-type -> included map. Missing file = empty
    /// map (every type defaults to included — see [`is_included`]).
    pub async fn load_filters() -> Result<std::collections::HashMap<String, bool>> {
        Self::load_filters_at(&events_dir()?).await
    }

    /// `load_filters`, with the source directory injected directly (issue
    /// #3922) — see [`Self::append_at`]'s docs for why this seam exists.
    pub(crate) async fn load_filters_at(
        dir: &std::path::Path,
    ) -> Result<std::collections::HashMap<String, bool>> {
        let path = filters_path_at(dir);
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
    /// What: Resolves the write target via `events_dir()` and delegates to
    /// [`Self::set_filter_at`].
    /// Test: `filter_toggle_persists_and_applies_default_included`.
    pub async fn set_filter(event_type: &str, included: bool) -> Result<()> {
        Self::set_filter_at(&events_dir()?, event_type, included).await
    }

    /// `set_filter`, with the target directory injected directly (issue
    /// #3922) — see [`Self::append_at`]'s docs for why this seam exists.
    pub(crate) async fn set_filter_at(
        dir: &std::path::Path,
        event_type: &str,
        included: bool,
    ) -> Result<()> {
        let path = filters_path_at(dir);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create events dir {}", parent.display()))?;
        }
        let mut filters = Self::load_filters_at(dir).await.unwrap_or_default();
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
        match events_dir() {
            Ok(dir) => Self::is_event_type_included_at(&dir, event_type).await,
            Err(_) => true, // matches `load_filters().unwrap_or_default()`'s prior fallback.
        }
    }

    /// `is_event_type_included`, with the source directory injected
    /// directly (issue #3922) — see [`Self::append_at`]'s docs for why this
    /// seam exists.
    pub(crate) async fn is_event_type_included_at(dir: &std::path::Path, event_type: &str) -> bool {
        let filters = Self::load_filters_at(dir).await.unwrap_or_default();
        is_included(&filters, event_type)
    }
}

fn is_included(filters: &std::collections::HashMap<String, bool>, event_type: &str) -> bool {
    filters.get(event_type).copied().unwrap_or(true)
}

#[cfg(test)]
// (issue #3922) Every test in this module now injects its own tempdir via
// `EventStore::*_at` instead of sandboxing the process-global `$HOME` — so
// none of them touch `crate::test_env::HOME_LOCK` at all, and none of them
// can race any OTHER test in the crate that mutates `$HOME` (with or
// without that lock). See `events_dir`'s doc comment for the CI failure
// this replaces and the local reproduction that pinned the mechanism.
mod tests {
    use super::*;

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
        let tmp = tempfile::tempdir().unwrap();
        let ev = sample_event("gmail-personal:msg1", "message.received");
        EventStore::append_at(tmp.path(), &ev).await.unwrap();
        let events = EventStore::read_events_at(tmp.path(), None).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "gmail-personal:msg1");
        assert_eq!(events[0].subject.as_deref(), Some("Hello"));
    }

    #[tokio::test]
    async fn read_events_returns_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        EventStore::append_at(tmp.path(), &sample_event("id1", "message.received"))
            .await
            .unwrap();
        EventStore::append_at(tmp.path(), &sample_event("id2", "message.received"))
            .await
            .unwrap();
        let events = EventStore::read_events_at(tmp.path(), None).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "id2", "newest appended must be first");
        assert_eq!(events[1].id, "id1");
    }

    /// (#3922) Previously flaked under parallel `cargo test` scheduling: this
    /// test sandboxed `$HOME` under `HOME_LOCK`, but `llm::http::tests`
    /// mutates `$HOME` under `#[serial_test::serial]` ALONE (it can't hold a
    /// `std::sync::Mutex` across `.await`), so the two never actually
    /// excluded each other — reproduced locally (2 failures in 5 runs under
    /// deliberately maximised overlap; see PR description for the exact
    /// repro command and captured failure text, which matched this test's
    /// own `ids.contains(..)` assertion). Injecting the tempdir directly
    /// removes `$HOME` from this test's execution entirely, so it can no
    /// longer be a party to that race regardless of what any other test in
    /// the crate does to the process environment.
    #[tokio::test]
    async fn dedup_seed_loads_recent_ids() {
        let tmp = tempfile::tempdir().unwrap();
        EventStore::append_at(tmp.path(), &sample_event("id1", "message.received"))
            .await
            .unwrap();
        EventStore::append_at(tmp.path(), &sample_event("id2", "message.received"))
            .await
            .unwrap();
        let ids = EventStore::recent_ids_at(tmp.path(), 10).await.unwrap();
        assert!(ids.contains("id1"));
        assert!(ids.contains("id2"));
    }

    #[tokio::test]
    async fn filter_toggle_persists_and_applies_default_included() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Default (no filters.json yet): included.
        assert!(EventStore::is_event_type_included_at(dir, "message.received").await);

        EventStore::set_filter_at(dir, "message.received", false)
            .await
            .unwrap();
        assert!(!EventStore::is_event_type_included_at(dir, "message.received").await);

        // A never-toggled type stays included.
        assert!(EventStore::is_event_type_included_at(dir, "event.created").await);

        // read_events reflects the retroactive exclusion.
        EventStore::append_at(dir, &sample_event("id1", "message.received"))
            .await
            .unwrap();
        let events = EventStore::read_events_at(dir, None).await.unwrap();
        assert!(!events[0].included);
    }

    /// (#3925) Standing regression guard for the #3922 race CLASS, not just
    /// the one instance: rather than a brute-force N-iteration loop (only
    /// catches the failure probabilistically, and only if it's `#[ignore]`d
    /// and run nightly it might catch it days late), this test
    /// DETERMINISTICALLY manufactures the exact attack shape that caused
    /// #3922 — a concurrent task mutating the process-global `$HOME` env
    /// var in a tight loop with NO synchronization at all, matching
    /// `llm::http::tests`'s `#[serial_test::serial]`-only pattern (it can't
    /// hold `HOME_LOCK`, a `std::sync::Mutex`, across `.await`) — running
    /// for the FULL duration of several concurrent `EventStore` scenarios
    /// that use the `_at` DI seam this fix introduced. Because `_at` never
    /// reads `$HOME`, every scenario below MUST see exactly its own ids no
    /// matter how aggressively `$HOME` churns concurrently; if a future
    /// change makes any `_at` method (or one of its callers) fall back to
    /// `events_dir()` again without an explicit override, this test starts
    /// failing on the FIRST run under this hammering, deterministically,
    /// rather than ~5% of the time.
    /// What: Holds `HOME_LOCK` for its own entire body (exactly like every
    /// other `$HOME`-touching test in this crate) so this test cannot
    /// itself become a NEW unguarded attacker against some other
    /// `HOME_LOCK`-respecting test — the hammering is fully internal to
    /// this test's own scope. Spawns a background task that rewrites
    /// `$HOME` on every `tokio::task::yield_now` while four scenarios each
    /// append 8 events to their OWN tempdir via `EventStore::append_at`,
    /// interleaved with `yield_now` points, then reads them back via
    /// `EventStore::recent_ids_at`. Asserts each scenario's id set is
    /// exactly its own 8 ids — nothing lost, nothing borrowed from a
    /// sibling.
    /// Test: itself.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn concurrent_event_store_scenarios_survive_home_env_hammering() {
        let _guard = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prev_home = std::env::var_os("HOME");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let hammer_stop = stop.clone();
        let hammer = tokio::spawn(async move {
            let mut i: u64 = 0;
            while !hammer_stop.load(std::sync::atomic::Ordering::Relaxed) {
                i += 1;
                // SAFETY: deliberately UNSYNCHRONIZED — this reproduces the
                // exact #3922 attack shape (a concurrent mutator that never
                // takes any lock). Held HOME_LOCK (above) keeps this whole
                // test's hammering from leaking into any OTHER test; the
                // scenarios below must survive it regardless.
                unsafe {
                    std::env::set_var("HOME", format!("/tmp/home-hammer-{i}"));
                }
                tokio::task::yield_now().await;
            }
        });

        async fn scenario(label: &'static str, n: usize) -> HashSet<String> {
            let tmp = tempfile::tempdir().unwrap();
            for i in 0..n {
                EventStore::append_at(
                    tmp.path(),
                    &sample_event(&format!("{label}-{i}"), "message.received"),
                )
                .await
                .unwrap();
                tokio::task::yield_now().await;
            }
            EventStore::recent_ids_at(tmp.path(), n).await.unwrap()
        }

        let (a, b, c, d) = tokio::join!(
            scenario("a", 8),
            scenario("b", 8),
            scenario("c", 8),
            scenario("d", 8),
        );

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        hammer.await.unwrap();

        // SAFETY: still holding HOME_LOCK; restoring pre-test HOME before
        // the guard drops at function end.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }

        for (label, ids) in [("a", a), ("b", b), ("c", c), ("d", d)] {
            assert_eq!(
                ids.len(),
                8,
                "scenario {label} lost or gained ids under $HOME hammering: {ids:?}"
            );
            for i in 0..8 {
                let want = format!("{label}-{i}");
                assert!(
                    ids.contains(&want),
                    "scenario {label} missing its own id {want}: {ids:?}"
                );
            }
        }
    }
}
