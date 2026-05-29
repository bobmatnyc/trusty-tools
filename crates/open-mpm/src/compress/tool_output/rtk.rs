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
/// `compress_tool_output` (synchronous, native) on `None`.
/// Test: `compress_tool_output_async_falls_back_when_rtk_absent`.
pub async fn compress_tool_output_async(tool_name: &str, output: &str) -> String {
    if let Some(s) = compress_via_rtk(tool_name, output).await {
        return s;
    }
    compress_tool_output(tool_name, output)
}
