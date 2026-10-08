//! A credentialed trusty-search URL never reaches review output (#9431).
//!
//! Why: with search unreachable, `result.error`, `review_body` (what
//! `review_pr` posts) and the log lines printed the configured URL verbatim,
//! userinfo password and `access_token` value included.
//! What: drives the real pipeline against an unreachable credentialed URL
//! through a real `HttpSearchClient`, capturing every tracing line, and checks
//! each sink drops the secret but keeps the host.
//! Test: `credential_url_never_reaches_result_error`,
//! `credential_url_never_reaches_review_body`,
//! `credential_url_never_reaches_a_log_line`.

use std::sync::Mutex;

use super::*;
use crate::integrations::HttpSearchClient;

/// The fake credential, used as both the password and the token value.
const SECRET: &str = "fake123fake";
/// The B6 smoke-run URL: port 9 refuses, so search is down.
const URL: &str = "http://user:fake123fake@127.0.0.1:9/p?access_token=fake123fake";
/// What each sink must still name.
const HOST: &str = "127.0.0.1:9";

/// Collects formatted log output (the `arn_tests` capture pattern).
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// One degraded local-diff review against [`URL`], with its log output.
async fn review_against_credential_url() -> (ReviewResult, String) {
    let (source, _tmp) = super::local_diff_source("+fn x() {}\n");
    let mut config = super::default_config();
    config.search_url = URL.to_string();
    config.context.require_search = Some(false); // degrade, so the LLM runs
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::Interactive,
    };
    let deps = ReviewDeps {
        llm: Arc::new(super::FakeLlm::approves()),
        verifier: None,
        search: Arc::new(HttpSearchClient::new(URL).expect("client")),
        analyze: Some(Arc::new(super::ReadyAnalyze)),
        dedup: None,
    };
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    let result = {
        // A current-thread runtime runs every task on this thread.
        let _guard = tracing::subscriber::set_default(subscriber);
        run_review(&config, input, deps).await
    };
    let bytes = capture.0.lock().expect("capture lock").clone();
    (result, String::from_utf8(bytes).expect("utf8 log output"))
}

/// Sink 1: the degraded reason in `result.error`.
#[tokio::test]
async fn credential_url_never_reaches_result_error() {
    let (result, _) = review_against_credential_url().await;
    assert_eq!(
        result.status,
        ReviewStatus::Degraded,
        "masking keeps the outcome"
    );
    let error = result.error.expect("a degraded review records its reason");
    assert!(!error.contains(SECRET), "secret in result.error: {error}");
    assert!(error.contains(HOST), "host lost from result.error: {error}");
}

/// Sink 2: the banner in `review_body`, the text `review_pr` posts.
#[tokio::test]
async fn credential_url_never_reaches_review_body() {
    let (result, _) = review_against_credential_url().await;
    let body = &result.review_body;
    assert!(body.contains("NOT AUTHORITATIVE"), "banner missing: {body}");
    assert!(!body.contains(SECRET), "secret in review_body: {body}");
    assert!(body.contains(HOST), "host lost from review_body: {body}");
}

/// Sink 3: every tracing line the run emits (stderr in production).
#[tokio::test]
async fn credential_url_never_reaches_a_log_line() {
    let (_, log) = review_against_credential_url().await;
    let leaked: Vec<&str> = log.lines().filter(|l| l.contains(SECRET)).collect();
    assert!(
        leaked.is_empty(),
        "secret in {} log line(s): {leaked:#?}",
        leaked.len()
    );
    assert!(log.contains(HOST), "no log line names the host: {log}");
}
