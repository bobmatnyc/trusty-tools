//! `WorkstreamConnector` — the DOC-44 "engineering lead / virtual twin"
//! architecture's Layer 1 (issue #3007, twin Phase 1).
//!
//! Why: DOC-44 (`docs/specs/DOC-44-engineering-lead-twin-orchestration.md`)
//! defines a unified cross-tool orchestration layer that supervises
//! workstreams spanning both `tm` (trusty-mpm) and `tcode` (trusty-code).
//! **At the time this module landed, DOC-44 was still an OPEN, unmerged
//! design PR (#3006)** — the spec is design input, not a ratified contract;
//! the issue that requested this module (#3007) titled it "DOC-42 Phase 1",
//! but `DOC-42` is already claimed by `agent-bundled-skills.md` (shipped in
//! tm 0.19.22) — the correct id is **DOC-44** (see DOC-44's own catalog note
//! in `docs/specs/README.md`). This module implements exactly DOC-44 §5.2's
//! Phase 1 scope (§8: "Define `WorkstreamConnector` trait", "Implement tm
//! Connector", "Implement tcode Connector", "Integration test both") — no
//! Workstream Ledger (Phase 2), Audit Log (Phase 3), Confidence Gate (Phase
//! 4), or Lead/Liaison agents (Phase 5) are built here.
//! What: [`WorkstreamConnector`] (the trait), its request/response value
//! types ([`CreateSessionReq`], [`BackendParams`], [`SessionInfo`],
//! [`SessionStatus`], [`AttachHandle`], [`AgentSpec`], [`DelegateHandle`]),
//! [`ConnectorError`] (the typed failure surface), and [`ConnectorTestKit`]
//! (shared conformance assertions). The two concrete implementations live in
//! their owning crates — `trusty_mpm::connectors::TmConnector`
//! (`crates/trusty-mpm/src/connectors/tm.rs`) and
//! `trusty_code::session::connector::TcodeConnector`
//! (`crates/trusty-code/src/session/connector.rs`) — because this crate is a
//! dependency leaf both `trusty-mpm` and `trusty-code` depend on, and adding
//! `reqwest`/daemon-specific transport code here would push that weight onto
//! every consumer of this crate, including ones that never touch a
//! connector.
//! Test: `cargo test -p trusty-agents-common connectors::` exercises every
//! submodule in place against a minimal mock; the two real backends are
//! covered by their owning crates' test suites (see each module's doc
//! pointers above).

mod connector;
mod error;
mod test_kit;
mod types;

pub use connector::WorkstreamConnector;
pub use error::ConnectorError;
pub use test_kit::ConnectorTestKit;
pub use types::{
    AgentSpec, AttachHandle, BackendParams, CreateSessionReq, DelegateHandle, SessionInfo,
    SessionStatus,
};
