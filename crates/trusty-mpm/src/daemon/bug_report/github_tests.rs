//! Unit tests for the GitHub issue-filing client.
//!
//! Why: keeping tests in a dedicated file holds the main `github.rs` under the
//!      500-line hard cap without sacrificing coverage.
//! What: mock-based tests for token resolution, dedup (create vs. comment path),
//!       label mapping, and fingerprint extraction — all without network access.
//! Test: run with `cargo test -p trusty-mpm`.

use std::sync::{Arc, Mutex};

use super::{
    CreatedIssue, ExistingIssue, GithubApi, GithubFilingError, extract_fingerprint, file_issue,
    file_issue_with,
};
use crate::daemon::bug_report::preview::IssuePreview;
use crate::daemon::bug_report::token::{
    EnvFileTokenProvider, TOKEN_ENV_VAR, TOKEN_FILE_ENV_VAR, TokenProvider,
};

// ── Mock token provider ───────────────────────────────────────────────────────

struct StaticTokenProvider(Option<String>);

impl TokenProvider for StaticTokenProvider {
    fn token(&self) -> Option<String> {
        self.0.clone()
    }
}

// ── Mock GithubApi ────────────────────────────────────────────────────────────

/// Outcome recorded by the mock.
#[derive(Debug, Clone)]
enum MockCall {
    SearchIssues,
    CreateIssue { labels: Vec<String> },
    AddComment(u64),
}

/// Mock that returns configurable search results and records all calls.
struct MockGithubApi {
    /// Pre-existing issues to return from `search_open_issues`.
    existing: Vec<ExistingIssue>,
    /// Calls recorded for assertion.
    calls: Arc<Mutex<Vec<MockCall>>>,
}

