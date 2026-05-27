//! Multi-source classification: external ticket systems as high-confidence
//! classification signals.
//!
//! Why: commit messages are often the worst signal for work category — a bare
//! `PROJ-1234 update handler` carries zero semantic content, but the JIRA
//! issue behind it has an explicit issue type. This module provides a
//! pluggable source layer that fetches issue metadata and maps it to TGA
//! classification categories before the commit-message rule tiers run.
//!
//! What: defines [`SourceConfig`] (the YAML-deserialisable config for each
//! source), [`ExternalSourceResolver`] (the per-run cache and dispatcher),
//! and per-source client implementations in [`jira`] and [`github_issues`].
//!
//! Test: unit-test ticket-key extraction regexes in `tests::*_ticket_extraction`;
//! integration-test the resolver with mock HTTP in
//! `tests::resolver_uses_cached_jira_result`.

pub mod github_issues;
pub mod jira;
pub mod resolver;

pub use resolver::ExternalSourceResolver;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Which external system a source entry describes.
///
/// Why: the `type:` discriminant in the YAML config selects the source
/// implementation, keeping the YAML schema stable even as new source types
/// are added.
/// What: a closed `#[serde(tag = "type", rename_all = "snake_case")]` enum
/// covering JIRA and GitHub Issues (issue #260 MVP).
/// Test: round-trip deserialization covered by `config::tests`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    /// JIRA Cloud / Server integration.
    Jira(JiraSourceConfig),
    /// GitHub Issues integration.
    GithubIssues(GithubIssuesSourceConfig),
}

/// Configuration for one JIRA source.
///
/// Why: JIRA issue type is the highest-confidence classification signal —
/// even a vague commit message like `PROJ-1234 fixes things` becomes
/// unambiguous once we know the issue type is `Bug`.
/// What: holds the JIRA base URL, the environment-variable name carrying the
/// API token, the project keys to scope queries to, and field mappings that
/// convert JIRA issue_type/labels/components to TGA category strings.
/// Test: see `tests::jira_source_config_deserializes` and
/// `tests::jira_source_config_email_env_deserializes` in this module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JiraSourceConfig {
    /// JIRA Cloud / Server base URL (e.g. `https://yourco.atlassian.net`).
    pub base_url: String,

    /// Name of the environment variable that carries the JIRA API token.
    /// The token is read at runtime — never stored in config files.
    ///
    /// When the env var is unset, external lookups for this source are
    /// skipped with a `tracing::warn!`.
    #[serde(default = "default_jira_token_env")]
    pub token_env: String,

    /// Optional JIRA username (literal email) for Basic-auth requests.
    ///
    /// If set, this literal string is used as the Basic-auth username
    /// together with the token from `token_env`. Kept for backward
    /// compatibility. Prefer `email_env` for new configurations so the
    /// email address is not stored in the config file.
    ///
    /// If both `username` and `email_env` are present, `username` wins.
    /// If neither is set, the token is sent as a Bearer token (uncommon
    /// for Atlassian Cloud, which requires Basic auth).
    #[serde(default)]
    pub username: Option<String>,

    /// Name of the environment variable carrying the JIRA user email.
    ///
    /// Mirrors `token_env` for the email half of Atlassian Cloud Basic
    /// auth. The env var is resolved at request time (not at config-load)
    /// so `export JIRA_EMAIL=you@co.com` before running `tga` is
    /// sufficient even if the config file was loaded before the export.
    ///
    /// Example YAML:
    /// ```yaml
    /// type: jira
    /// base_url: "https://yourco.atlassian.net"
    /// token_env: JIRA_API_TOKEN
    /// email_env: JIRA_EMAIL          # <-- recommended for Atlassian Cloud
    /// ```
    ///
    /// Ignored when `username` is also set (literal wins).
    #[serde(default)]
    pub email_env: Option<String>,

    /// Limit queries to issues under these project keys.
    /// Empty list means no project filter (query any project found in commits).
    #[serde(default)]
    pub project_keys: Vec<String>,

    /// Maps JIRA `issue_type` values to TGA category strings.
    ///
    /// Example:
    /// ```yaml
    /// issue_type:
    ///   Story: new_feature
    ///   Bug: bug_fix
    ///   Task: tech_debt_refactoring
    /// ```
    #[serde(default)]
    pub field_mappings: JiraFieldMappings,
}

