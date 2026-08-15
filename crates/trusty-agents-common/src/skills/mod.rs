//! Skill deploy/manifest/tier machinery, extracted from `trusty-mpm` (#2892, #2818).
//!
//! Why: `trusty-mpm` and `trusty-code` both need to keep `~/.claude/skills/`
//! (and `<project>/.claude/skills/`) populated with up-to-date skill files
//! without clobbering user-owned or user-modified content, and both need the
//! same project-custom > user-custom > bundled precedence rule. This logic
//! was originally trusty-mpm-only (`crates/trusty-mpm/src/core/
//! {skill_deployer,skill_manifest,skill_tiers}.rs`); moving it here (mirroring
//! the agent compose/deploy/manifest extraction, #2892) lets both harnesses
//! share one implementation instead of forking it.
//! What: three submodules — [`manifest`] (the checksum + atomic-write
//! ownership ledger, reusing [`crate::agents::manifest`]'s `ManifestError` /
//! `atomic_write` / `checksum` rather than defining a second self-contained
//! error type), [`deployer`] (writes skill `.md` sources into
//! `<dest>/<name>/SKILL.md`, consulting the manifest to avoid clobbering user
//! edits), and [`tiers`] (the project-custom / user-custom / bundled
//! precedence resolver and multi-tier deploy orchestrator), plus two halves of
//! the #4605 unreachable-bundled-skill fix — [`unmanaged`] (READ-ONLY
//! detection of a bundled-named skill a deploy target does not manage, which
//! [`tiers`] silently excludes from every deploy) and [`reconcile`] (the
//! explicit, human-initiated adoption that backs each one up and records it in
//! the manifest so the ordinary deploy reaches it again). `trusty-mpm`
//! re-exports every public item from `crate::core::{skill_deployer,
//! skill_manifest,skill_tiers}` for source compatibility with its existing
//! call sites.
//! Test: `cargo test -p trusty-agents-common skills::` exercises every
//! submodule in place; `cargo test -p trusty-mpm` exercises the re-exported
//! call sites end-to-end.
//!
//! [`manifest`]: crate::skills::manifest
//! [`deployer`]: crate::skills::deployer
//! [`tiers`]: crate::skills::tiers
//! [`unmanaged`]: crate::skills::unmanaged
//! [`reconcile`]: crate::skills::reconcile

pub mod deployer;
pub mod manifest;
pub mod reconcile;
pub mod source_scan;
pub mod tiers;
pub mod unmanaged;
