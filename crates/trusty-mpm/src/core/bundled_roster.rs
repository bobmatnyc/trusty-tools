//! The canonical bundled-agent NAME roster (#4442, shared with #4448).
//!
//! Why: two consumers ask "does this file resolve to a name tm ships?" — `tm
//! doctor`'s `asset_tier` probe, which REPORTS the answer, and the #4448
//! quarantine, which MOVES files on it. They must agree file-for-file. A second,
//! independently derived roster is not a convenience: doctor flagging a file the
//! sweep refuses is noise operators learn to ignore, and the sweep moving a file
//! doctor never flagged is a project's agent silently disappearing.
//!
//! What: [`bundled_roster`] — the on-disk agent source UNION the agents
//! compiled into this binary. Lives here, in `core`, rather than inside the
//! doctor probe, because the quarantine call sites in `session_launch` cannot
//! reach into `daemon`.
//!
//! Test: `crates/trusty-mpm/src/daemon/doctor_asset_tier_tests.rs`
//! (`roster_falls_back_to_the_embedded_bundle`,
//! `roster_keys_the_embedded_half_by_declared_name`).

use std::collections::BTreeSet;

use trusty_agents_common::agents::tier_audit::{agent_identity, bundled_agent_names};

use crate::core::paths::FrameworkPaths;

/// The canonical bundled-agent roster, as resolved NAMES.
///
/// Why: an empty roster would classify every shadowing copy as a legitimate
/// custom agent — a silent false green in doctor, and a silent no-op in the
/// quarantine. The on-disk source directory is the accurate authority (it is
/// literally what the deployer reads), but it is absent on a binary-only
/// install, so the agents compiled into this binary backstop it.
/// What: [`bundled_agent_names`] over [`FrameworkPaths::agent_source_dir`],
/// unioned with every `agents/*.md` entry of [`crate::core::bundle::ALL`]. The
/// embedded half runs the SAME [`agent_identity`] rule over the artifact's
/// contents rather than its `rel_path`, so both halves are keyed by declared
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
