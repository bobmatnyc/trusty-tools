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
use trusty_common::content::find_dev_checkout;

pub use crate::core::framework_content::FrameworkContent;

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

/// trusty-mpm's skills and instructions, resolved like [`agent_roster`] (#9012).
pub fn framework_content() -> Result<FrameworkContent, AgentContentError> {
    FrameworkContent::load(&resolve_content(dev_override())?)
}

/// [`framework_content`] against an explicit cache directory and override.
pub fn framework_content_in(
    cache_dir: &Path,
    dev: DevOverride,
) -> Result<FrameworkContent, AgentContentError> {
    FrameworkContent::load(&resolve_content_in(cache_dir, dev)?)
}

/// The content for work on `dir` — a project being launched (#9012).
///
/// Why: a daemon-spawned launch runs with the daemon's cwd, which says nothing
/// about the project; a launch of a trusty-tools checkout must read that
/// checkout's content, as `tm launch` run inside it does.
/// What: the trusted checkout enclosing `dir`, else the one enclosing this
/// process's cwd (the rule [`agent_roster`] applies), else the installed
/// bundle. A checkout that is found is the source; a failure reading it is
/// returned, never answered by another source.
/// Test: `framework_content_resolves_from_the_named_dir`.
pub fn framework_content_for(dir: &Path) -> Result<FrameworkContent, AgentContentError> {
    let checkout = find_dev_checkout(dir).or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| find_dev_checkout(&cwd))
    });
    let content = match checkout {
        Some(root) => resolve_content(DevOverride::At(root))?,
        None => resolve_content(DevOverride::Off)?,
    };
    FrameworkContent::load(&content)
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

    /// The checkout's skills and instructions (#9012).
    pub(crate) fn repo_content() -> super::FrameworkContent {
        super::FrameworkContent::load(&checkout_content(&repo_root()).expect("repo content"))
            .expect("repo framework content")
    }

    /// [`repo_content`], loaded once per test binary: the prompt tests compose
    /// hundreds of prompts, and each load reads ~200 files.
    pub(crate) fn rc() -> &'static super::FrameworkContent {
        static CONTENT: std::sync::OnceLock<super::FrameworkContent> = std::sync::OnceLock::new();
        CONTENT.get_or_init(repo_content)
    }

    /// The checkout's bundled PM instruction package, parsed at load (#9012).
    pub(crate) fn rc_package() -> &'static crate::core::instruction_package::InstructionPackage {
        rc().pm_package()
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

    /// #9012: a project inside the checkout reads the checkout's content even
    /// with an empty cache.
    #[test]
    fn framework_content_resolves_from_the_named_dir() {
        let content = framework_content_for(&test_support::repo_root().join("crates/trusty-mpm"))
            .expect("checkout content from the named dir");
        assert!(content.skill("skills/tm.md").is_some());
        let cache = tempfile::tempdir().expect("tempdir");
        let err = framework_content_in(cache.path(), DevOverride::Off).expect_err("nothing");
        assert!(err.is_not_installed(), "got {err:?}");
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
