//! The canonical bundled-agent NAME roster (#4442, shared with #4448).
//!
//! Why: two consumers ask "does this file resolve to a name tm ships?" — `tm
//! doctor`'s `asset_tier` probe, which REPORTS the answer, and the #4448
//! quarantine, which MOVES files on it. They must agree file-for-file. A second,
//! independently derived roster is not a convenience: doctor flagging a file the
//! sweep refuses is noise operators learn to ignore, and the sweep moving a file
//! doctor never flagged is a project's agent silently disappearing.
//!
//! What: [`bundled_roster`] — the on-disk agent source UNION the content
//! roster (#9011; compiled into the binary before that). Lives here, in
//! `core`, rather than inside the doctor probe, because the quarantine call
//! sites in `session_launch` cannot reach into `daemon`.
//!
//! Test: `crates/trusty-mpm/src/daemon/doctor_asset_tier_tests.rs`
//! (`roster_falls_back_to_the_embedded_bundle`,
//! `roster_keys_the_embedded_half_by_declared_name`).

use std::collections::BTreeSet;

use trusty_agents_common::agents::tier_audit::{agent_identity, bundled_agent_names};

use crate::core::content_source::{self, AgentContentError, AgentRoster};
use crate::core::paths::FrameworkPaths;

/// The canonical bundled-agent roster, as resolved NAMES.
///
/// Why: an empty roster would classify every shadowing copy as a legitimate
/// custom agent — a silent false green in doctor, and a silent no-op in the
/// quarantine. The on-disk source directory is the accurate authority (it is
/// literally what the deployer reads), but it is absent on a binary-only
/// install, so the content roster (#9011) backstops it. When the content
/// roster cannot be resolved this is `Err`, and callers skip classification
/// rather than run on half a roster.
/// What: resolves [`content_source::agent_roster`] and delegates to
/// [`bundled_roster_with`].
/// Test: `agent_roster_in_an_empty_cache_is_not_installed` (the resolver arm
/// this propagates).
pub fn bundled_roster(paths: &FrameworkPaths) -> Result<BTreeSet<String>, AgentContentError> {
    Ok(bundled_roster_with(paths, &content_source::agent_roster()?))
}

/// [`bundled_roster`] against a roster already resolved.
///
/// What: [`bundled_agent_names`] over [`FrameworkPaths::agent_source_dir`],
/// unioned with every file of `roster`. The roster half runs the SAME
/// [`agent_identity`] rule over each file's contents rather than its name, so
/// both halves are keyed by declared `name:` — `BASE-AGENT.md` declares
/// `name: base-agent`, and a stem-keyed half would silently exempt it.
/// Test: `roster_falls_back_to_the_embedded_bundle`,
/// `roster_keys_the_embedded_half_by_declared_name`.
pub fn bundled_roster_with(paths: &FrameworkPaths, roster: &AgentRoster) -> BTreeSet<String> {
    let mut names = bundled_agent_names(&paths.agent_source_dir());
    names.extend(
        roster
            .iter()
            .map(|(file_name, contents)| agent_identity(contents, file_name)),
    );
    names
}
