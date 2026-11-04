//! Agent-side KB glue over trusty-okf: the parts of the retired trusty-kb
//! crate that name an agent, a connector kind or the agent's index feed.
//!
//! Why: #10746 S11 deletes trusty-kb. Its generic code already lives in
//! trusty-okf (format, profile, bundle, build). What is left is agent glue,
//! which SPEC-OKF-01 §2.7.1 (B3, B4, B6) bars from trusty-okf: the
//! `<knowledge_dir>/<slug(agent)>` root mapping, the typed `gmail` and
//! `drive` registry kinds, and the in-tree index journal that this crate's
//! own index feed (`crate::stores::index_feed`) reads and writes.
//!
//! What:
//!   - [`roots`]  — [`roots::Roots`] (agent name to tree) and the
//!     [`roots::assert_within`] symlink backstop.
//!   - [`store`]  — [`store::KbStore`], one tree root plus its profile, with
//!     the confined entity paths and reads the OKG tools need.
//!   - [`okg`]    — the store-level OKG entry points (register, sources,
//!     ingest, index coverage) over `trusty_okf::build`, the typed registry
//!     view and the index journal.
//!
//! Every ingest, ledger and registry write goes through trusty-okf; this
//! module adds no second engine. The dependency edge is trusty-agents ->
//! trusty-okf only (B1, B2).
//!
//! Test: the unit tests in each submodule, `tests/kb_okg_artifacts.rs`, and
//! the `crate::tools::okg` and `crate::stores` suites that drive it.
//! Spec: SPEC-OKF-01 §2.7.1 (boundary rules) and §2.7.2 (module mapping).

pub mod okg;
pub mod roots;
pub mod store;
mod store_reconcile;
