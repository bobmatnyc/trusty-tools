# trusty-analyze

[![crates.io](https://img.shields.io/crates/v/trusty-analyze.svg)](https://crates.io/crates/trusty-analyze)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Sidecar code-analysis daemon for [trusty-search](../trusty-search). Fetches chunk
corpora from the trusty-search daemon, runs static analysis, and serves results as
JSON-RPC over a Unix socket and via MCP stdio.

> **#6287 (ADR-0032): the daemon no longer binds `127.0.0.1:7879`.** It serves
> `<data dir>/trusty-analyze/trusty-analyze.sock`, which every consumer derives
> through the same `trusty_common::daemon_socket_path` call rather than reading
> a written-down address. `trusty-analyze port` became
> [`trusty-analyze socket`](#socket-discovery-trusty-analyze-socket); the `/sse`
> stream and the `--mcp-port` HTTP/SSE listener are deleted, not moved.

## 📚 Documentation

Full documentation lives at the workspace top level in
[`docs/trusty-analyze/`](../../docs/trusty-analyze/): the
[research](../../docs/trusty-analyze/research/),
[sessions](../../docs/trusty-analyze/sessions/), and
[regression-testing](../../docs/trusty-analyze/regression-testing/) subdirs.
This README and the rustdoc stay in-crate; everything else lives under `docs/`.

## Prerequisites

> **trusty-analyze requires a running `trusty-search` daemon.**
>
> The analyzer performs a startup health check against `http://127.0.0.1:7878/health`
> (or the URL given by `--search-url`) and exits with code 1 if that check fails.
> There is no standalone or offline mode. **Start `trusty-search` before starting
> `trusty-analyze`.**
>
> Install trusty-search: `cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-search --locked`
> or see [trusty-search's README](../trusty-search/README.md) for prebuilt binaries.

## Installation

### Install from prebuilt binary

Prebuilt binaries for **macOS arm64** and **Linux x86_64** are published on the
[GitHub Releases page](https://github.com/bobmatnyc/trusty-tools/releases) under
tags of the form `trusty-analyze-v<version>`.

```bash
# Replace this with the version shown on the release page.
VERSION=x.y.z
curl -fsSL \
  "https://github.com/bobmatnyc/trusty-tools/releases/download/trusty-analyze-v${VERSION}/trusty-analyze-aarch64-apple-darwin.tar.gz" \
  | tar xz
sudo mv trusty-analyze /usr/local/bin/

# Linux x86_64 uses the same VERSION value.
curl -fsSL \
  "https://github.com/bobmatnyc/trusty-tools/releases/download/trusty-analyze-v${VERSION}/trusty-analyze-x86_64-unknown-linux-gnu.tar.gz" \
  | tar xz
sudo mv trusty-analyze /usr/local/bin/
```

Check the Releases page for the exact artifact names for your version.

### Install with cargo

**Standard install — every supported host:**

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze --locked
```

The build links no ONNX Runtime and downloads no model. Concept clustering uses a
deterministic hashed bag-of-words embedder.

> **#5067:** earlier releases defaulted to `bundled-ort`, which pulled in a
> fastembed/ONNX neural clustering embedder. No caller ever selected it, yet the
> daemon constructed it at every boot and the untimed Hugging Face request that
> construction made blocked startup for as long as the request took — 31m46s in
> one measured production boot. The embedder, the `bundled-ort` / `load-dynamic`
> / `cuda` features, and the separate Amazon Linux 2023 install path are all gone.


The installed binary is named `trusty-analyze`.

### With Homebrew (recommended)

```bash
brew tap bobmatnyc/trusty
brew install trusty-analyze
```

Or install directly without tapping:

```bash
brew install bobmatnyc/trusty/trusty-analyze
```

Homebrew provides:
- Automatic updates via `brew upgrade trusty-analyze`
- Standard macOS / Linux PATH integration
- Easy dependency management

## Quick Start

```bash
# trusty-search must be running first (hard runtime dependency)
trusty-search start

# Analyze a named index — the server starts itself if it is not up
trusty-analyze analyze <index-id> --top-k 20

# Check liveness
trusty-analyze health
```

### Running the server (#6350)

**You normally do not.** trusty-analyze runs ON DEMAND: `trusty-analyze deep`,
`trusty-review report --analyze`, and `tctl up` each start it if nothing is
serving its socket, and it exits after ten minutes with no traffic. There is no
LaunchAgent and no `service install` — installing the binary is the whole setup.

Run it in the foreground only when you want to watch it:

```bash
trusty-analyze --search-url http://127.0.0.1:7878 serve
```

Two environment variables govern the lifecycle:

| Variable | Effect |
|---|---|
| `TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS` | Idle window in seconds. `0` disables the idle exit. Default `600`. |
| `TRUSTY_ANALYZE_EXTERNAL=1` | Clients never start a server; they dial whatever you are running. |

**Migrating off the old daemon.** Every machine that installed trusty-analyze
before this change has `com.trusty.analyze` loaded, and upgrading the binary
does not unload it. Evict it once:

```bash
trusty-analyze service uninstall
```

`trusty-analyze setup daemon`, `tctl install` and `tctl upgrade` run the same
eviction, so you get it either way. `tctl` drives it through the shared
`RETIRED_SERVICES` registry — the same pass that clears the retired
`com.trusty.review` unit — and a unit that will not go down fails that member's
install or upgrade rather than passing quietly.

### Ops health check

Probe the daemon without the CLI — useful for monitoring, systemd
`ExecStartPost`, or container readiness probes. There is no `curl` form any
more; a JSON-RPC frame over the socket is the whole exchange:

```bash
printf '{"jsonrpc":"2.0","id":1,"method":"analyze.health"}\n' \
  | nc -U "$(trusty-analyze socket)"
# → {"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"…","search_reachable":true}}
```

`search_reachable` reflects whether the upstream `trusty-search` daemon (port
7878, still HTTP) is responding; a `false` here means every analysis method
will fail even though the analyzer process itself is up, and `status` reads
`degraded` rather than `ok`.

### Socket discovery (`trusty-analyze socket`)

`socket` replaces `port` (#6287). The path is derived, not discovered, so it
resolves whether or not anything is listening — which is why this command also
reports liveness and exits non-zero when nothing answers, preserving the
property that `$(trusty-analyze socket)` fails rather than handing a caller a
path to a dead socket:

```bash
trusty-analyze socket         # bare path: /Users/x/…/trusty-analyze.sock
trusty-analyze socket --json  # JSON:      {"socket":"…","serving":true}
```

## Features

- Cyclomatic and cognitive complexity per chunk, file, and index
- Code smell detection with configurable thresholds and named categories
- Quality grade aggregation (A–F) per file and per index
- Git blame temporal decay scoring (stale high-complexity code surfaces first)
- Concept clustering (k-means over hashed bag-of-words embeddings)
- Facts store: `(subject, predicate, object)` knowledge triples, persisted in redb
- SCIP protobuf ingest for LSP-quality symbol data
- Full JSON-RPC surface over a Unix socket + MCP stdio server (every method has a tool equivalent)

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

<!-- BEGIN GENERATED: mcp-tools -->
The MCP server registers **21 tools** with default features, **24 tools** with `--features review`. Authoritative source: `trusty_analyze::mcp::tool_descriptors + trusty_analyze::mcp::descriptors::review_tool_descriptors` —
this table is generated from it, not maintained by hand.

| Tool | Available | Arguments | Summary |
|---|---|---|---|
| `analyze_quality` | always | `index?`, `index_id?` | Aggregate quality stats: avg cyclomatic, %A, smell count |
| `analyzer_health` | always | — | Probe analyzer daemon liveness and version |
| `cluster_concepts` | always | `index?`, `index_id?`, `k?`, `method?` | Group chunks into concept clusters using k-means over hashed bag-of-words embeddings |
| `complexity_distribution` | always | `index?`, `index_id?` | Full A-F cyclomatic-complexity histogram over the whole index corpus, with the counted total. |
| `complexity_hotspots` | always | `index?`, `index_id?`, `top_n?` | Top-N chunks ranked by cyclomatic complexity |
| `console_metrics` | always | — | Return health and operational metrics for trusty-console polling. |
| `deep_analysis` | always | `index_id`, `model?` | Run an LLM-augmented deep analysis pass over an index: synthesises a deterministic review report from the indexed corpus, looks up detected… |
| `delete_fact` | always | `id` | Delete a fact by its u64 id |
| `extract_graph` | always | `index?`, `index_id?`, `language?` | Build the multi-language knowledge graph (nodes + edges) for an index |
| `extract_ner` | always | `index?`, `index_id?`, `top_k?` | Extract named entities from doc comments for a code index using NER |
| `find_smells` | always | `index?`, `index_id?`, `limit?`, `offset?`, `omit_content?` | Chunks with at least one detected code smell. |
| `ingest_scip` | always | `scip_base64`, `index?`, `index_id?` | Ingest a SCIP (Scalable and Precise Index for Code) protobuf index for a given index_id, enriching the knowledge graph with fully-resolved… |
| `list_analyze_indexes` | always | — | List all indexes known to the trusty-analyze daemon. |
| `list_entities` | always | `index?`, `index_id?`, `kind?`, `language?` | List symbol-level entities (functions, classes, ...) for an index |
| `list_facts` | always | `object?`, `predicate?`, `subject?` | List canonical facts, optionally filtered by subject/predicate/object |
| `review_diff` | always | `diff`, `index_id` | Review a unified git diff and return a structured quality report (per-file complexity, code smells, grade A-F, recommendations). |
| `review_github_pr` | always | `owner`, `repo`, `pr`, `index_id`, `post_comment?` | Fetch a GitHub pull request's unified diff and run a structured quality review against a trusty-search index. |
| `run_diagnostics` | always | `index?`, `index_id?`, `language?`, `limit?`, `offset?`, `tools?` | Run available external static-analysis tools (clippy, ruff, biome, staticcheck, pmd, rubocop, phpstan, swiftlint, detekt, clang-tidy,… |
| `scip_status` | always | `index?`, `index_id?` | Report whether a SCIP overlay has been ingested for an index. |
| `suggest_refactors` | always | `file?`, `index?`, `index_id?`, `min_severity?`, `top_k?` | Suggest concrete refactoring actions (extract method, reduce nesting, ...) ranked by severity, derived from complexity metrics and code… |
| `tr_review_diff` | `--features review` | `diff`, `context?`, `reviewer_model?` | LLM-backed review of a raw unified diff string via the embedded trusty-review pipeline. |
| `tr_review_health` | `--features review` | — | Probe the embedded trusty-review pipeline's liveness and configuration (dry_run mode, reviewer model, dependency URLs). |
| `tr_review_pr` | `--features review` | `owner`, `repo`, `pr`, `reviewer_model?` | LLM-backed review of a GitHub pull request via the embedded trusty-review pipeline. |
| `upsert_fact` | always | `subject`, `predicate`, `object`, `index_id`, `confidence?`, `provenance?` | Insert or update a canonical fact triple |
<!-- END GENERATED: mcp-tools -->

### RPC equivalents

Parity rule: every RPC method has an MCP tool. This mapping is hand-written —
the method a tool forwards to lives in the dispatcher's match arms, not in the
descriptors, so the generator above cannot derive it. The tool names here are
cross-checked against the descriptors by `http_equivalents_name_only_real_tools`
in `tests/generated_docs.rs`, so the list cannot name a tool that does not exist,
and the method names are cross-checked against the router by
`mcp_names_the_methods_the_router_registers`.

#6287 replaced the URL column: these were HTTP routes on port 7879 until
ADR-0032 moved the daemon onto a socket.

| Tool | RPC method |
|------|-----------------|
| `analyzer_health` | `analyze.health` |
| `complexity_hotspots` | `analyze.complexity_hotspots` |
| `complexity_distribution` | `analyze.complexity_distribution` |
| `find_smells` | `analyze.smells` |
| `analyze_quality` | `analyze.quality` |
| `run_diagnostics` | `analyze.diagnostics` |
| `list_facts` | `analyze.facts_list` |
| `upsert_fact` | `analyze.facts_upsert` |
| `delete_fact` | `analyze.facts_delete` |
| `ingest_scip` | `analyze.scip_ingest` |
| `scip_status` | `analyze.scip_status` |
| `cluster_concepts` | `analyze.clusters` |
| `extract_graph` | `analyze.graph` |
| `extract_ner` | `analyze.ner` (optional ONNX) |
| `list_entities` | `analyze.entities` |
| `list_analyze_indexes` | `analyze.list_indexes` |
| `suggest_refactors` | `analyze.refactor_suggestions` |
| `review_diff` | `analyze.review` |
| `review_github_pr` | `analyze.review_github_pr` |
| `deep_analysis` | `analyze.deep_analysis` |
| `console_metrics` | `analyze.health` plus `analyze.list_indexes`, for the trusty-console dashboard |

## RPC Surface

JSON-RPC 2.0 over `<data dir>/trusty-analyze/trusty-analyze.sock`, one
newline-terminated frame per request. Requires trusty-search on port 7878.
`service::rpc::METHODS` is the authoritative list — the four crates that dial
these names by literal are checked against it by
`tests/uds_consumer_contract.rs`:

```
analyze.health                    analyze.graph
analyze.list_indexes              analyze.entities
analyze.complexity_hotspots       analyze.clusters
analyze.complexity_distribution   analyze.ner
analyze.smells                    analyze.scip_ingest
analyze.refactor_suggestions      analyze.scip_status
analyze.quality                   analyze.review
analyze.diagnostics               analyze.review_github_pr
analyze.facts_list                analyze.deep_analysis
analyze.facts_upsert              analyze.facts_delete
```

## Deep-Analysis LLM Pass

`POST /analyze/deep` (and the `deep_analysis` MCP tool) generate a prose
narrative for an analyzed index using an LLM. The provider is selected by
the `TRUSTY_LLM_MODEL` environment variable.

### Using OpenRouter (default)

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
export TRUSTY_LLM_MODEL=openai/gpt-4o-mini   # default; override as needed
trusty-analyze --search-url http://127.0.0.1:7878 serve
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

trusty-analyze --search-url http://127.0.0.1:7878 serve
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
| `TRUSTY_ANALYZE_SOCKET` | derived from the data dir | Analyzer socket path, honoured by `trusty-audit`'s guard. `TRUSTY_ANALYZER_PORT` was removed with the listener (#6287). |
| `RUST_LOG` | `warn` | Tracing filter |

## Feature Flags

| Flag | Description |
|---|---|
| `http-server` | The `service` module — the UDS daemon and the axum client it uses to reach trusty-search (enabled by default). Required for the `trusty-analyze` binary. The name predates #6287 and is now about which dependencies compile, not about a listener this daemon binds. |
| `ner` | Optional ONNX-backed named entity recognition (separate model file required). Off by default; not on the boot path. |

`bundled-ort`, `load-dynamic`, and `cuda` were removed in #5067 along with the
neural clustering embedder. One install command works on every host.

## Architecture

The crate is a single `trusty-analyze` package containing the CLI binary
(`trusty-analyze`) and a library. All analysis engines, the socket server, and
the MCP stdio server live within this one crate. Shared types (complexity metrics,
code smells, knowledge-graph entities, facts) come from `trusty-common`.

```
trusty-search (port 7878, HTTP)          trusty-analyze (trusty-analyze.sock)
  GET /indexes/:id/chunks  ──────────►   complexity analysis (tree-sitter)
  (bulk corpus export)                   blame + temporal decay
                                         quality grade aggregation
                                         k-means concept clustering
                                         facts store (redb)
                                         JSON-RPC over UDS + MCP stdio
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

## Publishing

`trusty-analyze` is a **UI-embedding crate**: its `build.rs` invokes `pnpm` to
build the embedded Svelte dashboard served from `src/service/` unless told to
skip it. When publishing, always set `SKIP_UI_BUILD=1` so the committed
`ui-dist/` bundle is used as-is:

```bash
SKIP_UI_BUILD=1 cargo publish -p trusty-analyze
```

Without the flag, `cargo publish` runs `build.rs` inside the verification
tarball, where `pnpm` tries to write outside `OUT_DIR` and the publish fails.
The same flag applies to the sibling UI-embedding crates (`trusty-search`,
`trusty-memory`). See the workspace release workflow in the root
[CLAUDE.md](../../CLAUDE.md) and the `cargo-publish` skill for the full
tag → publish → install sequence (`trusty-analyze-v<version>`).

## License

Licensed under the [MIT License](https://opensource.org/licenses/MIT).

## Repository

<https://github.com/bobmatnyc/trusty-tools>
