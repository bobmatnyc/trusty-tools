//! Tests for `tools::pm_bridge` — `RecordingBackend`-driven routing
//! assertions, `scrub_branding` coverage, schema/name black-box hygiene, and
//! the RBAC gate (epic #3052, PR B).

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::intent::route::BridgeRoute;
use crate::tools::ToolRegistry;

/// Mock backend that records the route it was invoked with and returns a
/// fixed, caller-supplied transcript — mirrors `delegate.rs`'s
/// `RecordingRunner`.
struct RecordingBackend {
    invoked_with: Mutex<Vec<(BridgeRoute, String)>>,
    response: String,
}

impl RecordingBackend {
    fn new(response: impl Into<String>) -> Self {
        Self {
            invoked_with: Mutex::new(Vec::new()),
            response: response.into(),
        }
    }
}

#[async_trait]
impl PmBridgeBackend for RecordingBackend {
    async fn run(&self, route: BridgeRoute, task: &str) -> Result<String> {
        self.invoked_with
            .lock()
            .unwrap()
            .push((route, task.to_string()));
        Ok(self.response.clone())
    }
}

/// Mock backend that always fails, to exercise the scrubbed-error path.
struct FailingBackend {
    message: String,
}

#[async_trait]
impl PmBridgeBackend for FailingBackend {
    async fn run(&self, _route: BridgeRoute, _task: &str) -> Result<String> {
        Err(anyhow::anyhow!(self.message.clone()))
    }
}

// =====================================================================
// Routing
// =====================================================================

#[tokio::test]
async fn dispatch_task_routes_code_task_to_tcode() {
    let backend = Arc::new(RecordingBackend::new("done"));
    let tool = PmBridgeTool::new(backend.clone());

    let result = tool
        .execute(json!({ "task": "fix the failing unit test in parser.rs" }))
        .await;

    assert!(!result.is_error(), "expected success: {}", result.content());
    let invoked = backend.invoked_with.lock().unwrap();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0].0, BridgeRoute::Tcode);
}

#[tokio::test]
async fn dispatch_task_routes_orchestration_task_to_tm() {
    let backend = Arc::new(RecordingBackend::new("done"));
    let tool = PmBridgeTool::new(backend.clone());

    let result = tool
        .execute(json!({ "task": "spawn a new session and check the backlog" }))
        .await;

    assert!(!result.is_error(), "expected success: {}", result.content());
    let invoked = backend.invoked_with.lock().unwrap();
    assert_eq!(invoked.len(), 1);
    assert_eq!(invoked[0].0, BridgeRoute::Tm);
}

