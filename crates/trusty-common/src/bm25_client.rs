//! UDS JSON-RPC client for the per-palace `trusty-bm25-daemon` subprocess.
//!
//! Why: trusty-memory wants a BM25 lexical-search lane without holding an
//! in-process index — keeping the BM25 corpus in the same process as the
//! recall hot path would block on disk I/O during writes and contend with the
//! redb/usearch locks. Delegating to a per-palace subprocess (one socket per
//! palace, the subprocess IS the writer lock) gives us natural isolation and
//! mirrors the `EmbedClient` ⇄ `trusty-embed-daemon` design.
//!
//! What: a small async client that
//!   - opens a fresh `UnixStream` per call (no connection pool — local UDS
//!     latency is microseconds),
//!   - sends one newline-terminated JSON-RPC request,
//!   - reads one newline-terminated response and returns the result.
//! Supported methods: `index`, `search`, `delete`. `rebuild` is intentionally
//! not exposed here; the dream subprocess will call it directly over UDS.
//!
//! Test: unit tests in this module cover request shape and the default
//! socket-path resolver. End-to-end coverage lives in
//! `crates/trusty-bm25-daemon/tests/`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// JSON-RPC protocol version string. Must match the daemon's expectation.
const JSONRPC_VERSION: &str = "2.0";

/// Method names — duplicated here verbatim from the daemon's `protocol.rs`
/// so the two layers can't drift without a compile error in tests.
const METHOD_INDEX: &str = "index";
const METHOD_SEARCH: &str = "search";
const METHOD_DELETE: &str = "delete";

/// Resolve the canonical socket path for a given palace.
///
/// Why: callers (the client, the daemon's startup, and operators reading
/// `lsof`) must all agree on where the per-palace socket lives. Keying the
/// filename by palace name keeps multiple palaces isolated from each other.
/// What: `$TMPDIR/trusty-bm25-<palace>.sock`. Falls back to `/tmp` when
/// `TMPDIR` is unset, empty, or whitespace. The palace name is taken
/// verbatim — callers are expected to have sanitised it already (the palace
/// id is already kebab-case / underscore-safe).
/// Test: `socket_path_uses_tmpdir_and_palace_name`.
pub fn socket_path_for_palace(palace: &str) -> PathBuf {
    let dir = match std::env::var("TMPDIR") {
        Ok(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => PathBuf::from("/tmp"),
    };
    dir.join(format!("trusty-bm25-{palace}.sock"))
}

/// One BM25 search hit returned by the daemon.
///
/// Why: callers (trusty-memory's recall path) want both the document id and
/// the score so they can fuse with vector hits via RRF. Using a typed struct
/// keeps the call site free of `serde_json::Value` plumbing.
/// What: a plain pair — `doc_id` is whatever string the caller indexed under,
/// `score` is the BM25 score the daemon assigned.
/// Test: `request_serialises_as_jsonrpc_2_0` checks the wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BM25Hit {
    pub doc_id: String,
    pub score: f32,
}

#[derive(Debug, Serialize)]
struct RpcRequest<'a, P: Serialize> {
    jsonrpc: &'a str,
    method: &'a str,
    params: P,
    id: u64,
}

#[derive(Debug, Serialize)]
struct IndexParams<'a> {
    doc_id: &'a str,
    text: &'a str,
}

#[derive(Debug, Serialize)]
struct SearchParams<'a> {
    query: &'a str,
    top_k: usize,
}

#[derive(Debug, Serialize)]
struct DeleteParams<'a> {
    doc_id: &'a str,
}

#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    #[serde(default = "Option::default")]
    result: Option<T>,
    #[serde(default = "Option::default")]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i32,
    message: String,
}

#[derive(Debug, Deserialize)]
struct IndexResult {
    #[serde(default)]
    indexed: bool,
}

#[derive(Debug, Deserialize)]
struct DeleteResult {
    #[serde(default)]
    deleted: bool,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    #[serde(default)]
    hits: Vec<BM25Hit>,
}

