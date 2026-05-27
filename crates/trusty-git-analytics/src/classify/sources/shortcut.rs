//! Shortcut (formerly Clubhouse) REST API client for commit classification.
//!
//! Why: Shortcut story types are explicit classification signals — a commit
//! referencing a `bug` story is a bug fix even when the message is vague. This
//! module extracts Shortcut story IDs from commit messages and fetches their
//! story type / labels / workflow state to produce a [`super::ExternalSignal`].
//!
//! What: a regex-based ID extractor (handles both `[ch1234]` and `sc-1234`
//! references) plus a minimal reqwest-based client that calls
//! `GET /api/v3/stories/{id}`. Credentials are read from the environment
//! variable named in [`super::ShortcutSourceConfig::api_token_env`].
//!
//! Test: see `tests::extract_shortcut_ids_*` for extractor coverage and
//! the resolver integration tests for the full pipeline.

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::{ExternalSignal, ShortcutSourceConfig, EXTERNAL_SOURCE_CONFIDENCE};

/// Regex matching the `[ch1234]` bracket reference form.
///
/// Why: the `[ch<N>]` form is the legacy Clubhouse branch-name convention
/// and is widely used in existing commit histories.
/// What: captures the numeric story ID from `[ch1234]` references.
/// Test: covered by `tests::extract_shortcut_ids_bracket_form`.
fn bracket_ref_regex() -> Regex {
    Regex::new(r"\[ch(\d+)\]").expect("static regex is valid")
}

/// Regex matching the `sc-1234` short-code reference form.
///
/// Why: Shortcut's Git helper and branch-name conventions use `sc-<N>`.
/// What: captures the numeric story ID from `sc-1234` references.
/// Test: covered by `tests::extract_shortcut_ids_sc_form`.
fn sc_ref_regex() -> Regex {
    Regex::new(r"\bsc-(\d+)\b").expect("static regex is valid")
}

/// Extract all Shortcut story IDs from a commit message.
///
/// Why: Shortcut supports two reference formats (`[chNNN]` and `sc-NNN`);
/// both must be extracted so teams using either convention are covered.
/// What: runs both regexes, deduplicates by numeric ID, and returns a
/// `Vec<u64>` in left-to-right order of first appearance.
/// Test: covered by `tests::extract_shortcut_ids_*`.
pub fn extract_shortcut_ids(message: &str) -> Vec<u64> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    // `[ch1234]` form.
    for cap in bracket_ref_regex().captures_iter(message) {
        if let Some(num_m) = cap.get(1) {
            let id: u64 = num_m.as_str().parse().unwrap_or(0);
            if id > 0 && seen.insert(id) {
                out.push(id);
            }
        }
    }

    // `sc-1234` form.
    for cap in sc_ref_regex().captures_iter(message) {
        if let Some(num_m) = cap.get(1) {
            let id: u64 = num_m.as_str().parse().unwrap_or(0);
            if id > 0 && seen.insert(id) {
                out.push(id);
            }
        }
    }

    out
}

/// Partial deserialization target for `GET /api/v3/stories/{id}`.
///
/// Why: we only need `story_type`, `labels[].name`, and
/// `workflow_state.name` to produce classification signals.
/// What: a minimal serde struct over the Shortcut Stories REST response.
/// Test: covered by resolver integration tests with wiremock.
#[derive(Debug, Deserialize, Serialize)]
pub struct ShortcutStory {
    /// Numeric story ID.
    pub id: u64,
    /// Story type string: `"bug"`, `"feature"`, or `"chore"`.
    pub story_type: String,
    /// Labels attached to this story.
    #[serde(default)]
    pub labels: Vec<ShortcutLabel>,
    /// Workflow state the story currently occupies.
    pub workflow_state: Option<ShortcutWorkflowState>,
}

/// A Shortcut story label.
#[derive(Debug, Deserialize, Serialize)]
pub struct ShortcutLabel {
    /// Label name (e.g. `"security"`, `"ktlo"`).
    pub name: String,
}

/// A Shortcut workflow state.
#[derive(Debug, Deserialize, Serialize)]
pub struct ShortcutWorkflowState {
    /// Workflow state name (e.g. `"Done"`, `"In Progress"`).
    pub name: String,
}

