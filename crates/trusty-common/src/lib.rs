//! Shared utility surface for trusty-* projects.
//!
//! Why: Port auto-detect, data-directory resolution, tracing init, NO_COLOR
//! handling, and the OpenRouter chat-completions client appeared in both
//! trusty-memory and trusty-search with subtle divergence. Centralising keeps
//! them aligned and gives future trusty-* binaries a one-import surface.
//!
//! What: pure utility functions — no global state. Each subsystem is a free
//! function or a small helper struct.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only` covers
//! port walking, data-dir creation, and the OpenRouter request shape (without
//! hitting the network). The `--features` flag is load-bearing — see the
//! zero-feature test guard below.
//!
//! # Test isolation: `TRUSTY_DATA_DIR_OVERRIDE`
//!
//! macOS's [`dirs::data_dir()`] resolves the application-support directory via
//! `NSFileManager`, a native Cocoa API that completely ignores the `HOME` and
//! `XDG_DATA_HOME` environment variables. This makes it impossible to redirect
//! data-directory access in tests using ordinary env-var tricks, because the
//! kernel query bypasses the environment entirely.
//!
//! To work around this, [`resolve_data_dir`] checks the
//! [`DATA_DIR_OVERRIDE_ENV`] (`TRUSTY_DATA_DIR_OVERRIDE`) environment variable
//! before consulting `dirs::data_dir()`. When set, the variable's value is used
//! as the base directory verbatim, and `dirs::data_dir()` is never called.
//!
//! **This escape hatch is intended for testing only.** Do not set it in
//! production deployments; rely on the OS-standard data directory instead.

// docs.rs builds a release's documentation once, from the uploaded tarball,
// so a broken intra-doc link is baked into that version forever and only a new
// release can correct it. Deny keeps this crate at zero rather than letting the
// ratchet in `scripts/check_rustdoc_links.sh` absorb a new one.
#![deny(rustdoc::broken_intra_doc_links)]

// #4901: a feature-less `cargo test -p trusty-common` must not report success
// over the 25+ modules it never compiled.
//
// Why: this crate declares `default = []`. The bare crate-scoped command that
// CLAUDE.md prescribes as the per-crate check therefore runs 328 of the crate's
// ~2062 tests and exits 0 — including when a file in `memory_core` does not
// compile at all, which is how PR #4899 reported a green gate before the
// correct code existed. CI only reaches those modules through `--workspace`
// feature unification, so the local green is the one that lies.
// What: `build.rs` sets `trusty_common_no_features` when Cargo activated no
// feature at all, and this refuses to build the unit-test target under it. The
// guard is `cfg(test)`-scoped, so `cargo build`/`cargo check` and all 20
// consumer crates are untouched; a run that names any feature is a deliberate,
// visible choice and still builds.
// Test: the `trusty_common_build_script_ran` guard below covers the one way
// this could go quiet unnoticed — `build.rs` no longer running. The guard's own
// behaviour is demonstrated by breaking a `memory_core` file and re-running both
// command forms (PR for #4901).
#[cfg(all(test, trusty_common_no_features))]
compile_error!(
    "`cargo test -p trusty-common` ran with NO features enabled, and this crate's \
     `default` feature set is empty. That build compiles none of the feature-gated \
     modules (memory_core, embedder, uds, sld, symgraph, bm25, migrations, …): it \
     runs 328 of the crate's ~2062 tests and reports success over the rest — \
     including over a memory_core file that does not compile (#4901). \
     Name the gate you actually want: \
     `--features memory-core,embedder-test-support` for memory_core, \
     `--features <feature>` for any other gated module, or \
     `--features unconditional-only` to test just the always-compiled surface."
);

// #4901: the guard above discounts `CARGO_FEATURE_DEFAULT`, because Cargo
// activates `default` on every build including the bare one. That discount is
// correct only while `default` is `[]`.
//
// Why: give `default = ["foo"]` its own failure instead of letting it disarm
// the guard. Cargo would set `CARGO_FEATURE_FOO` on a bare `cargo test -p
// trusty-common`, `build.rs` would see a real feature enabled, and the
// zero-feature guard would stop firing — restoring the vacuous green with
// nothing red anywhere.
// What: `build.rs` scans the manifest for `default = []` and sets
// `trusty_common_default_not_empty` when it does not find exactly that,
// including when it cannot read the manifest at all. Scoped to `cfg(test)`
// like the guard it protects, so consumer builds are untouched.
// Test: `default_feature_set_is_empty` (`tests/feature_coverage.rs`) asserts
// the same fact through a real TOML parse.
#[cfg(all(test, trusty_common_default_not_empty))]
compile_error!(
    "crates/trusty-common/Cargo.toml no longer declares `default = []`. The #4901 \
     zero-feature test guard discounts `CARGO_FEATURE_DEFAULT` on the assumption \
     that `default` enables nothing; a non-empty `default` makes every bare \
     `cargo test -p trusty-common` look like a deliberate feature selection, and \
     the guard silently stops firing. Either keep `default` empty, or rewrite the \
     guard in build.rs to compare the enabled set against the default set."
);

// #4901: the guard above is only as live as the build script that feeds it.
// Deleting or short-circuiting `build.rs` would restore the silent green with
// nothing turning red, so the absence of its unconditional marker is itself a
// build failure. Checked on every build, not just `cfg(test)` — an inert guard
// is worth knowing about before someone relies on it.
#[cfg(not(trusty_common_build_script_ran))]
compile_error!(
    "crates/trusty-common/build.rs did not run. It emits the cfgs behind the \
     #4901 zero-feature test guard, so that guard is now inert and \
     `cargo test -p trusty-common` can report success again over the 25+ \
     modules it never compiled."
);

/// Shared trusty splash art + per-glyph shading (issue #3326).
///
/// Why: `tm`'s launch banner and `trusty-agents`' REPL startup splash must
/// present the same trusty branding; centralising the art text and its
/// color-bucket rule here stops the two binaries from drifting apart again.
/// What: [`banner::TRUSTY_SPLASH_ART`] (embedded ASCII/block-art text) and
/// [`banner::shade_bucket`] (glyph → RGB triple). Zero extra dependencies —
/// pure `&str` + `match`.
/// Test: `cargo test -p trusty-common --features unconditional-only -- banner::tests`.
pub mod banner;

pub mod chat;
pub mod claude_config;

/// Codex CLI MCP-server registration (`~/.codex/config.toml`).
///
/// Why: a Codex stdio registration with no argument vector launches the bare
/// binary, which prints help and exits before MCP initialization while the
/// connection still reads as enabled (#5264 / #5265).
/// What: [`codex_config::codex_config_path`] and
/// [`codex_config::patch_mcp_server`] — see the module docs.
/// Test: `cargo test -p trusty-common --features codex-config`.
#[cfg(feature = "codex-config")]
pub mod codex_config;

/// Canonical environment-variable name constants shared across the workspace.
///
/// Why: the same credential env-var names were spelled as bare literals at ~40
/// `std::env::var(...)` call sites across nine crates; centralizing them makes
/// a typo a compile error instead of a silent misread.
/// What: exposes [`ENV_OPENROUTER_API_KEY`](env_vars::ENV_OPENROUTER_API_KEY)
/// and [`ENV_GITHUB_TOKEN`](env_vars::ENV_GITHUB_TOKEN).
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// env_var_names_are_stable`.
pub mod env_vars;

pub mod project_discovery;

/// Shared graceful-shutdown signal helper for trusty-* daemons (issue #534).
///
/// Why: trusty-search, trusty-memory, and trusty-analyze all need the same
/// SIGTERM + SIGINT shutdown future to pass to axum's `with_graceful_shutdown`.
/// Centralising it here eliminates three-way duplication and guarantees every
/// daemon responds identically to `launchctl bootout`.
/// What: exposes [`shutdown_signal`] — an async fn that resolves on SIGTERM
/// (unix) or SIGINT/Ctrl-C (all platforms), whichever fires first.
/// Test: `cargo test -p trusty-common --features unconditional-only -- shutdown`.
pub mod shutdown;
pub use shutdown::shutdown_signal;

