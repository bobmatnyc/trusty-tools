use super::*;
use crate::core::config::{
    AzureDevOpsConfig, BitbucketConfig, ClassificationConfig, GithubConfig, JiraConfig,
    OutputConfig, PmConfig, RepositoryConfig,
};
use std::path::PathBuf;

fn empty_config() -> Config {
    Config::default()
}

#[test]
fn empty_config_yields_no_errors() {
    let cfg = empty_config();
    let errors = ConfigValidator::new(&cfg).validate();
    assert!(errors.is_empty(), "got {errors:?}");
}

#[test]
fn missing_repo_path_reported() {
    let mut cfg = empty_config();
    cfg.repositories.push(RepositoryConfig {
        path: PathBuf::from("/nonexistent/path/definitely-not-there-12345"),
        ..Default::default()
    });
    let errors = ConfigValidator::new(&cfg).validate();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ConfigError::RepoNotFound { .. })),
        "got {errors:?}"
    );
}

/// Create a unique temp directory for a test (avoids extra deps).
fn unique_tempdir(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "tga-validator-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

#[test]
fn existing_repo_path_passes() {
    let tmp = unique_tempdir("repo");
    let mut cfg = empty_config();
    cfg.repositories.push(RepositoryConfig {
        path: tmp.clone(),
        ..Default::default()
    });
    let errors = ConfigValidator::new(&cfg).validate();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !errors
            .iter()
            .any(|e| matches!(e, ConfigError::RepoNotFound { .. })),
        "got {errors:?}"
    );
}

/// Build a `GithubConfig` with PR fetching on and the given config token.
fn github_fetching_prs(token: Option<&str>) -> GithubConfig {
    GithubConfig {
        token: token.map(str::to_string),
        org: None,
        orgs: vec![],
        repo: None,
        fetch_prs: true,
        fetch_pr_reviews: true,
        review_fetch_concurrency: 1,
        ticket_regex: None,
        fetch_on_reference: false,
    }
}

/// Why: a config with `fetch_prs = true` and no token anywhere cannot fetch
/// PRs, so validation must say so before the run starts.
/// What: drive `github_token_missing` across every combination of config token
/// and `GITHUB_TOKEN` value, including whitespace-only ones.
/// Test: this test. #5313: it used to remove `GITHUB_TOKEN` process-wide to
/// reach the missing-token branch, which is `unsafe` under the 2024 edition and
/// races every other test thread. The env value is now an argument, so the
/// present-token branches are covered too — they were unreachable before.
#[test]
fn github_token_required_when_fetch_prs() {
    assert!(
        github_token_missing(&github_fetching_prs(None), None),
        "no config token and no env token means PRs cannot be fetched"
    );
    assert!(
        github_token_missing(&github_fetching_prs(Some("  ")), Some("\t")),
        "whitespace-only tokens count as absent"
    );
    assert!(
        !github_token_missing(&github_fetching_prs(Some("ghp_xxx")), None),
        "a config token satisfies the check"
    );
    assert!(
        !github_token_missing(&github_fetching_prs(None), Some("ghp_xxx")),
        "GITHUB_TOKEN satisfies the check"
    );

    let mut off = github_fetching_prs(None);
    off.fetch_prs = false;
    assert!(
        !github_token_missing(&off, None),
        "no token is required when fetch_prs is off"
    );
}

#[test]
fn github_token_in_config_satisfies() {
    let mut cfg = empty_config();
    cfg.github = Some(GithubConfig {
        token: Some("ghp_xxx".into()),
        org: None,
        orgs: vec![],
        repo: None,
        fetch_prs: true,
        fetch_pr_reviews: true,
        review_fetch_concurrency: 1,
        ticket_regex: None,
        fetch_on_reference: false,
    });
    let errors = ConfigValidator::new(&cfg).validate();
    assert!(
        !errors
            .iter()
            .any(|e| matches!(e, ConfigError::MissingGitHubToken)),
        "got {errors:?}"
    );
}

