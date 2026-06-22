//! Project registry: models, on-disk persistence, lifecycle management, and
//! NL-to-project resolver (WI-5, #1517).
//!
//! Why: the project registry tracks known repositories so operators and driver
//! skills can reference projects by name rather than supplying a full URL on
//! every session spawn. It seeds from `config.yaml`, auto-registers projects
//! from session history at boot, exposes typed MCP tools, and now provides a
//! natural-language resolver that maps free-text queries to registered projects.
//! What: re-exports [`Project`], [`ProjectRegistry`], [`derive_name_from_url`],
//! and the error types from the four submodules:
//! - `record` — the [`Project`] struct and `derive_name_from_url`.
//! - `store`  — [`ProjectStore`] on-disk JSON persistence.
//! - `registry` — [`ProjectRegistry`] lifecycle manager.
//! - `resolver` — NL→project resolver, session↔project binding, fleet grouping.
//! Test: each submodule carries inline unit tests; run with
//! `cargo test -p trusty-mpm`.

pub mod record;
pub mod registry;
pub mod resolver;
pub mod store;

pub use record::{Project, derive_name_from_url};
pub use registry::ProjectRegistry;
pub use resolver::{
    DISAMBIGUATION_FLOOR, KEYWORD_COLLECTION_FLOOR, ProjectFleet, ProjectMatch, ProjectResolution,
    ResolutionReason, ResolverError, fleet_by_project, resolve_project, resolve_session_project,
};
pub use store::ProjectStoreError;
