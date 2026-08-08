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
//! #5180: the transport itself is [`crate::uds::rpc::send_framed_request`] —
//! this module owns the JSON-RPC envelope and the error contract, not the
//! framing.
//! Supported methods: `index`, `search`, `delete`. `rebuild` is intentionally
//! not exposed here; the dream subprocess will call it directly over UDS.
//!
//! Test: unit tests in this module cover request shape and the default
//! socket-path resolver. End-to-end coverage lives in
//! `crates/trusty-bm25-daemon/tests/`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

/// JSON-RPC protocol version string. Must match the daemon's expectation.
const JSONRPC_VERSION: &str = "2.0";

/// Wall-clock ceiling for one exchange with the BM25 daemon.
///
/// Why (#5180): the hand-rolled framing this client used before had no bound at
/// all, so a daemon that accepted the connection and then wedged held the
/// caller open forever — and because `memory_remember` calls `index` while
/// holding the per-palace write mutex, one wedged daemon stalled every
/// concurrent writer on that palace too. Every method here is a tokenise-and-
/// look-up round trip that finishes in milliseconds, so 60 s is three orders of
/// magnitude of headroom and still finite. This is the one deliberate
/// behaviour change in the migration: an unbounded wait became a bounded one.
/// Test: `rpc_timeout_is_generous_but_finite`.
const RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// Method names — duplicated here verbatim from the daemon's `protocol.rs`
/// so the two layers can't drift without a compile error in tests.
const METHOD_INDEX: &str = "index";
const METHOD_SEARCH: &str = "search";
const METHOD_DELETE: &str = "delete";
const METHOD_STATS: &str = "stats";
const METHOD_MISSING_DOCS: &str = "missing_docs";

/// JSON-RPC code the daemon returns for a method it does not implement.
///
/// Why: a client newer than the daemon it is talking to gets this back for
/// `stats` / `missing_docs`, and that is a materially different situation from
/// a socket that is not there. Treating the two alike makes a healthy but
/// outdated daemon look unreachable, which sends an operator hunting the wrong
/// problem. Mirrors `trusty-bm25-daemon`'s `protocol::ERR_METHOD_NOT_FOUND` —
/// duplicated verbatim for the same reason the method names are: this crate
/// must not depend on the daemon crate.
/// What: `-32601`, the JSON-RPC standard code.
/// Test: `method_not_found_is_distinguishable_from_a_transport_error`.
pub const ERR_METHOD_NOT_FOUND: i32 = -32601;

/// A JSON-RPC error the daemon returned, carrying its code.
///
/// Why: `anyhow` flattens every failure into one opaque chain, so a caller
/// deciding how to degrade cannot tell "this daemon is too old to answer"
/// from "this daemon is gone". Attaching the code as a downcastable error
/// keeps every existing `anyhow::Result` signature intact while making that
/// one distinction available to the callers that need it.
/// What: the wire code and message. Reach it with
/// [`is_method_not_found`] or `err.downcast_ref::<Bm25RpcError>()`.
/// Test: `method_not_found_is_distinguishable_from_a_transport_error`.
#[derive(Debug, Clone)]
pub struct Bm25RpcError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for Bm25RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bm25 daemon error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for Bm25RpcError {}

/// True when `err` is the daemon answering "I do not implement that method".
///
/// Why: the caller that needs this is a backfiller deciding whether an
/// unanswerable coverage query means "degrade and shout about an old daemon"
/// or "the daemon is unreachable". Both fail closed, but they need different
/// log lines and different operator actions.
/// What: walks the `anyhow` chain for a [`Bm25RpcError`] carrying
/// [`ERR_METHOD_NOT_FOUND`].
/// Test: `method_not_found_is_distinguishable_from_a_transport_error`.
pub fn is_method_not_found(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|e| e.downcast_ref::<Bm25RpcError>())
        .any(|e| e.code == ERR_METHOD_NOT_FOUND)
}

