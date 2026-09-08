//! Git operations behind the framework-catalog sync.
//!
//! Why: this module used to own the session-workspace provisioner. ADR-0055
//! (#6000) removed it — trusty-mpm clones no repository and creates no worktree
//! for a session, and a `session_new` `repo_url` that is not already a local
//! directory is refused by
//! [`crate::core::local_repo_url::require_local_repo_url`]. The [`GitBackend`]
//! seam survives because `content::catalog_sync` still clones and refreshes the
//! framework catalog through it.
//! What: re-exports [`GitBackend`], [`RealGitBackend`], the [`FakeGitBackend`]
//! test double, and [`ProvisionError`].
//! Test: unit tests in `workspace/tests.rs` plus the catalog-sync tests.
//!
//! [`GitBackend`]: crate::provisioner::GitBackend
//! [`FakeGitBackend`]: crate::provisioner::FakeGitBackend
//! [`RealGitBackend`]: crate::provisioner::RealGitBackend
//! [`ProvisionError`]: crate::provisioner::ProvisionError

mod clone_progress;
pub mod workspace;

pub use workspace::{FakeGitBackend, GitBackend, ProvisionError, RealGitBackend};