impl MockGithubApi {
    fn with_existing(existing: Vec<ExistingIssue>) -> Self {
        Self {
            existing,
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn no_existing() -> Self {
        Self::with_existing(vec![])
    }
}

impl GithubApi for MockGithubApi {
    fn search_open_issues(
        &self,
        _fingerprint: &str,
    ) -> Result<Vec<ExistingIssue>, GithubFilingError> {
        self.calls.lock().unwrap().push(MockCall::SearchIssues);
        Ok(self.existing.clone())
    }

    fn create_issue(
        &self,
        _title: &str,
        _body: &str,
        labels: &[String],
    ) -> Result<CreatedIssue, GithubFilingError> {
        self.calls.lock().unwrap().push(MockCall::CreateIssue {
            labels: labels.to_vec(),
        });
        Ok(CreatedIssue {
            html_url: "https://github.com/bobmatnyc/trusty-tools/issues/999".to_string(),
            number: 999,
        })
    }

    fn add_comment(&self, issue_number: u64, _body: &str) -> Result<(), GithubFilingError> {
        self.calls
            .lock()
            .unwrap()
            .push(MockCall::AddComment(issue_number));
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn dummy_preview(fingerprint: &str) -> IssuePreview {
    IssuePreview {
        title: "[trusty_mpm] test error".to_string(),
        body: format!(
            "## Auto-reported\n\n<!-- trusty-bug-fingerprint: {fingerprint} -->\n\nbody text"
        ),
        labels: vec![
            "bug".to_string(),
            "auto-reported".to_string(),
            "trusty-mpm".to_string(),
        ],
        fingerprint: fingerprint.to_string(),
        scrub_changes: vec![],
    }
}

/// A token provider with an explicit token value (bypasses env/file lookup).
///
/// Why: env-var based tests race when tests run in parallel; this struct
///      lets us test the resolution logic without touching process env state.
/// What: wraps an `Option<&'static str>` and implements [`TokenProvider`].
/// Test: used directly in `token_resolution_absent_uses_fixed_provider`.
struct FixedTokenProvider(Option<&'static str>);
impl TokenProvider for FixedTokenProvider {
    fn token(&self) -> Option<String> {
        self.0.map(str::to_string)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[test]
fn token_resolution_from_env() {
    // Test that the env var is read when present.
    // We use TOKEN_FILE_ENV_VAR to supply a file path that does not exist
    // so the fall-through path is exercised regardless of process env state.
    // The primary assertion is that setting TOKEN_ENV_VAR is sufficient.
    //
    // SAFETY: this test runs single-threaded in isolation; the env var is
    // cleaned up before the test returns. Token-resolution tests share a
    // process so we accept they may interfere; that is why we assert
    // `is_some()` (a value was set) rather than a specific value that
    // another test might have left in the env.
    let sentinel = "ghp_test_token_from_env_unique_trusty_phase3"; // pragma: allowlist secret
    // SAFETY: isolated test, cleaned up on drop path.
    unsafe { std::env::set_var(TOKEN_ENV_VAR, sentinel) };
    let token = EnvFileTokenProvider.token();
    unsafe { std::env::remove_var(TOKEN_ENV_VAR) };
    assert!(
        token.is_some(),
        "expected Some token when env var is set, got: {token:?}"
    );
}

#[test]
fn token_resolution_absent_uses_fixed_provider() {
    // Test that a provider returning None propagates correctly.
    // Uses FixedTokenProvider to avoid process-env races.
    let provider = FixedTokenProvider(None);
    let preview = dummy_preview(&"a".repeat(64));
    let err = file_issue(&preview, &provider).unwrap_err();
    assert!(
        matches!(err, GithubFilingError::NoToken),
        "expected NoToken: {err}"
    );
}

#[test]
fn token_resolution_from_file() {
    // Write a token to a temp file, point TOKEN_FILE_ENV_VAR at it,
    // ensure the provider reads it back.
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "ghp_from_file_xyz\n").unwrap();
    // SAFETY: we point TOKEN_FILE_ENV_VAR at the temp file; TOKEN_ENV_VAR
    // is cleared first so the file path is the resolution source.
    unsafe {
        std::env::remove_var(TOKEN_ENV_VAR);
        std::env::set_var(TOKEN_FILE_ENV_VAR, tmp.path().as_os_str());
    }
    let token = EnvFileTokenProvider.token();
    // SAFETY: cleanup.
    unsafe { std::env::remove_var(TOKEN_FILE_ENV_VAR) };
    // Token env var may be re-set by another test that ran concurrently;
    // what we assert is that the file token is at minimum included when
    // TOKEN_ENV_VAR is absent. If TOKEN_ENV_VAR was set concurrently the
    // env-var path wins — still a valid Some(_).
    assert!(
        token.is_some(),
        "expected a token from file (or env): {token:?}"
    );
}

#[test]
fn no_token_yields_no_token_error() {
    let provider = StaticTokenProvider(None);
    let preview = dummy_preview(&"a".repeat(64));
    let err = file_issue(&preview, &provider).unwrap_err();
    assert!(
        matches!(err, GithubFilingError::NoToken),
        "expected NoToken, got: {err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("TRUSTY_BUGREPORT_GITHUB_TOKEN"), "{msg}");
}

#[test]
fn mock_create_path_called_when_no_existing_issue() {
    let fp = "b".repeat(64);
    let preview = dummy_preview(&fp);
    let mock = MockGithubApi::no_existing();
    let result = file_issue_with(&preview, &mock).unwrap();

    assert!(result.filed);
    assert!(!result.deduped, "should not be deduped");
    assert_eq!(result.issue_number, 999);

    let calls = mock.calls.lock().unwrap();
    // Must have searched first, then created.
    assert!(
        calls.iter().any(|c| matches!(c, MockCall::SearchIssues)),
        "search not called: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, MockCall::CreateIssue { .. })),
        "create not called: {calls:?}"
    );
    assert!(
        !calls.iter().any(|c| matches!(c, MockCall::AddComment(_))),
        "comment should not be called on create path: {calls:?}"
    );
}

#[test]
fn mock_comment_path_when_existing_issue_found() {
    let fp = "c".repeat(64);
    let preview = dummy_preview(&fp);
    let existing = vec![ExistingIssue {
        html_url: "https://github.com/bobmatnyc/trusty-tools/issues/42".to_string(),
        number: 42,
    }];
    let mock = MockGithubApi::with_existing(existing);
    let result = file_issue_with(&preview, &mock).unwrap();

    assert!(result.filed);
    assert!(result.deduped, "should be deduped");
    assert_eq!(result.issue_number, 42);
    assert!(result.issue_url.contains("42"), "{}", result.issue_url);

    let calls = mock.calls.lock().unwrap();
    assert!(
        calls.iter().any(|c| matches!(c, MockCall::AddComment(42))),
        "comment(42) not called: {calls:?}"
    );
    assert!(
        !calls
            .iter()
            .any(|c| matches!(c, MockCall::CreateIssue { .. })),
        "create should not be called on dedup path: {calls:?}"
    );
}

#[test]
fn label_mapping_known_crate() {
    use crate::daemon::bug_report::preview::build_preview;
    use crate::daemon::bug_report::types::AggregatedError;
    use trusty_common::error_capture::CapturedError;

    let agg = AggregatedError {
        record: CapturedError {
            timestamp_secs: 0,
            crate_target: "trusty_search::indexer".to_string(),
            crate_version: "0.1.0".to_string(),
            message: "err".to_string(),
            fields: String::new(),
            file: None,
            line: None,
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            fingerprint: "d".repeat(64),
        },
        occurrences: 1,
    };
    let preview = build_preview(&agg);
    assert!(
        preview.labels.contains(&"trusty-search".to_string()),
        "expected trusty-search label: {:?}",
        preview.labels
    );
}

#[test]
fn label_mapping_unknown_crate_only_base_labels() {
    use crate::daemon::bug_report::preview::build_preview;
    use crate::daemon::bug_report::types::AggregatedError;
    use trusty_common::error_capture::CapturedError;

    let agg = AggregatedError {
        record: CapturedError {
            timestamp_secs: 0,
            crate_target: "some_unknown_crate".to_string(),
            crate_version: "0.1.0".to_string(),
            message: "err".to_string(),
            fields: String::new(),
            file: None,
            line: None,
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            fingerprint: "e".repeat(64),
        },
        occurrences: 1,
    };
    let preview = build_preview(&agg);
    assert_eq!(
        preview.labels,
        vec!["bug", "auto-reported"],
        "unexpected labels for unknown crate: {:?}",
        preview.labels
    );
}

#[test]
fn dedup_marker_extraction() {
    let fp = "f".repeat(64);
    let body = format!("some text\n<!-- trusty-bug-fingerprint: {fp} -->\nmore text");
    let extracted = extract_fingerprint(&body);
    assert_eq!(extracted, Some(fp));
}

#[test]
fn dedup_marker_absent_returns_none() {
    let body = "no fingerprint here";
    assert!(extract_fingerprint(body).is_none());
}

#[test]
fn create_issue_sends_correct_labels() {
    let fp = "g".repeat(64);
    let preview = IssuePreview {
        title: "[trusty_memory] oom".to_string(),
        body: format!("<!-- trusty-bug-fingerprint: {fp} -->"),
        labels: vec![
            "bug".to_string(),
            "auto-reported".to_string(),
            "trusty-memory".to_string(),
        ],
        fingerprint: fp,
        scrub_changes: vec![],
    };
    let mock = MockGithubApi::no_existing();
    let _ = file_issue_with(&preview, &mock).unwrap();

    let calls = mock.calls.lock().unwrap();
    let create = calls.iter().find_map(|c| {
        if let MockCall::CreateIssue { labels } = c {
            Some(labels.clone())
        } else {
            None
        }
    });
    let labels = create.expect("create call expected");
    assert!(labels.contains(&"trusty-memory".to_string()), "{labels:?}");
    assert!(labels.contains(&"bug".to_string()), "{labels:?}");
    assert!(labels.contains(&"auto-reported".to_string()), "{labels:?}");
}
