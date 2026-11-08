//! Build pipeline: source registry, ledger, ingest engine, docstore fetcher,
//! read policy, trust labels, index journal and rebuild (SPEC-OKF-01 §2.9,
//! §4.5). The caller passes every index ID; this crate names none.
//!
//! Filled by slice S4 of #10746.
