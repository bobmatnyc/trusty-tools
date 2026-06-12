# Common Pitfalls — Full Reference

This page expands on the pitfalls mentioned in `CLAUDE.md`. See the main document for the complete list at a glance.

## 🔴 Using `unwrap()` in library crates

The compiler does not stop you, but it violates the project's hard rule.

**Rule:** Use `?` with `thiserror` error types in libraries. `expect()` is allowed only for invariants that genuinely cannot occur at runtime (not for "I think this will always be Some").

**Why:** Unwrap panics are unrecoverable and crash the calling application without a chance to handle errors gracefully. In a library, the calling code should always have the opportunity to handle failure modes.

## 🔴 Logging to stdout in a daemon or MCP server

MCP JSON-RPC framing uses stdout as the transport channel. A stray `println!` corrupts the protocol and breaks the entire communication link.

**Rule:** Always use `tracing::info!` / `tracing::debug!` etc. (which write to stderr).

**Why:** `init_tracing` is wired to stderr specifically so stdout stays clean for JSON-RPC framing. Stdout is the wire protocol; stderr is diagnostics.

## 🔴 Adding `axum` as an unconditional dependency in a library crate

Put it behind the `axum-server` feature flag, matching the pattern in `trusty-common`. Otherwise every library consumer pulls in the full axum + tower stack.

**Why:** Library consumers should not pay the dependency cost for a feature they may not use. The workspace convention is to gate all HTTP server dependencies behind feature flags.

## 🟡 Editing a shared crate without propagating changes

Modifying `trusty-common` (or its consolidated `symgraph` / `embedder` / `mcp` modules), `trusty-embedderd`, or `trusty-bm25-daemon` can silently break dependents.

**Rule:** Always run `cargo check` (workspace-wide) and `cargo test -p <consumer>` for every crate that imports the edited library.

**Why:** The workspace is atomic — a change in a library can break dozens of downstream crates. Verify compilation and tests across all dependents before committing.

## 🟡 Forgetting the Why/What/Test doc pattern on new public items

Clippy does not enforce this. Review public APIs manually before committing.

**Why:** The Why/What/Test pattern is how future readers understand design intent without digging through git history. It's the primary documentation for every public API.

## 🟡 Building the Svelte UI manually before `cargo build`

`trusty-search` uses `build.rs` to invoke pnpm if `ui-dist/` is stale. If pnpm is not installed, the build script fails loudly.

**Rule:** Install pnpm or set `SKIP_UI_BUILD=1` if you are not changing the UI.

**Why:** The build.rs script checks whether the source UI has changed; if so, it rebuilds the distributable. Without pnpm available, the build fails rather than silently shipping stale UI code.

## 🟡 `[patch.crates-io]` only works at the workspace root

Do not add `[patch]` tables inside individual crate `Cargo.toml` files; Cargo ignores them.

**Rule:** All patches must live in the root `Cargo.toml`.

**Why:** Cargo's patch mechanism is applied at the workspace level, not per-crate. Crate-local patches have no effect and lead to subtle version conflicts.

## 🔴 Growing a file past 500 lines instead of splitting

The compiler does not stop you, but continued feature additions to a 1,000+ line file make the module harder to review, reason about, and test.

**Rule:** Split proactively when a file approaches 500 lines. See the 500-line cap section in `CLAUDE.md` for the mechanical enforcement and ratchet system.

**Example:** The trusty-agents `ctrl/`, `runtime/`, and `workflow/engine/` modules (#170, #171, #172) were canonical examples of files that grew past the cap; all three have since been split into focused submodules and now serve as worked examples of a clean split.

## 🟢 MSRV drift

The workspace pins `rust-version = "1.91"`. Running `rustup update` and picking up a new nightly may introduce syntax that compiles locally but fails on CI.

**Rule:** Prefer stable channel toolchains.

**Why:** CI runs with a locked MSRV; new nightly features won't compile on the stable CI toolchain.

## 🟢 Edition mismatch

`trusty-mpm`, `trusty-mpm-gui`, `trusty-agents`, `trusty-agents-common`, and `trusty-agents-local` use edition 2024; all other crates use edition 2021.

**Rule:** Let-chains (`if let … && let …`) only work in edition 2024. Do not copy let-chain patterns into edition-2021 crates.

**Why:** Editions are opt-in. Newer syntax requires explicit edition upgrade. Check the crate's `Cargo.toml` before using advanced syntax features.