/// Classify a Shortcut story using the configured `field_mappings`.
///
/// Why: mappings are priority-ordered — story_type beats labels beats
/// workflow_state — because story type is the most authoritative signal.
/// What: walks `story_type → labels → workflow_state` in that order;
/// returns the first match as an [`ExternalSignal`].
/// Test: covered by `tests::classify_story_type_wins`,
/// `tests::classify_falls_through_to_labels`, and
/// `tests::classify_returns_none_on_no_match`.
pub fn classify_story(
    story: &ShortcutStory,
    config: &ShortcutSourceConfig,
) -> Option<ExternalSignal> {
    let mappings = &config.field_mappings;

    // Priority 1: story_type.
    if let Some(cat) = mappings.story_type.get(&story.story_type) {
        return Some(ExternalSignal {
            category: cat.clone(),
            confidence: EXTERNAL_SOURCE_CONFIDENCE,
            source: format!("shortcut:story_type:{}", story.story_type),
        });
    }

    // Priority 2: labels (first match wins).
    for label in &story.labels {
        if let Some(cat) = mappings.labels.get(label.name.as_str()) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: EXTERNAL_SOURCE_CONFIDENCE,
                source: format!("shortcut:label:{}", label.name),
            });
        }
    }

    // Priority 3: workflow state.
    if let Some(ws) = &story.workflow_state {
        if let Some(cat) = mappings.workflow_state.get(ws.name.as_str()) {
            return Some(ExternalSignal {
                category: cat.clone(),
                confidence: EXTERNAL_SOURCE_CONFIDENCE,
                source: format!("shortcut:workflow_state:{}", ws.name),
            });
        }
    }

    None
}

/// Fetch a Shortcut story by ID.
///
/// Why: the HTTP call must be isolated here so the resolver can inject a
/// mock client via its test-seam override.
/// What: issues `GET {base_url}/api/v3/stories/{id}` with the Shortcut
/// API token in the `Shortcut-Token` header. Returns `None` on any error
/// or when the token env var is unset.
/// Test: integration-tested via the resolver with wiremock.
pub async fn fetch_story(
    client: &reqwest::Client,
    config: &ShortcutSourceConfig,
    id: u64,
    base_url_override: Option<&str>,
) -> Option<ShortcutStory> {
    let token = match std::env::var(&config.api_token_env) {
        Ok(t) if !t.is_empty() => t,
        _ => {
            warn!(
                api_token_env = %config.api_token_env,
                "Shortcut API token env var `{}` is not set — did you `export {}` before running tga?",
                config.api_token_env, config.api_token_env,
            );
            return None;
        }
    };

    let base = base_url_override.unwrap_or("https://api.app.shortcut.com");
    let url = format!("{base}/api/v3/stories/{id}");

    let resp = match client
        .get(&url)
        .header("Shortcut-Token", &token)
        .header("Content-Type", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(id, error = %e, "Shortcut API request failed; skipping");
            return None;
        }
    };

    if !resp.status().is_success() {
        warn!(
            id,
            status = %resp.status(),
            "Shortcut API returned non-success status; skipping"
        );
        return None;
    }

    match resp.json::<ShortcutStory>().await {
        Ok(story) => Some(story),
        Err(e) => {
            warn!(id, error = %e, "failed to parse Shortcut story response; skipping");
            None
        }
    }
}

