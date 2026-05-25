//! Handler for `trusty-memory prompt-context`.
//!
//! Why: Claude Code's `UserPromptSubmit` hooks inject any stdout produced by
//! the hook command as additional context for the model. The trusty-memory
//! setup command installs a hook that runs `trusty-memory prompt-context`
//! before every prompt, so the model gets a freshly-rendered list of the
//! palace's hot-predicate facts without paying the per-message MCP tool-call
//! tax. This handler is the actual command the hook invokes.
//!
//! What: a side-effect-only command that talks to the running trusty-memory
//! HTTP daemon and prints the formatted prompt-context block to stdout. It is
//! designed to fail soft on every error path — missing daemon address file,
//! unreachable daemon, slow response — because failing the hook would block
//! every Claude Code prompt the user types.
//!
//! Note on MPM sub-agents: unlike `trusty-mpm hook`, this command is
//! **intentionally NOT** gated on the `CLAUDE_MPM_SUB_AGENT` environment
//! variable. Sub-agents benefit from the parent palace's prompt-fact block
//! just as much as the PM does — withholding it would force every nested
//! agent to rediscover project conventions from scratch. The token cost is
//! a single rendered fact list and the signal payoff (consistent style,
//! vocabulary, and architectural facts across the agent tree) is high. The
//! suppression of nested hook traffic happens at the `trusty-mpm hook`
//! layer instead, where doubled audit events are the real failure mode.
//!
//! Test: daemon-touching paths are exercised manually via `trusty-memory
//! prompt-context` after `trusty-memory start`. The soft-fail-on-missing-
//! daemon branch is covered by `prompt_context_returns_ok_without_daemon`.

use anyhow::Result;
use std::time::{Duration, Instant};

use crate::prompt_log::{PromptLogEntry, PromptLogger};

/// HTTP path served by the trusty-memory daemon that returns the formatted
/// prompt-context block as `text/plain`.
const PROMPT_CONTEXT_PATH: &str = "/api/v1/kg/prompt-context";

/// Connect + total request timeout. Kept short so a slow/dead daemon can
/// never block a Claude Code prompt for more than a couple seconds.
const HTTP_TIMEOUT: Duration = Duration::from_millis(2500);

/// Entry point for `trusty-memory prompt-context`.
///
/// Why: every error path in this handler must result in a clean exit 0 — the
/// `UserPromptSubmit` hook is wired into every Claude Code prompt the user
/// types, so any non-zero exit (or panic) would either block the prompt or
/// inject a confusing error into the model's context. Logging to stderr is
/// fine because Claude Code only ingests stdout from hook commands.
/// What:
///   1. Look up the daemon's bound address via the shared
///      `trusty_common::read_daemon_addr` helper; return `Ok(())` when the
///      file is missing (daemon not running).
///   2. `GET <addr>/api/v1/kg/prompt-context` with a tight timeout; print the
///      body verbatim to stdout on a 2xx, otherwise exit 0.
///
/// Sub-agent behaviour: deliberately unguarded. MPM-spawned sub-agents inject
/// the same prompt-context block as the PM because the marginal token cost
/// is small and the convention/style signal is high — see the module-level
/// note for the full rationale.
/// Test: `prompt_context_returns_ok_without_daemon` covers the no-daemon
/// branch; live-daemon paths are exercised manually after
/// `trusty-memory start`.
pub async fn handle_prompt_context() -> Result<()> {
    let start = Instant::now();
    // Best-effort capture of the user prompt the hook received on stdin.
    // UserPromptSubmit hooks deliver the prompt as the entire stdin payload.
    // Reading is short-circuited at 64 KiB to bound logging cost.
    let trigger_prompt = read_stdin_best_effort();

    // 1. Discover the running daemon. Missing file → daemon not running →
    //    silently exit. A real I/O error here (rare) also degrades to silence.
    let addr = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(addr)) => addr,
        Ok(None) => {
            log_entry(&trigger_prompt, "", 0, start);
            return Ok(());
        }
        Err(_) => {
            log_entry(&trigger_prompt, "", 0, start);
            return Ok(());
        }
    };

    // The shared helper persists the bare `host:port`. The web daemon binds
    // HTTP, so prepend the scheme when callers haven't already.
    let base = if addr.starts_with("http://") || addr.starts_with("https://") {
        addr
    } else {
        format!("http://{addr}")
    };
    let url = format!("{base}{PROMPT_CONTEXT_PATH}");

    // 2. Tightly-bounded HTTP call. Any failure → exit 0 silently so the
    //    Claude Code prompt is never blocked by a degraded daemon.
    let client = match reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            log_entry(&trigger_prompt, "", 0, start);
            return Ok(());
        }
    };

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(_) => {
            log_entry(&trigger_prompt, "", 0, start);
            return Ok(());
        }
    };
    if !resp.status().is_success() {
        log_entry(&trigger_prompt, "", 0, start);
        return Ok(());
    }
    let body = match resp.text().await {
        Ok(b) => b,
        Err(_) => {
            log_entry(&trigger_prompt, "", 0, start);
            return Ok(());
        }
    };
    // Print bare body — Claude Code injects stdout verbatim. No trailing
    // newline manipulation; the daemon's response already carries one.
    print!("{body}");

    // Best-effort log entry — `count_facts` approximates the number of
    // bulleted facts in the rendered Markdown block. Errors are swallowed
    // inside the logger.
    let facts_count = count_facts(&body);
    log_entry(&trigger_prompt, &body, facts_count, start);

    Ok(())
}

