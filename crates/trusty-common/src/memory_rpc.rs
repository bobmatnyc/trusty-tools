//! Discovery-based JSON-RPC access to the trusty-memory daemon (issue #2030).
//!
//! Why: several call sites across trusty-mpm and trusty-common used to
//! hand-roll a `reqwest` GET against a hardcoded fixed port and an ad-hoc
//! REST path (`/api/v1/palaces/{id}/drawers`). That fixed port was simply
//! wrong — trusty-memory auto-port-walks from ~7070-7079 — so those call
//! sites silently reported "could not reach trusty-memory" even when the
//! daemon was healthy, unless the operator manually exported
//! `TRUSTY_MEMORY_URL`. This module is the one shared helper: resolve the
//! daemon's *actual* bound address via discovery, then call its tools over
//! the same `POST /rpc` JSON-RPC dispatcher the MCP stdio server and UDS
//! transport share — never a REST path that doesn't exist in the
//! multi-transport daemon, and never the REST-based `monitor::memory_client`
//! (gated behind `monitor-tui`, which trusty-mpm does not enable) or the
//! subprocess-oriented `stdio_mcp_client` / `daemon_bridge` (no generic call
//! surface).
//! What: [`resolve_memory_base_url`] resolves the base URL (env override
//! first, discovery fallback, `Err` when neither is available);
//! [`resolve_memory_base_url_or_unreachable`] is the fail-open convenience
//! wrapper callers that must never error use. [`call_memory_tool`] resolves
//! the address and POSTs a JSON-RPC request for `method`/`params`, returning
//! the envelope's `result`. [`call_memory_tool_at`] is the same call against
//! an explicit, already-resolved base URL (used by callers — e.g. catch-up's
//! `CatchupOptions::memory_url` — that thread their own resolved/overridable
//! address through, including tests that point at a controlled unreachable
//! port).
//! Test: `resolve_memory_base_url_prefers_env_override`,
//! `resolve_memory_base_url_errs_when_undiscovered`,
//! `resolve_memory_base_url_or_unreachable_falls_back`,
//! `call_memory_tool_at_rejects_rpc_error`,
//! `call_memory_tool_at_unreachable_daemon` (ignored — exercises the fail-open
//! contract against a dead socket without needing a live daemon).

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

/// Environment variable that lets an operator pin trusty-memory's base URL
/// explicitly, bypassing discovery entirely.
///
/// Why: CI/dev environments may run trusty-memory on a non-standard host or
/// port, or point at a remote daemon; an explicit override must always win
/// over discovery.
/// What: the literal env var name `TRUSTY_MEMORY_URL`.
/// Test: `resolve_memory_base_url_prefers_env_override`.
pub const TRUSTY_MEMORY_URL_ENV: &str = "TRUSTY_MEMORY_URL";

/// The app name trusty-memory registers its discovery file under (matches
/// the daemon's own `resolve_data_dir("trusty-memory")` call).
const MEMORY_APP_NAME: &str = "trusty-memory";

/// A reserved, never-listening loopback address used as a last-resort
/// fallback so a fail-open caller's own HTTP call fails fast instead of
/// hanging.
///
/// Why: port 1 is a reserved/unassigned port that nothing listens on, so a
/// connection attempt fails immediately rather than timing out.
/// What: `http://127.0.0.1:1`.
/// Test: `resolve_memory_base_url_or_unreachable_falls_back`.
const UNREACHABLE_PLACEHOLDER: &str = "http://127.0.0.1:1";

