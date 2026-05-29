# trusty-embedderd — FastEmbed Sidecar Daemon

**Purpose**: FastEmbed wrapper sidecar daemon providing vector embeddings for trusty-search.

**License**: Elastic License 2.0

## Single-Install Bundling

`trusty-embedderd` and `trusty-search` are designed to run as **paired services**:
- `trusty-search` delegates embedding requests to `trusty-embedderd` over Unix domain sockets (localhost:5950)
- A single `trusty-embedderd` instance can serve multiple search indexes
- Both binaries are installed together via `cargo install --path crates/trusty-search --locked`

## Architecture

- **FastEmbed integration**: CPU and GPU support (ONNX runtime) with auto-tuning per platform
- **Batch processing**: Configurable batch sizes with memory-pressure detection
- **Model lifecycle**: Automatic model download and caching
- **Resource isolation**: Separate daemon process allows memory tuning independent of search workload

## Configuration

Configured via environment variables:
- `TRUSTY_MAX_BATCH_SIZE`: ONNX batch size (auto-tuned; override if OOM)
- `RUST_LOG`: Tracing filter (default: info)

## Integration Points

- **trusty-search**: Calls `/embed` RPC endpoint
- **HTTP API**: `POST /embed` for direct requests (used in testing)
- **Health checks**: `GET /health` endpoint

## See Also

- `crates/trusty-embedderd/README.md` for usage and design details
- `docs/trusty-search/` for integration context
