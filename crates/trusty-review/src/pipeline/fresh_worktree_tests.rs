//! #8411: a review in a freshly provisioned worktree or clone must run, not hard-skip.
//!
//! Why: a new worktree has no trusty-search index of its own. Auto-derive used
//! to leave `search_index` at the `"main"` default, which no daemon registers,
//! and the context gate turned the resulting `404 unknown index` into a hard
//! skip that told the operator to reindex the checkout (~7 minutes).
//! What: builds real-shaped git layouts in temp dirs — a main checkout, a
//! linked worktree OUTSIDE it (so the root-prefix match cannot find the main
//! index by accident), and a plain clone nothing indexes — then drives
//! `ReviewConfig::resolve_index`, `preflight_context` and `gather_context`
//! against a registry-aware fake daemon. No live daemon, no real index.
//! Test: this module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serial_test::serial;

use super::*;
use crate::{
    config::{InvocationSurface, ReviewConfig},
    integrations::{
        AnalyzeClient, AnalyzeClientError, AnalyzeHealthResponse, ComplexityHotspot,
        IndexStatusResponse, Smell,
        search_client::{
            EmbedderState, HealthResponse, IndexInfo, SearchClient, SearchClientError, SearchResult,
        },
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    pipeline::{runner::ReviewDeps, runner_context::gather_context},
};

// ── Fakes ─────────────────────────────────────────────────────────────────────

struct StubLlm;

#[async_trait]
impl LlmProvider for StubLlm {
    fn name(&self) -> &str {
        "stub"
    }
    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Transport("unused".to_string()))
    }
}

/// A healthy daemon that knows exactly the indexes it lists: any other id
/// answers `404 unknown index`, as trusty-search does.
struct Registry(Vec<IndexInfo>);

impl Registry {
    fn check(&self, id: &str) -> Result<(), SearchClientError> {
        if self.0.iter().any(|i| i.id == id) {
            Ok(())
        } else {
            Err(SearchClientError::Api {
                status: 404,
                body: format!("{{\"error\":\"unknown index: {id}\"}}"),
            })
        }
    }
}

#[async_trait]
impl SearchClient for Registry {
    async fn index_status(&self, id: &str) -> Result<IndexStatusResponse, SearchClientError> {
        self.check(id).map(|()| IndexStatusResponse::ready(id))
    }
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Ok(HealthResponse {
            status: "ok".to_string(),
            embedder: EmbedderState::Bool(true),
            warmboot_summary: None,
        })
    }
    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(self.0.clone())
    }
    async fn search(
        &self,
        id: &str,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        self.check(id).map(|()| Vec::new())
    }
}

/// Analysis exists only for the indexes the registry lists.
struct Analyze(Vec<String>);

#[async_trait]
impl AnalyzeClient for Analyze {
    async fn health(&self) -> Result<AnalyzeHealthResponse, AnalyzeClientError> {
        Ok(AnalyzeHealthResponse {
            status: "ok".to_string(),
            search_reachable: true,
        })
    }
    async fn has_analysis(&self, id: &str) -> bool {
        self.0.iter().any(|i| i == id)
    }
    async fn complexity_hotspots(
        &self,
        _: &str,
        _: Option<u32>,
    ) -> Result<Vec<ComplexityHotspot>, AnalyzeClientError> {
        Ok(vec![])
    }
    async fn smells(&self, _: &str) -> Result<Vec<Smell>, AnalyzeClientError> {
        Ok(vec![])
    }
}

// ── Fixtures ──────────────────────────────────────────────────────────────────

/// Restores the process CWD on drop, so a failing assertion cannot leak it.
struct CwdGuard(PathBuf);

impl CwdGuard {
    fn enter(dir: &Path) -> Self {
        let original = std::env::current_dir().expect("cwd readable");
        std::env::set_current_dir(dir).expect("enter test dir");
        Self(original)
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

/// Lay out what `git worktree add <wt>` leaves on disk: `<main>/.git/` with a
/// `worktrees/<name>/commondir`, and a `<wt>/.git` FILE pointing at it.
fn linked_worktree(main: &Path, wt: &Path) {
    let admin = main.join(".git").join("worktrees").join("wt");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    std::fs::create_dir_all(wt).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
}

fn index(id: &str, root: &Path) -> IndexInfo {
    IndexInfo {
        id: id.to_string(),
        name: None,
        root_path: Some(root.canonicalize().unwrap().display().to_string()),
    }
}

/// Config as a user-level MCP server or a bare CLI run sees it: no
/// `TRUSTY_SEARCH_INDEX`, so the index is auto-derived.
fn auto_config() -> ReviewConfig {
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;
    config
}

fn deps(registry: Vec<IndexInfo>) -> ReviewDeps {
    let analyzed = registry.iter().map(|i| i.id.clone()).collect();
    ReviewDeps {
        llm: Arc::new(StubLlm),
        verifier: None,
        search: Arc::new(Registry(registry)),
        analyze: Some(Arc::new(Analyze(analyzed))),
        dedup: None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A worktree with no index of its own reviews against its main checkout's
/// index — a normal, authoritative review, with no reindex step.
#[tokio::test]
#[serial]
async fn fresh_worktree_reuses_the_main_checkout_index_instead_of_skipping() {
    let tmp = tempfile::tempdir().unwrap();
    let (main, wt) = (tmp.path().join("main"), tmp.path().join("elsewhere/wt"));
    linked_worktree(&main, &wt);
    let registry = vec![index("repo-main", &main)];

    let mut config = auto_config();
    {
        let _cwd = CwdGuard::enter(&wt);
        config.resolve_index(&Registry(registry.clone())).await;
    }
    assert_eq!(config.search_index, "repo-main");

    let deps = deps(registry);
    let outcome = preflight_context(&config, &deps, InvocationSurface::Interactive).await;
    assert_eq!(
        outcome,
        GateOutcome::Proceed,
        "fresh worktree must not skip"
    );
    assert!(
        gather_context(&config, &deps, &["f".into()], &[], "t", "")
            .await
            .is_ok()
    );
}

/// A checkout nothing indexes (a fresh clone) still produces a review, and
/// that review says it ran without the index; it never proceeds as a normal
/// review.
#[tokio::test]
#[serial]
async fn fresh_clone_without_any_index_degrades_loudly_instead_of_skipping() {
    let tmp = tempfile::tempdir().unwrap();
    let clone = tmp.path().join("clone");
    std::fs::create_dir_all(clone.join(".git")).unwrap();
    // Only a sibling checkout is indexed, so the root-prefix match misses.
    let sibling = tmp.path().join("sibling");
    std::fs::create_dir_all(&sibling).unwrap();
    let registry = vec![index("unrelated", &sibling)];

    let mut config = auto_config();
    {
        let _cwd = CwdGuard::enter(&clone);
        config.resolve_index(&Registry(registry.clone())).await;
    }

    let deps = deps(registry);
    match preflight_context(&config, &deps, InvocationSurface::Interactive).await {
        GateOutcome::Degraded(reason) => assert!(
            reason.contains("no trusty-search index") && reason.contains("WITHOUT code context"),
            "the degraded reason must say the index was missing: {reason}"
        ),
        other => panic!("fresh clone must degrade, not {other:?}"),
    }
    assert!(
        gather_context(&config, &deps, &["f".into()], &[], "t", "")
            .await
            .is_ok()
    );

    // Fail-open check: where search is required, the missing index still
    // refuses rather than advancing to a normal review.
    assert!(matches!(
        preflight_context(&config, &deps, InvocationSurface::Hosted).await,
        GateOutcome::Skip(_)
    ));
}
