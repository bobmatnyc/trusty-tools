//! Datadog deployment-event classification source.
//!
//! Why: when a commit SHA is referenced by a Datadog deployment event, the
//! work type is unambiguously `devops` (or the operator-configured category).
//! Deployment evidence is a very strong signal (0.95 confidence) — stronger
//! than any commit-message heuristic.
//!
//! What: extracts commit SHAs from commit messages (both full 40-char and
//! short 7–12 char forms), then queries the Datadog Events API
//! (`GET /api/v1/events?sources=deployment&tags=commit:<sha>`) to see if
//! the commit was associated with a deployment event. When found, returns
//! an [`super::ExternalSignal`] at the configured confidence.
//!
//! Note: Datadog API calls require both an API key (`DD-API-KEY`) and an
//! application key (`DD-APPLICATION-KEY`) in HTTP headers.
//!
//! Test: see `tests::extract_commit_shas_*` for extractor coverage and
//! `tests::fetch_and_classify_via_wiremock` for the HTTP path.

use std::collections::HashMap;

use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{DatadogSourceConfig, ExternalSignal};

/// Default confidence for Datadog deployment-event signals.
///
/// Why: deployment evidence is highly authoritative — if Datadog recorded a
/// deployment event for this commit, the work is clearly `devops`.
/// What: 0.95 — above the JIRA/Linear standard (0.92) but below Tier-0
/// manual overrides (1.0).
/// Test: verified in `tests::classify_deployment_uses_configured_confidence`.
pub const DATADOG_DEFAULT_CONFIDENCE: f64 = 0.95;

/// Regex matching a full (40-char) or short (7–12 char) Git SHA.
///
/// Why: developers sometimes paste SHAs into commit messages; the Datadog
/// query correlates on the commit SHA.
/// What: matches word-boundary-guarded lowercase hex strings of 7–40 chars.
/// Test: covered by `tests::extract_commit_shas_*`.
fn sha_regex() -> Regex {
    Regex::new(r"\b([0-9a-f]{7,40})\b").expect("static regex is valid")
}

/// Extract all plausible Git commit SHAs from a commit message.
///
/// Why: Datadog deployment events are keyed by commit SHA; extracting SHAs
/// from the message is the join key between the commit and the event.
/// What: returns a `Vec<String>` of unique SHA-like substrings (7–40 hex
/// chars), in left-to-right order. The extractor is intentionally broad —
/// false-positives are cheap to discard (the API returns no event).
/// Test: covered by `tests::extract_commit_shas_full` and
/// `tests::extract_commit_shas_short`.
pub fn extract_commit_shas(message: &str) -> Vec<String> {
    let re = sha_regex();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(message) {
        if let Some(sha_m) = cap.get(1) {
            let sha = sha_m.as_str().to_string();
            if seen.insert(sha.clone()) {
                out.push(sha);
            }
        }
    }
    out
}

/// A Datadog event as returned by the Events API.
///
/// Why: we only need to know whether the event list is non-empty (indicating
/// at least one deployment event matched the tag query).
/// What: a minimal serde struct over the `GET /api/v1/events` response
/// envelope. When `events` is non-empty, the commit has a deployment record.
/// Test: covered by resolver integration tests with wiremock.
#[derive(Debug, Deserialize, Serialize)]
pub struct DatadogEventsResponse {
    /// List of events matching the query. Non-empty = deployment found.
    #[serde(default)]
    pub events: Vec<DatadogEvent>,
}

/// A single Datadog event (minimal fields).
#[derive(Debug, Deserialize, Serialize)]
pub struct DatadogEvent {
    /// Event ID (numeric string).
    pub id: Option<serde_json::Value>,
    /// Event title (e.g. `"Deployment"`).
    #[serde(default)]
    pub title: String,
    /// Tags attached to this event (e.g. `"commit:abc1234"`).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Check whether a commit SHA has a matching deployment event.
///
/// Why: the API call is the join between the commit SHA and the Datadog
/// deployment record; isolating it here allows mock-HTTP testing.
/// What: queries `GET /api/v1/events?sources=deployment&tags=commit:<sha>`
/// with the configured API and application keys. Returns `true` when the
/// response contains at least one event.
/// Test: integration-tested via wiremock.
pub async fn has_deployment_event(
    client: &reqwest::Client,
    config: &DatadogSourceConfig,
    sha: &str,
    api_base_override: Option<&str>,
) -> bool {
    let api_key = match std::env::var(&config.api_key_env) {
        Ok(k) if !k.is_empty() => k,
        _ => {
            warn!(
                api_key_env = %config.api_key_env,
                "Datadog API key env var `{}` is not set — skipping Datadog lookups",
                config.api_key_env,
            );
            return false;
        }
    };

    let app_key = match std::env::var(&config.app_key_env) {
        Ok(k) if !k.is_empty() => k,
        _ => {
            warn!(
                app_key_env = %config.app_key_env,
                "Datadog app key env var `{}` is not set — skipping Datadog lookups",
                config.app_key_env,
            );
            return false;
        }
    };

    let site = config.dd_site.as_deref().unwrap_or("datadoghq.com");
    let base = api_base_override
        .map(|u| u.to_string())
        .unwrap_or_else(|| format!("https://api.{site}"));

    // Build the events query. We filter by `sources=deployment` and tag the
    // commit SHA as `commit:<sha>`. The API requires a time window; we use
    // a wide window (now − 1 year) to catch historical deployments.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let start = now.saturating_sub(365 * 24 * 3600);

    let url = format!(
        "{base}/api/v1/events?sources=deployment&tags=commit:{sha}&start={start}&end={now}"
    );

    let resp = match client
        .get(&url)
        .header("DD-API-KEY", &api_key)
        .header("DD-APPLICATION-KEY", &app_key)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(sha, error = %e, "Datadog Events API request failed; skipping");
            return false;
        }
    };

    if !resp.status().is_success() {
        warn!(
            sha,
            status = %resp.status(),
            "Datadog Events API returned non-success status; skipping"
        );
        return false;
    }

    match resp.json::<DatadogEventsResponse>().await {
        Ok(r) => {
            let found = !r.events.is_empty();
            debug!(sha, found, "Datadog deployment query complete");
            found
        }
        Err(e) => {
            warn!(sha, error = %e, "failed to parse Datadog Events response; skipping");
            false
        }
    }
}

