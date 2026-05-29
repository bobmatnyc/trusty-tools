# trusty-bm25-daemon — BM25 Index Sidecar Daemon

**Purpose**: BM25 full-text index daemon serving trusty-memory's persistence layer.

**License**: Elastic License 2.0

## Single-Install Bundling

`trusty-bm25-daemon` and `trusty-memory` are designed to run as **paired services**:
- `trusty-memory` delegates BM25 indexing and search to `trusty-bm25-daemon` over Unix domain sockets (localhost:5951)
- A single daemon instance indexes and searches memory embeddings + metadata
- Both binaries are installed together via `cargo install --path crates/trusty-memory --locked`

## Architecture

- **BM25 scoring**: Okapi BM25 implementation for full-text search
- **Incremental indexing**: Add/update/delete document operations
- **Query DSL**: Support for phrase queries, field restrictions, boolean operators
- **Resource efficiency**: Lightweight indexing suitable for memory-palace scale (millions of nodes)

## Configuration

Configured via environment variables:
- `RUST_LOG`: Tracing filter (default: info)
- `TRUSTY_BM25_DB_PATH`: SQLite database location (auto-created)

## Integration Points

- **trusty-memory**: Calls `/search`, `/index`, `/delete` RPC endpoints
- **HTTP API**: Direct search interface for CLI tools
- **Health checks**: `GET /health` endpoint

## See Also

- `crates/trusty-bm25-daemon/README.md` for usage and design details
- `docs/trusty-memory/` for integration context