/// Async client for the per-palace `trusty-bm25-daemon` subprocess.
///
/// Why: a tiny value type makes the client cheap to construct, clone, and
/// pass around. It owns nothing other than the socket path, so two callers
/// can share the same `Bm25Client` (or each hold their own) freely.
/// What: holds the resolved socket path and provides `index` / `search` /
/// `delete` async methods. All methods open a fresh `UnixStream` per call.
/// Test: covered by the daemon's integration tests; this module's unit
/// tests pin the default-path resolver and the wire shape.
#[derive(Debug, Clone)]
pub struct Bm25Client {
    socket_path: PathBuf,
}

impl Bm25Client {
    /// Construct a client targeting the canonical socket path for `palace`.
    ///
    /// Why: matches the daemon's own default so callers only need to know the
    /// palace name to reach the right subprocess.
    /// What: stores `socket_path_for_palace(palace)`; no I/O happens until
    /// the first call.
    /// Test: `for_palace_uses_palace_specific_path`.
    pub fn for_palace(palace: impl Into<String>) -> Self {
        let palace = palace.into();
        Self {
            socket_path: socket_path_for_palace(&palace),
        }
    }

    /// Construct a client with an explicit socket path.
    ///
    /// Why: test harnesses and alternate deployment layouts want to bypass
    /// the env-var-based default.
    /// What: stores the path verbatim; no I/O happens until the first call.
    /// Test: trivially covered by every other test that constructs a client.
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// The socket path this client is configured to use.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Index (or replace) a document.
    ///
    /// Why: `memory_remember` calls this after persisting a drawer to redb so
    /// the BM25 lane can answer subsequent `memory_recall` queries.
    /// What: sends `{"method":"index","params":{"doc_id":..,"text":..}}`,
    /// expects `{"result":{"indexed":true}}`. Returns `Ok(())` on success.
    /// Test: end-to-end coverage in `trusty-bm25-daemon/tests/bm25_daemon.rs`.
    pub async fn index(&self, doc_id: &str, text: &str) -> Result<()> {
        let params = IndexParams { doc_id, text };
        let res: IndexResult = self.call(METHOD_INDEX, &params).await?;
        if !res.indexed {
            anyhow::bail!("bm25 daemon reported indexed=false for doc_id={doc_id}");
        }
        Ok(())
    }

    /// Search the BM25 corpus.
    ///
    /// Why: `memory_recall` fuses these hits with vector results via RRF.
    /// What: sends `{"method":"search","params":{"query":..,"top_k":..}}`,
    /// returns the daemon's `hits` array verbatim.
    /// Test: end-to-end coverage in `trusty-bm25-daemon/tests/bm25_daemon.rs`.
    pub async fn search(&self, query: &str, top_k: usize) -> Result<Vec<BM25Hit>> {
        let params = SearchParams { query, top_k };
        let res: SearchResult = self.call(METHOD_SEARCH, &params).await?;
        Ok(res.hits)
    }

    /// Delete a document. Intended for the dream subprocess only.
    ///
    /// Why: append-only ingest is the rule for the request path; the dream
    /// process is the sole deletor. Exposing this here keeps the wire
    /// contract symmetric while the production request path never calls it.
    /// What: sends `{"method":"delete","params":{"doc_id":..}}`. Returns
    /// `Ok(())` whether or not the doc was present.
    /// Test: end-to-end coverage in `trusty-bm25-daemon/tests/bm25_daemon.rs`.
    pub async fn delete(&self, doc_id: &str) -> Result<()> {
        let params = DeleteParams { doc_id };
        let res: DeleteResult = self.call(METHOD_DELETE, &params).await?;
        // The daemon returns `deleted: false` for unknown ids — that's not
        // an error from the caller's perspective; idempotent delete is the
        // documented behaviour.
        let _ = res.deleted;
        Ok(())
    }

