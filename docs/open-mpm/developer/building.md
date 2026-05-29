# Building

## Prerequisites

- **Rust stable 1.80+** (uses `edition = "2024"`)
- **`pnpm`** (only if rebuilding the embedded web UI)
- **A C/C++ toolchain** — required by the `usearch` and `tree-sitter` crates
  - macOS: `xcode-select --install`
  - Linux: `apt install build-essential` or equivalent
  - Windows: install MSVC build tools

## Build commands

```bash
# Debug build (fast compile, slow runtime)
cargo build

# Release build (slow compile, fast runtime)
cargo build --release

# Run without building first
cargo run -- --ctrl

# Run with logging
RUST_LOG=debug cargo run -- --workflow prescriptive --task-file ./t.md
```

The release binary lands in `target/release/open-mpm`. A second binary
`ompm` (a thin client wrapping the API server) is also produced.

## Embedded web UI

The web UI is baked into the binary via `rust-embed`. The Vite-built assets
live in `ui/dist/`.

### Rebuilding the UI

```bash
cd ui
pnpm install
pnpm build
# Outputs: ui/dist/  (consumed by rust-embed at compile time)

cd ..
cargo build  # Re-embeds the new ui/dist/
```

If you skip this step, the binary still compiles but `GET /` returns 404.

### Local UI dev loop

```bash
# Terminal 1 — Rust API server
cargo run -- --api --port 7654

# Terminal 2 — Vite dev server with HMR
cd ui && pnpm dev
# Vite proxies /api/* to http://localhost:7654
```

## Build counter

`build.rs` runs at compile time, captures the git commit hash, and bakes
it into the binary. At runtime, every invocation increments
`.open-mpm/state/build.json` so each run has a unique build number you
can correlate with `docs/performance/runs/*.json`.

```bash
cargo run -- --version
# open-mpm v0.1.37 (abc1234) build #189
```

## Cross-compilation

Not currently configured. A Docker-based release pipeline would be a
welcome contribution.

## Common build issues

### `usearch` or `tree-sitter` linking errors

Install a C/C++ toolchain (see Prerequisites).

### `rust-embed` complains `ui/dist not found`

Run `pnpm build` in `ui/` first, or stub it:

```bash
mkdir -p ui/dist
echo '<html></html>' > ui/dist/index.html
```

### Slow first build

The dependency graph is large (`async-openai`, `axum`, `usearch`,
`fastembed`, `tree-sitter-*`, …). First build can take 5+ minutes.
Subsequent builds use the incremental cache and are much faster.
Use `cargo build --jobs 8` (or your core count) to parallelize.

## Make targets

The `Makefile` wraps the common cargo commands:

```bash
make build      # cargo build
make release    # cargo build --release
make clean      # cargo clean
make ctrl       # cargo run -- --ctrl
make api        # cargo run -- --api --port 7654
make ui         # pnpm build in ui/
```

See `Makefile` for the full list.
