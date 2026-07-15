//! Pre-flight configuration validation.
//!
//! [`ConfigValidator`] runs a set of cross-field invariants over a loaded
//! [`Config`] and returns a list of [`ConfigError`] values describing every
//! problem found (not just the first). This is intentionally non-fatal at
//! the type level — callers can decide whether to bail out, print warnings,
//! or filter the error set by category.
//!
//! Validation is split into:
//!
//! - **Fatal errors** — returned in the result vector; the binary should
//!   refuse to proceed unless the user passes `--no-validate`.
//! - **Non-fatal warnings** — emitted via `tracing::warn!` and *not* added
//!   to the error vector; they describe suspicious-but-runnable
//!   configurations.
//!
//! # Example
//!
//! ```ignore
//! use tga::core::config::{Config, ConfigValidator};
//!
//! let cfg = Config::load(std::path::Path::new("config.yaml"))?;
//! let errors = ConfigValidator::new(&cfg).validate();
//! if !errors.is_empty() {
//!     for e in &errors {
//!         eprintln!("config error: {e}");
//!     }
//!     std::process::exit(1);
//! }
//! ```

use std::path::Path;

use super::{expand_path, Config};

/// A single configuration validation failure.
///
/// Variants are intentionally fine-grained so callers can categorize and
/// route specific failure modes (e.g. CI may tolerate a missing GitHub
/// token but not a missing repo path).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A configured repository path does not exist on disk.
    #[error("Repository path does not exist: {path}")]
    RepoNotFound {
        /// The configured filesystem path (after `~` expansion).
        path: String,
    },

    /// The configured output directory is not writable.
    #[error("Output directory is not writable: {path}")]
    OutputNotWritable {
        /// The configured output directory.
        path: String,
    },

    /// GitHub PR fetching is enabled but no token is configured.
    #[error("GitHub token required when fetch_prs = true")]
    MissingGitHubToken,

    /// Bitbucket is partially configured (at least one of
    /// `workspace`/`repo_slug` is missing while `fetch_prs = true`).
    #[error("Bitbucket config incomplete: {field} is required when fetch_prs = true")]
    IncompleteBitbucketConfig {
        /// The missing field name (`workspace` or `repo_slug`).
        field: String,
    },

    /// Bitbucket PR fetching is enabled but no usable auth credentials are
    /// available — neither a Bearer `token` nor a `username` + `app_password`
    /// pair (in config or env).
    #[error(
        "Bitbucket auth required when fetch_prs = true: \
         supply either `token` (or BITBUCKET_TOKEN) or `username` + `app_password` \
         (or BITBUCKET_APP_PASSWORD)"
    )]
    MissingBitbucketAuth,

    /// JIRA is partially configured (at least one of url/username/token is
    /// set, but not all of them).
    #[error("JIRA config incomplete: {field} is required")]
    IncompleteJiraConfig {
        /// The missing field name (`url`, `username`, or `token`).
        field: String,
    },

    /// LLM classification is enabled but the chosen provider has no API key
    /// available (neither in config nor in the environment).
    #[error("LLM API key missing for provider '{provider}'")]
    MissingLlmKey {
        /// Provider name (`openrouter`, `openai`, …).
        provider: String,
    },

    /// Two flags or settings contradict each other.
    #[error("Conflicting config: {message}")]
    Conflict {
        /// Human-readable description of the conflict.
        message: String,
    },

    /// `pm.azure_devops` failed schema validation (empty projects, non-cloud
    /// URL, missing PAT). The message is forwarded from
    /// [`AzureDevOpsConfig::validate`](crate::core::config::AzureDevOpsConfig::validate)
    /// verbatim so users see the same text as runtime errors.
    #[error("Invalid Azure DevOps config: {message}")]
    InvalidAzureDevOpsConfig {
        /// Forwarded error text from `AzureDevOpsConfig::validate`.
        message: String,
    },
}

/// Runs a battery of validation checks against a [`Config`].
///
/// Construct with [`ConfigValidator::new`] and call [`Self::validate`] to
/// collect the (possibly empty) list of errors.
pub struct ConfigValidator<'a> {
    config: &'a Config,
}

impl<'a> ConfigValidator<'a> {
    /// Wrap a `Config` reference for validation.
    pub fn new(config: &'a Config) -> Self {
        Self { config }
    }

