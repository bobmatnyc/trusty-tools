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
/// then 1–7 digits (bounding the digit run prevents over-matching trailing
/// digit runs — issue #285).  The leading `\b` anchor prevents partial
/// matches inside longer identifiers.  A post-filter in `extract_jira_keys`
/// rejects any match whose next character in the source string is a digit,
/// covering the case where the greedy digit bound is still reached before a
/// longer run ends (e.g. `UIARCH-32885` must not yield `UIARCH-3288` when
/// the regex crate's lack of lookahead would otherwise accept it).
/// Test: covered by `tests::extract_jira_keys_*`, including
/// `tests::extract_jira_keys_no_trailing_digit_overreach`.
fn jira_key_regex() -> Regex {
    // `\d{1,7}` caps the digit run at 7 digits (the highest realistic JIRA
    // ticket number at scale is ~9 999 999).  The Rust `regex` crate does not
    // support lookahead, so the trailing non-digit guard is enforced by the
    // post-filter in `extract_jira_keys` rather than inline in the pattern.
    Regex::new(r"\b([A-Z][A-Z0-9]{0,9}-\d{1,7})").expect("static regex is valid")
}

/// Extract all JIRA ticket keys from a commit message.
///
/// Why: a single commit can reference multiple JIRA tickets; collecting all
/// of them maximises the chance of finding one that maps to a configured
/// project key.
/// What: returns a `Vec<String>` of unique ticket keys found in `message`,
/// in left-to-right order of first appearance.  Matches followed immediately
/// by another digit are dropped (post-filter for issue #285: the bounded
/// `\d{1,7}` regex alone cannot prevent `UIARCH-3288` from being returned
/// from `UIARCH-32885` because the regex would still accept the shorter run;
/// checking `message.as_bytes()[end]` is a digit is the authoritative guard).
/// Test: covered by `tests::extract_jira_keys_single`,
/// `tests::extract_jira_keys_multiple_and_dedup`, and
/// `tests::extract_jira_keys_no_trailing_digit_overreach`.
pub fn extract_jira_keys(message: &str) -> Vec<String> {
    let re = jira_key_regex();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(message) {
        if let Some(key) = cap.get(1) {
            // Post-filter: reject a match whose immediately-following byte is
            // an ASCII digit.  This is the authoritative guard against
            // trailing-digit over-match (issue #285).  `key.end()` is a byte
            // offset into `message`; indexing into `message.as_bytes()` is
            // safe because the regex only matches ASCII characters.
            let end = key.end();
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
///
/// What: issues `GET {base_url}/rest/api/3/issue/{key}?fields=issuetype,labels,components`
/// with the auth mode determined by the config:
///
/// 1. `username` set (literal) → Basic auth with that literal email + token.
/// 2. `email_env` set → Basic auth with `std::env::var(email_env)` + token
///    (resolved at request time, not config-load time).
/// 3. Neither set → Bearer auth (token-only; rare for Atlassian Cloud which
///    requires Basic auth — a `warn!` is emitted).
///
/// Returns `None` on any HTTP error or when the token env var is unset.
///
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
                "JIRA token env var `{}` is not set in the tga process environment — \
                 did you `export {}` before running tga?",
                config.token_env, config.token_env,
            );
            return None;
        }
    };

    let base = base_url_override.unwrap_or(&config.base_url);
    let url = format!("{base}/rest/api/3/issue/{key}?fields=issuetype,labels,components");

    let mut req = client.get(&url);

    // Auth priority:
    //   1. username (literal email) — backward compat.
    //   2. email_env (env-var indirection) — recommended for Atlassian Cloud.
    //   3. Bearer token-only — unusual for Cloud; emit a warning.
    if let Some(username) = &config.username {
        req = req.basic_auth(username, Some(&token));
    } else if let Some(env_name) = &config.email_env {
        match std::env::var(env_name) {
            Ok(email) if !email.is_empty() => {
                req = req.basic_auth(email, Some(&token));
            }
            _ => {
                warn!(
                    email_env = %env_name,
                    "JIRA email env var `{env_name}` is not set in the tga process \
                     environment — did you `export {env_name}` before running tga? \
                     Falling back to Bearer auth (may return 403 on Atlassian Cloud).",
                );
                req = req.bearer_auth(&token);
            }
        }
    } else {
        warn!(
            "No JIRA email configured (neither `username` nor `email_env` is set). \
             Using Bearer auth — this typically returns HTTP 403 on Atlassian Cloud. \
             Add `email_env: JIRA_EMAIL` to your source config.",
        );
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

    /// Why: the old unbounded `\d+` regex over-matched trailing digit runs —
    /// `UIARCH-32885` was extracted from "per UIARCH-3288 followed by 5"
    /// instead of the correct `UIARCH-3288` (issue #285, live repro against
    /// duettoresearch.atlassian.net).
    /// What: asserts the correct key is extracted when the JIRA key is
    /// immediately followed by whitespace and then more digits.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_jira_keys_no_trailing_digit_overreach() {
        // The bug: "followed by 5" contains a lone digit; without the
        // post-filter, the regex matched "UIARCH-32885" (consuming the 5).
        let keys = extract_jira_keys("per UIARCH-3288 followed by 5");
        assert_eq!(
            keys,
            vec!["UIARCH-3288"],
            "must not over-consume the trailing ' 5'"
        );

        // Adjacent digit run separated by a hyphen is fine (two distinct keys).
        let keys2 = extract_jira_keys("PROJ-123 and PROJ-456");
        assert_eq!(keys2, vec!["PROJ-123", "PROJ-456"]);

        // A key directly adjacent to more digits (no space) must not be extracted.
        let keys3 = extract_jira_keys("PROJ-12345678");
        // 8 digits — exceeds the 7-digit cap, so nothing should match.
        assert!(keys3.is_empty(), "8-digit run should not match: {keys3:?}");

        // Normal 7-digit key at the cap must still match when followed by space.
        let keys4 = extract_jira_keys("PROJ-9999999 is the limit");
        assert_eq!(keys4, vec!["PROJ-9999999"]);
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
            email_env: None,
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
            email_env: None,
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
            email_env: None,
            project_keys: vec![],
            field_mappings: Default::default(),
        };
        assert!(classify_issue(&issue, &config).is_none());
    }

    /// Why: `email_env` round-trips through YAML deserialization so users who
    /// write `email_env: JIRA_EMAIL` in their config get the field populated
    /// rather than a silent unknown-field error.
    /// What: deserialize a JIRA source config with `email_env:` and assert the
    /// field is `Some("JIRA_EMAIL")`.
    /// Test: pure deserialization; no HTTP.
    #[test]
    fn jira_config_email_env_deserializes() {
        use super::super::SourceConfig;
        let yaml = r#"
type: jira
base_url: "https://acme.atlassian.net"
token_env: JIRA_API_TOKEN
email_env: JIRA_EMAIL
project_keys: ["PROJ"]
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Jira(j) => {
                assert_eq!(j.email_env.as_deref(), Some("JIRA_EMAIL"));
                assert_eq!(j.username, None);
            }
            other => panic!("expected Jira variant, got {other:?}"),
        }
    }

    /// Why: `deny_unknown_fields` on `JiraSourceConfig` must turn a YAML typo
    /// (e.g. `emial_env:`) into a loud parse error instead of silently
    /// dropping the field.
    /// What: attempt to deserialize a config with an unknown field and assert
    /// the result is `Err`.
    /// Test: pure deserialization; no HTTP.
    #[test]
    fn jira_config_unknown_field_is_rejected() {
        let yaml = r#"
type: jira
base_url: "https://acme.atlassian.net"
token_env: JIRA_API_TOKEN
emial_env: JIRA_EMAIL
"#;
        // The SourceConfig tagged enum peels off `type:` before forwarding to
        // JiraSourceConfig, so this error surfaces via the inner struct.
        let result: Result<super::super::SourceConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "unknown field must be rejected");
    }
}
