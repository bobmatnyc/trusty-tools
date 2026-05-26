# trusty-embedderd

Standalone ONNX embedding daemon for trusty-tools. Part of
[issue #110](https://github.com/bobmatnyc/trusty-tools/issues/110) Phase 1.

## Purpose

Runs the `AllMiniLML6V2Q` (all-MiniLM-L6-v2 INT8) model in a dedicated process
so trusty-search and other consumers can embed texts without loading the ONNX
runtime into their own RSS budget. Decouples crash domains: a jetsam OOM kill
of trusty-search doesn't destroy the model state.

## Running the daemon

```bash
# Default: binds to 127.0.0.1:7890
cargo run -p trusty-embedderd

# Custom address
cargo run -p trusty-embedderd -- --http 127.0.0.1:9000

# Via env var
TRUSTY_EMBEDDERD_ADDR=127.0.0.1:9000 cargo run -p trusty-embedderd
```

All logs are written to **stderr**. Stdout is never written to.

## CLI flags

| Flag | Default | Env var | Description |
|---|---|---|---|
| `--http <addr>` | `127.0.0.1:7890` | `TRUSTY_EMBEDDERD_ADDR` | TCP address to listen on |

## Endpoints

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

## Opt-in from trusty-search

Set `TRUSTY_EMBEDDER` to the daemon's base URL before starting trusty-search:

```bash
# Start the embedding daemon
trusty-embedderd --http 127.0.0.1:7890 &

# Start trusty-search with remote embedder
TRUSTY_EMBEDDER=http://127.0.0.1:7890 trusty-search start
```

The default (`TRUSTY_EMBEDDER` unset, `local`, or `in-process`) keeps the
existing in-process FastEmbedder behaviour unchanged.

## License

[Elastic License 2.0](LICENSE) — matching the rest of the trusty-* ecosystem.