/// Bounded in-memory ring buffer of recent tracing log lines.
///
/// Why: trusty-* daemons expose a `/logs/tail` endpoint so operators can read
/// recent logs over HTTP without file I/O or a daemon restart. The buffer and
/// its `tracing_subscriber::Layer` live here so every daemon shares one impl.
/// What: `LogBuffer` (thread-safe capped `VecDeque<String>`) plus
/// `LogBufferLayer` (the tracing layer that feeds it).
/// Test: `cargo test -p trusty-common --features unconditional-only log_buffer`
/// covers capacity eviction, tail semantics, and layer capture.
pub mod log_buffer;

/// Process-wide panic hook that logs the panic payload through `tracing`.
///
/// Why (issue #4764): a macOS `.ips` crash report carries mangled symbols but
/// not the panic message, so the literal cause of a daemon abort is otherwise
/// unrecoverable in production.
/// What: `install_panic_logger` wraps the existing hook, emitting payload,
/// location, thread, and backtrace as one `tracing::error!` before delegating.
/// Test: `cargo test -p trusty-common --features unconditional-only panic_hook`.
pub mod panic_hook;

/// Process RSS / CPU sampling and data-directory sizing for daemon health.
///
/// Why: every trusty-* daemon's `/health` endpoint reports its own resident
/// memory, CPU usage, and on-disk footprint; the sampling logic is identical
/// across them so it lives here once.
/// What: `SysMetrics` (per-process RSS + CPU sampler) and `dir_size_bytes`
/// (recursive directory byte count).
/// Test: `cargo test -p trusty-common --features unconditional-only sys_metrics`.
pub mod sys_metrics;

/// Robust executable discovery and daemon `PATH` composition.
///
/// Why: launchd relaunches daemons with a minimal `PATH`, breaking spawns of
/// Homebrew/user-installed tools (`tmux`, `claude`) until the inherited `PATH`
/// is patched (#1298). This module composes the full set of well-known bin
/// dirs for a generated launchd plist and provides a `PATH`-then-well-known
/// binary resolver so the daemon spawns survive a minimal inherited `PATH`.
/// What: `daemon_path_dirs`, `daemon_path_env`, `resolve_binary`.
/// Test: `cargo test -p trusty-common --features unconditional-only bin_resolve`.
pub mod bin_resolve;

/// MCP registration for GUI clients launched by launchd (#6307).
///
/// Why: a GUI client started by launchd sees only
/// `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, so an entry whose `command` is a bare
/// `trusty-memory` exits 127 before MCP initialization and the client reports
/// only "no tools". The registration must carry the absolute path of the
/// running binary and a working directory that exists.
/// What: [`gui_mcp_client::running_binary_path`],
/// [`gui_mcp_client::build_entry`], and [`gui_mcp_client::configure`] — the
/// single implementation both `trusty-memory setup` and `trusty-search setup`
/// call.
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// gui_mcp_client`.
pub mod gui_mcp_client;

/// macOS LaunchAgent generation and lifecycle management. macOS-only —
/// the module compiles to nothing on every other platform.
#[cfg(target_os = "macos")]
pub mod launchd;

/// Label-correct LaunchAgent activation with legacy eviction and rollback
/// (#4919). macOS-only, like [`launchd`] itself.
///
/// Why: `install()` + `bootstrap()` could write one plist and activate a
/// different unit, bounced daemons that had not changed, and left the service
/// down when a bootstrap failed.
/// What: [`launchd::LaunchdConfig::install_and_activate`] and its
/// [`launchd_activate::Activation`] outcome.
/// Test: `cargo test -p trusty-common --features unconditional-only launchd_activate`.
#[cfg(target_os = "macos")]
pub mod launchd_activate;

/// Pre-bootout verification of the ACTIVE launchd unit's grace window (#6590).
/// macOS-only, like [`launchd`] itself.
///
/// Why: launchd applies the `ExitTimeOut` of the job it has LOADED, and re-reads
/// the plist only at `bootstrap` — so the corrected window
/// [`launchd::LaunchdConfig::render_plist`] writes never governs the bootout
/// that immediately precedes the bootstrap. A host whose loaded unit predates
/// #4393 therefore still SIGKILLs the daemon 5 s into a 55 s snapshot flush.
/// What: [`launchd_grace::grace_verdict`] over the window launchd will really
/// grant, plus [`launchd_grace::quiesce_job`], which stops the process with a
/// directly-delivered SIGTERM — bounded by nothing launchd controls — so the
/// bootout finds it already gone.
/// Test: `cargo test -p trusty-common --features unconditional-only launchd_grace`.
#[cfg(target_os = "macos")]
pub mod launchd_grace;

/// Bootout-completion waiting for a live launchd restart (#6618). macOS-only,
/// like [`launchd`] itself.
///
/// Why: `launchctl bootout` returns when the unload is ACCEPTED, not when the
/// job is gone. A restart that bootstraps immediately afterwards races its own
/// bootout and launchd refuses it with `Bootstrap failed: 5: Input/output
/// error`.
/// What: [`launchd_restart::await_unload`] polls the label out of launchd, and
/// [`launchd_restart::restart_sequence`] orders the whole bounce around that
/// wait — reusing [`launchd_grace`]'s quiesce for the short-grace half of the
/// same window.
/// Test: `cargo test -p trusty-common --features unconditional-only launchd_restart`.
#[cfg(target_os = "macos")]
pub mod launchd_restart;

/// Canonical launchd labels for every trusty-* LaunchAgent (#4919).
///
/// Why: each daemon crate, the installer's mirror table, the Makefiles, and
/// the install scripts each restated their own label literal, so they drifted
/// — `trusty-search service install` bootstrapped `com.trusty.trusty-search`
/// while launchd had `com.trusty.search` loaded, activating nothing.
/// What: [`launchd_labels::SERVICES`] plus the `com.trusty.<stem>` convention
/// as executable code, and the legacy aliases an upgrade must evict.
/// [`launchd_labels::RETIRED_SERVICES`] carries the other half (#6290): a
/// daemon that has been retired keeps its row so an upgrade can still boot out
/// the unit a pre-retirement install left loaded.
/// Deliberately NOT macOS-gated, unlike `launchd`, so the drift tests run on
/// Linux CI too. (`launchd` is named here, not linked: it does not exist on
/// Linux, so a link to it breaks on the very platform this sentence is about.)
/// Test: `cargo test -p trusty-common --features unconditional-only launchd_labels`.
pub mod launchd_labels;

/// Authoritative, three-state launchd supervision detection (issue #4469).
///
/// Why: the env-var heuristic this replaces let an unsupervised child
/// self-report as supervised, defeating the `/health`-based verification
/// operators are instructed to trust. Kept OUTSIDE the `update-check` feature
/// gate because supervision is a daemon-lifecycle fact, not an upgrade
/// concern — `update::upgrade` merely happens to be its oldest caller.
/// What: [`supervision::launchd_supervision`] and its three-state
/// [`supervision::LaunchdSupervision`] answer.
/// Test: `cargo test -p trusty-common --features unconditional-only supervision`.
pub mod supervision;

/// Bounded `ETXTBSY` retry for process spawns (issue #5446).
///
/// Why: four independent copies of the same retry had grown across
/// `trusty-agents`, `trusty-common`, and `trusty-mpm` (#1528, #1634, #3570,
/// #5391; class epic #3451), so a policy fix had to land four times.
/// What: [`spawn_retry::retry_on_etxtbsy`] (sync) and
/// [`spawn_retry::retry_on_etxtbsy_async`] share one policy function. Ungated —
/// `tokio` with `process` + `time` is already an unconditional dependency of
/// this crate, so the async driver adds nothing to the dependency graph.
/// Test: `cargo test -p trusty-common --features unconditional-only spawn_retry`.
pub mod spawn_retry;

#[cfg(feature = "axum-server")]
pub mod server;

/// The one client for the trusty-memory daemon's Unix socket (#2030, #6286).
///
/// Why: every trusty-mpm / trusty-code / trusty-agents / monitor-TUI call site
/// that talks to trusty-memory needs the same "derive the socket, send one
/// JSON-RPC frame" pair. Three independent clients existed before #6286; this
/// is what they folded onto.
/// What: gated behind the `memory-rpc` feature, which implies `uds`. The socket
/// path comes from [`daemon_addr::daemon_socket_path`], the same call the
/// daemon makes — which is why ADR-0040 (#5803) left this module here when the
/// protocol primitives moved to the `trusty-mcp` crate.
/// Test: `cargo test -p trusty-common --features memory-rpc`.
#[cfg(feature = "memory-rpc")]
pub mod memory_rpc;

