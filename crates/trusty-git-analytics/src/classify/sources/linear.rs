//! Linear GraphQL API client for commit classification signals.
//!
//! Why: Linear issue types are the highest-confidence classification signal for
//! teams that use Linear as their tracker. A commit message like `ENG-1234 fix
//! login` tells us nothing, but Linear knows the issue type is `Bug`. This
//! module extracts Linear issue keys from commit messages and fetches their
//! type / labels / cycle to produce a [`super::ExternalSignal`].
//!
//! What: a regex-based key extractor plus a minimal reqwest-based GraphQL
//! client that queries `{ issue(id: "<key>") { type { name } labels { nodes
//! { name } } cycle { name } } }`. Credentials are read from the environment
//! variable named in [`super::LinearSourceConfig::api_key_env`].
//!
//! Test: see `tests::extract_linear_keys_*` for extractor coverage and the
//! resolver integration tests for the full pipeline.

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{ExternalSignal, LinearSourceConfig, EXTERNAL_SOURCE_CONFIDENCE};

/// Linear-specific confidence — same as the global constant (issue type is
/// very authoritative).
const LINEAR_CONFIDENCE: f64 = EXTERNAL_SOURCE_CONFIDENCE;

/// Regex matching a Linear-style ticket key (`TEAM-1234`).
///
/// Why: Linear keys are structurally identical to JIRA keys, but the
/// team-prefix disambiguation happens via `team_keys` config, not regex.
/// What: matches `\b([A-Z][A-Z0-9]{0,9}-\d{1,7})\b` — same bounds as the
/// JIRA extractor (issue #285 guard included via post-filter).
/// Test: covered by `tests::extract_linear_keys_*`.
fn linear_key_regex() -> Regex {
    Regex::new(r"\b([A-Z][A-Z0-9]{0,9}-\d{1,7})").expect("static regex is valid")
}

/// Extract all Linear issue keys from a commit message.
///
/// Why: a single commit can reference multiple Linear issues; collecting all
/// of them maximises the chance of finding one matching a configured team.
/// What: returns a `Vec<String>` of unique keys in left-to-right order.
/// Applies the same trailing-digit post-filter as the JIRA extractor (the
/// regex alone cannot prevent over-consuming a trailing digit run because
/// the Rust `regex` crate lacks lookahead).
/// Test: covered by `tests::extract_linear_keys_single`,
/// `tests::extract_linear_keys_multiple_and_dedup`, and
/// `tests::extract_linear_keys_ignores_lowercase`.
pub fn extract_linear_keys(message: &str) -> Vec<String> {
    let re = linear_key_regex();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(message) {
        if let Some(key) = cap.get(1) {
            let end = key.end();
            // Post-filter: trailing digit → skip (same guard as JIRA extractor).
            if message
                .as_bytes()
                .get(end)
                .is_some_and(|b| b.is_ascii_digit())
            {
                continue;
            }
            let k = key.as_str().to_string();
            if seen.insert(k.clone()) {
                out.push(k);
            }
        }
    }
    out
}

/// Whether a key's prefix matches one of the configured team keys.
///
/// Why: Linear key prefixes are user-defined team identifiers; without a
/// prefix filter the extractor cannot distinguish a Linear key from a JIRA
/// key that happens to share the same shape.
/// What: returns `true` when `team_keys` is empty (no filter) or when the
/// key's prefix matches one entry in the list.
/// Test: covered by `tests::team_key_filter_*`.
pub fn matches_team_key(key: &str, team_keys: &[String]) -> bool {
    if team_keys.is_empty() {
        return true;
    }
    let prefix = key.split('-').next().unwrap_or("");
    team_keys.iter().any(|tk| tk == prefix)
}

/// GraphQL query body for fetching a Linear issue.
fn issue_query(key: &str) -> serde_json::Value {
    serde_json::json!({
        "query": format!(
            r#"query {{
              issue(id: "{key}") {{
                identifier
                type {{ name }}
                labels {{ nodes {{ name }} }}
                cycle {{ name }}
              }}
            }}"#
        )
    })
}

