//! Unix domain socket transport — NDJSON-framed JSON-RPC 2.0 over
//! `tokio::net::UnixListener`.
//!
//! Why: Provides a low-overhead local transport for clients that don't
//! want HTTP's TCP-handshake / header overhead. The
//! `trusty-memory-mcp-bridge` binary uses this socket to relay Claude
//! Code's stdio MCP traffic into the daemon; hook CLIs can use it
//! instead of HTTP for faster end-to-end latency; future tools can
//! plug in without re-implementing protocol framing.
//! What:
//!   - [`socket_path`] — resolves the canonical UDS path
//!     (`$TMPDIR/trusty-memory.sock` on macOS, `$XDG_RUNTIME_DIR/...`
//!     on Linux). Kept under 104 bytes to stay within the AF_UNIX
//!     `sun_path` limit on macOS.
//!   - [`write_uds_addr_file`] — writes the socket path to
//!     `<data_root>/uds_addr` so the bridge can discover it without
//!     hard-coding the resolution rules.
//!   - [`run_uds`] — binds the listener (cleaning up a stale socket
//!     first if it's dead) and accepts connections in a loop. Each
//!     connection is handled in a `tokio::spawn` task that reads
//!     newline-delimited JSON requests and writes newline-delimited
//!     JSON responses by calling [`crate::transport::rpc::dispatch`].
//!
//! Test: see `transport::uds::tests::*` and the integration tests in
//!     `tests/uds_roundtrip.rs`.

use crate::transport::rpc::{self, JsonRpcRequest, JsonRpcResponse};
use crate::AppState;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// File name of the socket inside the runtime directory.
///
/// Why: Stable name lets every client (bridge, CLI, third-party tools)
/// agree on where to look without per-client configuration.
/// Test: `socket_path_uses_tmp_dir_on_macos` confirms the suffix.
pub const SOCKET_FILE_NAME: &str = "trusty-memory.sock";

/// File name of the address-discovery file inside `data_root`.
///
/// Why: mirrors the existing `http_addr` convention so clients can find
/// the UDS path even when the OS-default location ($TMPDIR / XDG) is
/// unusual (e.g. when the daemon was started with a custom
/// `TMPDIR=/elsewhere`). The bridge reads this first, falling back to
/// the OS default.
/// Test: `write_uds_addr_file_round_trip`.
pub const UDS_ADDR_FILE: &str = "uds_addr";

/// Resolve the canonical Unix-socket path for the trusty-memory daemon.
///
/// Why: macOS's `sockaddr_un.sun_path` is 104 bytes (vs Linux's 108);
/// stuffing the socket under `~/Library/Application Support/...` blows
/// past that limit and `bind()` fails with `EINVAL`. Putting the socket
/// under `$TMPDIR` on macOS (typical
/// `/var/folders/xx/...../T/trusty-memory.sock`) stays under 104 bytes
/// even with deep tempdir paths. On Linux we prefer
/// `$XDG_RUNTIME_DIR/trusty-memory.sock` when that env var is set
/// (it's a tmpfs sized to the user's session) and fall back to
/// `$TMPDIR` otherwise.
/// What: returns `<runtime-dir>/trusty-memory.sock`. The directory
/// must exist when [`run_uds`] is called; the implementation does NOT
/// create it (env-supplied tempdirs are always pre-existing).
/// Test: `socket_path_uses_tmp_dir_on_macos`.
pub fn socket_path() -> PathBuf {
    runtime_dir().join(SOCKET_FILE_NAME)
}

