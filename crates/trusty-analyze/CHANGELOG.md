# Changelog

All notable changes to trusty-analyze are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versions correspond to `Cargo.toml` patch releases.

---

## [0.12.6] — 2026-09-03

### Changed

- **The MCP dispatcher dials the daemon through the shared `trusty_mcp::daemon_bridge_json_rpc`.** `mcp::rpc_client::call` built its own JSON-RPC frame, called `trusty_common::uds::send_framed_request_capped` and unpacked an `RpcResponse` by hand — the same transport trusty-memory's stdio bridge carried a second copy of. Both now run one implementation ([#6316](https://github.com/bobmatnyc/trusty-tools/issues/6316))
  - The 32 MiB response budget and `core::mcp_client_timeout()` are unchanged; they are passed to the bridge rather than to the client. No request rewriter: the dispatcher has already built the exact `params` each `analyze.*` method expects
  - `mcp::stdio::run` is untouched. This crate's MCP surface is a tool translator with its own `tools/list` and its own #917 response-size guard, not an envelope forwarder, so the stdio loop stays where it is
  - A transport failure and a daemon-side JSON-RPC error still both surface as `DispatchError::Transport` naming the failing method. The message now carries the daemon's error code as well: `<method> over <socket>: <message> (<code>)`

## [0.12.5] — 2026-09-02

### Breaking

- `RefactorSuggestion` gains a public `region_kind` field, so an exhaustive
  struct literal built outside this crate no longer compiles.
- `core::refactor::analyze` (re-exported as `core::analyze_refactor`) takes an
  eighth parameter to carry the region kind through.
- Both are what move the version to 0.11.0 rather than 0.10.1; for a 0.x crate
  the breaking position is MINOR.
