//! Build pipeline: source registry, ledger, ingest engine, docstore fetcher,
//! read policy and trust labels (SPEC-OKF-01 §2.7.2, §2.9, §2.13.1). The
//! caller passes every root and every index ID; this crate names none.
//!
//! Why: an OKG build and an import are the same operation with different
//! callers (§2.13): a registered source's items are written into a bundle,
//! idempotently, behind a per-source ledger. The generic half of trusty-kb's
//! builder moves here so trusty-agents keeps only agent glue (B1-B6).
//! What: slice S4 of #10746 moves [`registry`], [`ledger`], [`jsonl`],
//! [`ingest`], [`docstore`], [`policy`] and [`trust`] from trusty-kb. The
//! registry's connector rows become [`registry::Locator::Connector`] (B6);
//! the ingest engine writes through `Bundle::put`, keeps editorial and
//! verification keys across a re-ingest (§2.9) and quarantines protected keys
//! (§2.13.1). #10746 S5: an ingest run writes `log.md` once (`LogBatch`);
//! the machine-local index journal lives in `state::index`. `rebuild` comes
//! with S6; the store entry points stay in trusty-kb until S11.
//! Test: `golden_registry_loads_every_row`,
//! `golden_registry_saves_byte_for_byte`,
//! `golden_ledger_reads_and_appends_in_the_same_format`,
//! `c116_protected_keys_from_every_input_are_quarantined`,
//! `c117_planted_verification_hash_never_verifies`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.7.2 (module mapping), §2.9, §2.13.1, §6.2 criteria 13,
//!   116, 117.

pub mod docstore;
pub mod ingest;
pub mod jsonl;
pub mod ledger;
pub mod policy;
pub mod registry;
pub mod trust;