#[test]
fn partial_jira_config_reports_each_missing_field() {
    let mut cfg = empty_config();
    cfg.jira = Some(JiraConfig {
        url: Some("https://x.atlassian.net".into()),
        // username & token missing
        ..Default::default()
    });
    let errors = ConfigValidator::new(&cfg).validate();
    let missing: Vec<&str> = errors
        .iter()
        .filter_map(|e| match e {
            ConfigError::IncompleteJiraConfig { field } => Some(field.as_str()),
            _ => None,
        })
        .collect();
    assert!(missing.contains(&"username"), "got {errors:?}");
    assert!(missing.contains(&"token"), "got {errors:?}");
}

#[test]
fn empty_jira_block_is_fine() {
    let mut cfg = empty_config();
    cfg.jira = Some(JiraConfig::default());
    let errors = ConfigValidator::new(&cfg).validate();
    assert!(
        !errors
            .iter()
            .any(|e| matches!(e, ConfigError::IncompleteJiraConfig { .. })),
        "got {errors:?}"
    );
}

/// Build a `ClassificationConfig` with LLM classification on.
fn llm_enabled(provider: &str, config_key: Option<&str>) -> ClassificationConfig {
    ClassificationConfig {
        use_llm: true,
        llm_provider: provider.into(),
        openrouter_api_key: config_key.map(str::to_string),
        ..Default::default()
    }
}

/// An `env_lookup` that answers only for the named key.
fn only(key: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
    move |k| (k == key).then(|| value.to_string())
}

/// Why: a config with `use_llm = true` and no API key anywhere cannot classify,
/// so validation must say so before the run starts.
/// What: drive `llm_key_missing` across every provider and every source a key
/// can come from, with `env_lookup` supplied per case.
/// Test: this test. #5725: it used to remove `OPENROUTER_API_KEY` and
/// `OPENAI_API_KEY` process-wide to reach the missing-key branch. That is
/// `unsafe` under the 2024 edition, and its restore raced
/// `profile::batch_reviewer::tests::from_slug_with_store_errors_when_no_credential_resolves` —
/// putting the key back mid-resolution and failing that test 40 runs out of 40.
/// The env lookup is now an argument, so the present-key branches are covered
/// too; they were unreachable before.
#[test]
fn llm_key_missing_across_providers_and_env() {
    let none = |_: &str| None;

    assert_eq!(
        llm_key_missing(&llm_enabled("openrouter", None), none),
        Some("openrouter".to_string()),
        "no config key and no env key means classification cannot run"
    );
    assert_eq!(
        llm_key_missing(&llm_enabled("openrouter", Some("   ")), |_| Some(
            "\t".to_string()
        )),
        Some("openrouter".to_string()),
        "whitespace-only keys count as absent"
    );
    assert_eq!(
        llm_key_missing(&llm_enabled("openrouter", Some("sk-or-xxx")), none),
        None,
        "a config key satisfies the check"
    );
    assert_eq!(
        llm_key_missing(
            &llm_enabled("openrouter", None),
            only("OPENROUTER_API_KEY", "sk-or-xxx")
        ),
        None,
        "OPENROUTER_API_KEY satisfies the check"
    );
    assert_eq!(
        llm_key_missing(
            &llm_enabled("openrouter", None),
            only("OPENAI_API_KEY", "sk-oa-xxx")
        ),
        Some("openrouter".to_string()),
        "the wrong provider's key does not satisfy openrouter"
    );

    // `openai` reads no config field — its key must come from the environment.
    assert_eq!(
        llm_key_missing(&llm_enabled("openai", Some("sk-or-xxx")), none),
        Some("openai".to_string()),
        "the openrouter config key is not an openai key"
    );
    assert_eq!(
        llm_key_missing(
            &llm_enabled("openai", None),
            only("OPENAI_API_KEY", "sk-oa-xxx")
        ),
        None
    );

    // `auto` accepts either provider's key.
    assert_eq!(
        llm_key_missing(
            &llm_enabled("auto", None),
            only("OPENAI_API_KEY", "sk-oa-xxx")
        ),
        None
    );
    assert_eq!(
        llm_key_missing(&llm_enabled("auto", None), none),
        Some("auto".to_string())
    );

    // Bedrock uses the AWS credential chain — no API-key check applies.
    assert_eq!(llm_key_missing(&llm_enabled("bedrock", None), none), None);

    // LLM off means no key is required.
    let mut off = llm_enabled("openrouter", None);
    off.use_llm = false;
    assert_eq!(llm_key_missing(&off, none), None);
}

