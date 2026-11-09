//! Connector adapter programs for trusty-okf import (SPEC-OKF-01 §2.15,
//! §3 (m) A, Appendix B "Acquire role", Appendix C).
//!
//! Why: authenticated acquisition runs in a separate program per source
//! kind that holds its own grant (ADR-0026). trusty-okf starts it, passes
//! exactly its caller's environment list, and links no connector or
//! credential code (B1, B6, B10). This crate is that program, kept out of
//! trusty-okf and out of the trusty-agents core, so the adapter's
//! credentials stay in the adapter.
//! What: [`server`] speaks the acquire transport (4-byte big-endian length
//! and a JSON header, [`wire`]) and the handshake, and answers `list` and
//! `fetch` requests one at a time; it exits on `shutdown` and on stdin EOF,
//! even mid-request. [`gmail`] and [`gdocs`] implement the two kinds over
//! `trusty_gworkspace::api::bulk` through [`google`], which builds the
//! `BaseClient` from the adapter's own token store and maps every API
//! failure to a protocol error code. [`config`] reads the adapter's grant
//! file. The binaries `okf-adapter-gmail` and `okf-adapter-gdocs` call
//! [`gmail::main`] and [`gdocs::main`].
//! Listing contract (from `trusty_okf::import::adapter`): every page sends
//! `complete` explicitly; a page cut at the adapter's own ceiling sends
//! `complete: false` with the page token it did not follow, so the host
//! never reads a truncated listing as whole.
//! Test: `crates/trusty-connectors/tests/conformance.rs` (the S9 criteria
//! against these binaries, Google stubbed), `server::tests`,
//! `gmail::tests`, `gdocs::tests`.
//! Slice S13 of #10746.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.15, §3 (m) A, §2.7.1 B10, Appendix B, Appendix C.

#![forbid(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod config;
pub mod gdocs;
pub mod gmail;
pub mod google;
pub mod server;
pub mod wire;