/// GraphQL response envelope.
///
/// Why: we only need the nested `issue` sub-object; this thin wrapper lets
/// serde skip all other top-level keys in the response.
/// What: a minimal serde struct covering the `data.issue` path.
/// Test: covered by resolver integration tests with wiremock.
#[derive(Debug, Deserialize)]
pub struct LinearResponse {
    /// The `data` field of the GraphQL response; `None` when the API returns
    /// only `errors`.
    pub data: Option<LinearData>,
}

/// `data` field of a Linear GraphQL response.
#[derive(Debug, Deserialize)]
pub struct LinearData {
    /// The resolved issue, or `None` when the ID was not found.
    pub issue: Option<LinearIssue>,
}

/// A Linear issue as returned by the GraphQL API.
#[derive(Debug, Deserialize, Serialize)]
pub struct LinearIssue {
    /// Issue identifier, e.g. `"ENG-1234"`.
    pub identifier: String,
    /// Issue type descriptor.
    #[serde(rename = "type")]
    pub issue_type: Option<LinearIssueType>,
    /// Labels attached to this issue.
    #[serde(default)]
    pub labels: LinearLabels,
    /// Sprint / cycle the issue belongs to.
    pub cycle: Option<LinearCycle>,
}

/// Linear issue type.
#[derive(Debug, Deserialize, Serialize)]
pub struct LinearIssueType {
    /// Type name (e.g. `"Bug"`, `"Feature"`, `"Improvement"`).
    pub name: String,
}

/// Wrapper around the `labels.nodes` array in a GraphQL response.
#[derive(Debug, Deserialize, Serialize, Default)]
pub struct LinearLabels {
    /// The label objects.
    #[serde(default)]
    pub nodes: Vec<LinearLabel>,
}

/// A Linear issue label.
#[derive(Debug, Deserialize, Serialize)]
pub struct LinearLabel {
    /// Label name (e.g. `"security"`, `"bug"`).
    pub name: String,
}

/// A Linear cycle (sprint).
#[derive(Debug, Deserialize, Serialize)]
pub struct LinearCycle {
    /// Cycle name, e.g. `"Sprint 42"`.
    pub name: String,
}

/// Classify a Linear issue using the configured `field_mappings`.
///
/// Why: mappings are priority-ordered — issue_type beats labels beats cycle —
/// because issue type is the most authoritative classification signal.
/// What: walks `issue_type → labels → cycle` in that order; returns the
/// first match as an [`ExternalSignal`].
/// Test: covered by `tests::classify_issue_type_wins`,
/// `tests::classify_falls_through_to_labels`, and
/// `tests::classify_returns_none_on_no_match`.
pub fn classify_issue(issue: &LinearIssue, config: &LinearSourceConfig) -> Option<ExternalSignal> {
    let mappings = &config.field_mappings;

    // Priority 1: issue type.
    if let Some(it) = &issue.issue_type {
        if let Some(cat) = mappings.issue_type.get(&it.name) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: LINEAR_CONFIDENCE,
                source: format!("linear:issue_type:{}", it.name),
            });
        }
    }

    // Priority 2: labels (first match wins).
    for label in &issue.labels.nodes {
        if let Some(cat) = mappings.labels.get(label.name.as_str()) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: LINEAR_CONFIDENCE,
                source: format!("linear:label:{}", label.name),
            });
        }
    }

    // Priority 3: cycle name (first match wins).
    if let Some(cycle) = &issue.cycle {
        if let Some(cat) = mappings.cycle.get(cycle.name.as_str()) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: LINEAR_CONFIDENCE,
                source: format!("linear:cycle:{}", cycle.name),
            });
        }
    }

    None
}