    /// Run every check and return all errors found.
    ///
    /// Non-fatal warnings (e.g. a configured-but-empty team roster) are
    /// emitted via `tracing::warn!` and **not** added to the returned
    /// vector. An empty result means the config passes validation.
    pub fn validate(&self) -> Vec<ConfigError> {
        let mut errors = Vec::new();
        self.check_repositories(&mut errors);
        self.check_output_dir(&mut errors);
        self.check_github_token(&mut errors);
        self.check_bitbucket_config(&mut errors);
        self.check_jira_config(&mut errors);
        self.check_azure_devops(&mut errors);
        self.check_llm_config(&mut errors);
        self.check_conflicting_flags(&mut errors);
        errors
    }

    /// Verify `pm.azure_devops` (when present) passes its own schema
    /// validation.
    ///
    /// Why: `Config::validate` already calls `AzureDevOpsConfig::validate`,
    /// but the CLI preflight only runs `ConfigValidator` — without this
    /// hook, a config with `fetch_prs: true` and empty `project`/`projects`
    /// would pass preflight, reach `AdoPrFetcher::fetch_pr`, and silently
    /// return `Ok(None)` for every PR (follow-up to issue #91).
    fn check_azure_devops(&self, errors: &mut Vec<ConfigError>) {
        let Some(ado) = self.config.azure_devops_config() else {
            return;
        };
        if let Err(e) = ado.validate() {
            errors.push(ConfigError::InvalidAzureDevOpsConfig {
                message: e.to_string(),
            });
        }
    }

    /// Verify every configured repository path exists on disk.
    ///
    /// Empty `repositories` is *not* a fatal validation error here — the
    /// existing [`Config::validate`] handles the "at least one repo
    /// required" rule. This check focuses on path-on-disk correctness.
    fn check_repositories(&self, errors: &mut Vec<ConfigError>) {
        if self.config.repositories.is_empty() {
            tracing::warn!("no repositories configured — `tga collect` will be a no-op");
            return;
        }
        for repo in &self.config.repositories {
            let expanded = expand_path(&repo.path);
            if !expanded.exists() {
                errors.push(ConfigError::RepoNotFound {
                    path: expanded.display().to_string(),
                });
            }
        }
    }

    /// Verify the output directory (if configured) is writable.
    ///
    /// If the directory does not yet exist, attempt to create it; failure
    /// to create is reported as `OutputNotWritable`.
    fn check_output_dir(&self, errors: &mut Vec<ConfigError>) {
        let Some(output) = self.config.output.as_ref() else {
            return;
        };
        let Some(dir) = output.directory.as_ref() else {
            return;
        };
        let expanded = expand_path(dir);
        if !is_dir_writable(&expanded) {
            errors.push(ConfigError::OutputNotWritable {
                path: expanded.display().to_string(),
            });
        }
    }