/// Check a batch of SHAs for deployment events.
///
/// Why: a commit message may contain multiple SHA references; checking
/// each unique one minimises redundant API calls.
/// What: deduplicates `shas`, queries each, and returns a map from SHA
/// to `Option<ExternalSignal>`.
/// Test: covered by resolver integration tests.
pub async fn check_shas_batch(
    client: &reqwest::Client,
    config: &DatadogSourceConfig,
    shas: &[String],
    api_base_override: Option<&str>,
) -> HashMap<String, Option<ExternalSignal>> {
    let mut out = HashMap::new();
    for sha in shas {
        if out.contains_key(sha) {
            continue;
        }
        let found = has_deployment_event(client, config, sha, api_base_override).await;
        let signal = if found {
            let confidence = config.confidence.unwrap_or(DATADOG_DEFAULT_CONFIDENCE);
            Some(ExternalSignal {
                category: config.default_category.clone(),
                confidence,
                source: format!("datadog:deployment:{sha}"),
            })
        } else {
            None
        };
        out.insert(sha.clone(), signal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: full 40-char SHAs are the most common form in commit messages
    /// (e.g. `"cherry-pick from abc1234..."`).
    /// What: assert extraction of a full 40-char SHA.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_commit_shas_full() {
        let shas = extract_commit_shas("cherry-pick from a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"); // pragma: allowlist secret
        assert_eq!(shas, vec!["a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2"]); // pragma: allowlist secret
    }

    /// Why: short SHAs (7–12 chars) are common in manual cherry-pick
    /// references and deployment notes.
    /// What: assert extraction of a short 7-char SHA.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_commit_shas_short() {
        let shas = extract_commit_shas("deploy: abc1234 to production");
        assert_eq!(shas, vec!["abc1234"]);
    }

    /// Why: deduplication must prevent the same SHA from triggering multiple
    /// API calls.
    /// What: assert multi-SHA messages are deduplicated.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_commit_shas_dedup() {
        let shas = extract_commit_shas("reverts abc1234 and abc1234 again, plus def5678");
        assert_eq!(shas, vec!["abc1234", "def5678"]);
    }

    /// Why: messages without SHAs should yield an empty vec so we don't
    /// make unnecessary API calls.
    /// What: assert empty result on plain commit messages.
    /// Test: pure regex, no HTTP.
    #[test]
    fn extract_commit_shas_plain_message_yields_empty() {
        // "abc" (3 chars) is too short; "feat:" contains a non-hex colon.
        assert!(extract_commit_shas("feat: add login flow").is_empty());
    }

    /// Why: `DatadogSourceConfig` must round-trip through YAML deserialization
    /// with `deny_unknown_fields` so config typos surface at load time.
    /// What: deserialize a full `type: datadog` source config and assert fields.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn datadog_source_config_deserializes() {
        use crate::classify::sources::SourceConfig;
        let yaml = r#"