/// General-purpose JSON-RPC client + transports (formerly the library half
/// of the `trusty-rpc` crate).
///
/// Why: Both `trpc` (the CLI) and any future library consumer want one
/// place that owns the JSON-RPC envelope construction, stdio-subprocess
/// transport, HTTP transport, and pretty-printers.
/// What: Gated behind the `rpc` feature; requires `uuid` for request id
/// generation. The HTTP transport reuses the workspace `reqwest`.
/// Test: `cargo test -p trusty-common --features rpc` runs the module's
/// own unit tests (envelope extraction, pretty-print smoke tests).
#[cfg(feature = "rpc")]
pub mod rpc;

/// Shared text-embedding abstraction (formerly the `trusty-embedder` crate).
///
/// Why: trusty-memory and trusty-search both ship near-identical `Embedder`
/// traits and `FastEmbedder` implementations; centralising the surface here
/// keeps them aligned and lets future consumers pick up embedding for free
/// without a separate published crate.
/// What: Gated behind the `embedder` feature. Exposes the `Embedder` trait,
/// `FastEmbedder` (fastembed-rs, all-MiniLM-L6-v2, 384-d) with LRU caching
/// and ORT warmup, and (under `embedder-test-support`) the `MockEmbedder`
/// test double.
/// Test: `cargo test -p trusty-common --features embedder,embedder-test-support`
/// covers the mock embedder and ONNX-backed `#[ignore]`d integration tests.
#[cfg(feature = "embedder")]
pub mod embedder;

/// Unified RPC client surface for the `trusty-embedderd` standalone process.
///
/// Why: absorbs both the former `trusty-embedder-client` HTTP crate (PR #163)
/// and the former `embed_client` UDS module (PR #157) into a single unified
/// module. Reduces workspace crate count and provides one trait (`EmbedderClient`)
/// with three concrete implementations (InProcess, HTTP remote, UDS remote) so
/// call sites are identical regardless of transport. The `embed-client` feature
/// and `embed_client` module are retired by issue #164; use `embedder-client`
/// and `trusty_common::embedder_client::UdsEmbedderClient` instead.
/// What: Gated behind the `embedder-client` feature. Exposes the
/// `EmbedderClient` trait, `InProcessEmbedderClient`, `RemoteEmbedderClient`
/// (HTTP), `UdsEmbedderClient` (UDS), `EmbedRequest` / `EmbedResponse` wire
/// types, and `EmbedderError`. The UDS impl uses `tokio::net::UnixStream`
/// with newline-framed JSON-RPC 2.0 — no additional dependencies.
/// Test: `cargo test -p trusty-common --features embedder-client` covers
/// error-display, JSON round-trip, URL assembly, UDS wire types, and empty-
/// batch short-circuits. ONNX-backed tests are in
/// `trusty-embedderd/tests/bit_identical.rs` (`#[ignore]`).
#[cfg(feature = "embedder-client")]
pub mod embedder_client;

/// Zero-dependency BM25 lexical index + code-aware tokenizer (issue #156).
///
/// Why: trusty-memory, trusty-search, and the per-palace
/// `trusty-bm25-daemon` subprocess all want one shared BM25 implementation
/// so the tokenizer's camelCase / PascalCase / alpha↔digit splits stay
/// consistent across the workspace. Originally ported from trusty-agents; now
/// the single source of truth lives here.
/// What: Gated behind the `bm25` feature. Adds no new dependencies — pure
/// `std` + `tracing` (already required).
/// Test: `cargo test -p trusty-common --features bm25`.
#[cfg(feature = "bm25")]
pub mod bm25;

/// Reusable schema-migration kernel (issue #179).
///
/// Why: trusty-search, trusty-memory, and other long-lived stores have grown
/// ad-hoc schema-migration loops that drift apart. Centralising the
/// `SchemaVersion` newtype, the `Migration<S>` trait, and a `MigrationRunner`
/// that applies pending steps in order (writing a stamp after each) collapses
/// those into one shared kernel. The `file_stamp` helper covers the common
/// "JSON sidecar in the store's data dir" stamp format; redb-stamp users get
/// a documented recipe instead of a heavyweight dep.
/// What: gated behind the `migrations` feature flag. Adds no new
/// dependencies — pure `serde` + `serde_json` + `anyhow` + `tracing` which
/// the crate already requires.
/// Test: `cargo test -p trusty-common --features migrations` covers the
/// runner ordering, crash resumption, write-stamp failure propagation, and
/// the file-stamp round-trip / atomic-write behaviour.
#[cfg(feature = "migrations")]
pub mod migrations;

// Why (#5329): the `bm25_client` module was REMOVED along with the
// `trusty-bm25-daemon` subprocess it spoke to. trusty-memory now links the
// `bm25` module's `BM25Index` directly, the way trusty-search always has, so
// there is no socket, no wire protocol and no binary to locate. The `uds`
// module and its `supervisor` submodule are untouched — trusty-console and
// trusty-agents supervise trusty-review / trusty-analyze through them.

/// Symbol-graph engine (formerly the `trusty-symgraph` crate).
///
/// Why: All trusty-* tools that touch source code (trusty-agents, trusty-search,
/// trusty-analyze) want the same `EntityType` / `RawEntity` / `EdgeKind`
/// data shapes and (for orchestrators) the same tree-sitter pipeline. Living
/// here lets the workspace ship one tree-sitter `links =` slot instead of
/// juggling two crates that both claim it.
/// What: Gated behind two features. `symgraph` exposes only the contracts
/// surface (`EntityType`, `RawEntity`, `EdgeKind`, `fact_hash_str`, tables)
/// — no tree-sitter, no `links` conflict. `symgraph-parser` additionally
/// pulls in tree-sitter and the full parse → registry → emit stack.
/// `symgraph-server` enables the HTTP server frontend.
/// Test: `cargo test -p trusty-common --features symgraph` exercises the
/// contracts surface; `cargo test -p trusty-symgraph` covers the parser
/// path through the thin re-export shim.
#[cfg(feature = "symgraph")]
pub mod symgraph;

/// Memory Palace storage engine (formerly the `trusty-memory-core` crate).
///
/// Why: Centralises the Memory Palace data model (`Palace` -> `Wing` ->
/// `Room` -> `Drawer`; "closet" is the keyword -> drawer-ids inverted index,
/// not a level — ADR-0027 D3), storage backends (usearch vector index + SQLite
/// knowledge graph + chat-session log + payload store), retrieval handle,
/// and the dream / decay / analytics / git-history surfaces so every
/// trusty-* binary that talks to a palace reuses the same types. Absorbed
/// into `trusty-common` (issue #5 phase 2d) so we ship one fewer published
/// crate.
/// What: Gated behind the `memory-core` feature because it pulls in heavy
/// storage deps (`usearch`, `rusqlite`, `r2d2`, `git2`). Enables
/// the embedder surface automatically (memory-core → embedder).
/// Test: `cargo test -p trusty-common --features memory-core` exercises
/// the full surface.
#[cfg(feature = "memory-core")]
pub mod memory_core;

/// Unified ticketing MCP server (formerly the `trusty-tickets` crate).
///
/// Why: Claude Code and the rest of the trusty-* suite need a single MCP
/// surface that can talk to GitHub Issues, JIRA, and Linear without the
/// caller needing to know which backend is configured. Absorbing into
/// `trusty-common` reduces the workspace crate count and co-locates the
/// HTTP client surface with the other protocol helpers.
/// What: Gated behind the `tickets` feature. Exposes `tickets::api::*`
/// (config, models, Backend trait, three concrete backends), `tickets::server`
/// (MCP dispatch loop + `run_stdio`), and `tickets::tools` (the tool-list
/// schema). Requires the `mcp` feature for the stdio loop.
/// Test: `cargo test -p trusty-common --features tickets` runs the module's own
/// unit tests (dispatch, tool-list counts, config parsing, serde round-trips).
/// Live backend tests require env-var credentials.
#[cfg(feature = "tickets")]
pub mod tickets;

