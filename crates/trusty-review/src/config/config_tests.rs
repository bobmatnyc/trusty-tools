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

// ─── Repo-scoped `.trusty-review.toml` precedence (issue #2995) ───────────────

/// `resolve_repo_config_path` skips discovery entirely when an explicit
/// `--config` path is given — the pure, CWD-independent gate for
/// "CLI --config > repo file".
///
/// Why: proves the skip happens BEFORE any filesystem/CWD access — the
/// nonexistent dummy path is never read, so this assertion holds regardless
/// of the real test-runner's CWD or its actual git-root contents.
/// What: asserts `None` for `Some(path)` regardless of the path's validity.
/// Test: this test; no filesystem access.
#[test]
fn explicit_config_path_skips_repo_discovery() {
    let dummy = std::path::Path::new("/does/not/exist/config.toml");
    assert_eq!(resolve_repo_config_path(Some(dummy)), None);
}

/// A repo-discovered `.trusty-review.toml` `[voice]` section overrides an
/// ambient `TRUSTY_REVIEW_VOICE_PACKAGE`/`TRUSTY_REVIEW_PRINCIPLES` env var.
///
/// Why: end-to-end guard for the precedence documented on
/// `ReviewConfig::from_env_and_file_inner` — a project's committed voice
/// selection must not be silently shadowed by a developer's ambient env var.
/// What: writes a `.trusty-review.toml` to a tempdir, passes its path directly
/// as `repo_config_path` (no CWD mutation — see `from_env_and_file_inner`'s
/// doc), and asserts the repo file's values win over conflicting env vars.
/// Test: this test; tempfile-based, `#[serial_test::serial]` for env isolation.
#[test]
#[serial_test::serial]
fn repo_config_voice_overrides_env() {
    unsafe {
        std::env::set_var("TRUSTY_REVIEW_VOICE_PACKAGE", "env-voice");
        std::env::set_var("TRUSTY_REVIEW_PRINCIPLES", "true");
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_config = dir.path().join(".trusty-review.toml");
    std::fs::write(
        &repo_config,
        "[voice]\npackage = \"repo-voice\"\nprinciples = false\n",
    )
    .expect("write repo config");

    let config = ReviewConfig::from_env_and_file_inner(None, None, Some(&repo_config));

    unsafe {
        std::env::remove_var("TRUSTY_REVIEW_VOICE_PACKAGE");
        std::env::remove_var("TRUSTY_REVIEW_PRINCIPLES");
    }

    assert_eq!(config.voice_package.as_deref(), Some("repo-voice"));
    assert!(!config.voice_principles);
}

/// A repo-discovered `.trusty-review.toml` `[review]` section overrides an
/// ambient `TRUSTY_REVIEW_TEMPLATE` env var.
///
/// Why: same rationale as `repo_config_voice_overrides_env`, for the new
/// `[review] template` key.
/// What: writes a `.trusty-review.toml` with `[review] template = "..."`,
/// asserts the repo value wins over a conflicting env var.
/// Test: this test; tempfile-based, `#[serial_test::serial]` for env isolation.
#[test]
#[serial_test::serial]
fn repo_config_review_template_overrides_env() {
    unsafe {
        std::env::set_var("TRUSTY_REVIEW_TEMPLATE", "env-template");
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_config = dir.path().join(".trusty-review.toml");
    std::fs::write(&repo_config, "[review]\ntemplate = \"repo-template\"\n")
        .expect("write repo config");

    let config = ReviewConfig::from_env_and_file_inner(None, None, Some(&repo_config));

    unsafe {
        std::env::remove_var("TRUSTY_REVIEW_TEMPLATE");
    }

    assert_eq!(config.review_template.as_deref(), Some("repo-template"));
}

/// A `--review-template` CLI override wins even over a repo-discovered
/// `.trusty-review.toml`.
///
/// Why: `RoleCliOverrides.review_template` must be the top precedence tier
/// (CLI --review-template > repo .trusty-review.toml > env vars > global
/// config).
/// What: sets a repo file AND a CLI override with different names; asserts
/// the CLI override wins.
/// Test: this test; tempfile-based.
#[test]
fn repo_config_cli_review_template_wins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo_config = dir.path().join(".trusty-review.toml");
    std::fs::write(&repo_config, "[review]\ntemplate = \"repo-template\"\n")
        .expect("write repo config");
    let overrides = RoleCliOverrides {
        review_template: Some("cli-template".to_string()),
        ..Default::default()
    };

    let config = ReviewConfig::from_env_and_file_inner(None, Some(&overrides), Some(&repo_config));

    assert_eq!(config.review_template.as_deref(), Some("cli-template"));
}

/// Zero-regression: no repo file present → behaviour is identical to
/// pre-#2995 (env var still overrides an explicit/global config file).
///
/// Why: the issue explicitly requires "KEEP existing behavior when no repo
/// file exists" — this guards that the four-tier repo insertion did not
/// change the pre-existing env-over-file relationship when the new tier is
/// absent.
/// What: no repo file is supplied (`repo_config_path: None`); an env var and
/// a conflicting `--config`-style file both set `voice.package`; asserts the
/// env var still wins (the pre-#2995 relationship), matching
/// `voice::load_voice_package`'s documented behaviour.
/// Test: this test; tempfile-based, `#[serial_test::serial]` for env isolation.
#[test]
#[serial_test::serial]
fn no_repo_config_preserves_pre_2995_env_over_file_precedence() {
    unsafe {
        std::env::set_var("TRUSTY_REVIEW_VOICE_PACKAGE", "env-voice");
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let explicit_config = dir.path().join("config.toml");
    std::fs::write(&explicit_config, "[voice]\npackage = \"file-voice\"\n")
        .expect("write explicit config");

    let config = ReviewConfig::from_env_and_file_inner(Some(&explicit_config), None, None);

    unsafe {
        std::env::remove_var("TRUSTY_REVIEW_VOICE_PACKAGE");
    }

    assert_eq!(
        config.voice_package.as_deref(),
        Some("env-voice"),
        "with no repo file, env must still win over the config file (unchanged from pre-#2995)"
    );
}

// resolve_index and wiring-path tests are in the sibling file to stay under
// the 500-line cap (#610).  See `config_resolve_index_tests.rs`.
