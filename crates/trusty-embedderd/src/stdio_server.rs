//! Stdio JSON-RPC server mode for `trusty-embedderd --stdio`.
//!
//! Why: when `trusty-search` spawns `trusty-embedderd` as a sidecar with
//! piped stdin/stdout, there is no need for a socket file or a port. The
//! parent owns the pipe handles and the OS manages the lifecycle — when the
//! parent closes its write end of the pipe, the child's `read_line` returns
//! EOF and the sidecar exits cleanly. This is the same transport pattern MCP
//! uses throughout the project.
//!
//! What: `run_stdio_server` reads newline-framed JSON-RPC 2.0 requests from
//! stdin and writes responses to stdout. It reuses the same dispatch logic
//! (via `uds_server::dispatch_request`) as the UDS server so the two
//! transports are always in sync. All tracing goes to stderr — stdout is
//! reserved exclusively for JSON-RPC frames.
//!
//! Critical invariant: **never write anything to stdout except JSON-RPC
//! frames**. Any stray `println!` will corrupt the protocol and cause the
//! parent's `StdioEmbedderClient` to fail with a JSON parse error.
//!
//! Test: `stdio_eof_terminates_cleanly` (unit, no daemon required);
//! `bit_identical_stdio` in `tests/bit_identical.rs` (ONNX, `#[ignore]`).

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::batch_queue::BatchQueue;
use crate::uds_server::dispatch_request;

/// Run the stdio JSON-RPC server loop.
///
/// Why: the sidecar transport for the auto-spawn path. Processes requests
/// until stdin closes (parent terminated) or an unrecoverable write error
/// occurs.
///
/// What: wraps `stdin()` in a `BufReader`, reads newline-terminated frames,
/// calls `dispatch_request` (shared with the UDS transport), writes one
/// newline-terminated response frame to `stdout()`. Exits cleanly on EOF.
///
/// Test: `cargo test -p trusty-embedderd` (unit tests). `bit_identical_stdio`
/// in `tests/bit_identical.rs` (ONNX model required, `#[ignore]`).
pub async fn run_stdio_server(queue: Arc<BatchQueue>) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut writer = stdout;
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .context("read JSON-RPC frame from stdin")?;

        if n == 0 {
            // EOF — parent closed the write end of the pipe. Exit cleanly.
            tracing::info!("trusty-embedderd --stdio: stdin closed (parent terminated) — exiting");
            return Ok(());
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = dispatch_request(trimmed, &queue).await;

        let mut payload =
            serde_json::to_vec(&response).context("serialise JSON-RPC response to stdout")?;
        payload.push(b'\n');

        writer
            .write_all(&payload)
            .await
            .context("write JSON-RPC response to stdout")?;

        writer
            .flush()
            .await
            .context("flush stdout after JSON-RPC response")?;
    }
}