/// Intent-source resolver (ISR) for the intent/method conformance gates (#1358).
///
/// Why: the DOC-15 conformance capability needs one shared resolver that both
/// the FRONT gate (trusty-mpm) and the BACK gate (trusty-review) call, so
/// ticket+spec resolution and the precedence rule (ticket > spec) are
/// implemented once, centrally, and the two gates can never disagree
/// (`SPEC-CONFORMANCE-03~draft`, spec §6).
/// What: gated behind the `intent-source` feature (depends on `tickets` and
/// `chat`). Exposes `intent_source::{resolve, ResolvedIntent, IntentQuery, …}`
/// plus the pluggable `TicketFetcher` / `IntentTokenResolver` / `SpecLookup` /
/// `MethodExtractor` seams. Fail-open throughout (`thiserror`, no `unwrap`).
/// Test: `cargo test -p trusty-common --features intent-source` runs the
/// module's AC-1..AC-7 unit tests with no network access.
#[cfg(feature = "intent-source")]
pub mod intent_source;

/// Language-agnostic Spec-Linked Documentation (SLD) reference grammar (DOC-38).
///
/// Why: DOC-38 promotes SLD from an incidental, Rust-only rustdoc convention to
/// a first-class, implementation-neutral standard usable in any language or
/// repository. The `intent_source` resolver already reads the Rust form; the
/// generalized grammar (frontmatter `spec_refs:`, per-language comment idioms,
/// fenced-code exclusion, the open `~<rev>` token) needs a shared, reusable home
/// so a documentation linter (`trusty-sld-lint`, DOC-38 §10 F1) and the resolver
/// parse ONE grammar, not two.
/// What: gated behind the lightweight `sld` feature (regex + serde_yaml +
/// thiserror only — no `tickets`/git2/rusqlite). Exposes the canonical
/// `SPEC-{SUBSYSTEM}-{NN}~{rev}` id grammar (`is_valid_spec_id`, `revision_of`,
/// `base_id`, `reference_regex`), the per-extension `CommentSyntax` table,
/// `parse_inline_refs` (fenced-code-aware `# Spec References` block parsing),
/// `parse_frontmatter_refs` (`spec_refs:` YAML), and `spec_anchors` /
/// `anchor_resolves` (heading-anchor scanning + revision-tolerant resolution).
/// `intent_source::spec_resolve` reuses this module's `revision_of`/`base_id`.
/// Test: `cargo test -p trusty-common --features sld` runs the module's unit
/// tests (grammar, inline, frontmatter, anchor) with no I/O.
#[cfg(feature = "sld")]
pub mod sld;

/// Declarative CLI help system with "did you mean?" suggestions (issue #216).
///
/// Why: every standalone trusty-* binary used to render its `--help` and
/// unknown-subcommand error output independently, so the formats drifted
/// apart over time. Centralising the help model into one YAML schema, one
/// canonical renderer, and one Jaro-Winkler suggester keeps the six binaries
/// (search, memory, analyze, mpm-cli, tga, trusty-agents) speaking with a single
/// user-facing voice.
/// What: gated behind the `cli-help` feature. Pulls in `serde_yaml`, `strsim`,
/// and `indexmap`. Exposes `HelpConfig` / `CommandDef` / `FlagDef` / `Example`
/// + `load_help` / `render_help` / `suggest`.
/// Test: `cargo test -p trusty-common --features cli-help`.
#[cfg(feature = "cli-help")]
pub mod help;

/// Unified monitor TUI for the trusty-search and trusty-memory daemons
/// (formerly the `trusty-monitor-tui` crate).
///
/// Why: operators run both daemons and want one terminal surface that shows
/// the health of both at a glance. Living here behind the `monitor-tui`
/// feature flag matches the workspace's "one fewer published crate" direction
/// (issue #31 companion) and keeps the dashboard logic unit-testable.
/// What: gated behind the `monitor-tui` feature, which pulls in `ratatui` and
/// `crossterm`. Exposes `monitor::run` (the entry point the `trusty-monitor`
/// binary calls) plus the pure `dashboard` / `search_client` / `memory_client`
/// submodules.
/// Test: `cargo test -p trusty-common --features monitor-tui` covers the
/// rendering, layout, and HTTP-client pieces.
#[cfg(feature = "monitor-tui")]
pub mod monitor;

// epic #1104: stdio MCP client + console metrics contract (feature-gated).
#[cfg(feature = "console-metrics")]
pub mod console_metrics;
#[cfg(feature = "stdio-mcp-client")]
pub mod stdio_mcp_client;

/// Whole-machine host metrics for the Foundry machine-status dashboard (#6517).
///
/// Why: [`sys_metrics`] samples only the current PROCESS; the Foundry dashboard
/// needs the whole HOST — overall CPU, system memory with a pressure signal,
/// per-mount + aggregate disk, and network throughput. This shared host-sampling
/// capability lives here (not in the console) per the workspace common-entry
/// rule, so any future consumer reuses the same typed shapes.
/// What: gated behind the `host-metrics` feature, which additionally enables
/// `sysinfo`'s `disk` and `network` features (the crate already depends on
/// `sysinfo` with `system` for `sys_metrics`). Exposes
/// [`host_metrics::HostSampler`] and the [`host_metrics::HostMetrics`] snapshot
/// with its subsystem structs and PROVISIONAL [`host_metrics::HostThresholds`].
/// Test: `cargo test -p trusty-common --features host-metrics -- host_metrics`.
#[cfg(feature = "host-metrics")]
pub mod host_metrics;

/// Upload trusty-* log files to object storage (#6533).
///
/// Why: every trusty daemon writes logs to a local path that nothing prunes and
/// nothing collects, so diagnosing a failure on another machine means asking a
/// human to find and send a file. The drain lives here rather than in any one
/// daemon because five crates produce logs and the common-entry rule gives that
/// capability exactly one implementation.
/// What: gated behind the `log-drain` feature, which implies `credentials` so
/// the collector's [`credentials::scrub_secrets`] pass cannot be
/// compiled out from under it. Exposes the [`log_drain::LogDestination`] trait
/// with `s3://` and `file://` adapters, [`log_drain::DestinationUri`],
/// [`log_drain::DrainManifest`], [`log_drain::collect`], and
/// [`log_drain::run_once`]. This is the drain CORE — no scheduler, and no
/// GitHub-identity resolution; the caller supplies a [`log_drain::DrainTarget`].
/// Test: `cargo test -p trusty-common --features log-drain --no-fail-fast`.
#[cfg(feature = "log-drain")]
pub mod log_drain;

/// Throttled crates.io update-notification helper.
///
/// Why: User-facing CLIs should nudge operators when a newer release is
/// available without adding perceptible latency. A shared implementation
/// keeps the throttle, cache, opt-out, and User-Agent logic consistent across
/// every consumer in the workspace.
/// What: Gated behind the `update-check` feature. Exposes
/// [`update::check_throttled`] (the main entry — reads a per-crate JSON cache
/// under the OS cache dir, queries crates.io at most once per 24 h),
/// [`update::check_crates_io`] (the raw network call), [`update::notice`]
/// (formatted upgrade message), and [`update::UpdateInfo`] (the result type).
/// All failures degrade to `None` — the check is best-effort and will not
/// panic or stall a CLI.
/// Opt-out: set `TRUSTY_NO_UPDATE_CHECK` or `CI` to any non-empty value.
/// Test: `cargo test -p trusty-common --features update-check`.
#[cfg(feature = "update-check")]
pub mod update;

/// Generated documentation regions — the code as the single source for the
/// volatile facts crate READMEs state (#5205 follow-up).
///
/// Why: MCP tool tables and counts were hand-maintained in both `README.md`
/// and `CLAUDE.md`, so they drifted from the descriptor functions and each
/// wrong entry had to be fixed twice. Three crates need the same machinery, so
/// it lives here rather than in three copies.
/// What: Gated behind the `docgen` feature (test-facing; enable it in
/// `[dev-dependencies]`). Exposes [`docgen::tool_rows`] and
/// [`docgen::render_tool_section`] (deterministic, name-sorted rendering),
/// [`docgen::assert_region`] / [`docgen::sync_region`] (check, or rewrite under
/// `UPDATE_DOCS=1`), and the `descriptor_source!` macro that makes the cited
/// symbol compiler-checked.
/// Test: `cargo test -p trusty-common --features docgen`, plus
/// `tests/generated_docs.rs` in trusty-search, trusty-memory, trusty-analyze.
#[cfg(feature = "docgen")]
pub mod docgen;

