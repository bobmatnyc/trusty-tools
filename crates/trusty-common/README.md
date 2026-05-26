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
| `symgraph` | Contracts surface only: `EntityType`, `RawEntity`, `EdgeKind` — no tree-sitter |
| `symgraph-parser` | Full symbol graph: tree-sitter grammars, `SymbolGraph`, emitter, editor |
| `symgraph-server` | HTTP server frontend for the symbol graph (implies `symgraph-parser`) |
| `migrations` | Reusable schema-migration kernel: `SchemaVersion`, `Migration`, `MigrationRunner`, file-stamp helpers |

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

## Migrations (`migrations` feature)

Shared schema-migration kernel (issue #179) for long-lived trusty-* stores.
Replaces the ad-hoc "if schema_version < N { … }" branches in trusty-search
and trusty-memory with one ordered runner that stamps a `SchemaVersion`
after each successful step.

```rust
use trusty_common::migrations::{
    Migration, MigrationRunner, SchemaVersion,
    file_stamp::{read_version_from_file, write_version_to_file},
};
use anyhow::Result;
use std::path::Path;

struct DropLegacyTables;
impl Migration<MyStore> for DropLegacyTables {
    fn from_version(&self) -> SchemaVersion { SchemaVersion::UNVERSIONED }
    fn label(&self) -> &'static str { "drop legacy tables" }
    fn apply(&self, store: &MyStore) -> Result<()> { /* … */ Ok(()) }
}

let runner = MigrationRunner::new(vec![Box::new(DropLegacyTables)]);
let stamp_path = Path::new("/var/lib/my-store/schema_version.json");
let current = read_version_from_file(stamp_path)?;
runner.run(&store, current, |v| write_version_to_file(stamp_path, v))?;
```

The `file_stamp` module owns the JSON-sidecar stamp format
(`{ "schema_version": <u32> }`, written atomically via temp + rename). Stores
that already depend on redb should use the recipe documented in the
`migrations::redb_stamp` module (kept doc-only here so this feature adds
zero new dependencies).

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
