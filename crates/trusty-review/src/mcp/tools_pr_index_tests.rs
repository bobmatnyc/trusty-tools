//! `review_pr`'s per-call trusty-search index resolution (#8649).
//!
//! Why: `review_pr` used the index the MCP server resolved from its own CWD at
//! startup (or `"main"`), so every other repo was reviewed against the wrong
//! index. These tests pin the per-repo mapping and its fail-closed misses.
//! What: a registry fake reporting `repo_identity` per index drives
//! `config_for_pr_repo` (one `AppState`, two repos) and the pure
//! `select_repo_index` / `resolve_repo_index` rules. No network.
//! Test: this module IS the tests.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    config::{
        ReviewConfig,
        repo_index::{RepoIndexError, resolve_repo_index, select_repo_index},
    },
    integrations::search_client::{
        EmbedderState, HealthResponse, IndexIdentity, IndexInfo, IndexStatusResponse, SearchClient,
        SearchClientError, SearchResult,
    },
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    service::AppState,
};

use super::config_for_pr_repo;

/// LLM stand-in; `AppState` needs one, these tests never call it.
struct UnusedLlm;

#[async_trait]
impl LlmProvider for UnusedLlm {
    fn name(&self) -> &str {
        "unused-8649"
    }

    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::Transport("not called in #8649 tests".into()))
    }
}

/// Search fake whose registry reports each index's `repo_identity`, or fails
/// to list at all when `indexes` is `None`.
struct Registry {
    indexes: Option<Vec<IndexIdentity>>,
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

    async fn list_index_identities(&self) -> Result<Vec<IndexIdentity>, SearchClientError> {
        self.indexes
            .clone()
            .ok_or_else(|| SearchClientError::Transport("connection refused".into()))
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

fn entry(id: &str, root: &str, identity: Option<&str>) -> IndexIdentity {
    IndexIdentity {
        id: id.into(),
        root_path: Some(root.into()),
        repo_identity: identity.map(str::to_string),
    }
}

/// A registry holding two repos (one with a session worktree facet) plus a
/// `"main"` index that belongs to neither.
fn two_repo_registry() -> Vec<IndexIdentity> {
    vec![
        entry("main", "/src/scratch", Some("someone/scratch")),
        entry(
            "code-intelligence-wt-abc",
            "/src/code-intelligence/.worktrees/abc",
            Some("duettoresearch/code-intelligence"),
        ),
        entry(
            "code-intelligence-9f1c2e3a",
            "/src/code-intelligence",
            Some("duettoresearch/code-intelligence"),
        ),
        entry(
            "trusty-tools-4e2cf878",
            "/src/trusty-tools",
            Some("bobmatnyc/trusty-tools"),
        ),
    ]
}

fn state_with(indexes: Option<Vec<IndexIdentity>>) -> AppState {
    let mut config = ReviewConfig::load(None);
    // The server-startup default a CWD-resolution miss leaves behind.
    config.search_index = "main".into();
    config.search_index_explicit = false;
    AppState::new(
        config,
        Arc::new(UnusedLlm),
        Arc::new(Registry { indexes }),
        None,
    )
}

/// Two repos, neither indexed as `"main"`, each resolve to their own index
/// through ONE `AppState`, and the shared startup config stays untouched.
#[tokio::test]
async fn two_repos_resolve_to_their_own_indexes_in_one_server() {
    let state = state_with(Some(two_repo_registry()));

    let ci = config_for_pr_repo(&state, "DuettoResearch", "code-intelligence")
        .await
        .expect("code-intelligence has an index");
    assert_eq!(ci.search_index, "code-intelligence-9f1c2e3a");
    assert!(ci.search_index_explicit);

    let tt = config_for_pr_repo(&state, "bobmatnyc", "trusty-tools")
        .await
        .expect("trusty-tools has an index");
    assert_eq!(tt.search_index, "trusty-tools-4e2cf878");

    assert_eq!(
        state.config.search_index, "main",
        "a review_pr call must not rewrite the server's startup index"
    );
}

/// The session pin is honoured when it belongs to the repo, and ignored when
/// it does not.
#[test]
fn pinned_index_wins_only_when_it_belongs_to_the_repo() {
    let reg = two_repo_registry();
    let key = "duettoresearch/code-intelligence";
    let pinned = select_repo_index(
        &reg,
        key,
        "code-intelligence",
        Some("code-intelligence-wt-abc"),
    );
    assert_eq!(pinned.as_deref(), Ok("code-intelligence-wt-abc"));

    let foreign_pin = select_repo_index(&reg, key, "code-intelligence", Some("main"));
    assert_eq!(foreign_pin.as_deref(), Ok("code-intelligence-9f1c2e3a"));
}

/// An index named after the repo but recorded for another repo is refused; one
/// with no recorded identity is accepted by name.
#[test]
fn same_named_index_of_another_repo_is_refused() {
    let other = vec![entry("widget", "/src/widget", Some("acme/widget"))];
    let err = select_repo_index(&other, "other/widget", "widget", None)
        .expect_err("acme's widget index must not serve other/widget");
    assert!(err.contains("belongs to acme/widget"), "{err}");

    let unknown = vec![entry("widget", "/src/widget", None)];
    assert_eq!(
        select_repo_index(&unknown, "other/widget", "widget", None).as_deref(),
        Ok("widget")
    );
}

/// A repo with no index yields an error naming the repo and the index id it
/// looked up — not a fall-back to the `"main"` index that IS registered.
#[tokio::test]
async fn missing_index_error_names_the_repo_and_the_index_id() {
    let state = state_with(Some(two_repo_registry()));
    let err = config_for_pr_repo(&state, "acme", "unindexed-repo")
        .await
        .expect_err("no index belongs to acme/unindexed-repo");
    assert!(matches!(err, RepoIndexError::NoIndex { .. }), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("acme/unindexed-repo"), "{msg}");
    assert!(msg.contains("\"unindexed-repo\""), "{msg}");
}

/// A registry that cannot be listed is an error naming the repo, never a
/// silent fall-back to the startup index.
#[tokio::test]
async fn registry_failure_is_an_error_not_a_default() {
    let search = Registry { indexes: None };
    let err = resolve_repo_index(&search, "acme", "widget", Some("main"))
        .await
        .expect_err("an unreadable registry must not resolve");
    let msg = err.to_string();
    assert!(matches!(err, RepoIndexError::Registry { .. }), "{msg}");
    assert!(
        msg.contains("acme/widget") && msg.contains("\"widget\""),
        "{msg}"
    );

    let bad = resolve_repo_index(&search, "", "widget", None).await;
    assert!(matches!(bad, Err(RepoIndexError::InvalidRepo { .. })));
}
