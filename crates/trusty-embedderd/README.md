# trusty-embedderd

Standalone ONNX embedding daemon for trusty-tools. Part of
[issue #110](https://github.com/bobmatnyc/trusty-tools/issues/110) Phase 1, with
the UDS transport and batching queue consolidated in
[issue #164](https://github.com/bobmatnyc/trusty-tools/issues/164).

## Purpose

Runs the `AllMiniLML6V2Q` (all-MiniLM-L6-v2 INT8) model in a dedicated process
so trusty-search and other consumers can embed texts without loading the ONNX
runtime into their own RSS budget. Decouples crash domains: a jetsam OOM kill
of trusty-search doesn't destroy the model state.

Supersedes the retired `trusty-embed-daemon` crate (PR #157) — both daemons
loaded the same model independently, so they were consolidated into a single
process that serves both HTTP and UDS transports concurrently.

## Transports

The daemon can listen on both transports simultaneously, sharing one
`FastEmbedder` instance and one shared `BatchQueue`:

- **HTTP** (always bound) — convenient for cross-host or container deployments
  and easy `curl` debugging. Bound by `--http`.
- **UDS** (optional) — lower latency for on-host callers (no TCP stack
  overhead, sub-ms connect). Wire-compatible with the JSON-RPC 2.0 protocol
  previously served by `trusty-embed-daemon`. Bound by `--socket`.

## Running the daemon

```bash
# Default: HTTP only on 127.0.0.1:7890
cargo run -p trusty-embedderd

# Custom HTTP address
cargo run -p trusty-embedderd -- --http 127.0.0.1:9000

# HTTP + UDS on the default socket path
cargo run -p trusty-embedderd -- --socket /tmp/trusty-embedderd.sock

# Via env vars
TRUSTY_EMBEDDERD_ADDR=127.0.0.1:9000 \
TRUSTY_EMBEDDERD_SOCKET=/tmp/trusty-embedderd.sock \
  cargo run -p trusty-embedderd
```

All logs are written to **stderr**. Stdout is never written to.

## CLI flags

| Flag | Default | Env var | Description |
|---|---|---|---|
| `--http <addr>` | `127.0.0.1:7890` | `TRUSTY_EMBEDDERD_ADDR` | TCP address to listen on |
| `--socket <path>` | unset | `TRUSTY_EMBEDDERD_SOCKET` | Unix domain socket path. When set, the daemon binds both the HTTP listener and the UDS socket, sharing one `BatchQueue` and one `FastEmbedder`. |
| `--batch-size <n>` | `32` | `TRUSTY_EMBEDDERD_BATCH_SIZE` | Maximum texts coalesced into one ONNX batch. |
| `--batch-window-ms <ms>` | `10` | `TRUSTY_EMBEDDERD_BATCH_WINDOW_MS` | Coalescing window; nearly-simultaneous arrivals merge into one ONNX call. |
| `-v / -vv / -vvv` | — | — | Increase verbosity (info / debug / trace). |

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

Each inner array has 384 elements (all-MiniLML6V2Q output dimension). An empty
`texts` array returns an empty `vectors` array.

## UDS protocol

Newline-framed JSON-RPC 2.0. Wire-compatible with the retired
`trusty-embed-daemon` so existing UDS clients work unchanged after switching
the socket path.

Request:

```json
{"jsonrpc":"2.0","method":"embed","params":{"texts":["hello world"]},"id":1}
```

Response:

```json
{"jsonrpc":"2.0","result":{"embeddings":[[0.1,0.2,...]]},"id":1}
```

Errors use the standard JSON-RPC error envelope:

```json
{"jsonrpc":"2.0","error":{"code":-32600,"message":"..."},"id":1}
```

See `crates/trusty-common/src/embedder_client/uds.rs` for the canonical Rust
client (`UdsEmbedderClient`).

## Batching behaviour

Both transports share one `BatchQueue`. Requests arriving within
`--batch-window-ms` are coalesced into one ONNX call (up to `--batch-size`
texts per call), then the resulting vectors fan back out to the original
caller via `oneshot` channels. This amortises ONNX session overhead across
concurrent requests without exposing the queue to callers.

## Opt-in from trusty-search

Set `TRUSTY_EMBEDDER` to the daemon's base URL before starting trusty-search:

```bash
# Start the embedding daemon (HTTP + UDS)
trusty-embedderd --http 127.0.0.1:7890 --socket /tmp/trusty-embedderd.sock &

# Start trusty-search with remote embedder (HTTP)
TRUSTY_EMBEDDER=http://127.0.0.1:7890 trusty-search start
```

The default (`TRUSTY_EMBEDDER` unset, `local`, or `in-process`) keeps the
existing in-process FastEmbedder behaviour unchanged.

## License

[Elastic License 2.0](LICENSE) — matching the rest of the trusty-* ecosystem.
