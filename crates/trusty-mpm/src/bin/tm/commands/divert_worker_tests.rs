//! Tests for the headless Claude Code worker (#6887).
//!
//! Why: every assertion here is about a decision made BEFORE the child runs —
//! the argv, the stdin envelope, the scrubbed environment, and how a reply or a
//! failure is read back. None of them spends a token, so the suite is safe to
//! run on every build.
//! What: the `#[cfg(test)]` module `divert_worker.rs` includes.

use super::*;

/// Why (#6887): the file bytes must travel on stdin, not argv — argv is visible
/// to every process on the machine via `ps` and is length-bounded. A refactor
/// that "simplified" the payload into a `--append-system-prompt` would leak the
/// source of every diverted file into the process table.
/// What: asserts the model reaches argv, the headless/JSON flags are present,
/// every capability the worker does not need is switched off, and no argument
/// carries file content.
#[test]
fn worker_argv_carries_the_model_and_no_file_content() {
    let argv = worker_argv("claude-haiku-4-5");

    assert!(
        argv.contains(&"-p".to_string()),
        "must run headless: {argv:?}"
    );
    let model_at = argv
        .iter()
        .position(|a| a == "--model")
        .expect("--model must be present");
    assert_eq!(argv[model_at + 1], "claude-haiku-4-5");
    let fmt_at = argv
        .iter()
        .position(|a| a == "--output-format")
        .expect("--output-format must be present");
    assert_eq!(
        argv[fmt_at + 1],
        "json",
        "usage and cost only come back structured"
    );

    // The worker needs no tools, no MCP servers, no skills, and nobody is there
    // to answer a permission prompt.
    for flag in [
        "--restricted",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--permission-prompts",
    ] {
        assert!(argv.contains(&flag.to_string()), "{flag} missing: {argv:?}");
    }
    assert!(!argv.contains(&"--mcp-config".to_string()));

    // The one thing that must never be in argv.
    let secret = "SENTINEL_FILE_BODY_9f2c";
    let payload = wrap_files(&[("a.rs".into(), secret.into())], "what is this?");
    assert!(payload.contains(secret), "the payload carries the content");
    assert!(
        !worker_argv("claude-haiku-4-5")
            .iter()
            .any(|a| a.contains(secret)),
        "file content must never reach argv"
    );
}

/// Why (#6887): shunt's envelope is what the #6882 measurements were taken
/// against, so the wire shape carries over only if it is byte-compatible.
/// What: one `<file path="…">` element per file, then the question.
#[test]
fn wrap_files_uses_the_shunt_envelope() {
    let payload = wrap_files(
        &[
            ("src/a.rs".into(), "fn a() {}".into()),
            ("src/b.rs".into(), "fn b() {}".into()),
        ],
        "which functions exist?",
    );
    assert!(payload.contains("<file path=\"src/a.rs\">\nfn a() {}\n</file>"));
    assert!(payload.contains("<file path=\"src/b.rs\">\nfn b() {}\n</file>"));
    assert!(
        payload
            .trim_end()
            .ends_with("Question: which functions exist?")
    );
}

/// Why (#6887, the nesting risk): `claude -p` DOES start inside a running
/// Claude Code session, so the scrub is not a workaround for a refusal — it is
/// there because those variables bind the child to the parent's session id, IPC
/// socket, output cap and effort level. A child that kept them would talk on
/// the parent's messaging socket and claim the parent's session id.
/// What: asserts every parent-session binding observed in a live hook
/// subprocess is on the scrub list.
#[test]
fn scrubbed_env_drops_every_nested_session_binding() {
    for name in [
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDE_CODE_EXECPATH",
        "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
        "CLAUDE_EFFORT",
        "CLAUDE_PID",
    ] {
        assert!(
            NESTED_SESSION_ENV.contains(&name),
            "{name} binds the child to the parent session and must be scrubbed"
        );
    }
}

/// Why (owner ruling 2026-09-07): the worker runs under the developer's
/// EXISTING Claude Code login, which is exactly what `CLAUDE_CONFIG_DIR`
/// selects. Scrubbing it would send the child to a config dir with no OAuth
/// session, and every diversion would fail with `Not logged in`.
/// What: asserts the config dir is not on the scrub list.
#[test]
fn scrubbed_env_keeps_the_config_dir() {
    assert!(
        !NESTED_SESSION_ENV.contains(&"CLAUDE_CONFIG_DIR"),
        "CLAUDE_CONFIG_DIR is the login the child inherits; scrubbing it breaks auth"
    );
}