/// Error-capture layer for the trusty-* consent-gated bug-reporting system
/// (bug-reporting Phase 1, issue #479).
///
/// Why: Every trusty-* daemon encounters runtime errors that developers need
///      to see but that must be captured locally and only filed to GitHub after
///      explicit user consent. A shared capture layer in `trusty-common` means
///      all daemons gain error capture without per-binary changes.
/// What: Gated behind the `bug-capture` feature. Exposes:
///      - [`error_capture::CapturedError`] — structured error record.
///      - [`error_capture::ErrorStore`] — ring buffer + JSONL store.
///      - [`error_capture::BugCaptureLayer`] — the tracing Layer.
///      - [`error_capture::bug_capture_layer`] — convenience constructor.
///      - [`error_capture::TRUSTY_NO_BUG_CAPTURE_ENV`] — opt-out env name.
///      Additive: does not alter stderr logging. Opt-out via
///      `TRUSTY_NO_BUG_CAPTURE=1`. New dep: `sha2` (already workspace-optional).
/// Test: `cargo test -p trusty-common --features bug-capture`.
#[cfg(feature = "bug-capture")]
pub mod error_capture;

/// The `~/.trusty-tools/<crate>/config.yaml` cross-crate config convention (#1220).
///
/// Why: every trusty-* crate had its own config location/format; #1220
/// standardises one convention so an operator always knows where a crate's
/// configuration lives. Centralising the path resolution and typed YAML
/// load/save here means each crate adopts it by calling two functions.
/// What: Gated behind the `crate-config` feature. Exposes
/// [`crate_config::crate_config_path`], [`crate_config::load`],
/// [`crate_config::load_or_default`], and [`crate_config::save`].
/// Test: `cargo test -p trusty-common --features crate-config -- crate_config::tests`.
#[cfg(feature = "crate-config")]
pub mod crate_config;

/// The credential authority's storage and resolution layer (DOC-45).
///
/// Why: credentials are not an inference concern. The resolver was filed under
/// `inference::` when its only consumers were LLM providers, but four of its
/// ten registry entries were already non-inference (Slack, Telegram,
/// `claude-code`) and consumers kept not finding it there. #4564 promotes it to
/// the top level so `trusty_common::credentials` is the one place a credential
/// is named, stored, and resolved — the module DOC-45 §2.3 calls "the
/// authority".
/// What: Gated behind the `credentials` feature. Exposes the [`KeyStore`]
/// trait and its three backends ([`credentials::MemoryKeyStore`],
/// [`credentials::FileKeyStore`], and — behind `keyring-store` —
/// `KeyringStore`), the provider [`credentials::env_var_for`] registry, the
/// 3-tier [`credentials::resolve_key`] precedence chain, the `.env.local`
/// loader, and [`credentials::redact_secret`].
/// Test: `cargo test -p trusty-common --features credentials -- credentials::`
/// and `cargo test -p trusty-common --features keyring-store -- credentials::`.
///
/// [`KeyStore`]: credentials::KeyStore
#[cfg(feature = "credentials")]
pub mod credentials;

/// Unified inference provider adapter layer (epic #2400).
///
/// Why: six trusty-* crates each hand-rolled their own LLM client, key
/// lookup, and `.env.local` loading. Epic #2400 centralises the adapter,
/// credential resolution, and capability registry here so every consumer
/// shares one implementation.
/// What: Gated behind the `credentials` feature (the gate predates #4564 and
/// is kept so the deprecated `inference::credentials` compatibility shim still
/// resolves for a consumer that enables only `credentials`). The inference
/// surface proper — adapter trait, capability registry, provider clients — is
/// gated behind `inference-client`.
/// Test: `cargo test -p trusty-common --features credentials -- inference::`
/// and `cargo test -p trusty-common --features keyring-store -- inference::`.
#[cfg(feature = "credentials")]
pub mod inference;

/// The workspace's single redb corruption / obsolete-format classifier (#5063).
///
/// Why: five crates each carried a byte-identical copy of the four-arm `match`
/// that decides whether an unopenable redb file may be quarantined, so a safety
/// change to that decision had to land five times. The classifier previously
/// lived under `memory-core`, which pulls in usearch, git2 and a bundled ORT
/// embedder — a cost no consumer would pay for a predicate, which is why the
/// copies existed. This module is gated behind the light `redb-open` feature
/// (`dep:redb` only) so every store can route through it.
/// What: [`redb_open::is_incompatible_format`] plus the quarantine-path helpers
/// two of those crates also duplicated. Each store's recovery POLICY stays in
/// its own crate — see the module docs for why they legitimately diverge.
/// Test: `cargo test -p trusty-common --features redb-open -- redb_open::`.
#[cfg(feature = "redb-open")]
pub mod redb_open;

// ─── Focused submodules (split from lib.rs in issue #1108) ────────────────

/// TCP port auto-walking helper.
///
/// Why: Running multiple daemon instances shouldn't produce noisy failures
/// when a port is already occupied.
/// What: Exposes [`bind_with_auto_port`] which walks forward to the next free
/// port within `max_attempts`.
/// Test: `cargo test -p trusty-common --features unconditional-only -- port::tests`.
pub mod port;

/// Canonical project-slug derivation (issue #1348).
///
/// Why: trusty-memory and trusty-installer both need the identical
/// directory-basename/repo-name → slug rule (the trusty-memory daemon's
/// `validate_palace_name` rejects a palace whose slug disagrees with the one it
/// re-derives). Centralising the rule here makes it the single source of truth
/// so the two crates cannot silently diverge.
/// What: Exposes [`slug::slugify_string`], re-exported at the crate root as
/// [`slugify_string`].
/// Test: `cargo test -p trusty-common --features unconditional-only -- slug::tests`.
pub mod slug;
pub use slug::slugify_string;

/// Canonical trusty-search index-id derivation from a project path (issue #1373).
///
/// Why: trusty-mpm (register-and-pin at session launch) and trusty-search
/// (`detect_project`, MCP serve pin) must derive the byte-for-byte identical
/// index id from the same project root, or a session pins one id while querying
/// another. Centralising the rule here — the crate both already depend on —
/// keeps them in lockstep without a trusty-mpm → trusty-search dependency edge.
/// What: Exposes [`index_id::derive_index_id`], [`index_id::resolve_project_root`],
/// [`index_id::find_git_root`], and [`index_id::identifies_same_path`] — the one
/// implementation of "do these two paths name the same directory tree?" that
/// both registration guards route through — plus
/// [`index_id::refuse_unindexable_root`], the one implementation of "may this
/// root become an index at all?" that both DERIVATION sites route through
/// (#6550).
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// index_id::tests`.
pub mod index_id;
pub use index_id::{
    IndexRootRefusal, derive_checkout_index_id, derive_index_id, find_git_root,
    identifies_same_path, refuse_unindexable_root, refuse_unindexable_root_against,
    resolve_project_root,
};

/// Project-derived trusty-search index identity — the PARTITIONING key
/// (epic #4207; supersedes the approach closed as won't-do in #4063).
///
/// Why: [`index_id::derive_index_id`]'s bare basename collides for unrelated
/// checkouts sharing a directory name, and a tm session gets its worktree UUID
/// as the id — so service identity is bound to ephemeral writer isolation and
/// BASE_PM's "pass the project name" instruction 404s. Both need ONE id derived
/// from the PROJECT. It lives here, beside `index_id`/`repo_identity`/`slug`,
/// because trusty-mpm, trusty-search, trusty-review and trusty-code must all
/// compute the identical id — the same single-source-of-truth rule `index_id`
/// was hoisted here for (#1373). Unconditional (not feature-gated) because an
/// identity that varies with a feature flag is worse than none.
/// What: exposes [`project_index_id::ProjectIdentity`] (origin + root + operator,
/// with a pure `index_id()`), [`project_index_id::derive_project_index_id`], and
/// [`project_index_id::resolve_operator_identity`]. Derivation only — wired
/// into no resolution path; registry reconciliation and migration of existing
/// indexes are separate slices of #4207.
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// project_index_id`.
pub mod project_index_id;
pub use project_index_id::{ProjectIdentity, derive_project_index_id};

