//! Bundled-skill reconcile/adoption — re-exported from `trusty-agents-common`.
//!
//! Why: adoption (`adopt_unmanaged_bundled_skills`,
//! `preview_unmanaged_bundled_skills`, `AdoptedSkill`) must stay beside the
//! manifest and deployer whose ownership rule it unsticks, in
//! `trusty-agents-common::skills::reconcile` (#4605). This shim matches the
//! existing `skill_tiers` re-export convention.
//! What: a blanket re-export of the shared crate's `skills::reconcile` API.
//! Test: `cargo test -p trusty-agents-common skills::reconcile`.

pub use trusty_agents_common::skills::reconcile::*;
