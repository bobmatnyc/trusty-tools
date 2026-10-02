//! The one place trusty-mpm resolves the agent roster and harness docs (#9011).
//!
//! Why: since #9011 the 43 agents and the four harness-understanding docs are
//! instructional content (ADR-0064), not compiled in. Every consumer — `tm
//! install`, the agent source freshness check, pm-guard, the framework
//! manifest, the SM prompt — must resolve them the same way, and a missing
//! source must fail loud in each of them.
//! What: [`agent_roster`] and [`harness_doc`] resolve the dev override from the
//! cwd (a trusty-tools checkout wins), else the bundle installed by `tm content
//! install`. Every failure is an [`AgentContentError`] naming the fix.
//! Functions tests drive hermetically take an [`AgentRoster`] parameter instead.
//! Test: `agent_roster_in_an_empty_cache_is_not_installed`,
//! `the_checkout_roster_resolves_from_inside_the_checkout`.

use std::path::Path;

pub use trusty_agents_common::agent_content::{AgentContentError, AgentRoster, DevOverride};
use trusty_agents_common::agent_content::{resolve_content, resolve_content_in};
use trusty_agents_common::harness_doc::HarnessDoc;

/// `DetectFrom(cwd)`, or `Off` when the cwd cannot be read.
pub fn dev_override() -> DevOverride {
    match std::env::current_dir() {
        Ok(cwd) => DevOverride::DetectFrom(cwd),
        Err(_) => DevOverride::Off,
    }
}

/// The agent roster from the checkout enclosing the cwd, else the installed
/// bundle in `~/.trusty-mpm/content`.
pub fn agent_roster() -> Result<AgentRoster, AgentContentError> {
    AgentRoster::load(&resolve_content(dev_override())?)
}

/// [`agent_roster`] against an explicit cache directory and override.
pub fn agent_roster_in(
    cache_dir: &Path,
    dev: DevOverride,
) -> Result<AgentRoster, AgentContentError> {
    AgentRoster::load(&resolve_content_in(cache_dir, dev)?)
}

/// The roster for a daemon query about `cwd`, or `None` (#9011).
///
/// Why: the daemon's own cwd says nothing about the project a query names, so
/// a dispatch from inside a trusty-tools checkout must see that checkout's
/// roster, exactly as `tm hook --pm-guard` running there does.
/// What: the checkout enclosing `cwd`, else the installed bundle; failing
/// both, [`agent_roster`] (the checkout enclosing the daemon's own cwd, the
/// resolution this replaced). A content error is logged at WARN and answered
/// `None`, which makes the shared-tree classifiers fail closed.
/// Test: `the_query_roster_resolves_from_the_query_cwd`.
pub fn agent_roster_for_query(cwd: &Path) -> Option<AgentRoster> {
    let resolved = resolve_content(DevOverride::DetectFrom(cwd.to_path_buf()))
        .and_then(|content| AgentRoster::load(&content))
        .or_else(|_| agent_roster());
    resolved
        .inspect_err(|err| {
            tracing::warn!(
                cwd = %cwd.display(),
                "agent roster unavailable for this query, so every agent counts as a writer: {err}"
            );
        })
        .ok()
}

/// The harness-understanding docs, resolved like [`agent_roster`].
pub fn harness_doc() -> Result<HarnessDoc, AgentContentError> {
    HarnessDoc::load(&resolve_content(dev_override())?)
}

/// Test helpers: the repository's own content, through `DevOverride::At`, so
/// no test depends on the cwd, HOME or an installed bundle.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use trusty_agents_common::agent_content::checkout_content;

    use super::{AgentRoster, HarnessDoc};

    /// The trusty-tools checkout this crate is built from.
    pub(crate) fn repo_root() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
    }

    /// The checkout's `content/agents` directory.
    pub(crate) fn repo_agents_dir() -> PathBuf {
        repo_root().join("content/agents")
    }

    /// The checkout's agent roster.
    pub(crate) fn repo_roster() -> AgentRoster {
        AgentRoster::load(&checkout_content(&repo_root()).expect("repo content"))
            .expect("repo roster")
    }

    /// The checkout's harness docs.
    pub(crate) fn repo_harness_doc() -> HarnessDoc {
        HarnessDoc::load(&checkout_content(&repo_root()).expect("repo content"))
            .expect("repo harness docs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #9011: no checkout and no lock is `NotInstalled`, naming the fix.
    #[test]
    fn agent_roster_in_an_empty_cache_is_not_installed() {
        let cache = tempfile::tempdir().expect("tempdir");
        let err = agent_roster_in(cache.path(), DevOverride::Off).expect_err("nothing installed");
        assert!(
            matches!(err, AgentContentError::NotInstalled { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("tm content install"), "{err}");
    }

    /// #9011: a daemon query resolves from the cwd it names, not the daemon's.
    #[test]
    fn the_query_roster_resolves_from_the_query_cwd() {
        let roster = agent_roster_for_query(&test_support::repo_root().join("crates/trusty-mpm"))
            .expect("checkout roster from the query cwd");
        assert_eq!(roster.len(), test_support::repo_roster().len());
    }

    /// From inside the checkout the dev override serves the working tree.
    #[test]
    fn the_checkout_roster_resolves_from_inside_the_checkout() {
        let cache = tempfile::tempdir().expect("tempdir");
        let roster = agent_roster_in(
            cache.path(),
            DevOverride::DetectFrom(test_support::repo_root().join("crates/trusty-mpm")),
        )
        .expect("checkout roster");
        assert!(roster.get("BASE-AGENT.md").is_some());
        assert_eq!(roster.len(), test_support::repo_roster().len());
    }
}
