//! Google Sheets service.
//!
//! Why: Sheets v4 surface is large; agent workflows need get/create/read
//! values/write values plus structured formatting and charts — which the
//! `core`, `formatting`, and `charts` sub-modules cover at parity with the
//! Python upstream.
//! What: Re-exports the `core`, `formatting`, and `charts` sub-modules.
//! Test: Per-sub-module.

pub mod charts;
pub mod core;
pub mod formatting;
