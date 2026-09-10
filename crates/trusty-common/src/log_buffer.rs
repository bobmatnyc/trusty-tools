//! Bounded in-memory ring buffer of recent tracing log lines.
//!
//! Why: Operators debugging a running daemon want the last N log lines
//!      without SSHing to the box, tailing a file, or restarting with a
//!      different `RUST_LOG`. A small in-process ring buffer lets the daemon
//!      serve recent logs over HTTP (`GET /logs/tail`) at near-zero cost and
//!      with no file I/O. The cap keeps memory bounded on a long-running
//!      process.
//! What: [`LogBuffer`] is a thread-safe `VecDeque<String>` capped at a fixed
//!      capacity; the oldest line is evicted on overflow. [`LogBufferLayer`]
//!      is a `tracing_subscriber::Layer` that formats every event into one
//!      line and pushes it onto the buffer. The HTTP handler drains the tail.
//! Test: see the `tests` module — capacity eviction, tail semantics, and a
//!      layer-integration test that emits events through a real subscriber.
//!
//! [`LogBuffer`]: crate::log_buffer::LogBuffer
//! [`LogBufferLayer`]: crate::log_buffer::LogBufferLayer

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// Default ring-buffer capacity (lines). Sized so a daemon retains a few
/// minutes of INFO-level chatter while costing well under 1 MB of RAM.
pub const DEFAULT_LOG_CAPACITY: usize = 1000;

/// Thread-safe, bounded ring buffer of formatted log lines.
///
/// Why: shared between the tracing `Layer` (writer) and the HTTP handler
///      (reader); both hold cheap `Arc` clones of the same underlying deque.
/// What: wraps `Arc<Mutex<VecDeque<String>>>`. `push` appends and evicts the
///      oldest line past capacity; `tail` snapshots the most recent N lines.
/// Test: `capacity_evicts_oldest`, `tail_returns_last_n`.
#[derive(Clone, Debug)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<String>>>,
    capacity: usize,
}

impl LogBuffer {
    /// Create an empty buffer with the given line capacity.
    ///
    /// Why: callers (daemon startup) choose the cap; tests use a tiny one.
    /// What: allocates a `VecDeque` with `capacity.max(1)` reserved slots so a
    ///      zero capacity is treated as one (a zero-cap ring is useless and
    ///      would panic on the eviction arithmetic).
    /// Test: `capacity_evicts_oldest`.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    /// Append a line, evicting the oldest entry when at capacity.
    ///
    /// Why: a tracing `Layer` calls this on every event; it must never panic
    ///      or block long. A poisoned mutex (a prior panic while logging) is
    ///      recovered via `into_inner` so logging itself never cascades a
    ///      panic into the daemon.
    /// What: pushes `line` to the back; if length now exceeds `capacity`,
    ///      pops the front.
    /// Test: `capacity_evicts_oldest`.
    pub fn push(&self, line: String) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.push_back(line);
        while guard.len() > self.capacity {
            guard.pop_front();
        }
    }

    /// Snapshot the most recent `n` lines (or all, when `n` exceeds the
    /// current length).
    ///
    /// Why: the `/logs/tail` handler returns these as a JSON array. Cloning
    ///      under the lock keeps the critical section short and lets the
    ///      caller serialise without holding the mutex.
    /// What: returns a `Vec<String>` of at most `n` lines, oldest-first.
    /// Test: `tail_returns_last_n`, `tail_all_when_n_exceeds_len`.
    #[must_use]
    pub fn tail(&self, n: usize) -> Vec<String> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let skip = guard.len().saturating_sub(n);
        guard.iter().skip(skip).cloned().collect()
    }

    /// Total number of lines currently buffered.
    ///
    /// Why: the `/logs/tail` response reports `total` so callers can tell
    ///      whether the buffer has wrapped.
    /// What: returns the deque length.
    /// Test: `tail_returns_last_n` asserts `len` after pushes.
    #[must_use]
    pub fn len(&self) -> usize {
        match self.inner.lock() {
            Ok(g) => g.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Whether the buffer holds no lines.
    ///
    /// Why: clippy requires `is_empty` alongside `len`; also a convenient
    ///      readiness check in tests.
    /// What: returns `len() == 0`.
    /// Test: covered by `capacity_evicts_oldest`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Remove and return every buffered line, oldest first (#7390).
    ///
    /// Why: [`capture_logs`] reuses ONE buffer for the whole test binary, so it
    /// needs to empty it around each capture rather than read a tail whose age
    /// it cannot tell. Test-only: nothing in the daemon path empties the ring.
    /// What: drains the deque under the same poison-tolerant lock as
    /// [`LogBuffer::push`].
    /// Test: `capture_logs_sees_events_from_the_body_only`.
    #[cfg(test)]
    fn take(&self) -> Vec<String> {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.drain(..).collect()
    }
}

/// `tracing_subscriber::Layer` that mirrors every event into a [`LogBuffer`].
///
/// Why: wiring this layer into the subscriber means the daemon's normal
///      `tracing::info!` / `warn!` calls are captured for `/logs/tail` with
///      no extra call sites — the buffer stays in lock-step with stderr.
/// What: on each event, formats
///      `<YYYY-MM-DD HH:MM:SS> [<level> <target>] <message> k=v …` into a
///      single line and pushes it. The leading local-time timestamp (issue
///      #846) lets the dashboard log view show when each line was emitted.
///      Level/target/fields are collected via a lightweight `Visit`
///      implementation.
/// Test: `layer_captures_events` installs the layer on a real subscriber and
///      asserts an emitted event lands in the buffer.
pub struct LogBufferLayer {
    buffer: LogBuffer,
}

impl LogBufferLayer {
    /// Wrap a [`LogBuffer`] as a tracing layer.
    ///
    /// Why: the daemon constructs the buffer first (so it can also hand a
    ///      clone to its HTTP state) and then builds the layer around it.
    /// What: stores a clone of the buffer handle.
    /// Test: `layer_captures_events`.
    #[must_use]
    pub fn new(buffer: LogBuffer) -> Self {
        Self { buffer }
    }
}

/// Field visitor that accumulates an event's message and key/value fields
/// into a single human-readable string.
///
/// Why: tracing events expose their data only through the `Visit` callback;
///      we render it to text once so the buffer stores plain `String`s.
/// What: the canonical `message` field becomes the line body; every other
///      field is appended as ` key=value`.
/// Test: exercised indirectly by `layer_captures_events`.
struct LineVisitor {
    message: String,
    fields: String,
}

impl LineVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
            fields: String::new(),
        }
    }
}

