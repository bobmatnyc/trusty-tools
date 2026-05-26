# trusty-common

[![crates.io](https://img.shields.io/crates/v/trusty-common.svg)](https://crates.io/crates/trusty-common)
[![License: Elastic-2.0](https://img.shields.io/badge/License-Elastic--2.0-blue.svg)](https://www.elastic.co/licensing/elastic-license)

Shared utility surface for the `trusty-*` AI tooling ecosystem. This crate is
the result of consolidating several formerly separate crates into one:
`trusty-mcp-core`, `trusty-rpc`, `trusty-embedder`, and `trusty-symgraph` have
all been absorbed here.

Each subsystem is feature-gated so consumers only pay for what they use.

## Installation

```toml
[dependencies]
trusty-common = "0.3"
```

With optional features:

```toml
trusty-common = { version = "0.3", features = ["axum-server", "mcp", "rpc", "embedder"] }
```

## Feature Flags

| Feature | Description |
|---|---|
| `axum-server` | Standard axum middleware stack (CORS, trace, gzip) + fast-fail reqwest client |
| `mcp` | JSON-RPC 2.0 / MCP primitives (formerly `trusty-mcp-core`) |
| `rpc` | General-purpose JSON-RPC client + stdio/HTTP transports (formerly `trusty-rpc`) |
| `embedder` | `Embedder` trait + `FastEmbedder` (formerly `trusty-embedder`) |
| `embedder-bundled-ort` | Bundle ONNX Runtime static libs — best for macOS and modern Linux |
| `embedder-cuda` | GPU-accelerated embedding via CUDA execution provider |
| `embedder-load-dynamic` | Dynamic ORT loading for AL2023 / glibc < 2.38 builds |
| `embedder-test-support` | Expose `MockEmbedder` outside `cfg(test)` for downstream tests |
| `embedder-client` | `EmbedderClient` trait + `InProcess` / `Remote` (HTTP) / `Uds` clients for `trusty-embedderd` (formerly the standalone `trusty-embedder-client` crate; UDS path supersedes the retired `embed_client` module from #157) |
| `symgraph` | Contracts surface only: `EntityType`, `RawEntity`, `EdgeKind` — no tree-sitter |
| `symgraph-parser` | Full symbol graph: tree-sitter grammars, `SymbolGraph`, emitter, editor |
| `symgraph-server` | HTTP server frontend for the symbol graph (implies `symgraph-parser`) |

By default the crate is dependency-light: `tokio`, `serde`, `reqwest`, and
`tracing`. Pull in only the features you need.

## What's Included

### Port Binding

`bind_with_auto_port` walks forward through ports when the requested one is
busy, so daemon restarts don't fail noisily.

```rust
use trusty_common::bind_with_auto_port;

let addr = "127.0.0.1:7878".parse()?;
let listener = bind_with_auto_port(addr, 10).await?;
println!("listening on {}", listener.local_addr()?);
```

### Data Directory

`resolve_data_dir(app_name)` resolves an OS-appropriate per-application
directory (`~/Library/Application Support/` on macOS, `~/.local/share/` on
Linux) and creates it if missing.

```rust
use trusty_common::resolve_data_dir;

let dir = resolve_data_dir("trusty-search")?;
// ~/Library/Application Support/trusty-search  (macOS)
// ~/.local/share/trusty-search                  (Linux)
```

### Daemon Address File

`write_daemon_addr` / `read_daemon_addr` persist and retrieve a running
daemon's bound `host:port` so MCP clients and follow-up CLI invocations can
find it without hardcoding a port.

### Tracing / CLI Init

```rust
use trusty_common::init_tracing;

// 0=warn, 1=info, 2=debug, 3=trace; RUST_LOG overrides
init_tracing(1);
```

Logs always go to **stderr** — stdout stays clean for MCP JSON-RPC framing.

### Chat Providers (OpenRouter + Ollama)

Provider-agnostic streaming chat via the `ChatProvider` trait. Supports
OpenRouter and Ollama out of the box; auto-detects a local Ollama instance.

```rust
use trusty_common::{OpenRouterProvider, ChatProvider, ChatEvent};

let provider = OpenRouterProvider::new(api_key, "anthropic/claude-3.5-sonnet".into());
let messages = vec![/* ... */];
let mut stream = provider.chat_stream(messages, None).await?;
while let Some(event) = stream.next().await {
    // handle ChatEvent::Delta, ChatEvent::Done, ChatEvent::ToolCall
}
```

### axum Middleware Stack (`axum-server` feature)

```rust
use trusty_common::server::with_standard_middleware;

let app = Router::new().route("/", get(handler));
let app = with_standard_middleware(app); // CORS + trace + gzip (SSE-safe)
```

Middleware order: `CorsLayer` → `TraceLayer` → `CompressionLayer` (gzip,
`text/event-stream` excluded for SSE compatibility).

### MCP / JSON-RPC Primitives (`mcp` feature)

Formerly the `trusty-mcp-core` crate. Provides the shared envelope types and
a ready-made stdio dispatch loop for MCP servers.

```rust
use trusty_common::mcp::{Request, Response, initialize_response, run_stdio_loop};

// Build the initialize response for your server
let result = initialize_response("my-server", "0.1.0", None);

// Run a full JSON-RPC 2.0 stdio loop
run_stdio_loop(|req: Request| async move {
    // dispatch and return a Response
    Response::ok(req.id, serde_json::json!({"result": "ok"}))
}).await?;
```

Key exports: `Request`, `Response`, `JsonRpcError`, `initialize_response`,
`run_stdio_loop`, `error_codes`.

### RPC Client (`rpc` feature)

Formerly the library half of the `trusty-rpc` crate. General-purpose
JSON-RPC 2.0 client with stdio-subprocess and HTTP transports.

```rust
use trusty_common::rpc::{RpcClient, StdioTransport};

let transport = StdioTransport::spawn("trusty-search", &["serve"])?;
let client = RpcClient::new(transport);
let result = client.call("search", serde_json::json!({"q": "fn auth"})).await?;
```

### Embeddings (`embedder` feature)

Formerly the `trusty-embedder` crate. Text-embedding pipeline backed by
`fastembed-rs` using the `AllMiniLML6V2Q` INT8 quantized model (384-dim,
~22 MB). LRU cache, ORT warmup, and CoreML acceleration on Apple Silicon are
all included.

```rust
use trusty_common::embedder::FastEmbedder;

let embedder = FastEmbedder::new().await?;
let vecs = embedder.embed(&["fn authenticate", "user login"]).await?;
// vecs: Vec<Vec<f32>>, each inner Vec is 384-dim
```

- **Model**: `AllMiniLML6V2Q` (INT8 quantized, ~22 MB)
- **Fallback**: `AllMiniLML6V2` (full-precision, ~86 MB) when quantized unavailable
- **Output dimension**: 384
- **Acceleration**: CoreML (Apple Silicon, auto-detected), CUDA (via `embedder-cuda`)

For tests, use `MockEmbedder` (requires the `embedder-test-support` feature):

```rust
use trusty_common::embedder::MockEmbedder;

let embedder = MockEmbedder::new(384); // deterministic zero vectors
```

### Embedder Clients (`embedder-client` feature)

Formerly the standalone `trusty-embedder-client` crate (MIT) — folded into
trusty-common under Elastic-2.0 in #166 and unified with the retired
`embed_client` UDS module in #164. Provides a single `EmbedderClient` trait
with three implementations sharing one `EmbedRequest` / `EmbedResponse` wire
format:

```rust
use trusty_common::embedder_client::{
    EmbedderClient, InProcessEmbedderClient, RemoteEmbedderClient, UdsEmbedderClient,
};
use std::sync::Arc;

// 1. In-process: wraps a local FastEmbedder. Use when you want zero IPC.
let inproc = InProcessEmbedderClient::from_arc(Arc::new(local_embedder));

// 2. HTTP-remote: talks to a trusty-embedderd over HTTP. Use for cross-host.
let http = RemoteEmbedderClient::new("http://127.0.0.1:7890")?;

// 3. UDS-remote: talks to a trusty-embedderd over a Unix domain socket.
//    Use for on-host callers that want lower latency than HTTP.
let uds = UdsEmbedderClient::new(UdsEmbedderClient::default_path());

// All three satisfy the same trait, so swapping is a one-line change:
let client: Arc<dyn EmbedderClient> = Arc::new(uds);
let vectors = client.embed_batch(vec!["fn auth".into()]).await?;
```

`UdsEmbedderClient` speaks newline-framed JSON-RPC 2.0 — wire-compatible with
the protocol previously served by the retired `trusty-embed-daemon`. The
default socket path (`UdsEmbedderClient::default_path()`) resolves to
`$TMPDIR/trusty-embedderd.sock`, matching the daemon's default. See
`crates/trusty-embedderd/README.md` for daemon-side setup.

### Symbol Graph (`symgraph` / `symgraph-parser` features)

Formerly the `trusty-symgraph` crate. A tree-sitter–powered symbol graph
engine for source-code analysis.

The `symgraph` feature exposes only the pure data contracts — no tree-sitter
dependency, no `links` conflict:

```rust
use trusty_common::symgraph::{EntityType, EdgeKind, RawEntity};
```

The `symgraph-parser` feature adds the full parse → registry → emit stack
(tree-sitter grammars for Rust, Python, JS/TS, Go, Java, C, C++):

```rust
use trusty_common::symgraph::{SymbolGraph, SymbolRegistry};

let graph = SymbolGraph::parse_file("src/main.rs")?;
```

**Important**: `symgraph-parser` brings in the `links = "tree-sitter"` native
library slot. Enable it in at most one crate per build graph (typically
`open-mpm`). Downstream crates that only need the data types enable `symgraph`
(contracts only) and stop there.

## Development

```bash
# Check the crate (no feature flags)
cargo check -p trusty-common

# Test all features
cargo test -p trusty-common --features axum-server,mcp,rpc,symgraph

# Test embedder (ONNX-backed tests are #[ignore] by default)
cargo test -p trusty-common --features embedder -- --include-ignored
```

## License

Licensed under the [Elastic License 2.0](https://www.elastic.co/licensing/elastic-license).

## Repository

<https://github.com/bobmatnyc/trusty-tools>
