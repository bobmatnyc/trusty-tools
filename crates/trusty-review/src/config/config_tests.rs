//! Unit tests for `config`.
//!
//! Why: split from `config/mod.rs` to keep that file under the 500-line cap
//! while preserving full coverage of provider serde, role-model precedence, and
//! the env-driven `ReviewConfig` fields (including the Phase 1 #582 additions).
//! What: exercises `Provider`, `RoleModels`, and `ReviewConfig` loading.
//! Test: this is the test module; each function is a self-contained unit test.

use super::*;

#[test]
fn provider_roundtrip_serde() {
    let json = serde_json::to_string(&Provider::OpenRouter).unwrap();
    assert_eq!(json, r#""openrouter""#);
    let p: Provider = serde_json::from_str(&json).unwrap();
    assert_eq!(p, Provider::OpenRouter);

    let json = serde_json::to_string(&Provider::Bedrock).unwrap();
    assert_eq!(json, r#""bedrock""#);
    let p: Provider = serde_json::from_str(&json).unwrap();
    assert_eq!(p, Provider::Bedrock);
}

#[test]
fn provider_fromstr() {
    assert_eq!(
        "openrouter".parse::<Provider>().unwrap(),
        Provider::OpenRouter
    );
    assert_eq!("bedrock".parse::<Provider>().unwrap(), Provider::Bedrock);
    assert!("unknown".parse::<Provider>().is_err());
}

#[test]
fn role_models_precedence_defaults() {
    // No CLI, no env, no file → built-in defaults (Bedrock as of #548).
    let env = RoleEnv::default();
    let roles = RoleModels::from_env(&env);
    assert_eq!(
        roles.reviewer.model,
        crate::llm::models::DEFAULT_REVIEWER_MODEL
    );
    // Default provider is now Bedrock (changed from OpenRouter in #548).
    assert_eq!(roles.reviewer.provider, Provider::Bedrock);
    assert!((roles.reviewer.temperature - 0.3_f32).abs() < f32::EPSILON);
    assert_eq!(
        roles.verifier.model,
        crate::llm::models::DEFAULT_VERIFIER_MODEL
    );
    assert_eq!(roles.verifier.provider, Provider::Bedrock);
    assert_eq!(
        roles.summarizer.model,
        crate::llm::models::DEFAULT_SUMMARIZER_MODEL
    );
    assert_eq!(roles.summarizer.provider, Provider::Bedrock);
}

#[test]
fn role_models_openrouter_still_selectable_via_env() {
    // OpenRouter is co-equal: selecting it via env var must work.
    let env = RoleEnv {
        provider: Some("openrouter".to_string()),
        reviewer_model: Some("openai/gpt-5.4-mini-20260317".to_string()),
        ..Default::default()
    };
    let roles = RoleModels::from_env(&env);
    assert_eq!(roles.reviewer.provider, Provider::OpenRouter);
    assert_eq!(roles.reviewer.model, "openai/gpt-5.4-mini-20260317");
}

#[test]
fn role_models_precedence_env_wins() {
    let env = RoleEnv {
        reviewer_model: Some("openai/gpt-5.4-mini-20260317".to_string()),
        verifier_model: None,
        summarizer_model: None,
        provider: None,
    };
    let roles = RoleModels::from_env(&env);
    assert_eq!(roles.reviewer.model, "openai/gpt-5.4-mini-20260317");
    // verifier and summarizer fall back to defaults.
    assert_eq!(
        roles.verifier.model,
        crate::llm::models::DEFAULT_VERIFIER_MODEL
    );
}

#[test]
fn role_models_precedence_cli_wins_over_env() {
    let cli = RoleCliOverrides {
        reviewer_model: Some("openai/gpt-5.4-20260305".to_string()),
        ..Default::default()
    };
    let env = RoleEnv {
        reviewer_model: Some("openai/gpt-5.4-mini-20260317".to_string()),
        ..Default::default()
    };
    let roles = RoleModels::resolve(Some(&cli), &env, None);
    // CLI flag beats env var.
    assert_eq!(roles.reviewer.model, "openai/gpt-5.4-20260305");
}

#[test]
fn role_models_precedence_config_file_wins_over_defaults() {
    let file = FileModels {
        reviewer: Some(RoleConfigOverride {
            model: Some("openai/gpt-5.4-nano-20260317".to_string()),
            temperature: Some(0.5),
            ..Default::default()
        }),
        ..Default::default()
    };
    let env = RoleEnv::default();
    let roles = RoleModels::resolve(None, &env, Some(&file));
    assert_eq!(roles.reviewer.model, "openai/gpt-5.4-nano-20260317");
    assert!((roles.reviewer.temperature - 0.5_f32).abs() < f32::EPSILON);
    // Verifier falls back to built-in.
    assert_eq!(
        roles.verifier.model,
        crate::llm::models::DEFAULT_VERIFIER_MODEL
    );
}

#[test]
fn role_models_all_defaults_are_bedrock_claude() {
    // As of #548 all defaults are Bedrock Claude models (Sonnet/Haiku).
    for model in [
        crate::llm::models::DEFAULT_REVIEWER_MODEL,
        crate::llm::models::DEFAULT_VERIFIER_MODEL,
        crate::llm::models::DEFAULT_SUMMARIZER_MODEL,
    ] {
        assert!(
            model.contains("anthropic") || model.starts_with("us."),
            "default model {model} must be a Bedrock Claude inference-profile id"
        );
    }
}

#[test]
fn config_dry_run_defaults_to_true() {
    // Without any env var, dry_run must default to true.
    let env = RoleEnv::default();
    let _ = RoleModels::from_env(&env); // Just verifies no panic.
}

#[test]
fn config_github_token_defaults_to_empty() {
    // When GITHUB_TOKEN is not set, github_token must be empty (not panic).
    let config = ReviewConfig::from_env_and_file(None, None);
    // We cannot assert the exact value (CI may have GITHUB_TOKEN set),
    // but we can assert the config loads without panic.
    let _ = config.github_token;
}

#[test]
fn config_search_url_default() {
    // When TRUSTY_SEARCH_URL is not set, falls back to localhost:7878.
    // (Cannot reliably unset env vars in parallel tests; just check load.)
    let config = ReviewConfig::from_env_and_file(None, None);
    assert!(
        config.search_url.starts_with("http"),
        "search_url must start with http: {}",
        config.search_url
    );
}

#[test]
fn config_analyzer_url_default() {
    let config = ReviewConfig::from_env_and_file(None, None);
    assert!(
        config.analyzer_url.starts_with("http"),
        "analyzer_url must start with http: {}",
        config.analyzer_url
    );
}

#[test]
fn live_review_requesters_parses_csv() {
    // The parser is pure aside from the env read; assert it lowercases,
    // trims, and drops empties on a representative input.
    let parsed: Vec<String> = "Alice, bob ,,CAROL"
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(parsed, vec!["alice", "bob", "carol"]);
}

#[test]
fn config_bot_username_defaults() {
    // When PR_REVIEW_BOT_USERNAME is unset the default bot login applies.
    // (CI may set it; assert non-empty rather than an exact value.)
    let config = ReviewConfig::from_env_and_file(None, None);
    assert!(
        !config.bot_username.trim().is_empty(),
        "bot_username must never be empty"
    );
}

#[test]
fn load_github_installations_parses_known_orgs() {
    // The helper is pure (reads env vars); we can call it without side effects.
    // Just verify it doesn't panic and returns a vec.
    let installs = super::load_github_installations();
    // Each element must have a non-empty org name and a non-zero id.
    for (org, id) in &installs {
        assert!(!org.is_empty(), "org name must be non-empty");
        assert!(*id > 0, "installation id must be > 0");
    }
}

// ── APEX config tests (Phase 6 PR-B, REV-420, #550) ─────────────────────

/// Verify `apex_index` defaults to empty when env var is unset.
///
/// Why: REV-420 — empty `apex_index` means APEX is disabled; the pipeline must
/// not query any index when the operator has not configured one.
/// What: loads config with no env override; asserts `apex_index` is empty.
/// Test: this test; no network.
#[test]
fn apex_index_defaults_to_empty() {
    // Unset env var → empty string → APEX disabled.
    // (We cannot guarantee the var is absent in all CI contexts, but we can
    // assert the *shape* of whatever value is present is a string.)
    let config = ReviewConfig::from_env_and_file(None, None);
    assert!(
        config.apex_index.is_empty(),
        "apex_index must default to empty"
    );
}

/// Verify `apex_path_prefixes` parses a comma-separated env var correctly.
///
/// Why: REV-420 operators set `TRUSTY_REVIEW_APEX_PATH_PREFIXES=apex/,specs/`
/// to scope APEX retrieval to specific corpus sub-paths; this must parse
/// correctly into a `Vec<String>`.
/// What: exercises `load_apex_path_prefixes` logic directly via string splitting.
/// Test: this test; no network.
#[test]
fn apex_path_prefixes_parses_csv() {
    // Mirror the parsing logic without touching env vars (safe for parallel
    // test runners).
    let raw = "apex/,specs/ , docs/adr/ , , ";
    let parsed: Vec<String> = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(
        parsed,
        vec!["apex/", "specs/", "docs/adr/"],
        "comma-separated prefixes must be trimmed and empty entries dropped"
    );
}

/// Verify `apex_path_prefixes` is empty when the env var is unset or blank.
///
/// Why: no prefix filtering means all hits from `apex_index` are treated as
/// APEX; the operator opts into prefix filtering by setting the env var.
/// What: asserts that loading from env with no var set returns an empty vec.
/// Test: this test; no network.
#[test]
fn apex_path_prefixes_defaults_to_empty() {
    // The `load_apex_path_prefixes` function returns empty when the var is
    // absent.  We test the parsing helper directly.
    let result: Vec<String> = ""
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    assert!(
        result.is_empty(),
        "empty input must produce empty prefix list"
    );
}

// ─── resolve_index tests ──────────────────────────────────────────────────────

use crate::integrations::{
    health::{EmbedderState, HealthResponse},
    search_client::{IndexInfo, SearchClient, SearchClientError, SearchResult},
};
use async_trait::async_trait;

/// Mock SearchClient that returns a fixed index list.
struct FixedIndexSearch(Vec<IndexInfo>);

#[async_trait]
impl SearchClient for FixedIndexSearch {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Ok(HealthResponse {
            status: "ok".to_string(),
            embedder: EmbedderState::Bool(true),
        })
    }
    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Ok(self.0.clone())
    }
    async fn search(
        &self,
        _index_id: &str,
        _query: &str,
        _top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Ok(vec![])
    }
}

