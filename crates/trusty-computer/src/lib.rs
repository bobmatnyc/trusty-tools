//! trusty-computer: a broker between local AI agents and a bounded cua-driver daemon.
//!
//! Why: the cua-driver daemon holds full Accessibility rights, so agents must
//! reach the desktop only through a component that narrows the tool surface,
//! hides the app menu bar, audits every call and never re-runs a retried call
//! (`docs/plans/trusty-computer-phase2.md`, issue #11460).
//! What: [`policy`] classifies the 56 cua-driver 0.23.2 tools and checks
//! arguments; [`redact`] strips AXMenuBar subtrees and fingerprints a snapshot;
//! [`audit`] is the fail-closed JSONL log; [`journal`] answers a repeated
//! `request_id`. Phase 2 adds [`driver`] (the socket client), [`manifest`]
//! (the per-lease bounded manifest), [`launcher`] and [`supervisor`] (the
//! daemon lifecycle). Phase 3 adds [`broker`] (grants, lease, operator ops),
//! `broker_call` (the call pipeline), [`broker_server`] (the sockets),
//! [`grants`], [`lease`] and [`peer`] (the uid check). Phase 4 adds
//! [`consent`] (dialog seam, codes, strikes), [`broker_operator`] (the consent
//! flow and the lockout), [`app_identity`] (the pid's real bundle) and
//! [`operator_cli`]; `broker_unlock` persists the lockout across restarts. Phase 5 adds [`mcp`], the
//! `trusty-computer-mcp` server. Phase 6 adds [`codesign`], [`launchd`] and
//! [`service_cli`] (`install-service`, `uninstall-service`).
//! Test: `tests/spike_policy.rs`, `tests/spike_redact.rs`, `tests/spike_audit.rs`,
//! `tests/spike_journal.rs`, `tests/spike_supervisor.rs`, `tests/driver_client.rs`,
//! `tests/broker_spike.rs`, `tests/broker_lease.rs`, `tests/broker_fail_closed.rs`,
//! `tests/consent_fail_closed.rs`, `tests/mcp_server.rs`, `tests/install.rs`.

pub mod app_identity;
pub mod audit;
pub mod broker;
mod broker_agent;
mod broker_call;
pub mod broker_operator;
pub mod broker_server;
mod broker_unlock;
pub mod codesign;
pub mod consent;
pub mod driver;
pub mod grants;
pub mod journal;
pub mod launchd;
pub mod launcher;
pub mod lease;
pub mod manifest;
pub mod mcp;
pub mod operator_cli;
pub mod peer;
pub mod policy;
mod policy_rules;
pub mod private;
pub mod pyvalue;
pub mod redact;
pub mod serve;
pub mod service_cli;
pub mod supervisor;
