//! `review_pr`'s per-call trusty-search index resolution (#8649).
//!
//! Why: `review_pr` used the index the MCP server resolved from its own CWD at
//! startup (or `"main"`), so every other repo was reviewed against the wrong
//! index. These tests pin the per-repo mapping, its fail-closed misses, and
//! the degraded diff-only review an unreadable registry falls back to.
//! What: a registry fake reporting `repo_identity` per index (and recording
//! each listing's filter) drives `review_pr_with` under a capturing runner,
//! `call_tool`, and the pure `select_repo_index` rules. No network.
//! Test: this module IS the tests.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

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
    mcp::tools::call_tool,
    models::ReviewResult,
    service::AppState,
};

use super::{PrIndex, resolve_pr_index, review_pr_with};

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

/// Search fake: a healthy daemon whose registry reports each index's
/// `repo_identity`, or fails to list at all when `indexes` is `None`. Each
/// listing's `repo_identity` filter is recorded, and applied as the daemon does.
struct Registry {
    indexes: Option<Vec<IndexIdentity>>,
    listings: Mutex<Vec<Option<String>>>,
}

impl Registry {
    fn new(indexes: Option<Vec<IndexIdentity>>) -> Self {
        Self {
            indexes,
            listings: Mutex::new(Vec::new()),
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

fn entry(id: &str, root: &str, identity: Option<&str>) -> IndexIdentity {
    IndexIdentity {
        id: id.into(),
        root_path: Some(root.into()),
        repo_identity: identity.map(str::to_string),
        last_used_unix: None,
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

/// An `AppState` whose startup index is the `"main"` a CWD-resolution miss
/// leaves behind, with `require_search` pinned so a developer's env cannot
/// flip the degrade decision.
fn state_with(indexes: Option<Vec<IndexIdentity>>, require_search: bool) -> AppState {
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".into();
    config.search_index_explicit = false;
    config.context.require_search = Some(require_search);
    AppState::new(
        config,
        Arc::new(UnusedLlm),
        Arc::new(Registry::new(indexes)),
        None,
    )
}

/// What one review run received, recorded by the capturing runner.
#[derive(Debug, Clone)]
struct Seen {
    index: String,
    search_health: Result<(), String>,
    analyze_ready: Option<bool>,
}

/// Drive `review_pr_with` for `owner/repo` under CLI auth with a token, and
/// return what the runner received (`None` when it never ran) plus the envelope.
async fn run_captured(state: &AppState, owner: &str, repo: &str) -> (Option<Seen>, Value) {
    let seen: Arc<Mutex<Option<Seen>>> = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&seen);
    let args = json!({ "owner": owner, "repo": repo, "pr": 7 });
    let envelope = review_pr_with(&args, state, move |config, _input, deps| async move {
        let search_health = deps
            .search
            .health()
            .await
            .map(|_| ())
            .map_err(|e| e.to_string());
        let analyze_ready = match deps.analyze.as_ref() {
            Some(a) => Some(a.has_analysis(&config.search_index).await),
            None => None,
        };
        if let Ok(mut slot) = sink.lock() {
            *slot = Some(Seen {
                index: config.search_index.clone(),
                search_health,
                analyze_ready,
            });
        }
        ReviewResult::new("o", "r", 7, "PR #7", "")
    })
    .await
    .expect("review_pr_with must not fail at the protocol level");
    let captured = seen.lock().ok().and_then(|s| s.clone());
    (captured, envelope)
}

/// Two repos, neither indexed as `"main"`, each resolve to their own index
/// through ONE `AppState`, and the shared startup config stays untouched.
#[tokio::test]
async fn two_repos_resolve_to_their_own_indexes_in_one_server() {
    let state = state_with(Some(two_repo_registry()), true);
    let ci = resolve_pr_index(&state, "DuettoResearch", "code-intelligence").await;
    assert_eq!(
        ci.ok(),
        Some(PrIndex::Resolved("code-intelligence-9f1c2e3a".into()))
    );
    let tt = resolve_pr_index(&state, "bobmatnyc", "trusty-tools").await;
    assert_eq!(
        tt.ok(),
        Some(PrIndex::Resolved("trusty-tools-4e2cf878".into()))
    );
    assert_eq!(
        state.config.search_index, "main",
        "a review_pr call must not rewrite the server's startup index"
    );
}

/// The review itself runs under each repo's resolved index, not the server's
/// startup `"main"` — the original #8649 bug, pinned at the runner boundary.
#[tokio::test]
#[serial_test::serial]
async fn resolved_config_reaches_the_review_for_each_repo() {
    // SAFETY: test-only env mutation, serialised via #[serial].
    unsafe { std::env::set_var("TRUSTY_REVIEW_AUTH_MODE", "cli") };
    let mut state = state_with(Some(two_repo_registry()), true);
    state.config.github_token = "test-token-8649".into();

    let (ci, _) = run_captured(&state, "duettoresearch", "code-intelligence").await;
    let (tt, _) = run_captured(&state, "bobmatnyc", "trusty-tools").await;
    // SAFETY: restore env before any assertion can unwind the test.
    unsafe { std::env::remove_var("TRUSTY_REVIEW_AUTH_MODE") };

    let ci = ci.expect("the code-intelligence review must run");
    assert_eq!(ci.index, "code-intelligence-9f1c2e3a");
    assert_eq!(
        ci.search_health,
        Ok(()),
        "a resolved review keeps live search"
    );
    let tt = tt.expect("the trusty-tools review must run");
    assert_eq!(tt.index, "trusty-tools-4e2cf878");
}

/// An unreadable registry with search not required runs a DEGRADED diff-only
/// review with NO index: null search and analyze clients, never `"main"`.
#[tokio::test]
#[serial_test::serial]
async fn unreadable_registry_degrades_to_diff_only_when_search_is_not_required() {
    // SAFETY: test-only env mutation, serialised via #[serial].
    unsafe { std::env::set_var("TRUSTY_REVIEW_AUTH_MODE", "cli") };
    let mut state = state_with(None, false);
    state.config.github_token = "test-token-8649".into();

    let (seen, envelope) = run_captured(&state, "acme", "widget").await;
    // SAFETY: restore env before any assertion can unwind the test.
    unsafe { std::env::remove_var("TRUSTY_REVIEW_AUTH_MODE") };

    let seen = seen.unwrap_or_else(|| panic!("the review must run degraded: {envelope}"));
    assert_eq!(seen.index, "", "a degraded review must carry no index");
    let reason = seen
        .search_health
        .expect_err("search must be the null client");
    assert!(
        reason.contains("DEGRADED") && reason.contains("acme/widget"),
        "{reason}"
    );
    assert_eq!(
        seen.analyze_ready,
        Some(false),
        "analyze must be the null client"
    );
}

/// With search required, an unreadable registry stays the named error.
#[tokio::test]
async fn registry_failure_is_an_error_when_search_is_required() {
    let state = state_with(None, true);
    let err = resolve_pr_index(&state, "acme", "widget")
        .await
        .expect_err("a required search must not degrade");
    let msg = err.to_string();
    assert!(matches!(err, RepoIndexError::Registry { .. }), "{msg}");
    assert!(
        msg.contains("acme/widget") && msg.contains("\"widget\""),
        "{msg}"
    );

    let search = Registry::new(None);
    let bad = resolve_repo_index(&search, "", "widget", None).await;
    assert!(matches!(bad, Err(RepoIndexError::InvalidRepo { .. })));
}

/// A repo with no index is an in-band error naming the repo and the index id,
/// returned before auth, even with search not required (#6687).
#[tokio::test]
#[serial_test::serial]
async fn missing_index_error_names_the_repo_and_the_index_id() {
    // SAFETY: test-only env mutation, serialised via #[serial].
    unsafe { std::env::set_var("TRUSTY_REVIEW_AUTH_MODE", "app") };
    let state = state_with(Some(two_repo_registry()), false);
    let args = json!({ "owner": "acme", "repo": "unindexed-repo", "pr": 5906 });
    let result = call_tool("review_pr", &args, &state).await;
    // SAFETY: restore env before any assertion can unwind the test.
    unsafe { std::env::remove_var("TRUSTY_REVIEW_AUTH_MODE") };

    let envelope = match result {
        Ok(envelope) => envelope,
        Err(e) => panic!("expected a named-index error envelope, got {e:?}"),
    };
    assert_eq!(envelope["isError"], json!(true), "{envelope}");
    let text = envelope["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("acme/unindexed-repo"), "{text}");
    assert!(text.contains("\"unindexed-repo\""), "{text}");
}

/// The filtered listing is asked first; the unfiltered one only on a miss.
#[tokio::test]
async fn identity_filter_is_queried_first_and_the_full_list_only_on_a_miss() {
    let search = Registry::new(Some(two_repo_registry()));
    let hit = resolve_repo_index(&search, "bobmatnyc", "trusty-tools", None).await;
    assert_eq!(hit.ok().as_deref(), Some("trusty-tools-4e2cf878"));
    let _ = resolve_repo_index(&search, "acme", "widget", None).await;
    let listings = search
        .listings
        .lock()
        .map(|l| l.clone())
        .unwrap_or_default();
    assert_eq!(
        listings,
        vec![
            Some("bobmatnyc/trusty-tools".to_string()),
            Some("acme/widget".to_string()),
            None,
        ]
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

/// Among one repo's indexes: most recently used, then shorter root, then id.
#[test]
fn identity_tiebreak_prefers_recent_use_then_short_root_then_id() {
    let key = "acme/widget";
    let used = |id: &str, root: &str, at: Option<u64>| IndexIdentity {
        last_used_unix: at,
        ..entry(id, root, Some(key))
    };
    let recent = vec![
        used("widget", "/w", Some(10)),
        used("widget-wt", "/w/.worktrees/a", Some(20)),
        used("widget-old", "/x", None),
    ];
    assert_eq!(
        select_repo_index(&recent, key, "widget", None).as_deref(),
        Ok("widget-wt")
    );
    let tied = vec![
        used("b", "/w/long", Some(5)),
        used("a", "/w", Some(5)),
        used("c", "/w", Some(5)),
    ];
    assert_eq!(
        select_repo_index(&tied, key, "widget", None).as_deref(),
        Ok("a")
    );
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

/// A recorded identity that does not parse — garbage, empty or blank — cannot
/// be verified, so the bare-name fallback refuses it. (`RepoIdentity::parse`
/// is lenient: most free text parses, as `unknown-owner/<slug>`, and is then
/// refused as another repo's.)
#[test]
fn unreadable_identity_is_refused_by_the_name_fallback() {
    for raw in ["ssh://not@an-identity", "", "   "] {
        let reg = vec![entry("widget", "/src/widget", Some(raw))];
        let err = select_repo_index(&reg, "acme/widget", "widget", None)
            .expect_err("an unverifiable identity must not be served by name");
        assert!(err.contains("unreadable repo_identity"), "{raw:?}: {err}");
    }
}

/// A fork: the pinned index exists but is recorded for another owner. The miss
/// names the pin and its identity, and the pin is not used.
#[test]
fn foreign_pin_is_named_in_the_miss() {
    let reg = vec![entry("tt-fork", "/src/tt", Some("alice/trusty-tools"))];
    let err = select_repo_index(
        &reg,
        "bobmatnyc/trusty-tools",
        "trusty-tools",
        Some("tt-fork"),
    )
    .expect_err("another owner's index must not be auto-accepted");
    assert!(err.contains("\"tt-fork\""), "{err}");
    assert!(err.contains("alice/trusty-tools"), "{err}");
}
