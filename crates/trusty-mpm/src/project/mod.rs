//! Project registry: models, on-disk persistence, and lifecycle management.
//!
//! Why: the project registry tracks known repositories so operators and driver
//! skills can reference projects by name rather than supplying a full URL on
//! every session spawn. It seeds from `config.yaml`, auto-registers projects
//! from session history at boot, and exposes typed MCP tools.
//! What: re-exports [`Project`], [`ProjectRegistry`], [`derive_name_from_url`],
//! and the error types from the three submodules:
//! - `record` — the [`Project`] struct and `derive_name_from_url`.
//! - `store`  — [`ProjectStore`] on-disk JSON persistence.
//! - `registry` — [`ProjectRegistry`] lifecycle manager.
//! Test: each submodule carries inline unit tests; `record`, `store`, and
//! `registry` tests are run by `cargo test -p trusty-mpm`.

pub mod record;
pub mod registry;
pub mod store;

pub use record::{Project, derive_name_from_url};
pub use registry::ProjectRegistry;
pub use store::ProjectStoreError;
