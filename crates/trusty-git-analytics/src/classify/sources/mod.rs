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
//! and per-source client implementations in [`jira`], [`github_issues`],
//! [`linear`], [`shortcut`], [`confluence`], and [`datadog`].
//!
//! Test: unit-test ticket-key extraction regexes in `tests::*_ticket_extraction`;
//! integration-test the resolver with mock HTTP in
//! `tests::resolver_uses_cached_jira_result`.

pub mod confluence;
pub mod datadog;
pub mod github_issues;
pub mod jira;
pub mod linear;
pub mod resolver;
pub mod shortcut;

pub use resolver::ExternalSourceResolver;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Which external system a source entry describes.
///
/// Why: the `type:` discriminant in the YAML config selects the source
/// implementation, keeping the YAML schema stable even as new source types
/// are added.
/// What: a closed `#[serde(tag = "type", rename_all = "snake_case")]` enum
/// covering JIRA, GitHub Issues, Linear, Shortcut, Confluence, and Datadog.
/// Test: round-trip deserialization covered by `config::tests`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    /// JIRA Cloud / Server integration.
    Jira(JiraSourceConfig),
    /// GitHub Issues integration.
    GithubIssues(GithubIssuesSourceConfig),
    /// Linear GraphQL API integration (issue #272).
    Linear(LinearSourceConfig),
    /// Shortcut (formerly Clubhouse) REST API integration (issue #273).
    Shortcut(ShortcutSourceConfig),
    /// Confluence page-label classification source (issue #274).
    ///
    /// Note: lower default confidence (0.80) — Confluence labels are
    /// typically organisational rather than work-type indicators. Treat as
    /// informational signal only; pair with a higher-confidence source.
    Confluence(ConfluenceSourceConfig),
    /// Datadog deployment-event classification source (issue #275).
    ///
    /// Confidence defaults to 0.95 — deployment evidence is a strong
    /// work-type signal.
    Datadog(DatadogSourceConfig),
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

/// Configuration for one Linear source (issue #272).
///
/// Why: Linear issue types are the highest-confidence classification signal
/// for Linear-heavy shops. Like JIRA, the issue type tells us far more than
/// the commit message alone.
/// What: holds the API key environment variable, the optional team-key filter
/// (so Linear keys are not confused with identically-shaped JIRA keys), and
/// field mappings for issue type, labels, and cycle.
/// Test: see `tests::linear_source_config_deserializes` and the
/// `linear::tests` module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LinearSourceConfig {
    /// Name of the environment variable carrying the Linear Personal API Key.
    /// The key is read at runtime — never stored in config files.
    #[serde(default = "default_linear_api_key_env")]
    pub api_key_env: String,

    /// Limit lookups to issues whose team prefix matches one of these keys
    /// (e.g. `["ENG", "BE"]`). Empty = all teams (no filter).
    ///
    /// Linear key prefixes are user-defined per workspace. The filter is the
    /// primary disambiguation mechanism between Linear keys and JIRA keys
    /// that share the same `TEAM-NNN` shape.
    #[serde(default)]
    pub team_keys: Vec<String>,

    /// Maps Linear issue fields to TGA category strings.
    #[serde(default)]
    pub field_mappings: LinearFieldMappings,
}

fn default_linear_api_key_env() -> String {
    "LINEAR_API_TOKEN".to_string()
}

/// Field-level mappings for Linear issue metadata.
///
/// Why: different Linear workspaces use different issue-type names; mapping
/// them explicitly keeps the mapping auditable.
/// What: three sub-maps — `issue_type`, `labels`, and `cycle`.
/// Test: covered by `linear::tests::classify_issue_type_wins`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct LinearFieldMappings {
    /// Maps Linear issue-type names (e.g. `"Bug"`, `"Feature"`) to TGA
    /// categories.
    #[serde(default)]
    pub issue_type: HashMap<String, String>,

    /// Maps Linear label names to TGA categories.
    #[serde(default)]
    pub labels: HashMap<String, String>,

    /// Maps Linear cycle / sprint names to TGA categories.
    #[serde(default)]
    pub cycle: HashMap<String, String>,
}