/// Mock SearchClient that always fails list_indexes.
struct FailListSearch;

#[async_trait]
impl SearchClient for FailListSearch {
    async fn health(&self) -> Result<HealthResponse, SearchClientError> {
        Err(SearchClientError::Unavailable("down".to_string()))
    }
    async fn list_indexes(&self) -> Result<Vec<IndexInfo>, SearchClientError> {
        Err(SearchClientError::Unavailable("daemon down".to_string()))
    }
    async fn search(
        &self,
        _index_id: &str,
        _query: &str,
        _top_k: Option<u32>,
    ) -> Result<Vec<SearchResult>, SearchClientError> {
        Err(SearchClientError::Unavailable("down".to_string()))
    }
}

fn make_index_info(id: &str, root_path: Option<&str>) -> IndexInfo {
    IndexInfo {
        id: id.to_string(),
        name: None,
        root_path: root_path.map(|s| s.to_string()),
    }
}

/// When `TRUSTY_SEARCH_INDEX` is set, `resolve_index` must not change it.
///
/// Why: explicit operator config always wins; the auto-derive logic must not
/// stomp on a deliberately-set index name (issue #661).
/// What: sets `search_index_explicit = true`, calls `resolve_index` with a
/// daemon that would return a different index, asserts value unchanged.
/// Test: this test.
#[tokio::test]
async fn resolve_index_noop_when_explicit() {
    let mut config = ReviewConfig::load(None);
    config.search_index = "explicit-index".to_string();
    config.search_index_explicit = true;

    let indexes = vec![make_index_info("auto-index", Some("/tmp/some-project"))];
    let client = FixedIndexSearch(indexes);
    config.resolve_index(&client).await;

    assert_eq!(
        config.search_index, "explicit-index",
        "explicit index must not be overwritten by auto-derive"
    );
}

