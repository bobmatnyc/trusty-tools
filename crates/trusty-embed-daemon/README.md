# trusty-embed-daemon

Batching ONNX embedding subprocess for the trusty-* ecosystem.

## Why this exists

`trusty-memory` (and other consumers) currently embed every `memory_recall`
query inline, serializing through a single `parking_lot::Mutex<TextEmbedding>`
inside `FastEmbedder`. Under 50 concurrent callers, p99 latency hits ~894 ms.
Moving embedding to a dedicated subprocess with a batching queue eliminates
the mutex contention entirely.

## Architecture

```
caller (e.g. trusty-memory)
  └─ EmbedClient (trusty-common, embed-client feature)
       └─ UDS JSON-RPC → $TMPDIR/trusty-embed.sock
                           │
                     trusty-embed-daemon
                           ├─ BatchQueue: tokio channel + 10ms / 32-item window
                           ├─ FastEmbedder (single ONNX instance, no contention)
                           └─ LRU cache (dedup repeated queries)
```

## Run

```bash
# Default socket: $TMPDIR/trusty-embed.sock
RUST_LOG=info cargo run -p trusty-embed-daemon

# Custom socket / batching params
RUST_LOG=info cargo run -p trusty-embed-daemon -- \
  --socket /tmp/my-embed.sock \
  --batch-size 64 \
  --batch-window-ms 5
```

## Protocol

JSON-RPC 2.0 over a Unix domain socket. Newline-delimited frames.

```json
// Request
{"jsonrpc":"2.0","method":"embed","params":{"texts":["q1","q2"]},"id":"abc"}

// Response
{"jsonrpc":"2.0","result":{"embeddings":[[0.1,...],[0.2,...]]},"id":"abc"}

// Error
{"jsonrpc":"2.0","error":{"code":-32603,"message":"..."},"id":"abc"}
```

See `crates/trusty-common/src/embed_client.rs` for the in-process client
companion (gated behind the `embed-client` feature).
