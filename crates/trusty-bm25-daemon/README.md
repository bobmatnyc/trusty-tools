# trusty-bm25-daemon

Per-palace BM25 lexical-search subprocess for the trusty-* ecosystem.

## Why

`trusty-memory`'s recall path historically had no lexical lane — only vector
similarity (L2 via usearch HNSW). For short, identifier-heavy queries
("cargo test", "PalaceHandle") BM25 routinely wins; for conceptual queries
the vector lane wins. Hybrid recall via Reciprocal Rank Fusion needs both
lanes to be cheap to query and cheap to update.

Running BM25 in-process inside `trusty-memory` would block the recall hot
path on disk I/O during writes and contend with the redb/usearch locks.
Splitting it into a per-palace subprocess gives each palace its own writer
(the subprocess IS the lock), eliminates contention, and matches the
architecture of the sibling `trusty-embed-daemon` (PR #157).

## What

One subprocess per palace. The daemon owns:

- a `trusty_common::bm25::BM25Index` (the same implementation trusty-search
  uses),
- a persistent snapshot at `<data_dir>/bm25_index.json`,
- a 50 ms write-coalescing queue (longer than embed-daemon's 10 ms because
  disk I/O dominates),
- a single Unix domain socket at `$TMPDIR/trusty-bm25-<palace>.sock`.

All requests speak newline-delimited JSON-RPC 2.0.

## Methods

| Method   | Params                                  | Result                       | Caller            |
|----------|-----------------------------------------|------------------------------|-------------------|
| `index`  | `{doc_id, text}`                        | `{indexed: true}`            | `memory_remember` |
| `search` | `{query, top_k}`                        | `{hits: [{doc_id, score}]}`  | `memory_recall`   |
| `delete` | `{doc_id}`                              | `{deleted: bool}`            | dream subprocess  |
| `rebuild`| `{}`                                    | `{doc_count: usize}`         | dream subprocess  |

The request path (`memory_remember`) only calls `index`. `delete` and
`rebuild` are reserved for the dream subprocess.

## Architecture (single-writer)

A single `tokio::task` owns the `BM25Index` mutably. Every JSON-RPC method —
including reads — flows through one `mpsc` channel into that task. The
channel IS the lock, so there is no `RwLock<Bm25Index>` and no contention.
At palace sizes typical for memory (hundreds to low thousands of drawers),
search latency is dominated by channel round-trip (~microseconds), not by
contention with writes.

Writes batch on 50 ms / 64-item windows; one disk flush per batch keeps
amortised I/O bounded.

## Wire protocol

```json
// request
{"jsonrpc":"2.0","method":"index","params":{"doc_id":"...","text":"..."},"id":1}

// response
{"jsonrpc":"2.0","result":{"indexed":true},"id":1}
```

Errors follow JSON-RPC 2.0 codes (`-32600` invalid request, `-32601` method
not found, `-32700` parse error, `-32603` internal error).

## CLI

```
trusty-bm25-daemon \
  --palace <name> \
  --data-dir <path> \
  [--socket <path>]
```

`--palace` selects the canonical socket path
(`$TMPDIR/trusty-bm25-<palace>.sock`); `--socket` overrides it.

## Running

Usually spawned by `trusty-memory` on first access to a palace. For manual
testing:

```bash
cargo run -p trusty-bm25-daemon -- \
  --palace test \
  --data-dir /tmp/bm25-test
```

Then send a request:

```bash
echo '{"jsonrpc":"2.0","method":"search","params":{"query":"cargo","top_k":5},"id":1}' \
  | nc -U "$TMPDIR/trusty-bm25-test.sock"
```

## Persistence

On startup the daemon loads `<data_dir>/bm25_index.json` if present. Every
write batch flushes back to that file (atomic write via `.tmp` + rename).
JSON keeps the snapshot human-inspectable; doc counts in memory palaces are
small enough that the format overhead is invisible.

## See also

- `crates/trusty-common/src/bm25.rs` — the canonical `BM25Index`.
- `crates/trusty-common/src/bm25_client.rs` — the `Bm25Client` consumers
  use to drive this daemon.
- `crates/trusty-embed-daemon/` — the sibling embed subprocess this design
  is modelled on.