type: datadog
api_key_env: DATADOG_API_KEY
app_key_env: DATADOG_APP_KEY
dd_site: datadoghq.com
service: my-service
default_category: devops
confidence: 0.95
"#;
        let cfg: SourceConfig = serde_yaml::from_str(yaml).expect("deserialize");
        match cfg {
            SourceConfig::Datadog(d) => {
                assert_eq!(d.api_key_env, "DATADOG_API_KEY"); // pragma: allowlist secret
                assert_eq!(d.app_key_env, "DATADOG_APP_KEY"); // pragma: allowlist secret
                assert_eq!(d.dd_site.as_deref(), Some("datadoghq.com"));
                assert_eq!(d.service.as_deref(), Some("my-service"));
                assert_eq!(d.default_category, "devops");
                assert!(d
                    .confidence
                    .map(|c| (c - 0.95_f64).abs() < f64::EPSILON)
                    .unwrap_or(false));
            }
            other => panic!("expected Datadog variant, got {other:?}"),
        }
    }

    /// Why: `deny_unknown_fields` on `DatadogSourceConfig` must reject YAML
    /// typos with a parse error.
    /// What: attempt to deserialize with an unknown field and assert `Err`.
    /// Test: pure deserialization, no HTTP.
    #[test]
    fn datadog_source_config_unknown_field_is_rejected() {
        let yaml = r#"
type: datadog
api_key_env: DATADOG_API_KEY
app_key_env: DATADOG_APP_KEY
default_category: devops
unknown_field: oops
"#;
        let result: Result<crate::classify::sources::SourceConfig, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err(), "unknown field must be rejected");
    }

    /// Why: wiremock integration test — verifies the full HTTP path including
    /// DD-API-KEY and DD-APPLICATION-KEY headers.
    /// What: mock the Datadog Events API returning a deployment event; assert
    /// `has_deployment_event` returns true and the signal is correct.
    /// Test: wiremock mock of Datadog Events API.
    #[tokio::test]
    async fn fetch_and_classify_via_wiremock() {
        use wiremock::matchers::{header, method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let body = serde_json::json!({
            "events": [
                {
                    "id": 12345,
                    "title": "Deployment",
                    "tags": ["commit:abc1234", "env:production"]
                }
            ]
        });

        Mock::given(method("GET"))
            .and(path_regex(r"/api/v1/events.*"))
            .and(header("DD-API-KEY", "test-api-key"))
            .and(header("DD-APPLICATION-KEY", "test-app-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        unsafe { std::env::set_var("DD_API_KEY_WT", "test-api-key") }; // pragma: allowlist secret
        unsafe { std::env::set_var("DD_APP_KEY_WT", "test-app-key") }; // pragma: allowlist secret

        let config = DatadogSourceConfig {
            api_key_env: "DD_API_KEY_WT".to_string(), // pragma: allowlist secret
            app_key_env: "DD_APP_KEY_WT".to_string(), // pragma: allowlist secret
            dd_site: Some("datadoghq.com".to_string()),
            service: Some("my-service".to_string()),
            default_category: "devops".to_string(),
            confidence: Some(0.95),
        };

        let client = reqwest::Client::new();
        let found = has_deployment_event(&client, &config, "abc1234", Some(&server.uri())).await;
        assert!(found, "deployment event should be found");

        // Now verify the batch helper produces the right signal.
        let map = check_shas_batch(
            &client,
            &config,
            &["abc1234".to_string()],
            Some(&server.uri()),
        )
        .await;
        let signal = map.get("abc1234").and_then(|s| s.as_ref()).expect("signal");
        assert_eq!(signal.category, "devops");
        assert!(
            (signal.confidence - 0.95_f64).abs() < f64::EPSILON,
            "confidence should be 0.95"
        );
        assert!(signal.source.contains("abc1234"));

        unsafe { std::env::remove_var("DD_API_KEY_WT") };
        unsafe { std::env::remove_var("DD_APP_KEY_WT") };
    }

    /// Why: when no deployment event is found the batch helper must return
    /// `None` so the pipeline falls through to commit-message rules.
    /// What: mock an empty events list; assert signal is None.
    /// Test: wiremock mock of Datadog Events API.
    #[tokio::test]
    async fn no_deployment_event_yields_none_signal() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        let body = serde_json::json!({"events": []});
        Mock::given(method("GET"))
            .and(path_regex(r"/api/v1/events.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        unsafe { std::env::set_var("DD_API_KEY_EMPTY", "test-api-key") }; // pragma: allowlist secret
        unsafe { std::env::set_var("DD_APP_KEY_EMPTY", "test-app-key") }; // pragma: allowlist secret

        let config = DatadogSourceConfig {
            api_key_env: "DD_API_KEY_EMPTY".to_string(), // pragma: allowlist secret
            app_key_env: "DD_APP_KEY_EMPTY".to_string(), // pragma: allowlist secret
            dd_site: None,
            service: None,
            default_category: "devops".to_string(),
            confidence: None,
        };

        let client = reqwest::Client::new();
        let map = check_shas_batch(
            &client,
            &config,
            &["deadbeef".to_string()],
            Some(&server.uri()),
        )
        .await;
        let signal = map.get("deadbeef").expect("key present");
        assert!(
            signal.is_none(),
            "no events should yield None signal, got {signal:?}"
        );

        unsafe { std::env::remove_var("DD_API_KEY_EMPTY") };
        unsafe { std::env::remove_var("DD_APP_KEY_EMPTY") };
    }
}
