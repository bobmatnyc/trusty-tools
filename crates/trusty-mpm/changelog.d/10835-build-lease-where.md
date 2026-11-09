Added
- `tm build-lease where <bin> [--profile debug|release] [--json]` prints the freshest built binary for the current checkout — the newest existing artifact by mtime across `CARGO_TARGET_DIR` (else `<checkout>/target`), the repo's shared target dir, and its seeded build-pool slots. It exits 1 naming every path searched when none exists, and takes no lease (#10835).
