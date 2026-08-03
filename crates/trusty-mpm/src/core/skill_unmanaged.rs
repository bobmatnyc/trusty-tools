//! Unmanaged bundled-skill detection — re-exported from `trusty-agents-common`.
//!
//! Why: the detector (`unmanaged_bundled_skills`, `UnmanagedBundledSkill`,
//! `SKILL_ENTRY_POINT`) lives beside the deployer and manifest it depends on
//! in `trusty-agents-common::skills::unmanaged` (#4605), so a second harness
//! reuses it rather than forking the "is this deployed skill reachable?" rule.
//! This shim matches the existing `skill_tiers` / `skill_deployer` /
//! `skill_manifest` re-export convention so trusty-mpm call sites import it
//! from `crate::core::` like every sibling.
//! What: a blanket re-export of the shared crate's `skills::unmanaged` API.
//! Test: `cargo test -p trusty-agents-common skills::unmanaged`.

pub use trusty_agents_common::skills::unmanaged::*;
