//! Per-index file-change event feed (#6524).
//!
//! Why: the file watcher applies every change it sees and then discards it, so
//! nothing could answer "what has changed in this repo lately". An operator
//! watching an index — and the console UI built on this — needs that recent
//! history, and a paused embedding stage makes it more useful still: the feed
//! shows what work is piling up behind the pause.
//!
//! What: [`FileEventFeed`] is a bounded ring of the last
//! [`FILE_EVENT_CAPACITY`] events plus a broadcast channel. Writers call
//! [`FileEventFeed::record`]; `search.index.file_events` opens through
//! [`FileEventFeed::subscribe_with_replay`], which hands back the ring and a
//! live subscription taken under ONE lock hold — the exactly-once open #6386
//! established for the reindex stream, for the same reason.
//!
//! The ring is IN-MEMORY ONLY. Nothing persists it and a daemon restart starts
//! it empty.
//!
//! Test: `file_events_tests` at the foot of this file;
//! `service::rpc::streams_tests` drives the stream over the socket.

use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

/// How many events one index's ring holds. The oldest is dropped on overflow.
pub const FILE_EVENT_CAPACITY: usize = 200;

/// Live-subscriber capacity. A consumer slower than this sees a `lag` frame
/// rather than silently missing events, matching the reindex stream.
const BROADCAST_CAPACITY: usize = 256;

/// What the watcher observed.
///
/// Why: the three arms of `service::watch_loop`'s event match, named on the
/// wire so a consumer can style them without parsing prose.
/// What: serialised snake_case — `modified`, `removed`, `rescan`.
/// Test: `event_kinds_serialise_in_snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEventKind {
    /// A file was created or written.
    Modified,
    /// A file was deleted.
    Removed,
    /// The OS dropped an unknown batch of events and the tree was reconciled
    /// from disk. No single path is implicated, so `path` is `"."`.
    Rescan,
}

/// One observed change.
///
/// `path` is relative to the index root — the same key the reindex walker
/// stores, computed by `service::watch_loop::watcher_relative_path`, so a
/// consumer can match a feed row against a search result's `file`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileEvent {
    /// Repo-root-relative path, or `"."` for a `Rescan`.
    pub path: String,
    /// What happened.
    pub kind: FileEventKind,
    /// Wall-clock time the daemon recorded it, in Unix milliseconds.
    pub at_unix_ms: u64,
}

/// One index's bounded ring of recent file changes, plus its live channel.
///
/// Why: two consumers with different needs — a UI opening cold wants the recent
/// history, and one already open wants the next event as it lands. Serving both
/// from one structure is what keeps them consistent; serving them from two
/// would reintroduce the #6386 straddle, where an event lands in both or in
/// neither.
/// What: a `VecDeque` capped at [`FILE_EVENT_CAPACITY`] behind a mutex, and a
/// broadcast sender. Every append and every open take that one lock, so the two
/// critical sections are ordered and each event reaches exactly one of the two
/// paths for any given subscriber.
/// Test: `the_ring_keeps_the_newest_events_in_order`,
/// `an_event_either_side_of_an_open_arrives_on_exactly_one_path`.
#[derive(Debug)]
pub struct FileEventFeed {
    ring: Mutex<VecDeque<FileEvent>>,
    sender: broadcast::Sender<FileEvent>,
}

impl Default for FileEventFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl FileEventFeed {
    /// An empty feed with a fresh broadcast channel.
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            ring: Mutex::new(VecDeque::with_capacity(FILE_EVENT_CAPACITY)),
            sender,
        }
    }

    /// Append one event and broadcast it, as one atomic step.
    ///
    /// Why: the append and the broadcast share the ring's lock so no
    /// [`subscribe_with_replay`](Self::subscribe_with_replay) can land between
    /// them. Without that, an event whose write and broadcast straddled an open
    /// would be delivered twice or not at all.
    /// What: evicts the oldest entry once at capacity, pushes, then sends.
    /// `broadcast::Sender::send` never blocks on a receiver and its `Err` (no
    /// receivers) is benign — the ring still holds the event for the next open.
    /// Test: `the_ring_keeps_the_newest_events_in_order`.
    pub async fn record(&self, kind: FileEventKind, path: impl Into<String>) {
        let event = FileEvent {
            path: path.into(),
            kind,
            at_unix_ms: now_unix_ms(),
        };
        let mut ring = self.ring.lock().await;
        if ring.len() >= FILE_EVENT_CAPACITY {
            ring.pop_front();
        }
        ring.push_back(event.clone());
        let _ = self.sender.send(event);
    }

    /// Open a stream: the ring snapshot and a live subscription, taken as one
    /// atomic step.
    ///
    /// Why: the ONE place a file-events stream may open, for the same reason
    /// `ReindexProgress::subscribe_with_replay` is (#6386). Snapshotting and
    /// subscribing as two statements lets an event land in both or in neither.
    /// What: takes the ring lock, clones the deque into a `Vec`, subscribes, and
    /// releases the lock only once both are in hand.
    /// Test: `an_event_either_side_of_an_open_arrives_on_exactly_one_path`.
    pub async fn subscribe_with_replay(&self) -> (Vec<FileEvent>, broadcast::Receiver<FileEvent>) {
        let ring = self.ring.lock().await;
        (ring.iter().cloned().collect(), self.sender.subscribe())
    }
}

