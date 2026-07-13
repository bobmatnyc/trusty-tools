//! End-to-end tests for the universal `config keys` CLI (issue #2404).
//!
//! Why: the acceptance criteria make the whole `config keys` surface a merge
//! gate — it must be 100% exercisable non-interactively. This suite proves the
//! argv grammar parses (the canonical forms + the deliberately-absent `get`),
//! the mount-once/extensibility contract holds, and the full set→list→test→unset
//! flow works through the injectable `inference::config::ops` seam against a
//! `MemoryKeyStore` and a mock provider — asserting throughout that NO key value
//! ever appears in any output.
//! What: black-box tests against `trusty_common::inference::config::*`, with the
//! live `test` probe pointed at the axum `MockInferenceServer`. Requires
//! `config-cli` (the command + ops) + `axum-server` (the mock).
//! Test: this file — run with
//! `cargo test -p trusty-common --features config-cli,axum-server`.

use std::io::Cursor;

use clap::Parser;
use serial_test::serial;

use trusty_common::inference::config::ops::{KeyTier, ProbeOutcome};
use trusty_common::inference::config::{ConfigCommand, ops};
use trusty_common::inference::credentials::{KeyStore, MemoryKeyStore};
use trusty_common::inference::providers::openrouter;
use trusty_common::inference::test_support::MockInferenceServer;
use trusty_common::inference::{Configurator, ProviderId, ResolvedProvider};

/// A fake secret used across the flow tests; asserted NEVER to appear in output.
const FAKE_KEY: &str = "sk-or-supersecretvalue-must-never-print-9999"; // pragma: allowlist secret

/// Minimal clap harness that mounts `ConfigCommand` exactly as a real binary
/// would — this both drives the argv-grammar tests AND demonstrates the two-line
/// mount recipe / zero-mount-churn extensibility contract.
#[derive(Parser)]
#[command(name = "harness")]
struct Harness {
    #[command(subcommand)]
    command: HarnessCmd,
}

#[derive(clap::Subcommand)]
enum HarnessCmd {
    /// The single mount point — line 1 of the #2405 recipe.
    Config(ConfigCommand),
}

/// Clear every provider env var so the injected `MemoryKeyStore` is the only
/// credential source in a test (a stray env var would shadow it / skew tiers).
fn clear_provider_env() {
    for var in [
        "OPENROUTER_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "FIREWORKS_API_KEY",
        "TOGETHER_API_KEY",
    ] {
        // SAFETY: every caller is `#[serial(dotenv_credential_env)]`, matching
        // the lock the credential resolver's own env-mutating tests use.
        unsafe { std::env::remove_var(var) };
    }
}

/// Decode a captured output buffer to a string for assertions.
fn out_string(buf: &[u8]) -> String {
    String::from_utf8(buf.to_vec()).expect("output is valid UTF-8")
}

// ── argv grammar ─────────────────────────────────────────────────────────────

/// Why: the canonical `config keys …` argv forms must parse, and the mount is a
/// single `Config(ConfigCommand)` variant (the extensibility contract: nothing
/// at the mount site knows about verbs).
/// Test: itself.
#[test]
fn config_keys_argv_grammar_parses() {
    let canonical: &[&[&str]] = &[
        &["harness", "config", "keys", "set", "openrouter"],
        &["harness", "config", "keys", "set", "openrouter", "sk-value"], // pragma: allowlist secret
        &["harness", "config", "keys", "list"],
        &["harness", "config", "keys", "test", "anthropic"],
        &["harness", "config", "keys", "unset", "fireworks"],
        &["harness", "config", "keys", "test", "together"],
    ];
    for argv in canonical {
        assert!(
            Harness::try_parse_from(*argv).is_ok(),
            "canonical grammar should parse: {argv:?}"
        );
    }
}

/// Why: there is deliberately NO `config keys get` — a value-reading verb would
/// violate the never-echo mandate. It must fail to parse.
/// Test: itself.
#[test]
fn config_keys_rejects_get() {
    assert!(
        Harness::try_parse_from(["harness", "config", "keys", "get", "openrouter"]).is_err(),
        "`config keys get` must not exist"
    );
}