/// Fetch a Linear issue by key via the GraphQL API.
///
/// Why: the HTTP call must be isolated here so the resolver can inject a
/// mock client via its test-seam override.
/// What: issues a `POST {base_url}/graphql` with a Personal API Key in the
/// `Authorization` header and a minimal GraphQL query for the issue fields
/// used by classification. Returns `None` on any error or when the token env
/// var is unset.
/// Test: integration-tested via the resolver with wiremock.
pub async fn fetch_issue(
    client: &reqwest::Client,
    config: &LinearSourceConfig,
    key: &str,
    base_url_override: Option<&str>,
) -> Option<LinearIssue> {
    let token = match std::env::var(&config.api_key_env) {
        Ok(t) if !t.is_empty() => t,
        _ => {
            warn!(
                api_key_env = %config.api_key_env,
                "Linear API key env var `{}` is not set — did you `export {}` before running tga?",
                config.api_key_env, config.api_key_env,
            );
            return None;
        }
    };

    let base = base_url_override.unwrap_or("https://api.linear.app");
    let url = format!("{base}/graphql");

    let body = issue_query(key);

    let resp = match client
        .post(&url)
        .header("Authorization", &token)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(key, error = %e, "Linear GraphQL request failed; skipping");
            return None;
        }
    };

    if !resp.status().is_success() {
        warn!(
            key,
            status = %resp.status(),
            "Linear GraphQL API returned non-success status; skipping"
        );
        return None;
    }

    match resp.json::<LinearResponse>().await {
        Ok(LinearResponse {
            data: Some(LinearData { issue: Some(issue) }),
        }) => Some(issue),
        Ok(_) => {
            warn!(
                key,
                "Linear GraphQL response contained no issue data; skipping"
            );
            None
        }
        Err(e) => {
            warn!(key, error = %e, "failed to parse Linear GraphQL response; skipping");
            None
        }
    }
}

