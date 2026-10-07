//! The one place trusty-mpm resolves the agent roster and harness docs (#9011).
//!
//! Why: since #9011 the 43 agents and the four harness-understanding docs are
//! instructional content (ADR-0064), not compiled in. Every consumer — `tm
//! install`, the agent source freshness check, pm-guard, the framework
//! manifest, the SM prompt — must resolve them the same way, and a missing
//! source must fail loud in each of them.
//! What: [`agent_roster`] and [`harness_doc`] resolve the dev override from the
//! cwd (a trusty-tools checkout wins), else the bundle installed by `tm content
//! update`. The framework-content resolvers fetch that bundle on first use
//! (#9396) — only on the session-composition and `tm install` paths; every
//! read-only caller (doctor, retirement, savings) resolves with
//! [`Fetch::Never`]. Every failure is an [`AgentContentError`] naming the fix.
//! Functions tests drive hermetically take an [`AgentRoster`] parameter instead.
//! Test: `agent_roster_in_an_empty_cache_is_not_installed`,
//! `the_checkout_roster_resolves_from_inside_the_checkout`.

use std::path::Path;

pub use trusty_agents_common::agent_content::{AgentContentError, AgentRoster, DevOverride};
use trusty_agents_common::agent_content::{
    ResolvedContent, checkout_content, resolve_content, resolve_content_in,
};
use trusty_agents_common::harness_doc::HarnessDoc;
use trusty_common::content::find_dev_checkout;

use crate::content::first_use::resolve_or_fetch_at;

/// Whether a resolver may fetch the content release when nothing is
/// installed (#9396).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch {
    /// Fetch once on first use: session composition and `tm install`.
    OnFirstUse,
    /// Never touch the network: `tm doctor` and every read-only caller.
    Never,
}

/// The content for work on `dir`, or on the cwd when `dir` is `None`.
///
/// Why: one rule for every caller, so a launch, its roster and the doctor
/// row that predicts the launch cannot resolve different sources (#9396).
/// What: [`resolve_for_in`] with this process's cwd and the default cache.
pub fn resolve_for(dir: Option<&Path>, fetch: Fetch) -> Result<ResolvedContent, AgentContentError> {
    let cwd = std::env::current_dir().ok();
    let cache = trusty_common::content::default_cache_dir();
    resolve_for_in(dir, cwd.as_deref(), cache.as_deref(), fetch)
}

/// [`resolve_for`] with the cwd and the cache named.
///
/// What: the trusted checkout enclosing `dir`, else the one enclosing `cwd`,
/// else the installed bundle in `cache`. A checkout that is found is the
/// source; a failure reading it is returned, never answered by another
/// source. With no checkout, [`Fetch::OnFirstUse`] fetches the release when
/// no lock is installed; [`Fetch::Never`] answers `NotInstalled`.
/// [`AgentContentError::NoCacheDir`] when no checkout serves and `cache` is
/// `None`.
/// Test: `a_managed_workspace_resolves_the_cache_once_a_bundle_lands`,
/// `the_launch_roster_comes_from_the_launch_content`,
/// `the_content_row_never_fetches`.
pub fn resolve_for_in(
    dir: Option<&Path>,
    cwd: Option<&Path>,
    cache: Option<&Path>,
    fetch: Fetch,
) -> Result<ResolvedContent, AgentContentError> {
    let checkout = dir
        .and_then(find_dev_checkout)
        .or_else(|| cwd.and_then(find_dev_checkout));
    if let Some(root) = checkout {
        return checkout_content(&root);
    }
    let cache = cache.ok_or(AgentContentError::NoCacheDir)?;
    match fetch {
        Fetch::OnFirstUse => resolve_or_fetch_at(cache, DevOverride::Off),
        Fetch::Never => resolve_content_in(cache, DevOverride::Off),
    }
}

/// The PM content and the agent roster of one launch, read from one source.
#[derive(Debug)]
pub struct LaunchContent {
    /// The skills and instructions.
    pub framework: FrameworkContent,
    /// The agent roster of the same source, or why it does not load.
    pub roster: Result<AgentRoster, AgentContentError>,
}

impl LaunchContent {
    /// Loads both from `resolved`; only a framework failure is an error.
    ///
    /// Test: `the_launch_roster_comes_from_the_launch_content`.
    pub fn load(resolved: &ResolvedContent) -> Result<Self, AgentContentError> {
        Ok(Self {
            framework: FrameworkContent::load(resolved)?,
            roster: AgentRoster::load(resolved),
        })
    }
}

/// The content and roster for a launch of `dir` (#9396).
///
/// Why: the roster used to resolve from the process cwd while the content
/// resolved for the project, so one launch could mix two sources.
/// What: [`resolve_for`] `dir` with [`Fetch::OnFirstUse`], then
/// [`LaunchContent::load`].
pub fn launch_content_for(dir: &Path) -> Result<LaunchContent, AgentContentError> {
    LaunchContent::load(&resolve_for(Some(dir), Fetch::OnFirstUse)?)
}

/// [`framework_content_for`], or [`framework_content`] for `None`, that never
/// fetches: `tm doctor` and the read-only callers (#9396).
pub fn framework_content_local(dir: Option<&Path>) -> Result<FrameworkContent, AgentContentError> {
    FrameworkContent::load(&resolve_for(dir, Fetch::Never)?)
}

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
///
/// #9396: with nothing installed and no checkout, the content release is
/// fetched once first ([`Fetch::OnFirstUse`]).
pub fn framework_content() -> Result<FrameworkContent, AgentContentError> {
    FrameworkContent::load(&resolve_for(None, Fetch::OnFirstUse)?)
}

/// [`agent_roster`], fetching the content release on first use (#9396): the
/// `tm install` gate, which reads the roster before anything else.
pub fn agent_roster_or_fetch() -> Result<AgentRoster, AgentContentError> {
    AgentRoster::load(&resolve_for(None, Fetch::OnFirstUse)?)
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
/// What: [`resolve_for`] `dir` — the checkout enclosing `dir`, else the
/// one enclosing this process's cwd, else the installed bundle. #9396: with
/// no checkout and no lock, the content release is fetched once
/// ([`Fetch::OnFirstUse`]).
/// Test: `framework_content_resolves_from_the_named_dir`;
/// `missing_lock_fetches_the_release_once` (the fetch).
pub fn framework_content_for(dir: &Path) -> Result<FrameworkContent, AgentContentError> {
    FrameworkContent::load(&resolve_for(Some(dir), Fetch::OnFirstUse)?)
}

/// Test helpers: the repository's own content, through `DevOverride::At`, so
/// no test depends on the cwd, HOME or an installed bundle.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use trusty_agents_common::agent_content::checkout_content;

    use super::{AgentRoster, HarnessDoc};

    /// The trusty-tools checkout this test runs for, resolved at runtime.
    ///
    /// #9298: never the compile-time `CARGO_MANIFEST_DIR`, which names the
    /// checkout that BUILT the binary — another worktree under a shared target.
    /// Test: `repo_root_follows_the_runtime_checkout_9298`.
    pub(crate) fn repo_root() -> PathBuf {
        trusty_common::test_harness::test_repo_root()
            .expect("resolve the trusty-tools checkout (set TRUSTY_TEST_REPO_ROOT)")
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

#[cfg(test)]
#[path = "content_source_root_tests.rs"]
mod content_source_root_tests;

#[cfg(test)]
#[path = "content_source_launch_tests.rs"]
mod content_source_launch_tests;
