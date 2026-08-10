# Changelog

All notable changes to trusty-analyze are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versions correspond to `Cargo.toml` patch releases.

---

## [0.9.0] — 2026-08-10

### Breaking

- **`POST /webhooks/github` is retired and now returns 404** (#5181, ADR-0034). GitHub deliveries reach `trusty-analyze` only through `trusty-console`'s `POST /api/webhooks/{source}`, which verifies the HMAC once, spools the payload durably, and relays over UDS to `trusty-analyze webhook-listen`. The route is deleted rather than stubbed, so a delivery still aimed at it fails visibly at GitHub instead of being acknowledged and dropped. Anyone with a GitHub webhook pointed at `trusty-analyze` directly must repoint it at the console. The analysis pipeline is unchanged — the route's handler already delegated to `webhook_drain`, which the UDS path uses.
- **Removed public API:** `service::handlers::review::github_webhook_handler`, `core::verify_webhook_signature` (and `core::github::verify_webhook_signature`), and `AnalyzerAppState::{webhook_secret, with_webhook_secret}`. This crate no longer verifies a webhook signature at all; that is `trusty-console`'s single implementation (ADR-0034 §3), so the `hmac`, `sha2` and `hex` dependencies are dropped.
- `webhook_listener::run` now takes a `TrustySearchClient`, which it needs to run the analysis pipeline. Callers must pass the client they already build from `--search-url` (#5192).

### Added

- `trusty-analyze webhook-listen` binds `trusty-analyze-webhook.sock`, the socket `trusty-console` has been relaying verified GitHub deliveries to since #5089 step 3 with nothing on the other end. Each delivery is fsync'd to a durable inbox under the crate's data directory before the acknowledgement is written; an acknowledgement is what lets console delete its own copy, so nothing is acked that is not already held. The listener exits on SIGTERM, so the socket exists without the service running resident. Both the socket and the inbox root resolve from `trusty_common::webhook_relay` rather than being spelled here, so the directory this service writes to is by construction the one `trusty-console` meters for an undrained backlog. The legacy `POST /webhooks/github` route is unchanged.
- `GET /indexes/{id}/complexity_distribution` and the matching `complexity_distribution` MCP tool return the full A-F cyclomatic-complexity histogram over an index, with the counted total, in a payload bounded at five rows regardless of corpus size (#5320).

### Fixed

- `service install` evicts `com.trusty.trusty-analyze`, the label an older
  installer registered. The registry recorded it as a legacy alias and nothing
  acted on it, so the record meant nothing on a host that needed it (#4868)
- **SCIP graph overlays now survive a daemon restart** (closes [#5049](https://github.com/bobmatnyc/trusty-tools/issues/5049)). `POST /indexes/{id}/scip` wrote into an in-process `HashMap<String, KgGraph>` and answered HTTP 200; a restart discarded the ingest, and `GET /indexes/{id}/graph` then served a tree-sitter-only graph indistinguishable from one where the overlay had been applied. A SCIP index is uploaded by the operator and cannot be re-derived from the corpus, so the overlay is now written to a redb store (`scip_overlays.redb`, a sibling of the facts store — no new CLI flag).
- **A caller can now tell "no SCIP data" from "empty SCIP graph"**. `GET /indexes/{id}/scip` is new: 404 when nothing has ever been ingested for that index, 200 with `{index_id, nodes, edges, ingested_at}` when an overlay exists — including a legitimately symbol-free one, which reports `nodes: 0`. `GET /indexes/{id}/graph` carries the same fact as an `x-scip-overlay: present|absent` response header; its JSON body is still a bare `KgGraph`, so existing consumers are unaffected. A failure to read the overlay store is a 500 rather than a silent fall-through to the tree-sitter-only graph.
- **`POST /webhooks/github` now fails closed when no webhook secret is configured** (closes [#5173](https://github.com/bobmatnyc/trusty-tools/issues/5173)). With `GITHUB_WEBHOOK_SECRET` unset the handler logged `no webhook secret configured — skipping webhook signature verification` and processed the payload, so any local process that could reach the loopback port could inject arbitrary PR coordinates into the analyze pipeline and make the daemon fetch a diff and post a comment under the daemon's `GITHUB_TOKEN`. An unset or empty secret now returns 401 `webhook secret not configured` before the payload is parsed, matching `trusty-review`'s `handle_github_webhook`. Deployments that relied on the unauthenticated path must set `GITHUB_WEBHOOK_SECRET`; every other endpoint is unaffected and the daemon still starts without it.
- Scope: this closes the webhook route only. `POST /review/github-pr` still accepts arbitrary `owner`/`repo`/`pr` coordinates with no authentication and drives the same `GITHUB_TOKEN`; it is unchanged here.
- `trusty-analyze webhook-listen` now drains its webhook inbox into the analysis pipeline instead of holding acknowledged deliveries forever. The PR-event filter and the fetch/analyse/comment pipeline moved to `webhook_drain`, so the legacy `POST /webhooks/github` route and the UDS drain run one implementation.
- A delivery is never analysed twice. The shared drain's processed-delivery ledger closes the crash window that would otherwise post a duplicate PR comment (#5192).
- `GET /indexes/{id}/refactor-suggestions` no longer suggests refactors for files with no mapped language. Documents, FAQs, and CI workflow YAML were scored by the keyword text heuristic, graded F, and returned as critical "extract method" suggestions (#5317).

### Changed

- `LAUNCHD_LABEL` is read from `trusty_common::launchd_labels::ANALYZE` rather
  than restated beside the installer's separate copy of it. The value is
  unchanged — the point is that the installer's copy can no longer drift away
  from the daemon's, which is what broke trusty-search (#4868)
- **One shared open-with-quarantine policy for both redb stores**, in the new `core::redb_open` module (part of [#5049](https://github.com/bobmatnyc/trusty-tools/issues/5049)). `FactStore` already renamed a format-obsolete `facts.redb` aside as `*.v2-incompatible` and booted with a loud `ERROR` ([#702](https://github.com/bobmatnyc/trusty-tools/issues/702)); the new SCIP overlay store now does the same, quarantining as `*.quarantined`. Both classify the redb error first: an obsolete on-disk format is moved aside, while a transient failure to open — permissions, disk, a held lock — stays fatal, because recreating on top of a file that is merely unavailable would destroy data that is still good. Neither store deletes anything. This replaces a duplicated classifier, so the two stores cannot drift into giving opposite answers to the same byte-level cause.
- **Breaking (library API), part of [#5049](https://github.com/bobmatnyc/trusty-tools/issues/5049):** `AnalyzerAppState::scip_overlays` changed type from `Arc<RwLock<HashMap<String, KgGraph>>>` to the new `core::ScipOverlayStore`, and `AnalyzerAppState::new` / `AnalyzerAppState::with_registry` take it as a required argument. It is a constructor parameter rather than a `with_*` override so no caller can end up with a non-durable overlay store by omission — that omission was the bug.
- The MCP tool section of `README.md` and `CLAUDE.md` is now generated from
  `mcp::tool_descriptors()` plus `mcp::descriptors::review_tool_descriptors()`
  by `tests/generated_docs.rs`. The feature-dependent surface is stated as
  derived numbers — 19 tools with default features, 22 with `--features
  review` — with a per-row `Available` column, replacing prose that told the
  reader to go read `tool_descriptors()` because no fixed number was safe.
  Regenerate with
  `UPDATE_DOCS=1 cargo test -p trusty-analyze --test generated_docs` (#5205)
- `review_tool_descriptors()` moved from the `#[cfg(feature = "review")]`
  `mcp::review` module to `mcp::descriptors`, so the three `tr_review_*`
  descriptors compile in every build. Dispatch stays feature-gated and
  `tools/list` is unchanged in both configurations; the move is what lets a
  default build — the only one CI runs — verify the documented review rows
  (#5205)
- `README.md` keeps its HTTP-equivalents table hand-written, because the route
  a tool forwards to is not in the descriptors. It now sits outside the
  generated markers and every tool name in it is asserted to be real by
  `http_equivalents_name_only_real_tools` (#5205)

### Removed

- **BREAKING — the next release of this crate must be `0.9.0`, not `0.8.x`.**
  Removed the fastembed/ONNX neural clustering embedder and, with it, public
  API: `EmbedderKind::Neural`, `embedder::NeuralEmbedder`, the
  `bundled-ort` / `load-dynamic` / `cuda` Cargo features (`default` is now
  `["http-server"]`), and `ClusterQueryParams::method`'s type (now
  `Option<String>`, validated in the handler). CI cannot detect a SemVer break
  (#4088 — the gap that got 0.7.3 yanked), so this line is the record a
  releaser has to act on. Nothing selected `method=neural` —
  `trusty-console`, the `cluster_concepts` MCP tool and the embedded UI all
  used the `bow` default — yet the daemon constructed the model at every boot,
  and the untimed Hugging Face request that construction made blocked the
  listener for as long as the request took (31m46s in one production boot;
  reproduced at 60.17s and 120.13s against a stub HF endpoint with matching
  injected delays, versus 0.20s after the fix). `bow` is now the sole
  embedder, `--fastembed-cache` is an accepted no-op so existing launchd
  plists keep starting, and `?method=neural` returns 400 instead of BOW
  vectors labelled `neural` (#5067)

## [0.8.0] — 2026-07-27

MINOR, not the patch 0.7.5 this was originally staged as (#4177). This crate
publicly re-exports `trusty-common` types — `src/types/entity.rs:14-15`:

```rust
pub use trusty_common::symgraph::contracts::EdgeKind;
pub use trusty_common::symgraph::{fact_hash_str, EntityType, RawEntity};
```

surfaced unconditionally as `trusty_analyze::types::{EntityType, RawEntity,
EdgeKind, fact_hash_str}` (`lib.rs:82 pub mod types` → `types/mod.rs:14 pub mod
entity` → `types/mod.rs:21 pub use entity::{…}`; no `cfg`, and the
`trusty-common/symgraph` feature is enabled unconditionally). Raising the
`trusty-common` requirement from `^0.26` to `^0.27` therefore changes the
*identity* of publicly re-exported types.

At patch level that is a break shipped silently: `^0.7.4` re-resolves
already-published consumers onto the new type identity, and a consumer that
also depends on `trusty-common 0.26` directly gets two semver-incompatible
copies linked at once and mismatched-type errors against
`trusty_analyze::types::EntityType`. That is bit-for-bit the defect that forced
the **0.7.3 yank** on this very crate. `^0.7` excludes 0.8.0, so published
consumers keep resolving to 0.7.4 and stay installable.

### Changed

- `trusty-common` requirement raised to `^0.27` (was `^0.26`, inherited from
  `[workspace.dependencies]`): 0.27.0 makes `ChatEvent` `#[non_exhaustive]`,
  which a `^0.26` requirement cannot express. Because the re-exports above are
  public, this requirement change is itself the reason for the MINOR level.

### Fixed

- **Post-publish source drift — `explain` did not compile against the
  `ChatEvent::Usage` variant.** `src/core/explain.rs` gained its
  `ChatEvent::Usage(_)` arm in #4112, *after* 0.7.4 was published, so the
  published 0.7.4 artifact cannot build against any `trusty-common` carrying
  that variant. This is the same failure shape as #4079 below — a downstream
  exhaustive `match` broken by an upstream variant addition — and this release
  ships the arm before it can bite a `cargo install`. The match also gained a
  wildcard arm now that `ChatEvent` is `#[non_exhaustive]`, so the next variant
  addition is no longer breaking here.

---

## [0.7.4] — 2026-07-27

### Fixed

- **`cargo install trusty-analyze` failed to compile with `error[E0063]: missing
  field `no_spawn_hint` in initializer of `DaemonBridgeConfig``**
  ([#4079](https://github.com/bobmatnyc/trusty-tools/issues/4079)): a fresh
  install of the previously-published `0.7.3` could not be built at all. `0.7.3`
  was published 2026-07-07 and declared `trusty-common = "0.22.0"` — a caret
  range `[0.22.0, 0.23.0)`. Five days later, `trusty-common` 0.22.5 added the
  public field `no_spawn_hint` to `DaemonBridgeConfig` (a SemVer-breaking
  public-field addition shipped in a *patch* bump; the struct carries neither
  `#[non_exhaustive]` nor a `Default` impl, so a struct literal missing a field
  is a hard compile error). `cargo install` re-resolves without a lockfile, so it
  picked the newest in-range `0.22.x` and paired 0.7.3's pre-field source with a
  post-field dependency. This release carries the corrected call site
  (`crates/trusty-analyze/src/commands/daemon_guard.rs` now sets
  `no_spawn_hint: None`) and declares `trusty-common ^0.26.0`, verified against
  the live registry with `cargo publish --dry-run`. Workspace CI never caught
  this because every member consumes `trusty-common` through a path dependency
  plus a root `[patch.crates-io]` override, so caller and definition are
  permanently in lockstep and no version resolution ever happens locally.
  Users on 0.7.3 could work around it with `cargo install trusty-analyze --locked`.

- **Smell-count false positives made the codebase-wide quality metric untrustworthy**
  ([#3522](https://github.com/bobmatnyc/trusty-tools/issues/3522)): the
  whole-codebase quality report and PR-review path (`core/quality.rs`,
  `core/review/mod.rs`) always scored chunks with the language-agnostic text
  heuristic, which over-counts `DeepNesting` on ordinary, idiomatically
  formatted Rust (e.g. `for (i, x) in v.iter().enumerate()`) and flags
  `MissingDocstring` on every undocumented function regardless of visibility.
  Both paths now dispatch through the existing language-aware
  `compute_complexity_for` (tree-sitter-backed for Rust/TypeScript), and
  `MissingDocstring` now only fires for public API surface (`pub` items in
  Rust; exported functions / non-private class methods in TS/JS). Measured on
  this repo's own `crates/` tree: smell count dropped from 21,407 to 8,906
  across ~47.8K chunks (0.448 → 0.186 smells/chunk).

### Changed

- **UI tokens now CI-enforced against the canonical Foundry source** (refs [#3486](https://github.com/bobmatnyc/trusty-tools/issues/3486)): flipped from the `scripts/check_token_drift.mjs` allowlist to ENFORCED. The `token-drift` CI job now compares `ui/src/lib/styles/tokens.css`'s plain-CSS `--trusty-*: #hex` values directly to `docs/design/UI/design-system/tokens.css` on every push/PR (light `[data-theme="light"]`, dark `:root, [data-theme="dark"]`; the crate-local alias layer is ignored), so a hand-edit that drifts this crate's palette from canonical fails the build.
- **UI design tokens migrated to Foundry v2** ([#3490](https://github.com/bobmatnyc/trusty-tools/issues/3490),
  epic [#3486](https://github.com/bobmatnyc/trusty-tools/issues/3486)): the dashboard's
  `tokens.css` now sources its light/dark palette from the canonical Foundry
  v2 ("rust-on-paper") design tokens instead of Catppuccin Mocha/Latte. The
  crate's existing component-facing alias names (`--bg`, `--border`,
  `--text`, `--grade-*`, etc.) and light/dark activation mechanism
  (`[data-theme]` on `<html>`) are unchanged — only the underlying color
  values moved.

### Security

- **Router-wide same-origin (CSRF) write guard** ([#3304](https://github.com/bobmatnyc/trusty-tools/issues/3304)):
  destructive write routes (`POST /indexes/{id}/scip`, `POST /review`,
  `POST /analyze/deep`, `POST /facts`, `DELETE /facts/{id}`, the GitHub webhook)
  are now guarded against cross-origin browser requests via the shared
  `trusty_common::server::with_guarded_middleware`. Method-gated (GET reads and
  `/sse` unaffected) and fail-open on a missing `Origin` (the console proxy,
  `curl`, and GitHub's server-side webhook POST keep working).

## [0.7.3] — 2026-07-09

### Changed

- Version reconcile to match already-published crates.io state; no functional change.

## [0.7.2] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-analyze` now produces
  the `trusty-analyze` binary only. Install the console with
  `cargo install trusty-console`. This is part of the single-owner-per-binary
  fix for the cargo binary-ownership collisions (#1262).

## [0.7.0] — 2026-06-09

### Added

- **`tools_run` / `tools_unavailable` fields in `run_diagnostics` response
  (#915)** — `DiagnosticsResponse` (HTTP) and the `run_diagnostics` MCP tool
  result now include:
  - `tools_run: Vec<String>` — names of analysis tools that executed
    successfully.
  - `tools_unavailable: Vec<String>` — names of tools that were requested but
    could not run (binary missing, feature disabled, index has no data, etc.).
  These are additive fields; no existing fields were removed or renamed.
  Callers that previously used an empty `diagnostics` list to infer
  "nothing ran" now get an explicit signal.

- **Distinguish clean from tools-unavailable in `run_diagnostics` (#915)** —
  `run_diagnostics` now correctly distinguishes between "all tools ran, no
  issues found" (clean) and "tools were unavailable so nothing ran"
  (degraded). Previously both cases returned an empty diagnostics list with
  no indication of which had occurred.

### Fixed

- **CALLS edge targets resolved via qualified node-id scheme across all 13
  language adapters (#913)** — the call-graph builder now uses the same
  qualified `<language>/<path>/<symbol>` node-id scheme that the entity
  linker uses when resolving callee targets. Previous adapters emitted bare
  symbol names as CALLS targets, which never matched any node in the graph,
  resulting in zero CALLS edges in `extract_graph` results. All 13 adapters
  (Rust, Python, TypeScript, JavaScript, Java, Go, C, C++, C#, Kotlin, PHP,
  Ruby, Scala) are fixed.

### Note on behavior change

The `omit_content` default changed to `true` in 0.6.0 (released same day).
Callers that relied on `content` being present in smells responses must add
`omit_content=false`. This bump to 0.7.0 (MINOR) is driven by the additive
`tools_run`/`tools_unavailable` response envelope.

---

## [0.6.0] — 2026-06-09

### Added

- **`limit` / `offset` / `omit_content` query parameters on smells + diagnostics
  endpoints (#917/#918)** — `GET /indexes/:id/smells` and
  `GET /indexes/:id/diagnostics` now accept:
  - `limit` (default 500, max enforced server-side)
  - `offset` (default 0, for cursor-style pagination)
  - `omit_content` (default **`true`** — see Changed below)
  The response body gains a pagination envelope:
  `{ total, returned, truncated, items: [...] }`.
  `find_smells` and `run_diagnostics` MCP tool input schemas are extended
  with the same three parameters and wired through `build_query()`.

- **MCP stdio size guard (#917)** — `guard_response_size()` in `mcp/stdio.rs`
  checks the serialised response length against
  `TRUSTY_MCP_MAX_RESPONSE_BYTES` (default **2 MB**, read at startup) before
  `write_all`. Oversized payloads are replaced with a well-formed
  `isError: true` truncation notice — the JSON-RPC `id` is preserved in the
  notice so the caller can correlate it — so an over-limit response can never
  kill the MCP session with `-32000`.

- **`build_query()` numeric/bool value support** — the MCP dispatch helper now
  parses JSON `Number` (u64) and `Boolean` values, not just strings, so
  `limit`, `offset`, and `omit_content` fields from MCP tool calls are
  forwarded correctly to the HTTP layer.

### Changed

- **`omit_content` defaults to `true` (behavior-affecting default change)** —
  `SmellItem` serialisation omits the raw chunk `content` field by default.
  This reduces typical smells payloads from multi-megabyte to tens of
  kilobytes. Pass `omit_content=false` (HTTP) or `"omit_content": false` (MCP)
  to restore the full text. **Callers that previously relied on `content`
  being present in smells responses must add `omit_content=false`.**

- **`omit_content` removed from `run_diagnostics` / `DiagnosticsParams`** —
  `ToolDiagnostic` carries no raw source body, so the field was a no-op.
  Removing it prevents callers from being misled into believing it affected
  output.

### Fixed

- **#917 — over-limit smells/diagnostics responses crashed the MCP session** —
  replaced unbounded `GET /smells` serialisation with the paginated,
  omit-content-by-default path described above; the stdio size guard provides
  an additional safety net at the transport layer.

- **JSON-RPC `id` echoed in stdio size-guard truncation notice** — previously
  the notice hardcoded `"id": null`, violating JSON-RPC 2.0 §5. The guard
  now parses the id from the oversized bytes and echoes it.

---

## [0.5.1] — 2026-06-07

### Added

- **Prebuilt binary distribution via GitHub Releases** — the `trusty-analyze`
  binary is now published to GitHub Releases on every tagged version for
  `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, and
  `x86_64-unknown-linux-gnu` (Amazon Linux 2023 `load-dynamic` variant).
  Install without Rust toolchain:
  ```
  curl -L https://github.com/bobmatnyc/trusty-tools/releases/download/trusty-analyze-v0.5.1/trusty-analyze-aarch64-apple-darwin.tar.gz | tar xz
  ```
  or via `cargo install --git`:
  ```
  cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze --locked
  ```
- **Cargo.toml packaging metadata** — added `exclude`, `keywords`, `categories`,
  and `[package.metadata.docs.rs]` so docs.rs renders the full API surface
  (including the `http-server` feature) and the crates.io page is correctly
  categorised.
- **Expanded `lib.rs` module-level docs** — top-level rustdoc now covers the
  analysis pipeline, transport options (HTTP API + MCP stdio/SSE), feature
  flags (`http-server`, `bundled-ort`, `load-dynamic`, `cuda`, `ner`, `review`),
  and quickstart examples.
- **CHANGELOG backfill** — all patch releases since 0.1.0 documented with
  accurate dates and descriptions.

### Changed

- **Workspace MIT relicense** — the workspace `license` field was changed from
  `Elastic-2.0` to `MIT`; `trusty-analyze` inherits `license.workspace = true`
  and is now MIT-licensed.

---

## [0.5.0] — 2026-06-03

### Added

- **redb 4.x + facts store recovery** (#702) — facts store upgraded to redb 4.x
  with graceful incompatible-file recovery: existing redb 2.x `facts.redb` is
  backed up to `facts.redb.v2-incompatible` and recreated on first start.

- **Optional `review` feature exposing trusty-review MCP tools** (#630/#631) —
  a new `review` Cargo feature wires trusty-review's MCP tool surface into the
  trusty-analyze daemon, enabling PR-review tools without a separate process.

- **On-demand SubprocessAnalyzeClient + facts store** (#632) — the analysis
  client now supports on-demand subprocess invocation for environments where a
  persistent daemon is not running.

- **Dashboard auto-start** (#684) — the web UI dashboard auto-starts on first
  daemon launch without requiring a manual invocation.

> **OPERATOR NOTE:** Existing `facts.redb` is backed up to
> `facts.redb.v2-incompatible` and recreated empty on first start after upgrade.
> No analysis history is stored in the facts store; only user-authored facts are
> affected.

---

## [0.4.2] — 2026-06-02

### Fixed
- **Amazon Linux 2023 / glibc < 2.38 build failure** (closes #605): the
  prebuilt ONNX Runtime bundled via `fastembed/ort-download-binaries` (ORT
  1.24.2, compiled against glibc 2.38) caused a link-time `__isoc23_strtol`
  unresolved-symbol error on AL2023 (glibc 2.34). The `load-dynamic` Cargo
  feature — introduced in #536 — bypasses the static bundle entirely and lets
  `ort` dlopen a system-installed `libonnxruntime.so` at runtime. README now
  documents the full three-step AL2023 installation procedure including the
  exact ORT version to download and how to set `ORT_DYLIB_PATH`.

---

## [0.4.1] — 2026-06-01

### Added
- **Load-dynamic ORT feature for glibc < 2.38** (closes #536) — a new `load-dynamic`
  Cargo feature lets `ort` dlopen a system-installed `libonnxruntime.so` instead of
  linking the bundled static library, enabling installation on Amazon Linux 2023
  (glibc 2.34) and other older-glibc hosts.

### Added
- **AWS Bedrock LLM provider for deep-analysis pass** (closes #530) — the
  `POST /analyze/deep` endpoint and `deep_analysis` MCP tool now route LLM calls
  through AWS Bedrock when `TRUSTY_LLM_MODEL` starts with `bedrock/`. Auth uses
  the standard AWS credential chain; no OpenRouter key is needed.

### Fixed
- **MCP `deep_analysis` timeout** (closes #528) — raised the timeout above
  OpenRouter's 120 s limit; improved error messaging when the API key is absent.

### Changed
- Excluded `ui/node_modules` from cargo package; fixed `.gitignore` for the
  embedded UI source tree.

---

## [0.3.0] — 2026-06-01

### Added
- **Connection-safe daemon upgrades** (closes #534) — graceful shutdown drains
  in-flight requests before exit; the `mcp_bridge` binary reconnects with
  exponential backoff after a restart. Use `launchctl bootout` (SIGTERM), not
  `kickstart -k` (SIGKILL), when upgrading.

---

## [0.2.1] — 2026-05-31

### Fixed
- Repaired `LaunchdConfig` build break introduced in 0.2.0.
- Added `reqwest` timeouts to all outbound HTTP calls to trusty-search.
- `spawn_blocking` used for neural-embedding calls to avoid blocking the async runtime.

---

## [0.2.0] — 2026-05-29

### Added
- **Update-check helper** (closes #455) — CLI notifies the user when a newer
  version of `trusty-analyze` is available on crates.io.
- **Declarative CLI help system** (closes #216) — structured `help.yaml` with
  `suggest` completing unknown subcommands; wired into all user-facing CLIs.
- **`axum` behind feature flag** (closes #249) — `axum` and `tower-http` are now
  optional behind the `http-server` feature flag, matching the convention
  established in `trusty-common`. Library consumers can drop the HTTP stack with
  `default-features = false`.
- Documentation migrated from in-crate to top-level `docs/trusty-analyze/`.

---

## [0.1.10] — 2026-05-22

### Fixed
- Routed all daemon output to stderr (MCP stdio framing requires a clean stdout).
- Resolved `list_facts` read-lock contention under concurrent MCP requests (#66, #67).

### Changed
- Included `ui/dist` and the MCP stdio harness in the release binary tarball.

---

## [0.1.6] — 2026-05-20

### Changed
- Adopted `trusty-common` `LaunchdConfig` and `claude_config` helpers in the
  service/setup module (closes #3), eliminating duplicate macOS service-install
  logic.

---

## [0.1.5] — 2026-05-20

### Changed
- Renamed crate from `trusty-analyzer` to `trusty-analyze` for consistency with
  the rest of the `trusty-*` ecosystem.

---

## [0.1.2] — 2026-05-11

### Added
- Light / dark / system theme support with Catppuccin Latte + Mocha palettes
- Svelte 5 dashboard with D3 visualizations and SSE live updates
- launchd service install/uninstall/status/logs subcommands (macOS)

### Fixed
- Dashboard now validates selected index against trusty-search index list; stale localStorage selections are cleared on refresh
- Empty-state guidance when no indexes are registered: "run trusty-search index <path>"

---

## [0.1.0] — full Phase 1 + Phase 2 static analysis engine

### Added — Phase 1 (static analysis engine, HTTP API, MCP server)

- **trusty-analyzer-core**: full analysis pipeline wired end-to-end
  - `client.rs` — reqwest HTTP client fetching `GET /indexes/:id/chunks` from trusty-search
  - `complexity.rs` — cyclomatic and cognitive complexity via tree-sitter AST walk
  - `blame.rs` — `git log --follow` parser + temporal decay scoring
  - `quality.rs` — grade aggregation (A–F) over ComplexityMetrics per file and index
  - `facts.rs` — `FactStore` backed by redb with upsert / query / delete
- **trusty-analyzer-service**: axum HTTP sidecar on port 7879
  - `GET /health` — liveness + trusty-search reachability check
  - `GET /indexes` — proxied from trusty-search
  - `GET /indexes/:id/complexity_hotspots[?top_k=N]`
  - `GET /indexes/:id/smells[?category=<name>]`
  - `GET /indexes/:id/quality`
  - `GET /facts[?subject=<s>&predicate=<p>]`
  - `POST /facts`
  - `DELETE /facts/:id`
- **trusty-analyzer-mcp**: MCP stdio server with 7 tools
  (`analyzer_health`, `complexity_hotspots`, `find_smells`, `analyze_quality`,
  `list_facts`, `upsert_fact`, `delete_fact`)
- **CLI subcommands**: `serve`, `analyze`, `facts list`, `facts upsert`, `health`
- Daemon PID lockfile (fs4), graceful shutdown, `--search-url` flag
- Integration test suite: self-analysis suite validating the static pipeline on
  own source tree

---

### Added — Phase 2 (language-specific static enrichment)

- **`LanguageAnalyzer` trait**: `detect` / `parse_static` / `enrich_semantics` lifecycle
  interface; concrete adapters plugged in without touching the orchestration layer
- **Tree-sitter adapters**: complete implementations for Python, Java, Go (complexity,
  smells, quality grade); Rust / TypeScript / C / C++ scaffolded
- **Knowledge Graph Phase 2**: CALLS edges extracted from Rust adapter via tree-sitter
  function-call pattern matching; cross-chunk entity linker resolves symbol references
  across file boundaries
- **k-means concept clustering** (bag-of-words): `linfa` k-means over TF-IDF vectors;
  `GET /indexes/:id/clusters?k=N&method=bow` endpoint
- **Neural clustering**: fastembed-backed embedding backend for `method=neural`
  clustering; uses model cached by trusty-search
- **SCIP protobuf ingest** (`#47`): `POST /indexes/:id/scip` accepts a serialized SCIP
  index protobuf; ingests occurrence → definition mappings into the knowledge graph for
  IDE-grade symbol resolution

#### New HTTP endpoints (Phase 2)

```
POST /indexes/:id/scip
     body: SCIP protobuf (application/octet-stream)
     → { symbols_ingested: N }

GET  /indexes/:id/clusters?k=N&method=bow|neural
     → Vec<ConceptCluster> (label, chunk_ids, centroid_terms)
```

#### New MCP tools (Phase 2)

| Tool | Equivalent endpoint |
|------|---------------------|
| `ingest_scip` | `POST /indexes/:id/scip` |
| `cluster_concepts` | `GET /indexes/:id/clusters` |

---

### Testing

- 107 tests passing across workspace (`cargo test --workspace`)
- Integration self-analysis suite covers HTTP API, MCP tools, SCIP ingest, clustering

---

[Unreleased]: https://github.com/bobmatnyc/trusty-tools/compare/trusty-analyze-v0.5.1...HEAD
[0.5.1]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.5.1
[0.5.0]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.5.0
[0.4.2]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.4.2
[0.4.1]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.4.1
[0.3.0]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.3.0
[0.2.1]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.2.1
[0.2.0]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.2.0
[0.1.10]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.10
[0.1.6]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.6
[0.1.5]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.5
[0.1.2]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.2
[0.1.0]: https://github.com/bobmatnyc/trusty-tools/releases/tag/trusty-analyze-v0.1.0