/// Shared best-effort trusty-search "ensure this project is indexed" helper
/// (issues #1373 / #1908), gated behind the `search-index` feature.
///
/// Why: the register-and-populate logic originally lived only in trusty-mpm's
/// session-launch path; trusty-code now wants the same behaviour at task start.
/// Promoting it here makes it the ONE implementation both crates call, per the
/// workspace common-entry-point rule, so they can never diverge.
/// What: exposes [`search_index::ensure_project_indexed`] (derive id →
/// best-effort find-or-create + freshness-gated reindex, fail-open) and the
/// [`search_index::index_is_fresh`] predicate. Feature-gated because it enables
/// `reqwest`'s `blocking` client; default builds pay nothing.
/// Test: `cargo test -p trusty-common --features search-index -- search_index::tests`.
#[cfg(feature = "search-index")]
pub mod search_index;

/// Bounded worker pool behind [`search_index::index_files_best_effort`] (issue
/// #2798), gated behind `search-index` alongside its only caller.
///
/// Why: the incremental index hook used to spawn one detached OS thread per
/// write with no cap, so a degraded-but-reachable trusty-search daemon — whose
/// per-file POSTs can each take ~6.2s — let threads accumulate faster than they
/// drained. Crate-private: it is an implementation detail of the hook, not a
/// general-purpose pool for other crates to reach for.
/// What: a fixed-size worker pool with a bounded queue; a submission that finds
/// both full is rejected (never blocked) and counted, and the caller logs what
/// it dropped.
/// Test: `cargo test -p trusty-common --features search-index -- index_dispatch`.
#[cfg(feature = "search-index")]
pub(crate) mod index_dispatch;

/// Shared trusty-search index READINESS probe (issue #2784), gated behind the
/// `search-index` feature alongside the warming helper it complements.
///
/// Why: [`search_index::ensure_project_indexed`] *warms* a project's index at
/// task start but never told the session whether it was actually ready — so a
/// daily-driver session could silently query during the semantic warm-up
/// window and get lexical-only results with no signal. This module adds the
/// missing *surfacing* half so both crates that warm (trusty-code, trusty-mpm)
/// can also report readiness from the ONE shared implementation.
/// What: exposes [`search_readiness::probe_index_readiness`] (fail-open probe),
/// the pure [`search_readiness::parse_readiness`] mapper, and
/// [`search_readiness::log_index_readiness`] (one stderr line surfacing lane
/// readiness to the session).
/// Test: `cargo test -p trusty-common --features search-index -- search_readiness::tests`.
#[cfg(feature = "search-index")]
pub mod search_readiness;

/// Canonical tmux-session naming shared by both session managers (SPEC-ONESM-01).
///
/// Why: trusty-mpm's `SessionManager` and trusty-agents' `TmManager` both create
/// tmux sessions, but only names carrying a managed prefix are recognised by
/// trusty-mpm's reconcile/prune/adopt/orphan-GC. Keeping the ONE naming rule here
/// — the crate both already depend on — lets trusty-agents emit managed names
/// without a `trusty-mpm` dependency edge, so its sessions stop being orphaned.
/// trusty-mpm's `core::names` re-exports this module verbatim for compatibility.
/// What: exposes the managed [`session_naming::PREFIX`] and legacy prefixes,
/// [`session_naming::is_managed_session_name`], [`session_naming::name_from_uuid`],
/// [`session_naming::name_from_dir`], [`session_naming::build_managed_session_name`]
/// / [`session_naming::build_session_name`] and the serial helpers.
/// Test: `cargo test -p trusty-common --features session-naming -- session_naming`.
#[cfg(feature = "session-naming")]
pub mod session_naming;

/// Canonical trusty-memory palace-ID derivation from project identity (#1217/#1605).
///
/// Why: trusty-memory (default-palace derivation at the CLI/hook/MCP edges) and
/// trusty-mpm (managed-session MCP injection — it pins `TRUSTY_MEMORY_PALACE` in
/// a cloned session's `.mcp.json`) must derive the byte-for-byte identical
/// palace slug from the same project identity, or a repo-cloned session resolves
/// the wrong palace. Centralising the pure rule here — the crate both already
/// depend on — keeps them in lockstep without a trusty-mpm → trusty-memory
/// dependency edge, exactly as `index_id` does for trusty-search index pinning.
/// What: Exposes [`palace_id::derive_palace_id`],
/// [`palace_id::owner_repo_from_git_remote`], [`palace_id::parent_dir_slug`],
/// and the [`palace_id::PALACE_OVERRIDE_ENV`] / [`palace_id::palace_override_from_env`]
/// env helpers, plus the id-shape contract [`palace_id::palace_id_is_valid`] /
/// [`palace_id::clamp_palace_id`] / [`palace_id::PALACE_ID_MAX_LEN`] that
/// trusty-memory's creation gate reads (#2443).
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// palace_id::tests`.
pub mod palace_id;
pub use palace_id::{
    PALACE_ID_MAX_LEN, PALACE_OVERRIDE_ENV, clamp_palace_id, derive_palace_id,
    owner_repo_from_git_remote, palace_id_is_valid, palace_override_from_env, parent_dir_slug,
    repo_slug_from_git_remote,
};

/// The single entry point for "which palace does this project use?" (#5811).
///
/// Why: [`palace_id::derive_palace_id`] is the PURE core and covers only three
/// of the four precedence levels — the committed `.trusty-tools/trusty-memory.yaml`
/// pin needs filesystem I/O, so it lived above the core in ONE caller
/// (trusty-memory). Every other caller therefore answered the question without
/// the pin, and trusty-mpm's pin-blind answer became the `TRUSTY_MEMORY_PALACE`
/// variable exported into managed sessions — the highest-precedence slot. A
/// derived name outranked the pin it was meant to lose to. This module owns the
/// whole rule, I/O included, so the split cannot reappear.
/// What: Gated behind the `palace-resolve` feature. Exposes
/// [`palace_resolve::resolve_palace`], [`palace_resolve::resolve_palace_with_remote`],
/// [`palace_resolve::PalaceResolution`], [`palace_resolve::PalaceSource`],
/// [`palace_resolve::PalaceResolveError`], the pin-file schema, and the shared
/// `git` probes.
/// Test: `cargo test -p trusty-common --features palace-resolve -- palace_resolve`.
#[cfg(feature = "palace-resolve")]
pub mod palace_resolve;
#[cfg(feature = "palace-resolve")]
pub use palace_resolve::{
    PIN_FILE_REL, PIN_SCHEMA_VERSION, PalaceResolution, PalaceResolveError, PalaceSource,
    ProjectPin, resolve_palace, resolve_palace_with_remote,
};

/// Palace-level alias map: redirect one palace name to another (issue #1939).
///
/// Why: trusty-mpm pins a managed session to the `owner-repo` palace slug, but
/// the pre-existing claude-mpm-era palace is the BARE repo name — so the pinned
/// palace does not exist and memory splits in two. A persisted alias map lets the
/// non-existent `owner-repo` name resolve to the existing bare palace. This is a
/// PALACE-level redirect, distinct from the in-palace term/KG entity aliases.
/// What: exposes [`palace_alias::PalaceAliasStore`] (load/register/resolve),
/// [`palace_alias::alias_target_if_absent`] (the one rule for whether a redirect
/// fires, shared with the registry), plus
/// [`palace_alias::default_palace_registry_dir`] and
/// [`palace_alias::palace_registry_dir_from`] for locating the registry dir. This
/// module is always compiled (no `memory-core` gate) so trusty-mpm's always-on
/// session-launch path can register aliases without pulling the storage engine.
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// palace_alias::tests`.
pub mod palace_alias;

/// Shared GitHub `owner/repo` path derivation (issue #1220).
///
/// Why: trusty-mpm's managed-session workspace root
/// (`~/trusty-mpm-projects/<owner>/<repo>/…`) and trusty-memory's palace-ID
/// derivation both need the canonical `owner/repo` identity of a project's git
/// origin remote. Centralising the parsing here keeps the two crates in lockstep.
/// What: Exposes [`github_path::GithubPath`], [`github_path::parse_github_path`]
/// (pure URL parse), and [`github_path::derive_github_path`] (reads
/// `remote.origin.url`).
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// github_path::tests`.
pub mod github_path;