/// Wall-clock milliseconds since the Unix epoch, `0` if the clock is before it.
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Shared handle type used on `IndexHandle` and by the watcher.
pub type SharedFileEventFeed = Arc<FileEventFeed>;

#[cfg(test)]
mod file_events_tests {
    use super::*;

    /// 201 events leave the last 200, oldest-first.
    ///
    /// Why: the ring is the whole bound on this feature's memory. A ring that
    /// grew, or that dropped from the wrong end, would either leak on a busy
    /// repo or show an operator the wrong 200 changes.
    /// What: records 201 numbered paths, then asserts the replay is exactly 200
    /// long, starts at `f1` (not `f0`), ends at `f200`, and is in record order.
    /// Test: this test.
    #[tokio::test]
    async fn the_ring_keeps_the_newest_events_in_order() {
        let feed = FileEventFeed::new();
        for i in 0..=FILE_EVENT_CAPACITY {
            feed.record(FileEventKind::Modified, format!("src/f{i}.rs"))
                .await;
        }
        let (replay, _live) = feed.subscribe_with_replay().await;
        assert_eq!(replay.len(), FILE_EVENT_CAPACITY);
        assert_eq!(replay[0].path, "src/f1.rs", "the oldest event is dropped");
        assert_eq!(replay[FILE_EVENT_CAPACITY - 1].path, "src/f200.rs");
        for (i, event) in replay.iter().enumerate() {
            assert_eq!(event.path, format!("src/f{}.rs", i + 1));
        }
    }

    /// Paths are stored verbatim, so a relative key stays relative.
    #[tokio::test]
    async fn a_relative_path_is_stored_unchanged() {
        let feed = FileEventFeed::new();
        feed.record(FileEventKind::Removed, "src/auth/mod.rs").await;
        let (replay, _live) = feed.subscribe_with_replay().await;
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].path, "src/auth/mod.rs");
        assert_eq!(replay[0].kind, FileEventKind::Removed);
        assert!(replay[0].at_unix_ms > 0, "a real timestamp is stamped");
    }

    /// The three kinds serialise as the documented wire strings.
    #[test]
    fn event_kinds_serialise_in_snake_case() {
        let json = serde_json::to_value(FileEvent {
            path: "src/lib.rs".into(),
            kind: FileEventKind::Modified,
            at_unix_ms: 42,
        })
        .expect("a FileEvent always serialises");
        assert_eq!(json["kind"], "modified");
        assert_eq!(json["path"], "src/lib.rs");
        assert_eq!(json["at_unix_ms"], 42);
        for (kind, wire) in [
            (FileEventKind::Modified, "modified"),
            (FileEventKind::Removed, "removed"),
            (FileEventKind::Rescan, "rescan"),
        ] {
            assert_eq!(serde_json::to_value(kind).expect("enum serialises"), wire);
        }
    }

    /// An event pushed either side of an open lands on exactly one path.
    ///
    /// Why: the #6386 argument, restated for this feed — the open and the
    /// record share one lock, so an event is either in the returned ring (and
    /// its broadcast predates the subscription) or absent from it (and the
    /// subscription predates its broadcast). Never both, never neither.
    /// What: records `before`, opens, records `after`, and asserts `before` is
    /// only in the replay and `after` is only on the live channel.
    /// Test: this test.
    #[tokio::test]
    async fn an_event_either_side_of_an_open_arrives_on_exactly_one_path() {
        let feed = FileEventFeed::new();
        feed.record(FileEventKind::Modified, "before.rs").await;
        let (replay, mut live) = feed.subscribe_with_replay().await;
        feed.record(FileEventKind::Modified, "after.rs").await;

        let replayed: Vec<&str> = replay.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(replayed, vec!["before.rs"]);

        let streamed = live.recv().await.expect("the live event is delivered");
        assert_eq!(streamed.path, "after.rs");
        assert!(
            live.try_recv().is_err(),
            "the replayed event must not also arrive live"
        );
    }
}
