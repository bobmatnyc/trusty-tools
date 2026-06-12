# Common Pitfalls Reference

🔴 **Using `unwrap()` in library crates** — the compiler does not stop you, but
it violates the project's hard rule. Use `?` with `thiserror` error types in
libraries. `expect()` is allowed only for invariants that genuinely cannot
occur at runtime (not for "I think this will always be Some").

🔴 **Logging to stdout in a daemon or MCP server** — MCP JSON-RPC framing uses
stdout as the transport channel. A stray `println!` corrupts the protocol.
Always use `tracing::info!` / `tracing::debug!` etc. (which write to stderr).

🔴 **Adding `axum` as an unconditional dependency in a library crate** — put it
behind the `axum-server` feature flag, matching the pattern in `trusty-common`.
Otherwise every library consumer pulls in the full axum + tower stack.

🟡 **Editing a shared crate without propagating changes** — modifying
`trusty-common` (or its consolidated `symgraph` / `embedder` / `mcp` modules),
`trusty-embedderd`, or `trusty-bm25-daemon` can silently break dependents. Always run `cargo check` (workspace-wide) and
`cargo test -p <consumer>` for every crate that imports the edited library.

🟡 **Forgetting the Why/What/Test doc pattern on new public items** — clippy
does not enforce this. Review public APIs manually before committing.

🟡 **Building the Svelte UI manually before `cargo build`** — `trusty-search`
uses `build.rs` to invoke pnpm if `ui-dist/` is stale. If pnpm is not
installed, the build script fails loudly. Install pnpm or set
`SKIP_UI_BUILD=1` if you are not changing the UI.

🟡 **`[patch.crates-io]` only works at the workspace root** — do not add
`[patch]` tables inside individual crate `Cargo.toml` files; Cargo ignores
them. All patches must live in the root `Cargo.toml`.

🔴 **Growing a file past its SLOC cap instead of splitting** — the compiler does
not stop you, but continued feature additions make the module harder to review,
reason about, and test. Split proactively. The applicable cap is **500 SLOC for
production files** and **1500 SLOC for test/benchmark files** (see the Key
Conventions section for the exact classification rules). SLOC counts code lines
only: blank lines, `//` comments, `///` doc comments, `//!` inner-doc comments,
and `/* ... */` block comments (including multi-line spans) are all excluded.
The trusty-agents `ctrl/`, `runtime/`, and `workflow/engine/` modules (#170,
#171, #172) were the canonical examples of files that grew past the prod cap;
all three have since been split into focused submodules and now serve as the
worked examples of a clean split.

🟢 **MSRV drift** — the workspace pins `rust-version = "1.91"`. Running
`rustup update` and picking up a new nightly may introduce syntax that
compiles locally but fails on CI. Prefer stable channel toolchains.

🟢 **Edition mismatch** — `trusty-mpm`, `trusty-mpm-gui`, `trusty-agents`, `trusty-agents-common`, and `trusty-agents-local` use edition 2024;
all other crates use edition 2021. Let-chains (`if let … && let …`) only
work in edition 2024. Do not copy let-chain patterns into edition-2021 crates.