impl Visit for LineVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // `{:?}` on the message preserves it without surrounding quotes
            // for string payloads in practice; use Display-ish formatting.
            let _ = write!(self.message, "{value:?}");
        } else {
            let _ = write!(self.fields, " {}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            let _ = write!(self.fields, " {}={value}", field.name());
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for LogBufferLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut visitor = LineVisitor::new();
        event.record(&mut visitor);
        // Trim the leading `"` artefact that `{:?}` adds for the message when
        // the payload was a quoted string literal — keep lines readable.
        let message = visitor.message.trim_matches('"');
        // Prepend a local-time timestamp (issue #846) so the dashboard log
        // view shows per-line timing. We use `chrono::Local` directly rather
        // than `tracing_subscriber::fmt::time::LocalTime`, which is unsound in
        // multithreaded programs.
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let line = format!(
            "{} [{} {}] {}{}",
            ts,
            meta.level(),
            meta.target(),
            message,
            visitor.fields
        );
        self.buffer.push(line);
    }
}

/// The one capture subscriber the crate's tests install, the buffer it writes
/// to, and the lock that gives one capturing test at a time exclusive use of it.
///
/// Why it is a static rather than a subscriber built per call: `tracing`'s
/// per-callsite interest and its global maximum level are recomputed, process
/// wide, every time a `Dispatch` is created — over the dispatchers alive at that
/// instant. A capture subscriber that lives only for one test is therefore racing
/// every other test that builds one, and the loser's callsites stay cached as
/// "no subscriber wants this" while its own subscriber is installed. A build of
/// this crate's suite failed once in six runs that way, with a `warn!` that ran
/// producing zero captured lines (#7390). One `Dispatch` that is created once and
/// never dropped is always in that recomputation, so the cached answer can no
/// longer go stale. This is the tracing-subscriber exception to the crate's
/// no-global-state rule, and it is compiled only into the test build.
#[cfg(test)]
static CAPTURE: std::sync::LazyLock<(tracing::Dispatch, LogBuffer, Mutex<()>)> =
    std::sync::LazyLock::new(|| {
        use tracing_subscriber::layer::SubscriberExt as _;

        let buffer = LogBuffer::new(256);
        let dispatch = tracing::Dispatch::new(
            tracing_subscriber::registry().with(LogBufferLayer::new(buffer.clone())),
        );
        (dispatch, buffer, Mutex::new(()))
    });

