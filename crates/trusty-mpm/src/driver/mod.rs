//! Driver autonomy subsystem: T1–T4 tier policy + session↔artifact correlation.
//!
//! Why: the *driver* — the calling agentic process that operates trusty-mpm via
//! its HTTP API / `tm` CLI — must decide, for every `pending_decision` a managed
//! session surfaces, whether to auto-accept the proposed default or escalate to a
//! human. Doing that safely requires (a) a structured tier model so behavior is
//! predictable, and (b) hard, non-LLM guardrails so a subtly-wrong harness can
//! never auto-merge bad work. This module is the Rust home of that policy; the
//! tier *criteria* are documented in
//! `docs/trusty-mpm/spec/SESSION_MANAGER_DRIVER_AGENT.md` §4.
//! What: re-exports the autonomy policy ([`evaluate_autonomy_tier`],
//! [`AutonomyTier`], [`AutonomyDecision`], [`GuardrailSignals`], …) and the
//! session correlation types ([`SessionCorrelation`], [`ScopeCheck`]) so callers
//! import from one stable path (`trusty_mpm::driver::*`).
//! Test: each submodule carries its own pure unit tests; run
//! `cargo test -p trusty-mpm driver::`.

pub mod correlation;
pub mod disposition;
pub mod policy;

pub use correlation::{ScopeCheck, SessionCorrelation};
pub use disposition::{DispositionReason, NoIntentKind};
pub use policy::{
    ActionContext, AutonomyDecision, AutonomyTier, ChangeClass, CiStatus, Disposition,
    GuardrailSignals, PolicyError, ReviewVerdict, evaluate_autonomy_tier,
};