// ── read_key_line: the scriptable stdin-pipe path ────────────────────────────

/// Why: `set` with no VALUE arg on a pipe reads the key from stdin; the value
/// must come back with its trailing newline stripped and nothing echoed.
/// Test: itself.
#[test]
fn read_key_line_trims_piped_value() {
    let mut reader = Cursor::new(format!("{FAKE_KEY}\n").into_bytes());
    assert_eq!(ops::read_key_line(&mut reader).unwrap(), FAKE_KEY);
}

// ── set → list → unset flow (no network) ─────────────────────────────────────

/// Why: the core credential lifecycle must work through the injectable ops and
/// never reveal the value: `set` stores it (confirmation redacted), `list`
/// reports the store tier by name only, `unset` removes it, and a second `unset`
/// reports absence. The fake key must appear in NONE of the captured output.
/// Test: itself.
#[test]
#[serial(dotenv_credential_env)]
fn set_then_list_reports_store_tier_without_value() {
    clear_provider_env();
    let store = MemoryKeyStore::new();

    // set
    let mut set_out = Vec::new();
    ops::set(&store, "openrouter", FAKE_KEY, &mut set_out).expect("set ok");
    let set_str = out_string(&set_out);
    assert!(set_str.contains("openrouter"), "{set_str}");
    assert!(!set_str.contains(FAKE_KEY), "set leaked the key: {set_str}");
    // The value really landed in the store.
    assert_eq!(store.get("openrouter").as_deref(), Some(FAKE_KEY));

    // list — reports the store tier, no value.
    let mut list_out = Vec::new();
    ops::list(&store, &mut list_out).expect("list ok");
    let list_str = out_string(&list_out);
    assert!(
        list_str.contains("openrouter") && list_str.contains(KeyTier::Store.label()),
        "list should show openrouter via the secure store: {list_str}"
    );
    // A provider with no key is reported as not configured.
    assert!(list_str.contains("not configured"), "{list_str}");
    assert!(
        !list_str.contains(FAKE_KEY),
        "list leaked the key: {list_str}"
    );

    // unset — removes it and reports removal.
    let mut unset_out = Vec::new();
    ops::unset(&store, "openrouter", &mut unset_out).expect("unset ok");
    assert!(store.get("openrouter").is_none(), "key should be gone");
    let unset_str = out_string(&unset_out);
    assert!(unset_str.contains("Removed"), "{unset_str}");
    assert!(!unset_str.contains(FAKE_KEY));

    // second unset — reports absence, not an error.
    let mut again = Vec::new();
    ops::unset(&store, "openrouter", &mut again).expect("idempotent unset");
    assert!(out_string(&again).contains("nothing to remove"));
}

/// Why: `list` must report the ENV tier (highest precedence) when a provider's
/// env var is set, distinct from the store tier — the tier-aware requirement.
/// Test: itself.
#[test]
#[serial(dotenv_credential_env)]
fn list_reports_env_tier() {
    clear_provider_env();
    let store = MemoryKeyStore::new();
    // Store also has a key, but the env var must win the tier report.
    store.set("openai", FAKE_KEY).unwrap();
    // SAFETY: guarded by `#[serial(dotenv_credential_env)]`.
    unsafe { std::env::set_var("OPENAI_API_KEY", "env-value") };

    let mut out = Vec::new();
    ops::list(&store, &mut out).expect("list ok");
    let s = out_string(&out);
    assert!(
        s.contains("openai") && s.contains(KeyTier::Env.label()),
        "openai should report the env tier: {s}"
    );
    assert!(
        !s.contains(FAKE_KEY) && !s.contains("env-value"),
        "list leaked a value: {s}"
    );

    unsafe { std::env::remove_var("OPENAI_API_KEY") };
}