/// Fetch a batch of Linear issues.
///
/// Why: same cache-before-fetch rationale as the JIRA batch helper.
/// What: deduplicates `keys`, fetches each unique key, and returns a map
/// from key to `Option<ExternalSignal>`.
/// Test: covered by resolver integration tests.
pub async fn fetch_issues_batch(
    client: &reqwest::Client,
    config: &LinearSourceConfig,
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

    /// Why: extracting a single Linear key from a typical commit message is
    /// the most common case; regressions here break all Linear-backed
    /// classification.
    /// What: asserts extraction from bare key, prefixed messages, and inline
    /// refs.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_linear_keys_single() {
        assert_eq!(extract_linear_keys("ENG-1234 fix null"), vec!["ENG-1234"]);
        assert_eq!(
            extract_linear_keys("fix: FRONTEND-99 update pipeline"),
            vec!["FRONTEND-99"]
        );
        assert_eq!(extract_linear_keys("BE-456"), vec!["BE-456"]);
    }

    /// Why: a commit may reference multiple Linear tickets; the extractor
    /// must return all of them (in order) without duplicates.
    /// What: asserts multi-key messages and deduplication.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_linear_keys_multiple_and_dedup() {
        let keys = extract_linear_keys("ENG-1 and BE-2 relate to FE-3 and ENG-1 again");
        assert_eq!(keys, vec!["ENG-1", "BE-2", "FE-3"]);
    }

    /// Why: the extractor must not match lowercase identifiers (false
    /// positives on branch names like `fix-123`).
    /// What: asserts lowercase keys are not extracted.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_linear_keys_ignores_lowercase() {
        assert!(extract_linear_keys("eng-123 lowercase").is_empty());
        assert!(extract_linear_keys("no ticket here").is_empty());
    }

    /// Why: `matches_team_key` is the primary guard against confusing Linear
    /// keys with JIRA keys that share the same shape.
    /// What: asserts filtering with and without a configured team list.
    /// Test: pure function, no HTTP.
    #[test]
    fn team_key_filter_empty_allows_all() {
        assert!(matches_team_key("ENG-1", &[]));
        assert!(matches_team_key("ANY-999", &[]));
    }

    /// Why: when team_keys is non-empty only configured prefixes should pass.
    /// What: asserts configured prefix passes and unknown prefix fails.
    /// Test: pure function, no HTTP.
    #[test]
    fn team_key_filter_restricts_to_configured_prefixes() {
        let teams = vec!["ENG".to_string(), "BE".to_string()];
        assert!(matches_team_key("ENG-1", &teams));
        assert!(matches_team_key("BE-42", &teams));
        assert!(!matches_team_key("JIRA-1234", &teams));
        assert!(!matches_team_key("FE-10", &teams));
    }

    /// Why: `classify_issue` must prefer issue-type over labels over cycle,
    /// matching the documented priority order.
    /// What: build a `LinearIssue` with all three fields populated; assert
    /// issue_type wins.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_issue_type_wins_over_labels() {
        use std::collections::HashMap;
        let issue = LinearIssue {
            identifier: "ENG-1".to_string(),
            issue_type: Some(LinearIssueType {
                name: "Bug".to_string(),
            }),
            labels: LinearLabels {
                nodes: vec![LinearLabel {
                    name: "enhancement".to_string(),
                }],
            },
            cycle: Some(LinearCycle {
                name: "Sprint 42".to_string(),
            }),
        };
        let config = LinearSourceConfig {
            api_key_env: "LINEAR_API_TOKEN".to_string(), // pragma: allowlist secret
            team_keys: vec![],
            field_mappings: crate::classify::sources::LinearFieldMappings {
                issue_type: {
                    let mut m = HashMap::new();
                    m.insert("Bug".to_string(), "bug_fix".to_string());
                    m
                },
                labels: {
                    let mut m = HashMap::new();
                    m.insert("enhancement".to_string(), "new_feature".to_string());
                    m
                },
                cycle: {
                    let mut m = HashMap::new();
                    m.insert("Sprint 42".to_string(), "sprint_delivery".to_string());
                    m
                },
            },
        };
        let signal = classify_issue(&issue, &config).expect("should match");
        assert_eq!(signal.category, "bug_fix");
        assert!(signal.source.contains("issue_type"));
    }

    /// Why: when issue type is absent/unmapped, labels should be the next
    /// fallback.
    /// What: build an issue with no issue-type mapping but a matching label.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_falls_through_to_labels() {
        use std::collections::HashMap;
        let issue = LinearIssue {
            identifier: "ENG-2".to_string(),
            issue_type: Some(LinearIssueType {
                name: "Epic".to_string(), // not in mappings
            }),
            labels: LinearLabels {
                nodes: vec![LinearLabel {
                    name: "security".to_string(),
                }],
            },
            cycle: None,
        };
        let config = LinearSourceConfig {
            api_key_env: "LINEAR_API_TOKEN".to_string(), // pragma: allowlist secret
            team_keys: vec![],
            field_mappings: crate::classify::sources::LinearFieldMappings {
                issue_type: HashMap::new(),
                labels: {
                    let mut m = HashMap::new();
                    m.insert("security".to_string(), "security".to_string());
                    m
                },
                cycle: HashMap::new(),
            },
        };
        let signal = classify_issue(&issue, &config).expect("should match via label");
        assert_eq!(signal.category, "security");
        assert!(signal.source.contains("label"));
    }

    /// Why: when no field matches, `classify_issue` must return `None` so
    /// the pipeline falls through to commit-message rules.
    /// What: build an issue with no mapped fields.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_returns_none_on_no_match() {
        use std::collections::HashMap;
        let issue = LinearIssue {
            identifier: "ENG-3".to_string(),
            issue_type: Some(LinearIssueType {
                name: "Unknown".to_string(),
            }),
            labels: LinearLabels { nodes: vec![] },
            cycle: None,
        };
        let config = LinearSourceConfig {
            api_key_env: "LINEAR_API_TOKEN".to_string(), // pragma: allowlist secret
            team_keys: vec![],
            field_mappings: crate::classify::sources::LinearFieldMappings {
                issue_type: HashMap::new(),
                labels: HashMap::new(),
                cycle: HashMap::new(),
            },
        };
        assert!(classify_issue(&issue, &config).is_none());
    }

    /// Why: `LinearSourceConfig` must round-trip through YAML deserialization
    /// with `deny_unknown_fields` so typos in the config are caught early.
    /// What: deserialize a full `type: linear` source config and assert fields.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn linear_source_config_deserializes() {
        use crate::classify::sources::SourceConfig;
        let yaml = r#"
