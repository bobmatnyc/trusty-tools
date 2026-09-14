//! SSE fan-out of the console event bus, with `Last-Event-ID` resume (issue
//! #6851, DOC-73 §4.4, slice 6).
//!
//! Why a module beside `event_bus` rather than inside it: `event_bus` owns what
//! is true — the ring, the durable log, the seq. This owns how one HTTP client
//! sees it, which is a different failure surface (a browser that reconnects, a
//! reader slower than the bus, a resume point older than anything retained).
//! Why not an extension of `machine_history::stream`: that stream carries host
//! samples with no resume contract at all, and merging the two would put a
//! resume bug and a graphing bug in one file (issue #6851's own framing).
//!
//! ## The wire contract
//!
//! | Frame | `id:` | Meaning |
//! |---|---|---|
//! | `ready` | no | The stream is live. Carries `next_seq`, `resumed_from`, `backfill` |
//! | `harness_event` | `<seq>` | One bus event, plus its `persisted` marker |
//! | `gap` | no | `(after_seq, before_seq)` will never be delivered |
//! | `lagged` | no | This reader fell behind the bus buffer by `dropped` events |
//! | `: heartbeat` | — | An SSE comment every 20 s, so an idle connection survives |
//!
//! Only a `harness_event` carries an `id:`, so a client's `Last-Event-ID` never
//! advances past data it did not receive. A reconnect with `Last-Event-ID: N`
//! yields exactly `N+1` onward, or a `gap` naming what the ring can no longer
//! reach — never a silent skip.
//!
//! Test: [`tests`] — resume across a forced disconnect, gap on an evicted
//! resume point, the healthy-empty and dead-bus response shapes, lag handling,
//! and the fail-closed `Last-Event-ID` parse.

pub(crate) mod frames;
pub(crate) mod route;
pub(crate) mod stream;

#[cfg(test)]
mod tests;

pub(crate) use route::{SSE_PATH, sse_handler};
