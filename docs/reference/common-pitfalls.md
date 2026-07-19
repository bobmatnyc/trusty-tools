# Common Pitfalls Reference

🔴 **Duplicating a shared capability instead of extending the common entry point** —
every cross-crate capability (external tool spawning, HTTP clients, config/secret
loading, daemon discovery) has exactly one canonical implementation. A second
implementation is a defect: fixes land N times, security patches miss copies, and
silent behavioral drift emerges. Before writing `Command::new(...)`, `reqwest::Client`,
`std::env::var()` for a concern used in multiple crates, search for an existing
entry point (`git grep`, then trusty-common source tree) and extend it. See the
common-entry-point principle and domain consolidation audit in
[.trusty-mpm/INSTRUCTIONS.md](../../.trusty-mpm/INSTRUCTIONS.md).

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

🟡 **Skipping proportional documentation on new public items** — clippy does
not enforce this. Scale documentation to how surprising the code is: the full
Why/What/Test pattern is mandatory for API entry points, design-heavy code,
error contracts, safety/TCC behavior, and cross-crate surfaces; a one-line summary
suffices for trivial items (simple getters, obvious one-liners). Avoid
defensive-reasoning paragraphs in doc comments — link to issues or ADRs instead
(`// See issue #1234 for the full context`). Review public APIs manually before
committing.

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

🟡 **`cargo install trusty-search` / `trusty-analyze` on Amazon Linux 2023
(or any glibc < 2.38 host)** — the default `bundled-ort` feature statically
links an ONNX Runtime build that requires glibc >= 2.38; AL2023 ships glibc
2.34. Either grab the prebuilt `x86_64-linux-al2023` GitHub Release tarball
(already configured with `load-dynamic`), or reinstall with
`--no-default-features --features load-dynamic` (`trusty-search`) /
`--no-default-features --features http-server,load-dynamic`
(`trusty-analyze`) plus `ORT_DYLIB_PATH` pointing at a host-compatible
`libonnxruntime.so`. See each crate's README ("AL2023 / glibc < 2.38 hosts")
and issue #2222. On a mismatched host, `trusty-embedderd`'s startup now fails
fast with an explicit glibc-version error instead of hanging for up to
`TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS` (default 180 s).

🟢 **MSRV drift** — the workspace pins `rust-version = "1.91"`. Running
`rustup update` and picking up a new nightly may introduce syntax that
compiles locally but fails on CI. Prefer stable channel toolchains.

🟢 **Edition mismatch** — `trusty-mpm`, `trusty-mpm-gui`, `trusty-agents`, `trusty-agents-common`, and `trusty-agents-local` use edition 2024;
all other crates use edition 2021. Let-chains (`if let … && let …`) only
work in edition 2024. Do not copy let-chain patterns into edition-2021 crates.

🟢 **`trusty-mpm-gui` is excluded from bare `cargo build`/`test`/`check`**
(#2951) — the root `Cargo.toml`'s `default-members` list omits it, matching
CI's existing `--workspace --exclude trusty-mpm-gui` for the same commands.
Use `cargo build -p trusty-mpm-gui` (or `--workspace`, which always builds
everything regardless of `default-members`) when you actually need it. This
also stops every agent worktree from silently producing a fresh ad-hoc-signed
GUI debug binary — and its own macOS "would like to access data from other
apps" TCC prompt — on every bare `cargo build`.

🟡 **`cargo tauri build` for `trusty-mpm-gui` needs Bob's Developer ID cert,
or an env-var override** — `crates/trusty-mpm-gui/tauri.conf.json` pins
`bundle.macOS.signingIdentity` to `"Developer ID Application: Bob Matsuoka
(4JH68XUHC5)"` (#2951, stable TCC identity instead of a fresh ad-hoc one per
rebuild — see the entry above). On a machine without that exact certificate
in the login keychain, Tauri's bundler hard-fails the signing step. Override
it with the `APPLE_SIGNING_IDENTITY` environment variable — Tauri's bundler
honors it in place of the config value (confirmed against the Tauri v2
[environment variables reference](https://v2.tauri.app/reference/environment-variables/):
"`APPLE_SIGNING_IDENTITY` — The identity used to code sign. Overwrites
`tauri.conf.json > bundle > macOS > signingIdentity`."):
`APPLE_SIGNING_IDENTITY=- cargo tauri build` for a local ad-hoc build (the
`-` pseudo-identity), or set it to a Developer ID string you do have. `cargo
build -p trusty-mpm-gui` / `cargo check -p trusty-mpm-gui` (no bundling) are
unaffected — this only matters for `cargo tauri build`/`tauri build`.

🟡 **`trusty-code-gui` mirrors the `trusty-mpm-gui` signing pattern above** —
same stable Developer ID identity (`Developer ID Application: Bob Matsuoka
(4JH68XUHC5)`) pinned in `crates/trusty-code-gui/tauri.conf.json`'s
`bundle.macOS.signingIdentity`, same `APPLE_SIGNING_IDENTITY` env-var
override for machines without that cert, and it is likewise excluded from
`default-members` (a bare `cargo build`/`test`/`check` skips it; use
`cargo build -p trusty-code-gui` or `--workspace`). `trusty-code-gui` is a
second, independent Tauri crate (`crates/trusty-code-gui`, for the
`trusty-code`/tcode daemon) — it does not join `trusty-mpm-gui`'s
`SIGNABLE_BINARIES` set, and as of this writing `trusty-code`/`tcode` has no
`tctl sign`-style fallback install path of its own for either the CLI or the
GUI binary (no `install-trusty-code-signed.sh` script or `CODE_SET` exists);
the Tauri `bundle.macOS.signingIdentity` config is the only signing path for
`trusty-code-gui` today.