type: linear
api_key_env: LINEAR_API_TOKEN
team_keys: ["ENG", "BE"]
field_mappings:
  issue_type:
    Bug: bug_fix
    Feature: new_feature
  labels:
    security: security
  cycle: {}
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Linear(l) => {
                assert_eq!(l.api_key_env, "LINEAR_API_TOKEN"); // pragma: allowlist secret
                assert_eq!(l.team_keys, vec!["ENG", "BE"]);
                assert_eq!(
                    l.field_mappings.issue_type.get("Bug"),
                    Some(&"bug_fix".to_string())
                );
                assert_eq!(
                    l.field_mappings.labels.get("security"),
                    Some(&"security".to_string())
                );
            }
            other => panic!("expected Linear variant, got {other:?}"),
        }
    }

    /// Why: `deny_unknown_fields` on `LinearSourceConfig` must reject a YAML
    /// typo (e.g. `api_key:` instead of `api_key_env:`) with a parse error.
    /// What: attempt to deserialize with an unknown field and assert `Err`.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn linear_source_config_unknown_field_is_rejected() {
        let yaml = r#"
type: linear
api_key: MY_KEY
team_keys: []
field_mappings:
  issue_type: {}
  labels: {}
  cycle: {}
"#;
        let result: Result<crate::classify::sources::SourceConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "unknown field must be rejected");
    }

    /// Why: wiremock integration test — verifies the full HTTP path including
    /// GraphQL body construction and response parsing.
    /// What: mock the Linear GraphQL endpoint returning a Bug issue; assert
    /// the fetch returns the correct issue and classification fires correctly.
    /// Test: wiremock mock of Linear GraphQL API; requires `tokio`.
    #[tokio::test]
    async fn fetch_and_classify_via_wiremock() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let body = serde_json::json!({
            "data": {
                "issue": {
                    "identifier": "ENG-99",
                    "type": {"name": "Bug"},
                    "labels": {"nodes": [{"name": "ktlo"}]},
                    "cycle": null
                }
            }
        });

        // Accept any POST to /graphql (the query body varies by key).
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        unsafe { std::env::set_var("LINEAR_API_TOKEN_WT", "test-token") }; // pragma: allowlist secret

        use std::collections::HashMap;
        let config = LinearSourceConfig {
            api_key_env: "LINEAR_API_TOKEN_WT".to_string(), // pragma: allowlist secret
            team_keys: vec![],
            field_mappings: crate::classify::sources::LinearFieldMappings {
                issue_type: {
                    let mut m = HashMap::new();
                    m.insert("Bug".to_string(), "bug_fix".to_string());
                    m
                },
                labels: HashMap::new(),
                cycle: HashMap::new(),
            },
        };

        let client = reqwest::Client::new();
        let issue = fetch_issue(&client, &config, "ENG-99", Some(&server.uri()))
            .await
            .expect("fetch should succeed");

        let signal = classify_issue(&issue, &config).expect("should classify");
        assert_eq!(signal.category, "bug_fix");
        assert!(signal.source.contains("issue_type"));

        unsafe { std::env::remove_var("LINEAR_API_TOKEN_WT") };
    }
}
