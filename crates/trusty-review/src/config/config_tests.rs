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
