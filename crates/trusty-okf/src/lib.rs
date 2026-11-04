//! trusty-okf — builds and maintains OKF (Open Knowledge Format) bundles.
//!
//! Why: OKF bundle code lived in `trusty-kb` mixed with agent glue and an MCP
//! server. SPEC-OKF-01 §2.7 (ADR-OKF-0001, amending ADR-0041) moves the
//! generic code here behind a boundary: every entry point takes a bundle root,
//! the crate knows no agent layout, holds no credential, has no MCP surface,
//! and its core is network-free.
//!
//! What: the module tree of SPEC-OKF-01 §4.1. This is the S1 scaffold of epic
//! #10746: each module is empty and its doc states what it will hold.
//!   - [`format`], [`profile`], [`bundle`], [`build`], [`state`], [`import`],
//!     [`extract`] — the network-free core.
//!   - `feed` and `retrieve` — behind the cargo feature `search-feed`.
//!   - `extract::worker` — behind the cargo feature `extract-worker`.
//!
//! Test: `scripts/check_okf_boundary.sh` checks boundary rules B1-B10 and
//! criteria 11-15 (§6.2); `scripts/check_okf_boundary_selftest.sh` proves it
//! can fail. Criteria 16 and 17 need the trusty-agents cut-over and are
//! checked in slice S11.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.7.1 (boundary rules), §4.1 (module layout), §6.2 (criteria
//!   11-17), held in the trusty-okf project directory.

#![deny(rustdoc::broken_intra_doc_links)]
#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod build;
pub mod bundle;
pub mod extract;
pub mod format;
pub mod import;
pub mod profile;
pub mod state;

#[cfg(feature = "search-feed")]
pub mod feed;
#[cfg(feature = "search-feed")]
pub mod retrieve;
