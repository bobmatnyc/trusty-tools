# trusty-embedder-client

RPC types and client implementations for the `trusty-embedderd` standalone
embedding process. Part of [issue #110](https://github.com/bobmatnyc/trusty-tools/issues/110)
Phase 1.

## Design rationale

The existing `FastEmbedder` runs the ONNX all-MiniLML6V2Q model inside the
trusty-search process. A crash or jetsam OOM kill of trusty-search also
destroys the model state and forces a cold restart. Extracting the embedder
into a dedicated `trusty-embedderd` process (#110) lets the two crash
independently, keeps the model resident across search-daemon restarts, and
keeps the large ONNX RSS footprint off the search daemon's memory budget.

This crate defines the shared contract between the daemon and its consumers:

- **`EmbedderClient` trait** — the single async primitive `embed_batch`.
- **`InProcessEmbedderClient`** — backward-compatible wrapper around
  `FastEmbedder`; zero behaviour change for existing deployments.
- **`RemoteEmbedderClient`** — HTTP client that delegates to a running
  `trusty-embedderd` instance.
- **`EmbedRequest` / `EmbedResponse`** — JSON wire types shared by the
  daemon and all consumers.
- **`EmbedderError`** — structured error enum for downstream error handling.

## Usage

### In-process mode (default, backward compatible)

```rust
use trusty_common::embedder::FastEmbedder;
use trusty_embedder_client::{EmbedderClient, InProcessEmbedderClient};

let embedder = FastEmbedder::new().await?;
let client = InProcessEmbedderClient::new(embedder);

let vectors = client.embed_batch(vec!["hello world".to_string()]).await?;
assert_eq!(vectors.len(), 1);
assert_eq!(vectors[0].len(), 384);
```

### Remote mode (opt-in via `TRUSTY_EMBEDDER`)

Start `trusty-embedderd` first:

```bash
cargo run -p trusty-embedderd -- --http 127.0.0.1:7890
```

Then connect from trusty-search by setting the env var:

```bash
TRUSTY_EMBEDDER=http://127.0.0.1:7890 trusty-search start
```

Or use the client directly:

```rust
use trusty_embedder_client::{EmbedderClient, RemoteEmbedderClient};

let client = RemoteEmbedderClient::new("http://127.0.0.1:7890");
let vectors = client.embed_batch(vec!["hello world".to_string()]).await?;
```

## License

MIT