#[test]
fn confidence_threshold_out_of_range_reported() {
    let mut cfg = empty_config();
    cfg.classification = Some(ClassificationConfig {
        confidence_threshold: 1.5,
        ..Default::default()
    });
    let errors = ConfigValidator::new(&cfg).validate();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ConfigError::Conflict { .. })),
        "got {errors:?}"
    );
}

#[test]
fn nonexistent_output_dir_is_created_and_passes() {
    let tmp = unique_tempdir("output");
    let nested = tmp.join("a/b/c");
    let mut cfg = empty_config();
    cfg.output = Some(OutputConfig {
        directory: Some(nested.clone()),
        ..Default::default()
    });
    let errors = ConfigValidator::new(&cfg).validate();
    let exists = nested.exists();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !errors
            .iter()
            .any(|e| matches!(e, ConfigError::OutputNotWritable { .. })),
        "got {errors:?}"
    );
    assert!(exists, "validator should have created the dir");
}

/// Build a `BitbucketConfig` with PR fetching on and the given auth fields.
fn bitbucket_fetching_prs(
    token: Option<&str>,
    username: Option<&str>,
    app_password: Option<&str>,
) -> BitbucketConfig {
    BitbucketConfig {
        token: token.map(str::to_string),
        username: username.map(str::to_string),
        app_password: app_password.map(str::to_string),
        workspace: Some("acme".into()),
        repo_slug: Some("widgets".into()),
        fetch_prs: true,
        ..Default::default()
    }
}

/// Why: Bitbucket PR fetching needs one of two auth modes, and either half of
/// either mode can arrive from the config file or the environment. Validation
/// must reject only the combinations that leave no usable mode.
/// What: drive `bitbucket_auth_missing` across bearer-token and basic-auth
/// sources, including whitespace-only values and the partial-basic case.
/// Test: this test. #5725: the four Bitbucket tests below used to clear
/// `BITBUCKET_TOKEN` and `BITBUCKET_APP_PASSWORD` process-wide through an
/// `EnvVarGuard`, which is `unsafe` under the 2024 edition and races every other
/// test thread. Both env values are now arguments, so the env-supplied branches
/// are covered too; they were unreachable before.
#[test]
fn bitbucket_auth_missing_across_config_and_env() {
    assert!(
        bitbucket_auth_missing(&bitbucket_fetching_prs(None, None, None), None, None),
        "no auth in config and none in the environment"
    );
    assert!(
        !bitbucket_auth_missing(
            &bitbucket_fetching_prs(Some("workspace-access-token"), None, None),
            None,
            None
        ),
        "a config bearer token satisfies the check"
    );
    assert!(
        !bitbucket_auth_missing(
            &bitbucket_fetching_prs(None, None, None),
            Some("env-token"),
            None
        ),
        "BITBUCKET_TOKEN satisfies the check"
    );
    assert!(
        !bitbucket_auth_missing(
            &bitbucket_fetching_prs(None, Some("alice"), Some("abcd")),
            None,
            None
        ),
        "a config username/app-password pair satisfies the check"
    );
    assert!(
        !bitbucket_auth_missing(
            &bitbucket_fetching_prs(None, Some("alice"), None),
            None,
            Some("env-pwd")
        ),
        "BITBUCKET_APP_PASSWORD pairs with a config username"
    );
    assert!(
        bitbucket_auth_missing(
            &bitbucket_fetching_prs(None, Some("alice"), None),
            None,
            None
        ),
        "a username with no password anywhere is partial auth, not auth"
    );
    assert!(
        bitbucket_auth_missing(
            &bitbucket_fetching_prs(None, None, Some("abcd")),
            None,
            None
        ),
        "an app password with no username is partial auth, not auth"
    );
    assert!(
        bitbucket_auth_missing(
            &bitbucket_fetching_prs(Some("  "), Some("alice"), Some("\t")),
            Some(" "),
            Some("")
        ),
        "whitespace-only values count as absent in every position"
    );

    let mut off = bitbucket_fetching_prs(None, None, None);
    off.fetch_prs = false;
    assert!(
        !bitbucket_auth_missing(&off, None, None),
        "no auth is required when fetch_prs is off"
    );
}

