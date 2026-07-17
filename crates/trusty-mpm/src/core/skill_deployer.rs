//! Skill deployment — re-exported from `trusty-agents-common`.
//!
//! Why: this module's implementation (`deploy_skills`, `deploy_skills_filtered`,
//! `DeployStats`, `is_skill_file`, `skill_stem`) moved to
//! `trusty-agents-common::skills::deployer` (#2892, #2818) so `trusty-code`
//! can eventually reuse the same manifest-based skill deploy pipeline instead
//! of forking it, mirroring the precedent set by the agent deployer
//! extraction. This shim keeps every existing trusty-mpm call site
//! (`crate::core::skill_deployer::{deploy_skills, DeployStats, …}`)
//! compiling unchanged.
//! What: a blanket re-export of the shared crate's public `skills::deployer` API.
//! Test: `cargo test -p trusty-agents-common skills::deployer` — the tests
//! moved with the implementation; this crate exercises the re-exported call
//! sites end-to-end via `cargo test -p trusty-mpm`.

pub use trusty_agents_common::skills::deployer::*;
