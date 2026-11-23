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

## Build provenance and the run counter

`build.rs` runs at compile time and bakes four values into the binary: the
short and full git commit hashes, that commit's date, and whether the working
tree was dirty. `--version` prints them and nothing else, so two invocations of
one binary print identical bytes (#4260).

```bash
cargo run -- --version
# trusty-agents v0.39.0 (abc1234, 2026-08-31T09:12:44-04:00)
```

Separately, each process start increments `.trusty-agents/state/build.json`.
That counter names the RUN, not the build — it renders as `run #N` in the
startup log line and lets you correlate a run with
`docs/performance/runs/*.json`. It never appears in `--version`.

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