/// Read the hook's stdin into a string, capped at 64 KiB.
///
/// Why (issue #105): the UserPromptSubmit hook delivers the user prompt as
/// stdin so we capture it for the enriched-prompt log. Stdin may be empty
/// (e.g. when the daemon is probed manually). The cap defends against an
/// adversarial prompt the size of a novel from inflating the log file.
/// What: synchronously reads stdin to EOF (or 64 KiB), returns the trimmed
/// payload. Failures degrade to an empty string — the hook continues either
/// way.
/// Test: not unit-tested (process stdin is hard to mock); covered by the
/// integration test which writes the entry directly.
fn read_stdin_best_effort() -> String {
    use std::io::Read;
    const STDIN_CAP_BYTES: usize = 64 * 1024;
    // `is_terminal()` lets us bail when stdin is the controlling TTY — there
    // is no prompt to read in that case and `read_to_string` would block.
    let stdin = std::io::stdin();
    if std::io::IsTerminal::is_terminal(&stdin) {
        return String::new();
    }
    let mut buf = String::new();
    let _ = stdin
        .lock()
        .take(STDIN_CAP_BYTES as u64)
        .read_to_string(&mut buf);
    buf
}

/// Approximate the number of facts in the rendered prompt-context body.
///
/// Why: the daemon's response is plain Markdown; counting bullet lines
/// (`- ` prefix) gives a quick proxy for "how many facts were injected" that
/// is useful for log analysis without an additional round trip.
/// What: counts non-empty lines whose first non-whitespace characters are
/// `- `. Returns 0 for an empty / placeholder body.
/// Test: covered indirectly by `single_event_roundtrip` in the integration
/// tests; the heuristic is intentionally cheap and approximate.
fn count_facts(body: &str) -> usize {
    body.lines()
        .filter(|l| l.trim_start().starts_with("- "))
        .count()
}