/// Run `body` under a subscriber that captures every event, and return the
/// captured lines alongside the body's value (#7365, #7390).
///
/// Why: the LEVEL a line carries is a behavioural contract in this crate — a
/// self-healing condition reported at warn reads to an operator as a fault
/// needing repair, which is the defect #7365 and #7390 each fixed. [`LogBuffer`]
/// already renders the level into the line, so a level regression becomes an
/// ordinary assertion instead of something only a human reading logs would
/// notice. It lives here, beside the buffer it drives, so the modules under test
/// share one capture rather than each growing a copy.
/// What: takes [`CAPTURE`]'s lock, empties its buffer, runs `body` with
/// [`CAPTURE`]'s dispatcher as this thread's default, and returns `body`'s value
/// with everything the buffer collected, oldest first. Only the locked thread
/// has that dispatcher as its default, so nothing another test logs concurrently
/// reaches these lines. A panicking `body` poisons the lock and the next caller
/// recovers it.
/// Test: `capture_logs_sees_events_from_the_body_only` below, plus
/// `search_index_reconcile.rs::{a_resolvable_conflict_is_logged_at_info_without_operator_advice,
/// an_unrecoverable_refusal_still_warns,
/// an_unanswered_create_the_registry_confirms_emits_no_warning}` and
/// `search_index_confirm.rs::an_exhausted_confirm_deadline_is_still_a_warning`.
#[cfg(test)]
pub(crate) fn capture_logs<T>(body: impl FnOnce() -> T) -> (T, Vec<String>) {
    let (dispatch, buffer, lock) = &*CAPTURE;
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = buffer.take();
    let value = tracing::dispatcher::with_default(dispatch, body);
    (value, buffer.take())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`capture_logs`] returns what the body logged, and only that (#7390).
    ///
    /// Why: every level assertion in this crate reads its evidence from here, so
    /// a capture that silently returned nothing would turn a "no warnings"
    /// assertion into one that passes for the wrong reason — which is exactly
    /// how the shared buffer's predecessor failed one run in six. Running it
    /// twice also pins the emptying: the second capture must not see the first
    /// one's line.
    /// What: captures one `warn!` and one `info!`, then captures again from a
    /// body that logs nothing.
    /// Test: itself.
    #[test]
    fn capture_logs_sees_events_from_the_body_only() {
        let ((), lines) = capture_logs(|| {
            tracing::warn!("first line");
            tracing::info!("second line");
        });

        assert_eq!(lines.len(), 2, "both events must be captured: {lines:?}");
        assert!(lines[0].contains("WARN") && lines[0].contains("first line"));
        assert!(lines[1].contains("INFO") && lines[1].contains("second line"));

        let ((), after) = capture_logs(|| {});
        assert!(
            after.is_empty(),
            "the buffer is emptied per capture, saw {after:?}"
        );
    }

    #[test]
    fn capacity_evicts_oldest() {
        let buf = LogBuffer::new(3);
        assert!(buf.is_empty());
        for i in 0..5 {
            buf.push(format!("line {i}"));
        }
        // Capacity 3 → only the last three survive.
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.tail(10), vec!["line 2", "line 3", "line 4"]);
    }

    #[test]
    fn tail_returns_last_n() {
        let buf = LogBuffer::new(100);
        for i in 0..10 {
            buf.push(format!("l{i}"));
        }
        assert_eq!(buf.len(), 10);
        assert_eq!(buf.tail(3), vec!["l7", "l8", "l9"]);
    }

    #[test]
    fn tail_all_when_n_exceeds_len() {
        let buf = LogBuffer::new(100);
        buf.push("only".to_string());
        assert_eq!(buf.tail(50), vec!["only"]);
        assert_eq!(buf.tail(0), Vec::<String>::new());
    }

    #[test]
    fn zero_capacity_treated_as_one() {
        let buf = LogBuffer::new(0);
        buf.push("a".to_string());
        buf.push("b".to_string());
        assert_eq!(buf.tail(10), vec!["b"]);
    }

    #[test]
    fn layer_captures_events() {
        use tracing_subscriber::layer::SubscriberExt;

        let buffer = LogBuffer::new(10);
        let subscriber = tracing_subscriber::registry().with(LogBufferLayer::new(buffer.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(answer = 42, "hello from test");
        });
        let lines = buffer.tail(10);
        assert_eq!(lines.len(), 1, "expected one captured line, got {lines:?}");
        let line = &lines[0];
        assert!(line.contains("hello from test"), "line was: {line}");
        assert!(line.contains("answer=42"), "line was: {line}");
        assert!(line.contains("INFO"), "line was: {line}");

        // Issue #846: every line is prefixed with a `YYYY-MM-DD HH:MM:SS `
        // local-time timestamp. Lock in the shape without over-fitting to a
        // specific clock value: 4-digit year, then '-', then a space-delimited
        // time component, with the level appearing after the timestamp.
        let bytes = line.as_bytes();
        assert!(
            bytes.len() >= 19,
            "line too short to hold a timestamp: {line}"
        );
        assert!(
            bytes[0..4].iter().all(u8::is_ascii_digit),
            "expected a 4-digit year prefix, line was: {line}"
        );
        assert_eq!(
            bytes[4], b'-',
            "expected '-' after the year, line was: {line}"
        );
        // The timestamp must come before the level/target bracket.
        let ts_end = line
            .find(" [")
            .expect("expected a ' [' after the timestamp");
        let ts = &line[..ts_end];
        assert_eq!(
            ts.len(),
            19,
            "timestamp should be exactly 'YYYY-MM-DD HH:MM:SS', got: {ts:?}"
        );
    }
}
