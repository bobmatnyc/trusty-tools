//! Shared registry fake and the surface contract of `resolve_pr_index` (#8651).
//!
//! Why: four surfaces resolve a PR repo's index through `resolve_pr_index`
//! now; each needs the same registry fake, and the surface argument decides
//! whether an unreadable registry degrades or fails.
//! What: `Registry` (a trusty-search fake reporting `repo_identity` per index),
//! the `entry`/`two_repo_registry` builders, and the Hosted-surface tests. The
//! Interactive-surface tests live in `mcp/tools_pr_index_tests.rs`.
//! Test: this module IS the tests.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::{
    config::{InvocationSurface, ReviewConfig, repo_index::RepoIndexError},
    integrations::search_client::{
        EmbedderState, HealthResponse, IndexIdentity, IndexInfo, IndexStatusResponse, SearchClient,
        SearchClientError, SearchResult,
    },
};

use super::{IndexPin, PrIndex, resolve_pr_index};

/// Search fake: a healthy daemon whose registry reports each index's
/// `repo_identity`, or fails to list at all when `indexes` is `None`. Each
/// listing's `repo_identity` filter is recorded, and applied as the daemon
/// does — unless `ignore_filter` is set, modelling a daemon whose
/// `?repo_identity=` filtering is broken and returns the full list regardless
/// (#8649: proves `resolve_repo_index`'s local re-check is not dead code).
pub(crate) struct Registry {
    pub(crate) indexes: Option<Vec<IndexIdentity>>,
    pub(crate) listings: Mutex<Vec<Option<String>>>,
    pub(crate) ignore_filter: bool,
}

impl Registry {
    pub(crate) fn new(indexes: Option<Vec<IndexIdentity>>) -> Self {
        Self {
            indexes,
            listings: Mutex::new(Vec::new()),
            ignore_filter: false,
        }
    }

    /// A daemon that ignores the `repo_identity=` filter argument entirely.
    pub(crate) fn new_ignoring_filter(indexes: Vec<IndexIdentity>) -> Self {
        Self {
            indexes: Some(indexes),
            listings: Mutex::new(Vec::new()),
            ignore_filter: true,
        }
    }
}