/// Fetch a batch of Shortcut stories.
///
/// Why: same cache-before-fetch rationale as the JIRA/Linear batch helpers.
/// What: deduplicates `ids`, fetches each unique story, and returns a map
/// from story ID string to `Option<ExternalSignal>`.
/// Test: covered by resolver integration tests.
pub async fn fetch_stories_batch(
    client: &reqwest::Client,
    config: &ShortcutSourceConfig,
    ids: &[u64],
    base_url_override: Option<&str>,
) -> HashMap<String, Option<ExternalSignal>> {
    let mut out = HashMap::new();
    for &id in ids {
        let key = id.to_string();
        if out.contains_key(&key) {
            continue;
        }
        let story = fetch_story(client, config, id, base_url_override).await;
        let signal = story.and_then(|s| classify_story(&s, config));
        out.insert(key, signal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the `[ch1234]` bracket form is the legacy Clubhouse format and
    /// the most common reference style in pre-Shortcut codebases.
    /// What: assert extraction from `[ch<N>]` references.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_shortcut_ids_bracket_form() {
        let ids = extract_shortcut_ids("fix: [ch1234] resolve null pointer");
        assert_eq!(ids, vec![1234u64]);
    }

    /// Why: the `sc-1234` form is used in Shortcut branch names and modern
    /// commit conventions.
    /// What: assert extraction from `sc-<N>` references.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_shortcut_ids_sc_form() {
        let ids = extract_shortcut_ids("feat: sc-42 add user profile");
        assert_eq!(ids, vec![42u64]);
    }

    /// Why: both forms may appear in the same commit; the extractor must
    /// return all unique IDs without duplicates.
    /// What: assert multi-ref extraction and deduplication.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_shortcut_ids_both_forms_and_dedup() {
        let ids = extract_shortcut_ids("fix: [ch100] and sc-200 (see [ch100] again)");
        assert_eq!(ids, vec![100u64, 200u64]);
    }

    /// Why: messages with no Shortcut references must yield an empty vec.
    /// What: assert empty result on messages without recognized patterns.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_shortcut_ids_no_match() {
        assert!(extract_shortcut_ids("feat: add login flow").is_empty());
        assert!(extract_shortcut_ids("fix: PROJ-123 jira style").is_empty());
    }

    /// Why: `classify_story` must prefer story_type over labels over
    /// workflow_state, matching the documented priority order.
    /// What: build a story with all three fields populated; assert story_type wins.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_story_type_wins_over_labels() {
        use std::collections::HashMap;
        let story = ShortcutStory {
            id: 1,
            story_type: "bug".to_string(),
            labels: vec![ShortcutLabel {
                name: "enhancement".to_string(),
            }],
            workflow_state: Some(ShortcutWorkflowState {
                name: "Done".to_string(),
            }),
        };
        let config = ShortcutSourceConfig {
            api_token_env: "SHORTCUT_API_TOKEN".to_string(), // pragma: allowlist secret
            workspace_id: "myco".to_string(),
            field_mappings: crate::classify::sources::ShortcutFieldMappings {
                story_type: {
                    let mut m = HashMap::new();
                    m.insert("bug".to_string(), "bug_fix".to_string());
                    m
                },
                labels: {
                    let mut m = HashMap::new();
                    m.insert("enhancement".to_string(), "new_feature".to_string());
                    m
                },
                workflow_state: {
                    let mut m = HashMap::new();
                    m.insert("Done".to_string(), "completed".to_string());
                    m
                },
            },
        };
        let signal = classify_story(&story, &config).expect("should match");
        assert_eq!(signal.category, "bug_fix");
        assert!(signal.source.contains("story_type"));
    }

    /// Why: when story_type is unmapped, labels should be the next fallback.
    /// What: build a story with no story-type mapping but a matching label.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_falls_through_to_labels() {
        use std::collections::HashMap;
        let story = ShortcutStory {
            id: 2,
            story_type: "chore".to_string(), // not in mappings
            labels: vec![ShortcutLabel {
                name: "security".to_string(),
            }],
            workflow_state: None,
        };
        let config = ShortcutSourceConfig {
            api_token_env: "SHORTCUT_API_TOKEN".to_string(), // pragma: allowlist secret
            workspace_id: "myco".to_string(),
            field_mappings: crate::classify::sources::ShortcutFieldMappings {
                story_type: HashMap::new(),
                labels: {
                    let mut m = HashMap::new();
                    m.insert("security".to_string(), "security".to_string());
                    m
                },
                workflow_state: HashMap::new(),
            },
        };
        let signal = classify_story(&story, &config).expect("should match via label");
        assert_eq!(signal.category, "security");
        assert!(signal.source.contains("label"));
    }

    /// Why: when no field matches, `classify_story` must return `None` so
    /// the pipeline falls through to commit-message rules.
    /// What: build a story with no mapped fields.
    /// Test: pure function, no HTTP.
    #[test]
    fn classify_returns_none_on_no_match() {
        use std::collections::HashMap;
        let story = ShortcutStory {
            id: 3,
            story_type: "unknown_type".to_string(),
            labels: vec![],
            workflow_state: None,
        };
        let config = ShortcutSourceConfig {
            api_token_env: "SHORTCUT_API_TOKEN".to_string(), // pragma: allowlist secret
            workspace_id: "myco".to_string(),
            field_mappings: crate::classify::sources::ShortcutFieldMappings {
                story_type: HashMap::new(),
                labels: HashMap::new(),
                workflow_state: HashMap::new(),
            },
        };
        assert!(classify_story(&story, &config).is_none());
    }

    /// Why: the Shortcut source config must round-trip through YAML
    /// deserialization with `deny_unknown_fields` so config typos are caught.
    /// What: deserialize a full `type: shortcut` source config and assert fields.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn shortcut_source_config_deserializes() {
        use crate::classify::sources::SourceConfig;
        let yaml = r#"
