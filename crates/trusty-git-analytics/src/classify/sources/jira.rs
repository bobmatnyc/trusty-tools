//! JIRA REST API v3 client for commit classification signals.
//!
//! Why: JIRA issue type is the highest-confidence classification signal for
//! JIRA-heavy shops. A commit message like `PROJ-1234 update handler` tells
//! us nothing, but JIRA knows the issue is a `Bug`, making the classification
//! unambiguous. This module extracts JIRA ticket keys from commit messages
//! and fetches their issue type / labels / components to produce a
//! [`super::ExternalSignal`].
//!
//! What: a regex-based ticket-key extractor plus a minimal reqwest-based
//! client that calls `GET /rest/api/3/issue/{key}?fields=issuetype,labels,components`.
//! Credentials are read from the environment variable named in
//! [`super::JiraSourceConfig::token_env`].
//!
//! Test: see `tests::extract_ticket_keys_*` for extractor coverage and
//! `tests::classify_returns_issue_type_signal` for resolver integration.

use std::collections::HashMap;

use regex::Regex;
use serde::Deserialize;
use tracing::warn;

use super::{ExternalSignal, JiraSourceConfig, EXTERNAL_SOURCE_CONFIDENCE};

/// Regex matching a JIRA-style ticket key (`PROJ-1234`).
///
/// Why: ticket keys in commit messages are the join key between a commit and
/// its JIRA issue; extracting them accurately is the critical first step.
/// What: matches one or more uppercase letters (optionally digits), then `-`,
/// then one or more digits. The `\b` word-boundary anchor prevents partial
/// matches inside longer identifiers.
/// Test: covered by `tests::extract_jira_keys_*`.
fn jira_key_regex() -> Regex {
    // Intentionally compiled fresh per call (cheap). Uses a word-boundary
    // anchor on both ends to avoid matching hex color codes (#FFFFFF) or
    // partial strings.
    Regex::new(r"\b([A-Z][A-Z0-9]{0,9}-\d+)\b").expect("static regex is valid")
}

/// Extract all JIRA ticket keys from a commit message.
///
/// Why: a single commit can reference multiple JIRA tickets; collecting all
/// of them maximises the chance of finding one that maps to a configured
/// project key.
/// What: returns a `Vec<String>` of unique ticket keys found in `message`,
/// in left-to-right order of first appearance.
/// Test: covered by `tests::extract_jira_keys_single` and
/// `tests::extract_jira_keys_multiple`.
pub fn extract_jira_keys(message: &str) -> Vec<String> {
    let re = jira_key_regex();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(message) {
        if let Some(key) = cap.get(1) {
            let k = key.as_str().to_string();
            if seen.insert(k.clone()) {
                out.push(k);
            }
        }
    }
    out
}

/// Partial deserialization target for `GET /rest/api/3/issue/{key}`.
///
/// Why: we only need `fields.issuetype.name`, `fields.labels`, and
/// `fields.components[].name` to produce classification signals; fetching
/// the full issue body wastes bandwidth.
/// What: a minimal serde struct covering just those fields.
/// Test: covered by the mock-HTTP tests in resolver integration.
#[derive(Debug, Deserialize)]
pub struct JiraIssue {
    /// Ticket key (e.g. `"PROJ-1234"`).
    pub key: String,
    /// Issue fields.
    pub fields: JiraIssueFields,
}

/// Fields extracted from a JIRA issue.
#[derive(Debug, Deserialize)]
pub struct JiraIssueFields {
    /// Issue type descriptor.
    #[serde(rename = "issuetype")]
    pub issue_type: Option<JiraIssueType>,

    /// Label strings attached to the issue.
    #[serde(default)]
    pub labels: Vec<String>,

    /// Component assignments.
    #[serde(default)]
    pub components: Vec<JiraComponent>,
}

/// JIRA issue-type descriptor.
#[derive(Debug, Deserialize)]
pub struct JiraIssueType {
    /// Type name (e.g. `"Story"`, `"Bug"`, `"Task"`).
    pub name: String,
}

/// JIRA component descriptor.
#[derive(Debug, Deserialize)]
pub struct JiraComponent {
    /// Component name (e.g. `"CI/CD"`, `"Platform"`).
    pub name: String,
}

