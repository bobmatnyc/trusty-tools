//! RTK subprocess delegation + the async compression wrapper.
//!
//! Why: When the user has installed RTK (https://github.com/rtk-ai/rtk),
//! delegating to it gets us the upstream implementation for free. When `rtk`
//! is absent we fall back to the native filter chain.
//! What: `compress_via_rtk` (subprocess), `which` (PATH probe), and the
//! `compress_tool_output_async` wrapper that prefers RTK then falls back.
//! Test: `compress_via_rtk_returns_none_when_binary_absent`,
//! `compress_tool_output_async_falls_back_when_rtk_absent` in `tool_output::tests`.

use super::compress_tool_output;

/// Which code path produced a compressed tool output.
///
/// Why: Issue #1956's `tm compress` stats logging needs to distinguish an
/// `rtk`-binary compression from the always-available native fallback chain
/// so aggregate savings can be broken down by path, matching the spike doc's
/// "compression path: native fallback chain (rtk NOT on PATH)" framing
/// (`docs/specs/tool-output-interception-seam.md`).
/// What: `RtkBinary` when the external `rtk` subprocess produced the result;
/// `NativeFallback` when the in-tree filter chain (`compress_tool_output`)
/// did, either because `rtk` is absent from `PATH` or its invocation failed.
/// Test: `compress_tool_output_async_with_path_reports_native_fallback_when_rtk_absent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionPath {
    /// The external `rtk` CLI subprocess compressed the output.
    RtkBinary,
    /// The in-tree native filter chain (`compress_tool_output`) compressed
    /// the output, either because `rtk` is not installed or it failed.
    NativeFallback,
}

impl CompressionPath {
    /// Stable, lowercase machine-parseable name for structured log fields.
    ///
    /// Why: `tm compress`'s stats log (issue #1956) emits this as the
    /// `compression_path` tracing field; a stable string constant keeps log
    /// consumers from having to special-case `Debug` formatting.
    /// What: `"rtk_binary"` / `"native_fallback"`.
    /// Test: `compression_path_as_str_is_stable`.
    pub fn as_str(self) -> &'static str {
        match self {
            CompressionPath::RtkBinary => "rtk_binary",
            CompressionPath::NativeFallback => "native_fallback",
        }
    }
}

/// Pipe `output` through the `rtk` CLI subprocess if installed.
///
/// Why: When the user has installed RTK (https://github.com/rtk-ai/rtk),
/// delegating to it gets us the upstream implementation for free, with
/// updates from the source project. When `rtk` is not on `PATH` we fall
/// back to the native filter.
/// What: Spawns `rtk <tool_name>`, writes `output` to stdin, returns stdout.
/// Returns `None` on any failure (missing binary, non-zero exit, stdin/stdout
/// IO error, decode error) so the caller can fall back gracefully.
/// Test: Covered by integration tests when `rtk` is available; unit tests
/// only verify the `None` path when the binary is absent.
pub async fn compress_via_rtk(tool_name: &str, output: &str) -> Option<String> {
    use tokio::io::AsyncWriteExt;
    use tokio::process::Command;

    // Quick existence check — if `rtk` is not on PATH, skip without spawning.
    which("rtk")?;

    let mut child = Command::new("rtk")
        .arg(tool_name)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    if let Some(mut stdin) = child.stdin.take() {
        // Write output and close stdin so rtk can finish.
        stdin.write_all(output.as_bytes()).await.ok()?;
        drop(stdin);
    }

    let out = child.wait_with_output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Look up an executable on `PATH`. Returns the absolute path if found.
///
/// Why: Avoids depending on the `which` crate while letting us short-circuit
/// when the binary is absent.
/// What: Splits `$PATH` (or `;`-separated on Windows), checks `dir/name`
/// (and `name.exe` on Windows). Returns the first existing match.
fn which(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let with_ext = dir.join(format!("{name}.exe"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

/// Compress a tool's output, trying the RTK subprocess first and falling
/// back to the native filter chain.
///
/// Why: Most users won't have RTK installed; the native filters are always
/// available. When RTK is present we delegate so we stay aligned with upstream.
/// What: Async wrapper — calls `compress_via_rtk`, falls back to
/// `compress_tool_output` (synchronous, native) on `None`. Thin wrapper over
/// [`compress_tool_output_async_with_path`] that discards the path signal,
/// kept as the original stable signature so existing callers (`trusty-agents`'s
/// `llm::tool_loop`) are unaffected by the #1959 hoist.
/// Test: `compress_tool_output_async_falls_back_when_rtk_absent`.
pub async fn compress_tool_output_async(tool_name: &str, output: &str) -> String {
    compress_tool_output_async_with_path(tool_name, output)
        .await
        .0
}

/// Compress a tool's output like [`compress_tool_output_async`], additionally
/// reporting which code path produced the result.
///
/// Why: Issue #1956's `tm compress` subcommand needs a `compression_path`
/// field for its structured stats log so compression effectiveness can be
/// broken down by rtk-binary vs. native-fallback — see [`CompressionPath`].
/// Splitting this out (rather than changing `compress_tool_output_async`'s
/// signature) keeps every existing call site source-compatible.
/// What: Tries `compress_via_rtk` first; returns `(text, RtkBinary)` on
/// success, else `(compress_tool_output(..), NativeFallback)`.
/// Test: `compress_tool_output_async_with_path_reports_native_fallback_when_rtk_absent`.
pub async fn compress_tool_output_async_with_path(
    tool_name: &str,
    output: &str,
) -> (String, CompressionPath) {
    if let Some(s) = compress_via_rtk(tool_name, output).await {
        return (s, CompressionPath::RtkBinary);
    }
    (
        compress_tool_output(tool_name, output),
        CompressionPath::NativeFallback,
    )
}
