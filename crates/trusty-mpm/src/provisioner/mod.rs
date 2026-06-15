//! Workspace provisioner for isolated agent session workspaces.
//!
//! Why: agent sessions must never share or collide with an operator's existing
//! project checkout; each session needs a clean, isolated workspace with its
//! own .mcp.json, .claude/settings.json, and deployed agents/skills.
//! What: the [`WorkspaceProvisioner`] clones a repo into
//! ~/.trusty-mpm/workspaces/<project>/<session-id>/, runs prepare_session,
//! and returns a [`PreparedWorkspace`] struct. The [`GitBackend`] trait seam
//! allows unit tests to substitute a [`FakeGitBackend`] without a real remote.
//! Test: unit tests in workspace.rs use FakeGitBackend; integration test
//! test_provision_real_repo uses a temp bare repo (marked #[ignore]).

pub mod workspace;

pub use workspace::{
    FakeGitBackend, GitBackend, PreparedWorkspace, ProvisionError, RealGitBackend,
    WorkspaceProvisioner,
};
