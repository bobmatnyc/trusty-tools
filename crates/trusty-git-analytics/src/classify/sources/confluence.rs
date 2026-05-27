//! Confluence REST API client for commit classification signals.
//!
//! Why: Confluence page labels can carry useful organisational signal —
//! a commit that references a runbook page is likely devops-related, and
//! one referencing an RFC is likely refactoring. This is a weaker signal
//! than JIRA issue types or Linear labels because Confluence labels are
//! typically organisational rather than work-type indicators, so the
//! default confidence is lower (0.80 vs the standard 0.92).
//!
//! Note: this source is intentionally documented as "informational signal
//! only" — it is recommended for use alongside a higher-confidence source
//! (e.g. JIRA) rather than as a standalone classifier.
//!
//! What: extracts Confluence page references from commit messages via two
//! patterns: URL fragments (`/wiki/spaces/...`) and Smart Commit syntax
//! (`[CONF-NNN]`). Fetches page labels via the Confluence REST API v1
//! (`GET /wiki/rest/api/content/{id}?expand=metadata.labels`). Auth is
//! Basic (email + API token, same scheme as JIRA Cloud).
//!
//! Test: see `tests::extract_confluence_refs_*` for extractor coverage
//! and `tests::fetch_and_classify_via_wiremock` for the HTTP path.

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{ConfluenceSourceConfig, ExternalSignal};

/// Default confidence for Confluence label signals.
///
/// Why: Confluence labels are organisational tags, not explicit work-type
/// classifications. Setting this below the standard 0.92 (used by JIRA and
/// GitHub Issues) allows stronger-signal sources to override Confluence
/// verdicts when combined in a multi-source config.
/// What: 0.80 — well above the tier-3 fuzzy floor (0.40/0.60) but below
/// the JIRA/Linear/GitHub standard.
/// Test: verified in `tests::classify_issue_uses_confluence_confidence`.
pub const CONFLUENCE_CONFIDENCE: f64 = 0.80;

/// Regex matching an inline Confluence page URL fragment.
///
/// Why: developers often paste Confluence page URLs into commit messages
/// (e.g. `https://yourco.atlassian.net/wiki/spaces/ENG/pages/123456`).
/// What: captures the page numeric ID from URL path `…/pages/<id>…`.
/// Test: covered by `tests::extract_confluence_refs_url`.
fn url_regex() -> Regex {
    Regex::new(r"/wiki/spaces/[^/]+/pages/(\d+)").expect("static regex is valid")
}

/// Regex matching Smart Commit `[CONF-NNN]` references.
///
/// Why: teams using Confluence Smart Commits syntax embed page references
/// as `[CONF-<N>]` in commit messages (similar to JIRA Smart Commits).
/// What: captures the numeric ID from `[CONF-<N>]`.
/// Test: covered by `tests::extract_confluence_refs_smart_commit`.
fn smart_commit_regex() -> Regex {
    Regex::new(r"\[CONF-(\d+)\]").expect("static regex is valid")
}

/// Extract all Confluence page IDs from a commit message.
///
/// Why: both URL and Smart Commit reference forms must be supported to
/// cover teams using either convention.
/// What: runs both regexes, deduplicates by numeric page ID, and returns
/// a `Vec<u64>` in left-to-right order of first appearance.
/// Test: covered by `tests::extract_confluence_refs_*`.
pub fn extract_confluence_ids(message: &str) -> Vec<u64> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    // URL form: /wiki/spaces/ENG/pages/123456
    for cap in url_regex().captures_iter(message) {
        if let Some(num_m) = cap.get(1) {
            let id: u64 = num_m.as_str().parse().unwrap_or(0);
            if id > 0 && seen.insert(id) {
                out.push(id);
            }
        }
    }

    // Smart Commit form: [CONF-123]
    for cap in smart_commit_regex().captures_iter(message) {
        if let Some(num_m) = cap.get(1) {
            let id: u64 = num_m.as_str().parse().unwrap_or(0);
            if id > 0 && seen.insert(id) {
                out.push(id);
            }
        }
    }

    out
}

/// Partial deserialization target for the Confluence content API.
///
/// Why: we only need the `metadata.labels.results[].name` array to produce
/// classification signals.
/// What: a minimal serde struct over the Confluence REST v1 response.
/// Test: covered by resolver integration tests with wiremock.
#[derive(Debug, Deserialize, Serialize)]
pub struct ConfluencePage {
    /// Numeric page ID as a string (Confluence returns it as a string).
    pub id: String,
    /// Page title (logged for diagnostics only).
    #[serde(default)]
    pub title: String,
    /// Metadata container, includes label list.
    pub metadata: Option<ConfluenceMetadata>,
}