/// Why (#6887 acceptance criterion 6): the ledger line reports what the
/// diversion cost, so the numbers have to come off the child's own JSON rather
/// than be estimated.
/// What: a representative `--output-format json` object parses into the answer
/// text, the billed model, and all four token counters plus the cost.
#[test]
fn parse_worker_json_extracts_text_and_usage() {
    let raw = r#"{
        "result": "  It defines two functions.  ",
        "is_error": false,
        "total_cost_usd": 0.019422,
        "usage": {
            "input_tokens": 10,
            "output_tokens": 412,
            "cache_read_input_tokens": 9094,
            "cache_creation_input_tokens": 21922
        },
        "modelUsage": { "claude-haiku-4-5": { "costUSD": 0.019422 } }
    }"#;
    let reply = parse_worker_json(raw).expect("a well-formed reply parses");
    assert_eq!(reply.text, "It defines two functions.");
    assert_eq!(reply.model, "claude-haiku-4-5");
    assert_eq!(reply.input_tokens, 10);
    assert_eq!(reply.output_tokens, 412);
    assert_eq!(reply.cache_read_tokens, 9094);
    assert_eq!(reply.cache_creation_tokens, 21922);
    assert!((reply.cost_usd - 0.019422).abs() < 1e-9);
}

/// Why (#6887 acceptance criterion 4): a logged-out child exits ZERO and
/// reports the failure inside the JSON (`is_error: true`, `result: "Not logged
/// in · Please run /login"`), verified live on this workstation with `--bare`.
/// A parser that trusted the exit code would print that sentence as the file
/// summary — a silent wrong answer, which is worse than a fall-through.
/// What: `is_error: true` and an empty `result` both become `Err`.
#[test]
fn parse_worker_json_rejects_an_error_result() {
    let logged_out = r#"{"is_error": true, "result": "Not logged in · Please run /login",
                          "total_cost_usd": 0, "usage": {}}"#;
    let err = parse_worker_json(logged_out).expect_err("an error result must not answer");
    assert!(
        err.contains("Not logged in"),
        "the reason must survive: {err}"
    );

    let empty = r#"{"is_error": false, "result": "", "usage": {}}"#;
    assert!(
        parse_worker_json(empty).is_err(),
        "an empty answer is a failure, not a summary"
    );

    assert!(parse_worker_json("not json at all").is_err());
}

/// Why (#6887 acceptance criterion 4): "the `claude` binary is missing" is one
/// of the three named worker failures, and it must come back as the same `Err`
/// shape as the other two so the caller has one recovery path.
/// What: a program path that cannot be executed reports the failure by name
/// instead of panicking. The path is injected rather than arranged by emptying
/// `PATH`, because this bin target forbids process-global env mutation (#5544)
/// and a `PATH` rewrite corrupts every other test in the binary.
#[tokio::test]
async fn spawn_worker_at_reports_an_unstartable_binary() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-such-claude");

    let err = spawn_worker_at(&missing, "m", "payload", std::time::Duration::from_secs(5))
        .await
        .expect_err("an unstartable binary must not answer");
    assert!(
        err.contains(WORKER_BINARY),
        "the error must name the worker: {err}"
    );
}

/// Why (#6887 acceptance criterion 4): a hung worker is the third named
/// failure. Without the timeout the agent waits forever on a command it cannot
/// cancel.
/// What: a stand-in that never exits is killed and reported as a timeout — not
/// as a generic spawn failure, which would send the operator looking for the
/// wrong cause.
#[cfg(unix)]
#[tokio::test]
async fn spawn_worker_at_times_out() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("slow-claude");
    // An absolute `sleep`: the child inherits this process's PATH, which the
    // test must not touch.
    std::fs::write(&fake, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = spawn_worker_at(&fake, "m", "payload", std::time::Duration::from_millis(300))
        .await
        .expect_err("a hung worker must not answer");
    assert!(
        err.contains("timed out"),
        "a hang must report as a timeout, not a generic failure: {err}"
    );
}

/// Why (#6887 acceptance criterion 4): the hook's fail-open branch turns on
/// this predicate, so it must actually reflect whether a binary is resolvable
/// rather than being a constant `true`.
/// What: agrees with `resolve_binary` for the worker binary.
#[test]
fn worker_available_reports_the_resolver() {
    assert_eq!(
        worker_available(),
        trusty_common::bin_resolve::resolve_binary(WORKER_BINARY).is_some()
    );
}
