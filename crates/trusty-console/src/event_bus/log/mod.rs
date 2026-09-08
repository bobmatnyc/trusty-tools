//! The durable, day-rotated NDJSON event log (issue #6848 slice 3b, DOC-73
//! §4.3).
//!
//! Why: slice 3 (PR #7152) shipped the in-memory ring only — a console
//! restart lost every event, and a viewer that reconnected mid-session had no
//! way to see what it missed. DOC-73 §4.3 calls for two retention tiers, the
//! ring and "a durable NDJSON file per day, written by console as each frame
//! is accepted"; this module is the second tier.
//! What: [`DurableLog`] is the handle [`super::bus::EventBus`] holds — a
//! bounded channel to a dedicated writer task, so a slow disk never blocks
//! ingest. [`LogConfig`] resolves the private directory
//! (`<console data dir>/event_log/`, hardened via
//! [`trusty_common::uds::prepare_socket_dir`], never a raw `create_dir_all`)
//! and the retention window. [`replay::ReplayItem`] is what a reconnecting
//! subscriber reads back: persisted events plus explicit
//! [`replay::ReplayItem::Gap`] markers wherever retention or a backpressure
//! drop left a hole. Submodules: `config` (paths, constants),
//! `error` (`LogError`), `format` (NDJSON encode + tolerant decode),
//! `recovery` (seq high-water mark + earliest-retained-seq on startup),
//! `retention` (day-file deletion), `writer` (`DurableLog` + the write task),
//! `replay` (replay-on-reconnect).
//! Test: `tests.rs`.

mod config;
mod error;
mod format;
mod recovery;
mod replay;
mod retention;
mod writer;

#[cfg(test)]
mod tests;

pub(crate) use config::LogConfig;
pub(crate) use error::LogError;
pub(crate) use replay::ReplayItem;
pub(crate) use writer::DurableLog;