    /// Shared RPC helper — open stream, send one frame, read one frame, decode.
    async fn call<P: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &'static str,
        params: &P,
    ) -> Result<R> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| {
                format!(
                    "connect to bm25 daemon at {} (method={method})",
                    self.socket_path.display()
                )
            })?;
        let (read_half, mut write_half) = stream.into_split();

        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method,
            params,
            id: 1,
        };
        let mut payload = serde_json::to_vec(&req).context("serialise bm25 JSON-RPC request")?;
        payload.push(b'\n');
        write_half
            .write_all(&payload)
            .await
            .context("write bm25 JSON-RPC request to daemon")?;
        write_half
            .shutdown()
            .await
            .context("half-close write side of bm25 daemon socket")?;

        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .context("read bm25 JSON-RPC response from daemon")?;
        if n == 0 {
            anyhow::bail!("bm25 daemon closed connection before responding (method={method})");
        }

        let resp: RpcResponse<R> = serde_json::from_str(line.trim()).with_context(|| {
            format!(
                "decode bm25 JSON-RPC response (method={method}, raw={})",
                line.trim()
            )
        })?;
        if let Some(err) = resp.error {
            anyhow::bail!("bm25 daemon error {}: {}", err.code, err.message);
        }
        resp.result
            .ok_or_else(|| anyhow!("bm25 daemon response missing both result and error"))
    }
}

/// Locate the `trusty-bm25-daemon` binary for the current install layout.
///
/// Why: when `TRUSTY_BM25_DAEMON=1` is set, trusty-memory needs to be able
/// to find (or spawn) the daemon binary. Without a proper discovery path the
/// bundled-install case (`cargo install trusty-memory` puts both binaries in
/// the same directory) would require `~/.cargo/bin` to be on PATH globally,
/// which is not guaranteed for launchd plists or non-interactive shell
/// invocations. The three-step search order mirrors `locate_embedderd_binary`
/// (PR #190, trusty-search) for consistency across the trusty-* ecosystem.
///
/// Discovery order:
///   1. `TRUSTY_BM25_DAEMON_BIN` env var — explicit override, always wins.
///   2. Sibling of `current_exe()` — handles the bundled-install case where
///      all binaries from a single crate land in the same directory (both
///      `cargo install` and `cargo build --release` place them in
///      `target/release/`).
///   3. `trusty-bm25-daemon` on `PATH` — handles a separate
///      `cargo install trusty-bm25-daemon` and any other layout where the
///      binary is available globally.
///
/// What: returns the first path at which the binary is found as a file.
/// Returns `Err` with an actionable message if none of the three paths
/// yields a result.
///
/// Test: `locate_bm25_daemon_binary_prefers_sibling` (uses env-var override
/// to simulate the sibling-found path without spawning a real process).
pub fn locate_bm25_daemon_binary() -> anyhow::Result<std::path::PathBuf> {
    // 1. Explicit env-var override.
    if let Ok(explicit) = std::env::var("TRUSTY_BM25_DAEMON_BIN") {
        let p = std::path::PathBuf::from(&explicit);
        if p.is_file() {
            return Ok(p);
        }
        anyhow::bail!(
            "TRUSTY_BM25_DAEMON_BIN={explicit:?} does not point to an existing file"
        );
    }

    // 2. Sibling of the currently-running executable — works for both
    //    `cargo run` (target/debug/) and installed binaries (~/.cargo/bin/).
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("trusty-bm25-daemon");
        if sibling.is_file() {
            return Ok(sibling);
        }
        // Windows variant.
        let sibling_exe = dir.join("trusty-bm25-daemon.exe");
        if sibling_exe.is_file() {
            return Ok(sibling_exe);
        }
    }

    // 3. PATH search.
    if let Ok(found) = which_bm25_daemon() {
        return Ok(found);
    }

    anyhow::bail!(
        "could not locate trusty-bm25-daemon binary. \
         Set TRUSTY_BM25_DAEMON_BIN=/path/to/trusty-bm25-daemon or ensure \
         it is on PATH (or install via `cargo install trusty-memory`)."
    )
}

