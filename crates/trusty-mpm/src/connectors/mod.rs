//! tm's [`trusty_agents_common::connectors::WorkstreamConnector`]
//! implementation (DOC-44 twin Phase 1, issue #3007).
//!
//! Why: DOC-44 §5.2 assigns the tm Connector to `trusty-mpm`, wrapping the
//! daemon's existing managed-session HTTP surface rather than duplicating
//! its logic. This is a NEW sibling module — `client/proxy.rs` (523 lines,
//! near the workspace's 500-SLOC production cap) is deliberately left
//! untouched; `SessionProxy`/`ManagedBackend` also has no create/list
//! operations to build on, so this module talks to the daemon's
//! `/api/v1/sessions/managed*` HTTP routes directly instead.
//! What: [`tm::TmConnector`] — see that module's docs for the full mapping
//! from trait methods to daemon routes.
//! Test: `cargo test -p trusty-mpm connectors::` — see `tm_tests.rs`.

pub mod tm;

pub use tm::TmConnector;