fn default_jira_token_env() -> String {
    "JIRA_API_TOKEN".to_string()
}

/// Field-level mappings for JIRA issue metadata.
///
/// Why: different JIRA setups use different issue-type names; mapping them
/// to canonical TGA categories here keeps the mapping explicit and auditable.
/// What: three sub-maps — `issue_type`, `labels`, and `components`.
/// Test: covered by `jira::tests::field_mapping_resolves_issue_type`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct JiraFieldMappings {
    /// Maps JIRA issue-type names (e.g. `"Story"`, `"Bug"`) to TGA categories.
    #[serde(default)]
    pub issue_type: HashMap<String, String>,

    /// Maps JIRA label strings to TGA categories.
    #[serde(default)]
    pub labels: HashMap<String, String>,

    /// Maps JIRA component names to TGA categories.
    #[serde(default)]
    pub components: HashMap<String, String>,
}

/// Configuration for one GitHub Issues source.
///
/// Why: teams using GitHub Issues as their tracker gain the same signal-boost
/// as JIRA users — a commit referencing `#123 fix login` becomes `bug_fix`
/// when the linked issue has a `bug` label.
/// What: holds the repo slug (or env var), the token env var, and a
/// label-to-category mapping.
/// Test: see `tests::github_issues_config_deserializes`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GithubIssuesSourceConfig {
    /// Repository slug in `owner/name` form, e.g. `"acme/widgets"`.
    ///
    /// When a commit references `#NNN` (without an `org/repo#NNN` qualifier),
    /// this is the repo used to look up the issue.
    pub repo: String,

    /// Name of the environment variable carrying the GitHub API token.
    /// The token is read at runtime — never stored in config files.
    ///
    /// When the env var is unset, external lookups for this source are
    /// skipped with a `tracing::warn!`.
    #[serde(default = "default_github_token_env")]
    pub token_env: String,

    /// Maps GitHub label names to TGA category strings.
    ///
    /// Example:
    /// ```yaml
    /// label_mappings:
    ///   bug: bug_fix
    ///   enhancement: new_feature
    ///   dependencies: tech_debt_refactoring
    /// ```
    #[serde(default)]
    pub label_mappings: HashMap<String, String>,
}

fn default_github_token_env() -> String {
    "GITHUB_TOKEN".to_string()
}

/// A resolved classification signal from an external source.
///
/// Why: the resolver returns this thin type so the pipeline can treat all
/// sources uniformly — regardless of whether the signal came from JIRA issue
/// type, JIRA labels, or GitHub issue labels, the pipeline only needs the
/// final `category` string and a `confidence`.
/// What: bundles the resolved TGA `category` string and the confidence to
/// attach to the resulting [`crate::classify::tiers::ClassificationResult`].
/// Test: covered by `resolver::tests::resolve_returns_jira_signal_for_known_key`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternalSignal {
    /// TGA category string (e.g. `"bug_fix"`, `"new_feature"`).
    pub category: String,
    /// Confidence to assign this verdict (0.0–1.0).
    pub confidence: f64,
    /// Human-readable provenance label for logs and the `method` field.
    pub source: String,
}

