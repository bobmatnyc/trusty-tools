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
    integrations::search_client::IndexIdentity,
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    mcp::tools::call_tool,
    models::ReviewResult,
    service::AppState,
};

use super::{PrIndex, resolve_pr_index, review_pr_with};
// #8651: the registry fake is shared with the other PR surfaces' tests.
use crate::pipeline::pr_index::tests::{Registry, entry, two_repo_registry};

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
    let envelope = review_pr_with(
        &args,
        state,
        move |config, _input, deps, _options| async move {
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
            crate::pipeline::ReviewOutcome {
                result: ReviewResult::new("o", "r", 7, "PR #7", ""),
                context_sources: Vec::new(),
            }
        },
    )
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

/// The operator's own default — no `require_search` override at all
/// (`config.context.require_search = None`) — still degrades an unreadable
/// registry on the MCP tool's `Interactive` surface, rather than erroring.
/// Regression: swapping `resolve_pr_index`'s `InvocationSurface::Interactive`
/// for `Hosted` fails this test, because `Hosted` requires search by default
/// and the registry failure would surface as a bare `RepoIndexError` instead
/// of a `PrIndex::DiffOnly` degrade (#8649).
#[tokio::test]
async fn default_require_search_degrades_review_pr_on_unreadable_registry() {
    let mut state = state_with(None, false);
    state.config.context.require_search = None;
    let index = resolve_pr_index(&state, "acme", "widget")
        .await
        .expect("an unreadable registry must degrade under the operator's default");
    assert!(matches!(index, PrIndex::DiffOnly(_)), "{index:?}");
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

/// A daemon that ignores the `?repo_identity=` filter and returns its full
/// index list regardless still gets the right answer, because
/// `resolve_repo_index` re-verifies each identity locally against the
/// "scoped" list rather than trusting the daemon's filtering blindly (#8649).
#[tokio::test]
async fn filter_ignoring_daemon_still_resolves_the_right_repo_and_refuses_the_wrong_one() {
    let search = Registry::new_ignoring_filter(two_repo_registry());
    let acme = resolve_repo_index(&search, "acme", "widget", None).await;
    assert!(
        matches!(acme, Err(RepoIndexError::NoIndex { .. })),
        "{acme:?}"
    );
    let tt = resolve_repo_index(&search, "bobmatnyc", "trusty-tools", None).await;
    assert_eq!(tt.ok().as_deref(), Some("trusty-tools-4e2cf878"));
}

/// A GitHub owner literally named `unknown-owner` must be refused, not treated
/// as a real owner — that string is also what an owner-less local index's
/// identity canonicalises to, so accepting it would let one owner's PR review
/// silently resolve to an unrelated, ownerless index (#8649).
#[tokio::test]
async fn owner_named_unknown_owner_sentinel_is_invalid() {
    let search = Registry::new(Some(vec![entry("widget", "/src/widget", None)]));
    let err = resolve_repo_index(&search, "unknown-owner", "widget", None)
        .await
        .expect_err("the unknown-owner sentinel must not resolve as a real owner");
    assert!(matches!(err, RepoIndexError::InvalidRepo { .. }), "{err}");
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

/// #9192: `review_pr` hands the review its parsed PR context and request, and
/// the envelope carries the outcome's `context_sources`.
#[tokio::test]
#[serial_test::serial]
async fn review_pr_passes_the_parsed_context_to_the_review() {
    use crate::models::{ContextSourceRecord, SourceState};
    use crate::pipeline::{CallerContext, OptionalContextRequest};

    // SAFETY: test-only env mutation, serialised via #[serial].
    unsafe { std::env::set_var("TRUSTY_REVIEW_AUTH_MODE", "cli") };
    let mut state = state_with(Some(two_repo_registry()), true);
    state.config.github_token = "test-token-9192".into();
    let seen: Arc<Mutex<Option<(CallerContext, OptionalContextRequest)>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let args = json!({
        "owner": "bobmatnyc", "repo": "trusty-tools", "pr": 7,
        "include_pr_body": true, "pr_description": "why", "pr_discussion": 3,
    });
    let envelope = review_pr_with(&args, &state, move |_config, input, _deps, options| {
        if let Ok(mut slot) = sink.lock() {
            *slot = Some((input.caller_context, options.request));
        }
        async move {
            crate::pipeline::ReviewOutcome {
                result: ReviewResult::new("o", "r", 7, "PR #7", ""),
                context_sources: vec![ContextSourceRecord::new("pr_body", SourceState::Used)],
            }
        }
    })
    .await;
    // SAFETY: restore env before any assertion can unwind the test.
    unsafe { std::env::remove_var("TRUSTY_REVIEW_AUTH_MODE") };

    let envelope = envelope.expect("a mistyped legacy param never fails the call");
    let (caller, request) = seen
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .expect("review ran");
    assert_eq!(caller.pr_description.as_deref(), Some("why"));
    assert_eq!(
        caller.pr_discussion, None,
        "a mistyped legacy param is ignored"
    );
    assert!(request.include_pr_body);
    assert_eq!(envelope["context_sources"][0]["source"], "pr_body");
}

/// #9194 AC1: `report_context: true` reaches the review's request, and the
/// envelope carries `context_sources` even when the list is empty
/// (amendment 3), with no row fabricated.
#[tokio::test]
#[serial_test::serial]
async fn review_pr_report_context_turns_the_ledger_on() {
    use crate::pipeline::OptionalContextRequest;

    // SAFETY: test-only env mutation, serialised via #[serial].
    unsafe { std::env::set_var("TRUSTY_REVIEW_AUTH_MODE", "cli") };
    let mut state = state_with(Some(two_repo_registry()), true);
    state.config.github_token = "test-token-9194".into();
    let seen: Arc<Mutex<Option<OptionalContextRequest>>> = Arc::default();
    let sink = Arc::clone(&seen);
    let args =
        json!({"owner": "bobmatnyc", "repo": "trusty-tools", "pr": 7, "report_context": true});
    let envelope = review_pr_with(&args, &state, move |_config, _input, _deps, options| {
        if let Ok(mut slot) = sink.lock() {
            *slot = Some(options.request);
        }
        async move {
            crate::pipeline::ReviewOutcome {
                result: ReviewResult::new("o", "r", 7, "PR #7", ""),
                context_sources: Vec::new(),
            }
        }
    })
    .await;
    // SAFETY: restore env before any assertion can unwind the test.
    unsafe { std::env::remove_var("TRUSTY_REVIEW_AUTH_MODE") };

    let envelope = envelope.expect("a boolean report_context is valid");
    let request = seen
        .lock()
        .ok()
        .and_then(|s| s.clone())
        .expect("review ran");
    assert!(request.report_context && request.ledger_enabled());
    assert!(!request.requested_new(), "report_context is no new input");
    assert_eq!(envelope["context_sources"], json!([]), "{envelope}");
}

/// #9192: a mistyped `include_pr_body` is a protocol error, before any review.
#[tokio::test]
async fn review_pr_rejects_a_mistyped_include_pr_body_before_reviewing() {
    let state = state_with(Some(two_repo_registry()), true);
    let args =
        json!({"owner": "bobmatnyc", "repo": "trusty-tools", "pr": 7, "include_pr_body": "yes"});
    let out = review_pr_with(&args, &state, |_c, _i, _d, _o| async {
        panic!("the review must not run")
    })
    .await;
    assert!(
        matches!(out, Err(crate::mcp::tools::ToolError::InvalidParams(m)) if m.contains("include_pr_body"))
    );
}