/// Resolve the canonical socket path for a given palace.
///
/// Why: callers (the client, the daemon's startup, and operators reading
/// `lsof`) must all agree on where the per-palace socket lives. Keying the
/// filename by palace name keeps multiple palaces isolated from each other.
/// What: `<$TMPDIR or /tmp>/trusty-<uid>/trusty-bm25-<palace>.sock`. The palace
/// name is taken verbatim — callers are expected to have sanitised it already
/// (the palace id is already kebab-case / underscore-safe).
///
/// #5099: the socket used to sit directly in `$TMPDIR`, falling back to `/tmp`.
/// On a Linux host with `TMPDIR` unset that is a world-writable directory that
/// cannot be narrowed, so the uid-keyed subdirectory from
/// [`crate::uds::scratch_socket_dir`] is interposed and held at `0700`.
///
/// Test: `socket_path_uses_tmpdir_and_palace_name`.
pub fn socket_path_for_palace(palace: &str) -> PathBuf {
    crate::uds::scratch_socket_dir().join(format!("trusty-bm25-{palace}.sock"))
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

/// Corpus-coverage figures reported by the daemon's `stats` method.
///
/// Why: an empty `search` result is ambiguous — it means either "the query
/// matched nothing" or "this daemon holds nothing to match against". Callers
/// that cannot tell those apart silently serve partial results as if they were
/// complete. `doc_count` resolves the ambiguity; `total_text_bytes` is what a
/// supervisor budgets RAM against, because the daemon retains every document's
/// full text in memory.
/// What: a plain pair of counters, deserialised from the daemon's
/// `StatsResult` wire shape. Neither field carries `#[serde(default)]`: a
/// daemon-side rename must fail the decode, because defaulting `doc_count` to
/// zero is indistinguishable from an empty palace.
/// Test: `stats_response_decodes`, `stats_response_rejects_a_renamed_field`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Bm25Stats {
    /// Live documents the daemon is serving.
    pub doc_count: usize,
    /// Summed byte length of the retained document text.
    pub total_text_bytes: u64,
}