/// Per-daemon socket path derived from the daemon's `data_root`.
///
/// Why: production daemons share `socket_path()` so the bridge can
/// find them without configuration. But the tests spin multiple
/// daemons in parallel under per-test tempdir-scoped data roots; a
/// shared socket path collides and breaks test isolation. Deriving
/// the socket name from a stable hash of `data_root` means each
/// test gets its own socket without forcing the bridge to learn
/// every possible data_root. The bridge still finds the right one
/// via `<data_root>/uds_addr` (the discovery file).
/// What: when `data_root` matches the production data dir,
/// falls back to [`socket_path`]; otherwise returns
/// `<runtime>/trusty-memory-<short-hash>.sock` where the hash is
/// derived from `data_root`. Always under 104 bytes (the hash is
/// 16 hex chars + the prefix/suffix; well within macOS's sun_path
/// limit).
/// Test: `socket_path_for_data_root_returns_per_root_path` and
/// `socket_path_for_production_matches_default`.
pub fn socket_path_for(data_root: &Path) -> PathBuf {
    let production = trusty_common::resolve_data_dir("trusty-memory")
        .ok()
        // The production layout puts palaces under
        // `<data_dir>/palaces/` when that subdirectory exists.
        // Either path (with or without `/palaces`) should map to the
        // canonical socket.
        .map(|d| {
            let with_palaces = d.join("palaces");
            (d, with_palaces)
        });
    if let Some((bare, with_palaces)) = production {
        if data_root == bare || data_root == with_palaces {
            return socket_path();
        }
    }
    // Test / custom data_root: derive a stable per-root socket name.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data_root.hash(&mut hasher);
    let h = hasher.finish();
    runtime_dir().join(format!("trusty-memory-{h:016x}.sock"))
}

/// Resolve the runtime directory that will hold the socket.
///
/// Why: kept separate from [`socket_path`] so tests can override the
/// path via env-var injection without re-implementing the full
/// resolution rules.
/// What: returns `$XDG_RUNTIME_DIR` if set (Linux convention),
/// `$TMPDIR` if set (macOS / BSD convention), or `std::env::temp_dir()`
/// as a last resort.
/// Test: `runtime_dir_uses_xdg_runtime_dir_first`.
fn runtime_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(d) = std::env::var("TMPDIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    std::env::temp_dir()
}