/// The one resolver for trusty-mpm's managed workspace layout (#5203, #5204).
///
/// Why: the managed workspace root and the session-worktree base name are shared
/// by four crates, three of which cannot depend on `trusty-mpm`. Each had
/// hardcoded `~/trusty-mpm-projects` / `.worktrees` independently, so retargeting
/// either silently broke `trusty-code`'s project picker, `trusty-search`'s
/// ephemeral-dir exclusion, and `trusty-memory`'s workstream attribution.
/// Centralising here — the crate all four already depend on — is CLAUDE.md's
/// "Common entry point" rule applied to a capability that had four copies.
/// What: exposes [`workspace_layout::workspace_root`],
/// [`workspace_layout::worktrees_dirname`], their `resolve_*` cores (which take
/// an already-loaded config value), [`workspace_layout::WorktreeDirNames`] for
/// scan paths, and [`workspace_layout::WorkspaceLayoutConfig`].
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// workspace_layout::tests`.
pub mod workspace_layout;

/// Canonical repository identity (DOC-37) — the path-independent join key that
/// relates the live checkout, `.base` clone, and session worktrees of one repo.
///
/// Why: index ids are bare path basenames, so every facet of a repo registers
/// as an unrelated index. [`repo_identity::RepoIdentity`] supplies the missing
/// `owner/repo` (or content-hash) join key so trusty-search can group and filter
/// indexes by repo; it lives here so trusty-search and trusty-mpm derive it
/// identically.
/// What: exposes [`repo_identity::RepoIdentity`] (`derive`/`canonical`/`parse`).
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// repo_identity::tests`.
pub mod repo_identity;

/// Shared Slack `mrkdwn` formatting/escaping primitives (epic #2636).
///
/// Why: the `mrkdwn` escape rule and code-fence helpers were born in
/// trusty-mpm's inbound Slack gateway; the native Slack MCP server in
/// trusty-channels now needs the byte-identical escaping to neutralise markup
/// injection from untrusted channel/user text. Centralising the pure primitives
/// here — the crate both already depend on — keeps them from diverging without a
/// trusty-channels → trusty-mpm dependency edge.
/// What: exposes [`slack_format::mrkdwn_escape`], [`slack_format::code_block`],
/// and [`slack_format::code_inline`]. Pure `std` string ops — no dependencies,
/// no feature gate.
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// slack_format::tests`.
pub mod slack_format;

/// Data-directory resolution and filesystem utilities.
///
/// Why: All trusty-* tools share the same per-app data-directory resolution
/// logic including the macOS `NSFileManager` bypass needed for test isolation.
/// What: Exposes [`data_dir::resolve_data_dir`], [`data_dir::sanitize_data_root`],
/// [`data_dir::DATA_DIR_OVERRIDE_ENV`], and [`data_dir::is_dir`].
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// data_dir::tests`.
pub mod data_dir;

/// Runtime "am I a `cargo test` process?" detection (issue #4255).
///
/// Why: every existing guard against a test run mutating the operator's live
/// state was either compile-time (`cfg(test)`, which does not reach a crate's
/// `tests/` or `[[bin]]` targets) or a per-test convention someone had to
/// remember. Both were forgotten, and the live `indexes.toml` accumulated
/// throwaway fixture roots as a result. A runtime check is the only one that
/// also covers the cross-process case, where a test POSTs to a REAL daemon.
/// Unconditional (not feature-gated) because trusty-search consumes it from a
/// path that no feature flag governs.
/// What: exposes [`test_harness::running_under_test_harness`] plus the
/// [`test_harness::FORCE_ENV`] / [`test_harness::ALLOW_PRODUCTION_ENV`]
/// override names.
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// test_harness::tests`.
pub mod test_harness;
pub use test_harness::running_under_test_harness;

/// Cross-process exclusive advisory lock around a whole-file critical section.
///
/// Why: the lock [`json_rmw`] needs is not JSON-specific. `trusty-search`'s
/// `indexes.toml` has its own TOML loader and fail-closed parse contract, so it
/// cannot route through [`json_rmw::update`], yet its writers (the daemon,
/// `prune`, `prune-orphans`) lose each other's updates for exactly the same
/// reason (#5344). Owning the lock here keeps ONE implementation for both.
/// What: Exposes [`file_lock::with_exclusive_lock`] and [`file_lock::lock_path`].
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// file_lock::tests`.
pub mod file_lock;

/// Cross-process locked read-modify-write for whole-file JSON documents.
///
/// Why: `trusty-mpm`'s `projects.json`, `trusty-gworkspace`'s `tokens.json`
/// (#3502) and the worktree registry of epic #4207 are each a small JSON file
/// mutated by several independent PROCESSES via load → mutate → save. Without
/// cross-process serialisation those writers lose each other's updates, and a
/// shared scratch path lets them publish a corrupt document. This module is the
/// single implementation of that critical section.
/// What: Exposes [`json_rmw::update`], [`json_rmw::lock_path`], and
/// [`json_rmw::JsonRmwError`].
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// json_rmw::tests`.
pub mod json_rmw;

/// Shared CLI daemon-guard helper (probe + spinner + spawn).
///
/// Why: trusty-search, trusty-memory, and trusty-analyze each had an identical
/// probe-spawn-poll-spinner loop in their `commands/daemon_guard.rs` files.
/// Centralising it here (issue #985) removes the divergence risk and gives
/// the three crates a single tested implementation to delegate to.
/// What: Exposes [`daemon_guard::DaemonGuardConfig`],
/// [`daemon_guard::probe_once`], [`daemon_guard::spin_until_ready`], and
/// [`daemon_guard::spawn_current_exe`].
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// daemon_guard::tests`.
pub mod daemon_guard;

/// Daemon HTTP-address file helpers.
///
/// Why: Both trusty-search and trusty-memory persist their bound `host:port`
/// to disk for discovery by CLI and MCP clients. Centralising keeps them in sync.
/// What: Exposes [`daemon_addr::write_daemon_addr`], [`daemon_addr::read_daemon_addr`],
/// [`daemon_addr::check_already_running`],
/// [`daemon_addr::resolve_daemon_base_url`] (discovery-first `http://` base
/// URL resolution, issue #2033), and [`daemon_addr::daemon_socket_path`] — the
/// UDS counterpart a daemon binds and its consumers dial (#6277, ADR-0032).
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// daemon_addr::tests`.
pub mod daemon_addr;

/// HTTP health-probe helper.
///
/// Why: Every daemon uses the same tight-timeout `/health` probe to detect
/// whether a prior instance is still running.
/// What: Exposes [`health_probe::probe_health`].
/// Test: covered via daemon_addr integration tests.
pub mod health_probe;

/// The local-client credential a loopback daemon and its clients share
/// through a `0600` file (#5439).
///
/// Why: a loopback bind limits remote reach but establishes no identity among
/// local callers, so `trusty-code serve --http` served sessions, transcripts,
/// and every mutation route to any process on the machine. Server and clients
/// need one answer to "where is the token and what counts as valid"; a second
/// spelling of either is the defect the common-entry-point rule forbids.
/// What: Exposes [`daemon_token::token_path`], [`daemon_token::ensure_token`]
/// (server side — read or mint at `0600`), [`daemon_token::read_token`] /
/// [`daemon_token::read_token_at`] (client side, best-effort),
/// [`daemon_token::mint_token`], and [`daemon_token::credentials_match`] (the
/// constant-time comparison every verifier uses instead of `==`). The axum
/// enforcement half is `server::bearer_auth`, behind `axum-server`.
/// Read the module's honesty clause before describing the boundary this
/// establishes: a `0600` file is an OS-user and browser-origin boundary, not
/// isolation from an untrusted process running as the same uid.
/// Test: `cargo test -p trusty-common --features daemon-token -- daemon_token::`.
#[cfg(feature = "daemon-token")]
pub mod daemon_token;