/// Why: some mounting binaries (trusty-search `main.rs`, trusty-agents
/// `runtime/startup.rs`) already `dotenvy::from_filename(".env.local")` at
/// startup, folding the file's values into the process env BEFORE `config`
/// ever runs. When a provider's key is present in BOTH the process env AND the
/// `.env.local` file, `list` cannot honestly tell "independently exported in
/// the shell" apart from "the binary's own startup load" — it must report the
/// ambiguous tier rather than confidently (and possibly wrongly) claiming
/// "environment variable".
/// Test: itself.
#[test]
#[serial(dotenv_credential_env)]
fn list_reports_ambiguous_tier_when_env_and_env_local_both_present() {
    clear_provider_env();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path().join(".env.local"),
        "ANTHROPIC_API_KEY=file-value\n",
    )
    .expect("write .env.local");

    let prior_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(tmp.path()).expect("chdir into tempdir");
    // SAFETY: guarded by `#[serial(dotenv_credential_env)]`; the same guard
    // also protects this test's exclusive use of the process cwd within this
    // test binary (every other test in this file is annotated the same way).
    unsafe { std::env::set_var("ANTHROPIC_API_KEY", "env-value") };

    let store = MemoryKeyStore::new();
    let mut out = Vec::new();
    let list_result = ops::list(&store, &mut out);

    // Restore process-global state before any assertion/panic can leak it into
    // other tests.
    unsafe { std::env::remove_var("ANTHROPIC_API_KEY") };
    std::env::set_current_dir(&prior_cwd).expect("restore cwd");

    list_result.expect("list ok");
    let s = out_string(&out);
    assert!(
        s.contains("anthropic") && s.contains(KeyTier::EnvOrEnvLocal.label()),
        "anthropic should report the ambiguous env/.env.local tier: {s}"
    );
    assert!(
        !s.contains("env-value") && !s.contains("file-value"),
        "list leaked a value: {s}"
    );
}

/// Why: `set`/`unset` must reject a provider that does not use an API key
/// (Bedrock — AWS chain) and an unknown provider, with an actionable message.
/// Test: itself.
#[test]
#[serial(dotenv_credential_env)]
fn set_rejects_unknown_and_keyless_providers() {
    clear_provider_env();
    let store = MemoryKeyStore::new();
    let mut out = Vec::new();
    assert!(ops::set(&store, "bedrock", "x", &mut out).is_err());
    assert!(ops::set(&store, "cohere", "x", &mut out).is_err());
    // Nothing should have been written for the rejected providers.
    assert!(store.list().is_empty());
}

// ── test (live probe) against the mock server ────────────────────────────────

/// Build a `Configurator` whose OpenRouter factory targets the mock server URL
/// instead of the real API — the same injection the adapter e2e suite uses.
fn mock_configurator(base_url: String) -> Configurator {
    let mut cfg = Configurator::new();
    cfg.register(
        ProviderId::OpenRouter,
        Box::new(move |r: &ResolvedProvider| openrouter::build(r, &base_url)),
    );
    cfg
}

/// Why: `config keys test` with a valid key must probe the provider and report
/// OK, without the key appearing in any output — the happy path of the
/// api-testable-locally check.
/// Test: itself.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn test_probe_ok_against_mock() {
    clear_provider_env();
    let body = serde_json::json!({
        "id": "gen-mock",
        "choices": [{"message": {"role": "assistant", "content": "pong"},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    });
    let server = MockInferenceServer::spawn(200, body).await.expect("spawn");
    let cfg = mock_configurator(server.url().to_string());

    let store = MemoryKeyStore::new();
    store.set("openrouter", FAKE_KEY).unwrap();

    let outcome = ops::probe(&store, &cfg, "openrouter").await.expect("probe");
    assert_eq!(outcome, ProbeOutcome::Ok);

    let mut out = Vec::new();
    ops::report_probe("openrouter", &outcome, &mut out).expect("report");
    let s = out_string(&out);
    assert!(s.contains("OK"), "{s}");
    assert!(!s.contains(FAKE_KEY), "probe report leaked the key: {s}");
}

/// Why: a 401/403 from the provider must classify as UNAUTHORIZED (not OK, not a
/// generic error), still without leaking the key.
/// Test: itself.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn test_probe_401_is_unauthorized() {
    clear_provider_env();
    let server = MockInferenceServer::spawn(401, serde_json::json!({"error": "invalid key"}))
        .await
        .expect("spawn");
    let cfg = mock_configurator(server.url().to_string());

    let store = MemoryKeyStore::new();
    store.set("openrouter", FAKE_KEY).unwrap();

    let outcome = ops::probe(&store, &cfg, "openrouter").await.expect("probe");
    assert_eq!(outcome, ProbeOutcome::Unauthorized);
    assert!(
        outcome.clone().into_result().is_err(),
        "401 must be a failure exit"
    );

    let mut out = Vec::new();
    ops::report_probe("openrouter", &outcome, &mut out).expect("report");
    assert!(!out_string(&out).contains(FAKE_KEY));
}