/// Configuration for one Shortcut (formerly Clubhouse) source (issue #273).
///
/// Why: Shortcut story types are explicit classification signals — `bug`,
/// `feature`, `chore` map directly to TGA categories with high confidence.
/// What: holds the API token environment variable, the workspace ID (for
/// log context), and field mappings for story type, labels, and workflow
/// state.
/// Test: see `tests::shortcut_source_config_deserializes` and the
/// `shortcut::tests` module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShortcutSourceConfig {
    /// Name of the environment variable carrying the Shortcut API token.
    /// The token is read at runtime — never stored in config files.
    #[serde(default = "default_shortcut_api_token_env")]
    pub api_token_env: String,

    /// Workspace identifier. Used for log context only; the REST API does
    /// not require it in the URL (stories are globally unique by ID).
    #[serde(default)]
    pub workspace_id: String,

    /// Maps Shortcut story fields to TGA category strings.
    #[serde(default)]
    pub field_mappings: ShortcutFieldMappings,
}

fn default_shortcut_api_token_env() -> String {
    "SHORTCUT_API_TOKEN".to_string()
}

/// Field-level mappings for Shortcut story metadata.
///
/// Why: Shortcut's three story types (`bug`, `feature`, `chore`) map cleanly
/// to TGA categories; labels and workflow state provide fallback signals.
/// What: three sub-maps — `story_type`, `labels`, and `workflow_state`.
/// Test: covered by `shortcut::tests::classify_story_type_wins`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ShortcutFieldMappings {
    /// Maps Shortcut story-type strings (`"bug"`, `"feature"`, `"chore"`)
    /// to TGA categories.
    #[serde(default)]
    pub story_type: HashMap<String, String>,

    /// Maps Shortcut label names to TGA categories.
    #[serde(default)]
    pub labels: HashMap<String, String>,

    /// Maps Shortcut workflow-state names to TGA categories (optional).
    #[serde(default)]
    pub workflow_state: HashMap<String, String>,
}

/// Configuration for one Confluence source (issue #274).
///
/// Why: Confluence page labels carry organisational classification signal —
/// runbooks suggest devops work, RFCs suggest refactoring. Signal quality
/// is lower than JIRA/Linear so the default confidence is 0.80.
/// What: holds the base URL, auth env vars, and label-to-category mappings.
/// Test: see `tests::confluence_source_config_deserializes` and the
/// `confluence::tests` module.
///
/// Note: treat this as an informational signal only. Pair with a
/// higher-confidence source (JIRA, Linear) for production use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfluenceSourceConfig {
    /// Confluence instance base URL, e.g.
    /// `"https://yourco.atlassian.net/wiki"`.
    pub base_url: String,

    /// Name of the env var carrying the Confluence API token.
    #[serde(default = "default_confluence_token_env")]
    pub token_env: String,

    /// Name of the env var carrying the Confluence user email (for
    /// Atlassian Cloud Basic auth).
    #[serde(default = "default_confluence_email_env")]
    pub email_env: String,

    /// Maps Confluence page label names to TGA categories.
    ///
    /// Example:
    /// ```yaml
    /// label_mappings:
    ///   runbook: devops
    ///   rfc: tech_debt_refactoring
    ///   incident: bug_fix
    /// ```
    #[serde(default)]
    pub label_mappings: HashMap<String, String>,
}

fn default_confluence_token_env() -> String {
    "CONFLUENCE_API_TOKEN".to_string()
}

fn default_confluence_email_env() -> String {
    "CONFLUENCE_EMAIL".to_string()
}

