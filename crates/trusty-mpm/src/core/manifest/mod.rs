//! Manifest-driven harness provisioning (HR-2 / DOC-17).
//!
//! Why: before HR-2, `prepare_session` provisioned a harness from compile-time
//! bundled assets alone — there was no declarative, overridable description of
//! *what* a harness gets. This module introduces a [`HarnessManifest`] describing
//! the agent set, skills, instruction layers, output style, MCP servers, and
//! model tiers a harness receives, plus the NORMATIVE precedence resolution
//! (project override > user config > catalog manifest > compiled-in default)
//! that `prepare_session` consumes. The compiled-in default reproduces today's
//! provisioning exactly, so an absent manifest is a zero-regression no-op.
//! What: a thin facade re-exporting the schema ([`HarnessManifest`] and its
//! sections), the compiled-in [`default_manifest`], and the precedence
//! [`resolve_manifest`] over [`ManifestSources`]. Submodules keep each concern
//! under the 500-SLOC cap.
//! Test: each submodule carries its own unit tests; the `prepare_session`
//! integration lives in `core/session_launch/tests.rs`.

mod apply;
mod default;
mod project_lang;
mod resolve;
mod schema;

pub use apply::HarnessPlan;
pub use default::default_manifest;
pub use resolve::{MANIFEST_FILE, ManifestSources, resolve_manifest};
pub use schema::{
    AgentSet, ContentSource, HarnessManifest, InstructionLayers, MANIFEST_VERSION, McpServers,
    ModelTiers, SkillSet, StyleSelection, selection_matches,
};