#[async_trait]
impl SearchClient for Registry {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Ok(HealthResponse {
            status: "ok".into(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: None,
        })
    }

    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        unreachable!("#8649 resolution must read list_index_identities")
    }

    async fn list_index_identities(
        &self,
        repo_identity: Option<&str>,
    ) -> Result<Vec<IndexIdentity>, SearchClientError> {
        if let Ok(mut listings) = self.listings.lock() {
            listings.push(repo_identity.map(str::to_string));
        }
        let all = self
            .indexes
            .clone()
            .ok_or_else(|| SearchClientError::Transport("connection refused".into()))?;
        if self.ignore_filter {
            return Ok(all);
        }
        Ok(match repo_identity {
            Some(filter) => all
                .into_iter()
                .filter(|i| i.repo_identity.as_deref() == Some(filter))
                .collect(),
            None => all,
        })
    }

    async fn index_status(&self, id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        Ok(IndexStatusResponse::ready(id))
    }

    async fn search(
        &self,
        _: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

pub(crate) fn entry(id: &str, root: &str, identity: Option<&str>) -> IndexIdentity {
    IndexIdentity {
        id: id.into(),
        root_path: Some(root.into()),
        repo_identity: identity.map(str::to_string),
        last_used_unix: None,
    }
}

/// A registry holding two repos (one with a session worktree facet) plus a
/// `"main"` index that belongs to neither.
pub(crate) fn two_repo_registry() -> Vec<IndexIdentity> {
    vec![
        entry("main", "/src/scratch", Some("someone/scratch")),
        entry(
            "code-intelligence-wt-abc",
            "/src/code-intelligence/.worktrees/abc",
            Some("acme-labs/code-intelligence"),
        ),
        entry(
            "code-intelligence-9f1c2e3a",
            "/src/code-intelligence",
            Some("acme-labs/code-intelligence"),
        ),
        entry(
            "trusty-tools-4e2cf878",
            "/src/trusty-tools",
            Some("bobmatnyc/trusty-tools"),
        ),
    ]
}

/// A config whose startup index is the `"main"` a CWD miss leaves behind.
pub(crate) fn startup_config(require_search: Option<bool>) -> ReviewConfig {
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".into();
    config.search_index_explicit = false;
    config.context.require_search = require_search;
    config
}

/// The hosted surfaces (webhook drain, service `review`, CLI GitHub-PR `run`)
/// require search by default, so an unreadable registry is an error there —
/// never a silent diff-only review that then posts (REV-011, #8651).
#[tokio::test]
async fn hosted_surface_requires_search_for_an_unreadable_registry() {
    let config = startup_config(None);
    let err = resolve_pr_index(
        &Registry::new(None),
        &config,
        InvocationSurface::Hosted,
        "acme",
        "widget",
        IndexPin::Prefer,
    )
    .await
    .expect_err("Hosted must not degrade past an unreadable registry by default");
    assert!(matches!(err, RepoIndexError::Registry { .. }), "{err}");
    assert!(err.to_string().contains("acme/widget"), "{err}");
}

/// An operator who opted out of search gets the DEGRADED diff-only review on
/// a hosted surface too — the surface's own `require_search` contract.
#[tokio::test]
async fn hosted_surface_degrades_when_the_operator_opts_out_of_search() {
    let config = startup_config(Some(false));
    let index = resolve_pr_index(
        &Registry::new(None),
        &config,
        InvocationSurface::Hosted,
        "acme",
        "widget",
        IndexPin::Prefer,
    )
    .await
    .expect("an explicit opt-out degrades");
    assert!(
        matches!(index, PrIndex::DiffOnly(ref n) if n.contains("DEGRADED")),
        "{index:?}"
    );

    // A resolvable repo still resolves on the hosted surface.
    let index = resolve_pr_index(
        &Registry::new(Some(two_repo_registry())),
        &config,
        InvocationSurface::Hosted,
        "bobmatnyc",
        "trusty-tools",
        IndexPin::Prefer,
    )
    .await;
    assert_eq!(
        index.ok(),
        Some(PrIndex::Resolved("trusty-tools-4e2cf878".into()))
    );
}

/// The unattended surfaces (webhook drain, service `review`, MCP `review_pr`)
/// pass `IndexPin::Prefer`: an explicit, ambient `TRUSTY_SEARCH_INDEX` naming
/// another repo's index is never inherited (#8651).
#[tokio::test]
async fn prefer_never_inherits_a_foreign_explicit_index() {
    let mut config = startup_config(None);
    config.search_index_explicit = true; // "main" is recorded for someone/scratch.
    let index = resolve_pr_index(
        &Registry::new(Some(two_repo_registry())),
        &config,
        InvocationSurface::Hosted,
        "bobmatnyc",
        "trusty-tools",
        IndexPin::Prefer,
    )
    .await;
    assert_eq!(
        index.ok(),
        Some(PrIndex::Resolved("trusty-tools-4e2cf878".into()))
    );
}

/// An operator pin is used only when it is recorded for the PR's repo; a
/// foreign, unregistered, or unverifiable pin is refused, naming both repos
/// where there are two (#8651). A legacy index with no recorded identity is
/// used for `--source-root` only, or when it carries the bare repo name.
#[tokio::test]
async fn operator_pin_is_used_only_for_its_own_repo() {
    use crate::config::repo_index::PinOrigin::{Env, SourceRoot};
    let mut indexes = two_repo_registry();
    indexes.push(entry("cto", "/src/cto", None));
    indexes.push(entry("trusty-tools", "/src/tt-legacy", None));
    indexes.push(entry(
        "garbled",
        "/src/garbled",
        Some("ssh://not@an-identity"),
    ));
    let registry = Registry::new(Some(indexes));
    let config = startup_config(None);
    let tt = ("bobmatnyc", "trusty-tools");
    for (pin, origin, want) in [
        ("trusty-tools-4e2cf878", Env, Ok("trusty-tools-4e2cf878")),
        (
            "main",
            Env,
            Err(["someone/scratch", "bobmatnyc/trusty-tools"]),
        ),
        (
            "main",
            SourceRoot,
            Err(["someone/scratch", "--source-root"]),
        ),
        (
            "absent",
            Env,
            Err(["not a registered index", "TRUSTY_SEARCH_INDEX"]),
        ),
        (
            "cto",
            Env,
            Err(["no recorded repo_identity", "Unset TRUSTY_SEARCH_INDEX"]),
        ),
        ("cto", SourceRoot, Ok("cto")),
        ("trusty-tools", Env, Ok("trusty-tools")),
        (
            "garbled",
            SourceRoot,
            Err(["unreadable", "bobmatnyc/trusty-tools"]),
        ),
    ] {
        let got = resolve_pr_index(
            &registry,
            &config,
            InvocationSurface::Hosted,
            tt.0,
            tt.1,
            IndexPin::Require(pin, origin),
        )
        .await;
        match (got, want) {
            (Ok(index), Ok(id)) => assert_eq!(index, PrIndex::Resolved(id.into()), "{pin}"),
            (Err(e), Err(needles)) => {
                let msg = e.to_string();
                assert!(
                    needles.iter().all(|n| msg.contains(n)),
                    "{pin}/{origin}: {msg}"
                );
            }
            (got, want) => panic!("{pin}/{origin}: got {got:?}, want {want:?}"),
        }
    }
}

/// Resolve `acme/unindexed` (absent from `two_repo_registry`) on `surface`.
async fn resolve_unindexed(
    surface: InvocationSurface,
    require_search: Option<bool>,
) -> Result<PrIndex, RepoIndexError> {
    resolve_pr_index(
        &Registry::new(Some(two_repo_registry())),
        &startup_config(require_search),
        surface,
        "acme",
        "unindexed",
        IndexPin::Prefer,
    )
    .await
}

/// #11409: the MCP tool's `Interactive` surface reviews an unindexed repo as a
/// DEGRADED diff-only review whose notice names the repo and the fix, unless
/// the operator required search.
#[tokio::test]
async fn interactive_no_index_degrades_to_diff_only() {
    for require in [None, Some(false)] {
        let index = resolve_unindexed(InvocationSurface::Interactive, require)
            .await
            .unwrap_or_else(|e| panic!("{require:?}: Interactive must degrade: {e}"));
        let PrIndex::DiffOnly(notice) = index else {
            panic!("{require:?}: want DiffOnly, got {index:?}");
        };
        for needle in ["DEGRADED", "acme/unindexed", "trusty-search index <path>"] {
            assert!(notice.contains(needle), "{needle}: {notice}");
        }
    }
}

/// #11409: an explicit `require_search = true` fails closed on the
/// interactive surface, and the hosted surfaces (webhook drain, service
/// `review`, CLI GitHub-PR `run`) fail closed even with search opted out.
#[tokio::test]
async fn no_index_fails_closed_when_required_or_hosted() {
    for (surface, require) in [
        (InvocationSurface::Interactive, Some(true)),
        (InvocationSurface::Hosted, None),
        (InvocationSurface::Hosted, Some(false)),
    ] {
        let got = resolve_unindexed(surface, require).await;
        assert!(
            matches!(got, Err(RepoIndexError::NoIndex { .. })),
            "{surface:?}/{require:?}: {got:?}"
        );
    }
}

/// #11409 regression: an unreadable registry keeps its #8649 behaviour on the
/// interactive surface — DiffOnly by default, the `Registry` error when
/// search is required.
#[tokio::test]
async fn unreadable_registry_is_unchanged_on_the_interactive_surface() {
    let registry = Registry::new(None);
    for (require, want_degrade) in [(None, true), (Some(true), false)] {
        let got = resolve_pr_index(
            &registry,
            &startup_config(require),
            InvocationSurface::Interactive,
            "acme",
            "widget",
            IndexPin::Prefer,
        )
        .await;
        if want_degrade {
            assert!(
                matches!(got, Ok(PrIndex::DiffOnly(ref n)) if n.contains("#8649") && n.contains("DEGRADED")),
                "{got:?}"
            );
        } else {
            assert!(
                matches!(got, Err(RepoIndexError::Registry { .. })),
                "{got:?}"
            );
        }
    }
}