/// Classify a JIRA issue using the configured `field_mappings`.
///
/// Why: mappings are priority-ordered — issue_type beats labels beats
/// components — because issue type is the most authoritative classification
/// signal in JIRA.
/// What: walks `issue_type → labels → components` in that order; returns the
/// first match as an [`ExternalSignal`].
/// Test: covered by `tests::field_mapping_resolve_issue_type_wins` and
/// `tests::field_mapping_falls_through_to_labels`.
pub fn classify_issue(issue: &JiraIssue, config: &JiraSourceConfig) -> Option<ExternalSignal> {
    let mappings = &config.field_mappings;

    // Priority 1: issue type.
    if let Some(it) = &issue.fields.issue_type {
        if let Some(cat) = mappings.issue_type.get(&it.name) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: EXTERNAL_SOURCE_CONFIDENCE,
                source: format!("jira:issue_type:{}", it.name),
            });
        }
    }

    // Priority 2: labels (first match wins).
    for label in &issue.fields.labels {
        if let Some(cat) = mappings.labels.get(label.as_str()) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: EXTERNAL_SOURCE_CONFIDENCE,
                source: format!("jira:label:{label}"),
            });
        }
    }

    // Priority 3: components (first match wins).
    for comp in &issue.fields.components {
        if let Some(cat) = mappings.components.get(comp.name.as_str()) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: EXTERNAL_SOURCE_CONFIDENCE,
                source: format!("jira:component:{}", comp.name),
            });
        }
    }

    None
}

/// Fetch a JIRA issue by key.
///
/// Why: the HTTP call must be isolated here so the resolver can inject a
/// mock client via its trait seam for testing.
/// What: issues `GET {base_url}/rest/api/3/issue/{key}?fields=issuetype,labels,components`
/// with Basic auth (email + token) or Bearer auth (token-only). Returns
/// `None` on any HTTP error or when the token env var is unset.
/// Test: integration-tested via the resolver with wiremock in
/// `resolver::tests`.
///
/// # Errors
///
/// HTTP failures are downgraded to `warn!` + `None` rather than propagating
/// to the caller. The classification pipeline is designed to be resilient to
/// external source failures — we always fall through to commit-message rules.
pub async fn fetch_issue(
    client: &reqwest::Client,
    config: &JiraSourceConfig,
    key: &str,
    base_url_override: Option<&str>,
) -> Option<JiraIssue> {
    let token = match std::env::var(&config.token_env) {
        Ok(t) if !t.is_empty() => t,
        _ => {
            warn!(
                token_env = %config.token_env,
                "JIRA token env var is unset or empty; skipping external lookup"
            );
            return None;
        }
    };

    let base = base_url_override.unwrap_or(&config.base_url);
    let url = format!("{base}/rest/api/3/issue/{key}?fields=issuetype,labels,components");

    let mut req = client.get(&url);

    // Basic auth (email + token) if username is configured; otherwise Bearer.
    if let Some(username) = &config.username {
        req = req.basic_auth(username, Some(&token));
    } else {
        req = req.bearer_auth(&token);
    }

    match req.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<JiraIssue>().await {
            Ok(issue) => Some(issue),
            Err(e) => {
                warn!(key, error = %e, "failed to parse JIRA issue response");
                None
            }
        },
        Ok(resp) => {
            warn!(
                key,
                status = %resp.status(),
                "JIRA API returned non-success status; skipping"
            );
            None
        }
        Err(e) => {
            warn!(key, error = %e, "JIRA API request failed; skipping");
            None
        }
    }
}

