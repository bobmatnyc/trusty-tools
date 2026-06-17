//! Intent-source resolver (ISR) — shared resolver for the conformance gates.
//!
//! # Spec References
//!
//! - [`SPEC-CONFORMANCE-03~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-03~draft)
//!
//! Why: the intent/method conformance capability (DOC-15) ships two gates — a
//! FRONT gate in trusty-mpm (pre-work) and a BACK gate in trusty-review
//! (post-implementation). Both must resolve "what method did the ticket and the
//! spec prescribe?" and apply the same precedence (ticket > spec). Implementing
//! that resolution **once**, in the crate both gates already depend on, is the
//! only placement that guarantees the two gates can never disagree (spec §6.5,
//! goal G4) and avoids a new cross-crate edge.
//!
//! What: the ISR resolves a [`IntentQuery`] (a PR or a ticket-id) into a single
//! [`ResolvedIntent`] carrying the ticket method, the spec method, the
//! precedence winner, and the `conflict`/`stale_spec` flags. Resolution is
//! **fail-open**: any fetch/parse failure yields
//! [`ResolvedIntent::unresolved`], never a panic or `Err` (spec §4.2). The
//! crate is a library, so errors are a `thiserror` enum ([`IsrError`]) and
//! there are no `unwrap()`s on fetch/parse paths.
//!
//! Submodules (one concept each, to stay under the 500-SLOC cap):
//! - [`types`]    — the data contract (`IntentQuery`, `ResolvedIntent`, …).
//! - [`linkage`]  — PR → ticket extraction (tga regexes lifted, spec §6.3).
//! - [`spec_resolve`] — SLD docstring-ref → spec section (spec §6.4; C4 hardens).
//! - [`extract`]  — method extraction (the OQ-1 seam; heuristic default).
//! - [`resolve`]  — the `resolve` entry point + precedence + auth seams.
//! - [`backend_fetcher`] — production `TicketFetcher`/`SpecLookup` defaults.
//!
//! Test: `super::tests` (inline `#[cfg(test)] mod tests`) covers AC-1..AC-7 —
//! all four precedence cases, fail-open, PR→ticket linkage, the no-SLD-ref gap,
//! and the stale-spec conflict flag — without any network access.

pub mod backend_fetcher;
pub mod extract;
pub mod linkage;
pub mod resolve;
pub mod spec_resolve;
pub mod types;

#[cfg(test)]
mod tests;

// ── Public facade re-exports (callers import from `intent_source::*`) ─────────

pub use extract::{HeuristicMethodExtractor, MethodExtractor, heuristic_method};
pub use linkage::{extract_pr_ticket, extract_ticket_id, is_ticketed};
pub use resolve::{
    EnvTokenResolver, IntentTokenResolver, SpecLookup, TicketData, TicketFetcher, apply_precedence,
    resolve, resolve_default,
};
pub use types::{
    ChangedFile, IntentQuery, IsrError, Method, MethodKind, Precedence, ResolvedIntent, SpecRef,
    TicketRef,
};