/// Resolve the palace identifier for the log entry.
///
/// Why (issue #125): Claude Code's `UserPromptSubmit` hook payload is a JSON
/// object carrying (among other fields) `"cwd": "<absolute-path>"` that
/// reflects the user's *actual* working directory at the moment the prompt
/// was submitted. The hook process inherits its own cwd from wherever it
/// was registered (often the parent project root), so deriving the palace
/// slug from the process cwd would tag every log entry with the wrong
/// project. We prefer the stdin-provided `cwd` and fall back to the process
/// cwd only when stdin doesn't carry one (e.g. manual `trusty-memory
/// prompt-context` invocations from a TTY).
/// What: best-effort `serde_json` parse of `stdin_payload`; on success and
/// when the JSON has a string `cwd` field, derive the slug via
/// [`crate::messaging::cwd_palace_slug_at`] from THAT path. Any failure
/// (non-JSON stdin, missing field, slug-derivation error) falls through to
/// the existing `cwd_palace_slug()` (process cwd) path and finally to the
/// `"<unknown>"` sentinel.
/// Test: `resolve_palace_for_log_prefers_stdin_cwd`.
fn resolve_palace_for_log(stdin_payload: &str) -> String {
    if let Some(slug) = palace_slug_from_stdin_cwd(stdin_payload) {
        return slug;
    }
    crate::messaging::cwd_palace_slug().unwrap_or_else(|_| "<unknown>".to_string())
}

/// Parse `stdin_payload` as JSON and, when it carries a `cwd` string, derive
/// the palace slug from that path.
///
/// Why: factored out so the unit test can exercise the stdin-override path
/// without manipulating the process cwd.
/// What: returns `Some(slug)` only when the payload parses as a JSON object,
/// contains a non-empty string `cwd`, and slug derivation succeeds for that
/// path. Returns `None` on every failure mode so the caller can fall back.
/// Test: `resolve_palace_for_log_prefers_stdin_cwd`.
fn palace_slug_from_stdin_cwd(stdin_payload: &str) -> Option<String> {
    if stdin_payload.trim().is_empty() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(stdin_payload).ok()?;
    let cwd = value.get("cwd")?.as_str()?;
    if cwd.is_empty() {
        return None;
    }
    crate::messaging::cwd_palace_slug_at(std::path::Path::new(cwd)).ok()
}