/// Why: the workspace and repo_slug checks are separate from auth and consult
/// no environment, so they are provable through the whole validator.
/// What: a Bitbucket block with PR fetching on and neither field set reports
/// both as incomplete.
/// Test: this test itself.
#[test]
fn bitbucket_requires_workspace_and_repo_slug_when_fetch_prs() {
    let mut cfg = empty_config();
    cfg.bitbucket = Some(BitbucketConfig {
        token: Some("bearer".into()),
        fetch_prs: true,
        ..Default::default()
    });
    let errors = ConfigValidator::new(&cfg).validate();
    let missing: Vec<&str> = errors
        .iter()
        .filter_map(|e| match e {
            ConfigError::IncompleteBitbucketConfig { field } => Some(field.as_str()),
            _ => None,
        })
        .collect();
    assert!(missing.contains(&"workspace"), "got {errors:?}");
    assert!(missing.contains(&"repo_slug"), "got {errors:?}");
}

/// Why: the two complete auth modes must pass the whole validator, not just the
/// decision function.
/// What: a bearer token and a username/app-password pair each yield no
/// Bitbucket error. An ambient `BITBUCKET_*` value could only ADD auth, so
/// these assertions need no environment control (#5725).
/// Test: this test itself.
#[test]
fn bitbucket_complete_auth_passes_validation() {
    for bb in [
        bitbucket_fetching_prs(None, Some("alice"), Some("abcd")),
        bitbucket_fetching_prs(Some("workspace-access-token"), None, None),
    ] {
        let mut cfg = empty_config();
        cfg.bitbucket = Some(bb);
        let errors = ConfigValidator::new(&cfg).validate();
        assert!(
            !errors.iter().any(|e| matches!(
                e,
                ConfigError::MissingBitbucketAuth | ConfigError::IncompleteBitbucketConfig { .. }
            )),
            "got {errors:?}"
        );
    }
}

#[test]
fn config_validator_rejects_ado_with_no_projects() {
    // Regression for the preflight gap surfaced after issue #91:
    // ConfigValidator must reject a `pm.azure_devops` block whose
    // `project` is None and `projects` is empty. Otherwise the CLI's
    // `--validate-only` would pass, the collection would proceed, and
    // every ADO PR fetch would silently return Ok(None).
    let mut cfg = empty_config();
    cfg.pm = Some(PmConfig {
        azure_devops: Some(AzureDevOpsConfig {
            organization_url: "https://dev.azure.com/myorg".into(),
            pat: "secret-pat".into(),
            project: None,
            projects: vec![],
            ticket_regex: r"AB#(\d+)".into(),
            team_keys: vec![],
            fetch_on_reference: true,
            fetch_prs: true,
        }),
    });
    let errors = ConfigValidator::new(&cfg).validate();
    assert!(
        errors
            .iter()
            .any(|e| matches!(e, ConfigError::InvalidAzureDevOpsConfig { .. })),
        "expected InvalidAzureDevOpsConfig, got: {errors:?}"
    );
}

#[test]
fn bitbucket_block_off_is_fine() {
    // `fetch_prs` defaults off, so the check returns before it reads anything —
    // no environment control needed (#5725).
    let mut cfg = empty_config();
    cfg.bitbucket = Some(BitbucketConfig::default());
    let errors = ConfigValidator::new(&cfg).validate();
    assert!(
        !errors.iter().any(|e| matches!(
            e,
            ConfigError::MissingBitbucketAuth | ConfigError::IncompleteBitbucketConfig { .. }
        )),
        "got {errors:?}"
    );
}