/// Build a `HashMap<String, Option<ExternalSignal>>` from a batch of JIRA
/// keys.
///
/// Why: a 15k-commit run may reference hundreds of unique JIRA keys. Fetching
/// each one on every commit re-classifying would hit rate limits and be slow.
/// This helper fetches each unique key once and caches the result.
/// What: deduplicates `keys`, fetches them concurrently (but not in parallel
/// to stay within JIRA rate limits), and returns a map from key to signal.
/// Test: covered by resolver integration tests.
pub async fn fetch_issues_batch(
    client: &reqwest::Client,
    config: &JiraSourceConfig,
    keys: &[String],
    base_url_override: Option<&str>,
) -> HashMap<String, Option<ExternalSignal>> {
    let mut out = HashMap::new();
    for key in keys {
        if out.contains_key(key) {
            continue;
        }
        let issue = fetch_issue(client, config, key, base_url_override).await;
        let signal = issue.and_then(|iss| classify_issue(&iss, config));
        out.insert(key.clone(), signal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: extracting single JIRA keys from common commit message patterns
    /// is the most critical path in the extractor; regressions here break
    /// all JIRA-backed classification.
    /// What: asserts that bare ticket refs, prefixed messages, and inline
    /// refs all extract the expected key.
    /// Test: pure regex exercise, no HTTP.
    #[test]
    fn extract_jira_keys_single() {
        assert_eq!(extract_jira_keys("PROJ-1234 fix null"), vec!["PROJ-1234"]);
        assert_eq!(
            extract_jira_keys("fix: INFRA-99 update pipeline"),
            vec!["INFRA-99"]
        );
        assert_eq!(extract_jira_keys("ENG-456"), vec!["ENG-456"]);
    }

    /// Why: a commit may reference multiple JIRA tickets; the extractor must
    /// return all of them (in order of appearance) without duplicates.
    /// What: asserts extraction from multi-key messages and deduplication.
    /// Test: pure regex exercise, no HTTP.
    #[test]
    fn extract_jira_keys_multiple_and_dedup() {
        let keys = extract_jira_keys("PROJ-1 and INFRA-2 relate to ENG-3 and PROJ-1 again");
        assert_eq!(keys, vec!["PROJ-1", "INFRA-2", "ENG-3"]);
    }

    /// Why: the extractor must not match lowercase-only identifiers (which
    /// would cause false-positives on things like `fix-123` in branch names).
    /// What: asserts that lowercase keys are not extracted.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_jira_keys_ignores_lowercase() {
        assert!(extract_jira_keys("proj-123 lowercase").is_empty());
        assert!(extract_jira_keys("no ticket here").is_empty());
    }

    /// Why: `classify_issue` must prefer issue-type over labels over
    /// components, even when all three would match.
    /// What: build a `JiraIssue` with all three fields populated and a config
    /// that maps all three; assert issue_type wins.
    /// Test: pure function, no HTTP.
    #[test]
    fn field_mapping_issue_type_wins_over_labels() {
        let issue = JiraIssue {
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                issue_type: Some(JiraIssueType {
                    name: "Bug".to_string(),
                }),
                labels: vec!["enhancement".to_string()],
                components: vec![JiraComponent {
                    name: "Platform".to_string(),
                }],
            },
        };
        let mut config = JiraSourceConfig {
            base_url: "https://acme.atlassian.net".to_string(),
            token_env: "JIRA_API_TOKEN".to_string(),
            username: None,
            project_keys: vec![],
            field_mappings: Default::default(),
        };
        config
            .field_mappings
            .issue_type
            .insert("Bug".to_string(), "bug_fix".to_string());
        config
            .field_mappings
            .labels
            .insert("enhancement".to_string(), "new_feature".to_string());
        config
            .field_mappings
            .components
            .insert("Platform".to_string(), "platform".to_string());

        let signal = classify_issue(&issue, &config).expect("should match");
        assert_eq!(signal.category, "bug_fix");
        assert!(signal.source.contains("issue_type"));
    }

    /// Why: when issue-type is absent or unmapped, labels should be the
    /// next fallback.
    /// What: build an issue with no issue-type mapping but a matching label.
    /// Test: pure function, no HTTP.
    #[test]
    fn field_mapping_falls_through_to_labels() {
        let issue = JiraIssue {
            key: "PROJ-2".to_string(),
            fields: JiraIssueFields {
                issue_type: Some(JiraIssueType {
                    name: "Epic".to_string(), // not in mappings
                }),
                labels: vec!["security".to_string()],
                components: vec![],
            },
        };
        let mut config = JiraSourceConfig {
            base_url: "https://acme.atlassian.net".to_string(),
            token_env: "JIRA_API_TOKEN".to_string(),
            username: None,
            project_keys: vec![],
            field_mappings: Default::default(),
        };
        config
            .field_mappings
            .labels
            .insert("security".to_string(), "security".to_string());

        let signal = classify_issue(&issue, &config).expect("should match via label");
        assert_eq!(signal.category, "security");
        assert!(signal.source.contains("label"));
    }

    /// Why: when no field matches, `classify_issue` must return `None` so
    /// the pipeline can fall through to commit-message rules.
    /// What: build an issue with no mapped fields.
    /// Test: pure function, no HTTP.
    #[test]
    fn field_mapping_returns_none_on_no_match() {
        let issue = JiraIssue {
            key: "PROJ-3".to_string(),
            fields: JiraIssueFields {
                issue_type: Some(JiraIssueType {
                    name: "Unknown-Type".to_string(),
                }),
                labels: vec![],
                components: vec![],
            },
        };
        let config = JiraSourceConfig {
            base_url: "https://acme.atlassian.net".to_string(),
            token_env: "JIRA_API_TOKEN".to_string(),
            username: None,
            project_keys: vec![],
            field_mappings: Default::default(),
        };
        assert!(classify_issue(&issue, &config).is_none());
    }
}
