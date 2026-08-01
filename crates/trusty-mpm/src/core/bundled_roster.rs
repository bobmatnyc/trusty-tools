//! The canonical bundled-agent NAME roster (#4442 probe, #4448 repair).
//!
//! Why: `trusty_agents_common::agents::tier_audit` answers "does this file
//! resolve to a name tm ships?" from a roster its CALLER supplies — it cannot
//! reach this binary's embedded bundle. That made the roster the one piece of
//! the shared contract still living at a single consumer, in
//! [`crate::daemon::doctor_asset_tier`]. It cannot stay there: #4448's
//! quarantine MOVES exactly the files that probe reports, so a roster the two
//! built differently would produce the drift `tier_audit` exists to prevent —
//! doctor flagging files the sweep skips, or the sweep moving files doctor
//! never flagged.
//!
//! What: [`bundled_roster`], the union of the on-disk agent source directory
//! and the agents compiled into this binary, keyed by declared `name:`.
//! Test: the `roster_*` cases in
//! `crates/trusty-mpm/src/daemon/doctor_asset_tier_tests.rs`, which exercise
//! this function through its original consumer.

use std::collections::BTreeSet;

use trusty_agents_common::agents::tier_audit::{agent_identity, bundled_agent_names};

use crate::core::paths::FrameworkPaths;

/// The names tm currently ships as bundled agents.
///
/// Why: an empty roster is the dangerous failure. For the doctor probe it is a
/// silent false green (every shadowing copy classifies as a legitimate custom
/// agent); for the #4448 quarantine it is a hard refusal, since a roster that
/// could not be built proves nothing about any file. The on-disk source
/// directory is the accurate authority — it is literally what the deployer
/// reads — but it is absent on a binary-only install, so the embedded bundle
/// backstops it and the union is non-empty on both install shapes.
/// What: [`bundled_agent_names`] over [`FrameworkPaths::agent_source_dir`],
/// unioned with every `agents/*.md` entry of [`crate::core::bundle::ALL`]. The
/// embedded half runs the SAME [`agent_identity`] rule over the artifact's
/// CONTENTS rather than its `rel_path`, so both halves are keyed by declared
/// `name:` — the bundle ships files whose stem and name differ (`BASE-AGENT.md`
/// declares `name: base-agent`), and a stem-keyed half would silently exempt
/// them.
/// Test: `roster_falls_back_to_the_embedded_bundle`,
/// `roster_keys_the_embedded_half_by_declared_name`.
pub fn bundled_roster(paths: &FrameworkPaths) -> BTreeSet<String> {
    let mut names = bundled_agent_names(&paths.agent_source_dir());
    names.extend(crate::core::bundle::ALL.iter().filter_map(|artifact| {
        let file_name = artifact.rel_path.strip_prefix("agents/")?;
        if !file_name.ends_with(".md") {
            return None;
        }
        Some(agent_identity(artifact.contents, file_name))
    }));
    names
}