/// Metadata block in a Confluence page response.
#[derive(Debug, Deserialize, Serialize)]
pub struct ConfluenceMetadata {
    /// Label collection.
    pub labels: Option<ConfluenceLabelList>,
}

/// A list of Confluence page labels.
#[derive(Debug, Deserialize, Serialize)]
pub struct ConfluenceLabelList {
    /// The label objects.
    #[serde(default)]
    pub results: Vec<ConfluenceLabel>,
}

/// A single Confluence page label.
#[derive(Debug, Deserialize, Serialize)]
pub struct ConfluenceLabel {
    /// Label prefix (e.g. `"global"`, `"team"`).
    #[serde(default)]
    pub prefix: String,
    /// Label name (e.g. `"runbook"`, `"rfc"`, `"incident"`).
    pub name: String,
}

/// Classify a Confluence page using the configured `label_mappings`.
///
/// Why: the label mapping converts Confluence's organisational labels to
/// TGA category strings. First matching label wins (users order their
/// highest-priority labels first in the YAML map).
/// What: iterates page labels; returns an [`ExternalSignal`] at
/// [`CONFLUENCE_CONFIDENCE`] for the first label that maps to a category,
/// or `None` if no label matches.
/// Test: covered by `tests::classify_page_matches_label` and
/// `tests::classify_page_returns_none_on_no_match`.
pub fn classify_page(
    page: &ConfluencePage,
    config: &ConfluenceSourceConfig,
) -> Option<ExternalSignal> {
    let labels = page
        .metadata
        .as_ref()
        .and_then(|m| m.labels.as_ref())
        .map(|ll| ll.results.as_slice())
        .unwrap_or(&[]);

    for label in labels {
        if let Some(cat) = config.label_mappings.get(label.name.as_str()) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: CONFLUENCE_CONFIDENCE,
                source: format!("confluence:label:{}", label.name),
            });
        }
    }

    None
}

/// Fetch a Confluence page by its numeric ID.
///
/// Why: the HTTP call must be isolated here so the resolver can inject a
/// mock client via its test-seam override.
/// What: issues
/// `GET {base_url}/wiki/rest/api/content/{id}?expand=metadata.labels`
/// with Basic auth (email + token). Returns `None` on any error or when
/// credentials are unavailable.
/// Test: integration-tested via the resolver with wiremock.
pub async fn fetch_page(
    client: &reqwest::Client,
    config: &ConfluenceSourceConfig,
    id: u64,
    base_url_override: Option<&str>,
) -> Option<ConfluencePage> {
    let token = match std::env::var(&config.token_env) {
        Ok(t) if !t.is_empty() => t,
        _ => {
            warn!(
                token_env = %config.token_env,
                "Confluence token env var `{}` is not set — skipping Confluence lookups",
                config.token_env,
            );
            return None;
        }
    };

    let email = match std::env::var(&config.email_env) {
        Ok(e) if !e.is_empty() => e,
        _ => {
            warn!(
                email_env = %config.email_env,
                "Confluence email env var `{}` is not set — skipping Confluence lookups",
                config.email_env,
            );
            return None;
        }
    };

    let base = base_url_override.unwrap_or(&config.base_url);
    let url = format!("{base}/wiki/rest/api/content/{id}?expand=metadata.labels");

    let resp = match client
        .get(&url)
        .basic_auth(&email, Some(&token))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(id, error = %e, "Confluence API request failed; skipping");
            return None;
        }
    };

    if !resp.status().is_success() {
        warn!(
            id,
            status = %resp.status(),
            "Confluence API returned non-success status; skipping"
        );
        return None;
    }

    match resp.json::<ConfluencePage>().await {
        Ok(page) => Some(page),
        Err(e) => {
            warn!(id, error = %e, "failed to parse Confluence page response; skipping");
            None
        }
    }
}