/// Append one log entry to the enriched-prompt log, swallowing failures.
fn log_entry(trigger_prompt: &str, injection: &str, facts_count: usize, start: Instant) {
    let logger = PromptLogger::from_env();
    let palace = resolve_palace_for_log(trigger_prompt);
    let entry = PromptLogEntry::new(
        "UserPromptSubmit",
        "prompt-context-facts",
        palace,
        trigger_prompt,
        injection,
    )
    .with_palace_facts_count(facts_count)
    .with_duration_ms(start.elapsed().as_millis() as u64);
    logger.log(entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (issue #125): when Claude Code invokes the UserPromptSubmit hook,
    /// the stdin JSON carries a `cwd` field that reflects the user's actual
    /// working directory at prompt time. The hook process cwd may be where
    /// the hook was registered (typically a fixed install root), not where
    /// the user actually is. The log palace must follow the stdin `cwd`.
    /// What: build a stdin JSON payload pointing at a tempdir, derive the
    /// expected slug for that tempdir via the *_at variant, and assert
    /// `resolve_palace_for_log` returns the same slug — even though the
    /// process cwd is unchanged and would resolve to a different slug.
    /// Test: itself.
    #[test]
    fn resolve_palace_for_log_prefers_stdin_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Pick a basename that won't slugify to the same thing as the
        // process cwd's basename (the worktree dir is `fix-124-rebase-damage`).
        let project = tmp.path().join("stdin-driven-project");
        std::fs::create_dir_all(&project).expect("create project dir");
        let payload = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "cwd": project.to_string_lossy(),
            "prompt": "hello"
        })
        .to_string();

        let expected =
            crate::messaging::cwd_palace_slug_at(&project).expect("derive slug from stdin cwd");
        let got = resolve_palace_for_log(&payload);
        assert_eq!(
            got, expected,
            "stdin `cwd` must override the process cwd for the log palace slug"
        );
        // Sanity: the slug should reflect the stdin-provided path's basename,
        // not the process cwd's basename. (The process cwd here is the
        // worktree, which slugifies to `fix-124-rebase-damage` or similar.)
        assert!(
            got.contains("stdin-driven-project"),
            "expected slug derived from stdin path, got {got:?}"
        );
    }

    /// Why (issue #125): when stdin is empty or non-JSON, the helper must
    /// fall through to the process-cwd resolution path so manual `trusty-
    /// memory prompt-context` invocations from a TTY still get a useful
    /// palace identifier.
    /// What: pass an empty string and a non-JSON string; assert the result
    /// is *not* the legacy `"<unknown>"` sentinel (the process cwd here is a
    /// real git repo, so cwd_palace_slug succeeds).
    /// Test: itself.
    #[test]
    fn resolve_palace_for_log_falls_back_to_process_cwd() {
        let from_empty = resolve_palace_for_log("");
        let from_garbage = resolve_palace_for_log("not json at all");
        // Process cwd is a real git worktree, so both should resolve to the
        // same non-sentinel slug.
        assert_eq!(from_empty, from_garbage);
        assert_ne!(from_empty, "<unknown>");
    }

    /// Why: the hook is wired into every Claude Code prompt the user types;
    /// failing it would block the prompt. The contract is that a missing
    /// daemon-address lockfile (the canonical "daemon not running" signal)
    /// must produce `Ok(())` with no stdout, not an error.
    /// What: redirects `trusty_common::resolve_data_dir` at a fresh tempdir
    /// via `TRUSTY_DATA_DIR_OVERRIDE` so `read_daemon_addr("trusty-memory")`
    /// observes a missing lockfile, then runs the handler and asserts it
    /// returns `Ok(())`.
    #[tokio::test]
    async fn prompt_context_returns_ok_without_daemon() {
        // Lock the process-wide env mutex so this test can't race with any
        // sibling test that also mutates `TRUSTY_DATA_DIR_OVERRIDE`.
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: tests serialise on `TRUSTY_DATA_DIR_OVERRIDE` by convention
        // across the trusty-* workspace (see trusty-common's lib.rs notes).
        // This test only mutates the env var inside its own scope.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        }
        let res = handle_prompt_context().await;
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            res.is_ok(),
            "missing daemon lockfile must degrade to Ok(()), got {res:?}"
        );
    }

    /// Why (issue #105): even when the daemon is down, the hook must still
    /// log an attempt entry so operators can see "prompt-context fired N
    /// times but the daemon was unreachable" in the JSONL stream.
    /// What: pin a tempdir as the data directory, run the handler with no
    /// daemon, and assert exactly one log file landed under `<tmp>/logs/`
    /// with a single JSONL line whose `injection_kind` is the prompt-context
    /// kind.
    /// Test: itself.
    #[tokio::test]
    async fn prompt_context_logs_attempt_without_daemon() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let tmp = tempfile::tempdir().expect("tempdir");
        // SAFETY: same convention as the sibling test.
        unsafe {
            std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
            // Defensive: clear any user-supplied env that would disable the
            // logger or redirect it elsewhere.
            std::env::remove_var(crate::prompt_log::ENV_ENABLED);
            std::env::remove_var(crate::prompt_log::ENV_DIR);
            std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
        }
        let res = handle_prompt_context().await;
        // Snapshot the data dir before clearing the env var.
        let logs_dir = trusty_common::resolve_data_dir("trusty-memory")
            .expect("resolve data dir")
            .join("logs");
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(res.is_ok());
        // Filter by FILE_PREFIX so coexisting `stdout.log`/`stderr.log` files
        // (created by other code paths under the same data root) don't trip
        // the count assertion.
        let files: Vec<_> = std::fs::read_dir(&logs_dir)
            .expect("logs dir should be created")
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("enriched-prompts."))
            })
            .collect();
        assert_eq!(
            files.len(),
            1,
            "expected one enriched-prompts log file, got {files:?}"
        );
        let content = std::fs::read_to_string(&files[0]).expect("read log");
        let line = content.lines().next().expect("at least one line");
        let parsed: crate::prompt_log::PromptLogEntry =
            serde_json::from_str(line).expect("parse JSONL");
        assert_eq!(parsed.hook_type, "UserPromptSubmit");
        assert_eq!(parsed.injection_kind, "prompt-context-facts");
    }
}
