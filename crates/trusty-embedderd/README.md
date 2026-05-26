# trusty-embedderd

Standalone ONNX embedding daemon for trusty-tools (issue #164 consolidation).

## Purpose

Runs the `AllMiniLML6V2Q` (all-MiniLM-L6-v2 INT8) model in a dedicated process
so trusty-search and other consumers can embed texts without loading the ONNX
runtime into their own RSS budget. Decouples crash domains: a jetsam OOM kill
of trusty-search doesn't destroy the model state.

Three transports share a single `BatchQueue` and a single ONNX session:

- **stdio** (`--stdio`): sidecar mode — JSON-RPC 2.0 over piped stdin/stdout.
  This is the **default auto-spawn transport** when `trusty-search start` spawns
  `trusty-embedderd` as a supervised child process (issue #110 Phase 2).
- **HTTP** (`--http addr:port`): for network-capable consumers or cross-host setups.
- **UDS** (`--socket /path`): low-latency in-host transport via a Unix Domain Socket.

## Running the daemon

### Stdio sidecar mode (default for trusty-search auto-spawn)

`trusty-search start` spawns `trusty-embedderd --stdio` automatically and
communicates via piped stdin/stdout. No manual setup needed.

When used as a sidecar the lifecycle is implicit: when the parent closes its
write end of the pipe (on exit), the child's stdin reaches EOF and the daemon
exits cleanly with code 0. No explicit kill signal needed.

You can also run it manually for testing the stdio transport:

```bash
# The parent side must write JSON-RPC frames to stdin and read responses from stdout.
# Logs always go to stderr — stdout is reserved for JSON-RPC frames.
cargo run -p trusty-embedderd -- --stdio
```

### HTTP mode

```bash
# Default: binds to 127.0.0.1:7890
cargo run -p trusty-embedderd -- --http 127.0.0.1:7890

# Custom address
cargo run -p trusty-embedderd -- --http 127.0.0.1:9000

# Via env var
TRUSTY_EMBEDDERD_ADDR=127.0.0.1:9000 cargo run -p trusty-embedderd -- --http ""
```

### UDS mode

```bash
cargo run -p trusty-embedderd -- --socket /tmp/trusty-embedderd.sock
```

### Combined HTTP + UDS (--stdio is mutually exclusive with both)

```bash
cargo run -p trusty-embedderd -- --http 127.0.0.1:7890 --socket /tmp/trusty-embedderd.sock
```

All logs are written to **stderr**. Stdout is reserved for JSON-RPC frames in
`--stdio` mode and is never written to in HTTP/UDS modes.

## CLI flags

| Flag | Default | Env var | Description |
|---|---|---|---|
| `--stdio` | off | — | Stdio sidecar mode. Mutually exclusive with `--http` and `--socket`. |
| `--http <addr>` | `127.0.0.1:7890` | `TRUSTY_EMBEDDERD_ADDR` | TCP address for HTTP listener. Pass `""` or omit to disable. |
| `--socket <path>` | none | `TRUSTY_EMBEDDERD_SOCKET` | Path for the UDS socket. Omit to disable. |
| `--batch-size <n>` | 64 | `TRUSTY_EMBED_BATCH_SIZE` | Max texts per ONNX batch. |
| `--batch-window-ms <ms>` | 10 | `TRUSTY_EMBED_BATCH_WINDOW_MS` | Coalescing window for concurrent requests. |

## Wire protocol (stdio and UDS transports)

Both stdio and UDS use **newline-framed JSON-RPC 2.0**. Each request and
response is a single JSON object terminated with `\n`.

### Request

```json
{"jsonrpc":"2.0","method":"embed","params":{"texts":["hello world","fn foo()"]},"id":1}
```

### Success response

```json
{"jsonrpc":"2.0","result":{"embeddings":[[0.1,0.2,...],[0.3,0.4,...]]},"id":1}
```

Each inner array has 384 elements (AllMiniLML6V2Q output dimension).

### Error response

```json
{"jsonrpc":"2.0","error":{"code":-32603,"message":"ort session failed"},"id":1}
```

## HTTP endpoints

### `GET /health`

Liveness probe. Returns HTTP 200 with:

```json
{"status": "ok", "model": "AllMiniLML6V2Q", "dim": 384}
```

### `POST /embed`

Embed a batch of texts.

Request body (`Content-Type: application/json`):

```json
{"texts": ["hello world", "fn authenticate() {...}"]}
```

Response body:

```json
{"vectors": [[0.1, 0.2, ...], [0.3, 0.4, ...]]}
```

Each inner array has 384 elements. An empty `texts` array returns an empty
`vectors` array.

## Integration with trusty-search

### Default (auto-spawn, stdio sidecar — Phase 2)

`trusty-search start` auto-spawns `trusty-embedderd --stdio` when `TRUSTY_EMBEDDER`
is unset. **`trusty-embedderd` is now bundled inside the `trusty-search` install**,
so one command installs both binaries:

```bash
cargo install trusty-search --locked   # installs trusty-search AND trusty-embedderd
trusty-search start                    # auto-spawns trusty-embedderd --stdio, supervised
```

For users who want **only** the embedding daemon (e.g. trusty-memory without
trusty-search), the standalone install is still available:

```bash
cargo install trusty-embedderd --locked
```

Override with `TRUSTY_EMBEDDER=in-process` to use the legacy in-process path.

### Manual HTTP remote

```bash
# Start the embedding daemon manually
trusty-embedderd --http 127.0.0.1:7890

# Point trusty-search at it
TRUSTY_EMBEDDER=http://127.0.0.1:7890 trusty-search start
```

### Manual UDS remote

```bash
trusty-embedderd --socket /tmp/my-embedderd.sock
TRUSTY_EMBEDDER=unix:/tmp/my-embedderd.sock trusty-search start
```

## License

[Elastic License 2.0](LICENSE) — matching the rest of the trusty-* ecosystem.
