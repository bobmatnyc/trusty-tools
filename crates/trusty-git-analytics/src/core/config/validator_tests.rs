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

#[test]
fn github_token_required_when_fetch_prs() {
    // Ensure env var is not set for this test.
    // SAFETY: setting env in tests is racy across threads; we use a
    // best-effort save/restore.
    let prev = std::env::var("GITHUB_TOKEN").ok();
    // SAFETY: env var manipulation is unsafe in 2024 edition.
    unsafe {
        std::env::remove_var("GITHUB_TOKEN");
    }

    let mut cfg = empty_config();
    cfg.github = Some(GithubConfig {
        token: None,
        org: None,
        orgs: vec![],
        repo: None,
        fetch_prs: true,
        fetch_pr_reviews: true,
        review_fetch_concurrency: 1,
        ticket_regex: None,
    });
    let errors = ConfigValidator::new(&cfg).validate();
    let found = errors
        .iter()
        .any(|e| matches!(e, ConfigError::MissingGitHubToken));

    // Restore env.
    if let Some(v) = prev {
        // SAFETY: env var manipulation is unsafe in 2024 edition.
        unsafe {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }
    assert!(found, "got {errors:?}");
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

#[test]
fn missing_llm_key_reported() {
    let prev_or = std::env::var("OPENROUTER_API_KEY").ok();
    let prev_oa = std::env::var("OPENAI_API_KEY").ok();
    // SAFETY: env var manipulation is unsafe in 2024 edition.
    unsafe {
        std::env::remove_var("OPENROUTER_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
    }

    let mut cfg = empty_config();
    cfg.classification = Some(ClassificationConfig {
        use_llm: true,
        llm_provider: "openrouter".into(),
        openrouter_api_key: None,
        ..Default::default()
    });
    let errors = ConfigValidator::new(&cfg).validate();
    let found = errors
        .iter()
        .any(|e| matches!(e, ConfigError::MissingLlmKey { .. }));

    // SAFETY: env var manipulation is unsafe in 2024 edition.
    unsafe {
        if let Some(v) = prev_or {
            std::env::set_var("OPENROUTER_API_KEY", v);
        }
        if let Some(v) = prev_oa {
            std::env::set_var("OPENAI_API_KEY", v);
        }
    }
    assert!(found, "got {errors:?}");
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

/// Drop guard that snapshots an env var, removes it for the test, and
/// restores it (or removes it again if it was originally absent) on drop.
///
/// Why: previous closure-based helpers ran restore code *after* the test
/// body, which meant a panicking assertion would skip the restore and
/// leak mutated global env state into the rest of the test run. A Drop
/// guard runs during unwinding too, so panicking tests still restore.
///
/// SAFETY: env-var mutation is `unsafe` in the 2024 edition. We accept
/// it here only inside `#[cfg(test)]` and rely on the guard being the
/// sole writer in the test it covers.
struct EnvVarGuard {
    name: &'static str,
    original: Option<String>,
}

impl EnvVarGuard {
    /// Snapshot `name`, remove it, and arrange for restore on drop.
    fn remove(name: &'static str) -> Self {
        let original = std::env::var(name).ok();
        // SAFETY: 2024-edition env mutation; isolated to this test thread
        // via Drop ordering; cleanup is guaranteed via the impl below.
        unsafe { std::env::remove_var(name) };
        Self { name, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see [`EnvVarGuard::remove`].
        unsafe {
            match self.original.as_deref() {
                Some(v) => std::env::set_var(self.name, v),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

/// Save and clear the Bitbucket env vars for the duration of a closure,
/// then restore them — panic-safe via [`EnvVarGuard`].
fn with_clean_bitbucket_env<F: FnOnce()>(f: F) {
    let _t = EnvVarGuard::remove("BITBUCKET_TOKEN");
    let _p = EnvVarGuard::remove("BITBUCKET_APP_PASSWORD");
    f();
    // Guards drop here (or earlier on panic), restoring env.
}

#[test]
fn bitbucket_requires_workspace_and_repo_slug_when_fetch_prs() {
    with_clean_bitbucket_env(|| {
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
    });
}

#[test]
fn bitbucket_accepts_app_password_pair() {
    with_clean_bitbucket_env(|| {
        let mut cfg = empty_config();
        cfg.bitbucket = Some(BitbucketConfig {
            username: Some("alice".into()),
            app_password: Some("abcd".into()),
            workspace: Some("acme".into()),
            repo_slug: Some("widgets".into()),
            fetch_prs: true,
            ..Default::default()
        });
        let errors = ConfigValidator::new(&cfg).validate();
        assert!(
            !errors.iter().any(|e| matches!(
                e,
                ConfigError::MissingBitbucketAuth | ConfigError::IncompleteBitbucketConfig { .. }
            )),
            "got {errors:?}"
        );
    });
}

#[test]
fn bitbucket_accepts_bearer_token() {
    with_clean_bitbucket_env(|| {
        let mut cfg = empty_config();
        cfg.bitbucket = Some(BitbucketConfig {
            token: Some("workspace-access-token".into()),
            workspace: Some("acme".into()),
            repo_slug: Some("widgets".into()),
            fetch_prs: true,
            ..Default::default()
        });
        let errors = ConfigValidator::new(&cfg).validate();
        assert!(
            !errors.iter().any(|e| matches!(
                e,
                ConfigError::MissingBitbucketAuth | ConfigError::IncompleteBitbucketConfig { .. }
            )),
            "got {errors:?}"
        );
    });
}

#[test]
fn bitbucket_rejects_partial_auth() {
    with_clean_bitbucket_env(|| {
        let mut cfg = empty_config();
        // username without app_password
        cfg.bitbucket = Some(BitbucketConfig {
            username: Some("alice".into()),
            workspace: Some("acme".into()),
            repo_slug: Some("widgets".into()),
            fetch_prs: true,
            ..Default::default()
        });
        let errors = ConfigValidator::new(&cfg).validate();
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ConfigError::MissingBitbucketAuth)),
            "got {errors:?}"
        );
    });
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
    with_clean_bitbucket_env(|| {
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
    });
}