    /// Verify GitHub is configured with a token when PR fetching is on.
    fn check_github_token(&self, errors: &mut Vec<ConfigError>) {
        let Some(gh) = self.config.github.as_ref() else {
            return;
        };
        if gh.fetch_prs {
            let token_present = gh
                .token
                .as_deref()
                .map(|t| !t.trim().is_empty())
                .unwrap_or(false);
            let env_present = std::env::var(trusty_common::env_vars::ENV_GITHUB_TOKEN)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false);
            if !token_present && !env_present {
                errors.push(ConfigError::MissingGitHubToken);
            }
        }
    }

    /// Verify Bitbucket Cloud is configured with workspace, repo, and at
    /// least one usable auth mode when PR fetching is on.
    ///
    /// Auth modes (checked in order):
    /// 1. Bearer `token` (from config or `BITBUCKET_TOKEN`).
    /// 2. Basic auth: `username` + `app_password` (or `BITBUCKET_APP_PASSWORD`).
    ///
    /// A wholly absent `bitbucket:` block is fine — the integration is just
    /// off.
    fn check_bitbucket_config(&self, errors: &mut Vec<ConfigError>) {
        let Some(bb) = self.config.bitbucket.as_ref() else {
            return;
        };
        if !bb.fetch_prs {
            return;
        }

        if bb
            .workspace
            .as_deref()
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            errors.push(ConfigError::IncompleteBitbucketConfig {
                field: "workspace".into(),
            });
        }
        if bb
            .repo_slug
            .as_deref()
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            errors.push(ConfigError::IncompleteBitbucketConfig {
                field: "repo_slug".into(),
            });
        }

        let nonempty = |o: Option<&str>| o.map(|s| !s.trim().is_empty()).unwrap_or(false);
        let token_in_cfg = nonempty(bb.token.as_deref());
        let token_in_env = std::env::var("BITBUCKET_TOKEN")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let user = nonempty(bb.username.as_deref());
        let pwd_in_cfg = nonempty(bb.app_password.as_deref());
        let pwd_in_env = std::env::var("BITBUCKET_APP_PASSWORD")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);

        let has_token = token_in_cfg || token_in_env;
        let has_basic = user && (pwd_in_cfg || pwd_in_env);

        if !has_token && !has_basic {
            errors.push(ConfigError::MissingBitbucketAuth);
        }
    }

    /// Verify JIRA configuration is complete *if any field is set*.
    ///
    /// A wholly absent JIRA block is fine — the integration is just off.
    /// A *partially* populated block is almost certainly a typo or a
    /// missed env-var substitution and is treated as fatal.
    fn check_jira_config(&self, errors: &mut Vec<ConfigError>) {
        let Some(jira) = self.config.jira.as_ref() else {
            return;
        };
        let url = jira.url.as_deref().unwrap_or("").trim();
        let username = jira.username.as_deref().unwrap_or("").trim();
        let token = jira.token.as_deref().unwrap_or("").trim();
        let any = !url.is_empty() || !username.is_empty() || !token.is_empty();
        if !any {
            return;
        }
        if url.is_empty() {
            errors.push(ConfigError::IncompleteJiraConfig {
                field: "url".into(),
            });
        }
        if username.is_empty() {
            errors.push(ConfigError::IncompleteJiraConfig {
                field: "username".into(),
            });
        }
        if token.is_empty() {
            errors.push(ConfigError::IncompleteJiraConfig {
                field: "token".into(),
            });
        }
    }

    /// Verify the LLM provider has an API key available when LLM
    /// classification is enabled.
    fn check_llm_config(&self, errors: &mut Vec<ConfigError>) {
        let Some(cls) = self.config.classification.as_ref() else {
            return;
        };
        if !cls.use_llm {
            return;
        }
        let provider = cls.llm_provider.as_str();
        let (config_key, env_keys): (Option<&str>, &[&str]) = match provider {
            "openrouter" => (cls.openrouter_api_key.as_deref(), &["OPENROUTER_API_KEY"]),
            "openai" => (None, &["OPENAI_API_KEY"]),
            // Bedrock uses the AWS default credential chain (env vars, shared
            // config, IAM role, etc.) — no single API-key check applies. Skip
            // the missing-key validation; the SDK will surface auth errors at
            // call time.
            "bedrock" => return,
            // "auto" — accept either provider's key.
            _ => (
                cls.openrouter_api_key.as_deref(),
                &["OPENROUTER_API_KEY", "OPENAI_API_KEY"],
            ),
        };
        let cfg_present = config_key.map(|k| !k.trim().is_empty()).unwrap_or(false);
        let env_present = env_keys.iter().any(|k| {
            std::env::var(k)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false)
        });
        if !cfg_present && !env_present {
            errors.push(ConfigError::MissingLlmKey {
                provider: provider.to_string(),
            });
        }
    }

    /// Detect contradictory toggle combinations.
    ///
    /// Currently checks:
    /// - Classification confidence threshold is in `[0.0, 1.0]`.
    /// - Min coverage percentage is in `[0.0, 100.0]`.
    fn check_conflicting_flags(&self, errors: &mut Vec<ConfigError>) {
        if let Some(cls) = self.config.classification.as_ref() {
            if !(0.0..=1.0).contains(&cls.confidence_threshold) {
                errors.push(ConfigError::Conflict {
                    message: format!(
                        "classification.confidence_threshold ({}) must be in [0.0, 1.0]",
                        cls.confidence_threshold
                    ),
                });
            }
            if !(0.0..=100.0).contains(&cls.min_coverage_pct) {
                errors.push(ConfigError::Conflict {
                    message: format!(
                        "classification.min_coverage_pct ({}) must be in [0.0, 100.0]",
                        cls.min_coverage_pct
                    ),
                });
            }
        }
    }
}

/// Return true if `path` is a directory that we can write to.
///
/// If the directory does not exist, attempt to create it (and its parents);
/// success implies writability and returns `true`. Failure to create or a
/// path that exists but is not a directory returns `false`.
fn is_dir_writable(path: &Path) -> bool {
    if !path.exists() {
        // Attempt to create — if we can, it's writable.
        return std::fs::create_dir_all(path).is_ok();
    }
    if !path.is_dir() {
        return false;
    }
    // Probe writability by creating and removing a temp file.
    let probe = path.join(".tga-write-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
#[path = "validator_tests.rs"]
mod tests;
