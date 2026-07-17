//! Ownership manifest for deployed skill files — re-exported from
//! `trusty-agents-common`.
//!
//! Why: this module's implementation (`SkillManifest`, `SkillManifestEntry`,
//! `SKILL_MANIFEST_FILE`) moved to `trusty-agents-common::skills::manifest`
//! (#2892, #2818) alongside `skill_deployer` and `skill_tiers`, so
//! `trusty-code` can eventually reuse the same skill ownership ledger instead
//! of forking it, mirroring the precedent set by the agent manifest
//! extraction. This shim keeps every existing trusty-mpm call site
//! (`crate::core::skill_manifest::{SkillManifest, SkillManifestEntry,
//! SKILL_MANIFEST_FILE}`) compiling unchanged.
//! What: a blanket re-export of the shared crate's public `skills::manifest` API.
//! Test: `cargo test -p trusty-agents-common skills::manifest` — the tests
//! moved with the implementation; this crate exercises the re-exported call
//! sites end-to-end via `cargo test -p trusty-mpm`.

pub use trusty_agents_common::skills::manifest::*;