/// Fetch a batch of Confluence pages.
///
/// Why: same cache-before-fetch rationale as the JIRA/Linear batch helpers.
/// What: deduplicates `ids`, fetches each unique page, and returns a map
/// from page ID string to `Option<ExternalSignal>`.
/// Test: covered by resolver integration tests.
pub async fn fetch_pages_batch(
    client: &reqwest::Client,
    config: &ConfluenceSourceConfig,
    ids: &[u64],
    base_url_override: Option<&str>,
) -> HashMap<String, Option<ExternalSignal>> {
    let mut out = HashMap::new();
    for &id in ids {
        let key = id.to_string();
        if out.contains_key(&key) {
            continue;
        }
        let page = fetch_page(client, config, id, base_url_override).await;
        let signal = page.and_then(|p| classify_page(&p, config));
        out.insert(key, signal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: URL-based Confluence page references are common when developers
    /// paste page links directly into commit messages.
    /// What: assert extraction from a full Confluence page URL.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_confluence_refs_url() {
        let ids = extract_confluence_ids(
            "see https://myco.atlassian.net/wiki/spaces/ENG/pages/123456789 for context",
        );
        assert_eq!(ids, vec![123456789u64]);
    }

    /// Why: Smart Commit syntax (`[CONF-NNN]`) is used by teams that integrate
    /// Confluence notifications with their commit workflows.
    /// What: assert extraction from a `[CONF-<N>]` reference.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_confluence_refs_smart_commit() {
        let ids = extract_confluence_ids("deploy: [CONF-4567] runbook followed");
        assert_eq!(ids, vec![4567u64]);
    }

    /// Why: both forms may appear in the same commit; deduplication must work.
    /// What: assert multi-ref extraction and deduplication. Note: the URL regex
    /// runs before the Smart Commit regex, so URL IDs appear first.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_confluence_refs_both_forms_and_dedup() {
        // URL form is processed first → 200 appears before 100 in results.
        let ids = extract_confluence_ids(
            "[CONF-100] and /wiki/spaces/ENG/pages/200 and [CONF-100] again",
        );
        // URL (200) is extracted first, Smart Commit (100) second.
        assert_eq!(ids, vec![200u64, 100u64]);
        // Reverse order: Smart Commit before URL → Smart Commit ID first in
        // text, but URL regex still runs first in the extractor, so URL wins.
        let ids2 = extract_confluence_ids("/wiki/spaces/ENG/pages/500 and [CONF-300]");
        assert_eq!(ids2, vec![500u64, 300u64]);
        // Deduplication: same ID from both forms yields one entry.
        let ids3 = extract_confluence_ids("/wiki/spaces/ENG/pages/777 and [CONF-777]");
        assert_eq!(ids3, vec![777u64]);
    }

    /// Why: messages without Confluence references must yield empty vec.
    /// What: assert empty result on plain commit messages.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_confluence_refs_no_match() {
        assert!(extract_confluence_ids("feat: add login flow").is_empty());
        assert!(extract_confluence_ids("fix: PROJ-123 jira style").is_empty());
    }

    /// Why: `classify_page` must return a signal at `CONFLUENCE_CONFIDENCE`
    /// for the first matching label and ignore subsequent ones.
    /// What: build a page with multiple labels; assert first match wins and
    /// confidence equals `CONFLUENCE_CONFIDENCE`.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_page_matches_label() {
        use std::collections::HashMap;
        let page = ConfluencePage {
            id: "123".to_string(),
            title: "Deployment Runbook".to_string(),
            metadata: Some(ConfluenceMetadata {
                labels: Some(ConfluenceLabelList {
                    results: vec![
                        ConfluenceLabel {
                            prefix: "global".to_string(),
                            name: "runbook".to_string(),
                        },
                        ConfluenceLabel {
                            prefix: "global".to_string(),
                            name: "rfc".to_string(),
                        },
                    ],
                }),
            }),
        };
        let config = ConfluenceSourceConfig {
            base_url: "https://myco.atlassian.net".to_string(),
            token_env: "CONF_TOKEN".to_string(), // pragma: allowlist secret
            email_env: "CONF_EMAIL".to_string(),
            label_mappings: {
                let mut m = HashMap::new();
                m.insert("runbook".to_string(), "devops".to_string());
                m.insert("rfc".to_string(), "tech_debt_refactoring".to_string());
                m
            },
        };
        let signal = classify_page(&page, &config).expect("should match");
        assert_eq!(signal.category, "devops");
        assert!(
            (signal.confidence - CONFLUENCE_CONFIDENCE).abs() < f64::EPSILON,
            "confidence should be CONFLUENCE_CONFIDENCE ({CONFLUENCE_CONFIDENCE})"
        );
        assert!(signal.source.contains("runbook"));
    }

    /// Why: `classify_page` must return `None` when no label matches so the
    /// pipeline falls through to commit-message rules.
    /// What: build a page with no mapped labels.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_page_returns_none_on_no_match() {
        use std::collections::HashMap;
        let page = ConfluencePage {
            id: "456".to_string(),
            title: "Untitled".to_string(),
            metadata: Some(ConfluenceMetadata {
                labels: Some(ConfluenceLabelList {
                    results: vec![ConfluenceLabel {
                        prefix: "global".to_string(),
                        name: "unlabeled".to_string(),
                    }],
                }),
            }),
        };
        let config = ConfluenceSourceConfig {
            base_url: "https://myco.atlassian.net".to_string(),
            token_env: "CONF_TOKEN".to_string(), // pragma: allowlist secret
            email_env: "CONF_EMAIL".to_string(),
            label_mappings: HashMap::new(),
        };
        assert!(classify_page(&page, &config).is_none());
    }

    /// Why: pages with no labels / metadata should not panic and must return
    /// `None` gracefully.
    /// What: build a page with `metadata: None`.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_page_with_no_metadata_returns_none() {
        use std::collections::HashMap;
        let page = ConfluencePage {
            id: "789".to_string(),
            title: "Empty".to_string(),
            metadata: None,
        };
        let config = ConfluenceSourceConfig {
            base_url: "https://myco.atlassian.net".to_string(),
            token_env: "CONF_TOKEN".to_string(), // pragma: allowlist secret
            email_env: "CONF_EMAIL".to_string(),
            label_mappings: {
                let mut m = HashMap::new();
                m.insert("runbook".to_string(), "devops".to_string());
                m
            },
        };
        assert!(classify_page(&page, &config).is_none());
    }

    /// Why: `ConfluenceSourceConfig` must round-trip through YAML with
    /// `deny_unknown_fields` so config typos are caught at load time.
    /// What: deserialize a full `type: confluence` source and assert fields.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn confluence_source_config_deserializes() {
        use crate::classify::sources::SourceConfig;
        let yaml = r#"