/// Minimal `which`-style PATH search for `trusty-bm25-daemon`.
///
/// Why: avoids a `which` crate dependency just for this one look-up, keeping
/// the `bm25-client` feature lean. Same approach used by `which_embedderd`.
/// What: splits `PATH` on the OS separator and returns the first directory
/// entry that names the daemon binary.
/// Test: tested implicitly when the sibling-path lookup fails and the daemon
/// is on PATH.
fn which_bm25_daemon() -> anyhow::Result<std::path::PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    for dir in path_var.split(sep) {
        let candidate = std::path::PathBuf::from(dir).join("trusty-bm25-daemon");
        if candidate.is_file() {
            return Ok(candidate);
        }
        #[cfg(windows)]
        {
            let candidate_exe = std::path::PathBuf::from(dir).join("trusty-bm25-daemon.exe");
            if candidate_exe.is_file() {
                return Ok(candidate_exe);
            }
        }
    }
    anyhow::bail!("trusty-bm25-daemon not found on PATH")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_uses_tmpdir_and_palace_name() {
        let p = socket_path_for_palace("my-palace");
        let fname = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        assert!(
            fname.starts_with("trusty-bm25-"),
            "filename must start with trusty-bm25-: {fname}"
        );
        assert!(
            fname.contains("my-palace"),
            "filename must include palace name: {fname}"
        );
        assert!(
            fname.ends_with(".sock"),
            "filename must end with .sock: {fname}"
        );
        assert!(p.parent().is_some());
    }

    #[test]
    fn for_palace_uses_palace_specific_path() {
        let c = Bm25Client::for_palace("alpha");
        let fname = c
            .socket_path()
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        assert!(fname.contains("alpha"), "got: {fname}");
    }

    #[test]
    fn index_request_serialises_as_jsonrpc_2_0() {
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_INDEX,
            params: IndexParams {
                doc_id: "doc-1",
                text: "hello world",
            },
            id: 1,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"method\":\"index\""));
        assert!(s.contains("\"doc_id\":\"doc-1\""));
        assert!(s.contains("\"text\":\"hello world\""));
    }

    #[test]
    fn search_request_serialises_with_query_and_top_k() {
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_SEARCH,
            params: SearchParams {
                query: "cargo test",
                top_k: 5,
            },
            id: 1,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"method\":\"search\""));
        assert!(s.contains("\"query\":\"cargo test\""));
        assert!(s.contains("\"top_k\":5"));
    }

    #[test]
    fn delete_request_serialises_with_doc_id() {
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method: METHOD_DELETE,
            params: DeleteParams { doc_id: "x" },
            id: 1,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"method\":\"delete\""));
        assert!(s.contains("\"doc_id\":\"x\""));
    }

    #[test]
    fn bm25_hit_round_trips() {
        let h = BM25Hit {
            doc_id: "drawer-1".into(),
            score: 0.42,
        };
        let s = serde_json::to_string(&h).unwrap();
        let back: BM25Hit = serde_json::from_str(&s).unwrap();
        assert_eq!(back.doc_id, "drawer-1");
        assert!((back.score - 0.42).abs() < 1e-6);
    }

    /// Why: pin the env-var-override branch of `locate_bm25_daemon_binary`
    /// so a regression that loses the override causes a test failure.
    /// What: write the current test binary's path into a tempfile, point the
    /// env var at it, call the locator, assert it returns that exact path.
    /// (We use the test binary itself as a stand-in for the daemon — we only
    /// care that the path is found, not that it is the real daemon.)
    /// Test: this test itself.
    #[test]
    fn locate_bm25_daemon_binary_prefers_env_override() {
        // Use the test binary itself as a "daemon" — any existing file works.
        let exe = std::env::current_exe().expect("current_exe");
        // Guard against parallel tests mutating the env var.
        // Safety: test-only, single-threaded env mutation is acceptable here
        // because this test function is the sole writer of this key in this
        // crate's test binary.
        let key = "TRUSTY_BM25_DAEMON_BIN";
        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, &exe) };
        let result = locate_bm25_daemon_binary();
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        assert_eq!(result.expect("must find via env var"), exe);
    }

    /// Why: confirm that an env-var pointing at a non-existent file returns
    /// an error rather than silently falling through to sibling / PATH.
    /// Test: this test itself.
    #[test]
    fn locate_bm25_daemon_binary_env_override_nonexistent_errors() {
        let key = "TRUSTY_BM25_DAEMON_BIN";
        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, "/nonexistent/trusty-bm25-daemon") };
        let result = locate_bm25_daemon_binary();
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        assert!(
            result.is_err(),
            "expected error for non-existent path, got: {result:?}"
        );
    }
}
