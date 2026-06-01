# trusty-analyze

[![crates.io](https://img.shields.io/crates/v/trusty-analyze.svg)](https://crates.io/crates/trusty-analyze)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Sidecar code-analysis daemon for [trusty-search](../trusty-search). Fetches chunk
corpora from the trusty-search daemon, runs static analysis, and serves results via
HTTP (port 7879) and MCP stdio.

## 📚 Documentation

Full documentation lives at the workspace top level in
[`docs/trusty-analyze/`](../../docs/trusty-analyze/): the
[research](../../docs/trusty-analyze/research/),
[sessions](../../docs/trusty-analyze/sessions/), and
[regression-testing](../../docs/trusty-analyze/regression-testing/) subdirs.
This README and the rustdoc stay in-crate; everything else lives under `docs/`.

## Installation

### Standard install (macOS / modern Linux, glibc ≥ 2.38)

```bash
cargo install trusty-analyze
```

The default build bundles a prebuilt ONNX Runtime (via `fastembed/ort-download-binaries`)
for the neural concept-clustering embedder. This is the correct choice for macOS and
any Linux host with glibc 2.38 or later.

### Amazon Linux 2023 / glibc < 2.38 (system ORT)

The bundled ORT static library requires glibc 2.38+. On AL2023 (glibc 2.34) or any
other host with an older glibc, install with the `load-dynamic` feature and point the
binary at the system ONNX Runtime at runtime:

```bash
cargo install trusty-analyze --no-default-features --features http-server,load-dynamic
```

Then set `ORT_DYLIB_PATH` to the system `libonnxruntime.so` before starting the daemon:

```bash
export ORT_DYLIB_PATH=/usr/local/lib/libonnxruntime.so.1.24.2
trusty-analyze serve --search-url http://127.0.0.1:7878
```

If no system ORT is available, install without any ORT backend. The daemon will still
run with the deterministic BoW embedder (no semantic clustering):

```bash
cargo install trusty-analyze --no-default-features --features http-server
```

The installed binary is named `trusty-analyze`. The crate name on crates.io is
`trusty-analyze`.

## Quick Start

```bash
# trusty-search must be running first (hard runtime dependency)
trusty-search daemon

# Run the analyzer sidecar
trusty-analyze serve --search-url http://127.0.0.1:7878

# Analyze a named index
trusty-analyze analyze <index-id> --top-k 20

# Check liveness
trusty-analyze health
```

## Features

- Cyclomatic and cognitive complexity per chunk, file, and index
- Code smell detection with configurable thresholds and named categories
- Quality grade aggregation (A–F) per file and per index
- Git blame temporal decay scoring (stale high-complexity code surfaces first)
- Concept clustering (k-means over embeddings, BoW or neural)
- Facts store: `(subject, predicate, object)` knowledge triples, persisted in redb
- SCIP protobuf ingest for LSP-quality symbol data
- Full HTTP API + MCP stdio server (every endpoint has a tool equivalent)

## Claude Code Integration

Add to your project's `.mcp.json`:

```json
{
  "mcpServers": {
    "trusty-analyzer": {
      "command": "trusty-analyze",
      "args": ["serve", "--mcp"],
      "env": {}
    }
  }
}
```

`trusty-search` must already be running. The analyzer performs a startup health
check against `http://127.0.0.1:7878/health` and exits with code 1 if
unreachable.

## MCP Tools

The MCP server registers **17 tools** (authoritative source: `src/mcp/mod.rs`
`tool_definitions`):

| Tool | HTTP equivalent |
|------|-----------------|
| `analyzer_health` | `GET /health` |
| `complexity_hotspots` | `GET /indexes/:id/complexity_hotspots` |
| `find_smells` | `GET /indexes/:id/smells` |
| `analyze_quality` | `GET /indexes/:id/quality` |
| `run_diagnostics` | (composite diagnostics run) |
| `list_facts` | `GET /facts` |
| `upsert_fact` | `POST /facts` |
| `delete_fact` | `DELETE /facts/:id` |
| `ingest_scip` | `POST /indexes/:id/scip` |
| `cluster_concepts` | `GET /indexes/:id/clusters` |
| `extract_graph` | knowledge-graph extraction |
| `extract_ner` | named-entity extraction (optional ONNX) |
| `list_entities` | enumerate extracted entities |
| `suggest_refactors` | refactor suggestions |
| `review_diff` | review a unified diff |
| `review_github_pr` | review a GitHub pull request |
| `deep_analysis` | combined deep-analysis pass |

## HTTP API

Port 7879. Requires trusty-search on port 7878.

```
GET  /health
GET  /indexes/:id/complexity_hotspots[?top_k=N]
GET  /indexes/:id/smells[?category=<name>]
GET  /indexes/:id/quality
GET  /indexes/:id/clusters?k=N&method=bow|neural
GET  /facts[?subject=<s>&predicate=<p>]
POST /facts
DELETE /facts/:id
POST /indexes/:id/scip
```

## Deep-Analysis LLM Pass

`POST /analyze/deep` (and the `deep_analysis` MCP tool) generate a prose
narrative for an analyzed index using an LLM. The provider is selected by
the `TRUSTY_LLM_MODEL` environment variable.

### Using OpenRouter (default)

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
export TRUSTY_LLM_MODEL=openai/gpt-4o-mini   # default; override as needed
trusty-analyze serve --search-url http://127.0.0.1:7878
```

### Using AWS Bedrock

Set the model id with the `bedrock/` prefix. No OpenRouter key is required —
auth uses the standard AWS credential chain (env vars, `~/.aws/credentials`,
IAM role, SSO).

```bash
# Claude Sonnet 4.6 via cross-region inference profile (recommended):
# Note: Sonnet 4.6 drops the date stamp and -v1:0 suffix from the profile id.
export TRUSTY_LLM_MODEL=bedrock/us.anthropic.claude-sonnet-4-6

# AWS credentials (any supported form):
export AWS_ACCESS_KEY_ID=...
export AWS_SECRET_ACCESS_KEY=...
export AWS_REGION=us-east-1           # or: export TRUSTY_AWS_REGION=eu-west-1

trusty-analyze serve --search-url http://127.0.0.1:7878
```

When the model id starts with `bedrock/`, the daemon routes the LLM call
through `aws-sdk-bedrockruntime`'s `Converse` endpoint rather than OpenRouter.
The rest of the deep-analysis pipeline (prompt construction, narrative
accumulation, recommendations extraction) is identical.

#### Bedrock environment variables

| Variable | Default | Description |
|---|---|---|
| `TRUSTY_LLM_MODEL` | `openai/gpt-4o-mini` | Model id. Prefix with `bedrock/` to select AWS Bedrock. |
| `TRUSTY_AWS_REGION` | — | AWS region for Bedrock calls (takes priority over `AWS_REGION`). |
| `AWS_REGION` | `us-east-1` | Fallback AWS region (standard env var). |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | — | Static AWS credentials. Alternatives: `AWS_PROFILE`, IAM role, SSO. |

## Configuration

| Variable | Default | Description |
|---|---|---|
| `TRUSTY_SEARCH_URL` | `http://127.0.0.1:7878` | trusty-search daemon address |
| `TRUSTY_ANALYZER_PORT` | `7879` | Analyzer listen port |
| `RUST_LOG` | `warn` | Tracing filter |

## Feature Flags

| Flag | Description |
|---|---|
| `http-server` | Axum HTTP daemon (enabled by default). Required for the `trusty-analyze` binary. |
| `bundled-ort` | **Default.** Bundle the static ONNX Runtime libs via fastembed. Requires glibc ≥ 2.38. |
| `load-dynamic` | Load ONNX Runtime dynamically from `ORT_DYLIB_PATH`. Use on glibc < 2.38 (AL2023). Mutually exclusive with `bundled-ort`. |
| `cuda` | GPU-accelerated embedding via ONNX Runtime CUDA EP. Always pair with `--no-default-features`. |
| `ner` | Optional ONNX-backed named entity recognition (separate model file required). |

### ORT backend selection summary

| Host | Recommended install command |
|---|---|
| macOS, Linux glibc ≥ 2.38 | `cargo install trusty-analyze` (default, bundled ORT) |
| Amazon Linux 2023 / glibc 2.34 | `cargo install trusty-analyze --no-default-features --features http-server,load-dynamic` + `ORT_DYLIB_PATH` |
| No ONNX Runtime available | `cargo install trusty-analyze --no-default-features --features http-server` (BoW fallback) |
| CUDA GPU | `cargo install trusty-analyze --no-default-features --features http-server,cuda` + `ORT_DYLIB_PATH` |

## Architecture

The crate is a single `trusty-analyze` package containing the CLI binary
(`trusty-analyze`) and a library. All analysis engines, the HTTP server, and the
MCP stdio server live within this one crate. Shared types (complexity metrics,
code smells, knowledge-graph entities, facts) come from `trusty-common`.

```
trusty-search (port 7878)                trusty-analyze (port 7879)
  GET /indexes/:id/chunks  ──────────►   complexity analysis (tree-sitter)
  (bulk corpus export)                   blame + temporal decay
                                         quality grade aggregation
                                         k-means concept clustering
                                         facts store (redb)
                                         axum HTTP API + MCP stdio
```

## Development

```bash
# Build
cargo build -p trusty-analyze

# Test
cargo test -p trusty-analyze

# Lint
cargo clippy -p trusty-analyze --all-targets -- -D warnings
```

See [CLAUDE.md](./CLAUDE.md) for full architecture, API reference, and project history.

## License

Licensed under the [MIT License](https://opensource.org/licenses/MIT).

## Repository

<https://github.com/bobmatnyc/trusty-tools>