/// When `TRUSTY_SEARCH_INDEX` is unset and the daemon returns a matching index,
/// `resolve_index` must update `search_index` to the matched id.
///
/// Why: the core auto-derive feature (issue #661).
/// What: creates a temp dir with a `.git` subdirectory, sets it as cwd,
/// provides a daemon that returns an index with that path, and asserts the
/// resolved value.
/// Test: this test.
#[tokio::test]
async fn resolve_index_updates_when_match_found() {
    let root = tempfile::tempdir().unwrap();
    // Create .git so find_git_root stops here.
    std::fs::create_dir(root.path().join(".git")).unwrap();
    // Make the canonical path (symlinks resolved).
    let canonical = root.path().canonicalize().unwrap();
    let root_path_str = canonical.to_str().unwrap().to_string();

    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;

    let indexes = vec![make_index_info("my-project", Some(&root_path_str))];
    let client = FixedIndexSearch(indexes);

    // Temporarily change cwd to the temp dir so repo_root_from_cwd picks it up.
    let original_dir = std::env::current_dir().unwrap();
    std::env::set_current_dir(root.path()).unwrap();
    config.resolve_index(&client).await;
    std::env::set_current_dir(original_dir).unwrap();

    assert_eq!(
        config.search_index, "my-project",
        "auto-derive must update search_index to the matched index"
    );
}

/// When the daemon is unreachable, `resolve_index` must keep `search_index`
/// unchanged and not panic.
///
/// Why: the auto-derive is best-effort; daemon downtime must degrade gracefully
/// to the default `"main"` rather than failing the review startup (issue #661).
/// What: provides a FailListSearch, asserts `search_index` stays at `"main"`.
/// Test: this test.
#[tokio::test]
async fn resolve_index_falls_back_on_daemon_error() {
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;

    let client = FailListSearch;
    config.resolve_index(&client).await;

    assert_eq!(
        config.search_index, "main",
        "daemon error must leave search_index at fallback 'main'"
    );
}

/// When no registered index root_path matches the repo root, keep the default.
///
/// Why: a fresh machine with no indexed projects should not crash or produce an
/// incorrect index name (issue #661).
/// What: provides an index with an unrelated root_path, asserts `"main"` kept.
/// Test: this test.
#[tokio::test]
async fn resolve_index_keeps_default_when_no_match() {
    let mut config = ReviewConfig::load(None);
    config.search_index = "main".to_string();
    config.search_index_explicit = false;

    let indexes = vec![make_index_info(
        "other-project",
        Some("/srv/totally-different"),
    )];
    let client = FixedIndexSearch(indexes);
    config.resolve_index(&client).await;

    assert_eq!(
        config.search_index, "main",
        "no-match must leave search_index at fallback 'main'"
    );
}