/// Resolve the trusty-memory daemon's base URL, discovery-first.
///
/// Why: every caller that needs to reach trusty-memory (catch-up, identity
/// seeding, the TUI health poller) must resolve the *actual* bound address —
/// trusty-memory auto-port-walks — rather than guessing a fixed port.
/// Centralising this means a future port-walk range change never requires
/// touching call sites again.
/// What: returns `Ok(url)` from [`TRUSTY_MEMORY_URL_ENV`] when set to a
/// non-empty value (an explicit override always wins over discovery);
/// otherwise reads `crate::daemon_addr::read_daemon_addr("trusty-memory")`
/// and formats `http://{addr}`. Returns `Err` when neither source yields an
/// address (the daemon has never started) or the discovery read fails.
/// Test: `resolve_memory_base_url_prefers_env_override`,
/// `resolve_memory_base_url_errs_when_undiscovered`.
pub fn resolve_memory_base_url() -> Result<String> {
    if let Ok(url) = std::env::var(TRUSTY_MEMORY_URL_ENV) {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    match crate::daemon_addr::read_daemon_addr(MEMORY_APP_NAME)? {
        Some(addr) => Ok(format!("http://{addr}")),
        None => Err(anyhow!(
            "trusty-memory daemon address not discovered (is the daemon running?)"
        )),
    }
}

/// Fail-open variant of [`resolve_memory_base_url`] for callers that must
/// never propagate an error.
///
/// Why: catch-up, identity-seeding, and the TUI health poller are all
/// designed to degrade gracefully when trusty-memory is unreachable rather
/// than aborting their caller. Centralising the "give me *a* URL, even a
/// dead one" fallback keeps that policy in one place instead of duplicated
/// `unwrap_or_else` calls at every call site.
/// What: delegates to [`resolve_memory_base_url`]; on `Err`, logs a warning
/// to stderr and returns [`UNREACHABLE_PLACEHOLDER`] so the caller's own HTTP
/// call fails fast rather than hanging or panicking.
/// Test: `resolve_memory_base_url_or_unreachable_falls_back`.
pub fn resolve_memory_base_url_or_unreachable() -> String {
    resolve_memory_base_url().unwrap_or_else(|e| {
        eprintln!("trusty-memory: {e}");
        UNREACHABLE_PLACEHOLDER.to_string()
    })
}

/// Call a trusty-memory MCP tool over JSON-RPC, resolving the address first.
///
/// Why: the common case for a one-off call (no pre-resolved/overridable base
/// threaded through by the caller) — resolve, then call.
/// What: resolves the base URL via [`resolve_memory_base_url`], then
/// delegates to [`call_memory_tool_at`].
/// Test: covered via `call_memory_tool_at`'s tests (this is a thin wrapper).
pub async fn call_memory_tool(method: &str, params: Value) -> Result<Value> {
    let base = resolve_memory_base_url()?;
    call_memory_tool_at(&base, method, params).await
}

/// Call a trusty-memory MCP tool over JSON-RPC against an explicit base URL.
///
/// Why: some callers (catch-up's `CatchupOptions::memory_url`, which is
/// resolved once up-front and threaded through for testability) already hold
/// a resolved or intentionally-overridden base URL and must not re-resolve
/// discovery on every call.
/// What: POSTs `{"jsonrpc":"2.0","id":1,"method":method,"params":params}` to
/// `{base_url}/rpc` — the same dispatcher the MCP stdio server and UDS
/// transport share (`trusty_memory::transport::rpc::dispatch`). Rejects a
/// non-2xx HTTP response and an `error` field in the JSON-RPC envelope as
/// `Err`; otherwise returns the `result` value (`Value::Null` if absent).
/// Test: `call_memory_tool_at_rejects_rpc_error`,
/// `call_memory_tool_at_unreachable_daemon` (ignored).
pub async fn call_memory_tool_at(base_url: &str, method: &str, params: Value) -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .context("build reqwest client")?;
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let url = format!("{}/rpc", base_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!(
            "trusty-memory returned {} for {method}",
            resp.status()
        ));
    }
    let envelope: Value = resp
        .json()
        .await
        .with_context(|| format!("parse {method} JSON-RPC response"))?;
    if let Some(err) = envelope.get("error").filter(|e| !e.is_null()) {
        return Err(anyhow!("{method} RPC error: {err}"));
    }
    Ok(envelope.get("result").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Reuse the crate-wide env-mutation lock (`data_dir::ENV_LOCK`) rather than
    // a module-local one: several test modules mutate `TRUSTY_DATA_DIR_OVERRIDE`,
    // and cargo runs tests in the same process across files, so a separate
    // lock would not prevent the race.
    use crate::data_dir::ENV_LOCK;

    #[test]
    fn resolve_memory_base_url_prefers_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(TRUSTY_MEMORY_URL_ENV, "http://example.test:1234");
        }
        let resolved = resolve_memory_base_url();
        unsafe {
            std::env::remove_var(TRUSTY_MEMORY_URL_ENV);
        }
        assert_eq!(resolved.unwrap(), "http://example.test:1234");
    }

    #[test]
    fn resolve_memory_base_url_errs_when_undiscovered() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var(TRUSTY_MEMORY_URL_ENV);
        }
        // Point the data-dir override at an empty temp dir so no `http_addr`
        // file is found, simulating "daemon never started".
        let tmp = std::env::temp_dir().join(format!(
            "trusty-common-memory-rpc-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let resolved = resolve_memory_base_url();
        unsafe {
            std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        }
        assert!(resolved.is_err(), "expected Err when undiscovered");
    }

    #[test]
    fn resolve_memory_base_url_or_unreachable_falls_back() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var(TRUSTY_MEMORY_URL_ENV);
        }
        let tmp = std::env::temp_dir().join(format!(
            "trusty-common-memory-rpc-test-fallback-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe {
            std::env::set_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let resolved = resolve_memory_base_url_or_unreachable();
        unsafe {
            std::env::remove_var(crate::data_dir::DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(resolved, UNREACHABLE_PLACEHOLDER);
    }

    /// Spawn a one-shot mock `/rpc` server that always replies with `body`.
    ///
    /// Why: `call_memory_tool_at` is a thin, otherwise-untestable HTTP+JSON-RPC
    /// wrapper; a minimal raw-TCP mock (mirroring the pattern already used by
    /// `trusty-mpm`'s `spawn_palace_mock`) lets the envelope-error and
    /// success-result branches be exercised without a live daemon.
    async fn spawn_rpc_mock(body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn call_memory_tool_at_returns_result_on_success() {
        let base =
            spawn_rpc_mock(r#"{"jsonrpc":"2.0","id":1,"result":{"palace":"p","drawers":[]}}"#)
                .await;
        let result = call_memory_tool_at(&base, "memory_list", json!({"palace": "p"}))
            .await
            .unwrap();
        assert_eq!(result["palace"], "p");
    }

    #[tokio::test]
    async fn call_memory_tool_at_rejects_rpc_error() {
        let base = spawn_rpc_mock(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"missing 'palace'"}}"#,
        )
        .await;
        let result = call_memory_tool_at(&base, "memory_list", json!({})).await;
        assert!(result.is_err());
    }

    /// Live-transport test: mark `#[ignore]` so CI doesn't need a daemon.
    /// Port 1 is reserved/unassigned, so the connection fails fast — this
    /// just proves the function returns `Err` rather than hanging or panicking.
    #[tokio::test]
    #[ignore]
    async fn call_memory_tool_at_unreachable_daemon() {
        let result = call_memory_tool_at(
            "http://127.0.0.1:1",
            "memory_list",
            json!({ "palace": "test-palace", "limit": 5 }),
        )
        .await;
        assert!(result.is_err());
    }
}