/// Confidence assigned to a classification verdict derived from an external
/// ticket-type mapping (JIRA issue type, GitHub label).
///
/// Why: external ticket-type signals are very high quality — a JIRA `Bug`
/// issue type is more reliable than any commit-message heuristic. Setting
/// this to 0.92 places it above regex-rule verdicts (≤0.95 for cc-feat) but
/// below Tier-0 manual overrides (1.0).
pub const EXTERNAL_SOURCE_CONFIDENCE: f64 = 0.92;

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the YAML schema for `sources:` must round-trip correctly so users
    /// who add a `sources:` block to their rules file get the expected config.
    /// What: deserialize a JIRA source config from YAML and assert fields.
    /// Test: pure deserialization; no HTTP.
    #[test]
    fn jira_source_config_deserializes() {
        let yaml = r#"
type: jira
base_url: "https://acme.atlassian.net"
token_env: "MY_JIRA_TOKEN"
project_keys: ["PROJ", "ENG"]
field_mappings:
  issue_type:
    Story: new_feature
    Bug: bug_fix
  labels:
    ktlo: tech_debt_refactoring
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Jira(j) => {
                assert_eq!(j.base_url, "https://acme.atlassian.net");
                assert_eq!(j.token_env, "MY_JIRA_TOKEN");
                assert_eq!(j.project_keys, vec!["PROJ", "ENG"]);
                assert_eq!(
                    j.field_mappings.issue_type.get("Story"),
                    Some(&"new_feature".to_string())
                );
                assert_eq!(
                    j.field_mappings.issue_type.get("Bug"),
                    Some(&"bug_fix".to_string())
                );
                assert_eq!(
                    j.field_mappings.labels.get("ktlo"),
                    Some(&"tech_debt_refactoring".to_string())
                );
            }
            other => panic!("expected Jira variant, got {other:?}"),
        }
    }

    /// Why: `email_env:` is the recommended way for Atlassian Cloud users to
    /// supply the Basic-auth email address without storing it in config files.
    /// Before this fix the field didn't exist, so YAML configs with `email_env:`
    /// would have the field silently dropped (serde default = None), causing
    /// Bearer auth and HTTP 403 on every JIRA call.
    /// What: deserialize a Jira source config with `email_env: JIRA_EMAIL` and
    /// assert the field is populated; `username` should remain `None`.
    /// Test: pure deserialization; no HTTP.
    #[test]
    fn jira_source_config_email_env_deserializes() {
        let yaml = r#"
type: jira
base_url: "https://duettoresearch.atlassian.net"
token_env: JIRA_API_TOKEN
email_env: JIRA_EMAIL
project_keys: ["DUE"]
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Jira(j) => {
                assert_eq!(j.email_env.as_deref(), Some("JIRA_EMAIL"));
                assert_eq!(
                    j.username, None,
                    "username must remain None when only email_env is set"
                );
                assert_eq!(j.token_env, "JIRA_API_TOKEN");
            }
            other => panic!("expected Jira variant, got {other:?}"),
        }
    }

    /// Why: the GitHub Issues source config must also round-trip correctly.
    /// What: deserialize a GitHub Issues source from YAML and assert fields.
    /// Test: pure deserialization; no HTTP.
    #[test]
    fn github_issues_source_config_deserializes() {
        let yaml = r#"
type: github_issues
repo: "acme/widgets"
token_env: "GITHUB_TOKEN"
label_mappings:
  bug: bug_fix
  enhancement: new_feature
  dependencies: tech_debt_refactoring
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::GithubIssues(g) => {
                assert_eq!(g.repo, "acme/widgets");
                assert_eq!(g.token_env, "GITHUB_TOKEN");
                assert_eq!(g.label_mappings.get("bug"), Some(&"bug_fix".to_string()));
                assert_eq!(
                    g.label_mappings.get("enhancement"),
                    Some(&"new_feature".to_string())
                );
            }
            other => panic!("expected GithubIssues variant, got {other:?}"),
        }
    }

    /// Why: `extend_defaults: false` is now the default for user-supplied rule
    /// files (issue #259 fix). When a `RuleSet` is deserialized from YAML
    /// without an explicit `extend_defaults:` key, it must default to `false`.
    /// What: deserialize a minimal rules YAML and assert `extend_defaults` is
    /// false.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn rule_set_extend_defaults_is_false_by_default() {
        use crate::classify::rules::RuleSet;
        let yaml = r#"
rules:
  - id: my-rule
    category: bug_fix
    keywords: ["bugfix:"]
"#;
        let rs: RuleSet = serde_yaml::from_str(yaml).expect("deserialize");
        assert!(
            !rs.extend_defaults,
            "extend_defaults must default to false for user-supplied rule files"
        );
    }

    /// Why: custom rule `priority` must default to 110 (above built-in 100)
    /// so user rules win without an explicit `priority:` in every YAML entry.
    /// What: deserialize a rule without a `priority:` field and assert 110.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn rule_priority_defaults_to_110() {
        use crate::classify::rules::Rule;
        let yaml = r#"
id: my-rule
category: bug_fix
keywords: ["bugfix:"]
"#;
        let rule: Rule = serde_yaml::from_str(yaml).expect("deserialize");
        assert_eq!(
            rule.priority, 110,
            "Rule.priority must default to 110 (above built-in 100)"
        );
    }
}