type: confluence
base_url: "https://myco.atlassian.net/wiki"
token_env: CONFLUENCE_API_TOKEN
email_env: CONFLUENCE_EMAIL
label_mappings:
  runbook: devops
  rfc: tech_debt_refactoring
  incident: bug_fix
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Confluence(c) => {
                assert_eq!(c.base_url, "https://myco.atlassian.net/wiki");
                assert_eq!(c.token_env, "CONFLUENCE_API_TOKEN"); // pragma: allowlist secret
                assert_eq!(c.email_env, "CONFLUENCE_EMAIL");
                assert_eq!(c.label_mappings.get("runbook"), Some(&"devops".to_string()));
                assert_eq!(
                    c.label_mappings.get("rfc"),
                    Some(&"tech_debt_refactoring".to_string())
                );
            }
            other => panic!("expected Confluence variant, got {other:?}"),
        }
    }

    /// Why: `deny_unknown_fields` must reject YAML typos loudly.
    /// What: attempt to deserialize with an unknown field and assert `Err`.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn confluence_source_config_unknown_field_is_rejected() {
        let yaml = r#"
type: confluence
base_url: "https://myco.atlassian.net/wiki"
token_env: CONFLUENCE_API_TOKEN
email_env: CONFLUENCE_EMAIL
api_token_env: CONFLUENCE_API_TOKEN
label_mappings: {}
"#;
        let result: Result<crate::classify::sources::SourceConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "unknown field must be rejected");
    }

    /// Why: wiremock integration test — verifies the full HTTP path.
    /// What: mock the Confluence REST endpoint; assert fetch and classify.
    /// Test: wiremock mock of Confluence content API.
    #[tokio::test]
    async fn fetch_and_classify_via_wiremock() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": "99",
            "title": "Deployment Runbook",
            "metadata": {
                "labels": {
                    "results": [
                        {"prefix": "global", "name": "runbook"},
                        {"prefix": "team", "name": "platform"}
                    ]
                }
            }
        });

        Mock::given(method("GET"))
            .and(path("/wiki/rest/api/content/99"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        unsafe { std::env::set_var("CONF_TOKEN_WT", "test-token") }; // pragma: allowlist secret
        unsafe { std::env::set_var("CONF_EMAIL_WT", "test@example.com") };

        use std::collections::HashMap;
        let config = ConfluenceSourceConfig {
            base_url: server.uri(),
            token_env: "CONF_TOKEN_WT".to_string(), // pragma: allowlist secret
            email_env: "CONF_EMAIL_WT".to_string(),
            label_mappings: {
                let mut m = HashMap::new();
                m.insert("runbook".to_string(), "devops".to_string());
                m
            },
        };

        let client = reqwest::Client::new();
        let page = fetch_page(&client, &config, 99, Some(&server.uri()))
            .await
            .expect("fetch should succeed");

        let signal = classify_page(&page, &config).expect("should classify");
        assert_eq!(signal.category, "devops");
        assert!((signal.confidence - CONFLUENCE_CONFIDENCE).abs() < f64::EPSILON);

        unsafe { std::env::remove_var("CONF_TOKEN_WT") };
        unsafe { std::env::remove_var("CONF_EMAIL_WT") };
    }
}
