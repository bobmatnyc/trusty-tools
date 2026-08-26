//! The live activity feed, and what to do when it stops (#6286).
//!
//! Why: the monitor's activity log polled `memory.activity` on a 2-second tick,
//! which was the stopgap the first pass at the UDS migration left behind when
//! it retired `/sse` with nothing in its place. An event showed up to two
//! seconds late, and one evicted from the activity log between two ticks was
//! never shown at all. `memory.activity_stream` pushes each event as it
//! happens.
//!
//! What: [`ActivityFeed::open`] dials the stream and drains it on a background
//! task into a bounded channel. The TUI's loop is a render tick, not an async
//! select, so it takes what has arrived with [`ActivityFeed::drain`] and never
//! blocks on the stream.
//!
//! **A dead stream is visible, and the caller falls back.** The daemon
//! restarting, the socket going away, or a terminal error frame all end the
//! stream; [`ActivityFeed::is_live`] goes false, the reason is on
//! [`ActivityFeed::last_error`], and the caller resumes polling
//! `memory.activity`. Blanking the log instead would present a live daemon as
//! an idle one.
//!
//! Test: `feed_drains_what_the_stream_pushed`,
//! `feed_reports_a_terminal_error_and_stops_being_live`,
//! `feed_open_fails_against_an_absent_socket`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::memory_rpc::MAX_FRAME_BYTES;
use crate::uds::send_framed_stream_request_capped;

use super::parsers::parse_memory_event;
use super::types::MemoryEvent;

/// The method the daemon serves this feed on.
const METHOD: &str = "memory.activity_stream";

/// How long to wait for the stream to open, and for each frame after that.
///
/// Why it is not the per-call [`super::types::REQUEST_TIMEOUT`]: that budget is
/// for a call that answers. This one bounds the DIAL and then each individual
/// frame read — and an idle daemon legitimately produces no frame for hours, so
/// a short per-frame budget would tear the stream down on a quiet system. Five
/// minutes is long enough to be quiet and short enough that a half-open socket
/// is eventually noticed.
const FRAME_TIMEOUT: Duration = Duration::from_secs(300);

/// How many events the feed buffers between two render ticks.
///
/// The TUI drains on every tick, so this only has to absorb a burst. 256
/// matches the daemon's own per-reader buffer, which means the two ends agree
/// about how far behind a reader may fall before anything is dropped.
const FEED_BUFFER: usize = 256;

/// A live subscription to the daemon's activity events.
///
/// Dropping it closes the channel, which ends the background task and the
/// stream with it.
#[derive(Debug)]
pub struct ActivityFeed {
    rx: mpsc::Receiver<MemoryEvent>,
    /// Set when the stream ends, with the reason. `None` while it is live.
    last_error: Arc<Mutex<Option<String>>>,
    live: Arc<std::sync::atomic::AtomicBool>,
}

impl ActivityFeed {
    /// Open the stream and start draining it.
    ///
    /// # Errors
    ///
    /// When the socket cannot be dialled or the daemon refuses to open the
    /// stream — which is what "the daemon is not running" and "this daemon
    /// predates the method" both look like. Either way the caller polls.
    ///
    /// Test: `feed_open_fails_against_an_absent_socket`.
    pub async fn open(socket: &Path) -> anyhow::Result<Self> {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": METHOD,
            "stream": true,
        });
        let mut stream = send_framed_stream_request_capped::<_, Value>(
            socket,
            &request,
            FRAME_TIMEOUT,
            MAX_FRAME_BYTES,
        )
        .await?;

        let (tx, rx) = mpsc::channel(FEED_BUFFER);
        let last_error = Arc::new(Mutex::new(None));
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let task_error = Arc::clone(&last_error);
        let task_live = Arc::clone(&live);

        tokio::spawn(async move {
            loop {
                match stream.next_frame().await {
                    Some(Ok(frame)) => {
                        // A `lagged` frame is the daemon telling this reader it
                        // missed events. It is not an activity event, so it does
                        // not go in the log — but it is recorded so the caller
                        // can say the feed has a hole rather than implying the
                        // daemon was quiet.
                        if frame.get("type").and_then(Value::as_str) == Some("lagged") {
                            let n = frame.get("lagged").and_then(Value::as_u64).unwrap_or(0);
                            *task_error.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(format!("activity feed lagged {n} events"));
                            continue;
                        }
                        let Some(event) = parse_memory_event(&frame) else {
                            // A shape this client does not know. Skipping one
                            // frame is right; ending the stream over it is not.
                            continue;
                        };
                        if tx.send(event).await.is_err() {
                            // The `ActivityFeed` was dropped.
                            break;
                        }
                    }
                    Some(Err(e)) => {
                        *task_error.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(format!("{e}"));
                        break;
                    }
                    None => {
                        *task_error.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some("the daemon closed the activity stream".to_string());
                        break;
                    }
                }
            }
            task_live.store(false, std::sync::atomic::Ordering::Relaxed);
        });

        Ok(Self {
            rx,
            last_error,
            live,
        })
    }

    /// Is the stream still delivering?
    ///
    /// `false` once it has ended, for any reason. The caller reads
    /// [`Self::last_error`] for which one and resumes polling.
    pub fn is_live(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Why the stream ended, or the last hole it reported.
    ///
    /// `Some` while [`Self::is_live`] is still true means the daemon reported a
    /// lag: the feed is working and some events were missed.
    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Take every event that has arrived since the last call, without blocking.
    ///
    /// Why not `recv().await`: the TUI's loop is a render tick that also reads
    /// the keyboard. Awaiting the next event there would stall the UI until the
    /// daemon happened to do something.
    pub fn drain(&mut self) -> Vec<MemoryEvent> {
        let mut out = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            out.push(event);
        }
        out
    }
}

#[cfg(test)]
#[path = "feed_tests.rs"]
mod tests;