type: shortcut
api_token_env: SHORTCUT_API_TOKEN
workspace_id: myco
field_mappings:
  story_type:
    bug: bug_fix
    feature: new_feature
    chore: tech_debt_refactoring
  labels:
    security: security
  workflow_state: {}
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Shortcut(s) => {
                assert_eq!(s.api_token_env, "SHORTCUT_API_TOKEN"); // pragma: allowlist secret
                assert_eq!(s.workspace_id, "myco");
                assert_eq!(
                    s.field_mappings.story_type.get("bug"),
                    Some(&"bug_fix".to_string())
                );
                assert_eq!(
                    s.field_mappings.story_type.get("feature"),
                    Some(&"new_feature".to_string())
                );
            }
            other => panic!("expected Shortcut variant, got {other:?}"),
        }
    }

    /// Why: `deny_unknown_fields` on `ShortcutSourceConfig` must reject YAML
    /// typos with a loud parse error.
    /// What: attempt to deserialize with an unknown field and assert `Err`.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn shortcut_source_config_unknown_field_is_rejected() {
        let yaml = r#"
type: shortcut
api_token_env: SHORTCUT_API_TOKEN
workspace_id: myco
workspace_slug: myco
field_mappings:
  story_type: {}
  labels: {}
  workflow_state: {}
"#;
        let result: Result<crate::classify::sources::SourceConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "unknown field must be rejected");
    }

    /// Why: wiremock integration test — verifies the full HTTP path including
    /// Shortcut-Token header and response parsing.
    /// What: mock the Shortcut REST endpoint returning a bug story; assert
    /// fetch returns the correct story and classification fires correctly.
    /// Test: wiremock mock of Shortcut Stories API.
    #[tokio::test]
    async fn fetch_and_classify_via_wiremock() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": 55,
            "story_type": "bug",
            "labels": [{"name": "ktlo"}],
            "workflow_state": {"name": "Done"}
        });

        Mock::given(method("GET"))
            .and(path("/api/v3/stories/55"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        unsafe { std::env::set_var("SHORTCUT_TOKEN_WT", "test-token") }; // pragma: allowlist secret

        use std::collections::HashMap;
        let config = ShortcutSourceConfig {
            api_token_env: "SHORTCUT_TOKEN_WT".to_string(), // pragma: allowlist secret
            workspace_id: "myco".to_string(),
            field_mappings: crate::classify::sources::ShortcutFieldMappings {
                story_type: {
                    let mut m = HashMap::new();
                    m.insert("bug".to_string(), "bug_fix".to_string());
                    m
                },
                labels: HashMap::new(),
                workflow_state: HashMap::new(),
            },
        };

        let client = reqwest::Client::new();
        let story = fetch_story(&client, &config, 55, Some(&server.uri()))
            .await
            .expect("fetch should succeed");

        let signal = classify_story(&story, &config).expect("should classify");
        assert_eq!(signal.category, "bug_fix");
        assert!(signal.source.contains("story_type"));

        unsafe { std::env::remove_var("SHORTCUT_TOKEN_WT") };
    }
}
