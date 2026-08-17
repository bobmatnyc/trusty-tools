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
//! - `worktree_policy` — the `worktree` opt-out decision, shared by the daemon
//!   and the out-of-process `tm` CLI (#3455, #4300). Since #5207 a project's
//!   own committed `.trusty-mpm.toml` outranks the machine-global registry;
//!   see [`worktree_enabled_for_project`]. The separate `agent_worktree` key
//!   (#5814) answers a different question — whether a DISPATCHED AGENT gets a
//!   worktree — through [`dispatched_agent_worktree_enabled`].
//! Test: each submodule carries inline unit tests; run with
//! `cargo test -p trusty-mpm`.
//!
//! [`Project`]: crate::project::Project
//! [`ProjectRegistry`]: crate::project::ProjectRegistry
//! [`derive_name_from_url`]: crate::project::derive_name_from_url
//! [`ProjectStore`]: crate::project::store::ProjectStore
//! [`worktree_enabled_for_project`]: crate::project::worktree_enabled_for_project
//! [`dispatched_agent_worktree_enabled`]: crate::project::dispatched_agent_worktree_enabled

pub mod record;
pub mod registry;
pub mod resolver;
pub mod store;
pub mod worktree_policy;

pub use record::{Project, derive_name_from_url};
pub use registry::ProjectRegistry;
pub use resolver::{
    DISAMBIGUATION_FLOOR, KEYWORD_COLLECTION_FLOOR, ProjectFleet, ProjectMatch, ProjectResolution,
    ResolutionReason, ResolverError, fleet_by_project, resolve_project, resolve_session_project,
};
pub use store::ProjectStoreError;
pub use worktree_policy::{
    dispatched_agent_worktree_enabled, registry_data_dir, registry_data_dir_under,
    worktree_enabled_for_origin, worktree_enabled_for_origin_at, worktree_enabled_for_project,
    worktree_enabled_in, worktree_override_in_project,
};
