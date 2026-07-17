//! Multi-tier skill precedence — re-exported from `trusty-agents-common`.
//!
//! Why: this module's implementation (`plan_skill_tiers`, `resolve_skill_tier`,
//! `deploy_all_skill_tiers`, `list_source_stems`, `list_project_custom_stems`,
//! `SkillTier`, `Shadow`, `TierPlan`, `TierDeploy`) moved to
//! `trusty-agents-common::skills::tiers` (#2892, #2818) alongside
//! `skill_deployer` and `skill_manifest`, so `trusty-code` can eventually
//! reuse the same project-custom > user-custom > bundled precedence resolver
//! instead of forking it. This shim keeps every existing trusty-mpm call site
//! (`crate::core::skill_tiers::{deploy_all_skill_tiers, resolve_skill_tier, …}`)
//! compiling unchanged.
//! What: a blanket re-export of the shared crate's public `skills::tiers` API.
//! Test: `cargo test -p trusty-agents-common skills::tiers` — the tests
//! moved with the implementation; this crate exercises the re-exported call
//! sites end-to-end via `cargo test -p trusty-mpm`.

pub use trusty_agents_common::skills::tiers::*;