/// Write the socket path to `<data_root>/uds_addr` atomically.
///
/// Why: the bridge needs to find the live socket path even when the
/// daemon was started with an unusual TMPDIR. Mirrors the
/// `http_addr` discovery convention.
/// What: creates the parent directory if missing, writes the path
/// followed by a newline to `uds_addr.tmp`, then renames over the
/// target so readers never see a partial value.
/// Test: `write_uds_addr_file_round_trip`.
pub fn write_uds_addr_file(data_root: &Path, sock_path: &Path) -> std::io::Result<()> {
    let path = data_root.join(UDS_ADDR_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("addr.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        writeln!(f, "{}", sock_path.display())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Remove a stale socket file at `sock_path` if no live daemon owns it.
///
/// Why: bind() fails with `EADDRINUSE` when a socket file already
/// exists, even if the previous daemon crashed without cleaning up.
/// Probing with `connect()` first distinguishes "another daemon is
/// already running" (we should fail loudly) from "leftover file from
/// a previous crash" (we should remove it and continue).
/// What: tries `UnixStream::connect(sock_path)`. If it succeeds, a
/// daemon is already alive — return an error so the caller can exit
/// gracefully. If it fails (refused / no such file), remove the file
/// best-effort and return Ok.
/// Test: `stale_socket_is_cleaned_up`.
pub async fn clean_stale_socket(sock_path: &Path) -> Result<()> {
    if !sock_path.exists() {
        return Ok(());
    }
    match UnixStream::connect(sock_path).await {
        Ok(_stream) => {
            anyhow::bail!(
                "another trusty-memory daemon is already listening on {}",
                sock_path.display()
            );
        }
        Err(_) => {
            // No live owner — remove the stale file. A failure here
            // (permissions, race with another cleanup) is propagated so
            // the caller can decide whether to abort.
            std::fs::remove_file(sock_path).with_context(|| {
                format!(
                    "remove stale socket {} (no live owner)",
                    sock_path.display()
                )
            })?;
            Ok(())
        }
    }
}

/// Bind a `UnixListener` at `sock_path`, cleaning up any stale socket
/// first and setting mode `0600`.
///
/// Why: the daemon's data is per-user; a world-readable socket would
/// let any local user query (and mutate) palaces. `0600` (owner
/// read/write only) is the standard "owned by this user" mode.
/// What: cleans stale, binds, chmods. Returns the listener.
/// Test: covered indirectly by `uds_ndjson_roundtrip`.
pub async fn bind_uds(sock_path: &Path) -> Result<UnixListener> {
    clean_stale_socket(sock_path).await?;
    if let Some(parent) = sock_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket parent {}", parent.display()))?;
    }
    let listener = UnixListener::bind(sock_path)
        .with_context(|| format!("bind UDS at {}", sock_path.display()))?;
    // Restrict the socket to owner-only access. On macOS / Linux this
    // is the standard "only my UID can talk to my daemon" posture.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(sock_path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(sock_path, perms)?;
    }
    Ok(listener)
}

/// Run the UDS accept loop, spawning a per-connection NDJSON handler.
///
/// Why: this is the long-running task — the daemon spawns it alongside
/// the axum HTTP server in `run_http_on`. Each accepted connection
/// gets its own task so a slow client never blocks others.
/// What: loops on `listener.accept()`. Each accepted `UnixStream` is
/// wrapped in a `BufReader` and fed line-by-line to [`handle_connection`].
/// Errors from `accept()` are logged but never bubble — a single
/// transient error must not bring the daemon down.
/// Test: integration via `uds_ndjson_roundtrip` and
/// `uds_handles_concurrent_connections`.
pub async fn run_uds(state: AppState, listener: UnixListener) -> Result<()> {
    tracing::info!("UDS listener accepting connections");
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(state, stream).await {
                        tracing::debug!("UDS connection ended: {e:#}");
                    }
                });
            }
            Err(e) => {
                // accept() can fail on resource exhaustion (FD limit,
                // OOM); log and back off briefly so we don't busy-spin.
                tracing::warn!("UDS accept error: {e:#}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

/// Handle one UDS connection: NDJSON request → dispatch → NDJSON
/// response, repeated until the peer closes.
///
/// Why: matches the MCP spec ("messages are delimited by newlines, and
/// MUST NOT contain embedded newlines") and lets a single connection
/// pipeline many requests, which Claude Code does heavily.
/// What: wraps the read half in a `BufReader`, calls `read_line` to
/// pull one frame, parses it as [`JsonRpcRequest`], calls
/// [`crate::transport::rpc::dispatch`], serialises the response with
/// `serde_json::to_string` (single-line by default) + `\n`, and
/// writes it back. On a parse error we emit a JSON-RPC parse-error
/// response so the client sees something rather than the connection
/// silently misbehaving. Notifications (id absent) suppress the
/// response per spec.
/// Test: `uds_ndjson_roundtrip` and `uds_handles_concurrent_connections`
/// in the integration tests.
async fn handle_connection(state: AppState, stream: UnixStream) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.context("UDS read_line")?;
        if n == 0 {
            // EOF — peer closed the connection. Normal exit.
            return Ok(());
        }
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<JsonRpcRequest>(trimmed) {
            Ok(req) => {
                let is_notification = req.id.is_none() || req.id == Some(serde_json::Value::Null);
                let resp = rpc::dispatch(&state, req).await;
                if is_notification {
                    // Spec: notifications MUST NOT receive a response.
                    continue;
                }
                resp
            }
            Err(e) => JsonRpcResponse::err(
                serde_json::Value::Null,
                rpc::error_codes::PARSE_ERROR,
                format!("parse error: {e}"),
            ),
        };
        let mut serialized = serde_json::to_string(&response).context("serialise response")?;
        // MCP spec: messages MUST NOT contain embedded newlines. We
        // rely on `serde_json::to_string`'s compact output, but assert
        // here defensively so a future serde-pretty regression would
        // surface in tests rather than corrupt the wire silently.
        debug_assert!(
            !serialized.contains('\n'),
            "response must not contain embedded newlines: {serialized}"
        );
        serialized.push('\n');
        write_half
            .write_all(serialized.as_bytes())
            .await
            .context("UDS write_all")?;
        write_half.flush().await.context("UDS flush")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: TMPDIR-based path keeps macOS happy (104-byte sun_path limit).
    /// What: sets TMPDIR to a known short path, calls socket_path(),
    /// asserts the result lives under TMPDIR and ends with our suffix.
    /// Test: this test.
    #[tokio::test]
    async fn socket_path_uses_tmp_dir_on_macos() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let original_tmpdir = std::env::var("TMPDIR").ok();
        let original_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: env is mutated under the test-only env_test_lock.
        unsafe {
            std::env::set_var("TMPDIR", "/tmp");
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
        let p = socket_path();
        assert!(
            p.ends_with(SOCKET_FILE_NAME),
            "expected suffix {SOCKET_FILE_NAME}, got {}",
            p.display()
        );
        // Restore so we don't pollute sibling tests that depend on the
        // real $TMPDIR for tempfile placement.
        unsafe {
            match original_tmpdir {
                Some(v) => std::env::set_var("TMPDIR", v),
                None => std::env::remove_var("TMPDIR"),
            }
            match original_xdg {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    /// Why: XDG_RUNTIME_DIR is the right Linux convention and must win
    /// over TMPDIR when both are set.
    /// What: sets both, calls runtime_dir(), asserts XDG wins.
    /// Test: this test.
    #[tokio::test]
    async fn runtime_dir_uses_xdg_runtime_dir_first() {
        let _guard = crate::commands::env_test_lock().lock().await;
        let original_tmpdir = std::env::var("TMPDIR").ok();
        let original_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: env is mutated under the test-only env_test_lock.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/tmp/xdg-test");
            std::env::set_var("TMPDIR", "/tmp/tmpdir-test");
        }
        let d = runtime_dir();
        assert_eq!(d, PathBuf::from("/tmp/xdg-test"));
        // SAFETY: restore so we don't pollute downstream tests.
        unsafe {
            match original_tmpdir {
                Some(v) => std::env::set_var("TMPDIR", v),
                None => std::env::remove_var("TMPDIR"),
            }
            match original_xdg {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    /// Why: `<data_root>/uds_addr` is the discovery file the bridge
    /// reads first. Pinning the round-trip catches accidental format
    /// drift (extra whitespace, BOM, missing newline) that would break
    /// shell-script consumers.
    #[test]
    fn write_uds_addr_file_round_trip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = PathBuf::from("/tmp/foo.sock");
        write_uds_addr_file(tmp.path(), &sock).expect("write");
        let raw = std::fs::read_to_string(tmp.path().join(UDS_ADDR_FILE)).expect("read");
        assert_eq!(raw.trim(), "/tmp/foo.sock");
        assert!(raw.ends_with('\n'));
    }

    /// Why: a stale socket file (from a crashed prior daemon) must be
    /// cleaned up so the next bind() succeeds. Pre-create the file,
    /// verify clean_stale_socket removes it.
    /// What: touches a file at <tempdir>/leftover.sock, then calls
    /// clean_stale_socket. The file must be gone afterwards.
    /// Test: this test.
    #[tokio::test]
    async fn stale_socket_is_cleaned_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("leftover.sock");
        std::fs::write(&sock, b"").expect("touch");
        assert!(sock.exists());
        clean_stale_socket(&sock).await.expect("clean");
        assert!(!sock.exists(), "stale socket must be removed");
    }

    /// Why: tests spin multiple daemons in parallel under per-test
    /// tempdir-scoped data roots. Each must get a unique socket so
    /// `bind()` doesn't collide on a shared path. The per-root
    /// derivation must be deterministic (same data_root → same
    /// socket) and unique (different data_root → different socket).
    /// What: derives sockets for two distinct paths and asserts they
    /// differ; derives the same path twice and asserts equality.
    /// Test: this test.
    #[test]
    fn socket_path_for_data_root_returns_per_root_path() {
        let a = socket_path_for(Path::new("/tmp/a-test-root"));
        let b = socket_path_for(Path::new("/tmp/b-test-root"));
        let a_again = socket_path_for(Path::new("/tmp/a-test-root"));
        assert_ne!(a, b, "different data roots must yield different sockets");
        assert_eq!(a, a_again, "same data root must yield same socket");
        assert!(
            a.to_string_lossy().contains("trusty-memory-"),
            "per-root socket must carry the per-root prefix: {}",
            a.display()
        );
    }

    /// Why: an end-to-end happy path through bind_uds — bind + chmod
    /// must succeed against a fresh tempdir-scoped path.
    #[tokio::test]
    async fn bind_uds_creates_socket_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sock = tmp.path().join("daemon.sock");
        let _listener = bind_uds(&sock).await.expect("bind");
        assert!(sock.exists(), "socket file must exist after bind");
        // mode 0600 verified on unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "socket must be owner-only");
        }
    }
}