/// The single HTTP-client constructor for loopback/daemon targets (#4392).
///
/// Why: reqwest 0.12 routes `127.0.0.1` through an exported `HTTP_PROXY` /
/// `ALL_PROXY` — hyper-util's matcher has no loopback exemption — so on a
/// machine with a proxy configured every tm↔daemon call fails and the caller
/// reports a healthy daemon as down. `.no_proxy()` fixes it, and it belongs at
/// ONE entry point rather than across ~133 `Client::builder()` sites.
/// What: Exposes [`http_client::loopback_client_builder`] (the primitive, no
/// timeout policy), [`http_client::loopback_client`] (standard bounds), and
/// `http_client::blocking_loopback_client_builder` behind `blocking-http` — the
/// last is left unlinked because it does not exist when that feature is off
/// (#6027).
/// Public-internet callers (crates.io, inference providers, the GitHub API)
/// deliberately do NOT route through here — they must keep honouring the
/// operator's proxy.
/// Test: `cargo test -p trusty-common --features unconditional-only --
/// http_client::tests`.
pub mod http_client;

/// Unix-domain-socket permission enforcement (issue #5099).
///
/// Why: ADR-0031 and ADR-0032 both argue for UDS over loopback TCP on the
/// strength of a `0600` socket, and no production code created one — every
/// `set_permissions` hit in the workspace was a test fixture. Four bind sites
/// each called `UnixListener::bind` bare. This is the single entry point they
/// now share, so the permission contract cannot drift between them.
/// What: Exposes [`uds::bind_hardened`] (`0700` directory, `0600` socket),
/// [`uds::prepare_socket_dir`], [`uds::scratch_socket_dir`] (the per-uid
/// replacement for the `$TMPDIR`-with-`/tmp`-fallback convention), and
/// [`uds::ensure_peer_is_self`] (`SO_PEERCRED` / `getpeereid`).
/// Test: `cargo test -p trusty-common --features uds uds::` — the `--features`
/// flag is load-bearing. `uds` is not a default feature, so the bare
/// `-p trusty-common` form compiles the module out and reports 0 tests run
/// while exiting 0. CI's `cargo test --workspace` enables it via feature
/// unification from `trusty-embedderd` and `trusty-bm25-daemon`.
#[cfg(all(unix, feature = "uds"))]
pub mod uds;

/// GitHub webhook HMAC-SHA256 verification (#5089 step 3, ADR-0034 §3).
///
/// Why: the check exists twice today and the two copies disagree on what an
/// unset secret means — `trusty-review` rejects, `trusty-analyze` processes the
/// payload anyway. ADR-0034 §3 collapses verification to one place (console)
/// and unifies the policy to fail-closed; this module is that place.
/// What: Exposes [`webhook_hmac::verify_github_signature`], its three-state
/// [`webhook_hmac::SignatureVerdict`], and [`webhook_hmac::sign_github_body`]
/// for test harnesses.
/// Test: `cargo test -p trusty-common --features webhook-hmac webhook_hmac::`.
#[cfg(feature = "webhook-hmac")]
pub mod webhook_hmac;

/// Console->target webhook relay wire contract (#5089 step 3, ADR-0034 §3).
///
/// Why: the sender (`trusty-console`) and the receivers (`trusty-review`,
/// `trusty-analyze`) cannot depend on each other, so a method name or field
/// list held by only one half is two copies waiting to drift.
/// What: Exposes [`webhook_relay::RELAY_METHOD`], the borrowed
/// [`webhook_relay::RelayFrame`] the sender writes, the owned
/// [`webhook_relay::RelayRequest`] a receiver reads, and
/// [`webhook_relay::RelayResponse`], whose `ack` is the only thing that
/// licenses the sender to delete its spool entry.
/// Test: `cargo test -p trusty-common --features webhook-relay webhook_relay::`.
#[cfg(feature = "webhook-relay")]
pub mod webhook_relay;

/// Global tracing subscriber initialisation helpers.
///
/// Why: Every trusty-* binary needs the same verbosity ladder, `RUST_LOG`
/// override, and (for daemons) the log-buffer + bug-capture layer composition.
/// What: Exposes [`tracing_init::init_tracing`],
/// [`tracing_init::init_tracing_with_buffer`],
/// [`tracing_init::init_tracing_with_buffer_and_capture`] (feature-gated),
/// and [`tracing_init::maybe_disable_color`].
/// Test: side-effecting global — covered by downstream integration tests.
pub mod tracing_init;

/// Deprecated single-shot OpenRouter helpers.
///
/// Why: Backward-compatible wrapper for the pre-streaming OpenRouter API.
/// New code should use `chat::OpenRouterProvider::chat_stream` instead.
/// What: Exposes [`openrouter_legacy::ChatMessage`],
/// [`openrouter_legacy::openrouter_chat`] (deprecated), and
/// [`openrouter_legacy::openrouter_chat_stream`] (deprecated).
/// Test: `chat_message_round_trips`, `openrouter_chat_rejects_empty_key`.
pub mod openrouter_legacy;

/// Incremental catch-up engine for the DOC-28 cutover bridge (#1762).
///
/// Why: when a native `tm` session starts, the operator needs a summary of
/// activity since the last session (paused sessions, git commits, memory palace
/// drawers). Hosting the engine here lets both trusty-mpm and (eventually)
/// trusty-code share it without code duplication.
/// What: gated behind the `catchup` feature (pulls in `rusqlite` via the
/// `mpm_registry` submodule). Exposes `catchup::{CatchupOptions, run_catchup,
/// run_catchup_blocking, generate_catchup_context, …}` plus per-source
/// submodules (`git`, `palace`, `state`, `session_finder`, `mpm_session`,
/// `mpm_registry`).
/// Test: `cargo test -p trusty-common --features catchup`.
// CUTOVER BRIDGE — remove post-migration (#1762)
#[cfg(feature = "catchup")]
pub mod catchup;

/// Why: two independent tmux implementations (trusty-mpm, trusty-agents)
/// meant the #2398/#2399 scrollback fix only reached one of them (issue
/// #3004). Always-on: it is a small, dependency-light (`serde` only) pure
/// command-construction layer, no feature gate needed.
/// What: `TmuxTarget`/`TmuxCommand`/`tmux_argv`, the scrollback-ergonomics
/// defaults, and the shared `managed_session_commands` ordering guarantee.
/// Test: `cargo test -p trusty-common --features unconditional-only -- tmux::`.
pub mod tmux;

/// The workspace's single entry point for invoking the GitHub CLI (#5475).
///
/// Why: `gh` was spawned from a dozen independent `Command::new("gh")` sites
/// across four crates, each re-deriving its own missing-binary,
/// unauthenticated, non-zero-exit and stderr policy — the exact duplication
/// the common-entry-point rule forbids, and one more copy was about to land
/// with #5487 / #5215.
/// What: gated behind the `gh-cli` feature. Exposes `gh::GhCommand` (builder
/// + blocking and tokio runners), `gh::GhOutput` (the full exit/stdout/stderr
/// triple, with the shared policies as combinators), `gh::GhError`, and the
/// `gh::gh_available` probe.
/// Test: `cargo test -p trusty-common --features gh-cli -- gh::`.
#[cfg(feature = "gh-cli")]
pub mod gh;

// ─── Re-exports preserving the pre-split public API ───────────────────────

pub use chat::{
    BedrockProvider, ChatEvent, ChatProvider, ChatUsage, DEFAULT_BEDROCK_MODEL, LocalModelConfig,
    OllamaProvider, OpenRouterProvider, SamplingParams, ToolCall, ToolDef,
    auto_detect_local_provider,
};

// Port
pub use port::bind_with_auto_port;

// Data directory
pub use data_dir::{DATA_DIR_OVERRIDE_ENV, is_dir, resolve_data_dir, sanitize_data_root};

// Daemon address
pub use daemon_addr::{
    check_already_running, daemon_socket_path, read_daemon_addr, remove_daemon_addr,
    resolve_daemon_base_url, write_daemon_addr,
};

// Health probe
pub use health_probe::probe_health;

// Panic logging (issue #4764)
pub use panic_hook::install_panic_logger;

// Tracing init
#[cfg(feature = "bug-capture")]
pub use tracing_init::init_tracing_with_buffer_and_capture;
pub use tracing_init::{init_tracing, init_tracing_with_buffer, maybe_disable_color};

// OpenRouter legacy (deprecated but must remain reachable)
#[allow(deprecated)]
pub use openrouter_legacy::{ChatMessage, openrouter_chat, openrouter_chat_stream};