#[tokio::test]
async fn dispatch_task_missing_task_arg_is_rejected_without_invoking_backend() {
    let backend = Arc::new(RecordingBackend::new("done"));
    let tool = PmBridgeTool::new(backend.clone());

    let result = tool.execute(json!({})).await;

    assert!(result.is_error());
    assert!(backend.invoked_with.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dispatch_task_empty_task_arg_is_rejected_without_invoking_backend() {
    let backend = Arc::new(RecordingBackend::new("done"));
    let tool = PmBridgeTool::new(backend.clone());

    let result = tool.execute(json!({ "task": "   " })).await;

    assert!(result.is_error());
    assert!(backend.invoked_with.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dispatch_task_scrubs_a_failing_backend_error() {
    let backend = Arc::new(FailingBackend {
        message: "failed to spawn tm serve --stdio: No such file or directory".to_string(),
    });
    let tool = PmBridgeTool::new(backend);

    let result = tool.execute(json!({ "task": "do something" })).await;

    assert!(result.is_error());
    let msg = result.content().to_lowercase();
    assert!(
        !msg.contains("tm serve") && !msg.contains(" tm "),
        "backend error must be scrubbed of backend identity, got: {}",
        result.content()
    );
}

// =====================================================================
// scrub_branding
// =====================================================================

#[test]
fn scrub_branding_removes_every_forbidden_token() {
    let sample = "Routed via tm to trusty-mpm, which handed off to tcode \
                  (trusty-code) for the actual edit.";
    let scrubbed = scrub_branding(sample);
    for forbidden in ["tm", "tcode", "trusty-mpm", "trusty-code"] {
        assert!(
            !scrubbed.to_lowercase().split_whitespace().any(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-') == forbidden
            }),
            "'{forbidden}' leaked into scrubbed output: {scrubbed}"
        );
    }
}

#[test]
fn scrub_branding_redacts_session_identifiers() {
    let sample = "session tm-quiet-falcon started; id=550e8400-e29b-41d4-a716-446655440000";
    let scrubbed = scrub_branding(sample);
    assert!(
        !scrubbed.contains("tm-quiet-falcon"),
        "tmux session name leaked: {scrubbed}"
    );
    assert!(
        !scrubbed.contains("550e8400-e29b-41d4-a716-446655440000"),
        "UUID session id leaked: {scrubbed}"
    );
    assert!(scrubbed.contains("[session]"), "got: {scrubbed}");
}

/// code-critic BLOCK finding 1 regression guard: `tm`'s own launch banner
/// prints the space-separated, title-case wordmark `Trusty MPM v{VERSION}`
/// (`crates/trusty-mpm/src/bin/tm/formatters/banner/mod.rs`'s narrow-terminal
/// fallback), and that banner text is literally the FIRST thing `run_tm`
/// observes via `session_activity`'s pane content — so it must scrub cleanly
/// even though it has no hyphen at all. The three lines below are three
/// SEPARATE `println!` calls in the real source
/// (`println!("\x1B[2J\x1B[1;1H"); println!("Trusty MPM v{}", ...);
/// println!("Launching...");`), each appending its own trailing `\n` — the
/// wordmark is newline-bounded on the real stdout, not glued to the escape
/// sequence.
#[test]
fn scrub_branding_removes_the_real_tm_launch_banner_plain_fallback() {
    let sample = "\u{1b}[2J\u{1b}[1;1H\nTrusty MPM v0.30.0\nLaunching...\n";
    let scrubbed = scrub_branding(sample);
    assert!(
        !scrubbed.to_lowercase().contains("trusty mpm"),
        "the real tm plain-fallback banner leaked: {scrubbed}"
    );
    assert!(
        scrubbed.contains("the system"),
        "expected the banner wordmark to be replaced, got: {scrubbed}"
    );
}

/// Sibling to the plain-fallback case: the two-panel box-drawing banner
/// embeds the identical wordmark inside a title-bar border
/// (`.../banner/two_panel/mod.rs`'s `render_title_bar`:
/// `format!(" Trusty MPM v{version} ")`, framed by `╭──── … ────╮`). Box
/// characters around the text must not defeat the word-bounded match.
#[test]
fn scrub_branding_removes_the_real_tm_launch_banner_box_form() {
    let sample = "\u{256d}\u{2500}\u{2500}\u{2500}\u{2500} Trusty MPM v0.30.0 \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}";
    let scrubbed = scrub_branding(sample);
    assert!(
        !scrubbed.to_lowercase().contains("trusty mpm"),
        "the real tm two-panel banner title bar leaked: {scrubbed}"
    );
    assert!(
        scrubbed.contains("the system"),
        "expected the banner wordmark to be replaced, got: {scrubbed}"
    );
}

/// Sanity-check other casings/spacings the real binaries could plausibly
/// emit (env-derived strings, ALL-CAPS log prefixes, underscore-joined
/// identifiers) beyond the two canonical banner forms above.
#[test]
fn scrub_branding_handles_assorted_casings_and_separators() {
    let cases = [
        "TRUSTY MPM daemon starting",
        "trusty_mpm.log rotated",
        "Trusty-Code session created",
        "TRUSTYCODE ready", // no separator at all
        "connecting to Trusty Code v0.2.0",
    ];
    for sample in cases {
        let scrubbed = scrub_branding(sample);
        let lower = scrubbed.to_lowercase();
        assert!(
            !lower.contains("trusty mpm")
                && !lower.contains("trusty_mpm")
                && !lower.contains("trusty-mpm")
                && !lower.contains("trusty code")
                && !lower.contains("trusty_code")
                && !lower.contains("trusty-code")
                && !lower.contains("trustycode"),
            "backend identity leaked from '{sample}': {scrubbed}"
        );
    }
}

#[test]
fn scrub_branding_leaves_unrelated_words_alone() {
    // Regression guard: a naive substring replace of "tm" would corrupt
    // "tmux", "atm", "item", "system" — the word-bounded regex must not.
    let sample = "The tmux pane showed an atm withdrawal item in the system log.";
    let scrubbed = scrub_branding(sample);
    assert_eq!(scrubbed, sample);
}

#[test]
fn scrub_branding_is_idempotent_on_clean_text() {
    let sample = "The change compiled and all tests passed.";
    assert_eq!(scrub_branding(sample), sample);
}

// =====================================================================
// Schema / name black-box hygiene
// =====================================================================

#[test]
fn name_and_schema_never_mention_backend_identity() {
    let backend = Arc::new(RecordingBackend::new("done"));
    let tool = PmBridgeTool::new(backend);

    assert_eq!(tool.name(), "dispatch_task");

    let schema_text = tool.schema().to_string().to_lowercase();
    for forbidden in ["trusty-mpm", "trusty-code", "tcode", "routing"] {
        assert!(
            !schema_text.contains(forbidden),
            "schema leaks '{forbidden}': {schema_text}"
        );
    }
    // "tm" alone is too common a substring to safely assert as absent from
    // free-form schema prose (it would false-positive on "system", "item",
    // etc.); the dedicated tokens above cover the actual backend names.
}

// =====================================================================
// RBAC gate
// =====================================================================

#[test]
fn dispatch_task_denies_read_only_and_analytics_tiers() {
    let backend = Arc::new(RecordingBackend::new("done"));
    let tool = Arc::new(
        PmBridgeTool::new(backend)
            .with_restricted_tiers(vec![ServiceTier::ReadOnly, ServiceTier::Analytics]),
    );
    let mut registry = ToolRegistry::new();
    registry.register(tool);

    let read_only = crate::rbac::UserIdentity::new("u1", "u1", ServiceTier::ReadOnly);
    let analytics = crate::rbac::UserIdentity::new("u2", "u2", ServiceTier::Analytics);
    let all_tier = crate::rbac::UserIdentity::new("u3", "u3", ServiceTier::All);

    assert!(
        registry
            .filter_tools_for_user(&read_only)
            .iter()
            .all(|t| t.name() != "dispatch_task"),
        "ReadOnly must not see dispatch_task"
    );
    assert!(
        registry
            .filter_tools_for_user(&analytics)
            .iter()
            .all(|t| t.name() != "dispatch_task"),
        "Analytics must not see dispatch_task"
    );
    assert!(
        registry
            .filter_tools_for_user(&all_tier)
            .iter()
            .any(|t| t.name() == "dispatch_task"),
        "All tier must still see dispatch_task"
    );
}