/// Configuration for one Datadog deployment-event source (issue #275).
///
/// Why: deployment evidence is the strongest possible work-type signal —
/// if a commit was deployed, it is devops work regardless of the message.
/// What: holds the Datadog API / application key env vars, the optional
/// DD site, the optional service filter, the category to assign, and the
/// override confidence (default 0.95).
/// Test: see `tests::datadog_source_config_deserializes` and the
/// `datadog::tests` module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DatadogSourceConfig {
    /// Name of the env var carrying the Datadog API key.
    #[serde(default = "default_datadog_api_key_env")]
    pub api_key_env: String,

    /// Name of the env var carrying the Datadog application key.
    #[serde(default = "default_datadog_app_key_env")]
    pub app_key_env: String,

    /// Datadog site (e.g. `"datadoghq.com"`, `"datadoghq.eu"`).
    /// Defaults to `"datadoghq.com"` when absent.
    #[serde(default)]
    pub dd_site: Option<String>,

    /// Optional service name filter. When set, only deployment events for
    /// this service name are considered. When absent, any deployment event
    /// matching the commit SHA is accepted.
    #[serde(default)]
    pub service: Option<String>,

    /// TGA category to assign when a deployment event is found.
    /// Defaults to `"devops"`.
    #[serde(default = "default_datadog_category")]
    pub default_category: String,

    /// Override confidence for this source. Defaults to 0.95.
    #[serde(default)]
    pub confidence: Option<f64>,
}

fn default_datadog_api_key_env() -> String {
    "DATADOG_API_KEY".to_string()
}

fn default_datadog_app_key_env() -> String {
    "DATADOG_APP_KEY".to_string()
}

fn default_datadog_category() -> String {
    "devops".to_string()
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

    /// Why: the Linear source config must round-trip through YAML so teams
    /// can drop a `type: linear` block into their rules file.
    /// What: deserialize a minimal Linear config and assert fields.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn linear_source_config_deserializes() {
        let yaml = r#"
type: linear
api_key_env: LINEAR_API_TOKEN
team_keys: ["ENG"]
field_mappings:
  issue_type:
    Bug: bug_fix
  labels: {}
  cycle: {}
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Linear(l) => {
                assert_eq!(l.api_key_env, "LINEAR_API_TOKEN");
                assert_eq!(l.team_keys, vec!["ENG"]);
                assert_eq!(
                    l.field_mappings.issue_type.get("Bug"),
                    Some(&"bug_fix".to_string())
                );
            }
            other => panic!("expected Linear variant, got {other:?}"),
        }
    }

    /// Why: the Shortcut source config must round-trip through YAML.
    /// What: deserialize a minimal Shortcut config and assert fields.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn shortcut_source_config_deserializes() {
        let yaml = r#"
type: shortcut
api_token_env: SHORTCUT_API_TOKEN
workspace_id: myco
field_mappings:
  story_type:
    bug: bug_fix
  labels: {}
  workflow_state: {}
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Shortcut(s) => {
                assert_eq!(s.api_token_env, "SHORTCUT_API_TOKEN");
                assert_eq!(s.workspace_id, "myco");
                assert_eq!(
                    s.field_mappings.story_type.get("bug"),
                    Some(&"bug_fix".to_string())
                );
            }
            other => panic!("expected Shortcut variant, got {other:?}"),
        }
    }

    /// Why: the Confluence source config must round-trip through YAML.
    /// What: deserialize a minimal Confluence config and assert fields.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn confluence_source_config_deserializes() {
        let yaml = r#"
type: confluence
base_url: "https://myco.atlassian.net/wiki"
token_env: CONFLUENCE_API_TOKEN
email_env: CONFLUENCE_EMAIL
label_mappings:
  runbook: devops
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Confluence(c) => {
                assert_eq!(c.base_url, "https://myco.atlassian.net/wiki");
                assert_eq!(c.token_env, "CONFLUENCE_API_TOKEN");
                assert_eq!(c.label_mappings.get("runbook"), Some(&"devops".to_string()));
            }
            other => panic!("expected Confluence variant, got {other:?}"),
        }
    }

    /// Why: the Datadog source config must round-trip through YAML.
    /// What: deserialize a minimal Datadog config and assert fields.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn datadog_source_config_deserializes() {
        let yaml = r#"
type: datadog
api_key_env: DATADOG_API_KEY
app_key_env: DATADOG_APP_KEY
default_category: devops
confidence: 0.95
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Datadog(d) => {
                assert_eq!(d.api_key_env, "DATADOG_API_KEY");
                assert_eq!(d.default_category, "devops");
                assert!(d
                    .confidence
                    .map(|c| (c - 0.95_f64).abs() < f64::EPSILON)
                    .unwrap_or(false));
            }
            other => panic!("expected Datadog variant, got {other:?}"),
        }
    }
}