/// Why: `InferenceError::Api`'s `Display` embeds the raw, provider-controlled
/// response BODY verbatim. A provider that echoes the offending credential back
/// in a non-401/403 error body (landing on the `ProbeOutcome::Failed` catch-all)
/// must NOT have that value reach `report_probe`'s output — the one place in
/// this credential-management CLI that must never leak a key. This drives the
/// full probe→report pipeline with a mock body that CONTAINS the fake key and
/// asserts it never surfaces.
/// Test: itself.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn probe_error_body_never_leaks_the_resolved_key() {
    clear_provider_env();
    // 400 (not 401/403) so this lands on the `Failed` catch-all, not
    // `Unauthorized` — and the body echoes the key back, as a misbehaving or
    // overly-verbose provider error page might.
    let server = MockInferenceServer::spawn(
        400,
        serde_json::json!({"error": {"message": format!("invalid credential: {FAKE_KEY}")}}),
    )
    .await
    .expect("spawn");
    let cfg = mock_configurator(server.url().to_string());

    let store = MemoryKeyStore::new();
    store.set("openrouter", FAKE_KEY).unwrap();

    let outcome = ops::probe(&store, &cfg, "openrouter").await.expect("probe");
    let ProbeOutcome::Failed(reason) = &outcome else {
        panic!("expected a Failed outcome for a 400 response, got {outcome:?}");
    };
    assert!(
        !reason.contains(FAKE_KEY),
        "ProbeOutcome::Failed leaked the key: {reason}"
    );

    let mut out = Vec::new();
    ops::report_probe("openrouter", &outcome, &mut out).expect("report");
    let s = out_string(&out);
    assert!(!s.contains(FAKE_KEY), "report_probe leaked the key: {s}");
    assert!(s.contains("ERROR"), "{s}");
}

/// Why: probing a provider with no key configured must degrade cleanly to
/// UNCONFIGURED (a clear message, no panic, no network call).
/// Test: itself.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn test_probe_unconfigured_when_no_key() {
    clear_provider_env();
    // No mock even needed — resolution short-circuits before any network call.
    let cfg = mock_configurator("http://127.0.0.1:1".to_string());
    let store = MemoryKeyStore::new();

    let outcome = ops::probe(&store, &cfg, "openrouter").await.expect("probe");
    assert_eq!(outcome, ProbeOutcome::Unconfigured);
    // Clean degrade: exit success, not an error.
    assert!(outcome.into_result().is_ok());
}

/// Why: Bedrock has no API key (AWS credential chain) so `test` must report it as
/// unsupported rather than attempting a key probe.
/// Test: itself.
#[tokio::test]
#[serial(dotenv_credential_env)]
async fn test_probe_bedrock_is_unsupported() {
    clear_provider_env();
    let cfg = mock_configurator("http://127.0.0.1:1".to_string());
    let store = MemoryKeyStore::new();

    let outcome = ops::probe(&store, &cfg, "bedrock").await.expect("probe");
    assert!(
        matches!(outcome, ProbeOutcome::Unsupported(_)),
        "{outcome:?}"
    );
}