- The daemon no longer binds `127.0.0.1:7879`. It serves JSON-RPC 2.0 over
  `<data dir>/trusty-analyze/trusty-analyze.sock`, which every consumer derives
  through `trusty_common::daemon_socket_path` rather than reading a
  written-down address (#6287, ADR-0032). The `http_addr` discovery file is
  gone with it. `serve --port` and `serve --mcp-port` are accepted, hidden, and
  ignored with a warning rather than removed: the launchd plist on every
  machine that installed before this change still passes `--port 7879`, a
  `cargo install` does not rewrite it, and clap exiting 2 under
  `KeepAlive::Always` is a permanent crash loop with nothing in the logs but a
  usage message. `serve --socket` overrides the derived path.
- `trusty-analyze port` is replaced by `trusty-analyze socket`. The path
  resolves whether or not a daemon is running, so the new command reports
  liveness too and exits non-zero when nothing answers — preserving the
  property that `$(trusty-analyze socket)` fails rather than handing a caller a
  path to a dead socket.
- `service::routes`, `service::ui` and the axum router are replaced by
  `service::rpc`. `service::events::DEFAULT_PORT` is removed, and
  `ApiErrorKind` replaces the `axum::http::StatusCode` the handlers reported
  through.
- `--mcp-port` and the `/sse` broadcast are DELETED, not ported. `/sse`'s only
  subscriber was this daemon's own SPA; `--mcp-port` had no in-repo consumer at
  all and was a second ADR-0032-forbidden HTTP surface.
- The embedded UI is not served by this daemon any more. `ui/dist` stays
  tracked; the console-hosted mount is follow-up work.
- `commands::daemon::handle_start` no longer takes a `socket` parameter. It
  probed the socket it was handed but always spawned a child that derived its
  own, so a non-default path would have been probed and reported while a
  different one was served. It resolves the single path itself now.

### Added

- **New `scip_status` MCP tool wraps `analyze.scip_status`** — an MCP caller can now distinguish an index with no SCIP overlay ingested from one whose overlay carried zero symbols, the same distinction `GET /indexes/{id}/scip` gave HTTP callers in #5054. `extract_graph`'s `scip_overlay` flag already carried this through the JSON-RPC body since #6287; this tool adds the dedicated node/edge/ingested_at lookup MCP had no way to reach ([#5056](https://github.com/bobmatnyc/trusty-tools/issues/5056))
- Refactor suggestions carry a `region_kind` for Python, distinguishing a `class_body` from a `function`. Every other language emits no key at all, so their payloads are unchanged.
- `service::events::CODE_DEADLINE_EXCEEDED` (`-32005`) so a handler that
  exhausted its own deadline stays distinguishable from one that broke —
  trusty-review reads the code to print "ran out of time" rather than "could not
  be reached". `CODE_NOT_FOUND` (`-32004`) preserves #5049's
  ingested-but-empty distinction across the transport change.
- `service::rpc::METHODS`, the list the four crates that dial these names by
  literal are checked against, and `tests/uds_consumer_contract.rs`, which
  stands the daemon up on a temp socket and asks each of them what it sees.
- `trusty-analyze doctor` warns when a retired LaunchAgent plist is still on
  disk (`~/Library/LaunchAgents/com.trusty.analyze.plist` from a pre-#6350
  install), naming `trusty-analyze service uninstall` as the way to clear it.
  The check only reports; it never deletes (#6621).
- `trusty-analyze version` subcommand, so `tctl doctor trusty-analyze
  --self-check` can spawn `version --json` instead of failing on a clap usage
  error. `--json` emits the DOC-1 capability-discovery envelope
  (`contract_version`, `tool_version`, `verbs`); without it, a one-line
  `trusty-analyze v<version>` (#6631).
- `trusty-analyze report --manifest <path> [--template cast] [--code-only]` and the matching `tr_report` MCP tool generate a technical due-diligence report over the embedded trusty-review pipeline, under the existing `review` feature. Both call `trusty_review::report::run_report` rather than reimplementing manifest loading, template precedence, or the credential preflight. (#6669)

### Fixed

- `trusty-analyze health`, `daemon status` and the `setup` readiness poll no
  longer report a healthy daemon as DOWN when `HTTP_PROXY` is exported. All
  three build through `trusty_common::http_client`, which applies `.no_proxy()`
  (#4392).
- **The dashboard misrouted every API call on a reload of a hash-routed URL (`/ui/#/`).** `computeBase()` in `ui/src/lib/base.js` ran its `$`-anchored `index.html` / `ui/` strips against the raw `document.baseURI`, which includes the URL fragment, so the `ui/` mount segment survived and API paths resolved under `/ui/` — onto the SPA catch-all, which answers `200 text/html` (closes [#4980](https://github.com/bobmatnyc/trusty-tools/issues/4980))
  - trusty-analyze mounts its SPA at `/ui/` (`src/service/routes.rs`) with the JSON API as siblings at the daemon root, so it had the identical defect to trusty-search rather than a merely theoretical one
  - the strips now run against `new URL(document.baseURI).pathname`, which carries neither fragment nor query, re-joined to `origin`. The `window.__ANALYZER_BASE__` override branch and the non-browser guard are unchanged
  - same fix as trusty-search, per the KEEP IN SYNC contract on this file; the committed `ui/dist/` bundle is regenerated, since CI and release set `SKIP_UI_BUILD=1` and ship whatever is committed
- **`--features review` pulled in trusty-review's entire default feature set, including a contributor-profile pipeline this crate never calls.** The `trusty-review` dependency carried no `default-features = false`, so enabling `review` transitively compiled `tga`, `rusqlite`, and a vendored libgit2 with no source-code trigger anywhere in trusty-analyze. It now takes `default-features = false` and gets only the `mcp` feature the `review` gate already names ([#5466](https://github.com/bobmatnyc/trusty-tools/issues/5466))
  - this had to land with the removal itself: trusty-review 0.16.0 deletes the `profile` feature, which would otherwise have broken `--features review` in a crate whose source nobody touched
- **`GET /indexes/{id}/diagnostics` ran unbounded and clients got zero bytes instead of a response.** The handler awaited one `spawn_blocking` that looped every unique file and spawned one subprocess per file-scoped tool with no per-request deadline; on the 4097-file trusty-tools index that ran past ten minutes and every client abandoned the connection at the transport layer ([#6018](https://github.com/bobmatnyc/trusty-tools/issues/6018))
  - the dispatch now takes a wall-clock deadline and checks it between subprocess spawns, so it stops mid-corpus and returns what it has. The response carries `timed_out` plus a `cutoff` object naming the files never reached and the tools never invoked — a truncated list can no longer read as a clean corpus
  - the deadline defaults to 180 s and is tunable with `TRUSTY_DIAGNOSTICS_DEADLINE_SECS`. Past that budget plus a 30 s grace the handler answers HTTP 504 with a JSON body saying which request was abandoned, rather than holding the connection
  - `service/routes.rs` layers a blanket `tower_http::timeout::TimeoutLayer` as a last-resort net under every non-streaming route. `/sse` is merged in after the layer so the event stream is not cut off
  - the four timeouts between a client and a `cargo clippy` subprocess now derive from one place, `core::deadlines`, instead of being independent hardcoded constants. Each rung is computed from the configured deadline, so raising `TRUSTY_DIAGNOSTICS_DEADLINE_SECS` cannot invert the ordering — a fixed 300 s router timeout used to lose to the handler's own budget at any deadline above 270 s, which handed the client the layer's empty-bodied 504 instead of the handler's structured JSON on the exact remediation path the error message recommends

- **The MCP `run_diagnostics` tool still timed out with no body in the default configuration.** `AnalyzerMcpServer`'s HTTP client used a flat 150 s request timeout, below the 180 s diagnostics deadline, so any run between the two produced a transport error rather than the daemon's answer — the original symptom, one layer further out ([#6018](https://github.com/bobmatnyc/trusty-tools/issues/6018))
  - the client timeout is now the outermost rung of the same ladder, with a floor that keeps `deep_analysis` above the 120 s OpenRouter ceiling regardless of how low the diagnostics deadline is set

- **A project-scoped build could outlive the request that asked for it.** The deadline gated only whether `run_project` STARTED; inside, each `cargo clippy` or `dotnet build` ran under a flat `build_tool_timeout()` (300 s default, and two spawns per project for Roslyn). Because `spawn_blocking` cannot be cancelled, one cold project — or several in series — kept building for multiples of that after the client had its 504, and a client retry stacked another build behind it on the same toolchain lock ([#6018](https://github.com/bobmatnyc/trusty-tools/issues/6018))
  - `StaticTool::run_project` now takes the request deadline. Clippy and Roslyn cap each subprocess at `min(remaining budget, build_tool_timeout())` and recheck between project roots, so the existing kill-on-timeout path terminates the child when the request ends instead of 300 s later. The default `run_project` checks the deadline between files

- **`cargo clippy` was invoked once per Rust file in a directory with no `Cargo.toml`, so it could never produce a diagnostic.** Every invocation errored with "could not find Cargo.toml" and returned `Ok(vec![])` while still costing ~0.155 s, which made a structurally useless tool the endpoint's main cost driver ([#6018](https://github.com/bobmatnyc/trusty-tools/issues/6018))
  - `ClippyTool` is now project-scoped, like the existing Roslyn tool: the dispatcher hands it real on-disk paths and calls `run_project` once per request instead of `run` once per file
  - `run_project` groups the files by their enclosing cargo root — the workspace root when one exists, so a 21-crate workspace is one build and not 21 — runs `cargo clippy --workspace` there under the build-class timeout, parses that output once, and keeps the diagnostics belonging to the requested files
- The chunk export now walks trusty-search's cursor pagination (`?after=`) rather than offset pagination, and refuses an export that falls short of the `total` the daemon reports. Offset mode reads trusty-search's in-memory chunk cache — a map that is evicted after 300s idle, rehydrated on a detached task the request does not wait for, and capped by `TRUSTY_MAX_CHUNKS` — so a cold or unreadable corpus answered HTTP 200 with an empty page and every analyze endpoint then asserted a confident zero. Refs #6043, #5917.
- `build.rs` keeps the committed `ui/dist/` bundle instead of rebuilding it on every cold build. It used to run the package manager's install and a full `vite build` unconditionally, and both write files git tracks: `vite build` empties `ui/dist/`, deleting the tracked `ui-source-hash.txt` the publish-time freshness gate reads, and the pnpm-absent `npm install` fallback rewrote `ui/package-lock.json` with the host platform's optional-dependency set. Freshness is decided by `scripts/check-ui-bundle-freshness.sh`, the same check `preflight-publish.sh` runs, and an unreadable answer keeps the committed bundle rather than rebuilding it. `FORCE_UI_BUILD=1` rebuilds unconditionally and re-stamps the bundle afterwards, which is what a UI change now needs. Backported from trusty-memory ([#6060](https://github.com/bobmatnyc/trusty-tools/pull/6060), [#5078](https://github.com/bobmatnyc/trusty-tools/issues/5078))
- `ui/package-lock.json` is untracked and ignored. Nothing read it — CI and every `build.rs` install run pnpm against `pnpm-lock.yaml` — and its only writer was the npm fallback above ([#5936](https://github.com/bobmatnyc/trusty-tools/issues/5936))
- `trusty-analyze service uninstall` reports a unit it could not clear and exits
  non-zero. It used to fold "no plist" and "removal failed" into one `false`, so
  a surviving file rendered as evicted or absent and the command exited 0 while
  launchd still reloaded the unit at next login. It now delegates the eviction
  to `LaunchdConfig::evict_legacy_detailed` — the workspace's one
  implementation, which also verifies launchd actually let go rather than
  trusting `bootout`'s exit code — and reports its `EvictionOutcome` per label
  (#6350).
- `--help` no longer advertises `service install`, `service status` and
  `service logs`; all three were removed from the CLI and each exited 2. The
  retired-`--port` warning points at `service uninstall` rather than the
  `service install` that no longer exists (#6350).
- **A server that ended could make its own successor fail to start.** `serve_with_idle` unlinked the socket while the router — and through it every `AnalyzerAppState` clone, so both redb handles — was still alive, so the `facts.redb` and `scip_overlays.redb` locks outlived the path a client keys off. A client that saw the unlink spawned a successor, whose `FactStore::open` hit `Database already open. Cannot acquire lock.`; the successor died before binding, `Supervisor::ensure_running` never noticed, and the caller waited out the full 20s spawn probe for a `SpawnTimeout`. The router is now released before the unlink, so the locks are free by the time anything can observe the server as gone (#6595)
  - measured at 54-560 ms of exposure on an idle machine, with `lsof` naming the exiting server as the only holder in 15 rounds out of 15; 0 out of 15 after the change
  - the signalled exit had the same window and no `IdleGuard` to close it: `serve_until_idle` returns `Shutdown` the instant the signal resolves, leaving a connection task holding a router clone. That case now waits out `SHUTDOWN_FLUSH_TIMEOUT` for the task to finish before unlinking, and warns rather than proceeding silently when a handler with no read budget outlasts it
- `analyze.health` no longer restarts the idle window. Every caller of it is a
  monitor — the console connector, the console's `console_metrics` MCP poll and
  `tctl`'s probe — each dialling every 15 s against a 600 s window, which kept
  one `trusty-analyze serve` process resident for 46 hours. It is registered as
  a liveness method now, so answering it costs the daemon nothing (#6621).
- `ensure_daemon_running` no longer races a launchd-supervised
  `com.trusty.analyze` unit onto its own socket. The PID-file check that
  coordinates this daemon's own bridges cannot see launchd, so during a
  bootout/bootstrap window left by a pre-#6350 install the socket read as
  "nothing is running" and a bridge would spawn a second, unsupervised
  process. The guard now asks `trusty_common::launchd_claim` first and waits
  for the unit instead of spawning — a no-op on the ordinary host, where
  ADR-0032 means no plist is installed at all (#6624).

### Changed

- `core::redb_open::is_format_obsolete` now delegates to
  `trusty_common::redb_open::is_incompatible_format` instead of carrying its own
  copy of the four-arm `match` (#5063). Same verdict for every input; the
  quarantine policy `open_or_quarantine` is unchanged and stays here, because it
  takes a caller-supplied suffix and recovery string that trusty-common's
  fixed-suffix helper does not offer.
- **MCP protocol primitives now come from the `trusty-mcp` crate instead of `trusty_common::mcp`** — imports move from `trusty_common::mcp::…` to `trusty_mcp::…`, and the `trusty-common/mcp` feature is replaced by a direct `trusty-mcp` dependency. No behaviour change: the types and functions are byte-identical, only their home crate moved (ADR-0040, [#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803))
- `serve` names the interface it binds as `LOOPBACK_BIND` instead of an unnamed
  `[127, 0, 0, 1]` literal (#6038). Behaviour is identical — the daemon answers
  on the IPv4 loopback and only there (ADR-0018) — but a client whose default
  URL said `localhost` looked correct while resolving `::1` first on macOS, and
  nothing in this file stated the address a client has to match.
- The MCP stdio server was an HTTP client of its own daemon; it is an RPC
  client of its own socket now. Every tool-handler call site is unchanged.
- trusty-analyze runs on demand instead of as a resident daemon. `serve` now
  exits after ten minutes with no traffic (`TRUSTY_ANALYZE_IDLE_TIMEOUT_SECS`
  overrides it; `0` disables the exit), unlinking its socket on the way out, and
  `trusty-analyze deep` starts the server itself rather than failing when
  nothing is listening (#6350).
  - `serve --mcp` is the exception and serves until it is signalled. The stdio
    loop that process runs dials the socket once per tool call and never
    respawns it, so an idle exit would strand a live MCP session with a
    transport error for the rest of its life (#6355).
- `trusty-analyze service install`, `service status` and `service logs` are
  removed — no LaunchAgent is installed any more. `service uninstall` remains as
  the migration: it unloads `com.trusty.analyze` and its legacy alias and deletes
  their plists. `setup daemon` runs the same eviction before doing anything else,
  so an upgrade moves off the resident unit without an explicit command (#6350).
- `service::rpc::release_stores` is a plain drop again. The `Arc::strong_count`
  poll it grew in #6595 waited for connection tasks to release the router before
  the socket unlink; `serve_until_idle` now performs that drain itself on the
  shutdown path (#6601), so keeping the caller-side loop would be two
  implementations of one guarantee.
- `serve_options` inherits `RpcServeOptions::shutdown_drain` from the shared
  default (`shutdown::plannable_grace()`). An override to this crate's 3 s
  `SHUTDOWN_FLUSH_TIMEOUT` was reverted before release: it rested on the
  supervisor SIGKILLing this server at `ANALYZE_SIGTERM_PATIENCE`, which no
  supervisor path does. A bound analyze child is detached, so `ensure_running`
  never enters it in the supervised population and no reap path reaches it;
  `trusty-analyze stop` sends SIGTERM, polls 5 s and only reports. The 3 s drain
  averted no SIGKILL and abandoned the #6595 guarantee — every redb handle
  released before the socket unlink — three seconds into a multi-minute
  `analyze.review`.
- `SHUTDOWN_FLUSH_TIMEOUT` rises from 1 s to 3 s and is now an alias of
  `trusty_common::uds::ANALYZE_SHUTDOWN_FLUSH` rather than a second literal
  asserted equal to it. It bounds the supervisor's spawn-failure kill — the one
  path that signals an analyze child — leaving 2 s of the 5 s patience for the
  socket unlink, the redb store drop and exit.

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).

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