/// Coverage answer from the daemon's `missing_docs` method.
///
/// Why: `Bm25Stats` answers "how many", which is not the question a caller
/// establishing coverage is asking. `missing.is_empty()` is a statement about
/// the SET of documents the daemon holds, and it stays correct no matter how
/// many documents the daemon holds that the caller never asked about.
/// What: `missing` names the requested ids the daemon does not hold; `checked`
/// echoes how many were examined so a caller can tell a real empty answer from
/// a request that never reached the index. No `#[serde(default)]`, for the
/// same reason as [`Bm25Stats`] — an absent `missing` field would decode as
/// "fully covered".
/// Test: `missing_docs_response_decodes`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bm25Coverage {
    /// Requested doc ids the daemon does not hold.
    pub missing: Vec<String>,
    /// How many ids the daemon examined.
    pub checked: usize,
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

    /// Ask the daemon how much corpus it is actually serving.
    ///
    /// Why: this is the call that lets a caller distinguish "indexed, no
    /// lexical hits" from "not indexed". A backfiller uses it to decide
    /// whether to skip a palace and to confirm what landed afterwards; a
    /// recall path can use it to decide whether an empty BM25 result deserves
    /// to influence fusion at all.
    /// What: sends `{"method":"stats"}` with an empty params object and
    /// returns the decoded [`Bm25Stats`]. Propagates the connection error when
    /// the daemon is absent — "cannot ask" is a different state from "asked,
    /// answered zero", and collapsing the two is the fail-open shape this
    /// method exists to prevent. Callers that want to degrade should match on
    /// the `Err` explicitly.
    /// Test: end-to-end coverage in `trusty-bm25-daemon/tests/bm25_daemon.rs`
    /// and `trusty-memory/tests/bm25_backfill_e2e.rs`.
    pub async fn stats(&self) -> Result<Bm25Stats> {
        self.call(METHOD_STATS, &serde_json::json!({})).await
    }

    /// Ask the daemon which of `doc_ids` it does not hold.
    ///
    /// Why: this is the only call that establishes coverage. [`Self::stats`]
    /// reports a count, and a count is satisfied by documents the caller never
    /// asked about — one stale entry left behind by a delete that never
    /// happened is enough for `doc_count >= my_drawers` to claim coverage over
    /// drawers the daemon has never seen. An empty `missing` list cannot be
    /// satisfied that way.
    /// What: sends `{"method":"missing_docs","params":{"doc_ids":[..]}}`.
    /// Propagates the error rather than degrading — a coverage question that
    /// could not be asked must never read as a coverage answer. A daemon older
    /// than 0.2.0 answers `-32601`; see [`is_method_not_found`].
    /// Test: `missing_docs_response_decodes`; end-to-end in
    /// `trusty-memory/tests/bm25_backfill_e2e.rs`.
    pub async fn missing_docs(&self, doc_ids: &[String]) -> Result<Bm25Coverage> {
        let params = serde_json::json!({ "doc_ids": doc_ids });
        self.call(METHOD_MISSING_DOCS, &params).await
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

    /// Shared RPC helper — send one frame, read one frame, decode.
    ///
    /// Why: #5180 — this used to hand-roll `write_all`, `BufReader::read_line`
    /// and `serde_json::from_str`: a fourth private copy of a framing contract
    /// ADR-0034 §4 says lives in exactly one place. It now routes through
    /// [`crate::uds::rpc::send_framed_request`], which owns the dial (with the
    /// #5099 permission check), the newline framing, the response size cap, and
    /// the timeout.
    /// What: builds the JSON-RPC envelope, hands it to the shared entry point,
    /// then applies this client's own error contract — a `-32601` still
    /// surfaces as a downcastable [`Bm25RpcError`] so [`is_method_not_found`]
    /// keeps working.
    /// Test: `search_sends_one_newline_framed_jsonrpc_frame`,
    /// `call_surfaces_a_daemon_rpc_error_with_its_code`.
    async fn call<P: Serialize, R: serde::de::DeserializeOwned>(
        &self,
        method: &'static str,
        params: &P,
    ) -> Result<R> {
        let req = RpcRequest {
            jsonrpc: JSONRPC_VERSION,
            method,
            params,
            id: 1,
        };
        let resp: RpcResponse<R> =
            crate::uds::rpc::send_framed_request(&self.socket_path, &req, RPC_TIMEOUT)
                .await
                .with_context(|| {
                    format!(
                        "bm25 daemon RPC at {} (method={method})",
                        self.socket_path.display()
                    )
                })?;
        if let Some(err) = resp.error {
            // Typed rather than a bare `bail!` so a caller can tell a daemon
            // that does not implement the method from one that is not there.
            return Err(anyhow::Error::new(Bm25RpcError {
                code: err.code,
                message: err.message,
            })
            .context(format!("bm25 daemon rejected method={method}")));
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
        anyhow::bail!("TRUSTY_BM25_DAEMON_BIN={explicit:?} does not point to an existing file");
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
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// Bind a hardened socket under `dir`, answer one connection with `reply`,
    /// and hand back whatever the client wrote.
    fn spawn_stub(dir: &Path, reply: &'static [u8]) -> (PathBuf, tokio::task::JoinHandle<Vec<u8>>) {
        let sock = dir.join("sockets").join("bm25-stub.sock");
        let listener = crate::uds::bind_hardened(&sock).expect("bind stub socket");
        let handle = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.expect("accept");
            // The client half-closes after its request frame, so this returns
            // exactly the request bytes.
            let mut raw = Vec::new();
            conn.read_to_end(&mut raw).await.expect("drain request");
            conn.write_all(reply).await.expect("write reply");
            conn.flush().await.expect("flush reply");
            raw
        });
        (sock, handle)
    }

    /// Why (#5180): this client's framing moved into `uds::rpc`, and the daemon
    /// on the other end is a separate crate that was NOT changed. A drift in
    /// the request bytes — a missing terminator, a second one, a length prefix,
    /// a renamed envelope field — is invisible to every other test here, which
    /// only serialise structs in isolation. This one asserts what actually goes
    /// down the socket, and fails if the framing changes.
    /// What: runs a real `search` against a stub listener and asserts the wire
    /// bytes are exactly one newline-terminated JSON-RPC 2.0 frame.
    /// Test: this test itself.
    #[tokio::test]
    async fn search_sends_one_newline_framed_jsonrpc_frame() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (sock, served) = spawn_stub(
            tmp.path(),
            b"{\"jsonrpc\":\"2.0\",\"result\":{\"hits\":[{\"doc_id\":\"d1\",\"score\":1.5}]},\"id\":1}\n",
        );

        let hits = Bm25Client::new(sock)
            .search("cargo test", 5)
            .await
            .expect("search round trip");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "d1");

        let raw = String::from_utf8(served.await.expect("join")).expect("utf8");
        assert!(
            raw.ends_with('\n'),
            "the request frame must be newline-terminated: {raw:?}"
        );
        assert_eq!(
            raw.matches('\n').count(),
            1,
            "exactly one frame, one terminator: {raw:?}"
        );
        let sent: serde_json::Value =
            serde_json::from_str(raw.trim_end_matches('\n')).expect("the frame is one JSON value");
        assert_eq!(sent["jsonrpc"], "2.0");
        assert_eq!(sent["method"], "search");
        assert_eq!(sent["params"]["query"], "cargo test");
        assert_eq!(sent["params"]["top_k"], 5);
        assert_eq!(sent["id"], 1);
    }

    /// Why (#5180): [`is_method_not_found`] is what lets a backfiller tell an
    /// outdated-but-healthy daemon from an absent one, and it depends on the
    /// error envelope surviving the transport as a downcastable
    /// [`Bm25RpcError`]. The migration replaced the decode path, so this pins
    /// the contract end to end rather than on a synthetic error.
    /// What: stubs a `-32601` reply to a real `missing_docs` call.
    /// Test: this test itself.
    #[tokio::test]
    async fn call_surfaces_a_daemon_rpc_error_with_its_code() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (sock, served) = spawn_stub(
            tmp.path(),
            b"{\"jsonrpc\":\"2.0\",\"error\":{\"code\":-32601,\"message\":\"unknown method\"},\"id\":1}\n",
        );

        let err = Bm25Client::new(sock)
            .missing_docs(&["a".to_string()])
            .await
            .expect_err("an error envelope is not a result");
        assert!(
            is_method_not_found(&err),
            "the daemon's -32601 must survive the transport: {err:#}"
        );
        let _ = served.await;
    }

    /// Why (#5180): the migration introduced a bound where there was none. A
    /// value that drifts down to seconds would start failing legitimate calls
    /// under load; removing it would restore the indefinite hang.
    /// What: pins the ceiling's order of magnitude.
    /// Test: this test itself.
    #[test]
    fn rpc_timeout_is_generous_but_finite() {
        let secs = RPC_TIMEOUT.as_secs();
        assert!(
            (30..=300).contains(&secs),
            "a BM25 exchange finishes in milliseconds; {secs}s is outside the \
             band that is generous without being an effective hang"
        );
    }

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
        // #5099: the parent must be the uid-keyed directory the daemon holds
        // at 0700, not a bare `$TMPDIR` (or worse, a world-writable `/tmp`).
        assert_eq!(
            p.parent(),
            Some(crate::uds::scratch_socket_dir().as_path()),
            "socket must live in the per-uid scratch socket directory"
        );
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

    /// Why: the client's `Bm25Stats` and the daemon's `StatsResult` are
    /// separate types in separate crates (deliberately — the client must not
    /// depend on the daemon binary). A field-name drift between them would
    /// silently decode as `Default::default()`, i.e. "zero documents", which
    /// reads exactly like an unindexed palace. Pinning the wire shape here is
    /// what stops that drift from being invisible.
    /// What: decodes the daemon's literal success envelope.
    /// Test: this test itself.
    #[test]
    fn stats_response_decodes() {
        let raw =
            r#"{"jsonrpc":"2.0","result":{"doc_count":1311,"total_text_bytes":4194304},"id":1}"#;
        let resp: RpcResponse<Bm25Stats> = serde_json::from_str(raw).unwrap();
        let stats = resp.result.expect("result present");
        assert_eq!(stats.doc_count, 1311);
        assert_eq!(stats.total_text_bytes, 4_194_304);
    }

    /// Why: with `#[serde(default)]` on the fields, a daemon-side rename
    /// decoded as `doc_count: 0` — which reads exactly like an empty palace
    /// and is the silent drift the type's own doc comment claims to prevent.
    /// Dropping the attribute turns that drift into a decode error.
    /// What: feeds a plausibly-renamed field and asserts the decode fails.
    /// Test: this test itself.
    #[test]
    fn stats_response_rejects_a_renamed_field() {
        let raw =
            r#"{"jsonrpc":"2.0","result":{"documents":1311,"total_text_bytes":4194304},"id":1}"#;
        let decoded: Result<RpcResponse<Bm25Stats>, _> = serde_json::from_str(raw);
        assert!(
            decoded.is_err(),
            "a renamed field must fail the decode, not default to zero documents"
        );
    }

    /// Why: `missing` is the field the coverage predicate reads. If it
    /// defaulted, an envelope that lost the field would decode as "nothing
    /// missing" — full coverage claimed off a response that never said so.
    /// What: decodes a populated answer, then asserts a truncated one errors.
    /// Test: this test itself.
    #[test]
    fn missing_docs_response_decodes() {
        let raw = r#"{"jsonrpc":"2.0","result":{"missing":["b"],"checked":2},"id":1}"#;
        let resp: RpcResponse<Bm25Coverage> = serde_json::from_str(raw).unwrap();
        let cov = resp.result.expect("result present");
        assert_eq!(cov.missing, vec!["b".to_string()]);
        assert_eq!(cov.checked, 2);

        let truncated = r#"{"jsonrpc":"2.0","result":{"checked":2},"id":1}"#;
        let decoded: Result<RpcResponse<Bm25Coverage>, _> = serde_json::from_str(truncated);
        assert!(
            decoded.is_err(),
            "an absent `missing` field must not decode as full coverage"
        );
    }

    /// Why: a client newer than its daemon gets `-32601` for `stats` and
    /// `missing_docs`. Classifying that as "daemon unreachable" sends an
    /// operator looking for a dead socket that is in fact serving fine.
    /// What: builds both error shapes and asserts the discriminator separates
    /// them — including that a transport failure is NOT method-not-found.
    /// Test: this test itself.
    #[test]
    fn method_not_found_is_distinguishable_from_a_transport_error() {
        let not_found = anyhow::Error::new(Bm25RpcError {
            code: ERR_METHOD_NOT_FOUND,
            message: "unknown method: missing_docs".to_string(),
        })
        .context("bm25 daemon rejected method=missing_docs");
        assert!(is_method_not_found(&not_found));

        let internal = anyhow::Error::new(Bm25RpcError {
            code: -32603,
            message: "index poisoned".to_string(),
        });
        assert!(!is_method_not_found(&internal));

        let transport = anyhow!("connect to bm25 daemon at /tmp/x.sock: No such file");
        assert!(!is_method_not_found(&transport));
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
    /// Test: this test itself. `#[serial]` serialises it against the other
    /// `TRUSTY_BM25_DAEMON_BIN`-mutating test so neither one's env restore can
    /// clear the override mid-flight in the other (issue #2252).
    #[test]
    #[serial_test::serial]
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
    /// `#[serial]` is load-bearing (issue #2252): without it this test races
    /// the sibling env-override test, whose restore can `remove_var` the
    /// override between this test's `set_var` and its locate call — the locator
    /// then hits the sibling/PATH fallback and finds a host-installed
    /// `trusty-bm25-daemon`, so the expected error never occurs and the assert
    /// fails on machines that have the daemon installed.
    /// Test: this test itself.
    #[test]
    #[serial_test::serial]
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
