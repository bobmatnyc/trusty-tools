# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.51.0] — 2026-09-01

### Added

- Per-index embedding pause/resume over the socket: `search.index.pause_embedding` and `search.index.resume_embedding` take `{"index_id"}` and answer `{"index_id", "embedding_paused"}`. Both are idempotent and refuse an unknown index with `404`. A paused index stops embedding at its next batch boundary and keeps its pending work; BM25, KG and the file watcher are unaffected. The state is in-memory and does not survive a daemon restart (#6524).
- `search.index.status`'s `stages.semantic` carries `paused: bool` beside the existing `status`, so a consumer can tell a parked stage from a running one without a new status variant (#6524).
- Per-index file-change feed: `search.index.file_events` replays the last 200 watcher-observed changes — `{path, kind: "modified"|"removed"|"rescan", at_unix_ms}`, path relative to the index root — then streams live ones. In-memory only (#6524).

### Removed

- `CodeIndexer::embed_deferred_chunks` — folded into `embed_deferred_chunks_gated(progress_tx, pause)`, which does the same work and takes an optional pause gate. Pass `pause: None` for the previous behaviour. Keeping both left a second, unguarded entry point to a durable write, which `scripts/check_teardown_guard.sh` flags (#6524, #3049).

## [0.50.0] — 2026-08-31

### Breaking

- `SearchAppState` gained a new public field, `allowlist_paths`, to carry the
  allowlist-gate configuration wired in this release. The struct is not
  `#[non_exhaustive]`, so any external struct-literal construction of
  `SearchAppState` no longer compiles — construct it via `SearchAppState::new`
  and `with_allowlist_paths` instead (#767).
- `PersistedIndex` is now `#[non_exhaustive]`, so future field additions stay non-breaking by construction. Construct it with `PersistedIndex::new(id, root_path)` (or `Default::default()`) and assign the fields you need — every field remains `pub`, so only the struct-literal syntax is withdrawn, not write access (#4390, #4391).
- `save_then_merge_contrib` returns `(Option<Arc<SymbolGraph>>,
  ContribMergeOutcome)` and `CodeIndexer::rebuild_symbol_graph_now` returns
  `ContribMergeOutcome`. Both were previously infallible-looking; callers that
  ignore the outcome are unaffected in behaviour, but this is a breaking change
  to the library API.
- `/health`'s `warmboot_summary.indexes_corpus_failed` now counts only indexes
  whose durable corpus failed to open. It was computed from
  `IndexStages::any_failed()`, so an index whose SEMANTIC lane died — corpus
  perfectly healthy — incremented a counter named for corpus-open failures.
  Two investigations in one day read the name literally, checked every index's
  `corpus_open_failure` (correctly `null`), found nothing, and dismissed a live
  count of `1` as a stale boot snapshot; the real cause was one index with
  `stages.semantic = "failed"`. The count is now read from
  `CodeIndexer::corpus_open_failed`, the same flag `GET /indexes/:id/status`
  reports, so the two surfaces can no longer disagree (issue #5927).
- A new `warmboot_summary.indexes_stage_failed` carries the any-lane count
  under a name that states what it measures. It is a strict superset of
  `indexes_corpus_failed` — a corpus-open failure fails every lane — and it is
  what forces `warm_boot_degraded` and the top-level `status: "degraded"`, so
  no daemon that used to report degraded stops doing so. A consumer that reads
  `indexes_corpus_failed` as "any lane failed" should move to the new key;
  monitors polling `warm_boot_degraded` or `status` need no change.

### Added

- **New regression tests guard the #4045 re-regressed fixes.** `tests_4045` (`commands::start`) asserts single-registration-per-corpus against the registry warm boot populates, driven through the colocated discovery scan that #3929 introduced — the path the #2305 / #2336 call-site guards do not sit on. No source behavior change; the crate's version was bumped only to keep main's version-parity gate green (`check-pr-version-bump.sh`).
- **`trusty-search serve`'s `TRUSTY_INDEX` precedence is now pinned by tests, so one argless `["serve"]` MCP declaration can be shared across projects** ([#4181](https://github.com/bobmatnyc/trusty-tools/issues/4181)). No behaviour change: `serve` already read `TRUSTY_INDEX`, with an explicit `--index` outranking it and the `--project` fallback of [#1373](https://github.com/bobmatnyc/trusty-tools/issues/1373) intact when neither is set. Verified end to end against the binary on all four combinations before anything was written
  - the source read as though it did not work, which is what earned the tests: `Serve` declares `#[arg(long)] index` with no `env`, looking like a second argument that shadows the `global = true, env = "TRUSTY_INDEX"` field on `Cli`. Both spell the same clap id, so clap propagates the one argument, environment value included
  - dropping `global` from `Cli::index`, renaming either field, or giving `Serve` an id of its own would break the argless declaration silently; `commands::serve::index_env_tests` now fails instead — confirmed by making that exact edit and watching `env_alone_pins_the_index` report `left: None`
  - the environment rows run in a child process, because clap reads `env` from the live process environment and mutating it in-process would race the rest of the binary's tests
  - `main`'s `Serve` arm now calls `commands::serve::resolve_pinned_index`, so the precedence is one function the tests can call rather than a match arm they would have to re-implement
- **A reindex now refuses a tree too large to index completely, instead of letting `TRUSTY_MAX_CHUNKS` truncate it into a corpus that reports success.** `create_index` accepted any `root_path`, and the only ceiling downstream dropped chunks past the cap and still emitted `complete` — a silently-partial index returns empty results a caller cannot tell from a legitimate miss. `service::index_budget::IndexBudget` checks the POST-FILTER walk (after `.gitignore`, `SKIP_DIRS`, `extra_skip_dirs`, `exclude_globs`, `extensions`, `path_filter`) against a file-count and total-byte ceiling before any staging corpus is opened, so a refusal leaves the existing index byte-identical. The refusal marks the reindex `Failed`, emits a fatal SSE frame, and sets `last_walk_error`, which `GET /indexes/:id/status` already reports and `/health` already reads as degraded (closes [#4356](https://github.com/bobmatnyc/trusty-tools/issues/4356))
  - defaults are `TRUSTY_MAX_INDEX_FILES=50000` and `TRUSTY_MAX_INDEX_BYTES=2147483648` (2 GiB); either set to `0` disables that dimension
  - **behaviour change:** an index whose walk exceeds either ceiling now fails where it previously completed (possibly truncated). The intended remedy is to narrow the index — `exclude_globs`, `extra_skip_dirs`, `include_paths`, `extensions` via `POST /indexes` or `PATCH /indexes/:id/config` — and the refusal message names both that and the env override
  - the ceiling is machine-wide daemon policy. The issue's literal ask — per-`create_index` `max_file_count` / `max_total_size` — is NOT delivered, so a caller who legitimately wants one oversized index must narrow it or raise the ceiling for every index on the box
  - the refusal fails every lane that was going to run (`mark_reindex_failed_before_lexical`) rather than reusing `mark_reindex_failed`, which leaves `lexical` alone under an invariant that only holds once the BM25 lane was genuinely built. Reusing it here stranded `lexical` at `InProgress` — an index stuck mid-walk with no reindex in flight
  - the refusal emits exactly ONE terminal SSE frame. It disarms `ReindexTerminationGuard` after pushing its own, so the guard's `Drop` no longer broadcasts a second `fatal` frame reading "exited unexpectedly (panic or cancellation)" — the CLI prints every error frame, so an operator who narrowed their index correctly was being sent to hunt a panic backtrace that did not exist
  - the tree walk and the budget check both run on the tokio blocking pool rather than a runtime worker. Both call `std::fs::metadata` over the same paths — `walker::should_skip_path`'s size guard stats every candidate, then `IndexBudget::check` sums the survivors — and on a network mount a stalled `stat()` blocks for as long as the kernel takes, so one `spawn_blocking` hop covers both and a stall costs a pool thread instead of freezing a worker. No wall-clock deadline: unlike `warm_boot::probe`, which needs only a yes/no about a volume and can abandon a frozen thread, this path needs the file list itself
  - test isolation: the two tests that set `TRUSTY_MAX_INDEX_FILES` live in their own test binary, `tests/index_budget_env.rs`. Every non-serial test in `service::reindex::tests` drives a reindex that reads that variable through `IndexBudget::from_env()`, so a process-wide override could refuse their walks as over-budget, and `#[serial]` does not order them against the non-serial majority ([#3769](https://github.com/bobmatnyc/trusty-tools/issues/3769)). The third env-touching test no longer writes env at all — `env_limit`'s parse branch is now a pure `parse_limit` it can call directly
- **The `create_index` MCP tool forwards `exclude_globs`.** `POST /indexes` has accepted the field since the repo-config work, but the tool only ever sent `id`, `root_path`, and `follow_links`, so an MCP caller could not narrow a large tree at registration time
  - one non-string entry rejects the whole array. `["**/a/**", 1]` used to forward just the string entries, narrowing the index by less than the caller wrote; `[1, 2]` was already rejected whole, so the two spellings now agree
- `trusty-search setup` now registers the MCP server with the Codex CLI as well as Claude Code, writing `[mcp_servers.trusty-search]` with `command = "trusty-search"` and `args = ["serve"]` into `~/.codex/config.toml`. Re-running it repairs a registration whose `args` is empty, a joined string, or a nested JSON string such as `["[\"serve\"]"]` — all three exec a process that prints help and exits before MCP initialization while Codex still lists the connection as enabled. Other tables and comments in the config are preserved (closes [#5264](https://github.com/bobmatnyc/trusty-tools/issues/5264))
- `search_health` now returns a structured diagnostic instead of forwarding `GET /health` verbatim. It names the daemon that answered — base URL, version, index and chunk counts — so a healthy daemon that is not yours is visible, and separates "nothing is listening" from "answered badly" from "healthy, but this project has no index" from "no index could be named, so nothing about this project was checked". Every non-ok verdict carries a `remediation` naming the command that fixes it, and callers branch on `healthy` rather than on the call succeeding. Optional `index_id` argument; otherwise the session pin, then the working directory, decides which index is checked (#5264).
- `trusty-search doctor` checks the MCP-client registrations `trusty-search setup` writes — Codex's `~/.codex/config.toml` and Claude Code's two global settings files — reporting each one's executable and version, effective arguments, selected project/index, daemon ownership, and remediation. A registration missing the `serve` argument, which launches the bare binary and never enters MCP mode, is now an error rather than something nothing on the machine reported. When neither global Claude settings file exists, doctor still emits a warning naming the project-local files it does not scan, so that case reads as a stated scope limit rather than silence (#5264).
- **A bare `trusty-search serve` now scopes its MCP session to the working directory, and says which index it chose and why** ([#5264](https://github.com/bobmatnyc/trusty-tools/issues/5264)). `setup` writes `args = ["serve"]` with no `--index`/`--project`, and ADR-0042 left the per-project index to `TRUSTY_INDEX` — which `tm launch` injects into each `claude` process. Codex is not launched by tm, so nothing injected it and every session `setup` created was unpinned: each `search`, `grep`, `chat`, or `index_status` call had to carry an explicit `index_id` or fail. The index is now resolved from the process's working directory instead, so one argless registration still works across every project and nothing is baked into `~/.codex/config.toml`
  - the pin reaches every project-scoped tool, not just one: they all resolve through the session pin already, so the working-directory tier is applied once at startup rather than added to each tool
  - **a derived index is confirmed against the daemon before it is pinned.** `derive_index_id` is a bare path basename, so two unrelated checkouts both named `api` derive the same id; pinning on the id alone would have served one project's code as the other's and looked healthy doing it. The daemon's `root_path` is compared against the root the id came from, and a mismatch refuses the pin and names the conflicting root
  - a working directory that is unindexed, that collides, or that cannot be confirmed because the daemon is unreachable leaves the session unpinned and prints why — the behaviour a bare `serve` already had, so a refusal costs nothing that previously worked, and explicit `index_id` calls keep working
  - an explicit `--index` (or `TRUSTY_INDEX`) or `--project` still wins outright and skips the working-directory probe entirely; the startup line now names the source, e.g. `pinned to index acme (from working directory)` vs `(from --index / TRUSTY_INDEX)`
  - resolved once at startup, not per call: `serve` is a long-lived stdio process whose working directory cannot change after exec
  - the tier is stdio-only. An HTTP listener serves many clients, so scoping it to whichever directory launched it would apply one client's project to all of them
- `GET /health` reports `indexes_stuck_mid_walk` and `GET /indexes/:id/status` reports `stuck_mid_walk`: an index whose lexical walk started and was then abandoned (the reindex task panicked or was cancelled, leaving the stage frozen at `in_progress`) is now distinguishable from one that is genuinely mid-reindex, and forces `status: "degraded"`. Detection only — clear it with `POST /indexes/:id/reindex` ([#5336](https://github.com/bobmatnyc/trusty-tools/issues/5336))
- **`/health`'s `deferred_embed_queue_depth` field now has real end-to-end test coverage.** #5546 caught that the field's doc comment cited a test, `health_reports_the_deferred_embed_queue_depth`, that was never written — the only existing coverage (`tests_stall.rs`'s `HealthResponse` fixtures) hardcoded the field to `0` in a struct literal and asserted nothing about it. `health_reports_the_deferred_embed_queue_depth_end_to_end` (`service::server::tests_stall`) drives a real queued job through `spawn_deferred_embed_pass`, holding the background-reindex semaphore itself so the increment is observed deterministically, then releases it and polls `/health` until the depth drains back down — proving the field tracks the live queue in both directions. `health.rs`'s doc comment now cites this test by name.
- `WatchEvent::Rescan`, a new variant on the public `WatchEvent` enum, carrying
  the dropped-event signal to the watch loop. Breaking for exhaustive downstream
  matches, so this needs the 0.x minor position rather than a patch bump.
- `IndexedFiles::paths`, used by the rescan reconcile to find files deleted
  while the event queue was overflowing.
- The UDS RPC surface now serves `search.chat` (`POST /chat`) and
  `search.admin.stop` (`POST /admin/stop`), the last two routes with a named
  consumer — trusty-mpm's TUI stop key and the search UI's chat panel, which
  #6155 moves into `trusty-console`. Both run the same transport-free core their
  axum handler wraps, so the two doors answer identically (#6285).
- **The daemon binds a hardened Unix socket alongside its HTTP listener** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285), [ADR-0032](../../docs/adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)). `trusty-search start` now binds `<data dir>/trusty-search.sock` through `trusty_common::uds::bind_singleton_hardened` and serves a framed JSON-RPC router on it, while `127.0.0.1:7878` keeps serving every route it did before. The path is derived, not published — the same `trusty_common::daemon_socket_path` entry point trusty-memory, trusty-review, trusty-analyze and trusty-mpm resolve — so there is no discovery file for a stale write to contradict
  - `search.health` is the first method served, and it answers the body `GET /health` answers: both call `service::server::health_report`, so the two transports cannot report a different index count, status, or version. `health_over_the_socket_matches_the_http_body` pins that
  - a method no slice has claimed answers `method_not_found` naming what is served, then closes cleanly — the contract every later slice inherits for the names it has not moved yet
  - a socket that cannot be bound FAILS startup with an error naming the path, before the port file or the shared discovery registry announce the daemon. There is no degrade to HTTP-only
  - one stop condition — SIGTERM, SIGINT, or the in-process `POST /admin/stop` — is awaited once and fanned out to both listeners through a cancellation token. Awaiting it separately in each works on SIGTERM and fails on Ctrl-C, where `tokio::signal` delivers to whichever waiter is registered. The socket file is unlinked before its listener is dropped
  - `service::server::build_router_on` takes the `Arc<SearchAppState>` the caller already holds, so the socket and the router are two doors onto ONE daemon: one registry, one set of tickers. `build_router` and `build_router_with_self_origins` are unchanged for every existing caller

Nothing was removed. The HTTP surface, its routes, the embedded SPA, and every consumer that dials `127.0.0.1:7878` are untouched by this change — the route families move onto the socket one slice at a time, and the retire slice deletes the axum surface and migrates the consumers.
- **The query surface is served over the Unix socket** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285), [ADR-0032](../../docs/adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)). Slice 3 of the migration: six methods join the read surface on `<data dir>/trusty-search.sock`, and every HTTP route they mirror still answers exactly as it did
  - `search.query` (`POST /indexes/{id}/search`), `search.query.all` (`POST /search`), `search.grep` (`POST /indexes/{id}/grep`), `search.grep.all` (`POST /grep`), `search.similar` (`POST /indexes/{id}/search_similar`), and `search.typeahead` (`GET /indexes/{id}/typeahead`)
  - each method and its HTTP route call ONE function — `search_report`, `global_search_report`, `grep_report`, `global_grep_report`, `search_similar_report`, `typeahead_report`, which the axum handlers now wrap. A per-family parity test drives the real axum router and the real RPC router against one shared state over an index holding a real corpus, asserts a non-empty answer, then compares the bodies excluding only `latency_ms`, which both sides sample from the host clock
  - the five routes that take a request BODY carry it nested under `body` beside `index_id`, rather than flattened. `SearchQuery` and `GlobalSearchRequest` reject unknown fields because a misspelled filter returns too much data ([#3401](https://github.com/bobmatnyc/trusty-tools/issues/3401)), and an unmodified decode of the same JSON document is what keeps that refusal identical on both transports. `search.typeahead`, whose HTTP form is a query string, keeps slice 2's flattened shape
  - both guards that HTTP puts in front of these six routes cross with them: the admission limiter ([#2845](https://github.com/bobmatnyc/trusty-tools/issues/2845)) and the interactive query deadline ([#907](https://github.com/bobmatnyc/trusty-tools/issues/907)) now live on the shared daemon state, so the socket gates on the SAME semaphore and the SAME deadline instead of a second copy of each. Without this the daemon would have served twice the configured concurrency while both doors reported obeying it
  - an expired deadline answers a new code, `-32005` — the number `trusty-analyze` already uses for the same meaning. A busy daemon clears on its own, but a query that outran the deadline does it again unless the caller narrows it, so the two are not one class
- **The read surface is served over the Unix socket** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285), [ADR-0032](../../docs/adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)). Slice 2 of the migration: nine methods join `search.health` on `<data dir>/trusty-search.sock`, and every HTTP route they mirror still answers exactly as it did
  - `search.indexes.list` (`GET /indexes`), `search.index.status` (`GET /indexes/{id}/status`), `search.index.config.get` (`GET /indexes/{id}/config`), `search.config.get` (`GET /config`), `search.chunks.list` (`GET /indexes/{id}/chunks`), `search.graph.get` / `search.graph.stats` / `search.graph.neighbors` (`GET /indexes/{id}/graph[/stats|/neighbors]`), and `search.call_chain` (`GET /indexes/{id}/call_chain`)
  - each method and its HTTP route call ONE function — `list_indexes_report`, `index_status_report` and their siblings, which the axum handlers now wrap. A per-family parity test drives the real axum router and the real RPC router against one shared state and compares the bodies, excluding only the fields sampled from the host clock at answer time (`graph`'s `generated_at`, the call-chain report's `# Generated:` line)
  - a route whose HTTP form splits its arguments across the path and the query string carries them as one `params` object: the `{id}` becomes an `index_id` field beside the query fields, under the same names. The one difference a caller sees is typing — a query string delivers `limit=100` as text and JSON-RPC delivers it as a number
  - a refusal keeps the classification HTTP gives it. 400 answers `invalid_params`, 404 answers `-32004`, and 503 splits by whether the condition can clear: `-32002` when it can, `-32012` when it cannot. `RpcError` carries no body, so the `retryable` field the index-scoped error contract names becomes the code — a cold-parked index and a permanently failed restore no longer read alike. `restore_via` does not cross, deliberately: it names an HTTP route that does not exist on this transport, and the retire slice replaces it with a method name
- **The two SSE surfaces are served over the Unix socket** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285), [ADR-0032](../../docs/adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)). Slice 5 of the migration: `search.status.stream` (`GET /status/stream`) and `search.index.reindex.stream` (`GET /indexes/{id}/reindex/stream`) join the twenty-three one-shot methods on `<data dir>/trusty-search.sock`, and both HTTP routes still answer exactly as they did
  - the framing is `trusty_common::uds::server`'s multi-frame extension ([#6286](https://github.com/bobmatnyc/trusty-tools/issues/6286), trusty-common 0.44.2) — `RpcRouter::typed_stream` and `write_stream`, the same mechanism trusty-memory's chat tokens ride. Nothing here re-implements SSE's `data: …\n\n` encoding; one stream item is the JSON document one `data:` line carries, parsed rather than prefixed
  - a stream method lives in the router's STREAMING table, never the unary one. A request without `"stream": true` against either name is refused with `CODE_STREAM_REQUIRED` rather than answered with one frame, so a consumer cannot silently read a truncated sequence as a complete one
  - `reindex_progress_events_match_the_sse_body_frame_for_frame` drives the real axum router and the real RPC router against one planted progress record and compares the sequences whole. The live half is pinned separately: events emitted after the subscription arrive in emission order, and a finished reindex ends its stream after the replay rather than idling on a broadcast nothing will send to again
  - `an_unknown_index_is_refused_before_the_reindex_stream_opens` pins the SSE route's `404` as a terminal error frame carrying `-32004`. A stream that opened and ended empty would read as "the reindex produced nothing"
  - two SSE-only frames have no socket counterpart, both deliberately: the 20 s `: heartbeat` comment, which exists so an idle TCP body is not torn down by the OS or a proxy (a Unix socket has neither, and no read budget applies between frames), and the `data:` framing itself
  - neither method takes an admission permit or a query deadline, matching the `free` group both HTTP routes are in. A stream runs as long as its reindex does, so a permit held for its lifetime would let two dashboards exhaust the semaphore `/health` and every query share
  - `a_client_that_drops_mid_stream_leaves_no_producer_subscribed` drives a REAL socket: a client that hangs up mid-stream releases the producer task and its broadcast subscription, and no admission permit is held at any point. The disconnect clears at the server's next write — the stream client half-closes its write side after the request frame, so read-EOF is not a hangup signal — and each producer additionally selects on `Sender::closed()` so it ends with that write rather than one event later
- The Unix-socket RPC surface serves the last four routes with a named consumer:
  `search.index.config.set`, `search.config.set`, `search.logs.tail` and
  `search.registry.orphans`. Each runs the same core its HTTP route runs, so a
  caller moving onto the socket gets the same body and the same refusals (#6285).
- **The write surface is served over the Unix socket** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285), [ADR-0032](../../docs/adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)). Slice 4 of the migration: seven methods join the read and query surfaces on `<data dir>/trusty-search.sock`, and every HTTP route they mirror still answers exactly as it did
  - `search.index.create` (`POST /indexes`), `search.index.delete` (`DELETE /indexes/{id}`), `search.index.relocate` (`PATCH /indexes/{id}`), `search.index.file.put` (`POST /indexes/{id}/index-file`), `search.index.file.remove` (`POST /indexes/{id}/remove-file`), `search.index.reindex` (`POST /indexes/{id}/reindex`), and `search.graph.ingest` (`POST /indexes/{id}/graph`)
  - each method and its HTTP route call ONE function — `create_index_report`, `delete_index_report`, `relocate_index_report`, `index_file_report`, `remove_file_report`, `reindex_report`, `ingest_graph_report`, which the axum handlers now wrap. Every refusal is that one function's, so a socket write cannot land where the HTTP route would have refused it, nor report a landing that did not happen
  - the reindex TRIGGER is here; `GET /indexes/{id}/reindex/stream` and the other SSE routes stay on HTTP until slice 5
  - the two concurrency lanes are copied from the axum router rather than smoothed out. The four per-index writes are admission-limited on the same shared semaphore the query surface uses and are deliberately NOT deadline-bounded, because a reindex or a large ingest legitimately runs for minutes. The three registry-level routes have no limiter at all, so a delete does not queue behind a running reindex on one transport and sail past it on the other
  - every mutating method carries a failure-arm test that asserts the refusal on BOTH transports and then re-reads what must not have moved: the registry entry, the `indexes.toml` row, the handle's root path, the merged graph. A delete of a registration the [#767](https://github.com/bobmatnyc/trusty-tools/issues/767) allowlist gate excluded still removes the row over the socket, matching [#6363](https://github.com/bobmatnyc/trusty-tools/issues/6363) / [PR #6365](https://github.com/bobmatnyc/trusty-tools/pull/6365); a contribution that persisted but could not be merged is refused over the socket too, matching [#5505](https://github.com/bobmatnyc/trusty-tools/issues/5505), and a traversal confirms it really is unqueryable
  - a contributed graph over 8 MiB is refused on the socket where HTTP accepts up to 64 MiB, because the frame budget bounds a request as well as a response. The refusal is loud — the listener stops at the budget and nothing partial is stored — and a large ingest uses the still-mounted HTTP route until the retire slice raises the budget on the listener and the client together
- `GET /registry/orphans` lists registrations in `indexes.toml` whose root is
  gone, separately from the ones the daemon declined to judge. It reads the
  registry file rather than the in-memory registry, so it can see a registration
  the warm-boot allowlist excluded — which `GET /indexes` cannot (#6371, #6363).
  It removes nothing; `DELETE /indexes/{id}` stays the one deregistration path.
- `DELETE /indexes/{id}` and `search.index.delete` accept an optional `expected_root_path`. When present the delete is refused with `409` unless the registration's current root path is exactly that value, so a caller acting on a stale census cannot remove an index that was re-registered under the same id in the meantime. The comparison runs twice: once before any teardown, and again under the exclusive teardown lock the delete waits on — a relocate rewrites `root_path` while holding only that lock's shared side, so one landing inside the wait turns the delete into a refusal rather than a success against a moved, live root. A registration that is absent, and a registry file that cannot be read, are refusals too — a comparison that could not run never permits the delete. Absent, the delete behaves exactly as before (#6380).
- `GET /indexes?details=true` and the `console_metrics` tool report `last_used_unix` per index — `max(last_queried_unix, last_indexed_unix)` off `indexes.toml`, absent for an index never searched or indexed. The trusty-console index roster renders it as a Last Used column and sorts by it (#6424).

### Fixed

- The opt-in index allowlist is now enforced. `check_path` shipped in PR #789 with zero production call sites, so a root that was never approved was registered exactly as before the control existed. `POST /indexes`, the reindex `root_path` override, and `PATCH /indexes/:id` now refuse an unapproved root with `403` and a `remedy` field naming the command that approves it (#767).
- Warm-boot drops registry entries whose root the allowlist no longer approves, so removing a root actually stops it being indexed instead of only blocking new registrations. On-disk data is left alone; re-approving restores it on the next boot (#767).
- A root is approved when `allowlist.toml` lists it, when the `tm` project registry lists it, when it is a worktree provisioned under one of those, or when it sits inside one. A sibling of an approved root is not approved. The hard sensitive-path denylist runs first and wins over every one of them (#767).
- On first boot a daemon with no `allowlist.toml` seeds one from the roots it is already serving, so switching the gate on does not silently un-index a working install. The pass records a durable stamp so it runs once even if the file is later deleted, never rewrites an existing file, creates it with `create_new` so a concurrent `index add` wins rather than being clobbered, and re-checks the denylist so a sensitive root is not carried over (#767).
- Warm-boot relocation is gated. The salvage phase adopted a candidate root from `roots.toml`, persisted it, and started watching it without consulting either the allowlist or the denylist — so a leftover colocated index in a personal directory could be adopted by any approved-but-missing root with no operator action. Candidates are now filtered before anything is persisted (#767).
- `trusty-search index add` gains `--allow-sensitive-path`, which approves a root under an OS-temp or app-support prefix. The credential, secret-marker, and top-level-home denylist rows are never relaxed. This is the only way to approve such a root — `POST /indexes` with `allow_sensitive_path: true` relaxes the ephemeral-prefix denylist rows and nothing else, and a request that sets it still needs the root approved (#767, #2914).
- An empty, relative, or `/` entry in `allowlist.toml` or the project registry no longer approves every path. Containment is decided with `starts_with`, so one malformed row previously turned default-deny into a global allow (#767).
- Indexing no longer writes an allowlist entry. It used to create one after every successful registration, promoting derived approvals — provisioned worktrees, registered projects, sub-roots — into permanent hand-file entries, so removing the parent stopped stopping the child and every ephemeral worktree left a row behind. `index add` is the verb that approves; indexing only updates settings on an entry that already exists (#767).
- **A reindex whose parse/embed producer panicked reported `complete`.** The `JoinError` was logged and written to the termination guard's failure slot, but that slot is only read by the guard's `Drop` — and the normal path disarms the guard. The #601 gate reads counters only, so the batches committed before the panic left `vector_count > 0` and it stayed silent; SSE subscribers, `/health` and the status endpoint all saw a successful reindex over a file list that was never finished. The captured reason now becomes the run's terminal verdict (`validate::override_with_producer_failure`), which rolls the partial staged corpus back instead of promoting it over the complete live one and marks the index failed (closes [#1451](https://github.com/bobmatnyc/trusty-tools/issues/1451))
  - the fatal SSE frame and the `reindex[…]: FAILED` log line now report the real `vector_count` instead of a hardcoded `0`, which was only ever true on the zero-vector path
- **The reindex guard's SSE error frame named the empty string when it fired before the index id was known.** Only the log message branched on emptiness; the JSON payload serialized `"index_id": ""` unconditionally, so a consumer routing on that field acted on a value matching no index. The field is now omitted entirely until `with_index_id` stamps it
- **Search dropped candidates after fusion without telling the caller, so a short result set was indistinguishable from a small one.** `top_k: 10` returning 3 rows read exactly the same whether 3 chunks matched or 7 were deleted on the way out — the mechanism behind [#2203](https://github.com/bobmatnyc/trusty-tools/issues/2203), where users concluded search was broken on an intact corpus. The drops themselves may be correct; being unable to tell them apart from a genuine result count was the defect
  - `POST /indexes/:id/search` now returns `meta.dropped` — a per-site tally (`unresolved_corpus`, `mode_filtered`, `docstring_filtered`, `archived`, `out_of_root`) — plus `meta.dropped_total`. The reason travels with the count because the remedy differs per site: `unresolved_corpus` is a fault, the rest are the requested `mode` / `exclude_archived` / root scope working as asked
  - `CodeIndexer::search_with_drops` is the new entry point carrying the tally; `CodeIndexer::search` is unchanged in signature and now delegates to it
  - a fused id with no corpus row is counted, `warn!`ed, and incremented into `trusty_search_dropped_unresolved_corpus_total`. It used to be a `trace!` reading "likely race", which asserted a cause it never checked and is exactly wrong in the case that matters: when `fetch_chunks_for_ids`' durable read fails it falls back to an in-memory map that is empty after idle eviction, so every id misses and a healthy index returns nothing
  - `meta.stale_index_root` keeps its existing boolean meaning; `meta.dropped.out_of_root` adds the count it never carried
- **Orphaned `.usearch.tmp` staging files were never cleaned up.** `save` writes a staging file and renames it into place, and every failure path deletes its own; a process SIGKILLed between the write and the rename runs no cleanup at all. Since [#4395](https://github.com/bobmatnyc/trusty-tools/issues/4395) made staging names process-scoped (`hnsw.usearch.<pid>.tmp`) no later process overwrites the leftover either, so one file per (index, killed pid) accumulated beside the live snapshot forever — nothing in `core/store/` called `read_dir` (closes [#2936](https://github.com/bobmatnyc/trusty-tools/issues/2936))
  - `UsearchStore::load_from` now reaps first: a staging file is deleted when its embedded pid names a dead process, and pre-#4395 bare `.tmp` names go unconditionally
  - a staging file whose pid is ALIVE is left alone. Colocated snapshots sit in the project root outside every data directory, so two daemons can legitimately stage beside the same snapshot; deleting one mid-write is the cross-process corruption #4395 removed. Such a file is consumed by its own process's next successful save, since `staging_path` is deterministic per process
  - the reap is best-effort throughout — an unreadable directory or a failed unlink is logged at `debug` and skipped, never propagated, so a snapshot that loads fine is not refused because a leftover could not be tidied
  - part (b): a real `AbortHandle` abort against a real in-progress `save()`, swept across a spread of delays, now pins the "an aborted flush never reaches its rename" guarantee that was previously asserted only by inspection. The sweep straddles the rename in practice — some delays leave the old snapshot, some the new — and any torn state fails
- **`index_file` ignored `skip_vector`, so a BM25-only index kept embedding on every file save.** The batch reindex path has routed `skip_vector` through `parse_files_only` since #2984 Phase 1, but `CodeIndexer::index_file` — the choke point for the file watcher, the boot-time reconciler, and `POST /indexes/:id/index-file` — called `embed_chunks_in_batches` unconditionally. An index whose `/health` reported the semantic stage `Skipped` was still paying the embedder per save and still growing an HNSW graph nothing would serve (closes [#3048](https://github.com/bobmatnyc/trusty-tools/issues/3048))
  - the flag now mirrors onto `CodeIndexer` itself, as `skip_kg` already did. Reading `IndexHandle::skip_vector` at the call site would not work: the watch loop holds only an `Arc<RwLock<CodeIndexer>>` and never sees a handle, and `IndexHandle` is rebuilt on every `PATCH /indexes/:id/config`, so any snapshot a long-lived task took is permanently stale. The `indexer` `Arc` is preserved verbatim across those rebuilds
  - `apply_component_transition` syncs the indexer's copy on every vector turn-on and turn-off, so a `PATCH { "vector": false }` stops the watcher embedding immediately rather than at the next daemon restart
  - skip_kg parity on the same path: `index_file` ended with an unconditional `rebuild_symbol_graph`, which builds the petgraph AND persists it, so every watcher save paid the ~50-100 MB/index cost `skip_kg` exists to avoid. `finish::finish_reindex` had skipped it since #313. Gated at the `index_file` call site, not inside `rebuild_symbol_graph`, which is shared with reindex, `remove_file`, and contributed-graph ingest
  - both gates ship with a control test asserting the enabled case still embeds and still builds a graph, so the pair proves the flag decides rather than the code path
- **`DELETE /indexes/:id` neither awaited nor cancelled in-flight work, and reported `data_deleted` from the request rather than from the disk.** The delete removed the registration and `remove_dir_all`'d the data directory while a writer was still writing into it; recreating the same id immediately then let a new-epoch task interleave with the old one, because on-disk paths are keyed purely by sanitized id (closes [#3049](https://github.com/bobmatnyc/trusty-tools/issues/3049))
  - `unregister_index` signals a per-index cancel flag, then waits up to 30s for the EXCLUSIVE side of a new per-index teardown lock and holds it across teardown. Every path that mutates `handle.indexer` holds the SHARED side for the span of its write, so acquiring it exclusively means no writer of any kind is in flight
  - the guarded set is `index_file_handler`, `remove_file_handler`, `ingest_graph_handler`, `relocate_index_handler`, the filesystem watch loop, boot reconcile, `run_reindex`, the deferred-embed pass, and the component catch-up. The full table, including the paths deliberately left unguarded and why, is on `INDEX_TEARDOWN_LOCKS` in `service/reindex/semaphore.rs`
  - a read/write lock rather than the existing 1-permit `index_semaphore`: writers stay as concurrent with each other as before, so a single `index-file` call does not serialise behind a full reindex. `index_semaphore` keeps its unchanged mutual-exclusion role
  - `run_reindex` polls the cancel flag at its producer and consumer batch boundaries, so the wait costs one batch rather than one corpus
  - on timeout a `?delete_data=true` delete is ABANDONED and nothing at all is changed, so the retry its log advertises actually works. Refusing only the `remove_dir_all` while still removing the registration stranded the data directory permanently: the retry hit `removed=false` and never reached the removal branch again, and the orphan reaper covers registrations whose `root_path` vanished, not data directories with no registration. A `?delete_data=false` delete (the orphan reaper's mode) still deregisters — no data was going to be removed, so no orphan is possible. The response's `quiesced: false` alongside `removed: false` reports the abandonment
  - `POST /indexes` and `PATCH /indexes/:id/config` now take the teardown lock's shared side too. `create_index_handler`'s only guard was `state.registry.get(&id).is_some()`, and `unregister_index` removes that entry partway through teardown, so a create landing in the window registered a second generation over an index the delete was about to `remove_dir_all`. The PATCH path was listed as guarded and was not — it quiesced through `index_semaphore` before the primitive changed
  - writers acquire through `acquire_index_teardown_read`/`_write`, which re-validate the map entry with `Arc::ptr_eq` after acquiring. A writer parked on an entry that a teardown then evicted would otherwise hold a lock nothing else could reach, and the next delete would see no contention and report `quiesced: true` against a live writer. `unregister_index` also evicts while still holding the exclusive guard, so no destructive step can run after a racing caller has been handed a fresh lock
  - `data_deleted` is now assigned only when `remove_index_data_dir` returns `Ok`. It used to be computed as `removed && params.delete_data` — straight from the request — while a removal failure was downgraded to a `tracing::warn!`, so a delete whose data removal failed answered `data_deleted: true` and a caller recorded the corpus as reclaimed while every byte remained
  - three more writers were unguarded, and are now guarded: the startup schema migration (`spawn_index_migrations`, which loops `commit_parsed_batch` over 64-file batches at every boot), the filesystem watcher's stale-chunk removal in `handle_modified` (its guard existed but sat AFTER a `remove_chunk` loop that had already deleted from redb), and eviction on the timeout path
  - a `?delete_data=false` delete that TIMES OUT no longer evicts the index's teardown lock or its `INDEX_LOCKS` entry. That is the endpoint's DEFAULT mode, and evicting around a writer that outlasted the wait handed the next caller a fresh, uncontended lock: a later `?delete_data=true` then reported `quiesced: true` and destroyed the directory that writer was using. Eviction is now conditional on the quiesce wait having succeeded. The cancel flag is still evicted on both paths — the index is deregistered either way, so the in-flight writer must keep seeing `true` (it holds its own `Arc`), while a recreate under the same id must start uncancelled
  - which paths owe the guard is now enforced by `scripts/check_teardown_guard.sh`, not by a prose table. It fails when a durable-write call site neither acquires the guard earlier in its own scope nor is declared in `scripts/teardown-guard-manifest.tsv` with a reason, and refuses a scan that inspects fewer files, call sites, or guard acquisitions than its declared floors. The hand-derived table was wrong in each of the three preceding rounds
  - a scope, for that gate, is the innermost enclosing function or DEFERRED BODY — a spawn call, an `async` block, or a closure. The guard is a scope guard, dropped when the function that took it returns, so anything deferred past that return is unprotected by it. Matching only `spawn(` was not enough: `let fut = async move { idx.commit_parsed_batch(…).await }; tokio::spawn(fut);` shares neither a line nor a brace with the spawn, and the review defeated the gate's first version with exactly that. A call to the right of a deferred opener is now inside that body whether or not the brace balance ever revealed it. The disclosed cost is a manifest row for a write inside a body that is immediately awaited, which is safe but not textually visible as safe
  - not covered: `embed_deferred_chunks` has no interior cancel checkpoint, so a delete concurrent with an active deferred-embed pass waits for the whole pass rather than for a batch, and abandons itself if that outlasts 30s. Retrying it succeeds once the pass finishes
- **Publishing can no longer ship an admin dashboard built from source the crate no longer carries.** `cargo publish` packages the committed `ui-dist/` verbatim — `SKIP_UI_BUILD=1` short-circuits `build.rs` on every release path, and the mirror step that refreshes the bundle (`make -C crates/trusty-search sync-ui`) is human-remembered. Skip it and every existing gate still passes: clean tree, right tag, free version, unchanged public API. That shipped three times, most recently 0.37.0, whose dashboard had no dark mode because [#3509](https://github.com/bobmatnyc/trusty-tools/issues/3509)'s `tokens.css` / `theme-bootstrap.js` rewrite never reached `ui-dist/`. `scripts/preflight-publish.sh` now runs a seventh check, `scripts/check-ui-bundle-freshness.sh` (closes [#3606](https://github.com/bobmatnyc/trusty-tools/issues/3606))
  - each bundle carries `ui-source-hash.txt`, a digest of the UI source it was built from, written by `scripts/stamp-ui-bundle.sh` during the build; the gate recomputes that digest and fails on a mismatch. It rebuilds nothing, because `ci.yml`'s `ui-checks` job had already rejected rebuild-then-diff — Vite's content-hashed filenames are not byte-stable across toolchain patch versions
  - the first version compared commit ancestry and a review laundered it in three commits: a real source change was caught, then one unrelated line appended to the bundle's `index.html` flipped the same stale bundle to pass, because any commit touching the bundle directory read as "rebuilt after the source". An edit to the bundle does not move the source digest
  - a byte-identical rebuild still moves the digest, since the digest is over the source rather than the output — so `make release-prep` always leaves a changed `ui-source-hash.txt` to commit. That is why there is no override flag
  - a green run states the counts it inspected — source files, bundle files, blob hashes, resolved asset references — and zero of any of them is a failure. A missing or digest-less stamp, an absent or empty `scripts/ui-bundle-manifest.tsv`, a manifest row whose bundle tracks nothing, a source tree with no build inputs, and a crate shipping a bundle without a manifest row all fail rather than pass quietly
  - `ASSET-MISSING` covers what a digest match cannot see: a mirror that copied `index.html` and stopped, leaving it pointing at hashed assets that are not in the tree
  - `.github/workflows/ui-bundle-freshness.yml` runs the same script at merge time, closing the day-long window the incident used — drift merged 2026-07-20, published 2026-07-21
- **Deleted `crates/trusty-search/.github/`, three workflow files GitHub has never once run.** `CLAUDE.md` cited a `ui-dist-check` job that fails on a stale bundle, and that job was real — a full rebuild-then-diff, present since the initial monorepo import (`13f9fa2c0`, 2026-05-19). GitHub Actions only discovers workflows under the repo-root `.github/workflows/`, so a nested copy is inert whatever it contains. The directory also held a `publish.yml` triggering on `v*` tags (this repo tags `trusty-search-v*`) that ran `cargo publish --no-verify --allow-dirty`. Deleted rather than hoisted: hoisting would reintroduce the byte-diff `ci.yml` had already rejected and duplicate the gate above. `CLAUDE.md` now says CI never checked this and names the gate that does
- Test isolation: the two tests that mutate `TRUSTY_HNSW_REVIEW_IDLE` moved out of the lib test binary into their own integration binary, so they can no longer make `hnsw_idle_demotion_reviews_clean_promoted_store` fail by holding the process-global gate at `0` while it runs (#3769).
- The `grep` MCP tool's description and its `index_id` parameter description no longer contradict each other. Both said an omitted `index_id` fans out across every registered index; the dispatcher actually resolves the session-pinned project index first and only sweeps every index when the session has no pin, so a pinned caller following the old text got zero matches with no explanation. Text only — `grep`'s behaviour is unchanged (closes [#3805](https://github.com/bobmatnyc/trusty-tools/issues/3805))
- **A genuinely corrupt durable corpus was recreated empty and served as healthy, so a live watcher rebuilt a partial corpus over it.** `open_corpus_db_or_recreate` treated `Storage(Corrupted(_))`, `RepairAborted` and `Storage(Io(InvalidData))` the same as a stale redb-2.x file: move it aside, return `Ok` with a fresh EMPTY corpus. Because that was an `Ok`, `build_indexer_from_entry` took its success arm and wired the empty store with `corpus_open_failed` left false — and the #4122 write quarantine, the #4087 query-surface `503`s, and `/health`'s degraded signal all key on that flag, so none of them could fire. The index came up reporting healthy with 0 chunks and an active watcher, and ordinary file saves then persisted a fresh partial corpus over the recreated one. Corruption now backs the file aside and returns a typed `CorpusCorrupted` error, which quarantines the index (closes [#4227](https://github.com/bobmatnyc/trusty-tools/issues/4227))
  - `UpgradeRequired` is deliberately excluded and still recreates silently: it names a known old on-disk format with a data-preserving recovery tool (`trusty-search migrate-redb`), so quarantining it would turn every redb major upgrade into an outage. The split lives in the new `is_genuine_corpus_corruption`, beside the existing `is_incompatible_corpus_format` that decides only whether the file is moved aside
  - the damaged bytes are still renamed to the `.v2-incompatible` sibling before the error returns, so failing closed never destroys the operator's recovery source; no replacement file is created, so the next boot opens a clean corpus and boot reconcile reindexes from source
  - `CorpusOpenFailure::classify` recognises the new marker as `FormatIncompatible`, so the status surfaces say "rebuild" rather than the `Unclassified` "diagnose first" wording — `FormatIncompatible` was previously unreachable from the real open path, since corruption never propagated as `Err`
  - the `LOAD-BEARING INVARIANT` in `core/indexer/quarantine.rs` (`corpus_open_failed == true` implies `corpus == None`) is preserved by construction: the corrupt path wires no store at all rather than wiring one alongside the flag
  - `incompatible_corpus_is_backed_up_and_recreated` claimed to simulate a redb-2.x file but wrote garbage bytes, which produce `Storage(Io(InvalidData))` — the corruption bucket — so the `UpgradeRequired` path it named was never exercised. Replaced with `old_format_is_backed_up_and_recreated_without_error`, which builds a real redb 2.x database through the `redb2` dependency the `migrate-redb` subcommand already links
- Boot reconcile can now see commits made while the daemon was down. `indexed_head_sha` is persisted in `indexes.toml` and restore reads it instead of re-deriving it from live git, which previously made the stored and current SHA equal by construction so every git-backed index reported up-to-date on every boot (#4391).
- An interrupted deferred-embed pass no longer leaves a silently under-embedded index. The pass is recorded as outstanding in `indexes.toml` before it is queued and cleared when it commits, and warm boot re-arms the catch-up for any index still carrying the marker (#4390).
- `/health` now reports `deferred_embed_queue_depth`, which its own doc comment already claimed was exposed (#4390).
- `service::SearchClient` no longer routes its loopback daemon calls through an
  exported `HTTP_PROXY`. It builds through
  `trusty_common::http_client::loopback_client_builder` (#4392).
- Test stabilization: the six filesystem-watcher tests no longer depend on a fixed startup sleep plus a single save landing inside a 2–3 s deadline. Each now re-applies its save once per debounce window and polls the condition until it holds, so a save lost to a macOS FSEvents queue overflow — which arrives as a `MustScanSubDirs` event naming a directory to rescan rather than the file that changed, and is never redelivered — is retried instead of stranding the test (#4731).
- `daemon.env` is now sourced before the command line is parsed on the `trusty-search start` path, so a variable in it that backs a CLI flag (`TRUSTY_NO_AUTO_DISCOVER`, `TRUSTY_INDEX`) actually takes effect. It used to be read after parsing, which made every such setting a silent no-op — the file advertised itself as a working configuration mechanism and was not one for that class of setting, and `#767`'s auto-discovery suppression never took effect on machines that configured it there. Precedence is unchanged: shell env still beats the file, and the file still beats compiled-in defaults. `TRUSTY_DATA_DIR`, `TRUSTY_DEVICE`, and `TRUSTY_SEARCH_FANOUT_CONCURRENCY` deliberately stay on the post-parse pass so `--data-dir` / `--device` / `--fanout-concurrency` keep winning, and so a `daemon.env` cannot redirect the data dir that decides where `daemon.env` itself lives. Only `start` sources the file; client subcommands do not (closes [#4827](https://github.com/bobmatnyc/trusty-tools/issues/4827))
- `daemon.env` read failures and malformed lines are reported at `warn` instead of being swallowed. An unreadable file used to be indistinguishable from an absent one, and a line missing its `=` was dropped without a word while startup continued on compiled-in defaults
- `trusty-search service install` now writes `RUST_LOG=info` into the generated launchd unit. launchd exec's the daemon with no shell environment, so `RUST_LOG` arrived unset, tracing filtered at `warn`, and every `tracing::info!` the daemon writes about its own boot — including the lines that confirm auto-discovery suppression — was dropped. A `RUST_LOG` the installed unit already carried, or one exported in the installing shell, still wins over the new default (closes [#4829](https://github.com/bobmatnyc/trusty-tools/issues/4829))
- **The docx extractor discarded every table and heading boundary, so a 5x4 table indexed as 20 anonymous paragraphs and a heading was indistinguishable from body text.** `docx::extract` streamed `<w:t>` run text and flushed on `</w:p>`, which is the only structure marker it recognised — a `<w:tr>`/`<w:tc>` carried none, and `<w:pStyle>` was never read at all (closes [#4879](https://github.com/bobmatnyc/trusty-tools/issues/4879))
  - a `<w:tbl>` now emits one `| cell | cell |` line per row with a markdown `| --- |` delimiter after the first, so the column boundary survives into `chunk_text`, which sees extraction output as plain lines. Cell text is whitespace-collapsed and literal `|` is escaped, so the delimiter cannot be confused with content
  - a heading paragraph is prefixed with markdown `#` markers at its outline depth, resolved from `word/styles.xml` (`<w:name>` / `<w:outlineLvl>`) with a fallback to the style ID itself for documents that ship no stylesheet. `w:outlineLvl` 9 means "body text" in OOXML, so Word's built-in `TOC Heading` is correctly not reported as a heading
  - nested tables render inside the enclosing cell instead of escaping to the body and desynchronising the outer row; a multi-paragraph cell stays one column
  - a document that ends inside an open `<w:tbl>` emits its table content instead of dropping it. `</w:tr>` and `</w:tbl>` are the only paths that flush a table, so without an EOF unwind a `.docx` read mid-write — which the file watcher does routinely, since Word and LibreOffice write in place — indexed with no error and no warning while every cell was missing. The unwind replays the same close helpers the well-formed path uses, so its output is byte-identical to the closed document's rather than an approximation
  - `<w:tbl>` nesting is capped at 16 deep. The parse allocates one context per open table, so a crafted `document.xml` under the 50 MiB cap could otherwise drive millions of them from ~19 bytes of markup each. Past the cap a table is not tracked and its text degrades to plain prose rather than being lost
  - `word/styles.xml` is read under the same `MAX_DOCUMENT_XML_BYTES` bound and `Read::take` hard cap as the body, so the added part is not a new zip-bomb surface. An unreadable stylesheet degrades to the style-ID fallback and logs, rather than failing the extraction
  - on this repo's own `docs/trusty-analyze/research/code_search_analysis.docx` the recovered outline goes from 0 headings to 9 (1 title + 8 numbered sections); on a 6-heading/4-table fixture, from 0 headings and 0 table delimiters to 6 and 4 — the numbers the #4875 spike measured `anydoc` reaching, without adopting it
- **The dashboard reported itself offline on every reload of a hash-routed URL (`/ui/#/`).** `computeBase()` in `ui/src/lib/base.js` ran its `$`-anchored `index.html` / `ui/` strips against the raw `document.baseURI`, which includes the URL fragment, so the `ui/` mount segment was never stripped and every API call resolved under `/ui/` — onto the SPA catch-all, which answers `200 text/html` with `index.html` (closes [#4980](https://github.com/bobmatnyc/trusty-tools/issues/4980))
  - nothing failed at the HTTP layer, which is why this went unnoticed for ~18 minor versions: `request()` in `api.js` falls back to `res.text()` for non-JSON and handed callers an HTML string, while `EventSource` hard-failed on the wrong Content-Type
  - `router.svelte.js` writes the hash on the first sidebar click, so a first visit to bare `/ui/` worked and every reload, restored tab, or bookmark afterwards did not — the steady state after a minute of use. A query string (`/ui/?tab=1`) misrouted identically
  - the strips now run against `new URL(document.baseURI).pathname`, which carries neither fragment nor query, re-joined to `origin`. The `window.__SEARCH_BASE__` override branch and the non-browser guard are unchanged, and the proxy case still resolves `https://console/proxy/search/ui/#/` to `https://console/proxy/search/`
  - latent since v0.24.10 (introduced in d087b888); the same fix is applied to the KEEP IN SYNC copies in trusty-memory and trusty-analyze
  - three `hash-routed load` cases added to `ui/src/lib/base.test.js`, verified to fail against the unfixed code and pass after; the committed `ui-dist/` bundle is regenerated, since CI and release set `SKIP_UI_BUILD=1` and ship whatever is committed
- **Reindex teardown no longer waits out a poll tick to stop the RSS pollers.** Both pollers tested their stop `AtomicBool` only at the top of their loop, so a poller parked in `Interval::tick()` could not see the flag until the tick expired; `stop_pollers` then made it worse by not signalling the sidecar poller until the daemon poller had already joined, so the two waits ran back to back instead of together. The pollers now park on a shutdown `Notify` alongside the tick, and `stop_pollers` signals both before awaiting either (closes [#5047](https://github.com/bobmatnyc/trusty-tools/issues/5047))
  - measured on the same fixture, `stop_pollers` end to end: **1399–1401ms before, 0.14–0.97ms after**
  - sampling cadence is unchanged (1s daemon / 500ms sidecar) — the fix is a wakeup, not a shorter interval, so RSS is sampled exactly as often as before
  - nothing is skipped: the pollers already did no work between the flag being set and their exit (they re-check the flag at the top of the loop and break before sampling), and `finish_reindex` still takes its own synchronous post-teardown sample for both peaks
  - the stop flag remains the source of truth, so a lost wakeup degrades to the old wait-out-the-tick behaviour rather than to a poller that never exits
  - covered by `pollers_tests.rs`. The two halves are pinned separately: reverting the wakeup takes `stop_pollers_returns_without_waiting_out_the_tick` to 1401ms against its 750ms ceiling, and reverting the signalling order fails `stop_pollers_signals_both_before_awaiting_either`, which observes the order directly rather than timing it — once the wakeup exists that reordering costs ~100µs, below anything a wall-clock assertion could catch without flaking
- **A declared semantic search from a `tm` worktree had nowhere to go.** [#5060](https://github.com/bobmatnyc/trusty-tools/issues/5060) registers session worktrees `skip_vector`, and [#5068](https://github.com/bobmatnyc/trusty-tools/issues/5068) turned a pinned `"stage": "semantic"` against such an index into a permanent `503 vector_unavailable`; [#5579](https://github.com/bobmatnyc/trusty-tools/pull/5579) then registered the repo's base checkout with the vector lane on, but nothing sent the query there. `POST /indexes/:id/search` now resolves that refusal against the sibling index carrying the same `repo_identity` whose vector lane is ready, and answers from it — `meta.routed_from_index`, `meta.served_by_index` and `meta.served_root_path` name both indexes and the tree the returned paths are relative to, since the base checkout sits on a different commit than the worktree (part of [#5069](https://github.com/bobmatnyc/trusty-tools/issues/5069))
  - the routing is triggered by the caller's own declaration of the semantic lane and by nothing else: an unpinned query still degrades to lexical unchanged, and a vector-carrying index of a DIFFERENT `repo_identity` is never substituted
  - when no sibling qualifies — no stored identity, none built with vectors, none loadable, corpus unreadable — the original `503` is returned untouched
- `trusty-search integrate cursor` no longer writes a Cursor rules file telling agents to call `search_code`, a tool the MCP dispatcher has never had. The rules now name the real tools (`search`, `search_lexical`/`search_semantic`/`search_kg`, `grep`, `search_similar`, `get_call_chain`, `reindex`, `index_status`, `search_health`), and the test pins them against `tool_descriptors()` instead of a literal string — the old assertion was wrong in the same way the rules body was, so a green suite proved nothing. The root `README.md` tool list was corrected to the full 21-tool set at the same time (closes [#5138](https://github.com/bobmatnyc/trusty-tools/issues/5138))
- `indexes.toml` writes now take a cross-process advisory lock, not just the
  process-wide mutex added in
  [#5335](https://github.com/bobmatnyc/trusty-tools/pull/5335)
  ([#5344](https://github.com/bobmatnyc/trusty-tools/issues/5344)). `prune`,
  `prune-orphans` and `migrate storage` run as separate processes from the
  daemon, so nothing ordered their whole-file overwrites against its writes —
  a session on 2026-08-05 observed five daemons against one registry with
  last-writer-wins. Every write now blocks on an `indexes.toml.lock` sidecar
  via the shared `trusty_common::file_lock` entry point.
- `prune`, `prune-orphans` and `migrate storage` no longer republish the
  survivors of the snapshot they loaded. They remove or patch BY ID, re-reading
  the current file under the lock, so an index registered while the operator was
  reading the report — or while the migration was copying data directories — is
  no longer deleted with the command reporting success.
- The `daemon.env` malformed-line warning no longer logs the offending line verbatim. `daemon.env` is an operator file holding live provider credentials, and a typo'd assignment — `OPENROUTER_API_KEY sk-…` written with a space instead of an `=` — is exactly the shape that reaches this arm, so the secret was landing in the daemon log in cleartext. The warning now reports the line number, the line's character count, and its leading token only when that token has the shape of a conventional environment-variable name; a bare secret on its own line degrades to the length alone. This matches the success path, which has always logged loaded key names and never values
- `POST /indexes/{id}/index-file` and `POST /indexes/{id}/remove-file` now load a
  cold-parked index and apply the write, instead of answering
  `503 index_not_resident` and pointing the caller at
  `POST /indexes/{id}/search` to warm the index as a side effect (#5349). Both
  handlers route through the same resolve-or-lazy-load function `search` uses, so
  a write and a read against identical daemon state can no longer disagree about
  whether an index is reachable — which mattered most for network-mounted roots
  (#3408), where these two endpoints are the only thing keeping the index
  current. A load that genuinely fails still refuses the write: the caller gets
  the residency verdict (`index_restore_failed` / `index_loading` / `404`), never
  a `200` for a write no index received. `index_not_resident` is now unreachable
  from these two endpoints, so a caller branching on it should branch on
  `index_restore_failed` instead; `status`, `chunks`, and `grep` still report it
  and still name `search` as the way to clear it.
- MCP tools now surface a daemon `503` as a structured error instead of a prose
  string (#5350). A cold-parked, restore-failed, corpus-unavailable, or
  vector-unavailable index used to reach an MCP client as
  `POST <url> returned 503 Service Unavailable: {…}`, so the `error` code,
  `retryable` flag, and `restore_via` hint added in #5345 survived only as text.
  `tools/call` now carries the daemon body verbatim in `_meta` under
  `error_code: "INDEX_UNAVAILABLE"`, and the bare-method form carries it in
  `error.data` under the new JSON-RPC code `-32012`. Applies to every verb
  (`search`, `index_status`, `list_chunks`, `index_file`, `remove_file`,
  `get_call_chain`, `delete_index`). A 503 with no JSON body, or any other
  status, is unchanged.
- `POST /indexes/{id}/graph` no longer reports success for a contribution that
  is not queryable (#5505). When the contributed-overlay merge failed, the
  endpoint answered `200` with `replaced: true` and graph totals that excluded
  the contribution just ingested, while queries silently returned incomplete
  results. It now answers `503 contrib_not_merged` with `persisted: true` — the
  contribution is durable, so the next successful rebuild restores it.
- That 503's `retryable` is earned rather than assumed. One unreadable
  `kg_contrib` row fails the whole load, so the response now names it in
  `blocking_producer` and reports `retryable: false` when the blocker is
  ANOTHER producer's row — retrying that ingest would fail identically forever.
  It stays `true` when the blocker is the caller's own row (a re-send replaces
  it) or when no single row is implicated.
- A reindex whose contributed-overlay merge fails no longer reports the graph
  stage `Ready`. The rebuild installs no graph in that case, so `Ready` would
  have described the pre-reindex graph as the run's product; the stage is now
  `Failed` with the reason, and `kg_complete` / `complete` carry
  `kg_contrib_merge_error`. The lexical and semantic stages are untouched.
- A failed contributed-overlay load no longer replaces the serving symbol graph
  with a derived-only one, and a lost (panicked or cancelled) save/merge worker
  no longer replaces it with an EMPTY graph. Both now install nothing and keep
  serving the previous graph until a rebuild succeeds.
- A failed derived-KG persist keeps merging (the in-memory graph is complete)
  but is now reported: the ingest response carries
  `derived_graph_persist_degraded` instead of only writing a log line.
- **A reindex destroyed every contributed graph and reported success.** The staged corpus swap opened a fresh `index.redb.tmp`, seeded it with chunks, entities, file hashes and `_meta`, and renamed it over the live corpus — `kg_contrib` was in neither the fresh file nor the seeding copy, so the promotion deleted it. A force reindex skipped seeding entirely and lost it the same way. Nothing rebuilt it afterwards: `kg_nodes` / `kg_edges` are derived from the chunk corpus and genuinely regenerated, but `kg_contrib` is written only by `POST /indexes/{id}/graph`, which a reindex never calls. The overlay is now carried into staging on both paths before any batch writes, restoring the ADR-0009 contract that contributed data survives restart *and* reindex ([PR #5527](https://github.com/bobmatnyc/trusty-tools/pull/5527))
  - the loss was silent because `load_contrib_graphs()` on an empty table returns `Ok(vec![])`, which reads identically to "nothing was ever contributed" — not an error path, so the run reported `Complete` and the graph stage `Ready` while every contributed edge was gone
  - a force reindex carries the overlay too. Starting empty is right for chunks, which the run rebuilds; it is wrong for externally-supplied data no part of the daemon can regenerate
  - `copy_all_from`'s doc comment claimed all KG tables are rebuilt at the end of every reindex. That is true of the derived pair and false of the contributed table, and the sentence is what made the defect invisible to a reader; it now distinguishes the two
- **`corpus_corruption_quarantine_4227.rs` used a `PersistedIndex` struct literal that no longer compiles.** #5523 marked `PersistedIndex` `#[non_exhaustive]` and converted every struct-literal construction site that existed at the time of its sweep; the #4227 quarantine test file landed in parallel and was never visited, so `cargo check -p trusty-search --tests` failed with `E0639` once both were on `main`. The test now builds through `PersistedIndex::new()`. `finish_reindex`'s teardown helpers (`stop_pollers`, `resolve_corpus_swap`, `resolve_hnsw_swap`, `rebuild_kg`) are also extracted into a new `finish_teardown` module, bringing `finish.rs` back under the 500-SLOC cap after #5514 and #5527 combined pushed it over
- **14 dangling `Test:` doc-comment pointers repointed to the tests that actually cover the claimed behavior.** #5510, #5514, and #5523 merged with `Test:` citations naming test functions that either landed under a different name or were never written, tripping `scripts/check_test_pointers.sh` on main. Most were renames (`restore_*` → `resolve_*` in `boot_markers.rs`/`persistence.rs`; `registration_with_serve_is_ok` → `registration_with_serve_is_ok_and_reports_the_five_facts`; `real_redb_2x_file_is_old_format_not_corruption` → `old_format_is_backed_up_and_recreated_without_error`) or pointed at the wrong module for an end-to-end test that actually lives in `commands::start_restore`'s `markers_tests` submodule. `degraded.rs`'s citation of `ingest_503_is_not_retryable_when_another_producers_row_is_the_blocker` was a duplicate of an already-cited test's coverage and is consolidated rather than repointed. `health.rs`'s citation of `health_reports_the_deferred_embed_queue_depth` named a test that was never written — no test exercises the live `deferred_embed_queue_depth()` wiring end to end — so the doc now states that gap honestly instead of citing a nonexistent test.
- **The `Teardown guard` CI gate reported ten unguarded durable writes in `core/corpus/contrib.rs` that do not exist.** Every flagged site is a `#[cfg(test)]` unit test over its own `tempfile::tempdir()` `CorpusStore`, unreachable from any live index a `DELETE` could race; the one real production writer, `ingest_graph_handler`, was already declared. The gate skips test regions through `scripts/lib/sloc_awk.sh`'s `#[cfg(test)] mod` matcher, which balanced braces over text with no notion of string literals — so the unmatched `{` in `b"{ not a contrib graph"` left the whole module reading as unterminated and its test code was scanned as production. Region detection now reads a string-aware re-strip of the raw lines, closing three mechanisms: a brace inside any literal form, a `//` inside a string (a URL) that made the comment stripper eat a closing brace, and an unmatched `/*` inside a string — a glob like `"**/*.rs"` — that blanked every line to EOF including the `#[cfg(test)]` attribute itself
  - `commands/integrate.rs`, `core/repo_config.rs`, and `service/grep.rs` were primed for the same false report by the glob mechanism and are fixed by the same change; they were silent only because their tests happen not to call a method in `teardown-guard-methods.tsv`
  - the gate now names a file whose `#[cfg(test)] mod` region it could not close and tells the reader not to add a manifest row. Two independent investigations of this failure both proposed `EXEMPT` rows, because the message named call sites and nothing pointed at the classifier — and an exemption bought to clear a false red outlives the mistake
  - that diagnostic asks the shared matcher whether a region was opened (`emit_opener`) instead of re-scanning the raw file. Its first version used a second, string-blind awk, so a file that merely quoted `#[cfg(test)] mod` inside a string read as a failed close: a genuine unguarded write there would have been answered with "do NOT add a manifest row, fix the parser", which is the exact inversion of the correct fix
- The file watcher no longer silently drops the OS "you missed some, re-scan"
  signal. On a kernel or user event-queue overflow `notify` raises
  `Flag::Rescan`, but `notify-debouncer-mini`'s `DebouncedEvent` has no field
  for the flag, so it was destroyed before the watcher saw it — on macOS the
  surviving path names a directory, which `handle_modified` discarded at its
  `is_dir()` guard, and on Linux the event carries no path at all. The daemon
  never learned about any change in the dropped batch, so searches over those
  files answered as though the edits had never happened.
- Raw events are now tapped ahead of the debouncer, and a rescan triggers a full
  re-walk of the watched tree: every walked file is re-indexed in bounded
  batches, chunks for tracked files that no longer exist on disk are dropped,
  and the symbol graph is rebuilt once.
- A reconcile that does not fully reconcile the tree is retried with backoff
  instead of being reported as success. That covers both ways a pass falls
  short: it returns an error, or it returns successfully having skipped files it
  could not read. The partial case previously cleared the consecutive-failure
  count and scheduled nothing, so a transiently unreadable file was left stale
  behind a single WARN until some unrelated event happened to touch it. Files
  that could not be read are counted into `RescanStats::files_unreadable`.
- `POST /indexes/:id/index-file` no longer answers `"indexed": true` for a write the per-index `TRUSTY_MAX_CHUNKS` cap discarded. It now returns `500 index_file_failed` naming the cap and how many of the file's chunks were dropped. Previously an index that had reached its cap reported success for every subsequent write, permanently, while accepting nothing — and a search over the discarded content was indistinguishable from a correct empty result. Re-indexing a file already in the corpus still succeeds at cap, since the cap only rejects new chunk ids.
- `CommitTimings::chunks_dropped_by_cap` now also counts chunks dropped by the corpus write-lock cap re-check, which a concurrent commit can reach even when the pre-filter let them through; `CommitTimings::chunks` counts only what was committed.
- The file watcher no longer orphans chunks when a file lands only partly at the chunk cap. `index_file` raises its refusal after committing whatever fit, and the watcher used to record nothing on that error — so a later delete found no tracked chunk ids and left the committed chunks in the corpus, BM25 and HNSW for the index's lifetime, with a later edit's stale-chunk removal skipped the same way. It now records the ids the corpus actually holds for the file, read from the corpus rather than from the id set the file parsed into.
- The M001 schema migration now reports how many chunks the cap discarded while re-chunking `pub const`/`pub static` declarations. A chunk id embeds its chunk type, and M001 only visits files with no `Constant` chunks, so every chunk it splits out is new to the corpus and subject to the cap — a legacy index near its cap could complete the migration having silently indexed less than it claimed. The count is a warning, not an error: failing the migration would re-run the whole pass at every boot and drop the same chunks again.
- The `search_health` MCP tool no longer reports an index whose chunk count is UNREADABLE as one holding zero chunks. The daemon's `chunk_count` is optional and `GET /indexes/:id/status` deliberately sends `null` under a 200 when the durable corpus failed to open, so reading it with `unwrap_or(0)` turned "I could not measure this" into "it is empty" — and prescribed `trusty-search index` / `doctor --fix`, a reindex, against a write-quarantined corpus whose chunks are intact on disk. An absent, null, or non-numeric count now reports `index_unknown` and forwards the `corpus_open_failure` block that arrives in the same body and was previously discarded, with remediation keyed to the daemon's own `transient` classifier: retry when it says the failure is transient, operator action when it does not. A genuine count of `0` still reports `index_empty` with its existing reindex remediation.
- `/health`'s `warm_boot_degraded` recompute no longer clears a real degraded signal because it could not read an index's stages. A contended `try_read` was folded into "this stage did not fail" and the result written to the sticky flag; the identical idiom in the `/health` poll is justified by a re-scan two seconds later, which this one-shot recompute does not have, so an unreadable handle cleared the flag until the daemon restarted. Contention is not hypothetical here — the deferred-embed catch-up that triggers the recompute takes the same write lock, and tokio's fair `RwLock` fails `try_read` on a merely queued writer. An unreadable handle now blocks only the CLEARING of the flag and never manufactures a failure, and the recompute reports whether its scan was conclusive so an inconclusive one leaves the deferred-embed drain edge armed for the next poll instead of spending it.
- **`neither_source_leaves_the_project_fallback_intact` read the invoking shell's `TRUSTY_INDEX` instead of the clean environment it asserts.** Two of its rows parsed in-process, and clap resolves `env = "TRUSTY_INDEX"` from the live process environment during `try_parse_from`. The first row failed outright under any shell that exports the variable. The second masked more than it reported: it asserts that `--project` still derives an index id when nothing else pins one (#1373), and with the variable set it passed on the environment value, so the derivation it names was never exercised. Both rows now spawn the clean child process the module's other rows already use (closes [#5709](https://github.com/bobmatnyc/trusty-tools/issues/5709))
- Stopped `memguard`'s doc comment from linking to
  `trusty_common::sys_metrics::physical_footprint_mb`, which is macOS-gated and
  so unresolvable on Linux. The link is cross-crate, which is why repairing
  trusty-common's own six links did not fix it.
- **Merge-base resolution compared two commits, so it never saw uncommitted work.** `core::git::resolve_branch_files` ran `git diff --name-only <base>..HEAD`; on a four-change fixture it reported 1 of the 4 real differences, missing the uncommitted edit, the untracked file, and the deletion. A live worktree is mostly uncommitted work, so [ADR-0050](https://github.com/bobmatnyc/trusty-tools/blob/main/docs/adr/0050-colocated-path-tied-identity-with-delta-indexed-worktree-facets.md)'s delta-indexed worktree facets cannot be built on that answer. First increment of that ADR; it adds no indexing behaviour of its own
  - `core::git::resolve_merge_base_delta` is the new entry point. It diffs the merge-base against the **working tree**, adds untracked files via `ls-files --others --exclude-standard`, and returns `changed` and `deleted` separately — a deletion means "drop the base facet's chunks", which the old single list could not express
  - a failed untracked-file step returns `None` rather than a partial delta. A partial delta reads as a complete answer while silently omitting files that exist in the tree, and the caller cannot tell the two apart
  - both git calls use `-z`, so paths containing spaces or non-ASCII bytes arrive verbatim instead of in git's quoted form
  - `resolve_branch_files` delegates and keeps its signature, so the [#122](https://github.com/bobmatnyc/trusty-tools/issues/122) branch boost now covers a file being edited before it is committed. Deletions stay out of it — a deleted file has no chunks to boost
- `POST /indexes` now refuses with `409` when the requested `root_path` names a different directory tree than the one already registered under that id. Registration was asymmetric: a second id claiming a registered tree was rejected and hardened three times over, but the same id over a new tree was accepted with `200 {created: false}` while the previously registered tree went on answering every query. Live (resident) handles only — an id whose only claim is a stale cold record still re-registers at the new tree, which is how an index whose tree moved gets recreated at all (#3993 round 3's recreate-after-move path); that reap is now logged at `warn` naming both roots. A genuine re-registration of the same tree — including a differently-cased spelling of one inode — stays idempotent, and the `already exists` response now names the tree it joined so a client can verify rather than infer ([#5827](https://github.com/bobmatnyc/trusty-tools/issues/5827))
- `DELETE /indexes/{id}` no longer releases its teardown guard while the index's
  file watcher still holds the indexer. `WatcherTask::stop` only called
  `JoinHandle::abort()`, which reaps an idle task inline but not a running one,
  so the watcher's `Arc<RwLock<CodeIndexer>>` — and the open redb corpus it owns
  — could outlive the delete. Recreating the same id then hit
  `DatabaseAlreadyOpen`, set `corpus_open_failed`, and answered `500`. `stop`
  now awaits the consumer task's termination (issue #3049).
- `POST /indexes/{id}/search` no longer answers an index whose durable corpus
  cannot be read with an empty result set at HTTP 200. The in-memory chunk map
  and BM25 corpus are a cache of that corpus; idle eviction empties them, the
  rehydrate that would refill them read the same broken corpus and reported its
  failure only to the log, and `fetch_chunks_for_ids` did the same before
  falling back to those now-empty maps. An index holding 85,269 chunks returned
  `results: []` with `bm25_lane_degraded: true` — a flag that means "still
  warming up", which is the one thing this state is not — and the workaround was
  to delete and recreate the index. A failed durable read is now recorded on the
  index and the search refuses with `503 index_corpus_unavailable`, naming the
  index and the underlying fault. The record clears on the next successful read,
  so a transient failure does not wedge the index. Refs #5917.
- The sibling surfaces that read the same corpus refuse it too, instead of
  answering. `POST /indexes/{id}/grep` and `POST /grep` derive their file set
  from the chunk corpus, so an unreadable one answered
  `{"matches": [], "total": 0}` — "this literal is nowhere in your code" for a
  corpus that was never scanned. `GET /indexes/{id}/call_chain` resolves its
  entry point against the same snapshot and answered
  `404 entry point not found` for a symbol that exists. All three now return the
  same `503 index_corpus_unavailable`. The global `POST /search` fan-out still
  answers `200` so one broken index cannot fail the sweep, but now reports the
  index it dropped as `corpus_read_failed_indexes_skipped`. Refs #5917.
- The two producers of `index_corpus_unavailable` carry one field set: the
  open-failure body (#4087) gained `retryable` beside its `transient`, and the
  read-failure body (#6043) gained `failure_kind: "read_failed"` and `transient`
  beside its `retryable`. Refs #5917.
- **An upgrade onto the #767 allowlist gate dropped 103 of 121 registered indexes from warm boot.** The one-time grandfather pass ran only when `allowlist.toml` was ABSENT, so a box whose file already held ~24 hand-added entries never got the migration and `retain_approved_entries` excluded every registered root that file did not list — `warm-boot DEGRADED: only 11/37 indexes loaded` with `skipped_tcc: 0`. Before #5686 nothing read `allowlist.toml` at registration time, so a pre-upgrade copy is a partial record of `index add` calls, never a curated policy (closes [#5926](https://github.com/bobmatnyc/trusty-tools/issues/5926))
  - the pass now keys on the durable `.grandfathered` stamp alone and MERGES the registered roots the allowlist union does not already approve into whatever file is there. It creates the file when absent, exactly as before
  - the stamp is what separates "never approved because the gate is new" from "explicitly de-approved": it is written only by a pass that has had its turn, so once it exists a root the operator pruned stays pruned (`grandfather_does_not_resurrect_a_root_removed_after_the_pass_ran`). The buggy early return never wrote it, so an affected install is still distinguishable and the fix reaches it on the next start
  - an `allowlist.toml` that does not PARSE is left untouched and does not burn the one-time pass — the merge is a read-modify-write, and overwriting an unreadable file would discard every approval in it
  - each root is still re-checked against the hard denylist before it is written, so a sensitive root that predates the gate is refused rather than laundered into a standing approval

- **Nothing observed the exclusion.** It existed only as one `warn` line per entry, so the drop surfaced on `/health` as `skipped_tcc: 0` beside a "< 80% of prior" error whose remedy text was re-granting Full Disk Access — the one explanation those counters ruled out
  - `GET /health`'s `warmboot_summary` gains `indexes_skipped_unapproved`. Any non-zero value forces `warm_boot_degraded` on its own, so an allowlist exclusion is degraded even on a box with no prior count recorded, and `recompute_warm_boot_degraded` will not heal the flag while indexes stay excluded
  - the allowlist exclusion now emits its own `error!` naming the cause and the remedy, and the "< 80% of prior" line drops the Full Disk Access instruction when that counter already explains the drop
- **The `clustering` feature did not compile.** `concept_cluster`'s `chunk_with` test helper builds a `CodeChunk` literal and predates the `on_branch` and `archive_reason` fields; `#[serde(default)]` on those two covers deserialization only, so the literal failed `E0063`. The module sits behind the off-by-default `clustering` feature and no CI job passes that flag, so its 7 tests had never run. The helper now names both fields (closes [#5931](https://github.com/bobmatnyc/trusty-tools/issues/5931))
- `GET /indexes/{id}/chunks` no longer reports a failed corpus read as an empty
  index. Both enumeration paths absorbed the failure: the cursor path turned a
  redb read error into an empty page with `next_cursor: null`, which a paging
  client reads as "corpus exhausted", and the offset path reported the in-memory
  chunk map's length as `total` even when a rehydrate had not committed. An
  index holding 50,929 chunks exported zero of them at HTTP 200, and
  trusty-analyze scored the empty corpus and published
  `complexity_distribution total: 0`. Both paths now return
  `503 index_corpus_unavailable` with `retryable: true`. The offset path waits
  out a slow rehydrate first, retrying on the same `REHYDRATE_RACE_RETRIES`
  budget the BM25 and grep lanes use (~27s at the defaults), so a large cold
  index that simply takes 27-40s to rehydrate serves its corpus rather than
  erroring. Refs #6043, #5917.
- `build.rs` keeps the committed `ui-dist/` bundle instead of running `make release-prep` on every cold build. That target rebuilds the UI and re-mirrors `ui/dist/` into `ui-dist/`, rewriting files git tracks on a build that had no UI change to ship. Freshness is decided by `scripts/check-ui-bundle-freshness.sh`, the same check `preflight-publish.sh` runs as CHECK 7, and an unreadable answer keeps the committed bundle rather than rebuilding it. `FORCE_UI_BUILD=1` rebuilds unconditionally, and the pnpm fallback path re-stamps the bundle only when a build actually ran. Backported from trusty-memory ([#6060](https://github.com/bobmatnyc/trusty-tools/pull/6060), [#5078](https://github.com/bobmatnyc/trusty-tools/issues/5078))
- **The symbol graph keyed every node by bare `function_name` and resolved every callee to whichever definition registered first, so call chains were confident and wrong.** On the 85k-chunk dogfood index a probe over 15 entry points found 99 callee edges of which 12% were same-file and 74% pointed into an unrelated crate; `HnswStore::upsert` anchored to trusty-common's method while its callers were all trusty-search's, and `self.index.write()` bound to an arbitrary workspace `write`. Every consumer of `get_call_chain` (HTTP and MCP) and of KG expansion read those edges as fact — [#6167](https://github.com/bobmatnyc/trusty-tools/issues/6167)
  - node identity is now `<file>::<symbol>`, so two files defining the same name get two nodes. Registration used to keep the first and silently drop the rest: a four-definition corpus built three nodes
  - a callee becomes an edge only when the resolver has grounds — the caller's own file, then its directory, then its package, then a name with exactly one definition in the whole corpus. A name that is ambiguous at the narrowest scope that matched produces no edge at all, rather than an arbitrary one. Cross-file resolution additionally requires a matching file extension, and `CallsFunction` targets must be callable, so a Rust call no longer binds to a `.ts` method or to a module
  - `<path>::<symbol>` now anchors an entry point instead of returning 404, matching a full qualified key or any path suffix of one. `Type::method` and bare names are unchanged
  - a bare entry-point name matching several definitions still anchors to the most-connected one, but the report now names the alternatives instead of hiding the choice
  - `SymbolGraph::resolve_symbol` returns the new `SymbolMatch`; `symbol_for_chunk` and `degrees` are keyed by qualified identity; `callers_keyed` / `callees_keyed` traverse by it. `resolve_entry_point` returns `EntryAnchor` rather than a `(symbol, chunk)` tuple
  - a persisted graph written before this change loads unchanged, with the old one-node-per-name behaviour, until the index is rebuilt
- **A symbol graph persisted before the #6169 fix kept loading, so `get_call_chain` answered with the old bare-name semantics on every index built before the upgrade.** #6169 changed how call edges are constructed; it invalidated nothing already on disk, and the previous release note said so — an existing index kept its stale graph until someone reindexed by hand. The persisted rows carried no marker separating the two formats, so nothing could tell them apart — [#6171](https://github.com/bobmatnyc/trusty-tools/issues/6171)
  - `save_kg_graph` now stamps a KG format version into `_meta`, in the same redb transaction as the rows it describes, so the stamp and the graph can never disagree
  - `SymbolGraph::load_from_corpus` checks that stamp before hydrating anything and returns `Ok(None)` when it does not match. Every caller already reads `Ok(None)` as "rebuild from the chunk corpus", so a stale graph is replaced on the next warm-boot with no manual reindex and nothing to delete
  - the check fails closed: a missing stamp (every pre-#6169 index), a truncated one, an unreadable `_meta` table, and a version from a newer daemon all force the rebuild. A version mismatch logs which format was found and how many nodes were discarded; a corpus that has simply never saved a graph stays silent
  - **upgrade cost:** every index that already exists pays one full symbol-graph rebuild on its first load after upgrading — on a large corpus that is the dominant non-embedding part of a warm boot, and it is paid per index, not once. The rebuild re-stamps the corpus, so every boot after it loads from disk as before. No reindex or embedding work is triggered
- Resolved 12 broken rustdoc intra-doc links in `service::rpc`'s module-level
  docs (`queries`, `writes`, `streams`, `chat`) — private helper names (`guarded`,
  `bulk_guarded`, `unguarded`) are now plain code spans rather than dead links,
  and public items (`register`, `IndexBody`, `SearchAppState`, `METHOD_CHAT`)
  resolve through fully-qualified reference-style links — unblocking the
  rustdoc intra-doc-link publish gate. No behavior change (#6285).
- **A cold-parked index reports the retryable `-32002` end to end, proved through both transports** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285)). Slice 2 proved only the permanent half of the 503 split against a live daemon; the retryable half is the one a consumer acts on, because such an index is registered, built, and one search away from serving.
- **A daemon that cannot bind its socket is now proven to exit rather than serve HTTP alone** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285)). The behaviour shipped in slice 1; what was missing was a test driving `run_daemon()` itself. `run_daemon_refuses_a_socket_another_process_is_serving` starts a real listener on the isolated socket path first, then calls `run_daemon`, and asserts both that it returns an error naming the path and that no `http_addr` file was written — so a half-bound daemon can never announce itself to the consumers the retire slice will move.
- A reindex could leave an index unable to accept any further vector write for
  the life of the daemon. Each reindex checkpoint saved to a staging file and
  recorded it as the store's snapshot source; an idle or memory-pressure
  demotion then re-viewed the store from that file; resolving the swap renamed
  or deleted it without telling the store, so every later write failed
  `usearch failed to promote view → mutable load: No such file or directory`,
  and a failed promote never clears `is_view`, so the store could not heal
  itself. Committing the swap now re-points the store at the live path, and
  aborting it restores the store from the live snapshot. As a backstop for a
  recorded snapshot that vanishes for any other reason, promoting a view whose
  file can no longer be read rebuilds the graph from the mapping still held in
  memory instead of failing forever (#6299).
- `trusty-search setup --client chatgpt` prints the registration a GUI MCP
  client can actually spawn: this binary's absolute path, the `serve` argument
  vector, and a working directory that exists (#6307). Configured with the bare
  command `trusty-search`, ChatGPT desktop failed the spawn with exit 127 —
  launchd starts GUI apps with `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, which
  contains no directory any trusty-* binary installs into — and reported only
  that no tools were found. The path is read from `current_exe`, so it is right
  wherever the binary was installed rather than assuming `~/.cargo/bin`, and the
  working directory replaces the client form's `~/code` default, which does not
  exist on every machine and fails the spawn on its own. ChatGPT desktop keeps
  no local MCP config file that a tool may write, so the command prints the
  three values to paste and changes nothing on disk.
- **Two `commands/start/restore.rs` tests left 6 empty directories in `$HOME` on every run.** `warmboot_counts_every_entry_the_allowlist_excluded` and `a_partial_pre_gate_allowlist_does_not_cost_indexes_on_upgrade` built their fixture roots with `tempdir_in(dirs::home_dir())` inside a `.map()` and then called `std::mem::forget(dir)` to keep each guard alive past the closure. `mem::forget` skips `TempDir::drop`, so `~/ts-warmboot-count*` and `~/ts-upgrade-partial*` accumulated one pair of triples per `cargo test -p trusty-search` — 120 of them on the reporter's machine over nine days (refs [#6333](https://github.com/bobmatnyc/trusty-tools/issues/6333))
  - the guards now `unzip` out of the same `map` into a `Vec<TempDir>` bound in the test body, so the roots still exist while `retain_approved_entries` canonicalizes them and `Drop` removes them at return. The sibling `warmboot_drops_unapproved_entries` already held its guards this way
- **`DELETE /indexes/{id}` was a silent no-op for a registration the #767 allowlist excluded at warm boot.** `retain_approved_entries` drops an unapproved root before it reaches the hot registry or the cold store, and `unregister_index` decided "does this index exist?" from those two stores alone — so the row was neither deletable nor unknown: the handler skipped the whole durable-cleanup branch and answered `200 {"removed": false, "data_deleted": false}` with the `indexes.toml` row and the on-disk data dir untouched. A live 0.49.4 daemon accumulated 60 such rows, each keeping `warm_boot_degraded` true on every boot and clearable only by stopping the daemon and hand-editing the file; the console delete (#6360) reads `removed: false` as a failure and calls the same handler, so it could not clear them either (refs [#6363](https://github.com/bobmatnyc/trusty-tools/issues/6363))
  - a registration-only id is now removed like any other: the `indexes.toml` row is dropped through `persistence::remove_index_registry_entry`, the data dir goes when `?delete_data=true` asks for it, the root is scrubbed from `roots.toml`, and the response reports `removed: true` with the real `data_deleted`
  - an id in NO store and no `indexes.toml` row now answers `404 {"error": "unknown index: <id>"}`. It used to answer `200 {"removed": false}` — the same body a real-but-undeletable registration produced, which is what let 60 of them go unnoticed
  - a delete whose durable cleanup FAILED now answers `500` with `ok: false` and an `error` naming the failed step. The `indexes.toml` rewrite failure was a `warn!` with no representation on the wire, so a delete that changed nothing durable was indistinguishable from one that succeeded. A registration-only delete that fails this way reports `removed: false`, because nothing was removed
  - `persistence::find_index_registry_entry` / `…_at` look one registration up by id. A registry that cannot be PARSED propagates the error rather than reading back as "absent" (#4317/#4871), so an unreadable file can never produce a 404 for an index that is really there
- **The per-index chunk cap is now a property of the index, not of the process.** `CodeIndexer` resolves `TRUSTY_MAX_CHUNKS` once at construction and stores it, and `CodeIndexer::with_chunk_cap` pins a cap on one instance. The cap was previously re-read from the environment on every insert, so the `chunk_cap` tests — which needed a cap of two or three — pinned that value across the whole test process and silently truncated every other index alive at that moment. `#[serial_test::serial]` did not contain it: that attribute excludes only other `#[serial]` tests, and the tests it corrupted were not tagged. `cargo test -p trusty-search -- chunk_cap tests_cursor eviction_kg_paths` failed 6 runs in 8 before this change and 0 in 10 after ([#6369](https://github.com/bobmatnyc/trusty-tools/issues/6369)).
- A reindex progress stream opened while a reindex was running could lose an
  event or deliver one twice. `ReindexProgress::push` appended to the replay
  buffer under a lock and broadcast after releasing it, while a stream opened by
  snapshotting the buffer under that lock and subscribing after releasing it — so
  an event appended after the snapshot and broadcast before the subscribe reached
  neither path, and one appended before the snapshot but broadcast after the
  subscribe reached both. A dashboard silently skipped a batch, or showed one
  twice, with no error anywhere. `push` now broadcasts while still holding the
  lock, and both transports open through one new
  `ReindexProgress::subscribe_with_replay`, which takes the replay buffer, the
  status, and the subscription under that same lock. Every event emitted through
  `push` or `push_terminal` now lands on exactly one path. Refs #6386.
- A stream opened while a reindex was FINISHING could end without its terminal
  frame. The six terminal transitions in the reindex runner stored the status and
  pushed the terminal event as two separate unlocked steps, and on the `Complete`
  path an RSS poll, a git subprocess, a marker-file write and two `RwLock` writes
  sat between them. A stream opening in that window read a terminal status while
  the replay buffer still lacked the terminal event, and both transports stop
  reading the live channel once the status is terminal — so the stream ended
  silently and the client waited on a completion that had already happened. A new
  `ReindexProgress::push_terminal` stores the status and emits the event under one
  hold of the replay-buffer lock, and every terminal transition routes through it.
  `ReindexTerminationGuard::drop` remains the one emission outside that rule,
  because `Drop` cannot await the lock. Refs #6386.
- `GET /indexes/{id}/reindex/stream` (SSE) and `search.index.reindex.stream`
  (Unix socket) shared the bug because each had its own copy of the two-step
  open. Both now call the one method, so the two transports cannot drift apart
  again. Neither route's observable frame sequence changes for a client that was
  not hitting the race. Refs #6386.
- `trusty-search cleanup` re-reads each index's registration immediately before its own delete, instead of deleting straight from the listing an operator confirmed minutes earlier. An index id is derived from its `root_path`, so a root wiped and recreated inside that window named a live, freshly-reindexed index under the same id — and this command deletes with `delete_data=true`. The delete now proceeds only when the fresh `GET /indexes/{id}/status` succeeded, still reports `chunk_count: 0`, and still reports the root the listing showed; it carries that root as `expected_root_path` so the daemon repeats the comparison under the teardown lock. Every re-check failure — unreachable daemon, non-2xx, an unparseable body, a missing count or root, a populated index, a moved root — refuses that id, reports why, and exits non-zero. The listing step no longer reads an unanswered status as zero chunks either: those indexes are counted and left alone rather than offered for deletion (#6410).
- A reindex halted by the background memory poller reported `"status":
  "complete"` and `"memory_limit_hit": false` on its terminal SSE frame, while
  the daemon recorded the same run as `AbortedMemory`. The frame derived those
  two fields from the batch loop's own `mem_limit_hit` flag, and the enum status
  derived from `mem_limit_hit || mem_abort` — the poller sets `mem_abort` on its
  own tick and the producer then halts at the next batch boundary, so a run can
  end with `mem_abort` set and `mem_limit_hit` never set. A consumer reading the
  wire string, which is what `trusty_common::monitor::search_client` does, saw a
  memory-aborted reindex as a success and would retry straight into the same
  ceiling. Both fields now come from the terminal status the frame already
  carries, so the payload and the enum cannot disagree. `RunTotals::mem_limit_hit`
  is gone with them. Refs #6415, #6386.

### Changed

- `trusty-search index relocate` approves the destination before calling the daemon, and withdraws that approval if the call fails. Approving afterwards meant the now-gated `PATCH /indexes/:id` refused every destination that was not already approved — the normal case for a moved repo (#767).
- `SearchAppState` is `#[non_exhaustive]`. It gains fields regularly and each one was a breaking change purely because an external struct literal could name them all; taken alongside the `allowlist_paths` break so the next field costs nothing (#767).
- `service::daemon::pid_alive` is now `pub` and is the crate's single implementation. `commands::stop` carried a byte-identical private copy; the new staging reaper would have made a third. `pub` rather than `pub(crate)` because `commands::stop` lives in the binary crate and reaches this through the library's public `service` module. Additive only — no existing signature changed
- `core::corpus_recovery::is_incompatible_corpus_format` and
  `backup_incompatible_corpus` now delegate to
  `trusty_common::redb_open::is_incompatible_format` and
  `backup_incompatible_file`, and `INCOMPATIBLE_CORPUS_SUFFIX` aliases
  trusty-common's constant (#5063). Same verdict and same quarantine path —
  suffix plus numbered anti-clobber fallback — for every input. The corpus
  recovery policy is unchanged and stays here: it fails CLOSED on genuine
  corruption (#4227) via `is_genuine_corpus_corruption`, which remains
  trusty-search's own predicate, and it opens with a tuned page-cache size.
- Daemon-address resolution now runs through
  `trusty_common::daemon_guard::DaemonAddrLayout::TRUSTY_SEARCH`
  ([#5670](https://github.com/bobmatnyc/trusty-tools/issues/5670)). The private
  copy in `commands::daemon_utils` is deleted: `daemon_base_url()`,
  `daemon_port_path()`, `service::http_addr_path()`, and
  `service::write_http_addr_file()` are now thin bindings to the shared
  implementation, and `service::DEFAULT_PORT` reads its value from the same
  layout. Every discovery path, fallback, probe timeout, and refresh write is
  byte-for-byte what it was — the change exists so `tga`, which cannot depend on
  this crate, stops needing a second copy of the rules that #117 and #3545 were
  fixed in.
- **MCP protocol primitives now come from the `trusty-mcp` crate instead of `trusty_common::mcp`** — imports move from `trusty_common::mcp::…` to `trusty_mcp::…`, and the `trusty-common/mcp` feature is replaced by a direct `trusty-mcp` dependency. No behaviour change: the types and functions are byte-identical, only their home crate moved (ADR-0040, [#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803))
- **`core::bm25` now exposes a trusty-search domain type instead of re-exporting the shared scorer.** The module was a 16-line `pub use trusty_common::bm25::{tokenize, BM25Index as Bm25Index}`, while trusty-memory already wrapped the same scorer in its own `PalaceBm25Index`. The two subsystems share one BM25 implementation but differ in persistence and lifecycle — search keys documents by chunk id against redb/usearch, memory keeps per-palace snapshots — so search-specific behaviour had nowhere to live except inside the shared core that memory also depends on. `CodeBm25Index` is a newtype over `trusty_common::bm25::BM25Index` exposing the eight operations the code indexer actually performs; every method delegates straight through, so scoring, tokenization, and the per-document term cap are unchanged. `trusty_common::bm25::BM25Index` itself was not modified
- **Breaking: `trusty_search::core::bm25::Bm25Index` and `trusty_search::core::bm25::tokenize` are removed.** `core` is re-published from `lib.rs`, so both were public API. Keeping the alias would have left one module exporting two names for BM25 when trusty-search called neither the alias nor `tokenize` anywhere in its own source. A caller that wants the raw scorer should depend on `trusty-common` and name `trusty_common::bm25::BM25Index` directly, which is what trusty-memory already does
- The dashboard's Svelte source moved out of this crate to `crates/trusty-console/ui-search/`, where trusty-console builds it and serves it at `/tools/search/`. `/ui` still serves the same bytes: the crate-root `ui-dist/` bundle stays committed and embedded, now mirrored from the console's build by `make -C crates/trusty-search sync-ui`. `build.rs` no longer builds anything and reports a stale bundle instead of rebuilding it (#6155, #6284).
- `POST /admin/stop` now answers `503 shutdown_unavailable` (`retryable: false`)
  when no shutdown driver is listening, instead of reporting `ok: true` for a
  daemon that will keep running. A live daemon subscribes before it serves, so
  the refusal fires only when the stop genuinely cannot happen (#6285).
- The Unix socket accepts frames up to 64 MiB instead of the shared 8 MiB
  control-plane default, matching the `DefaultBodyLimit` that
  `POST /indexes/{id}/graph` already carried — which now names that constant
  rather than restating the literal. A client reads its own responses under its
  own budget, so a consumer dialling these names must use
  `trusty_common::uds::send_framed_request_capped` with the same figure (#6285).
- **Every socket method's lane is now pinned, not one representative per lane** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285)). Slice 4's review recorded that its two lane tests each covered a single method, so moving one between `bulk_write!` and `free_write!` by mistake changed behaviour under load with nothing failing — the `*_over_the_socket_matches_the_http_body` tests compare bodies and say nothing about admission. `lanes_tests.rs` now carries a row per method, checked against `service::socket::METHODS` first so a method that reaches the socket without a row fails rather than going untested. All twenty-five are asserted against a saturated limiter; the deadline axis is asserted against a subject that genuinely pends, because `tokio::time::timeout` polls its inner future before its deadline and a fast handler answers identically in either lane.
- **Three refusal classes get their own JSON-RPC codes instead of `internal_error`** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285)). The write surface is the first to refuse for reasons a caller can act on but did not cause by malforming its request, and all three read as "file a bug" under the old mapping: `403` (root not approved for indexing) answers `-32003`, `409` (a registry collision) answers `-32009`, and `429` (the reindex cooldown) answers `-32013`. The first two are the numbers `trusty-mpm` already uses for the same meanings. `500` is unchanged and still `internal_error` — a corpus that would not open is not something the caller could have sent differently.
- Deleting an index now deletes its on-disk data by default, and keeping the data is the explicit opt-out (owner ruling, #6422).
  - `trusty-search index remove` sends `?delete_data=true` and asks "This cannot be undone." before it runs. `--keep-data` deregisters only and leaves the corpus; `--yes` answers the prompt for a script. A non-interactive run without either is refused rather than prompted into a hang.
  - `index remove` now reads the daemon's answer instead of its status code. `200 {"removed": false}` is a failure, and a delete whose registration went while `data_deleted` came back `false` is a failure too — it used to print a tick, recording the corpus as reclaimed while every byte was still on disk (#3049). The local config and allowlist rows are still cleared in that case, because the registration really is gone.
  - The `delete_index` MCP tool takes a new `delete_data` argument, absent ⇒ `true`. A present-but-non-boolean value is `InvalidParams` rather than a coerced guess. **Breaking for remote callers that wanted deregister-only**: there was no way to ask for it before, so nothing that worked stops working, but the tool's schema and description now advertise the choice.
  - `DELETE /indexes/{id}` itself is unchanged: absent `delete_data` still means preserve (#4123). Every surface above sends the flag explicitly, so the two defaults never have to agree.
- Each `indeterminate` row of `GET /registry/orphans` now carries `colocated` and `repo_identity`, the same registration metadata an `orphans` row already carried. A caller offering a per-row review of a root the daemon could not check needs to show what the registration is before an operator settles it.

### Security

- **A failed read of either input to the reindex root-move trust gate let an untrusted root override through.** Both inputs degraded to "trusted": `handle.read_indexed_root().await.unwrap_or(None)` turned a redb error into "this index has no prior root", which skips the #2178 gate entirely, and `load_index_registry(…).ok()` turned an unparseable `indexes.toml` into "no persisted entry", which `root_move_is_trusted` answers `true` for. The gate now refuses on either read failure — `409` from `POST /indexes/:id/reindex`, `mark_reindex_failed` plus a fatal SSE `error` event from the runner (closes [#5357](https://github.com/bobmatnyc/trusty-tools/issues/5357))
  - refusing costs a retry; proceeding walks an unvalidated root and lets the finish-phase prune pass delete every chunk it does not find there — the #402 root-hijack incident's ending, reproduced in `reindex_refuses_when_the_corpus_indexed_root_read_fails`
  - `Ok(None)` still means "nothing to relativize against" and still skips the gate, so BM25-only and never-stamped indexes are unaffected; only a genuine `Err` refuses
  - a third fail-open sat inside the read itself: `read_indexed_root_sync` returned `Ok(None)` when the stored `indexed_root` bytes failed UTF-8 decode, so a CORRUPTED root read back as "never indexed" and skipped the gate before any of the above could see it. Only `write_indexed_root_sync` writes that key and it always writes valid UTF-8, so the corrupt case is now an error
  - the decision moved into `service/reindex/root_gate.rs`, so the runner's spawn-time gate and `reindex_handler`'s request-time mirror of it are now one function instead of two copies that had already drifted
  - a root MOVE now applies `set_root_path` in the runner, after its own gate, instead of in the handler at request time. One read of `indexes.toml` drives both the decision and the mutation, so a registry write landing between the two reads can no longer strand the indexer on a root the corpus was never relativized against — dangling paths that pass the lexical `file_is_within_root` filter and report `stale_index_root: false`. An override where the corpus already agrees with the new root still syncs in the handler; nothing there needs deciding. Until the runner's gate passes, the handle's new root and the indexer's old one disagree, which fails closed: empty results with `stale_index_root: true`
  - a refusal also points the indexer back at the root the corpus is actually relative to, for the routes to that divergence the deferral does not cover (a stale in-memory handle)

### Documentation

- **The module `Test:` pointer in `commands/daemon_utils.rs` now names in-crate tests.** It cited `trusty_common::daemon_guard::addr_tests`, which `scripts/check_test_pointers.sh` can never resolve because it scopes a pointer to the citing file's own crate — so the pointer broke the `Test pointers` CI gate on `main`. It now leads with `daemon_base_url_falls_back_when_http_addr_dead` and `daemon_base_url_prefers_isolated_instance_over_stale_default_cache`, the two regression tests in that file that exercise the delegation, and keeps the trusty-common reference as prose.
- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).
- **Module docs render once instead of twice.** 11 modules carried both an outer `///` on their `mod x;` declaration and their own inner `//!`; rustdoc concatenates the two, so each module page showed two summary lines and two Why/What/Test triples. The outer is gone and the inner `//!` is now the single module doc, per the `//!` convention in `documentation-style` and DOC-38 §3.1 ([#5754](https://github.com/bobmatnyc/trusty-tools/pull/5754))
  - no prose was lost: each pair was read on both sides and the outer removed only where every fact already appeared in the inner
  - `service_unit::launchd_unit_tests` was the one place the two sides were byte-identical — its macOS-gating paragraph appeared verbatim twice
  - links in the merged doc used to resolve against the parent module's scope, which is what broke 452 of the 852 intra-doc links repaired in #5744; inner-only removes that trap rather than working around it
- Fixed the stale doc comment on `SHRINK_GUARD_RATIO_DIVISOR`
  (`core::store::usearch_store`), which still described the periodic HNSW
  persister's checkpoint race as open work tracked by #3970. #3970 is closed;
  #3975 shipped the staged-write-then-swap the comment cited as a
  recommendation, and the comment now describes that fix (#6202).
- The `service::rpc` module headers link their own items again. `error` and
  `reads` each carry a `///` doc on their `pub mod` line, which merges with the
  module's `//!` header and makes rustdoc resolve the whole doc in
  `service::rpc` — so `rpc_error_from_http`, `RpcError`, `CODE_UNAVAILABLE`,
  `CODE_UNAVAILABLE_PERMANENT` and `register` rendered as dead literal text and
  denied `cargo doc`. Each header now ends with fully-qualified reference
  definitions.

## [0.44.0] — 2026-08-10

### Added

- Reindex now reports a per-stage wall-clock breakdown — hash-cache load, corpus
  carryover copy, batch pipeline, prune, HNSW commit, corpus commit, KG, and an
  explicit unattributed remainder — in the `reindex phase timings` log line and
  in the `complete` SSE event's `timings` object. This replaces the derived
  `model_load_approx_ms` residual, which folded five distinct stages into one
  number computed by subtraction, leaving the cost of a corpus carryover copy
  unmeasurable. Adds `tests/reindex_stage_profile.rs`, an `#[ignore]`d harness
  that prints cold and warm breakdowns against a throwaway temp corpus (#5024).

### Fixed

- Updated doc comments, `Cargo.toml`, and `CLAUDE.md` that still named
  `open-mpm` as a consumer/orchestrator to say `trusty-agents` (renamed in
  #831), and removed an orphaned `.open-mpm/agents/` fixture directory
  (2 stray `.toml` files, unreferenced by any code) that was left behind in
  the crate tree.
- `cargo test -p trusty-search` compiles again. #5345 gave `index_status` and
  `chunks` a JSON body, changing their error type from `StatusCode` to
  `(StatusCode, Json<Value>)`, but two `assert_eq!` calls in
  `deleted_cold_parked_index_is_404_not_a_permanent_503` still compared the
  whole tuple against a bare `StatusCode`, so the lib-test target failed to
  build and took every gate on `main` with it. The three guards that test pins
  now destructure the response and assert both the 404 and the
  `unknown index: <id>` body, so a regression that returns 404 while still
  advertising `restore_via` is caught rather than passing on the code alone.
- A write-quarantined index (#4122) no longer performs any durable write. Its
  shutdown flush used to write its deliberately-empty in-memory corpus over the
  legacy `chunks.json` snapshot — a file the warm-boot `chunks.json →
  index.redb` migration still reads — while the quarantine's own ERROR
  diagnostic claimed the on-disk corpus was untouched. The gate now sits on the
  whole snapshot-write family (`save_chunks_to_disk`, `flush_corpus_to_disk`,
  `save_vector_store`, `spawn_incremental_persist`), so the HNSW graph is
  protected on the same path, and the diagnostic's claim is true of every
  on-disk artifact.
- Warm boot no longer spends a full tracked-root relocation walk on every registry entry whose `root_path` was deleted. The walk (measured 9.5–10.5 s over 248 tracked roots) was recomputed per dead entry even though its result is identical for all of them within a boot; 55 dead entries on the reporting machine kept a live 70k-chunk index unreachable for the better part of an hour. Entries are now triaged with a single `stat`, live indexes are restored first, and the dead cohort shares one walk under a global `TRUSTY_WARMBOOT_SALVAGE_SECS` ceiling (default 30 s, `0` disables). No registration is removed and no index data is deleted by a failed or skipped probe. (#4846)
- An index skipped by a warm-boot restore timeout is now retried on a backoff instead of staying dark until a human restarts the daemon. Parking it in the cold store (#4087) made it reachable only by a query naming its id verbatim — `list_indexes` omits cold entries and boot reconcile walks registered handles only, so PR #4717's never-walked guard could never see it. The cold store now records why an entry is parked, and a recovery pass drains the timeout cohort (never the deliberately deferred one) every `TRUSTY_WARMBOOT_RETRY_SECS` (default 60 s, `0` disables), giving up after 5 attempts into the existing `indexes_failed` state. (#4250)
- `warm_boot_degraded` and `warmboot_summary.indexes_skipped_timeout` now report the live timeout cohort rather than a counter frozen at boot completion, so a daemon that got its indexes back stops reporting degraded and one that did not keeps a signal of its own. (#4250)
- A cold index whose corpus open lost a race (`DatabaseAlreadyOpen`) or ran past
  its deadline is no longer deregistered for the daemon's lifetime. The restore
  ran to completion without registering a handle, so the loader consumed the
  cold entry anyway and the index became a 404 with no way back — one client
  retry against a slow ~20s cold-start open was enough. Registration is now the
  only evidence a restore succeeded; a transient failure keeps the entry and
  returns a retryable 503 so the next query recovers it.
- No test process can resolve the operator's live data directory any more, so the test suite can no longer register fixture roots in `indexes.toml` (closes [#4255](https://github.com/bobmatnyc/trusty-tools/issues/4255))
  - issue #4094 guarded this with `#[cfg(test)]`, which is set per compilation unit: the `tests/` integration targets and the `[[bin]]` unit tests link the library built without it and kept resolving the real location. `default_data_dir` now branches on the runtime check `trusty_common::running_under_test_harness()` instead
  - `tests/registry_isolation.rs` proves it from the non-`cfg(test)` linkage the old guard missed: it performs a real `upsert_index_registry_entry` and asserts the operator's `indexes.toml` is byte-identical afterwards
  - `TRUSTY_DATA_DIR` still wins over both branches, so tests that set it are unchanged
- The shutdown flush can now actually finish. Its per-index budget floors at
  30 s and ceilings at 20 min, while every window that terminates the daemon
  granted 3–5 s: launchd's `ExitTimeOut` default (measured 5 s on macOS, not
  the documented "system-defined" anything), `trusty-search stop`, and the
  orphan reaper. A flush with real work to do was SIGKILLed mid-sweep on every
  path, losing HNSW vectors committed since the last checkpoint. Two changes:
  every generated LaunchAgent plist now declares `ExitTimeOut`, and `stop` and
  the reaper wait the same window. **An already-installed LaunchAgent keeps
  launchd's 5 s default until its plist is regenerated** — re-run the
  installer's service setup to pick it up (#4393)
- A per-index flush deadline can no longer outlive the process that granted it.
  Deadlines are now minted by a `ShutdownBudget` counting down from the instant
  SIGTERM landed, so a sweep that runs out of window stops cleanly at an index
  boundary — logging how many indexes kept their last incremental checkpoint —
  instead of being cut off partway through a write (#4393)
- `trusty-search start` no longer SIGKILLs healthy daemons that merely share
  its executable name. Its orphan reaper matched on process name plus `start`
  in argv, so any lock-visibility asymmetry — a second instance under
  `--data-dir`/`TRUSTY_DATA_DIR`, a daemon restarted without the override it
  started with, a deleted lockfile — turned a routine `start` into the
  destruction of a live production daemon, with 3 s to shut down. The reaper
  now reaps only processes it has positively identified as sharing its own data
  directory, read from the candidate's own `--data-dir` argument or
  `TRUSTY_DATA_DIR`; a process whose argv or environment cannot be read is
  spared and reported, never killed. `trusty-search stop` keeps its explicit
  stop-everything contract (#4395)
- Two daemons indexing the same repository can no longer splice each other's
  HNSW snapshot. Both staged through the same `hnsw.usearch.tmp` before
  renaming, and colocated indexes keep that file in the project root — outside
  every data directory, so even fully data-dir-isolated daemons collided there.
  The staging name is now scoped to the writing process (#4395)
- `list_indexes?details=true`'s `size_bytes` and `GET /indexes/:id/status`'s `disk_bytes` now sum BOTH storage layouts — the legacy global `<data_dir>/indexes/<id>/` and the colocated `<root_path>/.trusty-search/` introduced by #403. They previously measured only the global directory, which for a colocated index holds metadata and no corpus; because that directory exists, the metric returned `0` rather than `null`. A healthy 527 MB / 71,433-chunk index reported `0`, and an operator diagnosed 11 such indexes as broken and considered deleting them. The field keeps its name and its documented contract ("sum of all file sizes under the index data directory") — reading one of the two locations was the bug, not the contract (#4706).
- `null` now means what it says: neither layout exists. It is no longer reachable for an index that holds data (#4706).
- `last_indexed` gained the same repair. It probed only the global directory for `index.redb`/`chunks.json`, so a colocated index reported `null` freshness — the gap `IndexHandle.last_indexed_at` (#878) was added to paper over, and which still read `null` for a warm-booted handle that had not reindexed in this daemon's lifetime. The newer mtime across the two layouts now wins, so a stale global copy cannot shadow a live colocated write (#4706).
- A `<root_path>/.trusty-search/` with no `index.redb` is no longer counted as an index corpus. That directory name is also the daemon's own runtime directory (`$HOME/.trusty-search/` holds `http_addr` and `mcp_http_addr`), so an index rooted at `$HOME` would have reported the daemon's runtime files as its corpus (#4706).
- `POST /indexes/:id/search` no longer walks both storage directories on every query to compute a byte count it discarded — the freshness field it actually uses now takes a `stat`-only path (#4706).
- MCP `search`, `search_lexical`/`search_semantic`/`search_kg`/`search_all`, `grep`, `index_status`, and `list_chunks` against a worktree that has never been indexed now return a retryable `INDEX_NOT_READY` error carrying the state, reason, and a `grep`/`find` fallback, instead of the daemon's permanent-sounding `404 unknown index` (#4715).
- `GET /indexes/:id/status`, `GET /indexes/:id/chunks`, and `POST /indexes/:id/grep` now return `503` rather than `404` for an index that is registered but not resident (cold-parked after a timed-out warm-boot restore, or permanently restore-failed). A `404` from these endpoints now means the same thing it has always meant on the search endpoint: no such index anywhere (#4715).
- `POST /indexes/:id/grep` now reports a permanently restore-failed index as `index_restore_failed` with the operator remedy (`restart the daemon or re-register`), matching the search endpoint, instead of telling the caller to retry after a restore that will never happen (#4715).
- `GET /indexes/:id/status` gained `semantic_coverage`, carrying `vectors_present` read from the live vector store alongside the per-boot `embedded_this_boot` delta. `stages.semantic.embedded` counts only what the current boot's embed pass computed, so it reads `0` on a fully-working index whose HNSW snapshot was already current — indistinguishable from the dead-lane signature of #2178, and enough to get three healthy indexes flagged during an estate audit. `stages.semantic.embedded` keeps its name and value for wire compatibility (#4787).
- `semantic_coverage.vectors_unavailable_reason` distinguishes `no_vector_store` (BM25-only or `skip_vector` — healthy, nothing to count) from `count_unreadable` (a vector store is attached but its count errored — a fault). A single `null` reported the fault as "not applicable", which is this issue's own defect one level down (#4787).
- `GET /health` gained `indexes_populated`, `indexes_empty`, and `total_chunks`. `indexes` and `warmboot_summary.indexes_loaded` count registration slots, so a deployment where 220 of 222 indexes held zero chunks reported `indexes_loaded: 222/222`, `indexes_failed: 0`, `status: ok` while a consuming application returned empty context on 913 consecutive operations across 44 days. Counts come from the durable corpus (the in-memory map reads `0` after idle eviction) and exclude corpus-failed indexes, whose real count is unknown rather than zero (#4839).
- A durable chunk-count READ error no longer falls back to the in-memory map when counting populated indexes. That map reads 0 after idle eviction (#681), so an unreadable corpus on a populated index would have been counted as empty — manufacturing the exact false signal this issue exists to remove. Such an index is now excluded from `indexes_populated`, `indexes_empty`, and `total_chunks` and logged at warn, so `indexes_populated + indexes_empty <= indexes` and the shortfall is visible (#4839).
- `service install` reads operator tunables from the LEGACY unit when the
  canonical plist does not exist yet. On the host this issue describes — one
  whose live agent still carries the old label — the read returned nothing, so
  `TRUSTY_NO_AUTO_DISCOVER`, `TRUSTY_DEVICE` and `TRUSTY_BM25_CORPUS_CAP` were
  silently dropped moments before eviction deleted the plist holding the only
  record. That defeated #4823 precisely on the migration path this change
  introduces (#4868)
- `service install --force` reloads even when the rendered unit is unchanged,
  and `make deploy` uses it. A deploy changes the BINARY behind a byte-identical
  plist, so without it the install reported the unit already current and launchd
  kept running the old image. `deploy` also boots the job out before
  `cargo install` — the unit is `KeepAlive::Always` (#4113), so a SIGTERM'd
  daemon is relaunched into the middle of the install and gets its binary
  swapped underneath it, which is #87 (#4868)
- `service install` no longer destroys environment variables the live unit
  carried. Before this issue, install wrote a differently-named plist and never
  touched the running agent, so anything the template failed to reproduce was
  merely absent from a file nobody read. Once install began overwriting the LIVE
  unit, the same gap became data loss: the plist on the owner's host carried
  `TRUSTY_WARMBOOT_INDEX_TIMEOUT_SECS`, `TRUSTY_EMBEDDERD_CALL_TIMEOUT_SECS`,
  `FASTEMBED_CACHE_DIR`, `FASTEMBED_CACHE_PATH` and `RUST_LOG`, none of which the
  named-tunable allowlist mentioned. The first is the hand-patch from an incident
  where a restart cost a 200k-chunk index to a 30 s redb open timeout under
  warm-boot contention — dropping it re-arms that incident, invisibly, until the
  next restart. Every key the installed unit carried is now carried forward
  unless the template computes it itself (`HF_HOME`, `PATH`), so extending an
  allowlist is no longer what stands between an operator and a lost setting
  (#4868)
- The regenerated unit keeps a `WorkingDirectory` the installed one had. The
  template never emitted the key, so regeneration silently changed the daemon's
  working directory (#4868)
- `make deploy` boots out the LEGACY label as well as the canonical one. On a
  mid-migration host the loaded unit is still `com.trusty.trusty-search` with
  `KeepAlive::Always`, so booting out only the canonical label left that daemon
  running into the binary swap — the #87 hazard the bootout exists to prevent,
  and which the old `unload $(PLIST_LEGACY)` line had covered (#4868)
- `trusty-search service install` now installs the unit launchd actually has
  loaded. `LAUNCHD_LABEL` was `com.trusty.trusty-search`; the live agent is
  `com.trusty.search`. So install wrote a plist under the wrong name,
  bootstrapped a SECOND daemon contending for :7878 and the index locks, booted
  out nothing, and left #4393's `ExitTimeOut` fix in a file launchd never reads
  — the corrected plist was written but never activated. The label now comes
  from `trusty_common::launchd_labels::SEARCH`, and install evicts the labels
  earlier installers registered (`com.trusty.trusty-search`,
  `com.bobmatnyc.trusty-search`) so #2938's stranded duplicate is cleaned up
  rather than resurrected (#4868)
- Re-running `service install` with no configuration change no longer restarts
  the daemon: the unit is reloaded only when the rendered plist differs from
  what is installed or the label is not loaded. A failed activation restores and
  re-bootstraps the previous plist instead of leaving search down (#4868)
- The log-rotation agent's label is derived from the daemon's rather than
  restated, and install evicts the orphaned
  `com.trusty.trusty-search.logrotate` a prior version left loaded (#4868)
- `make deploy` no longer declares `com.bobmatnyc.trusty-search` canonical — a
  third label family that has never existed on a host. Every deploy therefore
  unloaded a missing file, killed the daemon, failed to load it back, and fell
  through to `trusty-search start`, leaving it unsupervised and CLI-detached for
  the whole `cargo install`. The target now defers to `service install` (#4868)
- An unparseable `indexes.toml` is now an error rather than an empty registry.
  It previously read back as "no index was ever registered", and the next write
  published that view — overwriting a whole registry with a single entry, which
  is the mass-deregistration both #4317 (73 → 31, then 42 → 5) and #4871
  recorded. The corrupt file is left intact for recovery.
- Registry mutations are serialized process-wide and stage through a
  per-write temporary file instead of one shared `indexes.toml.tmp`. Concurrent
  writers previously interleaved: a registration landing between another task's
  load and its save was silently discarded (the observed 80 → 88 → 80 revert),
  and two writers racing on the shared temp file could publish a spliced,
  unparseable registry.
- The boot orphan-reaper removes orphans by id instead of republishing a
  pre-boot snapshot of the survivors, so an index registered while the sweep
  was deciding is no longer erased by the cleanup.

Known limitation: the fail-closed parse guarantees the WRITE path only. Every
read-only caller still swallows the load error and treats a corrupt registry as
empty — `reindex/runner.rs`, `warm_boot/mod.rs`, `server/tickers.rs`,
`reconcile.rs`, `server/indexes.rs`, `server/indexes_relocate.rs`,
`server/index_config.rs`, `persistence_timestamps.rs`, and
`commands/start/restore.rs`. That is unchanged behaviour, not a regression, but
the guarantee does not extend to them. See #4871.
- A crafted spreadsheet can no longer force unbounded decompression: xlsx/xlsm packages are now capped at 256 MiB of total uncompressed content and 16384 entries before calamine opens them (closes [#4894](https://github.com/bobmatnyc/trusty-tools/issues/4894))
  - a 511 KB adversarial workbook used to extract *successfully* in 143 ms while peaking at 529.8 MiB RSS. `MAX_OFFICE_FILE_BYTES` (10 MiB) bounds the container rather than the decompressed payload, and `EXTRACT_TIMEOUT` (30 s) is a time bound the attack never approaches — so neither existing mitigation applied
  - calamine exposes no size limit and the zip layer bounds an entry read by its *compressed* length, so the check runs outside calamine: declared sizes are summed from the central directory first (rejecting a declared bomb with zero decompression), then every entry is drained through `Read::take` into a sink so a lying size field is caught too. The guard itself allocates O(1) regardless of the cap
  - the document is read into memory once and both the guard and calamine parse those same bytes. Validating a path and then reopening it let an attacker with write access to a watched directory swap a benign workbook for a bomb between the two opens, with the watcher supplying unlimited retries
  - an entry-count cap covers what the byte caps cannot: a package of empty entries declares and decompresses to nothing, yet 200 000 of them in a 16.6 MiB container cost 142.8 MiB of zip metadata before either byte pass ran
  - measured on the same fixture: peak RSS 529.8 MiB before, 11.7 MiB after
  - the cap is package-wide rather than per-part like the docx path because calamine reads the whole package, and because a dense workbook at the container cap decompresses to ~190 MiB almost entirely within one sheet entry — a 50 MiB per-part cap would reject legitimate files
  - `.xls` is unaffected: CFB has no stream compression, so the container cap already bounds it
  - one visible consequence: a file that is not a workbook in any format is now logged as `spreadsheet extraction failed: Cannot detect file format` rather than as the xlsx reader's zip failure, because format detection now sniffs content instead of trusting the extension
  - the guard now fails CLOSED on a zip it cannot open. It reads with `zip` 2.x while calamine parses the same bytes with its own `zip` 8.x, so "our reader gave up" was never evidence that calamine would; treating it as nothing-to-bound let a package crafted to trip the older reader skip every cap and reach calamine unbounded. A container starting `PK` that fails to open is now refused; `.xls` (CFB) and non-workbook input still pass through so calamine keeps owning that diagnosis
- `POST /indexes/:id/reindex` with a `root_path` override no longer makes every
  search return nothing. The override rebuilt the index handle around the same
  indexer, leaving the indexer's own `root_path` on the old value — so the
  absolute `file` on each result was built against the old root while the search
  post-filter (#64/#541) checked it against the new one, dropping 100% of
  matches. Callers saw `results: []` with `stale_index_root: true` on an index
  whose `/status` read `ready` with a full chunk count.
- `GET /indexes/:id/status` and `GET /indexes/:id/chunks` now return a JSON body with their `503`/`404` instead of a bare status code. An MCP caller previously saw `returned 503 Service Unavailable: ` with empty text and could neither tell a cold-parked index from a permanently restore-failed one nor learn how to clear it (#5061).
- The cold-parked `503` from `status`, `chunks`, and `grep` now carries `restore_via: "POST /indexes/{id}/search"`, naming the one endpoint that reloads the index. These three cannot reload it themselves, so without the hint a caller polled a `503` that nothing else would clear while a plain `search` on the same id self-healed (#5061).
- `status`, `chunks`, and `grep` now render the not-resident / restore-failed / unknown verdict through one shared builder, so the three can no longer report the same daemon state three different ways (#5061).
- `POST /indexes/:id/index-file` and `POST /indexes/:id/remove-file` join that contract. Both did a bare hot-registry lookup and returned a bodyless `404` for a cold-parked index, which under the #4715 rule asserts the index exists nowhere. These two are the supported incremental-indexing path for network-mounted roots (#3408), where the OS watcher cannot fire and the caller is the only thing keeping the index current — so a caller told "unknown index" stops pushing and the index silently stops being updated. They now return `503 index_not_resident` with the `restore_via` hint (#5061).
- A failed `index-file` / `remove-file` now returns a JSON body naming the failure and the path instead of a bodyless `500`, so a caller can tell which push did not land (#5061).
- `POST /indexes/:id/search` now renders its residency verdicts through the same builder as every other index-scoped endpoint. It previously hand-rolled them, omitting `retryable` and `index_id` — and it is the endpoint the other bodies' `restore_via` hint points at, so a caller that followed the hint into a restore failure met the one body that did not carry the field it was told to branch on. `index_loading` and `embedder_initializing` gained the same two fields (#5061).
- `POST /indexes/:id/search` with `stage: "semantic"` against an index whose vector lane is unavailable now returns `503 vector_unavailable` instead of `200` carrying BM25 rows. `reason` separates `skipped_by_config` (permanent — the index was built with the vector component disabled, which #5060 made the default for worktree indexes) from `stage_not_ready` (transient — the embed pass has not finished), and `retryable` carries the same split as a boolean. This mirrors the `503 kg_unavailable` contract `get_call_chain` already uses for `skip_kg` indexes (#5068).
- The search response `meta` block gained `vector_unavailable` and `vector_disabled_by_config`, the counterparts to the existing `bm25_lane_degraded` flag. An unpinned hybrid query still succeeds and still degrades to the ready lanes, but the caller can now see that no vector lane contributed without diffing `search_capabilities` (#5068).
- `DELETE /indexes/:id` now clears the index's cold-store records. Deleting a
  cold-parked or restore-failed index left them behind, so `/status`,
  `/chunks`, and `grep` answered 503 forever for an id that no longer existed —
  inverting #5057's rule that 404 means "absent from every store". A
  cold-parked-only index is also removed from `indexes.toml` now, instead of
  being resurrected by the next warm boot.
- The two #4846 warm-boot budget regression tests now assert the NUMBER of tracked-root relocation walks a boot performs instead of how many milliseconds it took, ending a false CI red that fired at ~6% and reproduced crate-scoped at 1-in-20. Widening the ceiling was not available as a fix: a contended post-fix boot measured 416–435 ms (once 833 ms) while the pre-fix cost is 24 walks ≈ 340–400 ms, so any floor that cleared the false reds also cleared the regression the tests exist to catch. A count has no such overlap — post-fix a boot walks the tracked roots once, or not at all with salvage disabled, against 24 times before the fix. Both tests also print `boot`, `one_walk`, and the retired ceiling on a PASSING run (`cargo test -- --nocapture`), so cost erosion in the boot path is visible before it becomes anyone's red gate. (#5084)
- Auto-discovery excludes the configured worktree base, not just a hardcoded `.worktrees`, so session worktrees under a retargeted base are no longer indexed as duplicate content (#5204).
- The MCP `INDEX_NOT_READY` payload gained `next_steps.discover`, pointing at `list_indexes`, and `fallback_scope`, which names the circular-advice trap explicitly. `suggested_fallback: ["grep", "find"]` was previously the only actionable field; an agent read "grep" as trusty-search's own index-backed `grep` tool, which reports the same failure under the same session pin (#5213).
- An unresolvable `index_id` on `search`, the per-lane search tools, and every index-management tool now errors with a message naming `list_indexes` rather than the bare "missing required string field: index_id" (#5213).
- `search_all`'s tool description no longer contradicts its `index_id` parameter description. Both now state the actual three-tier precedence — explicit id, then the session pin (#1373), then cross-project fan-out only in an unpinned session — and the stale "issue #10" reference is gone (#5213).

### Changed

- The write-quarantine module docs no longer list genuine corruption among the
  conditions that trigger it. Corruption is absorbed before `corpus_open_failed`
  can be set: `open_corpus_db_or_recreate` classifies it as recoverable, moves
  the file aside, and returns a fresh empty corpus. Only lock contention and
  transient I/O reach the quarantine, so its population is transient-dominated —
  which the docs now say, along with the recreate-to-empty gap it does not cover.
- The signed-install script prints `trusty-search service install` as the
  restart step instead of a hand-run `launchctl bootout`/`bootstrap` pair
  against a plist path it guessed. Its path resolver had picked
  `com.trusty.trusty-search.plist` as canonical and labelled the live
  `com.trusty.search` a drifted alias — the reverse of the truth (#4868)
- **The embedder UDS socket path moves into a per-uid directory**
  ([#5099](https://github.com/bobmatnyc/trusty-tools/issues/5099)).
  `embedder_supervisor::default_socket_path` resolved to `$TMPDIR`, falling back
  to `/tmp` on headless Linux — world-writable, and unable to be narrowed to
  `0700`. It now resolves under `trusty_common::uds::scratch_socket_dir()`
  (`<$TMPDIR or /tmp>/trusty-<uid>/`), keeping the PID suffix that separates
  concurrent daemons. Affects the `TRUSTY_EMBEDDER=unix:/path` transport only;
  the default auto-spawn path uses stdio and is unchanged.
- The MCP tool section of `README.md` and `CLAUDE.md` is now generated from
  `tool_descriptors()` by `tests/generated_docs.rs`, from one render call that
  feeds both files, so the roster and count can no longer drift or disagree
  between them. The table gains an `Arguments` column derived from each tool's
  JSON Schema. Regenerate with
  `UPDATE_DOCS=1 cargo test -p trusty-search --test generated_docs` (#5205)

## [0.42.3] — 2026-08-05

### Fixed

- **Warm boot paid a full tracked-root relocation walk per dead registry
  entry, starving live indexes for the better part of an hour** (issues
  [#4250](https://github.com/bobmatnyc/trusty-tools/issues/4250),
  [#4846](https://github.com/bobmatnyc/trusty-tools/issues/4846)). Measured on
  the reporting machine's own registry (248 tracked roots, 55 dead entries):
  9.5-10.5s per dead entry, recomputed from scratch even though the walk's
  result is identical for every entry in a boot. Entries are now triaged with
  one stat into separate live/dead vectors; the dead cohort runs after both
  live phases under one global salvage budget, and the tracked-root walk runs
  once for the whole cohort instead of once per entry. A timeout-skipped index
  is no longer permanently parked in the cold store with nothing able to see
  it again — a recovery pass now retries the timeout cohort on a backoff (5
  attempts) while leaving deliberately deferred entries alone. No
  registration is removed and no index data is deleted by a failed or skipped
  probe (#4883).

- **The shutdown flush's own window was unreachable — every real termination
  deadline in the system was shorter than the flush's 30s floor** (issues
  [#4393](https://github.com/bobmatnyc/trusty-tools/issues/4393),
  [#4395](https://github.com/bobmatnyc/trusty-tools/issues/4395)). launchd's
  `ExitTimeOut` default (5s), `trusty-search stop` (5s), and the orphan
  reaper's SIGKILL window (3s) all fired before a flush with real work could
  finish. The plist now emits an explicit `ExitTimeOut` of 60s from a shared
  constant, and a per-index flush deadline can only be minted from the actual
  SIGTERM-to-now budget, so it is structurally incapable of outliving the
  process. Separately, the startup orphan reaper identified victims by
  process name plus `start` in argv — any lock-visibility asymmetry could
  make it SIGKILL a healthy production daemon; it now identifies a candidate
  by the data directory the process itself declares, and a candidate whose
  argv/environment cannot be read is spared rather than assumed guilty
  (#4868).

- **A write-quarantined index (the #4122 corpus-open-failed guard) still
  performed a destructive write on shutdown**
  ([#4226](https://github.com/bobmatnyc/trusty-tools/issues/4226)). The
  quarantine gated the incremental-ingest family only; the snapshot-writer
  family (`save_chunks_to_disk`, `flush_corpus_to_disk`, `save_vector_store`,
  `spawn_incremental_persist`) shared the same `corpus.is_none()` enabling
  condition quarantine guarantees, so a shutdown flush wrote the
  deliberately-empty in-memory corpus over the legacy `chunks.json` recovery
  snapshot — while the refusal diagnostic told the operator the on-disk
  corpus was untouched. All four snapshot writers are now gated on
  `corpus_open_failed` directly, closing the same hole for the HNSW graph
  file (#4866).

- **The test suite could reach and pollute the operator's live index
  registry** ([#4255](https://github.com/bobmatnyc/trusty-tools/issues/4255)).
  `cfg(test)` is set per compilation unit and does not cover a crate's
  `tests/` integration targets or `[[bin]]` unit tests, so
  `persistence::default_data_dir` kept resolving the operator's real data
  directory for them; separately, `trusty_common::search_index`'s mutating
  entry points POST to whatever daemon is discoverable, which under `cargo
  test` is the operator's own, running in a different process no
  compile-time guard could reach. This is how the live registry accumulated
  five stray `.tmpXXXXXX` roots and, over time, reached 396 entries. Both
  paths now gate on a single runtime check that reads the running
  executable's own path (cargo test binaries always live under a `deps/`
  directory); `TRUSTY_ALLOW_PRODUCTION_STATE=1` is the explicit, narrowly
  scoped opt-in for the one test that deliberately drives a real daemon
  (#4864).

## [0.42.2] — 2026-08-04

### Fixed

- The daemon no longer aborts from its disk-size metrics ticker. The shared
  `trusty_common::sys_metrics::dir_size_bytes` walk could raise a non-unwinding
  panic out of a directory-handle destructor, killing the process with no
  graceful shutdown — 40 self-aborts (`SIGABRT`) in one week, roughly every
  7 minutes under load, each relaunching via launchd `KeepAlive` into the same
  full auto-discover sweep that recreated the condition. The walk is now
  panic-safe; see the `trusty-common` entry for the mechanism (#4764)
- `trusty-search` now installs a panic hook at startup that logs the panic
  payload, location, thread, and backtrace through `tracing` before the default
  hook runs. macOS `.ips` crash reports do not carry the panic message, so
  daemon aborts previously reached the operator with the one datum that names
  the cause missing (#4764)
- **Issue #4823 — `service install` no longer discards a deliberate
  `--no-auto-discover`.** `trusty-search service install` regenerated the
  launchd unit from a fixed template, so an operator who disabled the
  auto-discovery scan lost that setting on the next install and could not make
  it durable by any supported means. Three changes:
  - `service install` accepts `--no-auto-discover`, which writes the flag into
    the generated unit's `ProgramArguments`. Re-running `service install`
    **preserves** the setting and says so; re-enabling the scan now requires an
    explicit `service install --auto-discover`, so a capability change is never
    a silent side effect of reinstalling.
  - Operator tunables (`TRUSTY_DEVICE`, `TRUSTY_MEMORY_LIMIT_MB`, and the rest
    of `PERSISTED_ENV_VARS`) that the installed unit already carried now survive
    regeneration instead of being blanked whenever `service install` runs from a
    shell that exports none of them. An exported value still wins.
  - `--no-auto-discover` / `TRUSTY_NO_AUTO_DISCOVER` accepts `1`/`true`/`yes`/
    `on` and `0`/`false`/`no`/`off` (case-insensitive). Previously the env var
    went through clap's strict `FromStr<bool>`, so the `=1` spelling documented
    in the README and the #314 changelog — and already present in many
    `daemon.env` files — was **rejected** and aborted daemon startup:

        error: invalid value '1' for '--no-auto-discover': [possible values: true, false]

    An unrecognised spelling is still an error rather than a silent `false`, so
    a typo fails loudly instead of quietly re-enabling the scan. The suppression
    itself travels as a CLI flag, never as a `TRUSTY_NO_AUTO_DISCOVER` entry in
    the generated plist, so a generated unit can never carry a value the daemon
    would refuse to parse.

### Changed

- The `disk_bytes` health metric is recomputed every 60 s instead of every 10 s.
  Walking a multi-GB, actively-mutating data directory six times less often
  cuts exposure to the reindex/prune race behind #4764 by the same factor, at
  no user-visible cost for an at-a-glance footprint figure (#4764)

## [0.42.1] — 2026-08-04

### Fixed

- `/health` now reports `status: "degraded"` when the embedder permanently
  failed to reach its configured backend — `embedder_bootstrap: "failed"` (the
  graceful Python/MPS bootstrap gave up for this daemon's lifetime) or
  `"fell_back_to_ort"` (the swap-back watchdog abandoned a dead sidecar). Both
  previously sat next to `status: "ok"` forever, so a silent MPS → CPU
  performance regression was invisible to every monitor. `embedder: "ready"` is
  unchanged and still describes the currently-active backend (#4125)
- The graceful Python bootstrap's readiness probe now gets a larger budget on
  each retry instead of the same flat `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS`
  twice. A cold torch import + model load, racing the daemon's own warm-boot,
  could exceed the flat 30 s on both attempts and permanently abandon a healthy
  sidecar (#4125)
- Reindex resume-from-checkpoint (#3979, shipped in 0.42.0): four review findings on the live reindex data path.
  - The checkpoint's config fingerprint concatenated fields without unambiguous framing, so distinct walk configurations could hash identically (`exclude_globs = ["a", "b"]` collided with `["a,b"]`) and a checkpoint could be adopted for a configuration it was not built under. Fields and list elements are now length-prefixed, and paths are hashed as raw `OsStr` bytes rather than a lossy UTF-8 rendering.
  - `CHECKPOINT_SCHEMA_VERSION` bumped 1 → 2 so every checkpoint written by 0.42.0 invalidates on the version gate and falls back to a full reindex — never misread as valid under the new fingerprint.
  - The staging corpus was opened twice (once to validate the checkpoint, once to adopt it). redb locks the file exclusively, so the second open could fail and silently abandon the resume; the probe now hands its open handle to the adoption, so the file is opened exactly once.
  - "A promoted corpus never carries an in-progress checkpoint" was upheld only by the order two statements happened to appear in. Clearing the record and releasing the staging handle are now one operation that yields the token the promotion rename requires, with a fallback clear on the released file when the in-store clear cannot be confirmed.
- The graceful Python bootstrap's per-retry readiness-probe budget is now
  capped at 3x the configured `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` (90 s at
  the default). It previously scaled by attempt number with no ceiling, so a
  raised `TRUSTY_PY_BOOTSTRAP_RETRIES` grew a single probe's budget without
  limit — attempt 100 would have held one live Python child on one probe for 50
  minutes — and a pathologically large base could panic on `Duration` overflow.
  A capped probe now says so in its timeout log line, so it is distinguishable
  from an uncapped one (#4125)

## [0.42.0] — 2026-08-04

### Added

- An interrupted reindex now resumes from where it stopped instead of redoing
  the entire walk/parse/embed. Every non-`force` reindex stamps a versioned
  checkpoint record into the staging corpus it is building; after a crash the
  next reindex validates that record and adopts the staged corpus, so the
  batches already committed — and the hours of embedding they represent on a
  large index — survive
  ([#3979](https://github.com/bobmatnyc/trusty-tools/issues/3979)).
  - The resume unit is the file, and the skip stays content-verified: an adopted
    file is still re-read and re-hashed against its current bytes before it can
    be skipped, so a file edited, deleted, or created during the interruption is
    handled exactly as on a clean run.
  - Every ambiguous case falls back to a full reindex, never a partial one — an
    unopenable staging corpus, a missing or corrupt record, a schema-version
    bump, a different index id, a moved walk root, a daemon upgrade, a changed
    walk config, or a record past its adoption window. The live corpus is never
    read, written, or deleted by the resume path.
  - New optional knobs: `TRUSTY_REINDEX_RESUME` (set to `0` to restore the
    previous always-rebuild behaviour) and
    `TRUSTY_REINDEX_CHECKPOINT_MAX_AGE_SECS` (default `86400`; `0` disables the
    age gate).
  - `--force` reindexes deliberately neither write nor consume a checkpoint, and
    a resumed run always runs the vector catch-up pass so semantic search is not
    short the chunks it inherited.

### Changed

- **BREAKING (HTTP API): `DELETE /indexes/:id` no longer destroys on-disk data
  by default** ([#4123](https://github.com/bobmatnyc/trusty-tools/issues/4123)).
  The handler hardcoded `delete_data=true`, so every `DELETE` destroyed the
  index's data directory and the HTTP surface offered NO way to merely
  deregister. Registry hygiene was therefore impossible through the API: an
  operator clearing 49 stale entries had to stop the daemon and hand-edit
  `indexes.toml`, because one mis-typed id would have destroyed a real corpus.
  A bare `DELETE` now deregisters only (the same safe path the orphan-reaper
  has always used); destroying data requires an explicit
  `?delete_data=true`. The response gained a `data_deleted` field so callers
  can confirm which semantics ran. An unparseable `delete_data` value is
  rejected with `400` rather than guessed at.

  This also makes three long-standing pieces of documentation true rather than
  false — all of them already promised the preserving behaviour that did not
  exist: the API reference (`crates/trusty-search/CLAUDE.md`, "On-disk redb data
  is preserved"), `trusty-search index remove`'s `--help` ("The on-disk redb /
  HNSW snapshot is preserved — re-registering with the same path reuses it"),
  and the UI's delete confirmation ("On-disk data is preserved.").

  **Action required for callers that relied on `DELETE` reclaiming disk** —
  they will silently stop reclaiming it and leave orphaned data behind. The
  in-tree ones are updated in this change to pass `?delete_data=true`: the
  `delete_index` MCP tool (its descriptor promises "and all its data"),
  `trusty-search cleanup`, trusty-mpm's decommission + orphan-sweep index GC,
  and the benchmark harnesses that require a clean slate. `trusty-search index
  remove` and the UI are deliberately left on the new preserving default,
  because that is what they already told users they did.
- Credential resolution now imports from `trusty_common::credentials` instead of
  `trusty_common::inference::credentials`, which was deprecated in the same
  change (see [#4564](https://github.com/bobmatnyc/trusty-tools/issues/4564)).
  Import-path churn only — no behaviour, precedence, or credential surface
  changes in this crate.

- **BREAKING:** `service::lazy_loader::store`'s public `register_cold_entries` changed its return type from `()` to `Vec<Arc<()>>` (it now hands back the per-entry residency tokens that `mark_loaded_if` needs to detect a reindex that raced a cold restore).
- `trusty-common` requirement raised to `^0.27` (was `^0.26`): 0.27.0 makes
  `ChatEvent` `#[non_exhaustive]`, which a `^0.26` requirement cannot express.
  `service::ui`'s `ChatEvent` match gained a wildcard arm accordingly.

### Fixed

- The generated LaunchAgent plist now sets `KeepAlive` to `true` instead of
  `{ SuccessfulExit: false }`, so launchd restarts the daemon after a **clean**
  (exit 0) shutdown as well as a crash. Previously a plain SIGTERM or orderly
  drain left trusty-search down indefinitely with no automatic recovery and no
  alarm, silently degrading every search-backed consumer
  ([#4113](https://github.com/bobmatnyc/trusty-tools/issues/4113)).
  - Deliberate "stop it and leave it stopped" is now expressed through launchd's
    unload path — `launchctl bootout gui/$(id -u)/com.trusty.trusty-search` or
    `trusty-search service uninstall` — which removes the job and therefore
    outranks any `KeepAlive` setting.
  - `trusty-search stop` now says so: on a host where the LaunchAgent is loaded
    it prints that launchd will restart the daemon shortly and names the
    `bootout` command that keeps it stopped.
  - An already-installed plist keeps the old policy until it is regenerated —
    re-run `trusty-search service install` to pick up the change.
- Test integrity: four test-only defects that let the suite report green
  without proving what it claims.
  - The `#2178` P0 root-hijack data-loss guard
    (`reindex_refuses_untrusted_root_move_and_preserves_corpus`) no longer
    isolates `indexes.toml` with a process-global
    `std::env::set_var("TRUSTY_DATA_DIR", …)` behind `#[serial]`. `#[serial]`
    excludes only other serial tests, so a non-serial sibling could still
    clobber the variable mid-test; `load_index_registry` then found no
    persisted entry, the gate trusted the hijacked root, and the assertion
    flipped `Failed` → `Complete`. Both tests in that file now run alone in a
    child process whose data dir is supplied at spawn time — deterministic
    isolation with the assertions unchanged
    ([#4213](https://github.com/bobmatnyc/trusty-tools/issues/4213)).
  - Unit tests no longer register index entries in the developer's real
    daemon registry. `data_dir()`'s un-overridden fallback resolves to an
    isolated per-process directory in test builds, so a test that forgets to
    set `TRUSTY_DATA_DIR` can no longer write throwaway fixtures pointing at
    `~/.trusty-search-test-roots/…` into the live `indexes.toml` — the debris
    that kept `search_health` reporting `degraded`
    ([#4094](https://github.com/bobmatnyc/trusty-tools/issues/4094)).
  - `test_trim_heap_never_increases_rss_after_bulk_free` no longer asserts an
    invariant that concurrent test execution can break. It sampled
    whole-process RSS either side of `trim_heap()`, so any sibling test
    allocating in that window failed the bound with `malloc_trim` behaving
    perfectly (observed 181 MB → 194 MB on an unrelated PR). Following the
    `#3705` precedent, the bound is now a sanity band plus a calibrated
    concurrency-noise budget, and the raw before/after MB numbers are always
    printed ([#3954](https://github.com/bobmatnyc/trusty-tools/issues/3954)).
  - The `#2847` legacy-path regression tests now force their failure at
    `index_data_dir()`'s own `create_dir_all` (`ENOTDIR`) rather than one
    layer above at `data_dir()`'s, matching the precision of their colocated
    counterpart, which blocks the immediate dispatch target
    ([#3963](https://github.com/bobmatnyc/trusty-tools/issues/3963)).
- **An index whose durable corpus failed to open kept accepting watcher
  writes, permanently destroying the corpus (issue #4122, P0 data loss).**
  When `CorpusStore::open` failed at load time the loader set
  `corpus_open_failed` but left the handle fully live, watcher included, so
  ordinary unrelated file saves rebuilt a fresh PARTIAL corpus over the
  never-opened original — in production `chunk_count` climbed `0 → 68 → 1334`
  and the index came back "healthy" on the next restart holding the wrong
  content. A `corpus_open_failed` index is now **write-quarantined**: every
  incremental write path (`service::watch_loop`, `service::reconcile`, and
  `POST /indexes/{id}/index-file`) is refused at the shared
  `CodeIndexer::index_file` choke point, and the watcher additionally bails
  before it reads and chunks the saved file. Refusals are emitted at ERROR
  (the only level `trusty_common::error_capture` persists to `errors.jsonl` /
  `list_recent_errors` / `tm doctor`), throttled to the 1st and every 100th,
  and counted on `CodeIndexer::refused_incremental_writes`. **Recovery is a
  daemon restart** — only a successful `CorpusStore::open` lifts the
  quarantine and it is attempted solely at load time, so the ERROR text says
  so explicitly and warns that a reindex neither clears the state nor
  persists anything (with no corpus wired it skips staging entirely). The
  clean-restart path (the incident's `cto-duetto`, restored at its full
  200,090 chunks) is unaffected. Bulk reindex is deliberately not gated,
  which is safe because a quarantined index holds no `CorpusStore` — an
  invariant now documented and pinned by `debug_assert!`s, since boot
  reconcile auto-fires reindexes with no quarantine check. Reads are
  unchanged; corpus-failed indexes still return empty results (that is issue
  #4087, not fixed here).

- **`POST /indexes` reported every stage `Pending` even when a colocated corpus
  restore succeeded** ([#4110](https://github.com/bobmatnyc/trusty-tools/issues/4110)).
  `create_index_handler` doubles as the "adopt an existing colocated corpus"
  door — `build_indexer_from_entry` synchronously restores the redb corpus, the
  HNSW snapshot and the symbol graph — but the handler then set
  `lexical: pending(), semantic: pending()` unconditionally and threw that
  outcome away. Since `search_capabilities` is derived from `stages`, a fully
  intact index came up advertising no vector lane: semantic search hard-errored
  with "requires Stage 2 (embeddings), which is not yet ready" and `search_all`
  silently degraded to BM25-only, every hit reporting `match_reason="bm25"` —
  indistinguishable from a genuinely dead vector lane. Only a daemon restart
  cleared it, because the warm-boot path already classified correctly from the
  same signals. The registration path now derives stages with
  `derive_warm_boot_stages`, the same pure classifier warm-boot and
  lazy-restore use, over the same signals read the same way, so the two can no
  longer disagree about an identical on-disk state. A genuinely new index still
  reports `created` (lexical `Pending`), not warm-boot's `walking`.

---
- **An index that was never populated was recorded as needing nothing, forever
  — 221 of 222 production indexes served zero results for 44 days while
  `/health` reported fully healthy (issue
  [#4680](https://github.com/bobmatnyc/trusty-tools/issues/4680)).** Boot
  reconcile only ever asked "has the source drifted?", never "does this index
  hold any data at all?". Both of its staleness markers answer "no drift" for
  an empty index: the git path compares a HEAD SHA that restore re-derives from
  live git (so `stored == current` unconditionally — issue
  [#4391](https://github.com/bobmatnyc/trusty-tools/issues/4391)) and counted
  the index `up_to_date`; the mtime path reads `last_indexed_unix`, whose
  writer has no production callers, got `None`, and counted the index
  `skipped_no_data`. Neither branch ever re-drove the walk, on that boot or any
  later one.
  - `reconcile_one_index` now checks, *before* consulting either marker,
    whether an index claims lexical work is underway (`lexical: in_progress`,
    which the warm-boot classifier stamps for every restored empty corpus)
    while no walk has ever been driven for it in this daemon's lifetime. Such
    an index is stuck, not current, and gets a full **non-force** background
    reindex — the incremental hash cache and staged-corpus carryover both
    apply, so recovery re-walks and re-adds and never clears or rebuilds a
    corpus from scratch.
  - The retry is bounded to at most one walk per index per daemon lifetime (it
    keys off `last_walk_started_at`), so a walk that legitimately finds zero
    indexable files — everything gitignored or filtered out — is never
    re-driven in a loop. "Walk found nothing" and "walk never completed" are
    now distinguishable rather than both presenting as `chunk_count: 0`.
  - New `boot_reconcile.stuck_retried` counter on `GET /health` reports how
    many indexes were recovered this way, instead of the recovery hiding inside
    `up_to_date` / `skipped_no_data`.
- **`GET /health` reported `status: "ok"` while indexes were stuck at zero
  chunks (issue [#4680](https://github.com/bobmatnyc/trusty-tools/issues/4680)).**
  Every existing signal was structurally blind to this: `indexes` and
  `warmboot_summary.indexes_loaded` count registered index *slots*, not
  populated ones, and `indexes_corpus_failed` keys off `stages.any_failed()` —
  which a stuck index never trips, because it reports no failed lane at all,
  only an indefinite, false `"walking"`. A new `indexes_stuck_empty` field
  counts registered indexes whose lexical stage claims a walk is underway that
  has never been driven, and a non-zero count forces the top-level `status` to
  `"degraded"` so existing `status != "ok"` monitors catch it. The count is
  derived from the same predicate boot reconcile uses to decide what to
  recover, so the reported number and the recovery can never disagree.
- **Two more index-registration paths had no `root_path` collision guard —
  fourth occurrence of the #2305/#2336 `DatabaseAlreadyOpen` class (issue
  #3993).** An audit prompted by #3929 found `find_root_path_collision`
  (issue #2336) unreached from two call sites that can reproduce the same
  hazard: two index ids claiming one physical `<root>/.trusty-search/index.redb`
  corpus. **Gap E:** `POST /indexes/:id/reindex` with a `root_path` override
  (`reindex_handlers.rs`) registered the new root with no collision check at
  all — now rejected with `409 Conflict` naming the existing owner, same as
  `create_index`/`relocate_index`. **Gap F:** `find_root_path_collision` scanned
  only LIVE handles, so a cold (unloaded) index entry parked in
  `state.cold_store` was invisible to it; a colliding cold entry's later
  on-demand restore (`restore_index_on_demand`, `lazy_restore.rs`) opened the
  same redb with no guard at all, a third source of the hazard.

  **Adversarial re-review (first round BLOCK) found the Gap F fix incomplete:**
  checking only live handles from `restore_index_on_demand` closes the crash
  from the cold entry's OWN restore, but the write side
  (`create_index_handler`, `relocate_index_handler`, and Gap E's reindex
  override) still never consulted `state.cold_store` — so a brand-new
  registration could silently claim a pre-existing cold entry's root_path
  with **no race required at all**, and the resulting live collision would
  later mark the *pre-existing, legitimate* cold entry failed instead of
  rejecting the interloper — inverting first-claimant-wins. Fixed for real
  this round: `find_root_path_collision` now takes both `handles` (live) and
  `cold_entries` (`state.cold_store.snapshot()`), and all three write-side
  call sites pass both — still one shared primitive, no fourth (or fifth)
  collision mechanism. `restore_index_on_demand` also gained a
  `corpus_open_failed` ground-truth backstop mirroring
  `create_index_handler`/`relocate_index_handler`, closing the residual
  genuine race (two different cold entries sharing one root_path only through
  pre-existing on-disk corruption, restored concurrently) that the best-effort
  guard alone cannot. A colliding cold entry — live or cold on the losing
  side — is marked permanently failed (existing #1106 semantics) instead of
  silently registered broken. Not gated on #1681 or #2611 (neither addresses
  collision safety).

  **Adversarial re-review (third round WARN) found the round-2 fix itself
  introduced a HIGH-severity availability regression:** `create_index_handler`
  never cleared a stale `state.cold_store` record for the id being
  (re)created. Repro: park cold `foo` → `root_old`; `create_index(foo,
  root_new)` succeeds and `foo` goes live at `root_new`, but the cold store
  still claims `root_old` for `foo` forever — nothing ever triggers cleanup,
  since `foo` now always resolves via the live registry path. A later,
  wholly unrelated, legitimate `create_index(bar, root_old)` was then falsely
  rejected with `409` even though nothing live or cold genuinely depended on
  `root_old` any more. Fixed by reaping any cold-store record for the exact
  id just (re)registered — `ColdIndexStore::mark_loaded`, keyed strictly by
  `IndexId`, so it can only ever clear the record for that one id, never a
  record that merely happens to share a root_path with someone else (the
  existing collision guard still protects every other id's legitimate
  claim). `relocate_index_handler` and the reindex `root_path` override gained
  the identical reap call for consistency and to self-heal any pre-existing
  residue, though neither can itself *create* the hole — both require the id
  to already be live to reach the write, so under correct operation a stale
  cold record for that same id cannot coexist with it. The reindex override's
  narrower, pre-existing TOCTOU window (two concurrent overrides racing onto
  one still-unclaimed root, the same accepted-race shape as #2519) is left as
  a follow-up rather than fixed here — reindex has no synchronous fresh-corpus
  open to hang a `corpus_open_failed` ground-truth backstop on the way
  create/relocate do, so closing it properly needs a registration-wide
  mutex/lock, a materially larger change than this collision-guard fix.

  **Adversarial re-review (fourth round WARN) found the round-3
  `ColdIndexStore::mark_loaded` reap was itself an uncoordinated SECOND
  writer of `cold_store.entries`, racing the opt-in
  `TRUSTY_MAX_RESIDENT_INDEXES` residency-sweep's `cold_park_index`
  (`lazy_loader::residency`) — both mutate the same map with no `.await`
  between their two `DashMap` ops, so a relocate (or reindex-override) racing
  a residency-park of the SAME id could leave it in NEITHER the live
  registry NOR the cold store (unreachable until an operator manually
  re-registers it).** Fixed by having `cold_park_index` snapshot the handle
  it intends to park (`registry.get(id)`) *before* inserting the cold entry,
  then comparing that snapshot against whatever `remove_and_get` actually
  removes via `Arc::ptr_eq`. On a match (the common case), parking proceeds
  as before. On a mismatch — a concurrent write swapped in a different
  handle in the interim — the swapped-in handle is handed straight back via
  a new identity-preserving `IndexRegistry::restore` (no new `Arc`, so no
  other holder's `Arc::ptr_eq` breaks) and the park's own cold-store
  insertion is undone, so the id is never left in neither store. Only the
  feature's default-off, sub-microsecond window is affected; the fix is
  proven against a deterministic (synchronization-based, not timing-based)
  reproduction of the exact race.

  **Adversarial re-review (fifth round BLOCK) found the round-4 fix itself
  incomplete: `ColdIndexStore::mark_loaded` — the reap called by
  `create_index_handler` / `relocate_index_handler` / `reindex_handler`'s
  override, and internally by `cold_park_index_inner`'s own rollback path —
  remained an unconditional `entries.remove(id)` with no identity/generation
  check analogous to the `Arc::ptr_eq` guard round 4 added on the registry
  side.** Of the 10 possible interleavings between `cold_park_index_inner`'s
  three sequential steps and a concurrent handler's register+reap pair, round
  4 correctly closed 6 (5 where the registry-side `Arc::ptr_eq` mismatch
  triggers a rollback, plus 1 already-safe ordering) but left 2 of the
  remaining 4 — where the handler's `register` lands entirely before the
  park's `expected` snapshot, so the registry-side identity check trivially
  matches — still able to orphan the index: the handler's later unconditional
  `mark_loaded` reaps whatever is CURRENTLY parked under `id`, which by then
  is the park's own freshly-and-legitimately-inserted cold entry, not the
  stale leftover it believes it's cleaning up. Reproduced by execution
  (`cold_park_index_handler_naive_reap_before_park_orphans_index`): `parked =
  true, hot = false, is_cold = false` — reachable in neither store. Fixed by
  giving `ColdIndexStore` the identical identity discipline
  `IndexRegistry::restore` already applies: every cold-store insertion
  (`register_cold_entries`) is now stamped with a fresh, `Arc::ptr_eq`-
  comparable identity token (`ColdEntry.token`); `entry_token(id)` lets a
  caller snapshot "the entry I observed" immediately before its own write;
  and the new `mark_loaded_if(id, expected_token)` — the guarded counterpart
  to `mark_loaded` — only removes the entry when the CURRENT token still
  matches what was snapshotted (or both are `None`), leaving a mismatched
  reap as a safe no-op instead of deleting an entry it doesn't recognize. All
  three handler call sites and `cold_park_index_inner`'s own rollback now
  snapshot-then-guard via this pattern instead of calling `mark_loaded`
  unconditionally; `mark_loaded` itself is unchanged and remains correct for
  `get_or_load_index`'s call site, which is already serialized by the
  per-index `loading_gate` mutex and `cold_park_index`'s in-flight guard.
  `cold_park_index_handler_reap_guarded_before_park_never_orphans` proves the
  fixed counterpart of the same interleaving no longer orphans the id (it
  degrades to the pre-existing, disclosed "stale but present" residual
  instead).

- **Test-side remediation for `create_index`/`relocate_index` tests spuriously
  denied by the sensitive-path denylist (issue #3955).** `SENSITIVE_PATH_PREFIXES`
  denies `/tmp/`, `/private/tmp`, and `/var/folders` — which on macOS is where
  `std::env::temp_dir()` resolves by default, and on Linux CI it's `/tmp`
  outright. A dozen-plus `create_index_*`/`relocate_index_*` tests allocated
  their index roots via `tempfile::tempdir()`, so they intermittently got
  HTTP 400 from the very denylist they weren't testing. The denylist itself is
  correct and unchanged; the fix is a new shared test helper,
  `service::server::test_support::allowlisted_index_root`, that roots test
  index directories under `$HOME/.trusty-search-test-roots` — safe regardless
  of `$TMPDIR` or where the checkout itself lives (unlike the ad hoc
  `target/`-relative workaround some of these tests already had, which still
  fails if the checkout is placed under a denylisted prefix). RAII `TempDir`
  cleanup plus a best-effort 24h staleness sweep keep `$HOME` tidy.
- **Staged-write-then-swap for the periodic HNSW incremental persister closes
  a crash-safety hole independent of shutdown (issue #3970).**
  `spawn_incremental_persist` used to checkpoint the in-memory HNSW graph
  straight to the LIVE snapshot every `HNSW_SNAPSHOT_BATCH_INTERVAL` batches
  during EVERY reindex. Reindex progress is monotonic, so any reasonably
  large reindex crossed `UsearchStore::save`'s shrink guard threshold as
  ordinary healthy progress — from that checkpoint on, the complete
  pre-reindex snapshot was already overwritten by a partial, still-growing
  one, and an ungraceful termination (SIGKILL, OOM-kill, process abort, power
  loss) at any later point permanently stranded the index. This was the same
  vulnerability class as #1717 but reached through a different, far more
  frequently exercised path, and was NOT fixed by PR #3968 (which closes only
  the graceful-shutdown flush path). The periodic persister now redirects
  every checkpoint during a reindex to a staging path
  (`service::reindex::hnsw_swap`, mirroring the redb corpus's existing
  atomic staged-swap, #603/#839) and publishes to the live path in one
  atomic rename only when the reindex reaches a terminal `Ready` outcome;
  any other outcome (failure, memory-abort) discards the staged snapshot and
  leaves the live one untouched. Incremental crash-safety checkpointing
  during the reindex is fully preserved — the periodic persister is never
  skipped, only its destination changes, deliberately avoiding a
  skip-while-`Running` gate (which would have traded this hole for the loss
  of ALL in-reindex progress instead of just the tail).
  Round-2 adversarial review found and fixed two further issues in the swap
  itself: (1) the staging→live swap is two renames, not one — the sidecar is
  now renamed BEFORE the binary so an interruption between them can only
  leave a live pairing whose `next_key` sits ahead of (never behind) actual
  usage, which cannot collide on a subsequent write, and `UsearchStore::load_from`
  now additionally refuses to load a binary reporting MORE vectors than its
  paired sidecar describes, as defense-in-depth against a torn pairing from
  any source; (2) `CodeIndexer::end_reindex_staging` is no longer called
  before the swap (or abort cleanup) fully resolves — both now wait for any
  still-running periodic-persist task to quiesce first
  (`CodeIndexer::wait_for_incremental_persist_drain`), closing a race where a
  detached task that outlived the reindex's batch loop could otherwise
  observe the flag clear early and write partial state straight to the live
  path.
  **Scope correction:** what this fix buys is (a) bounded memory, by
  flushing vectors out of RAM during a long reindex, and (b) a safe,
  complete crash-recovery baseline — the live snapshot always reflects the
  last complete pre-reindex state, never a partial one. It does NOT enable
  resuming an interrupted reindex from its partial progress; after any
  crash mid-reindex, the next reindex attempt redoes the entire
  walk/parse/embed from scratch. That pre-existing gap is NOT #3969 (which
  is the different problem of a reindex never automatically restarting at
  all for non-HEAD-driven runs) — it is tracked separately as **issue
  #3979**.
- **Shutdown no longer publishes a partial in-flight reindex over a complete
  on-disk HNSW snapshot, closing the residual data-loss race the #1711 guard
  left open (issue #1717).** The #1711 guard (PR #1716) only catches an
  in-memory index with exactly 0 vectors; a background reindex that is only
  partially complete when SIGTERM lands (e.g. 5,000 of 312,000 vectors
  upserted into a freshly promoted, not-yet-restored store) is non-zero, so
  that guard did not fire — the shutdown flush silently overwrote a complete
  on-disk HNSW snapshot with the partial one. Two changes close this:
  1. `flush_one_index_on_shutdown` now checks `SearchAppState::reindex_progress`
     and skips the flush entirely — exactly, at any completion percentage —
     whenever a reindex for that index is still `ReindexStatus::Running`.
     This is the fix that actually closes the reported race, but it is
     scoped to the GRACEFUL shutdown flush path specifically (mirroring the
     identical guard the residency-park sweep already used for the same
     reason) — it does not run at all on an ungraceful termination
     (SIGKILL/OOM-kill/process abort/power loss).
  2. `UsearchStore::save()` additionally refuses a save whose in-memory vector
     count falls below half of what tracked `remove()` calls since the last
     save can explain, relative to the on-disk sidecar's count. This is
     defense-in-depth for callers with no reindex-progress signal available.
     Deliberate deletions (single-file removal, prune passes, bulk corpus
     reduction) are tracked via a per-store `removed_since_save` counter,
     incremented only when a vector is actually dropped from the HNSW graph,
     and are therefore never blocked no matter how large the reduction.

  Two known residual gaps, tracked separately, not fixed in this change:
  - **Issue #3970**: the periodic incremental HNSW persister
    (`spawn_incremental_persist`, called every 16 batches during EVERY
    reindex, independent of shutdown entirely) is guarded only by the ratio
    guard above — and that guard provides essentially NO protection there,
    on any reindex large enough to matter, because ordinary healthy progress
    is guaranteed to cross its 50% threshold before finishing. Once it does,
    the complete pre-reindex on-disk snapshot has already been overwritten
    by a partial, still-growing one; an ungraceful crash at any later point
    permanently strands the index at whatever fraction was last
    checkpointed. The recommended fix is a staged-write-then-swap for the
    HNSW snapshot, mirroring what the redb corpus already has via
    #603/#839 — explicitly NOT a skip-while-`Running` gate on the periodic
    save, which would defeat incremental persistence's entire purpose.
  - **Issue #3969**: a reindex triggered by something OTHER than a HEAD
    change (e.g. `--force` on an unchanged HEAD, or an embedding-model
    upgrade) that is interrupted before completion is not automatically
    retried on the next boot — `indexed_head_sha` is only re-stamped on
    successful completion, and boot-time reconcile only retries when the
    stored SHA is stale relative to HEAD. The index is left at its
    pre-reindex state (not corrupted, not silently smaller — just not
    caught up) until an operator triggers another reindex.
- **A legacy/colocated index whose storage path could not be resolved at
  warm-boot no longer silently restores as a healthy 0-chunk store (issue
  #2847).** `build_indexer_from_entry` / `build_store_for_entry` previously
  only logged a WARN when `corpus_redb_path_for_entry` / `hnsw_path_for_entry`
  failed (e.g. a colocated `.trusty-search` shadow path that could not be
  created — missing/broken symlink, permission denied) and otherwise
  proceeded as if the index had simply never been populated —
  `corpus_open_failed` stayed `false` and `hnsw_load_failed` stayed `false`,
  so the daemon reported the index as healthy while it served zero results.
  Both resolution failures now flag `corpus_open_failed` / `hnsw_load_failed`
  so the existing warm-boot stage classifier reports the index as degraded
  instead — a genuinely empty, never-indexed index is unaffected and still
  reports as pending, not failed.
- **Warm-boot's colocated-root discovery scan now honors `--no-auto-discover`
  / `TRUSTY_NO_AUTO_DISCOVER` (issue #3929).** Previously the flag only gated
  the unrelated `auto_discover_and_index()` git-repo scan; `restore_indexes`
  called `collect_colocated_entries` unconditionally on every boot, so a
  restart with the flag set still walked every tracked root in `roots.toml`
  and re-registered already-tracked indexes under a second, differently
  derived id — both pointing at the same `<root>/.trusty-search/index.redb`.
  redb is single-open, so the second registration failed with
  `DatabaseAlreadyOpen` (188/222 indexes on the reporter's production box).
  The scan is now gated by a new `collect_colocated_for_warmboot` helper in
  `commands/start/restore.rs`.
- **Hardened the warm-boot corpus dedup guard with file-identity (device,
  inode) matching, on top of the existing root-path canonicalization (issue
  #3929).** Two colocated entries whose `root_path` strings do not
  canonicalize to the same value (e.g. two different mount-point aliases of
  one backing NFS/EFS export) but whose resolved `index.redb` is the same
  physical file are now still collapsed to one survivor — closing a gap in
  `corpus_dedup_key` (`service/warm_boot/mod.rs`) where "same `colocated`
  corpus" was determined purely by root-path string equality.
- **`GET /health`'s top-level `status` now reflects the FULL
  `warm_boot_degraded` signal, not just corpus-open failure (issue #3706).**
  `overall_status` previously downgraded to `"degraded"` only when
  `indexes_corpus_failed > 0` or a watcher was network-degraded, ignoring
  the other three conditions `warmboot_summary.warm_boot_degraded` itself
  aggregates (per its own doc comment in `state.rs`): a TCC/FDA denial, a
  scan timeout, and mass index loss (loaded < 80% of the prior-known
  count). A daemon that was genuinely warm-boot-degraded purely from one of
  those three still reported `status: "ok"` on `/health` — exactly the
  silent-degradation gap `warm_boot_degraded` exists to close, and the
  reason trusty-review's `is_serving()` (#3693/#3704) never got a chance to
  catch it, since it only consults `warm_boot_degraded` once `status`
  itself already reads `"degraded"`. `overall_status` now checks
  `warmboot_summary.warm_boot_degraded` directly (a strict superset of the
  old `indexes_corpus_failed > 0` check, since that count is already
  folded into `warm_boot_degraded`), so all four conditions now flip the
  top-level status.
- Corpus-open failures are now classified rather than collapsed into one string.
  A transient open timeout or lock contention no longer reports "incompatible or
  corrupted format" and no longer prescribes `trusty-search index <path> --force`
  — wording that had already cost one healthy 200k-chunk index to a destructive
  rebuild. Transient states say the on-disk corpus is presumed intact and
  explicitly forbid a reindex; only a redb-reported format incompatibility or
  corruption keeps the rebuild instruction. `GET /indexes/:id/status` gains a
  `corpus_open_failure` object (`kind`, `transient`, `reason`) and reports
  `chunk_count: null` instead of a partial-looking in-memory count while the
  corpus is unopened (see
  [#4333](https://github.com/bobmatnyc/trusty-tools/issues/4333)).
- An index whose durable corpus failed to open no longer answers searches with
  `HTTP 200` and an empty result set — a total per-index outage that was
  indistinguishable from "no matches". `POST /indexes/:id/search` now returns
  `503 index_corpus_unavailable` carrying the failure classification, and
  `POST /search` excludes such indexes from the fan-out and reports them in a new
  `corpus_failed_indexes_skipped` field. An index whose eager warm-boot restore
  **times out** is now parked in the cold store — recoverable by lazy load on the
  next query — instead of being dropped from both the registry and the cold store
  for the rest of the boot. A restore that **panics** is deliberately not parked:
  it is broken rather than slow, so it keeps failing loudly instead of being
  reported as lazy/recoverable, and panics are now counted separately from
  timeouts in the warm-boot summary (see
  [#4087](https://github.com/bobmatnyc/trusty-tools/issues/4087)).
- The orphan reaper no longer defers indefinitely on ambiguous relocation
  candidates. The first ambiguous observation is stamped and logged at ERROR (so
  it reaches `errors.jsonl` / `tm doctor` rather than only the log file), and
  after a grace period — 7 days by default, tunable via
  `TRUSTY_AMBIGUOUS_ROOT_GRACE_SECS`, disabled with `0` — the stale *registration*
  is removed with a logged warning. On-disk index data is never deleted, so the
  entry stays recoverable with `trusty-search index <path>` (see
  [#4095](https://github.com/bobmatnyc/trusty-tools/issues/4095)).
- An index whose vector layer warm-booted empty is no longer left permanently
  broken while still self-reporting `ready`. Two defects combined: the vector
  layer never recovered, and the status surface never admitted it
  ([#4707](https://github.com/bobmatnyc/trusty-tools/issues/4707)).
  - **Recovery.** When warm-boot's `UsearchStore::load_from` discards a snapshot
    (the `#2922` size floor, a corrupt sidecar, the `#3970` torn-pairing guard)
    the store falls back to a fresh empty one. Every later save of that empty
    store was then correctly refused by the `#1711` data-loss guard — and
    nothing further happened, so the index served zero vectors forever with an
    intact snapshot sitting on disk. After refusing the write, `save()` now
    adopts that on-disk snapshot, so the vector lane recovers without a reindex.
    The `#1711` guard is unchanged and nothing is ever written on that path;
    adoption only moves in-memory state towards what is already durable, and
    reuses `load_from`, so a truncated or torn snapshot is rejected by exactly
    the code that rejects it at warm-boot. The `#1717` shrink refusal
    deliberately does not recover this way — a partial in-memory index may hold
    vectors disk does not.
  - **Honest health.** The semantic stage is no longer published as `ready`
    when the live vector store holds zero vectors, a corpus exists, and an
    embedder is wired. An all-hash-skipped incremental reindex legitimately
    embeds nothing (`#868`), and both the fast pass and the deferred-embed pass
    marked `ready` on that basis without ever consulting the store they were
    vouching for. The stage now reports `failed` with an actionable reason, so
    `search_capabilities` stops advertising `vector` and the search handler
    keeps down-shifting queries to the working lexical lane instead of routing
    them through a query-embed step whose failure surfaced as
    `500 internal search error` on every query.

### Security

- boot reconcile no longer indexes gitignored files when a git probe fails (closes [#4733](https://github.com/bobmatnyc/trusty-tools/issues/4733))
  - `head_sha()` returns `None` for a repo git merely declined to read — a stale worktree gitlink, `detected dubious ownership`, an unreadable `.git` — and reconcile dropped into the mtime catch-up walk meant for genuinely non-git roots. That walk honours `SKIP_DIRS` but not `.gitignore`, so previously-excluded files entered the corpus and became retrievable through the `search` and `grep` MCP tools
  - a new three-state `core::git::probe_work_tree` gates it: the mtime path now requires a CORROBORATED "no repository here". A work tree that has commits but was never stamped gets a full background reindex instead — safe regardless of git's health, because the reindex walk drives the `ignore` crate with `require_git(false)` and so applies `.gitignore` even when git cannot read the repository
  - a work tree with NO usable HEAD (an unborn `git init`, or a repo git declined to read) is left untouched and counted under a new `skipped_unresolvable_git` field on `GET /health`'s reconcile summary. A reindex there could not converge — `finish_reindex` re-stamps `indexed_head_sha` from the same `head_sha` that returned `None`, so it would re-walk the whole tree on every boot forever. Reporting it distinctly keeps a security refusal from reading as ordinary emptiness
  - the exit code is not a classifier (git exits 128 for every fatal, and a bare repo exits 0 printing `false`), so the probe matches only the parenthesised `not a git repository (or any of the parent directories)` stderr and corroborates it against an ancestor `.git` witness on a canonicalised path

---
## [0.39.1] — 2026-07-26

### Fixed

- **Interactive search queries now preempt background catch-up embeds at
  wave granularity via the previously-dormant `EmbedPool` priority lanes
  (issue #3748 slice B PR 1).** The two-lane `EmbedPool` (Interactive/
  Background, biased select, built for issue #41) was constructed and
  installed at boot but had zero callers — both the query path
  (`core::indexer::search::lanes::{embed_text, embed_query}`) and the
  catch-up path (`core::indexer::ingest::embed::embed_chunks_in_batches`)
  called the raw shared embedder directly, so a large catch-up pass could
  still starve interactive `/search` for its full duration. `CodeIndexer`
  now carries an optional `Arc<EmbedPool>` (wired at every production
  construction site — `restore_one_index`, `restore_index_on_demand`,
  `create_index_handler`, and the relocate handler — once the daemon's pool
  finishes warming up); queries route through the Interactive lane and
  catch-up sub-batches route through the Background lane, one pool request
  per wave, so a queued interactive request now waits at most one in-flight
  wave rather than the whole reindex. No pool installed (tests, CLI paths)
  falls back to the pre-#3748 direct-embedder call unchanged. Second
  dedicated catch-up sidecar (PR 2) deferred pending measurement.
  **Code-critic review round (PR #3784) fixed 4 issues before merge:**
  (1) *boot-race self-heal* — `install_embedder` unblocks request handlers
  strictly BEFORE `install_embed_pool` completes, so an index constructed in
  that window used to stay poolless for the daemon's lifetime; `CodeIndexer`
  now registers the daemon's own pool slot (`set_embed_pool_source`, an
  `Arc<RwLock<..>>` clone of `SearchAppState::embed_pool`) rather than a
  one-time snapshot, and `resolve_embed_pool` lazily re-checks + self-heals
  a lock-free `ArcSwapOption` cache on the next embed call once the pool
  comes online; (2) *observability* — `set_embed_pool`/`set_embed_pool_source`
  now `warn!` when a daemon path installs an empty pool, self-heals log at
  `info!`, and `GET /health` gained `indexes_embed_pool_missing` (same
  registry-scan pattern as `indexes_kg_disabled`); (3) *inflight collapse* —
  `EmbedPool::with_autotune` now floors its worker count at
  `resolve_embed_inflight()` so a ≤16 GB host (autotune=1 worker) doesn't
  silently serialize the `TRUSTY_EMBED_INFLIGHT` (default 2) concurrent
  sub-batches issue #753's ANE-idle fix relies on; (4) the priority-ordering
  regression test now uses a deterministic channel-send rendezvous instead of
  fixed sleeps and loops 20x to reliably catch a dropped `biased;`.
- **Deferred-embed catch-up queue is now size-ordered; `warm_boot_degraded`
  recomputes instead of staying sticky until restart (issue #3748 slice A).**
  The warm-boot deferred-embed (C2) catch-up queue was strictly serial and
  size-blind: one oversized repo (e.g. 94k chunks) that finished its fast
  pass before smaller repos would head-of-line-block every other index's
  semantic readiness for hours, and the boot-time `warm_boot_degraded` flag
  never re-evaluated once catch-up finished. `service::reindex::defer_embed_queue`
  (new module) now dispatches catch-up jobs ascending by the PENDING
  (un-embedded) chunk delta — not total corpus size, so an incremental
  reindex with one changed chunk in a 94k-chunk repo sorts by its real,
  near-instant embed cost — with FIFO tiebreak for equal sizes. An
  anti-starvation gate prevents a large job from being starved indefinitely
  by a steady trickle of newer, smaller arrivals, WITHOUT reverting an
  entire same-burst arrival (the warm-boot shape this fix targets — dozens
  of repos enqueuing within milliseconds of each other) back to raw arrival
  order just because the burst takes a while to fully drain by size: only a
  job that arrives a full `MAX_WAIT` (5 minutes) LATER than the oldest
  still-pending job counts as a genuinely later wave and can force a
  promotion. `GET /health`'s `warmboot_summary.warm_boot_degraded` now
  recomputes when the catch-up queue fully drains, folding in a live scan
  for any index with a `Failed` stage (so a genuinely failed embed pass
  still counts as degraded) instead of remaining frozen at its boot-time
  value forever. No embedder-concurrency or worker-pool changes (tracked
  separately as slice B).
- **`doctor_data_dir_returns_non_empty_path` deflaked at the source (issue
  #3697).** An audit confirmed every `TRUSTY_DATA_DIR` mutation site in the
  `--bin trusty-search` test binary already carried the crate's `#[serial]`
  convention (from #3673/#3686), so the flake persisted for a different
  reason: the test itself still read/wrote the shared process env var.
  Split `doctor_data_dir()` into a pure, parameter-injectable
  `doctor_data_dir_from(Option<String>)` core (mirrors the
  `SearchAppState::with_registry_path` fix for the same flake class, issue
  #2717) and pointed the test at it directly — it no longer touches process
  env at all, so it can't race any sibling test regardless of tagging.
- **`commands::start::embedder_fallback::tests::fallback_logs_build_failure_exactly_once`
  deflaked (issue #3689).** This test counts `tracing` events via a
  thread-local subscriber (`tracing::subscriber::with_default`); `tracing`'s
  per-callsite interest cache is process-global, not per-thread, so a
  concurrently-scheduled sibling test hitting the same `tracing::error!` call
  sites with no subscriber installed could leave a call site cached as
  "never interested," silently dropping an event this test expects to count.
  All 7 tests in the module share those call sites and are now `#[serial]`,
  matching the crate's existing isolation convention (#3629/#3673/#3608).
- **`core::memguard_enforce::tests::test_anon_rss_for_self_pid_on_linux` deflaked
  (issue #3762, recurrence of #3716's flake class).** The test compared two
  genuinely independent, non-atomic `/proc` reads taken microseconds apart
  (`anon_rss_mb_for_pid` parses `/proc/<pid>/status` directly;
  `current_rss_mb_for_pid` re-reads via `sysinfo`), so concurrent
  `cargo test --workspace` allocation churn could transiently make the anon
  sample larger than the already-stale total sample (observed in CI: "anon RSS
  (185 MB) must never exceed total RSS (116 MB)", passed on rerun). Mirrors
  #3716's fix: replaced the strict `anon <= total` bound with a one-directional
  structural check plus a generous sampling-skew headroom, rather than
  tightening/loosening an exact-equality bound.
- **Panic on non-char-boundary truncation of free-form text in 4 sites
  (issue #3685).** The `search` / `search_lexical` / `search_semantic` /
  `search_kg` / `search_all` MCP tool handlers and the HTTP `search` endpoint
  logged the query text truncated with a raw byte-index slice
  (`&query_text[..query_text.len().min(80)]`), which panics with "byte index
  is not a char boundary" whenever byte 80 lands mid-way through a
  multi-byte UTF-8 character (e.g. an emoji or CJK query), crashing the
  request instead of just logging it. `commands::index_status::truncate_reason`
  had the same bug (`&msg[..79]` on free-form, non-ASCII-guaranteed
  `JoinError`/embedder failure-reason text). Replaced all four sites with a
  new `trusty_search::truncate_at_char_boundary` helper that backs off to the
  nearest valid char boundary (mirroring the existing backward-scan pattern
  in `core::extract::extract_text`'s byte-cap truncation), plus regression
  tests covering an emoji split and a CJK split at the query-log 80-byte
  boundary and a separate emoji split at `truncate_reason`'s 79-byte
  boundary.
- **`core::memguard_enforce::tests::enforcement_rss_mb_for_pid_matches_chosen_measure`
  deflaked for good (issue #3716).** Three successive rounds of calibrating a
  "two live RSS samples of the same measure agree" tolerance on this test
  (10 MB, then 60% relative) all failed under `cargo test --workspace` CI
  churn — the 60% bound was itself exceeded twice on an unrelated release PR.
  The assertion is restructured to noise-immune single-sample checks (both
  measures resolve to `Some` and land in a sane band) plus a genuinely
  structural, Linux-only anon-subset-of-total check with generous headroom,
  instead of comparing two independently re-sampled live readings. Dispatch
  correctness stays pinned by the existing behavioral tests
  `run_memory_pressure_tick_respects_total_override_env` (cross-platform) and
  `run_memory_pressure_tick_gate_uses_anon_not_total_rss_on_linux`
  (Linux-only, since anon and total are defined to be equal off-Linux).

---
## [0.39.0] — 2026-07-23

### Changed

- **Cost-scaled idle-eviction threshold + oldest-idle-first sweep ordering
  (issue #3683 slice 2).** The idle-chunk/BM25/entity-eviction window is
  raised from a flat 60s (issue #2166) to a 300s floor, now scaled per-index
  by that index's own measured (or, before its first rehydrate, on-disk
  chunk-count-estimated) rehydrate cost — an expensive-to-rehydrate index
  (the i-0076 production incident's 315K-chunk / 27-40s-scan corpus) earns
  proportionally more idle time before eviction than a cheap one, directly
  addressing the #3683 RCA's thrash-eviction root cause. The idle sweep
  itself now processes indexes oldest-idle-first rather than the registry's
  arbitrary iteration order. New env override `TRUSTY_REHYDRATE_COST_SCALE_UNIT_MS`
  (default 1000ms per extra base-window multiple; `0` disables cost-scaling).
  `TRUSTY_CHUNKS_IDLE_EVICT_SECS` continues to set the base window.
- **Budgeted, oldest-idle-first, recency-exempt memory-pressure sweep (issue
  #3683 slice 2 — critic-review follow-up).** The pressure sweep
  (`TRUSTY_MEMORY_ENFORCE_SECS` / `TRUSTY_MEMORY_HIGH_WATER_PCT`) no longer
  unconditionally clears every registered index the instant RSS crosses the
  high-water mark. It now: (1) processes indexes oldest-idle-first (here the
  ordering is load-bearing, unlike the idle-eviction ticker's cosmetic use of
  the same sort); (2) stops once it has (estimatedly) freed enough to reach
  the high-water mark, instead of sweeping the whole fleet; (3) exempts
  recently-queried (hot) indexes from the first pass — new env
  `TRUSTY_MEMORY_PRESSURE_EXEMPT_IDLE_SECS` (default 30s; `0` disables the
  exemption) — falling through to a "desperation" second pass that clears
  hot indexes too if the exemption-respecting pass can't reach the target
  (avoiding an OOM kill outweighs a hot index's warm cache). The sweep's
  stop-early budget reports whether it actually visited every candidate
  (`Exhausted`) or stopped on its (uncalibrated) freed-bytes estimate while
  candidates remained (`EarlyStop`); `run_memory_pressure_tick` only trusts
  the post-sweep RSS as the next hysteresis baseline on `Exhausted` — an
  `EarlyStop` resets the baseline instead, so a steady-state RSS plateau
  never wedges the sweep from re-attempting the untouched indexes (round-2
  critic-review follow-up).
- **Anonymous-RSS memory-pressure enforcement gate (issue #3683 slice 3 —
  final slice, Defect 3).** The steady-state memory-pressure ENFORCEMENT
  decision (`over_high_water`, its hysteresis baseline, and the sweep's
  `target_freed_mb` budget) now reads anonymous RSS (`/proc/<pid>/status`'s
  `RssAnon`) by default on Linux instead of total RSS — on the #3683
  production workload, file-backed redb mmap pages (kernel-reclaimable on
  their own) dominated total RSS, reading the daemon as permanently over its
  ceiling even when a sweep freed almost nothing durable. New env
  `TRUSTY_MEMORY_ENFORCE_MEASURE=anon|total` lets operators pick the
  enforcement measure explicitly (default `anon` on Linux, `total` on
  macOS, where `current_rss_mb` already reads `phys_footprint` — itself
  already anon-equivalent in spirit). Total RSS stays visible in `/health`
  and this ticker's log lines for operator context regardless of which
  measure gates enforcement; every comparison in the enforcement chain uses
  the same measure end to end so the slice-2 hysteresis baseline is never
  compared against a different measure than the one that set it. If `anon`
  is selected but `RssAnon` is permanently unavailable (pre-4.5 kernel,
  hardened/restricted container), enforcement now degrades to total RSS
  automatically (once, with a `tracing::warn!`) instead of silently
  disabling the enforcement ticker forever (critic-review HIGH finding).
  **Upgrade note:** since anon RSS is always `<=` total RSS, the same
  `TRUSTY_MEMORY_LIMIT_MB` now trips the sweep and the hard-limit restart
  LATER (at a higher real footprint) on Linux than before — re-validate a
  limit tuned as an OOM backstop, or set `TRUSTY_MEMORY_ENFORCE_MEASURE=total`
  to preserve the prior trip point exactly.

### Fixed

- **Deflake `test_rss_for_self_pid` under `cargo test --workspace` (issue
  #3702).** The test asserted two RSS samples of the same process (which
  delegate to the exact same sampling call) agree within a fixed 10MB —
  fine in isolation, but under the workspace test run's shared-process
  parallel execution, concurrent sibling-test allocation churn shifted the
  readings by up to 30MB across 3 consecutive CI runs, blocking green-only
  merges on unrelated PRs. Replaced the fixed bound with sanity-band,
  relative-agreement, and deliberate-allocation-growth checks that keep
  catching a genuinely broken/stale RSS reading without depending on a
  quiet process.
- **Detached, deduplicated corpus rehydrate — stops the 408 livelock (issue
  #3683 slice 1).** BM25/chunk rehydration after idle eviction used to run
  the redb scan AND the map-publish/flag-clear inline inside the caller's
  own awaited future — including interactive query handlers wrapped in
  `apply_query_timeout`'s `tokio::time::timeout`. On expiry that whole
  future was cancelled, discarding completed rehydrate work and leaving the
  index cold, so the next query paid the full O(corpus) scan again
  (self-sustaining livelock under repeated timeouts on a large corpus — 27s+
  observed on a 315K-chunk NFS-backed index). Rehydration now runs as a
  detached, per-index-deduplicated `tokio::spawn` task (mirroring the
  #3659 `open_guard` pattern) that commits regardless of how many callers
  time out waiting for it; `ensure_chunks_loaded` / `ensure_bm25_entities_loaded`
  are now thin, bounded-wait wrappers around one consolidated scan (also
  killing a pre-existing double `load_all_chunks()` scan for queries that
  touch both lanes).
- **Deterministic BM25 corpus-cap selection across evict/rehydrate cycles
  (issue #3684).** The rehydrate scan (and warm-boot restore) now sort
  chunks by their stable id before the cap-truncated BM25 upsert loop, so
  which subset of an over-cap corpus is lexically searchable no longer
  shifts with redb's B-tree iteration order between cycles. Cap drops during
  a rehydrate now log a per-rebuild dropped-count (via
  `Bm25Index::upsert_document_reporting`) and emit a
  `trusty_bm25_docs_dropped` gauge, instead of relying on trusty-common's
  process-wide log-once latch.
- **Detached rehydrate hardening — code-critic review round 2 (issue
  #3683).** Three follow-up fixes to the detached rehydrate task above:
  - Panic-safe gate clearing: the per-index rehydrate-in-flight gate is now
    cleared by a real `Drop` guard (`RehydrateGateClearOnDrop`), constructed
    before any fallible work, so a panic anywhere in the commit phases can no
    longer wedge the gate at `Some(dead_notify)` forever (the exact
    #3659/#3666 "opposite-polarity" bug recurring in a new guard).
  - Evict-vs-rehydrate commit race: a new per-index `rehydrate_generation`
    counter, bumped by every real idle-evict/`reclaim_memory_now` clear, is
    snapshotted before a rehydrate spawns and checked before its commit — a
    concurrent evict/reclaim landing mid-rehydrate now invalidates the
    pending commit instead of silently overwriting `*_evicted` back to
    `false`.
  - The bounded per-query rehydrate wait was silently guaranteed to lose
    against the 27-40s cold-scan latency measured in production; raised to
    9s (~27s total across retries) and made the degrade observable instead
    of silent — a sticky `lane_degraded` flag, `trusty_bm25_lane_degraded`
    gauge, `trusty_bm25_lane_degraded_total` /
    `trusty_grep_fallback_lane_degraded_total` counters, and a new
    `meta.bm25_lane_degraded` field on the search HTTP response (mirroring
    `WarmBootSummary.warm_boot_degraded`) now distinguish "degraded, corpus
    still rehydrating" from a genuine empty result.
  - The `trusty_bm25_docs_dropped` gauge above now always reports the
    current dropped count (including zero), instead of only when nonzero,
    so it can't hold a stale reading from a prior rehydrate.
- **Detached rehydrate hardening — code-critic review round 3 (issue
  #3683), the remaining HIGH.** The evict-vs-rehydrate race fix above still
  had a narrower load-then-store window: reading `rehydrate_generation` via
  a bare atomic load, then separately storing the `*_evicted` flags, left a
  gap in which a concurrent evict's own bump-and-set could land and get
  silently clobbered by the commit's flag-clear. `rehydrate_generation` is
  now a `std::sync::Mutex<u64>`; both the evict side (bump generation + set
  flag `true`) and the commit side (read generation + conditionally clear
  flags) hold that same lock across their entire sequence, making the two
  critical sections mutually exclusive with no window left to race.

---
## [0.38.1] — 2026-07-22

### Fixed

- **Panic-safe, serialized redb corpus open on concurrent warm-boot (issue
  #3659).** Warm-boot (eager restore), lazy-load, `POST /indexes`
  create/relocate, and the reindex atomic-swap re-open could all reach
  `CorpusStore::open` for the SAME `index.redb` at once before an index is
  registered — nothing serialized them. A torn concurrent read of a
  half-written file doesn't always surface as a classified `DatabaseError`
  (the #702/#703 guarantee); it can trip an internal assertion inside redb's
  `page_manager` and panic. `core::corpus::open_guard::open_serialized` now
  serializes every corpus open per-canonical-path (at most one opener in
  flight for a given file) and converts any panic into a typed `Err` via
  `spawn_blocking` + `catch_unwind`, so the existing migration/rebuild retry
  ladder always sees a normal `Result`. A wedged opener (e.g. a TCC-denied
  volume, issue #718) times out instead of hanging its caller forever, and
  the path is then marked permanently refused so later callers fail fast
  rather than queue behind it.
- **Idle-evicted chunk/BM25/entity caches now actually return memory to the
  OS (issue #3657).** Production saw RSS climb 20.3 → 26.4 GiB over ~5 hours
  toward an OOM-kill while the daemon repeatedly logged `evicted N in-memory
  chunks after 60s idle` — the maps were genuinely emptied (every value is
  owned data, never `Arc`-aliased elsewhere), but the Linux release binary's
  default glibc allocator never handed the freed small-object heap back to
  the OS. Both the idle-eviction ticker and the issue #2846 memory-pressure
  reclaim sweep now call `libc::malloc_trim(0)` on a `spawn_blocking` task
  (Linux-only; no-op elsewhere — and moved off the tokio worker thread since
  a trim over a many-GiB fragmented heap can hold the malloc arena lock for
  tens to hundreds of milliseconds) right after a bulk clear, and log the
  observed RSS before/after so "evicted N chunks" claims are independently
  verifiable instead of assumed.
- **`TRUSTY_MEMORY_LIMIT_MB` auto-tune now respects cgroup memory ceilings
  on Linux, including NESTED systemd/Docker/Kubernetes cgroups (issue #3657
  follow-up on #2846).** RAM detection previously read only `/proc/meminfo`
  (the HOST's total physical RAM), so on a host with far more RAM than a
  cgroup allows this one process, the 25%-of-RAM auto-tuned soft ceiling
  could land ABOVE the actual enforced cgroup limit — silently defeating the
  #2846 memory-pressure enforcement ticker before the kernel's cgroup
  OOM-killer fires. Detection now resolves this process's own cgroup path
  from `/proc/self/cgroup` (not just the cgroupfs root — a systemd-managed
  service like `trusty-search.service` lives at a nested path such as
  `/system.slice/trusty-search.service`, which the root's own `memory.max`/
  `memory.limit_in_bytes` does not reflect) and reads the ceiling at that
  nested location for both cgroup v2 (`memory.max`) and v1
  (`memory.limit_in_bytes`), using whichever ceiling (cgroup or host RAM) is
  smaller.

---
## [0.38.0] — 2026-07-21

Ships the epic #3524 slice 6 default flip. Depends on trusty-embedderd-py
0.1.1 (drift-closed in this same release) for the `/health` provider
readback used to verify the flip below.

### Changed

- **Graceful-Python embedder is now the DEFAULT on Apple Silicon (epic #3524
  slice 6, PR 5/5 — the default flip).** `trusty-search start` with
  `TRUSTY_EMBEDDER` unset/`auto` on aarch64 macOS now serves on the ort
  stdio sidecar immediately while bootstrapping the python/MPS sidecar in
  the background and hot-swapping to it once proven — previously this
  required opting in via the now-retired `TRUSTY_PY_DEFAULT` ship-gate.
  Validated by the epic #3524 slice 2-4 spike (numerically identical
  results, ~2.4x faster end-to-end) and soaked per PR #3610 before this
  flip. Every other platform (Linux, Intel mac, CUDA) is completely
  unaffected — the flip is scoped to `cfg!(all(target_arch = "aarch64",
  target_os = "macos"))` only. `TRUSTY_EMBEDDER=stdio` remains, and is now
  the sole, permanent per-invocation escape hatch back to the unchanged ort
  path on Apple Silicon.

### Fixed

- **Daemon stack-overflow crash from deep recursion in the AST chunker/entity
  walk (issue #3537).** `walk_for_chunks` (`core/chunker/walk.rs`) and
  `walk_rust` (`core/entity.rs`) were native recursive descents whose stack
  depth tracked raw tree-sitter parse-tree depth — attacker-influenced, since
  it comes directly from file content being indexed. A deeply nested
  global-scope construct (e.g. deeply templated C++ headers, or any deeply
  nested expression/type outside a function body, which is already pruned)
  could exceed the process stack and abort the whole daemon with `fatal
  runtime error: stack overflow`, taking every other index down with it.
  Both walks are now iterative (explicit heap-allocated work stack, matching
  the pattern `collect_calls` already used), which removes the crash
  regardless of nesting depth, plus a bounded max-walk-depth guard (logged,
  not silent) so a single pathological file degrades to "partially chunked"
  rather than either crashing or — since `classify_node`'s per-node
  `Node::parent()` lookups are not O(1) — hanging the indexing worker on
  superlinear traversal cost.

## [0.37.2] — 2026-07-21

Patch release closing unpublished source drift under the already-published
0.37.1 (issue #3366 defect class). 0.37.1 was published to crates.io (from
`831103dd`) containing only the `ui-dist` regeneration below; every other
entry in this section — the #3545 CLI daemon-discovery fix and the epic
#3524 slice 5/6 embedder work — landed on `main` in later commits that
never bumped the version, so none of it is in the live 0.37.1 tarball. This
release carries all of it.

### Fixed

- **CLI daemon discovery (`index`/`list`/`reindex`/`search`/`port`/`serve`)
  now honors `TRUSTY_DATA_DIR` (issue #3545).** These subcommands resolved
  the daemon's address via a generic `trusty_common` resolver keyed only to
  the test-only `TRUSTY_DATA_DIR_OVERRIDE` env var and a file location
  distinct from the one `start`/`run_daemon()` actually wrote
  (`$HOME/.trusty-search/http_addr`, hardcoded regardless of
  `TRUSTY_DATA_DIR`) — so an isolated instance's clients could silently
  reconnect to a stale, cached production-daemon address instead of the
  isolated instance, even with `TRUSTY_DATA_DIR` and a non-default port set.
  This caused an accidental production-daemon index mutation during PR
  #3529. `service::daemon::http_addr_path()` now honors `TRUSTY_DATA_DIR`
  (mirroring `daemon_dir()`), and every CLI call site reads/writes through
  that single resolver instead of the generic one, so `start` and every
  client subcommand always agree on which daemon they mean.
  - **Follow-up (code-critic review):** the first cut of this fix removed
    the only writer of the generic `trusty_common::write_daemon_addr`
    registry without replacing it, silently breaking two other consumers
    that still read it exclusively — `trusty_common::monitor::search_client`
    (`trusty-search monitor status`/`monitor indexes`/`monitor tui`, whose
    `[r]` hotkey reindexes via that resolved address) and trusty-installer's
    `ensure` (register-index + readiness-poll stages). `run_daemon()` now
    also populates that registry, but **only for the default
    (`TRUSTY_DATA_DIR`-unset) instance** — an isolated instance still never
    writes it, preserving the original fix's isolation guarantee. The CLI's
    reachability-probe refresh write also now goes through the same atomic
    tmp+rename helper the daemon itself uses, instead of a bare
    `std::fs::write` that could race a torn read.

### Added

- **Swap-BACK watchdog: python/MPS → ort on confirmed sidecar death (epic
  #3524 slice 6, PR 4/5) — ships DEFAULT OFF (unchanged behind
  `TRUSTY_PY_DEFAULT`).** PR-3 hot-swaps ort→python once the sidecar proves
  itself, then stops watching. This PR adds the other half: once hot-swapped,
  a new detached watchdog task (`commands/start/swap_back_watchdog.rs`)
  watches the python sidecar's pid slot and the existing
  `EmbedderStallTracker`, and swaps `SwitchableEmbedder` back to a fresh ort
  backend if the python sidecar is ever confirmed dead beyond recovery —
  search never degrades permanently, with no daemon restart required.
  - **The swap-back predicate uses a DEFINITIVE supervisor signal, not a
    heuristic (post-merge-review fix — code-critic BLOCK on PR #3584).** The
    first version of this predicate fired on `active.kind == Python &&
    pid_slot == 0 && recent_timeout_count > 0`, debounced across 2 polling
    ticks. Code-critic caught a real false-positive window before merge: an
    ordinary idle-shutdown ALSO zeros the pid slot, and a stale non-zero
    `recent_timeout_count` left over from an earlier TRANSIENT failure is
    never cleared by idle-shutdown (only a subsequent successful embed clears
    it) — so a quiet period with zero further embed traffic after an
    idle-shutdown could satisfy the heuristic and PERMANENTLY swap away a
    perfectly healthy sidecar. Fixed by adding
    `trusty-common`'s new `EmbedderSupervisor::terminated_signal()` (see that
    crate's changelog) — an `Arc<AtomicBool>` the supervision loop sets ONLY
    at the instant it exhausts `max_restarts` / a wedge-restart storm, never
    on a clean exit or an intentional shutdown. `LazyEmbedderHandle::is_confirmed_terminated()`
    exposes this (via the extended `PythonAdapterTeardown` trait), and the
    predicate is now simply `active.kind == Python &&
    is_confirmed_terminated()` — no debounce needed, since the flag is
    monotonic and unambiguous the instant it's observed. The regression test
    `swap_back_does_not_fire_on_stale_timeout_count_plus_idle_shutdown`
    reproduces the exact scenario code-critic named and fails against the
    pre-fix heuristic.
  - On confirmed death: builds a fresh ort backend via the same
    `build_ort_stdio_sidecar()` the daemon's default path uses, hot-swaps
    `SwitchableEmbedder` to it (`ActiveBackend { kind: Ort, bootstrap:
    FellBackToOrt }`), reinstalls the ort pid slot, and cleanly shuts down the
    dead python handle via `LazyEmbedderHandle::shutdown()` (no orphan). Logs
    a loud `warn`: "python/MPS sidecar unrecoverable — fell back to ort;
    search unaffected". `/health`'s `embedder_bootstrap` already reports
    `"fell_back_to_ort"` for this state (wired in PR-2/PR-3).
  - **CAS upgrade (code-critic LOW follow-up from PR-3 review)**:
    `SwitchableEmbedder::set_bootstrap_state` was a non-atomic
    read-modify-write on `active`, safe only because PR-3's orchestrator was
    the sole writer. PR-4 introduces a second writer (the watchdog's own
    `swap_to`), so `set_bootstrap_state` now uses `ArcSwap::rcu` — a real
    compare-and-swap retry loop — so a concurrent `swap_to` and
    `set_bootstrap_state` can never lose either side's update.
  - Bounded and quiet: polls every 15s (not a tight loop), and stops
    permanently once it either acts (swaps back) or notices the active
    backend already moved away from python — never watches a backend it no
    longer owns.
  - New file: `commands/start/swap_back_watchdog.rs`. `drive_bootstrap`
    (`graceful_bootstrap.rs`) now returns the python adapter's teardown
    handle on success so `run_graceful_python_bootstrap` can hand it to the
    watchdog — a pure return-type addition; no existing call site needed to
    change.

- **Graceful Apple-Silicon default gating + background bootstrap→hot-swap
  orchestrator (epic #3524 slice 6, PR 3/5) — ships DEFAULT OFF.** On Apple
  Silicon, once `TRUSTY_PY_DEFAULT` is enabled, `trusty-search start` now
  serves on the ort stdio sidecar IMMEDIATELY (identical to today's default)
  while a new background task bootstraps the python/MPS sidecar (venv +
  launcher discovery), proves it with one real readiness-probe embed call,
  and hot-swaps the running `SwitchableEmbedder` (epic #3524 PR-1) over to it
  — with zero HTTP-listener or startup delay. On any bootstrap or
  readiness-probe failure (after `TRUSTY_PY_BOOTSTRAP_RETRIES` attempts,
  default 2, with a linear backoff between attempts) the daemon stays
  permanently on the still-installed ort backend for that daemon's lifetime
  and `/health`'s `embedder_bootstrap` reports `"failed"` instead of
  `"bootstrapping"` forever.
  - **`TRUSTY_PY_DEFAULT`** (env, default OFF): the ship-gate for this PR.
    Unset/falsy leaves Apple-Silicon unset/`auto` resolution completely
    unchanged (ort) — this PR is a no-op for real users until a later slice
    (PR-5) flips the default on after a soak period. `TRUSTY_EMBEDDER=stdio`
    remains the permanent per-invocation opt-out even after that flip.
  - **`TRUSTY_EMBEDDER_PYTHON_EAGER`** (env, default OFF): reaches the
    existing eager, blocking `python` arm (identical to explicit
    `TRUSTY_EMBEDDER=python`) via unset/`auto` instead of an explicit value;
    not platform-gated. Takes precedence over the ship-gate flag being off,
    but the ship-gate flag wins outright when both are set (no reason to
    block startup when the background path is available).
  - **`TRUSTY_PY_BOOTSTRAP_RETRIES`** (env, default `2`): number of
    bootstrap→probe attempts the background orchestrator makes before giving
    up and marking the bootstrap `Failed`. A malformed or `0` value falls
    back to the default rather than disabling retries.
  - Linux/CUDA/Intel-mac hosts are completely unaffected: the new
    `DefaultEmbedderMode::GracefulPython` resolution is unreachable off
    Apple Silicon regardless of env — `ensure_venv`/`uv`/torch are never
    invoked there.
  - This PR is swap-in only: detecting a live python sidecar dying after a
    successful hot-swap and falling back to ort is epic #3524 slice 6 PR-4's
    scope (seam left in `commands/start/graceful_bootstrap.rs`'s
    `drive_bootstrap` doc comment).
  - New files: `commands/start/graceful_bootstrap.rs` (the orchestrator).
    Extended: `SwitchableEmbedder::set_bootstrap_state` (updates only the
    bootstrap status, leaving the live backend untouched — used to mark
    `Failed` without disturbing the still-serving ort backend).
  - **Fix (code-critic HIGH, pre-merge review): no more orphaned python
    child on a bootstrap-probe failure/timeout.** The readiness probe forces
    a REAL `trusty-embedderd-py` child to spawn (torch+MPS, hundreds of
    MB-GB); previously, dropping the adapter on a failed/timed-out probe left
    that child alive until the idle watchdog reaped it up to 1800s later —
    with retries, up to `TRUSTY_PY_BOOTSTRAP_RETRIES` such orphans
    concurrently on the memory-constrained Apple-Silicon machines this
    targets. Added `LazyEmbedderHandle::shutdown()`
    (`service/embedder_supervisor/mod.rs`) — an eager, cooperative
    counterpart to the existing idle-shutdown watchdog's own teardown
    (`SupervisorHandle::shutdown()`, issue #2979) — and a
    `PythonAdapterTeardown` seam so `try_bootstrap_once` calls it
    immediately on every probe failure/timeout path, before ever dropping
    the handle.

### Changed

- **`/health` reports the true active embedder backend + MPS provider (epic
  #3524 slice 6, PR 2/5, closes #3530, #3493 P1)** — `GET /health` now sources
  `embedder_info` (`provider`, `quantized`, new `model` and `backend` fields)
  from the REAL installed `ActiveBackend` via
  `SearchAppState::current_switchable_embedder()` (epic #3524 PR-1's
  `SwitchableEmbedder`) instead of inferring: the previous `quantized:
  dimension == 384` check was always `true` regardless of the actual model,
  and the Python/MPS sidecar reported `provider=CoreML` even though it never
  touches ONNX Runtime. A new top-level `embedder_bootstrap` field
  (`"n/a"`/`"bootstrapping"`/`"ready"`/`"failed"`/`"fell_back_to_ort"`) mirrors
  `ActiveBackend::bootstrap`. Falls back gracefully to the old
  prediction-based path when no `SwitchableEmbedder` handle is installed yet
  (e.g. very early boot) — never panics or 500s.
  - Fixed an ordering gap from PR-1: `commands/start/daemon.rs`'s init task
    now installs the `SwitchableEmbedder` handle BEFORE flipping embedder
    readiness, so `/health` can never observe `is_embedder_ready() == true`
    while the switchable handle is still absent.
  - `LazySlotEmbedderAdapter::provider()` (`commands/start/embedder.rs`) now
    distinguishes the ort stdio sidecar from the Python/MPS sidecar (both use
    the same adapter type) and predicts through the matching resolver —
    `trusty_common::embedder::resolve_expected_python_provider` for the
    Python arm — instead of always using the ORT-oriented resolver.
  - `ActiveBackend::quantized` is no longer set from `TRUSTY_EMBEDDER_MODEL`
    for every backend: only the ort/in-process `FastEmbedder` path actually
    honours that env var (see `backend_respects_quantized_env`); the Python
    sidecar and a manually managed remote sidecar always report `false`
    rather than inheriting an unrelated `int8` setting.
  - Fixed the `(Q)` startup-log hardcode (`embedder initialized:
    model=AllMiniLML6V2(Q) ...`) to report the real resolved model name via
    the new `FastEmbedder::model_name()` (trusty-common).
  - `SearchAppState::switchable_embedder` is now backed by
    `arc_swap::ArcSwapOption` instead of `tokio::sync::RwLock` — `/health`
    reads it wait-free (`load_full`), matching the non-blocking-`/health`
    invariant from issue #1006.
  - Forward-note (low priority, not implemented here): `BackendKind::Remote`
    still collapses HTTP and UDS into one `"remote"` `/health` tag — see
    `backend_kind_str`'s doc for the split-out path if a consumer ever needs
    to distinguish them.

- **`SwitchableEmbedder` plumbing (epic #3524 slice 6, PR 1/5)** — pure
  refactor, no behavior change. `build_embedder()` now wraps whatever backend
  it constructs (ort stdio sidecar, opt-in Python/MPS sidecar, in-process,
  remote HTTP/UDS, candle) in a new `SwitchableEmbedder`
  (`service/embedder_supervisor/switchable.rs`) that holds the live backend
  behind `arc_swap::ArcSwap` and implements the crate-local `core::Embedder`
  trait itself, delegating every call to whichever backend is currently
  installed. This closes a real gap the embed-pool workers had: each worker
  captures its own `Arc::clone` of the embedder at construction
  (`service/embed_pool.rs`), so writing a fresh `Arc` into
  `SearchAppState::embedder_slot` could never reach an already-running
  worker. Every existing owner (the slot, the pool workers, warm-boot
  restore) now transparently holds the same `SwitchableEmbedder` and will
  observe a future hot-swap (`SwitchableEmbedder::swap_to`, wired up by a
  later slice) with zero further call-site changes. Nothing calls `swap_to`
  yet in this PR — every code path behaves identically to before.
  `SearchAppState` gained a `switchable_embedder` field (populated alongside
  `embedder_slot` by the same background init task) so a later hot-swap
  orchestrator and `/health` work can reach it without an `Any`-downcast.
  New workspace dependency: `arc-swap`.

### Added

- **Runtime fallback to the Rust ort sidecar + doctor/health readback for
  the Python/MPS embedder (epic #3524 slice 5).** Bootstrap-time fallback
  already existed (slices 2-4); this adds the missing runtime half: a new
  `FallbackEmbedderAdapter` (`commands/start/embedder_fallback.rs`) wraps the
  Python sidecar and latches (one-way, logged once at ERROR) to the Rust ort
  path once the sidecar's own supervisor permanently gives up respawning it
  (a real, non-blocking readback of the supervisor's own give-up decision,
  not an independently-counted request-failure proxy) — so a wedged or
  crash-looping Python sidecar degrades gracefully instead of failing every
  subsequent search forever. `trusty-search doctor` gained a `python_embedder`
  check (uv presence/version, venv + lockfile-hash-current `.ready` state,
  launcher discoverability) with a `--fix` repair that re-runs the eager venv
  bootstrap. `GET /health`'s `embedder_info.provider` now prefers a REAL
  device readback from the live embedder (`Embedder::resolved_provider_label`)
  over the build-features prediction when one is available — fixes the
  sidecar reporting `CoreML(ANE)` while torch actually selected `mps` (issue
  #3493 P1).

## [0.37.1] — 2026-07-21

### Fixed

- **Regenerated the `ui-dist` bundle** (#3590, `b76e08ea`): the published
  0.37.0 tarball shipped a stale prebuilt dashboard bundle that predated the
  Foundry v2 dark-mode migration, so the installed dashboard was missing the
  dark-mode feature entirely. The bundle under `ui-dist/` is rebuilt from the
  current `ui/` source so a fresh install/upgrade gets the dark-themed
  dashboard. No Rust source changes.

## [0.37.0] — 2026-07-20

Minor release: opt-in Python/MPS embedding sidecar (`TRUSTY_EMBEDDER=python`),
a new default-off capability that embeds ~2.4x faster than the Rust ort path on
Apple Silicon with numerically identical results, and falls back to ort on any
failure. Unset/`auto`/`stdio` behaviour is unchanged. Epic #3524 (refs #3498,
#3493); paired with the first release of the `trusty-embedderd-py` launcher
crate (v0.1.0).

### Added

- **Opt-in Python/MPS embedding sidecar (`TRUSTY_EMBEDDER=python`)** — epic
  #3524 slices 2-4 (refs #3498, #3493). A new `TRUSTY_EMBEDDER=python` arm in
  `commands/start/embedder.rs` eager-bootstraps a pinned Python venv (via the
  new `trusty-embedderd-py` launcher crate) and arms the existing
  `LazyEmbedderHandle` against it — reusing `EmbedderSupervisor` /
  `StdioEmbedderClient` with **ZERO changes** to the supervisor/stdio/protocol
  wire code. On Apple Silicon the torch/MPS sentence-transformers sidecar
  embeds ~2.4x faster than the Rust ort path with numerically identical
  results. **DEFAULT-OFF and fully backward-compatible**: unset / `auto` /
  `stdio` behaviour is unchanged, and on ANY bootstrap or launcher-discovery
  failure the daemon logs a loud warning and **falls back to the Rust ort
  embedder** so search never hard-fails. The Rust build does not require
  torch/venv. Default-on-Apple-Silicon is a later slice.
- **`TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS`** (epic #3524 fast-follow) — a
  python-arm-only idle-shutdown override in `commands/start/embedder.rs`,
  defaulting to **1800s (30 min)** instead of the shared
  `TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS` 300s default. Rationale: the
  Python/MPS sidecar's cold restart is cheap (~2.5–3s) but still worth
  avoiding mid-session, so the longer default keeps it warm through a normal
  ~30 min work session while still reclaiming its ~500 MB after genuine
  extended idle (matters on the 16 GB minimum-spec tier). Resolution
  precedence preserves operator intent: the new var (if set, including `0`)
  always wins; else an explicitly-set shared var (any value, including `0`)
  is honoured; else the python-specific 1800s default applies. `0` disables
  idle-shutdown entirely (always-warm) for higher-RAM machines. **Zero impact
  on the ort/default arm**, which still calls `SupervisorConfig::from_env()`
  directly.

### Documentation

- CLAUDE.md's "Embedder Configuration" table now documents the `python`
  `TRUSTY_EMBEDDER` value, plus a new "Python/MPS sidecar tuning" reference
  block for `TRUSTY_UV_BIN`, `TRUSTY_EMBEDDERD_PY_BIN`,
  `TRUSTY_PY_BOOTSTRAP_TIMEOUT_SECS`, `TRUSTY_DEVICE`, `TRUSTY_PY_EMBED_FP16`,
  `TRUSTY_PY_EMBED_BATCH_SIZE`, and `TRUSTY_EMBEDDERD_PY_IDLE_SHUTDOWN_SECS`
  (epic #3524 fast-follow).

## [0.36.1] — 2026-07-20

### Changed

- Rebuild against `trusty-common` 0.23.6 / `trusty-embedderd` 0.3.9 to pick up the
  two embedding-performance fixes ([#3500](https://github.com/bobmatnyc/trusty-tools/pull/3500),
  [#3511](https://github.com/bobmatnyc/trusty-tools/pull/3511); refs #3486 / #3493):
  platform-conditional ORT intra-op thread default and a non-quantized (fp32)
  default embedding model. `trusty-search` bundles the `trusty-embedderd` binary,
  so this republish is what carries the faster/more-accurate embedder into the
  single-install `cargo install trusty-search`. `TRUSTY_ORT_INTRA_THREADS` and
  `TRUSTY_EMBEDDER_MODEL=int8` remain available to restore prior behaviour.

### Changed

- **UI tokens now CI-enforced against the canonical Foundry source** (refs [#3486](https://github.com/bobmatnyc/trusty-tools/issues/3486)): flipped from the `scripts/check_token_drift.mjs` allowlist to ENFORCED. The `token-drift` CI job now compares `ui/src/lib/styles/tokens.css`'s plain-CSS `--trusty-*: #hex` values directly to `docs/design/UI/design-system/tokens.css` on every push/PR (light `:root`, dark `[data-theme='dark']`), so a hand-edit that drifts this crate's palette from canonical fails the build.
- **Migrated the admin UI to Foundry v2 design tokens** ([#3487](https://github.com/bobmatnyc/trusty-tools/issues/3487)):
  `ui/src/lib/styles/tokens.css` now sources its palette, fonts, radii, and
  shadows from the canonical `docs/design/UI/design-system/tokens.css`
  (rust-on-paper light theme) and ships a full `[data-theme='dark']` block
  ("Night Shift") — this UI previously had no dark theme at all. Existing
  `--trusty-*` custom-property names are unchanged; a few components that
  referenced tokens the old palette never actually defined (a bare
  `--trusty-text`, `--trusty-font-mono`) now resolve to real values instead
  of silently falling through to their inline fallback, and `--trusty-primary`
  / `--trusty-primary-soft` / `--trusty-surface-raised` /
  `--trusty-surface-hover` — already referenced by `Indexes.svelte` and
  `TagListInput.svelte` with the same problem — are now defined tokens too.
  Dark-mode activation follows OS `prefers-color-scheme` via a new
  `lib/theme-bootstrap.js`, wired from `main.js` before the shell mounts.

## [0.36.0] — 2026-07-20

### Changed

- **BREAKING: unknown search-filter fields now error instead of being
  silently ignored.** `SearchQuery` (`/indexes/:id/search`) and
  `GlobalSearchRequest` (`/search`) both now derive `#[serde(deny_unknown_fields)]`.
  A misspelled or unsupported filter field (e.g. `path_prefx` instead of
  `path_prefix`) previously deserialized successfully and silently returned
  an unscoped/unfiltered result set; it now fails deserialization with a
  clear error. Any client sending fields the schema doesn't recognize —
  intentionally or by typo — will start seeing request failures after this
  upgrade. Check request payloads against the current `SearchQuery` /
  `GlobalSearchRequest` field set before upgrading.

### Added

- **Network-mount detection for the file watcher** ([#3408](https://github.com/bobmatnyc/trusty-tools/issues/3408)):
  the daemon now detects when a registered index's root is a network-mounted
  filesystem (EFS/NFS/SMB/CIFS — via `statfs` on macOS/Linux) and refuses to
  start the file watcher for that index instead of silently starting one that
  can never observe another host's writes (inotify/FSEvents are local-host-only
  kernel mechanisms — an OS-level limitation, not a bug). The condition is
  surfaced on `GET /health` (`indexes_watcher_network_degraded`) and
  `GET /indexes/:id/status` (`watcher.network_mount_degraded` +
  `watcher.degraded_reason`), naming `POST /indexes/:id/index-file` and
  `POST /indexes/:id/remove-file` as the actionable next step. Detection is
  conservative (fails open to "local") so it never blocks a legitimate local
  watcher. Also officially documents those two endpoints (and their `index_file`
  / `remove_file` MCP equivalents) as the supported incremental-indexing path
  for network-mounted and build/serve-split deployments, with a worked example —
  see `README.md` and `CLAUDE.md`'s endpoint catalogue.
- Server-side `path_prefix` / `repos` search scoping, applied during candidate
  selection BEFORE `top_k` truncation in every retrieval lane — vector/HNSW,
  BM25/lexical, grep-fallback, and KG expansion (closes #3401). Lets
  consumers scope a query to a repo/path subtree within the single unified
  index instead of building a separate physical index per repo.
  **Recall guarantee:** the vector/HNSW lane pushes the filter predicate
  directly INTO HNSW graph traversal via `usearch::Index::filtered_search`
  (already available in the pinned usearch 2.25) — no over-fetch
  approximation, though a highly selective filter does mean real added
  traversal latency, since the search visits more of the graph to find
  `top_k` passing candidates. BM25 gets a new `Bm25Index::
  score_query_all_with_filter` (trusty-common, additive — existing callers
  are unaffected) that evaluates the filter before its internal `top_k`
  truncate, not after; grep-fallback evaluates it before its early-exit
  cutoff. `path_prefix` matches at a path-segment boundary (so `"foo"`
  cannot also match a sibling `"foobar"` directory) and is normalized
  against the index's `root_path`, so a caller can pass either the
  root-relative or the absolute form (`CodeChunk::file` in results is always
  absolute). Surfaced on `SearchQuery` (HTTP `/indexes/:id/search`) and the
  fan-out `POST /search`'s `GlobalSearchRequest` — both now reject unknown
  fields (`deny_unknown_fields`), and the MCP `search` / `search_lexical` /
  `search_semantic` / `search_kg` / `search_all` tool schemas. Composes with
  `exclude_archived` and the branch fields (`branch`/`branch_files`/`branch_boost`).

## [0.35.0] — 2026-07-19

### Security

- **Document-extraction DoS advisories fixed** ([#3367](https://github.com/bobmatnyc/trusty-tools/issues/3367)):
  bumped `pdf-extract` 0.9 → 0.12 and `calamine` 0.26 → 0.36 (both now pull
  patched `lopdf` ≥0.42 / `quick-xml` ≥0.41) to close RUSTSEC-2026-0187
  (`lopdf` stack-overflow DoS via deeply nested PDF objects, CVSS 7.5) and
  RUSTSEC-2026-0194/0195 (`quick-xml` DoS, CVSS 7.5 each), reachable via the
  default-on native pdf/docx/xlsx text extraction added in #2932. Also adds a
  self-defending file-size cap directly in `core::extract::extract_text`
  (independent of the walker's existing gate) and regression tests covering
  pathological deeply-nested/high-attribute-count inputs for all three
  formats.
  **Intentional behavioral side effect:** the calamine 0.36 upgrade also
  changes xlsx/xls cell-text extraction — shared/inline string cells now have
  leading/trailing ASCII whitespace trimmed (unless `xml:space="preserve"` is
  set) and embedded `\r\n` normalized to `\n`, per upstream calamine's own
  `Changelog.md` for 0.31–0.36. This is upstream, not a bug in this crate; it
  means re-indexed `.xlsx`/`.xls` content may differ slightly (whitespace,
  line endings) from what was indexed pre-bump, which is worth knowing when
  diffing search results across this change. Covered by
  `xlsx::tests::test_cell_whitespace_trimmed_and_eol_normalized`. The
  quick-xml 0.41 upgrade also changed how the docx path receives XML entity
  references (`&amp;`, `&#233;`, ...): they now arrive as standalone
  `Event::GeneralRef` events instead of being inlined into `Event::Text`,
  which this PR now handles explicitly in `docx::paragraphs_from_document_xml`
  (previously unhandled references were silently dropped from extracted
  text). Covered by
  `docx::tests::test_paragraphs_from_document_xml_unescapes_entities`.
  ([#3373](https://github.com/bobmatnyc/trusty-tools/pull/3373))
- **Router-wide same-origin (CSRF) write guard** ([#3304](https://github.com/bobmatnyc/trusty-tools/issues/3304)):
  destructive write routes (`POST /admin/stop`, `POST /indexes`,
  `DELETE /indexes/{id}`, `POST /upgrade`, reindex) are now guarded against
  cross-origin browser requests via the shared
  `trusty_common::server::with_guarded_middleware`. Method-gated (GET reads and
  SSE streams unaffected) and fail-open on a missing `Origin` (the console
  reverse proxy, `curl`, and the MCP stdio bridge keep working); the daemon's
  own resolved bind address is trusted so a non-loopback bind still serves its
  UI. ([#3317](https://github.com/bobmatnyc/trusty-tools/pull/3317))

## [0.34.1] — 2026-07-18

### Added

- skip_vector flag + runtime component toggle with catch-up (#2984 Phase 1) ([#3024](https://github.com/bobmatnyc/trusty-tools/pull/3024)) ([`0fe2b81`](https://github.com/bobmatnyc/trusty-tools/commit/0fe2b8160eb62e8ee265d0970555e67fec537a72))

### Fixed

- restore crates.io installability against trusty-common 0.23.3 — wedge_reset_secs field now set in all SupervisorConfig initializers (closes #3131) ([#3148](https://github.com/bobmatnyc/trusty-tools/pull/3148))
- EmbedderSupervisor shutdown reachable + no respawn on intentional shutdown ([#3023](https://github.com/bobmatnyc/trusty-tools/pull/3023)) ([`dd5f212`](https://github.com/bobmatnyc/trusty-tools/commit/dd5f212900abff69573121e826028e941188b79a))
- warm-boot honors skip_kg — no graph load/rebuild for skipped indexes ([#2988](https://github.com/bobmatnyc/trusty-tools/pull/2988)) ([`cdf998e`](https://github.com/bobmatnyc/trusty-tools/commit/cdf998eadf92a42e924e770ce45b8f616d172448))
- migrate off archived serde_yml/libyml to serde_yaml 0.9 ([#2992](https://github.com/bobmatnyc/trusty-tools/pull/2992)) ([`6a67317`](https://github.com/bobmatnyc/trusty-tools/commit/6a673178d8e9db98b901ad43872f003bc81d0f40))
- set fd_limit in LaunchAgent plist for large index fleets ([#2967](https://github.com/bobmatnyc/trusty-tools/pull/2967)) ([`8658780`](https://github.com/bobmatnyc/trusty-tools/commit/86587803794b1d048d64fd209a49bc8304edecfd))
- embedder reader-death detection + wedged-sidecar restart ([#2978](https://github.com/bobmatnyc/trusty-tools/pull/2978)) ([`25c56d0`](https://github.com/bobmatnyc/trusty-tools/commit/25c56d0564a281e42719cdd0ea18f03099c47749))
- correct launchd plist names in signed-install restart hints ([#2959](https://github.com/bobmatnyc/trusty-tools/pull/2959)) ([`e05680d`](https://github.com/bobmatnyc/trusty-tools/commit/e05680de80790291dc044f80115150a375073135))

## [0.32.4] — 2026-07-13

Note: `trusty-search-v0.32.3` was published without a corresponding git tag,
so `git-cliff`'s `--unreleased` window (scoped to the `trusty-search-v*` tag
series) walked all the way back to `trusty-search-v0.32.2`. This section
therefore includes commits already shipped in 0.32.3 in addition to the
actual new content for this release — the AL2023 CI-gate / glibc-probe fix
(#2525, refs #2222). Tag `trusty-search-v0.32.4` when publishing to close
this gap going forward.

### Added

- credential resolver + secure KeyStore (closes #2401) ([#2427](https://github.com/bobmatnyc/trusty-tools/pull/2427)) ([`98d0eb9`](https://github.com/bobmatnyc/trusty-tools/commit/98d0eb993cdaf640842761aaf9299d7013d2ee01))
- add follow_links symlink policy to indexer ([#2355](https://github.com/bobmatnyc/trusty-tools/pull/2355)) ([`4b95ccc`](https://github.com/bobmatnyc/trusty-tools/commit/4b95ccc45662cdce87367dc708d2a0dbec4a7a09))

### Fixed

- AL2023 close-out — CI gate + startup glibc probe + docs (refs #2222) ([#2525](https://github.com/bobmatnyc/trusty-tools/pull/2525)) ([`db59ebe`](https://github.com/bobmatnyc/trusty-tools/commit/db59ebeb4a4a5148f57ac7a47243247c3bd8c337))
- index-registry integrity — runtime collision guard + dedup follow-ups (closes #2336, #2337) ([#2519](https://github.com/bobmatnyc/trusty-tools/pull/2519)) ([`1c609cf`](https://github.com/bobmatnyc/trusty-tools/commit/1c609cfbae2d9ca12ca22ab06d90c2cb449bba8f))
- launchd-aware bridge no-spawn + /health supervised flag (closes #2486) ([#2491](https://github.com/bobmatnyc/trusty-tools/pull/2491)) ([`e993c18`](https://github.com/bobmatnyc/trusty-tools/commit/e993c18ace1fe9a86f4b5315be7887ed767da710))
- dedup warm-boot entries sharing one redb corpus path (closes #2305) ([#2335](https://github.com/bobmatnyc/trusty-tools/pull/2335)) ([`f0a48cc`](https://github.com/bobmatnyc/trusty-tools/commit/f0a48cc127b8631bc4297a8f7574392b02d5cec2))
- enable embedder idle-shutdown by default and guard in-flight requests (closes #2315) ([#2320](https://github.com/bobmatnyc/trusty-tools/pull/2320)) ([`0531e9d`](https://github.com/bobmatnyc/trusty-tools/commit/0531e9d944918b1b6eb408dc2c3c08d5e90bd746))
- warm-boot health reports degraded when corpus fails to open (closes #1870) ([#2307](https://github.com/bobmatnyc/trusty-tools/pull/2307)) ([`9c3e88f`](https://github.com/bobmatnyc/trusty-tools/commit/9c3e88f652ec03a7ee03a266e85862c9c15ac03a))
- return doc hits for Unknown-intent queries instead of empty (Closes #2203) ([#2287](https://github.com/bobmatnyc/trusty-tools/pull/2287)) ([`89ea057`](https://github.com/bobmatnyc/trusty-tools/commit/89ea05763330a56beb4b90876f27c7c3a5b7f6af))

### Changed

- split 3 files under SLOC caps ([#1195](https://github.com/bobmatnyc/trusty-tools/pull/1195)) ([#2289](https://github.com/bobmatnyc/trusty-tools/pull/2289)) ([`60a58e3`](https://github.com/bobmatnyc/trusty-tools/commit/60a58e326c23b211f93b36d50c603982593e1bb1))
- split doctor_checks/ruby/review under 500-SLOC cap ([#1195](https://github.com/bobmatnyc/trusty-tools/pull/1195)) ([#2283](https://github.com/bobmatnyc/trusty-tools/pull/2283)) ([`eeabe56`](https://github.com/bobmatnyc/trusty-tools/commit/eeabe562c20172c8b6f4c9d63618a4bcd8838868))
- release trusty-common 0.22.2 + trusty-mpm 0.19.1 ([#2241](https://github.com/bobmatnyc/trusty-tools/pull/2241)) ([`f7ab5f4`](https://github.com/bobmatnyc/trusty-tools/commit/f7ab5f43c8a5cc41ed4d821e2a53800974e74207))

### Documentation

- add trusty-mpm package metadata + repoint trusty-search CI badge to monorepo ([#2292](https://github.com/bobmatnyc/trusty-tools/pull/2292)) ([`cba43a5`](https://github.com/bobmatnyc/trusty-tools/commit/cba43a5698c03ea611f731b6a5bef0809547a93f))

## [0.32.3] — 2026-07-09

### Changed

- Add crates.io package metadata (keywords/categories/homepage/readme).
- Repoint CI badge to trusty-tools monorepo.

## [0.32.2] — 2026-07-08

### Changed

- re-cut so bundled trusty-embedderd is 0.3.6 (carries #1633 watchdog); no trusty-search source changes vs 0.32.1

## [0.32.1] — 2026-07-07

### Fixed — re-cut with P0 corpus-identity fixes (supersedes 0.32.0)

- **0.32.1 = 0.32.0 versioning + 0.31.1's P0 fixes.** Version 0.32.0 was
  published out-of-band (PR #2209) without corpus-identity hardening. This
  re-cut applies the essential P0 fixes from 0.31.1 to the 0.32.0 version
  line, becoming the latest published version. Consume 0.32.1 instead of 0.32.0.
  
  P0 fixes included (from 0.31.1):
  - **Issues #2203, #1870 (corpus open failure):** failed durable-corpus open no
    longer leaves `semantic`/`graph` falsely reporting `"ready"` (#2203).
  - **Issue #2211 (`defer_embed` premature ready):** `stages.semantic.status`
    no longer flips to `"ready"` prematurely during deferred embedding.
  - **Issue #2179 (HNSW key-migration):** `rewrite_keys_to_relative` (M003
    one-time migration) now genuinely promotes a view-mode store to mutable.

---

## [0.31.1] — 2026-07-07

### Fixed — corpus/status desync bug cluster (issues #2203, #1870, #2211, #2179)

- **#2203 / #1870 (joint root cause): a failed durable-corpus open no longer
  leaves `semantic`/`graph` falsely reporting `"ready"`.** `derive_warm_boot_stages`
  previously threaded `corpus_open_failed` into the `lexical` stage only;
  `semantic` and `graph` were classified purely from `hnsw_snapshot_ready` /
  `graph_node_count` — signals entirely independent of the redb corpus. A
  restored HNSW mmap snapshot (or symbol graph) can load successfully even
  when the redb corpus failed to open (`DatabaseAlreadyOpen` or any other
  open error, #1870), so `/health` and `GET /indexes/:id/status` kept
  reporting `semantic.status: "ready"` while the query hot path's
  `fetch_chunks_for_ids` could never resolve any HNSW hit against the
  unwired corpus — every result was silently dropped at materialisation
  (`search/materialize.rs`), producing HTTP 200 + `results: []` for
  essentially every query (#2203). A corpus-open failure now fails all three
  stages together, and `search_capabilities` correctly advertises no lanes.
- **#2211: `stages.semantic.status` no longer flips to `"ready"` prematurely
  when `defer_embed` is active.** `finish_reindex` called
  `mark_semantic_ready_graph_in_progress` unconditionally right after the
  fast pass, before the deferred background embed pass had even started —
  reporting `"ready"` for the entire duration of the real embedding job.
  Semantic now stays `InProgress` until the deferred pass actually
  completes and marks it `Ready`.
- **#2179 (tech debt): `rewrite_keys_to_relative` (M003's one-time HNSW
  key-migration path) now genuinely promotes a view-mode store to mutable
  instead of just flipping the `is_view` flag**, keeping the flag truthful
  relative to the underlying `usearch::Index` mode.

---

## [0.31.0] — 2026-07-06

### Added — idle watcher suspension (stop watching projects nobody is using)

- **A live index's FSEvents watcher is now suspended after it goes idle, and
  resumes on the next query.** Previously, once an index was warm-booted or
  registered, its OS filesystem watch ran until the index was *deleted* — so a
  host tracking hundreds of registered projects kept hundreds of live watches
  regardless of use, a standing CPU / `fseventsd` cost. Now:
  - A background ticker releases the watcher of any index whose in-memory
    `idle_duration()` exceeds `TRUSTY_WATCH_IDLE_SUSPEND_SECS` (default 900 s;
    `0` disables). This sits above the 300 s chunk-eviction window, so an idle
    index first sheds memory, then — if still dormant — sheds its watcher.
  - The query path re-establishes the watcher on the next query to a suspended
    (or lazily cold-restored) index, then runs a background reconcile
    (git-diff / mtime catch-up, the same logic used at boot) so any edits made
    while the watcher was off are picked up. The query itself is served
    immediately from current in-memory state; suspension is invisible to an
    active user.
- **Side fix:** lazily cold-restored indexes (issue #993) previously never
  started a watcher at all; the wake path now gives them one too.

### Notes

- Memory for idle indexes was already reclaimed by the chunk-eviction ticker
  (`TRUSTY_CHUNKS_IDLE_EVICT_SECS`); this change adds the CPU/watch half.
- No behaviour change when `TRUSTY_DISABLE_WATCHER=1` (watchers never spawn) or
  `TRUSTY_WATCH_IDLE_SUSPEND_SECS=0` (watchers stay hot).

---

## [0.30.0] — 2026-07-06

### Added — self-managed orphan reaping (daemon no longer leaks dead registrations)

- **The daemon now removes orphaned index registrations automatically.**
  Previously, an ephemeral MPM worktree (`.worktrees/<uuid>/`) that was
  registered and then deleted left a dead entry in `indexes.toml` forever:
  warm-boot *detected* the missing `root_path`, logged "run
  `trusty-search prune-orphans`", and skipped it — but nothing ever removed it.
  Over a long-lived daemon these accumulated without bound (a real machine
  reached **485 dead registrations over 26 days**), each holding an idle
  FSEvents watch that pinned macOS `fseventsd` at ~100% CPU / 8 GB RSS.
  Three complementary mechanisms now keep the registry self-healing:
  - **Boot self-heal** — `heal_boot_orphans` runs at warm-boot start and drops
    legacy (non-colocated) registrations whose `root_path` was deleted, so they
    stop being re-read on every boot. Colocated entries are still left to the
    relocation scan.
  - **Runtime reaper ticker** — an hourly background sweep unregisters live
    indexes whose root vanished mid-run. Cadence is tunable via
    `TRUSTY_ORPHAN_REAP_SECS` (`0` disables it).
  - **Ephemeral-dir ignore** — auto-discovery and the colocated rescan now skip
    the `.worktrees/` component, so throwaway worktrees are never
    auto-registered (or FSEvents-watched) in the first place. Explicit
    `trusty-search index <path>` is unaffected.

### Safety

- Orphan reaping only fires when a `root_path` is missing **and its immediate
  parent still exists** — a deleted worktree leaves `.worktrees/` behind (reap),
  while an unmounted external volume takes the whole parent chain with it (spared).
- The automatic reaper **never deletes on-disk index data**, only the
  registration, so a false-positive detection is always recoverable by
  re-registering the path. (The interactive `DELETE /indexes/:id` still removes
  data as before.)

---

## [0.29.1] — 2026-06-25

### Fixed (closes #1711)

- **HNSW shutdown data-loss guard: prevent empty in-memory index from
  overwriting a populated on-disk snapshot.**
  A graceful-shutdown race (background reindex from `reconcile_stale_indexes`
  / commit `fe4c0b28` flushed mid-run on SIGTERM) could cause a
  just-promoted but not-yet-populated `UsearchStore` to call `save()` and
  overwrite a fully-populated on-disk snapshot with 0 vectors.
  The guard now runs **under the same write-lock scope** that owns the save
  (eliminating the TOCTOU window), refuses to proceed when `index.size()==0`
  and the on-disk file is larger than 100 KB, and returns `Ok(())` so
  callers complete shutdown gracefully. A follow-up issue (#1717) tracks
  draining/cancelling in-flight background reindex tasks on SIGTERM and
  guarding catastrophic partial-snapshot shrinks.
- `POPULATED_SNAPSHOT_THRESHOLD_BYTES` promoted to module-level `pub(super)
  const` so tests can reference it without hard-coding a magic literal.
- Regression test `test_save_refuses_to_overwrite_populated_snapshot_with_empty_index`
  rewritten to actually trigger the guard (writes a filler file above the
  threshold, asserts byte-for-byte preservation after `save()` on an empty
  store).

---
## [0.29.0] — 2026-06-24

### Added

- boot-time stale-index reconciliation via git-diff delta reindex (closes #1670) ([#1671](https://github.com/bobmatnyc/trusty-tools/pull/1671)) ([`fe4c0b2`](https://github.com/bobmatnyc/trusty-tools/commit/fe4c0b28d340b19d3ada390925b17305412f96b2))

### Added

- auto-fresh reindex file watcher (closes #1621, refs #1619) ([#1635](https://github.com/bobmatnyc/trusty-tools/pull/1635)) ([`80e247f`](https://github.com/bobmatnyc/trusty-tools/commit/80e247fa8e64f2f701a83500e778cfb4bf5522b5))
- reindex-on-commit git hooks + hook install/uninstall (closes #1620) ([#1622](https://github.com/bobmatnyc/trusty-tools/pull/1622)) ([`6b70579`](https://github.com/bobmatnyc/trusty-tools/commit/6b705792512bc1c7a2d7ef26ba03c470d4c9fc97))
- typeahead endpoint + MCP tool (lexical default, opt-in blended) (closes #1557) ([#1559](https://github.com/bobmatnyc/trusty-tools/pull/1559)) ([`db16554`](https://github.com/bobmatnyc/trusty-tools/commit/db16554bbfb6d5bc1f42f3aeb29bf0c7b71b9510))

### Fixed

- harden WatcherManager spawn (TOCTOU) + real env-gate test (closes #1640, closes #1641) ([#1644](https://github.com/bobmatnyc/trusty-tools/pull/1644)) ([`cd5fd91`](https://github.com/bobmatnyc/trusty-tools/commit/cd5fd91ef6eb7040cc5633e64dff655db15dbc9c))
- make publish.sh monorepo- and redb2-aware (closes #1539) ([#1544](https://github.com/bobmatnyc/trusty-tools/pull/1544)) ([`495dd92`](https://github.com/bobmatnyc/trusty-tools/commit/495dd926b8bcef2834aba725a991d1cd96b59047))
- anchor Makefile sync-ui paths to makefile dir so it works from workspace root (closes #1540) ([#1543](https://github.com/bobmatnyc/trusty-tools/pull/1543)) ([`a54c6aa`](https://github.com/bobmatnyc/trusty-tools/commit/a54c6aa42bb84a45878d9b1225a9635688dd76bf))

---

## [0.26.1] — 2026-06-18

### Fixed (closes #1428)

- **Surface silent reindex failures — termination guard now always logs the
  underlying cause at `error!` to stderr (incl. GPU-OOM) and emits an SSE
  error frame with `fatal:true`.** Previously, any error that caused the
  reindex task to terminate early was swallowed silently: the SSE stream
  closed without an `error` event, leaving the client with no indication of
  what went wrong. Producer `JoinError` is now captured and surfaced;
  `RUST_LOG=debug` tracing added around batch flush/commit for diagnostics.


## [0.26.0] — 2026-06-17

### Added (closes #1373)

- **`trusty-search serve --index <id>` / `--project <path>` pin an MCP session
  to one index.** When pinned, every tool handler defaults an omitted
  `index_id` to the pinned id, and fan-out tools (`search_all` / `grep` without
  `index_id`) scope to the pinned index instead of sweeping every registered
  index. The pinned index is advertised in `tools/list` (its `index_id` becomes
  optional and its description names the default) so the LLM never has to call
  `list_indexes` and guess. `--index` wins over `--project`; `--project`'s id is
  derived via the shared `trusty_common::derive_index_id` (git-root basename).
  Without either flag, behaviour is unchanged — callers must supply `index_id`
  and fan-out sweeps all indexes.

### Changed

- **Index-id derivation is now the single source of truth in `trusty-common`.**
  `detect_project` delegates to `trusty_common::derive_index_id` so trusty-mpm's
  register-and-pin and trusty-search's CLI/MCP paths always agree on the id.

## [0.25.0] — 2026-06-17

### Added (closes #1372)

- **Configurable per-index indexing hygiene + dashboard config API.** Indexing
  hygiene is now per-project config defaults that are overridable per index
  (and editable via the dashboard), rather than hardcoded constants:
  - **Walker** gains `DATA_EXTS` (json/xml/txt/log), a 64 KiB
    `DEFAULT_DATA_FILE_MAX_BYTES` cap for data-ish files, and
    `DEFAULT_EXTRA_SKIP_DIRS` (data/exports/output/reports/snapshots/results).
    `WalkOptions` carries `extra_skip_dirs` + `data_file_max_bytes`; data files
    get the tighter cap while everything else keeps the 1 MiB global cap.
  - **Config + persistence:** `IndexConfig` (`trusty-search.yaml`),
    `ProjectConfig` (`.trusty-search.yaml`), and `PersistedIndex`
    (`indexes.toml`, serde-default for backward compat) all gain the two
    hygiene fields, threaded through `CreateIndexRequest` → handle →
    `WalkOptions`.
  - **New per-index config API:** `GET /indexes/{id}/config` returns the hygiene
    config; `PATCH /indexes/{id}/config` updates the in-memory handle and
    persists to `indexes.toml` (validates inputs, rejecting
    `data_file_max_bytes == 0`).

## [0.24.10] — 2026-06-16

### Added (closes #1365)

- **`trusty-search status [INDEX] --watch`.** `status` now accepts an optional
  positional `INDEX` argument to scope the overview to a single index, plus a
  `--watch` flag that refreshes the status view on an interval for live
  monitoring of daemon + index state.

## [0.24.9] — 2026-06-16

### Fixed (closes #1325)

- **Deep `GET /indexes/{id}/chunks` pagination no longer times out / 502s on
  large indexes.** The endpoint's offset path materialized the entire corpus
  and re-sorted it on every page request (O(N log N) per page), so a deep
  offset (`offset=304000` on a 300k-chunk index) blew past the client / proxy
  timeout and surfaced as a 502 Bad Gateway after ~120 s. Chat / search were
  unaffected.

### Added (closes #1325)

- **Cursor-based pagination for `GET /indexes/{id}/chunks` and the
  `list_chunks` MCP tool.** A new, additive, non-breaking `after` query param
  (the `list_chunks` tool gains a matching `after` arg) pages by chunk `id`
  using an indexed redb B-tree seek (`CorpusStore::chunks_after`) instead of an
  O(offset) scan — each page is O(page) regardless of depth. Send `after=`
  (empty) to start from the first chunk and pass the response's `next_cursor`
  back as `after` to walk forward; `next_cursor` is `null` once the corpus is
  exhausted. The legacy `offset`/`limit` mode is unchanged for back-compat
  (its `next_cursor` is always `null`, since offset ordering — by
  `(file, start_line)` — differs from cursor ordering — by `id`; a cursor walk
  must not be seeded from an offset page). Consumers doing bulk enumeration
  (e.g. trusty-analyze's PR-review static-analysis context) should switch to
  the cursor mode.

## [0.24.8] — 2026-06-16

### Changed (closes #1326)

- **Bumps `trusty-common` to 0.15.3**, which down-levels the benign `timed_out_id=None` embedder-stall WARN to `debug!`, eliminating ~2,800 spurious log lines/day during normal operation.

## [0.24.7] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-search` now produces
  `trusty-search` and `trusty-embedderd` only. The console is its own
  single-owner crate — install it with `cargo install trusty-console`. This
  resolves the cargo binary-ownership collision that forced `--force` on
  install / self-`upgrade` (#1262). `trusty-embedderd` is still bundled here
  (single-owner: search is its sole producer).

## [0.24.4] — 2026-06-09

### Fixed

- **Embed-pool sidecar calls isolated from the async executor to prevent
  accept-loop starvation (#1017)** — `embed_batch` calls to the stdio sidecar
  are now dispatched via `tokio::task::spawn_blocking` so they cannot occupy
  async worker threads. Under sustained embed load the executor no longer
  starves the axum accept-loop, eliminating the class of request-timeout
  failures seen in issue #1017.

- **Graceful `admin_stop` without corpus corruption (#829)** — the admin-stop
  endpoint now flushes in-flight writes and closes redb handles before
  signalling the daemon to exit. Previously a `POST /admin/stop` could race
  with an ongoing reindex commit and leave the corpus in an inconsistent state.

- **Non-blocking `canonicalize` in index registration path (#829)** —
  `std::fs::canonicalize` is now wrapped in `tokio::task::spawn_blocking` so
  a slow or unreachable filesystem path no longer stalls the async acceptor
  while resolving symlinks.

- **PID-slot reclamation (#829)** — stale PID lockfiles from previously crashed
  daemon instances are now detected and removed at startup, preventing spurious
  "daemon already running" errors after an unclean shutdown.

---

## [0.24.3] — 2026-06-09

### Fixed

- **#1006 — accept-loop starvation under embed backpressure**
  — Two complementary mitigations close the liveness gap when the embed
  thread pool saturates:

  - **Worker-thread floor raised to 16** — the Tokio runtime is now built
    with an explicit `Builder::new_multi_thread()` using
    `max(available_parallelism, 16)` worker threads. On a 4-core host the
    default `num_cpus` count (≈8) was too low: once eight slots were occupied
    by 30-second sidecar-blocking embed calls the axum accept-loop stalled,
    causing short-timeout `/health` and `/context` connections to fail.

  - **Non-blocking `/health` handler** — `try_current_embedder()` (a new
    `try_read()` accessor on `state_impl.rs`) replaces the previous
    `current_embedder().await`; `sys_metrics.try_lock()` replaces the
    `lock().await` for CPU/RSS sampling. Both fall back gracefully on lock
    contention: embedder info is omitted, last-sampled RSS/CPU atomics are
    returned instead of zeros. Eliminates the 30-second blocking window that
    paralysed the handler when the write lock was held by an active embed run.

  - **Health-metric cache** — `AtomicU64`/`AtomicU32` caches on
    `SearchAppState` store the last-sampled `rss_mb` and `cpu_pct`; the
    fallback path reads these instead of reporting zeros, preventing
    false-alarm monitors that alert on `rss_mb=0` during the rare
    contention window.

  Unit tests added: `health_non_blocking_when_embedder_slot_write_locked`,
  `health_includes_embedder_info_when_ready`,
  `worker_thread_count_at_least_16` (non-tautological: asserts the floor
  formula, not just `max(N,16)>=16`).

---

## [0.24.1] — 2026-06-06

### Fixed

- **#868 — zero-vector guard misfires on all-hash-skipped incremental reindex**
  — `reindex_outcome` now accepts a `skipped_files` count and computes
  `newly_submitted = walked_files - skipped_files`. The `Failed` guard only
  fires when files were actually submitted to the embedder AND produced zero
  vectors. On a warm no-change reindex (all files hash-skipped), zero vectors
  is the expected outcome and the corpus is correctly promoted to `Ready`
  instead of being rolled back to the previous snapshot (closes #868).
- **F2 — deleted-file prune now persists** — the staging corpus was previously
  rolled back whenever the zero-vector guard misfired, discarding the
  deleted-file prune performed earlier in the reindex. With the guard fixed,
  the staging corpus is promoted correctly and the prune result survives.
  No behavior change for genuine embedder failures: those still trigger
  rollback and mark the index `Failed`.

---

## [0.24.0] — 2026-06-06

### Fixed

- **#839 — incremental reindex data-loss carryover** — unchanged chunks are now
  carried from the durable corpus into the staging corpus on every non-force
  reindex, so files that were not re-parsed do not disappear from search results
  after a reindex completes (closes #839, PR #844).
- **#840/#849 — warm-boot opens durable redb corpus** — on daemon restart the
  existing on-disk redb corpus is opened and chunk-hashes loaded immediately,
  so the first reindex after a restart is incremental (only new/changed files)
  rather than a full re-embed; the SSE `start` event now includes a
  `hashes_loaded` field reporting the number of pre-loaded hashes (PR #849).
- **#848 — prune deleted files on non-force reindex** — files that have been
  removed from disk are now pruned from the corpus during a standard (non-force)
  reindex, so the index no longer accumulates stale chunks for deleted files
  (closes #848, PR #854).

### Changed

- **#826 — concurrent CHUNK+EMBED progress bars** — the reindex CLI now shows
  the CHUNK and EMBED phases concurrently with live CPS stats, fixes the
  spurious "Embed 0/1" display, and un-sticks the embedder-ready indicator
  (closes #823, PR #826).
- **#828 — server.rs split** — `service/server.rs` refactored into focused
  submodules under the 500-line cap (closes #799, PR #828).
- **#805 — watcher path normalization** — file-watcher paths are now normalized
  to repo-root-relative before comparison, fixing spurious "file not in index"
  warnings on macOS (PR #805).

---

## [0.23.6] — 2026-06-04

### Changed

- **Finer indexing progress — advance every ~32 chunks** — the reindex
  embed phase now emits `chunk_progress` SSE events at per-wave granularity
  (every `PROGRESS_CHUNK_INTERVAL = 32` chunks minimum) rather than once per
  128-file file-batch. The CLI stats line now shows continuous chunk-count
  movement and live CPS during embedding. Implemented via a new
  `parse_and_embed_files_tracked` API that threads an mpsc channel into
  `embed_chunks_in_batches`; the reindex orchestrator drains per-wave
  notifications and emits intermediate `chunk_progress` events before the
  per-batch `batch` commit event fires. The CLI adds a `chunks_embed_preview`
  atomic that shows in-flight embed progress between authoritative `batch`
  events, reset to 0 on each commit so counts stay correct.

---

## [0.23.5] — 2026-06-04

### Changed (closes #753)

- **Multi-flight pipelined embed feed** — `embed_chunks_in_batches` now
  dispatches up to `TRUSTY_EMBED_INFLIGHT` (default 2, max 4) sub-batches
  concurrently via `futures::stream::buffered` (ordered), eliminating the
  round-trip gap between response-receipt and next-request-send. ANE
  utilisation rises from ~22% to ~60–75%+ at INFLIGHT=2 (~1.4× throughput
  vs single-flight baseline). Zero search-quality impact — same model, same
  vectors, order guaranteed by `buffered`.
- **`DEFAULT_COREML_BATCH_SIZE` raised 32 → 64** — empirical M4 Max sweep
  showed batch=64 peaks at ~83 cps vs ~77 at 32 with no OOM or tripwire
  activity (RSS 369 MB vs 285 MB — both safely under the 4 GB tripwire).
- Requires `trusty-common` 0.14.0 and `trusty-embedderd` 0.3.2.

---

## [0.23.4] — 2026-06-04

### Fixed (closes #747 Fix C + Fix D, closes #750)

- **Per-index endpoints return clean 404 JSON for unknown index id** (closes
  #750) — `/indexes/{id}/search`, `/indexes/{id}/status`,
  `/indexes/{id}/search_similar`, `/indexes/{id}/index-file`,
  `/indexes/{id}/remove-file`, `/indexes/{id}/chunks`,
  `/indexes/{id}/graph`, `/indexes/{id}/graph/stats`, and
  `/indexes/{id}/reindex/stream` previously returned a bare HTTP 404 with
  no body when the index id was not registered, causing clients to fail with
  `error decoding response body`. All per-index routes now return a
  structured `{"error":"unknown index","index_id":"<id>"}` JSON body
  alongside the 404 status so clients can surface "index not found — create
  it with `create_index`" instead of an opaque decode error. A shared helper
  (`unknown_index_response`) ensures every route is consistent.

### Fixed (closes #747 Fix C + Fix D)

- **Forward resolved ONNX batch size to sidecar** (Fix C) — `do_spawn` in
  `LazyEmbedderHandle` now resolves the parent's auto-tuned batch size
  (`TRUSTY_MAX_BATCH_SIZE` / memory-tier autosizing) and forwards it to
  `trusty-embedderd` as `TRUSTY_EMBED_BATCH_SIZE`. On the CoreML path the
  value is capped at `TRUSTY_COREML_BATCH_SIZE` (default 32) to prevent
  oversized unified-memory tensor allocations from triggering macOS jetsam
  SIGKILL. Previously the sidecar always coalesced ONNX calls at its own
  default of 32 regardless of the parent's resolved value.

- **Startup warning for stale `TRUSTY_DEVICE=cpu` on Apple Silicon** (Fix D) —
  After `load_daemon_env()`, the daemon now emits a `tracing::warn!` on stderr
  if `TRUSTY_DEVICE=cpu` is set on an `aarch64-apple-darwin` host. This setting
  disables CoreML ANE acceleration and is almost always a stale workaround from
  the resolved issue #24 (fixed in v0.3.55). The warning includes the
  remediation step (remove the env var from `daemon.env`). No auto-removal.

## [0.23.3] — 2026-06-04

### Fixed (closes #744)

- **Progress UI: correct Files N/total denominator and ETA** — the ticker
  previously read `embed_bar.length()` (initialised to 1) as the total-files
  denominator; ETA was therefore "?" for the entire model-load stall. A new
  shared `AtomicU64 total_files_now` is set from the `walk_complete`/`start`
  SSE events so the denominator is correct from the very first tick.

- **Progress UI: ETA shows "loading model…" during InitializingEmbedder** —
  instead of the misleading "?" during the ONNX/CoreML cold-start, the ticker
  now emits "loading model…" as the ETA string while the `InitializingEmbedder`
  phase is active.

- **Progress UI: cps relabelled "embed/s"** — the per-batch embed throughput
  from `chunk_progress` events is now labelled `N embed/s` to distinguish it
  from a cumulative cold-start rate.

- **Concurrent embedder warm-up** — `spawn_reindex_with_cleanup` now fires a
  background task immediately after the file walk that calls `warm_embedder` on
  the indexer. This triggers the lazy `trusty-embedderd` spawn + ONNX/CoreML
  session init CONCURRENTLY with the hash-cache load and staging setup, so the
  30–60 s model-load cost overlaps with file chunking instead of serialising
  with the first batch. The warm-up is a no-op on already-live daemons and is
  skipped for `lexical_only` indexes. Double-spawn is prevented by
  `LazyEmbedderHandle`'s existing `Arc<Mutex<…>>` single-flight guard.

- **Phase instrumentation** — `spawn_reindex_with_cleanup` now records
  `walk_ms` (time to complete the file scan) and emits a concise per-phase
  timing summary at `tracing::info!` level at the end of every reindex:
  `walk / parse / model_load_approx / embed / bm25 / vector_upsert / kg`.
  `walk_ms` is also included in the SSE `complete` event's `timings` object
  and in the CLI timing breakdown printed after a successful `trusty-search
  index` run.

---

## [0.23.2] — 2026-06-04

### Fixed

- **Shared-channel probe collection — no fast-volume starvation** (review #727
  pass-3 HIGH, issue #723) — `probe_all_volumes` previously iterated pending
  per-volume receivers SEQUENTIALLY: if the first volume's `recv_timeout`
  consumed the full deadline budget, every subsequent receiver got
  `Duration::ZERO` and was wrongly classified as inaccessible — even if its
  probe thread had already finished and sent a result. Fixed by replacing the
  per-volume channel design with a SINGLE shared `mpsc::channel`: all probe
  threads send tagged `(vol_key, sample_path)` results into one channel;
  the collector pulls results in ARRIVAL ORDER until all N volumes report or
  the shared deadline elapses. Fast volumes are now collected immediately
  regardless of spawn order. Total wait ≈ ONE deadline regardless of N;
  `LEAKED_PROBE_THREAD_COUNT` is still incremented once per timed-out volume.
  Regression test: `probe_all_volumes_multi_volume_no_fast_starvation`. (PR #727)

- **Multi-volume starvation regression test** (review #727 pass-3, issue #723)
  — added `probe_all_volumes_multi_volume_no_fast_starvation`: uses injected
  probe delays (2 fast volumes at 5 ms, 1 slow at 250 ms, deadline 50 ms) to
  assert fast volumes are Accessible, only the slow volume is Inaccessible,
  total elapsed < 2 × deadline, and `LEAKED_PROBE_THREAD_COUNT` increments by
  exactly 1. (PR #727)

- **Health-test TOCTOU fix** (review #727 pass-3, issue #723) —
  `health_includes_warmboot_leaked_probe_threads` in `server.rs` previously
  read `leaked_probe_thread_count()` AFTER calling the handler; a concurrent
  serial test incrementing the counter between the handler return and the read
  could produce `expected > resp.field`, causing a spurious failure. Fixed by
  reading the counter BEFORE the handler call and marking the test
  `#[serial_test::serial]` to prevent concurrent counter mutations. (PR #727)

- **Parallel volume probing — bounded warm-boot time** (review #727 finding 1,
  issue #723) — `probe_all_volumes` now spawns ALL per-volume probe threads
  simultaneously and collects their results under a SINGLE shared wall-clock
  deadline. Total warm-boot stall is bounded at ≈ONE deadline regardless of
  how many distinct volumes are being probed (previously N × deadline). Each
  blocked volume still leaks exactly one OS thread; the
  `LEAKED_PROBE_THREAD_COUNT` counter is incremented once per timed-out volume
  as before. (PR #727)

- **Deterministic probe counter tests** (review #727 finding 2) —
  `probe_timeout_increments_leaked_thread_count` no longer restores the global
  `LEAKED_PROBE_THREAD_COUNT` counter via `store(before, ...)` at the end of
  the test. The restore was racy: it could silently roll back increments from
  a concurrent serial test that also touches the counter. The test now asserts
  `after >= before + 1` (monotone growth) which is the correct invariant and
  is deterministic under a multi-threaded runner. (PR #727)

- **Linux volume-key false-positive guard** (review #727 finding 3) —
  `volume_key` now uses an exact string match (`== "Volumes"`) instead of
  `eq_ignore_ascii_case("Volumes")`, and the `/Volumes/<label>` special-casing
  is fully gated behind `#[cfg(target_os = "macos")]`. On Linux, paths like
  `/volumes/...` (lowercase) were previously mis-classified as external macOS
  volume keys, producing spurious warm-boot `TIMED_OUT` warnings. macOS
  behavior is unchanged. (PR #727)

- **Probe deeper index path for TCC detection** (review #727 finding 2, issue
  #723) — the per-volume warm-boot probe now calls `stat` on the representative
  sample index path inside the volume (e.g.
  `/Volumes/SSD1/Projects/myrepo`) instead of the bare volume mount-point root
  (`/Volumes/SSD1`). On macOS, `stat` on the volume root can succeed even when
  TCC denies access to files inside the volume; probing the deeper path is what
  actually detects the TCC-blocked-inside-volume scenario that issue #723
  targets. The once-per-volume design (at most one leaked thread per blocked
  volume) is preserved.

- **Surface leaked probe-thread count in `/health`** (review #727 finding 3,
  issue #723) — a timed-out volume probe now increments a process-global
  `LEAKED_PROBE_THREAD_COUNT` counter and emits a `tracing::warn!` with the
  running total. The counter is exposed in `GET /health` as
  `warmboot_leaked_probe_threads` (integer, always present, zero on healthy
  machines), giving operators visibility into probe thread accumulation on
  launchd-managed daemons that restart repeatedly.

---

## [0.23.0] — 2026-06-03

### Changed

- **redb 4.x + incompatible-corpus backup/rebuild on open** (#702) — index.redb
  and kg.redb are upgraded to redb 4.x. Existing redb 2.x files are detected as
  incompatible, backed up to `*.v2-incompatible`, and rebuilt (reindex triggered
  automatically). Possible multi-minute reindex window on first start after upgrade.

- **TRUSTY_HNSW_MMAP_SERVE (default on)** (#709) — warm-booted HNSW snapshots
  are now served directly from the mmap page cache, significantly reducing RSS.
  Promotion to a heap-resident copy is deferred until the first write. Disable with
  `TRUSTY_HNSW_MMAP_SERVE=0` on NFS/EFS-backed storage where cold page-fault
  latency matters more than RSS.

- **TRUSTY_VECTOR_QUANT (f16/i8)** (#712) — optional vector quantization for new
  HNSW indexes: `f16` (≈2× smaller, small recall cost) or `i8` (≈4× smaller,
  larger recall cost). Requires a forced reindex to take effect on existing indexes.

- **Persistent reindex hash cache** (#662) — content-hash cache for incremental
  reindex is now stored on disk and survives daemon restarts, avoiding unnecessary
  re-embedding on startup.

- **Dashboard auto-start** (#686) — the web UI dashboard auto-starts on first
  daemon launch without requiring a manual `trusty-search ui` invocation.

- **Bulk select/delete/reindex + Documents=0 fix** (#683) — UI and API support
  bulk operations; fixed a regression where new indexes incorrectly reported 0
  documents.

- **GET /indexes?details=true root_path** (#661) — the index list endpoint now
  accepts `details=true` to include `root_path` for each index.

- **Portable-paths fix + migration M004 schema 3→4** (#674) — index paths are
  now stored in a platform-portable form; M004 migration runs automatically on
  first start (non-destructive and idempotent).

> **OPERATOR NOTES:**
> 1. Existing `index.redb` and `kg.redb` files are redb 2.x and will be backed up
>    to `*.v2-incompatible` and rebuilt (reindex) on first start after upgrade.
>    Expect a multi-minute reindex window for large indexes.
> 2. Migration M004 runs automatically, is non-destructive, and is idempotent.

## [0.22.3] - 2026-06-02

### Fixed

- **CUDA arena VRAM OOM prevention (issue #600)** — via trusty-common 0.11.1:
  ORT's BFCArena is now configured with `arena_extend_strategy = kSameAsRequested`
  and an explicit `gpu_mem_limit` (default 12 GiB, tunable via
  `TRUSTY_GPU_MEM_LIMIT_BYTES` / `TRUSTY_GPU_MEM_LIMIT_MB`). Eliminates VRAM OOM
  on 16 GB Tesla T4 GPUs without requiring the `TRUSTY_MAX_BATCH_SIZE=32` workaround.

- **Accurate `/health` provider reporting (issue #604)** — the `provider` field in
  `/health` responses now reports the actual ORT execution provider in use (CUDA,
  CoreML, CPU) rather than always reporting CPU.

- **Non-destructive reindex with atomic swap (issue #603)** — `POST
  /indexes/:id/reindex` now builds a new corpus in a temporary database and swaps
  it atomically on completion, so the existing index stays fully searchable while
  the rebuild runs. Partial or failed reindex jobs no longer corrupt the live index.

- **Portable data paths and migration (issue #602)** — data-directory paths stored
  in persisted index metadata are now normalised at restore time so indexes survive
  machine renames, home-directory changes, and cross-machine copy. A forward
  migration updates stale absolute paths automatically.

- **Non-empty index validation (issue #601)** — the daemon now rejects a reindex
  swap if the freshly built corpus contains zero chunks, preventing an accidental
  wipe of a healthy index caused by a transient file-system or embedder failure.

---

## [0.18.0] - 2026-05-28

### Changed

- **Reduced default redb page-cache ceiling from 512 MB to 64 MB** (#329).
  Empirical profiling showed the actual redb working set for the trusty-tools
  corpus (23,513 chunks) is ~87 MB: a 512 MB cap run peaked at 557 MB RSS while
  an 8 MB cap run peaked at 470 MB — a difference of exactly 87 MB. The 512 MB
  ceiling was massively over-provisioned. The new 64 MB default captures the full
  working set with ~27 MB of headroom for B-tree internal nodes and future corpus
  growth, without the 33% indexing speed penalty observed at 8 MB (where I/O
  pressure becomes the bottleneck). Peak RSS during `--force` reindex of the
  trusty-tools corpus drops from 571 MB (v0.17.0 baseline) to 518 MB median
  (3-run distribution: 515/518/522 MB) — a 53 MB / 9.3% reduction with
  negligible timing impact (+1.6%, within noise). Override via
  `TRUSTY_REDB_CACHE_MB=<MB>` env var if needed.

### Performance

- See `docs/trusty-search/regression-testing/v0.18.0-redb-cap-reduction-cert-2026-05-28.md`
  for full cert numbers (3-run peak RSS distribution and reindex time comparison).

### Notes

- This is the B.2 quick-win from #329. The deferred B.1 (eliminate doc_terms),
  B.3 (lazy chunk LRU), and B.5 (posting compression) optimizations are tracked
  in the #329 follow-up work.
- Warm reindex is unchanged (empirically free — see profiling doc §9 M2).
- The `TRUSTY_REDB_CACHE_MB` env var override was already present; no API change.

---

## [0.17.0] - 2026-05-27

### Added

- **Issue #313 — Stage-1-minimal (`skip_kg`) mode.** A new additive flag
  `skip_kg: bool` on `PersistedIndex`, `IndexHandle`, and `IndexConfig` lets
  operators permanently suppress the Phase 3 Knowledge Graph rebuild for a
  specific index without disabling the embedder / vector search.

  **Three surfaces (D3):**
  - CLI: `trusty-search index --no-kg`
  - YAML: `skip_kg: true` in `trusty-search.yaml`
  - Env: `TRUSTY_NO_KG=1` (machine-wide default applied at `POST /indexes`)

  **Orthogonality (D1):** `skip_kg` and `lexical_only` are independent flags.
  Both can be set simultaneously. `lexical_only` suppresses Stages 2 and 3;
  `skip_kg` suppresses Stage 3 only, leaving vector embeddings intact.

  **503 contract (D2):** `GET /indexes/:id/call_chain` returns a structured
  503 JSON error `{ "error": "kg_unavailable", "reason": "skipped_by_config",
  "index": "…" }` when `skip_kg=true`. Callers must handle this status and
  not treat it as an index-absent 404.

  **Warm-boot:** on daemon restart, indexes with `skip_kg=true` have their
  graph stage initialised as `Skipped` rather than `Pending`, so no spurious
  KG-rebuild attempt is triggered.

  **Performance savings (per index):** ~50–100 MB heap (symbol graph), ~400 ms
  per reindex (tree-sitter extraction pass). Recommended for large
  documentation-only or generated-code sub-indexes in polyrepos.

---

## [0.16.0] - 2026-05-27

### Changed

- **Issue #315 — Lazy `trusty-embedderd` spawn with single-flight + optional
  idle shutdown.** `trusty-search start` no longer spawns the `trusty-embedderd`
  subprocess at daemon boot. Instead, a `LazyEmbedderHandle` is armed at
  startup and the child process starts on the first call to `embed` or
  `embed_batch` (reindex, hybrid search, `context_inference`). For
  `lexical_only` deployments with no semantic workloads the sidecar is never
  spawned, saving ~123 MB RSS.

  **Startup log change:** the boot log now contains
  `"embedderd supervisor armed, deferred spawn enabled"` instead of the
  previous "spawning sidecar" message. The first embed request logs
  `"LazyEmbedderHandle: first embed request — spawning trusty-embedderd"`.

  **Single-flight guarantee:** concurrent first callers serialise on an
  internal `Mutex`; exactly one spawn attempt is made regardless of how many
  embed calls arrive simultaneously.

  **Optional idle shutdown** (`TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS`, default
  `0` = disabled): when set to a non-zero value, the sidecar is killed after
  that many seconds of inactivity and the spawn gate is reset so the next
  embed request triggers a fresh spawn. Useful for `lexical_only` deployments
  that occasionally run a reindex.

  **Escape hatches unaffected:**
  - `TRUSTY_EMBEDDER=in-process` — no supervisor, no change.
  - `TRUSTY_EMBEDDER=http://...` or `unix://...` — no spawn, no change.
  - Binary discovery (`TRUSTY_EMBEDDERD_BIN`, PATH) still runs at daemon boot
    and fails fast if the binary is missing, preserving the existing install-hint
    error for misconfigured deployments.

  ```bash
  # Arm idle-shutdown for a lexical_only deployment:
  TRUSTY_EMBEDDERD_IDLE_SHUTDOWN_SECS=300 trusty-search start
  ```

---

## [0.15.1] - 2026-05-27

### Added

- **Issue #314 — `--no-auto-discover` flag and `TRUSTY_NO_AUTO_DISCOVER` env
  var for `trusty-search start`.** When either is set, the post-hydration
  auto-discovery scan (which walks `scan_paths` and indexes any unregistered
  project) is skipped entirely. The daemon starts with only the indexes already
  present in `indexes.toml` or registered at runtime.

  Precedence: CLI flag > env var > default (auto-discover enabled).

  Useful for CI/CD environments that must not discover arbitrary repositories,
  when the scan-paths tree is very large, or when reproducible startup
  behaviour is required.

  ```bash
  # Suppress auto-discovery via flag:
  trusty-search start --no-auto-discover

  # Suppress via env var (e.g. in a systemd unit or launchd plist):
  TRUSTY_NO_AUTO_DISCOVER=1 trusty-search start
  ```

---

## [0.15.0] - 2026-05-27

### Added

- **Issue #317 — Three-phase reindex progress bar (Walking → Chunking →
  Embedding).** The CLI reindex progress bar now shows file enumeration
  explicitly instead of several silent seconds before the first bar appeared.
  A single `ProgressBar` is reused across all three phases — the bar resets
  its position to 0 and updates its label at each phase boundary, "quickly
  filling to 100% then restarting" exactly as requested:

  - **Walking files…** — the daemon emits a new `walk_complete` SSE event
    after the file-system walk finishes. The bar fills instantaneously (the
    walk is synchronous on the daemon; the event arrives the moment it's done).
  - **Chunking…** — the `start` event (emitted immediately after
    `walk_complete`) triggers this brief label while the daemon begins the
    parse/embed pipeline. On large repos this handoff is visible for a fraction
    of a second before the first `batch` event arrives.
  - **Embedding chunks…** — the first `batch` event flips the bar into this
    phase and it fills as batches arrive, exactly as the old `ParseEmbed` phase
    did. For `lexical_only` indexes the embed phase is skipped; the bar stays
    on **Chunking** (there are no `batch` events for BM25-only indexes).

  **Daemon side:** a new `walk_complete` SSE event is emitted before the
  existing `start` event. Shape: `{"event":"walk_complete","total_files":1155}`.
  Old CLI clients that don't recognise `walk_complete` simply ignore it and
  wait for `start` — fully backward-compatible. New CLI clients talking to an
  old daemon (no `walk_complete`) fall back to the legacy two-phase flow
  (`start` → Embedding) automatically.

  **Decision on chunk+embed split (3 phases vs 2):** the daemon's pipelined
  orchestrator fuses parse+embed per batch — there is no clean "all chunks,
  then all embeds" split. `Chunking` is therefore a synthetic brief phase
  (the label shown between `walk_complete` and the first `batch` event, which
  is typically under one second). `Embedding` covers the rest of the pipeline
  exactly as the old `ParseEmbed` variant did. This matches Option 2 from the
  design spec and delivers the three visible phase labels the user asked for.

- **Bundled install** — `cargo install trusty-search` now produces **both**
  `trusty-search` and `trusty-embedderd` binaries from a single command.
  A second `[[bin]]` entry in `trusty-search/Cargo.toml` delegates to the
  `trusty-embedderd` library crate (`trusty_embedderd::run()`), so the
  sidecar binary is built and installed alongside the search daemon with
  zero extra steps. The standalone `cargo install trusty-embedderd` still
  works for advanced users who want only the embedding daemon.

  **Upgrade action (users coming from Phase 2):** simply run:
  ```
  cargo install trusty-search --locked --force
  ```
  No separate `cargo install trusty-embedderd` required.

### Changed (BREAKING)

- **#110 Phase 2 — `trusty-embedderd` is now a required runtime dependency.**
  When `TRUSTY_EMBEDDER` is unset, `trusty-search start` auto-spawns
  `trusty-embedderd --stdio` as a supervised child process and communicates via
  piped stdin/stdout (JSON-RPC 2.0). The child is restarted automatically on
  crash (up to `TRUSTY_EMBEDDERD_MAX_RESTARTS`, default 5) and is killed when
  the parent exits (via `kill_on_drop`).

  **BREAKING:** If `trusty-embedderd` is not found on PATH and
  `TRUSTY_EMBEDDERD_BIN` is unset, `trusty-search start` now **exits with an
  error** rather than silently falling back to in-process embedding. This is a
  deployment error — the sidecar architecture is a core design commitment, not
  an optional feature.

  **Upgrade action required:** install both binaries in one command:
  ```
  cargo install trusty-search --locked
  ```
  `cargo install trusty-search` now installs `trusty-embedderd` automatically —
  no second install command needed (bundled install, see above).
  To run without the sidecar (CI, debugging), set `TRUSTY_EMBEDDER=in-process`
  explicitly. The in-process path is an escape hatch, not a default.

### Added

- New `service/embedder_supervisor.rs` façade module: `SupervisorConfig` (with
  `from_env()` / `into_common()`), `locate_embedderd_binary()`, and
  `default_socket_path()`.
- Four `TRUSTY_EMBEDDER` modes: `auto`/unset (default stdio-sidecar),
  `in-process`, `http://...`, `unix:/path`.
- New `UdsEmbedderAdapter` for the `unix:` transport mode.
- New `SlotEmbedderAdapter` for the stdio-sidecar default: reads through the
  supervisor's `Arc<RwLock<Arc<dyn EmbedderClient>>>` slot so crash-restart
  swaps are transparent to all call sites.
- Integration test file `tests/embedder_supervisor_e2e.rs` with 7 `#[ignore]`-
  tagged lifecycle tests (spawn, batch, concurrency, crash-restart, empty batch,
  bit-identical, bad-path).
- New environment variables:
  - `TRUSTY_EMBEDDERD_STARTUP_TIMEOUT_SECS` (default 30)
  - `TRUSTY_EMBEDDERD_RESTART_BACKOFF_MAX_SECS` (default 60)
  - `TRUSTY_EMBEDDERD_MAX_RESTARTS` (default 5)
  - `TRUSTY_EMBEDDERD_BIN` — explicit path to the binary (overrides PATH search)

- **Schema migration framework.** Daemon startup now auto-migrates existing
  redb indexes when the schema version changes between releases. Migrations are
  non-blocking — the daemon serves queries at the pre-migration schema quality
  while each per-index task runs in the background. The schema version is
  persisted in a new `_meta` redb table after each successful migration step
  (crash-safe: a crash before the version write triggers a retry on next
  startup; idempotent `apply` implementations make retries safe).
  Set `TRUSTY_DISABLE_MIGRATIONS=1` to skip auto-migrations (debugging /
  one-off restore scenarios).

- **Migration M001: per-`pub const`/`pub static` Rust re-chunking (issue #143).**
  Indexes created before v0.11.1 had one `ChunkType::Code` chunk per Rust file
  instead of one `ChunkType::Constant` chunk per `pub const`/`pub static`
  declaration. M001 re-indexes every affected Rust file on first startup after
  upgrade, bringing those indexes up to v0.11.1 search quality. Idempotency is
  guaranteed by a "has Constant chunks?" pre-check; a regex pre-filter
  (`\bpub\s+(const|static)\b`) skips files that have no qualifying declarations
  without incurring the ~10 ms/file tree-sitter parse cost.

---

## [0.14.0] — 2026-05-27

### Added

- **`--data-dir <PATH>` flag on `trusty-search start`** (with `TRUSTY_DATA_DIR` env
  var) — overrides the platform default data directory for redb index storage,
  PID/port lockfiles, and `indexes.toml`. Enables multiple isolated daemon
  instances on the same machine; each instance gets its own data dir, binds its
  own port, and has no knowledge of the others.

  This flag was the key enabler for the Stage-1 cert methodology (issue #281):
  launching a fresh isolated daemon with `--data-dir /tmp/ts-stage1-cert` and
  a `HOME` override to suppress auto-discovery let us measure a clean reindex
  against a known-empty data dir without touching the production daemon on 7878.

  ```bash
  # Launch isolated cert daemon on a different port with its own data dir
  HOME=/tmp/ts-cert-home RUST_LOG=info trusty-search start \
      --data-dir /tmp/ts-stage1-cert \
      --port 7980 \
      --foreground
  ```

  The env var form is convenient for CI and container deployments:
  ```bash
  TRUSTY_DATA_DIR=/ci/search-data trusty-search start
  ```

  See `docs/trusty-search/regression-testing/v0.14.0-stage1-cert-2026-05-27.md`
  for the Stage-1 certification run that motivated this feature (issue #281).

---

## [0.12.1] — 2026-05-26

### Changed

- **Internal dep refactor (no behaviour change).** The `trusty-embedder-client`
  crate dependency has been removed. `EmbedderClient`, `RemoteEmbedderClient`,
  `EmbedRequest`, and `EmbedResponse` are now re-exported from
  `trusty_common::embedder_client` (feature `embedder-client`). All call sites
  updated from `trusty_embedder_client::` to `trusty_common::embedder_client::`.
  The remote-embedder opt-in path (`TRUSTY_EMBEDDER=http://...`) is fully
  functional and unchanged.

---

## [0.12.0] — 2026-05-26

### Added

- **#110 Phase 1** **Optional remote embedder via `TRUSTY_EMBEDDER` env var.**
  Set `TRUSTY_EMBEDDER=http://127.0.0.1:7890` to route all embed calls to a
  running `trusty-embedderd` instance instead of running ONNX in-process.
  Default behaviour (unset, `local`, or `in-process`) is unchanged.
  The startup log now always prints `embedder: in-process` or
  `embedder: remote <url>` so operators can confirm the active mode.

  New companion crates (v0.1.0, MIT):
  - `trusty-embedder-client` — `EmbedderClient` trait + JSON/HTTP wire types,
    `InProcessEmbedderClient` (default), and `RemoteEmbedderClient`
  - `trusty-embedderd` — standalone daemon that loads `AllMiniLML6V2(Q)` once
    and serves `POST /embed` + `GET /health` (clap CLI + axum HTTP, stderr logging)

---

## [0.11.1] — 2026-05-26

### Added

- **#143** `ChunkType::Constant` chunks per `pub const` / `pub static` Rust declaration.
  The Rust tree-sitter chunker now emits one `Constant` chunk per top-level public
  constant/static, with `function_name = Some(<identifier>)` (e.g. `BRUSILOV_EPOCH`).
  Previously a file containing only `pub const` declarations produced a single whole-file
  `Code` chunk with null `function_name`, making every constant invisible to symbol-name
  queries and the Definition-intent boost. Phase 1 covers Rust only; Python /
  TypeScript / Go / Java follow-up noted via TODO comment in the chunker.
- **#142** SCREAMING_SNAKE_CASE pattern in `QueryClassifier` — queries that are a
  single ALL_CAPS_WITH_UNDERSCORES identifier (e.g. `MAX_BATCH_SIZE`, `BRUSILOV_EPOCH`,
  `KIKUCHI_MAX_DEPTH`) now classify as `Intent::Definition` instead of `Unknown`.
  This was a gap in the priority chain: `SNAKE_IDENT_RE` matched lowercase snake_case
  but not SCREAMING_SNAKE; `ACRONYM_HINT_RE` fired on ALL_CAPS tokens *inside*
  multi-word queries but not on a whole-query constant name.

### Fixed

- **#142 + #143** Together these two fixes unblock the Definition-intent boost (#122)
  for constant lookups: the classifier correctly recognises SCREAMING_SNAKE queries,
  and the corpus now has per-constant chunks with non-null `function_name` for the
  structural lane to surface.

---

## [0.11.0] — 2026-05-26  **BREAKING**

### Removed

- **#152 / #145 PROVENANCE-ONLY decision** — Louvain community detection and
  `community_cohesion` ranking have been deleted. Empirical data showed the KG
  ranking lane lost Hit@1 by 16.7 pp vs semantic-only on KG-targeted queries
  (7/18 vs 10/18). The symbol-graph infrastructure is preserved — `get_call_chain`
  and `search_kg` MCP tools continue to work.

  BREAKING CHANGES:
  - `CodeChunk.community_id` field removed from schema (read tolerance preserved
    via `#[serde(default)]` — existing serialised chunks are tolerated on
    deserialise).
  - Post-RRF reranker no longer applies `community_cohesion` blending. The
    `meta.graph_scoring` and `meta.community_cohesion` fields are gone from
    search response JSON.
  - `GET /indexes/:id/communities` and `GET /indexes/:id/communities/:symbol`
    endpoints return 404 (removed, not deprecated).
  - `spawn_community_detection` removed from the reindex pipeline.

  Deleted components:
  - `src/core/community.rs` — entire Louvain implementation (673 lines)
  - `src/core/indexer/graph_score.rs` — `GraphScorer` / centrality bonus table (309 lines)
  - `SearchAppState::graph_scorer()` and `invalidate_graph_scorer()` methods
  - `GraphScorerCache` type alias and `spawn_community_detection` reindex task
  - `CodeChunk::community_id` field
  - `GET /indexes/:id/communities` and `GET /indexes/:id/communities/:symbol` endpoints
  - `meta.graph_scoring` and `meta.community_cohesion` fields from search response

  Migration notes for callers:
  - `CodeChunk` serialisations with `community_id` are tolerated (ignored on
    deserialise via `#[serde(default)]`). No schema migration required.
  - Old redb community tables (`KG_COMMUNITIES_TABLE`, `kg_symbol_community`)
    remain defined in `corpus.rs` for migration tolerance; they are no longer
    written or read by the active search path.
  - Remove any code polling `meta.graph_scoring` or `meta.community_cohesion`
    from search responses.
  - Remove any calls to the `/communities` or `/communities/:symbol` endpoints.

---

## [0.10.0] — 2026-05-25

### Added

- **#138** **Per-lane MCP tools — push intent classification to the LLM.**
  Four new MCP tools — `search_lexical`, `search_semantic`, `search_kg`,
  `search_all` — let the calling LLM pick the right lane combination
  instead of relying on the server-side regex intent classifier.
  - `search_lexical` — BM25 + grep only, ripgrep-equivalent latency.
    Always available.
  - `search_semantic` — BM25 + HNSW via RRF, no KG. Requires Stage 2
    (`vector`) on the index.
  - `search_kg` — BM25 + HNSW + KG expansion, forced ON. Requires Stage 3
    (`kg`).
  - `search_all` — full hybrid (lexical + semantic + KG), adaptive
    routing. Polymorphic: with `index_id` it's per-index hybrid (ticket
    spec); without, it falls back to legacy cross-project fan-out
    (issue #10) for back-compat.

  The legacy `search` tool stays as a back-compat alias for the
  per-index full hybrid. The MCP `tools/list` response now surfaces
  five lane-related search tools.

  When a per-lane tool is called against an index whose prerequisite
  stage isn't `Ready`, the daemon returns a structured `STAGE_NOT_READY`
  error (JSON-RPC code `-32010` or, via `tools/call`, `isError: true`
  with `_meta.error_code = "STAGE_NOT_READY"`). The error carries the
  full `current_stages` snapshot and a `suggested_tools` retry hint so
  the LLM can pick a fallback without a second status probe.

  `SearchStage` gains `Semantic` and `Graph` variants alongside the
  existing `Lexical`. The search dispatcher routes each variant to its
  fixed lane combination: `Lexical` skips HNSW + KG; `Semantic` runs
  BM25 + HNSW but skips KG; `Graph` forces KG expansion even on
  Definition-intent seed queries. `stage = None` keeps the legacy
  adaptive routing.

  Tool descriptions follow the ticket's authoring guide (when-to-use
  hook, fit/don't-fit examples, cost class, failure-mode hint) and
  carry `examples` arrays in their JSON schemas to nudge LLM tool
  selection. The classifier and per-stage gating remain in place as
  defensive fallbacks for non-MCP HTTP callers.

### Changed

- The `search_all` MCP tool is now polymorphic: when invoked with an
  `index_id`, it dispatches the per-index full hybrid (matching the
  #138 spec); when invoked without one, it preserves the legacy
  cross-project fan-out behaviour. Callers using either form keep
  working without code changes.

---

## [0.9.2] — 2026-05-25

### Fixed

- **#122** Definition boost regresses Hit@1 on function-name queries with
  descriptor / string-literal matches. The struct-definition boost added in
  v0.8.x (#117) covered `Struct`/`Enum`/`Class`/`Trait`/`TypeAlias` chunks
  but deliberately excluded `Function`/`Method` because we assumed the
  `inject_entity_exact_match` lane would carry function-name queries. The
  synthetic-corpus baseline (#123) reproduced a clean failure for Q04
  `BRUSILOV_EPOCH`, where a usage site (`calibration.rs`) out-ranked the
  canonical declaration (`constants.rs`) across all three search modes.

  The fix extends `apply_score_adjustments` to also apply
  `STRUCT_DEFINITION_BOOST` (2.0×) to `Function`/`Method` chunks whose
  `function_name` matches a query token. The chunk_type filter is the
  natural defense against the JSON-descriptor false-positive case (a
  `Constant` chunk containing `"get_call_chain"` as a string literal in an
  MCP tool descriptor): JSON-descriptor chunks are typed `Constant` or
  `Statement`, not `Function`, so they are never boosted.

  Four regression tests pin the new behavior:
  `test_function_definition_boost_surfaces_function_over_string_literal_usage`,
  `test_method_definition_boost_fires`,
  `test_function_boost_skipped_on_conceptual_intent`, and
  `test_function_boost_no_op_when_function_name_missing`.

---

## [0.9.1] — 2026-05-25

### Fixed

- **#135** Warm-boot stages restoration — fixes silent BM25-only fallback on
  existing indexes. The v0.9.0 staged-pipeline refactor introduced a regression
  in the daemon's warm-boot path: every index restored from `indexes.toml`
  came back with `stages = Pending` for lexical / semantic / graph, regardless
  of what was on disk. Because the search handler now derives
  `search_capabilities` from `stages` (not the legacy top-level `status`), the
  hybrid pipeline was silently disabled on every fully-indexed registered
  project until the operator force-reindexed.

  The fix inspects each index's on-disk artifacts after warm-boot:
  `corpus.chunk_count()` (lexical readiness), `hnsw.usearch` presence
  (semantic readiness), and the rehydrated symbol graph's `node_count()`
  (graph readiness). A `lexical_only` index forces semantic + graph to
  `Skipped` regardless of on-disk state. An index with `chunk_count == 0`
  but a registered entry is treated as mid-reindex recovery (lexical →
  `InProgress`) so the next reindex resumes via the hash-skip path.

  No schema change: the existing on-disk artifacts are authoritative, so
  `indexes.toml` did not need a `stages_marker` field. Existing daemons
  pick up the fix on the next restart with no migration step.

---

## [0.9.0] — 2026-05-25

### Added — staged-pipeline (Phase 1)

- **#109 (Phase 1)** Staged indexing pipeline — initial cut. The reindex
  pipeline now exposes per-stage progress so searches can run as soon as
  the lexical lane (Stage 1) is ready, without blocking on the embedder
  (Stage 2) or symbol-graph build (Stage 3).

  - **Status surface.** `GET /indexes/{id}/status` gains two additive
    fields (back-compat preserved):
    - `stages: { lexical: …, semantic: …, graph: … }` carrying per-stage
      `status` (`pending` | `in_progress` | `ready` | `skipped`),
      timestamps, and counters.
    - `search_capabilities: ["bm25", "literal", "exact_match", …]`
      growing as each stage flips to `ready` (`+ ["vector"]` when
      semantic ready, `+ ["kg"]` when graph ready).
    The legacy top-level `status` field is unchanged for existing API
    consumers.

  - **Search handler graceful degradation.** The handler now consults
    `search_capabilities` (not the top-level `status`) to decide which
    lanes to run. Searches during a reindex hit only the BM25 lane until
    the embedder catches up — the response carries
    `meta.search_capabilities` so clients can show "lexical-only" badges
    or retry once the semantic lane lands.

  - **`?stage=lexical` query param.** Per-query opt-in to Stage-1-only
    routing even on a fully-indexed index. Useful for
    grep-replacement use cases that don't want semantic noise.

  - **`--lexical-only` CLI flag and `lexical_only: true` API field.**
    Permanent opt-out from Stage 2 and Stage 3 at index-create time.
    The index stays at `status: indexed_lexical` forever; the reindex
    pipeline skips the embedder entirely. Persisted to `indexes.toml`
    so the choice survives daemon restarts. Useful for callers who
    explicitly want a "daemonized ripgrep" without the embedder
    overhead.

  - **Backpressure stub.** Search calls ping a per-index
    `tokio::sync::Notify` so the background Stage-2 task can yield
    briefly. Phase-2 work will tune the policy.

  Out of scope for Phase 1 (deferred to Phase 2): Stage 3 (Louvain) /
  KG-edge resolution async split — they remain in the synchronous
  reindex tail; file-watcher debouncing; full backpressure tuning.

  Pinned by `service::reindex::tests::stage_1_completes_and_search_works_before_embedding`,
  `lexical_only_index_never_runs_stage_2`,
  `search_capabilities_grows_as_stages_complete`, and the per-stage
  registry tests in `core::registry::tests::stage_status_*`.

### Changed

- **`CodeIndexer`** gains a `parse_files_only` method that mirrors
  `parse_and_embed_files` but skips the ONNX embed step entirely. Used
  exclusively by the `lexical_only` reindex path so a BM25-only index
  never pays the embedder's session-arena cost. Existing callers are
  untouched.
- **`SearchQuery`** gains an optional `stage: Option<SearchStage>`
  field. Defaults to `None` so existing callers see no behaviour
  change; setting `Some(SearchStage::Lexical)` forces the
  Stage-1-only lane routing.
- **`PersistedIndex`** gains a `lexical_only: bool` field with
  `#[serde(default)]` so legacy `indexes.toml` files load as `false`
  (full pipeline). Only explicit-`true` is written to disk to keep
  the on-disk format compact.

---

## [0.8.3] — 2026-05-25

### Fixed
- **#118** `mode=text` searches no longer return silently empty result
  sets. The walker's `include_docs` default flipped from `false` to
  `true`: prose docs (`*.md`, `*.mdx`, `*.rst`, `*.txt`, `*.adoc`) and
  `CHANGELOG*` / `LICENSE*` / `NOTICE*` files (with extensions) are now
  indexed alongside source. The per-mode hard filter
  (`is_allowed_for_mode`) is the single source of truth for which file
  types each mode returns — code-mode results never include `.md` chunks
  because the post-RRF filter rejects them, regardless of what the
  walker indexed.

  Migration: an `indexes.toml` entry written by v0.8.2 (where
  `include_docs = false` was the default and omitted via
  `skip_serializing_if`) now deserialises as `true` under v0.8.3 —
  `mode=text` searches start working on the next daemon restart with no
  explicit migration step. Indexes that PERSISTED an explicit
  `include_docs = false` (e.g. via `trusty-search.yaml`) keep their
  opt-out. Pinned by
  `service::persistence::tests::include_docs_defaults_true_and_round_trips`.

  The file watcher (`watch_loop`) follows the new default so live `.md`
  edits flow into the index too; the v0.8.2 `is_default_doc_excluded`
  guard there was removed.

  Acceptance pinned by
  `service::walker::tests::test_issue_118_acceptance_walks_both_source_and_docs`
  (walk side) plus the existing
  `core::indexer::tests::test_mode_filter_code_returns_only_source` /
  `test_mode_filter_text_returns_only_prose_and_named_docs` (search side).
- **#117** Definition-intent searches now surface struct/enum/class/trait
  declarations above usage sites. On the v0.8.1 benchmark the query
  `HNSW vector similarity search` placed `hnsw_store.rs` at rank 8 behind
  `retrieval.rs` and `mmr.rs` because the BM25 lane couldn't distinguish
  "file mentions HNSW many times" from "file IS the HNSW declaration". Two
  layered fixes:
  - #119's classifier upgrade routes the query to `Definition` (was
    `Unknown`), which already demotes docs and runs the grep lane.
  - The post-RRF reranker (`apply_score_adjustments`) now multiplies the
    score of any `Struct`/`Enum`/`Class`/`Trait`/`TypeAlias` chunk by
    `STRUCT_DEFINITION_BOOST = 2.0` when the chunk's `function_name`
    contains (case-insensitive) at least one query token. Substring
    rather than exact match so `HnswStore`/`hnswstore` matches the
    `hnsw` token; `is_struct_definition_chunk_type` enforces that only
    declaration-shaped chunks qualify (free code and methods don't).

  Acceptance pinned by `test_struct_definition_boost_surfaces_struct_over_usage`:
  a corpus with one Struct declaration (`HnswStore` in `hnsw_store.rs`)
  and three usage chunks now ranks the declaration in top-3 for the
  canonical query.
- **#119** `QueryClassifier` now recognises three additional query shapes
  that were silently returning `intent: "Unknown"` on the v0.8.1
  grep-equivalency benchmark — keeping the existing intent-aware lane
  weighting, RRF balance, and effective-mode override dormant on 12 of
  14 real queries. The new rules:
  - **Single `snake_case` identifier** (e.g. `apply_archive_downrank`,
    `is_default_doc_excluded`, `get_call_chain`, `bm25_search`) →
    `Definition`. Token must be the whole query and must contain at
    least one underscore so a bare `foo` is not pulled into the rule.
  - **ALL-CAPS acronym hint** (e.g. `HNSW`, `BM25`, `RRF`, `ORT`, `API`,
    `LRU`) anywhere in the query → `Definition`. These almost always
    refer to a struct, module, or type name in the codebase, so routing
    them to Definition lets the structural lane surface the canonical
    declaration over usage sites. This also closes #117 (see below).
  - **≥4-word natural-language query with no identifier tokens** (e.g.
    `axum middleware concurrency limiter`,
    `Louvain community detection modularity`,
    `redb persistence write transaction`,
    `embed batch async worker pool`) → `Conceptual`. Lower bar than the
    existing 6-word `LONG_NL_RE` so short concept queries also route to
    the vector lane.

  Benchmark impact: ≥13/14 of the canonical queries now classify as
  non-`Unknown` (was 2/14). Pinned by
  `core::classifier::tests::test_canonical_benchmark_at_least_12_of_14_classified`.

---

## [0.8.2] — 2026-05-25

### Fixed
- **#100 follow-up** Clarified the `reindex complete:` daemon log line so a
  hash-skip-only run no longer looks like a walker → chunker regression. The
  log now includes `indexed_new=` (files that actually re-chunked this run,
  derived as `indexed - skipped`) alongside the existing counters. Previously
  a second reindex of an unchanged workspace logged `files=N chunks=0` —
  textually identical to a hypothetical walker bug that yields paths but
  drops them — so operators kept misreading the fast path as a failure
  (extensive investigation in the v0.8.1 issue thread). The same
  `indexed_new` field is now surfaced on the reindex `complete` SSE event so
  external callers (CLI, dashboard, open-mpm) read the same signal.

### Added
- New end-to-end integration test `reindex_persists_chunks_end_to_end` in
  `service::reindex::tests` that runs the FULL pipeline (walker → chunker →
  corpus) twice against a staged tempdir. The first reindex asserts
  `total_chunks > 0`, `chunk_count() > 0`, and that a search for a unique
  function name returns a chunk whose `file` field equals the canonical
  `lib.rs` path. The second reindex asserts `total_chunks == 0` AND
  `skipped == 1` — pinning the hash-skip fast path so the next bisection
  doesn't waste another round chasing a non-existent walker bug. The
  walker-only unit tests added in v0.8.1 catch the walker yield but not the
  chunker / corpus end of the pipeline; this test closes that gap.

### Internal
- `apply_successful_commit` and `emit_complete_event` derive `indexed_new`
  from the existing `indexed` and `skipped` counters — no new tracked state.

---

## [0.8.1] — 2026-05-25

### Fixed
- **#100** Walker now honours `.gitignore`. Previously the walker used
  `walkdir` directly, which ignores all VCS-aware ignore files; combined with
  the per-index chunk budget (`TRUSTY_MAX_CHUNKS`, auto-tuned from the memory
  policy), this caused silent partial-index failures where a gitignored
  subtree (e.g. `claude-mpm-patch/` full of minified bundles) dominated or
  exhausted the budget before the walker reached the actual project source.
  The walker now delegates to the `ignore` crate — the same engine ripgrep
  uses — and respects `.gitignore`, `.git/info/exclude`, the global git
  ignore, `.ignore`, `.rgignore`, and parent-directory ignore files. The
  hardcoded `SKIP_DIRS` / `should_skip_path` filters still apply as
  defence-in-depth for projects without a `.gitignore` (closes #100).

### Added
- **#100** `respect_gitignore` opt-out for indexes that intentionally walk a
  gitignored / vendored subtree. The flag rides on `WalkOptions`, `IndexConfig`
  (`trusty-search.yaml`), `IndexHandle`, `PersistedIndex` (with serde default
  for back-compat with existing `indexes.toml` files), and the
  `POST /indexes` `respect_gitignore` request field. Default `true` so every
  existing caller picks up the fix automatically without a wire change.
- **#100** `walk_truncated_by_budget` (boolean) and `chunks_dropped_by_cap`
  (count) surfaced in `GET /indexes/:id/status` and the reindex `complete` SSE
  event. Non-zero ⇒ the index is incomplete because the per-index chunk cap
  was reached during the walk; operators previously had no way to distinguish
  a clean index from one whose tail was silently dropped. Defaults to
  `false` / `0` for indexes warm-booted from disk that haven't been reindexed
  since the daemon started.

### Internal
- New `ignore = "0.4"` direct dependency in `crates/trusty-search/Cargo.toml`
  (previously a transitive dep via globset / notify).
- `CommitTimings.chunks_dropped_by_cap` plumbs the per-batch cap-drop count
  from `core::indexer::ingest` up through `service::reindex` to
  `ReindexProgress`.
- 4 new walker unit tests pin the behaviour:
  `test_walker_honors_gitignore`, `test_walker_respects_disable_flag`,
  `test_walker_honors_dot_ignore`, `test_walker_still_skips_hardcoded_dirs`.

---

## [0.3.57] — 2026-05-21

### Changed
- Granular per-phase progress for `trusty-search index` / `reindex`. The live
  progress display now carries a phase label on its header line
  (`Connecting → Parsing & embedding files → Done`) and the stats line shows
  embedding throughput (`<N> cps`) and a file-derived ETA. The post-reindex
  timing breakdown is reorganised into five named phases — Parse/chunk, Embed,
  Upsert vectors, BM25 index, Knowledge graph — and now includes the
  vector-upsert timing. Progress draws to **stderr** only and is suppressed
  entirely when stdout is not a TTY (piped / redirected output).

---

## [0.3.56] — 2026-05-21

### Fixed
- **#127** `TRUSTY_INDEX_MEMORY_LIMIT_MB` auto-tune raised from 40% to 75% of system RAM. The old 40% fraction yielded only a ~52 GB ceiling on a 128 GB host, but large repos (e.g. 114k chunks) peak at ~76 GB RSS during reindex on Apple Silicon — the pipeline hit the limit and skipped batches, leaving the index incomplete. 75% of RAM gives the transient indexing pipeline enough headroom while still reserving 25% for the OS and other processes. The `TRUSTY_INDEX_MEMORY_LIMIT_MB` env override is unchanged; the startup log now reports "75% of RAM".
- **#128** Batch HNSW upsert no longer silently drops a whole 128-file batch when one embedding fails. `UsearchStore::upsert_batch` now screens each vector for NaN / infinity / all-zero (degenerate-for-cosine) components and isolates per-item `add` failures: the offending chunk id is logged at `warn`, its key-map entry is rolled back, and the remaining vectors index normally. The call only returns `Err` when *every* vector fails (a systemic problem).

---

## [0.3.36] — 2026-05-15

### Added
- **#122** Branch-aware scoring: `branch_files` request field boosts chunks from the current branch by a configurable multiplier (default 1.5×, clamped to `[1.0, 3.0]`); results carry `on_branch: bool`; when `branch_files` is absent, the daemon shells out to `git merge-base` + `git diff --name-only` to derive the file list automatically

### Fixed
- **#121** Embedder init hang: ORT initialization now runs on a blocking thread with a configurable timeout (`TRUSTY_EMBEDDER_INIT_TIMEOUT_SECS`); a timeout surfaces as an error state rather than hanging forever
- **#120** `MEMORY_LIMIT_MB` recomputed as 25% of system RAM instead of a fixed tier cap; `TRUSTY_MEMORY_LIMIT_MB` still overrides

### Changed
- Makefile: `CLOSES` variable support in `patch` target; surgical daemon stop in `deploy` (PID lockfile + `pkill -x`) instead of broad pattern match; `kill` before deploy prevents OOM during compile; `launchctl unload` in deploy target prevents dual-daemon OOM
- Workflow: `closes #N` now required in all resolution commits

---

## [0.3.35] — 2026-05-14

### Fixed
- **#119** CoreML jetsam crash on Apple Silicon (via trusty-embedder v0.1.5 bump)
- **#118** `DELETE /indexes/:id` now persisted to `indexes.toml` so removals survive daemon restart
- Daemon now detaches from terminal when started without `--foreground`, fixing crash when the parent tmux session is killed
- ORT batch size default lowered from 200 MB/slot estimate; clamp changed to `[8, 64]` to prevent 94 GB reindex spikes

### Changed
- `TRUSTY_DEVICE` persisted to `daemon.env` so `--device cpu` survives daemon restarts
- Makefile: `deploy` target added with `CARGO_BUILD_JOBS=2` to prevent OOM kills; `cargo install` removed from `patch` target

---

## [0.3.34] — 2026-05-13

_(version bump only; internal release pipeline fix)_

---

## [0.3.33] — 2026-05-13

### Added
- OpenRPC `rpc.discover` endpoint exposed via trusty-mcp-core helpers
- `SearchMcpService` implements `ServiceDescriptor` (#115)
- Migration script for mcp-vector-search → trusty-search

### Fixed
- **#117** `serve --http` no longer clobbers the daemon's `http_addr` discovery file
- **#116** tree-sitter upgraded to 0.26 for direct linking compatibility with open-mpm
- **#114** glibc 2.34 compatibility for CUDA builds on Amazon Linux 2023
- Test flakiness in file-watcher test on macOS (stray tmpdir events)

### Changed
- trusty-mcp-core bumped to v0.1.1 for OpenRPC support
- trusty-embedder bumped to v0.1.4 for bundled-ort support

---

## [0.3.32] — 2026-05-12

### Fixed
- **#117** `serve --http` flag no longer overwrites the daemon's HTTP address discovery file, preventing the CLI from connecting to the wrong process

---

## [0.3.31] — 2026-05-12

### Added
- **#112** Index context inference and smart fan-out routing: queries against unknown or multi-index contexts are routed to the best-matching indexes automatically
- **#113** Runtime CUDA auto-detection with GPU batch size tuning: when a CUDA-capable GPU is detected, `TRUSTY_MAX_BATCH_SIZE` is auto-bumped to 512; set `TRUSTY_MAX_BATCH_SIZE_EXPLICIT=1` to keep a manually configured value

---

## [0.3.30] — 2026-05-12

### Added
- **#110** `POST /search` fan-out endpoint: search across all registered indexes in a single call, results merged by RRF score
- **#111** `path_filter` field on index registration: restrict which file paths are indexed for a given `IndexId`
- **#91** Classifier extended to match leading-acronym identifiers (`BM25Index`, `IOError`, `URLParser`)

---

## [0.3.29] — 2026-05-12

### Fixed
- `colored::Colorize` import gated to macOS only, fixing compilation on Linux

---

## [0.3.28] — 2026-05-12

### Changed
- **#97** Extracted 52 functions from `main.rs` into `commands/` modules for improved maintainability
- **#98, #109** Extracted helpers in `build.rs` and `spawn_reindex` into focused async helpers
- **#98** Reindex phases extracted into focused async helpers
- **#103** `symbol_graph` helpers extracted to reduce cyclomatic complexity
- **#101, #104** Replaced `unwrap`/`panic`/`process::exit` with proper error handling throughout

### Tests
- **#99** Unit tests added for CLI command handlers and daemon-guard paths

---

## [0.3.27] - 2026-05-12

### Fixed
- **#87** macOS SIGKILL on binary replace: `trusty-search start` now exits with an error if a daemon is already running; `make install` and `make patch` stop the daemon before reinstalling the binary
- **#82** Memory limit enforcement during reindex: tier-based hard caps on `TRUSTY_MAX_BATCH_SIZE` env-var overrides (Medium=64, Large=128, XLarge=256) prevent RSS spikes from misconfigured batch sizes; existing background RSS poller confirmed active
- **#89** ORT ONNX arena pre-allocation: confirmed mitigated by `with_arena_allocator(false)`; tier hard caps add defense-in-depth

### Improved
- **#88** Intent classifier now recognises domain-term Definition queries: PascalCase/CamelCase identifiers, and standalone "definition"/"interface"/"schema"/"type"/"enum"/"model" trigger Definition intent
- **#91** Compound noun classifier: CamelCase compound noun queries (e.g. "QueryClassifier intent classification") now route to Definition intent instead of Unknown
- **#92** Definition-intent ranking: `.md`/`.toml`/`.json`/`.yaml` files scored at 0.5× in RRF fusion for Definition intent only; source files rank first for symbol lookups
- **#94** KG expansion: results merged by score before `take(top_k)`; `hybrid+kg` match_reason now surfaces on large indexes

---

## [0.1.46] — 4 indexing speed optimizations

### Performance
- **INT8 quantized model**: switch fastembed model to `AllMiniLML6V2Q` (INT8 quantized); same 384-dim output, ~30% faster ONNX inference
- **Batch upsert**: accumulate HNSW vectors across all chunks in a reindex pass and call a single `UsearchStore::upsert_batch` instead of N individual inserts; eliminates per-chunk lock overhead
- **Split lock** (`parse_and_embed_files` / `commit_parsed_batch`): parsing + embedding now runs outside the write lock; the write lock is held only for the final redb + HNSW commit, enabling higher concurrency
- **Batch size 512**: increase ONNX batch size 256 → 512 for better GPU/NEON/AVX2 saturation
- Combined target: **< 2 min on a 14k-file repo** (down from ~2–4 min after v0.1.34)

---

## [0.1.45] — multi-line progress + blue-green verify + incremental index

### Added
- **Multi-line progress display**: `indicatif::MultiProgress` shows concurrent bars — one per active reindex stream — plus a summary line with aggregate `chunks/s`
- **Blue-green verify**: after a reindex completes, a lightweight verification pass confirms the new HNSW index answers a canary query before swapping the live handle; prevents silent corruption on large repos
- **Incremental index flag** (`--incremental`, default on): skips files whose sha2 fingerprint matches the stored value even across daemon restarts; `--force` still triggers a full rebuild

---

## [0.1.44] — async HTTP server in trusty-common

### Added
- `trusty-common`: `server` module with `with_standard_middleware` (axum-server feature) and `daemon_http_client` helper
- `trusty-search-service`: `build_router` uses `with_standard_middleware`
- `main.rs`: all daemon HTTP call sites use `trusty_common::server::daemon_http_client`

---

## [0.1.43] — HTTP timeouts (fix status hang)

### Fixed
- Add 2s connect / 5s request timeouts to all daemon HTTP calls via `daemon_client()` helper; `status`, `health`, `doctor`, `query`, and `reindex` now fail fast with a clear error instead of hanging when the daemon is not running

---

## [0.1.42] — status/health unified + doctor command

### Added
- `status` and `health` are now aliases for the same `run_status()` handler; output shows daemon version, port, and per-index chunk counts
- `trusty-search doctor`: 6-check diagnostic (daemon liveness, model cache, data-dir writability, stale lockfile, empty indexes, port reachability) with colored ✓/⚠/✗ output
- `doctor --fix`: auto-repairs stale lockfile and empty indexes via `run_reindex` with progress bar; exits 1 on any error

---

## [0.1.41] — `index` primary command + indicatif progress bar

### Added
- `trusty-search index [PATH] [--name <id>] [--force]`: auto-registers the index if absent, skips if already indexed, `--force` triggers full reindex; replaces the awkward `init` + `reindex` two-step
- `indicatif` progress bar during reindex: `⟳ Indexing {id} [████░░] {pos}/{len} files — {eta} remaining`; updates on each SSE batch event, finishes with chunk count and elapsed time
- `register_index_with_daemon()` and `fetch_chunk_count()` helpers shared between `Init` and `Index` commands
- `init` and `reindex` preserved as backward-compatible aliases

---

## [0.1.40] — wire shared crates throughout

### Refactored
- `ui.rs`: replace inline OpenRouter HTTP client with `trusty_common::openrouter_chat` (~50 lines removed)
- All three shared crates (`trusty-mcp-core`, `trusty-embedder`, `trusty-common`) fully wired into every consumer
- Pin shared crates to public git tags on `bobmatnyc/trusty-common` (v0.1.0); remove in-tree copies (net −985 LOC)

---

## [0.1.39] — exclude minified JS/build dirs from indexing

### Added
- `should_skip_path()`: skips `*.min.js/css`, `*.bundle.js`, `*.chunk.js`, hashed bundles, binary extensions, files > 1 MB
- `should_skip_content()`: heuristic minification detection for `.js/mjs/cjs` (< 5 lines with any line > 500 chars)
- `SKIP_DIRS`: `node_modules`, `dist`, `build`, `target`, `.git`, `__pycache__`, `.next`, `.nuxt`, `.svelte-kit`, `vendor`, `.gradle`, `.m2`, `coverage`, `.nyc_output`
- Reindex emits SSE `"skip"` event with `reason:"minified"` for content-skipped files
- 14 new tests covering all skip patterns

---

## [0.1.38] — shared crates (trusty-mcp-core, trusty-embedder, trusty-common)

### Added
- `trusty-mcp-core`: `McpRequest`/`McpResponse`/`JsonRpcError`, error code constants, `run_stdio_loop` generic async stdio handler, CORS/Trace axum helpers
- `trusty-embedder`: `Embedder` trait, `FastEmbedder` with LRU cache + persistent model cache dir, `EMBED_DIM=384`, `MockEmbedder` for tests, `embed_one` helper
- `trusty-common`: `bind_with_auto_port`, `resolve_data_dir`/`cache_dir`, `ConcurrentRegistry<K,V>`, `init_tracing`, `maybe_disable_color`
- All three registered in workspace; 163 tests passing

### Refactored
- Adopt shared crates and delete inlined equivalents; `trusty-search-core::embed` becomes a thin facade over the shared `Embedder` trait
- Daemon port binding goes through shared async helper; `main.rs` uses `init_tracing`/`maybe_disable_color`

---

## [0.1.37] — daemon early-exit + model cache

### Fixed
- `is_already_running()` checks lockfile before `FastEmbedder::new()` so "another daemon running" exits in < 1 ms instead of after an 86 MB model download

### Added
- `model_cache_dir()` resolves `~/Library/Caches/trusty-search/models/`; model downloads once and loads from disk on all subsequent daemon starts
- `serial_test` on embed tests prevents `hf_hub` lock-file races in parallel test runs

---

## [0.1.36] — HTTP ↔ MCP functional parity

### Added
- Four missing MCP tools added for full HTTP endpoint coverage:
  - `delete_index` ← `DELETE /indexes/:id`
  - `reindex` ← `POST /indexes/:id/reindex`
  - `index_status` ← `GET /indexes/:id/status`
  - `chat` ← `POST /chat` (OpenRouter proxy)
- `test_tools_list_complete` asserts HTTP/MCP parity; 151 tests passing

---

## [0.1.35] — Svelte admin UI + MCP stdio server

### Added
- **Web management UI** served at `GET /ui`:
  - Collections panel: list/create/delete indexes, reindex with live SSE progress
  - Search panel: single and cross-collection hybrid search, `match_reason` badges, compact/full snippet toggle
  - Chat panel: OpenRouter-backed conversational Q&A (gated by `OPENROUTER_API_KEY`)
  - Admin panel: daemon info, per-file index/remove ops, danger zone
  - Static assets embedded at compile time via `include_dir`
- `POST /chat` endpoint proxies to OpenRouter with search context injection
- `DELETE /indexes/:id` endpoint
- `trusty-search ui` subcommand: start daemon + open browser
- **MCP stdio JSON-RPC server** (full JSON-RPC 2.0 over stdin/stdout, protocol 2024-11-05):
  - `initialize` handshake, `notifications/initialized` suppressed correctly
  - `tools/list`: all 6 tools (`search_code`, `index_file`, `remove_file`, `list_indexes`, `create_index`, `search_health`)
  - `tools/call`: MCP-spec content envelope with `isError` flag
  - Graceful shutdown on stdin EOF; errors to stderr only

---

## [0.1.34] — 4× faster indexing

### Performance
- Eliminate 452 symbol-graph rebuilds per reindex: `index_files_batch_no_rebuild` defers graph rebuild to once at completion
- `resolve_callee` O(N×S) linear suffix scan replaced with O(1) hash lookup using precomputed simple-name → `NodeIndex` map
- Batch size 32 → 128 for better ONNX saturation
- `drain` RawChunk corpus instead of cloning (saves ~115k allocations per reindex)
- Expected reduction on large monorepos: ~46 min → 2–4 min

---

## [0.1.33] — hot BM25/HNSW/LRU fixes + CLI stubs

### Fixed
- **Bug A**: Wire `FastEmbedder` + `UsearchStore` in `create_index_handler`; HNSW now actually stores and returns vector results
- **Bug B**: Replace per-query BM25 rebuild with persistent `Arc<RwLock<Bm25Index>>` maintained incrementally at index time; search is O(df_i) not O(corpus)
- **Bug C**: LRU embedding cache now deduplicates across requests (was masked by Bug B)

### Added
- `status` CLI: daemon health + per-index chunk counts
- `query` CLI: `POST /indexes/:id/search` with ranked output or `--json`
- `init` now calls `POST /indexes` on the daemon (fixes misleading "Registered" message)

---

## [0.1.32] — convert command

### Added
- `trusty-search convert project|all`: migrate indexes from mcp-vector-search by reading `.mcp-vector-search/config.json` files
  - `convert project`: git-style upward discovery from CWD
  - `convert all`: scans `~` at depth 6, skipping noise dirs
  - `--dry-run`: preview without contacting the daemon
  - `--concurrency`: bounds parallel migrations via `tokio::Semaphore`
  - Idempotent: existing indexes detected via daemon `{created: false}` response

---

## [0.1.31] — large codebase performance

### Performance
- `CodeIndexer::index_files_batch`: parses N files in parallel via rayon, embeds all chunks in 256-chunk ONNX batches, takes corpus write lock once per batch
- Incremental hash skip: files whose content hash matches the previous reindex are skipped; new SSE events: `"skip"`, `"batch"` (with `chunks_per_sec`), `"complete"` now carries skipped count
- `UsearchStore::with_capacity_hint`: tunes HNSW (connectivity=32, expansion_add=128, expansion_search=64) when expected chunk count > 50k
- `.gradle`/`.groovy`/`.kts`/`.mjs`/`.cjs` added to `SOURCE_EXTS`; Java/Gradle build dirs pruned from walker

---

## [0.1.30] — start/stop CLI

### Added
- `trusty-search start`: starts the HTTP daemon (replaces `daemon`)
- `trusty-search stop`: reads PID from fs4 lockfile, sends SIGTERM, polls up to 5s for port file to disappear

---

## [0.1.29] — reindex + SSE progress streaming

### Added
- `walker::walk_source_files`: walkdir-based, skips `.git`/`target`/`node_modules`/etc.
- `POST /indexes/:id/reindex`: spawns background reindex task with optional `{root_path}` body
- `GET /indexes/:id/reindex/stream`: SSE endpoint emitting `start`/`progress`/`complete`/`error` events with replay buffer for late subscribers
- `trusty-search reindex [PATH]` CLI: connects to SSE stream, renders live percentage/file progress
- `trusty-search add <PATH>`: walks directories and indexes every source file match
- `trusty-search remove <FILE>`: calls `/indexes/:id/remove-file`
- `trusty-search list`: calls `/indexes` and renders registry

---

## [0.1.28] — SCIP ingest interface

### Added
- SCIP ingest interface with `CodeEntityIndex` trait and `from_refs` constructor ([#24])

---

## [0.1.27] — ONNX NER gated

### Added
- ONNX NER for doc comment NLP entity extraction, gated by model file presence ([#23])

---

## [0.1.26] — ConceptCluster k-means

### Added
- `ConceptCluster` entities via fastembed + linfa k-means ([#22])

---

## [0.1.25] — complexity metrics

### Added
- Complexity and code quality metrics per chunk ([#32])

---

## [0.1.24] — search_similar

### Added
- Code-to-code similarity search and `search_similar` MCP tool ([#31])

---

## [0.1.23] — git blame integration

### Added
- Git blame integration per-chunk with temporal decay scoring ([#30])

---

## [0.1.22] — benchmark harness

### Added
- Benchmark harness: MRR@5 and Recall@10 evaluation ([#25])

---

## [0.1.21] — canonical facts table

### Added
- Canonical facts table with provenance tracking and HTTP query API ([#26])

---

## [0.1.20] — MMR diversity

### Added
- MMR (Maximal Marginal Relevance) diversity pass after RRF fusion ([#28])

---

## [0.1.19] — entity-match RRF lane

### Added
- Entity-match RRF lane for exact symbol name queries ([#20])

---

## [0.1.18] — KG rich edge types

### Added
- Knowledge Graph CALLS/IMPORTS/INHERITS/CONTAINS edges derived from chunk AST data ([#33])

---

## [0.1.17] — virtual_terms in BM25

### Added
- Populate `virtual_terms` from entities and append to BM25 documents for enriched lexical matching ([#19])

---

## [0.1.16] — intent-gated KG traversal

### Added
- Intent-gated KG traversal with `EdgeKind` score multipliers ([#18])

---

## [0.1.15] — EntityExtractor Phase A

### Added
- `EntityExtractor` Phase A: structural entities (functions, classes, imports) ([#17])

---

## [0.1.14] — CodeChunk extended fields

### Added
- Extend `CodeChunk` with `chunk_type`, `calls`, `inherits_from`, `complexity_score`, `chunk_depth` ([#29])

---

## [0.1.13] — BM25 three-pass tokenizer

### Added
- Three-pass BM25 tokenizer with camelCase and snake_case splitting ([#27])

---

## [0.1.12] — QueryClassifier entity keywords

### Added
- Extend `QueryClassifier` with entity-type keyword recognition ([#21])

---

## [0.1.11] — RawEntity + EdgeKind schema

### Added
- Canonical `RawEntity` schema and `EdgeKind` enum ([#16])

---

## [0.1.10] — CI + Dependabot

### Added
- GitHub Actions CI workflow and Dependabot config ([#9])

---

## [0.1.9] — daemon + graceful shutdown

### Added
- Daemon with PID lockfile (fs4), auto-port binding, graceful shutdown ([#8])

---

## [0.1.8] — MCP server

### Added
- MCP server with stdio and HTTP/SSE transport ([#7])

---

## [0.1.7] — FileWatcher

### Added
- `FileWatcher` with notify-debouncer-mini, 500ms debounce, fsevent backend ([#6])

---

## [0.1.6] — SymbolGraph KG expansion

### Added
- Build `SymbolGraph` from tree-sitter parse output; wire KG expansion (callers_of/callees_of) into the query pipeline ([#5])

---

## [0.1.5] — AST chunker + entity extraction

### Added
- Replace sliding-window chunker with tree-sitter AST-aware chunker ([#4])
- Initial `EntityExtractor` ([#17])

---

## [0.1.4] — search pipeline

### Added
- `CodeIndexer::search` end-to-end: HNSW + BM25 + RRF fusion ([#3])

---

## [0.1.3] — CLI redesign with auto-detection

### Added
- Project auto-detection and clean CLI help structure ([#14])

---

## [0.1.2] — UsearchStore HNSW wiring

### Added
- Wire `UsearchStore` to real usearch HNSW `Index` for add/search/remove ([#2])

---

## [0.1.1] — FastEmbedder implementation

### Added
- `FastEmbedder` with fastembed-rs + LRU cache ([#1])

---

## [0.1.0] — initial scaffold

### Added
- Workspace scaffold: `trusty-search-core`, `trusty-search-service`, `trusty-search-mcp`, CLI binary
- Query classifier (regex-based intent detection)
- BM25 lexical index (ported from open-mpm)
- `IndexRegistry` with `DashMap` + `Arc<RwLock<CodeIndexer>>`
- axum router skeleton

[Unreleased]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.36...HEAD
[0.3.36]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.35...v0.3.36
[0.3.35]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.34...v0.3.35
[0.3.34]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.33...v0.3.34
[0.3.33]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.32...v0.3.33
[0.3.32]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.31...v0.3.32
[0.3.31]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.30...v0.3.31
[0.3.30]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.29...v0.3.30
[0.3.29]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.28...v0.3.29
[0.3.28]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.27...v0.3.28
[0.3.27]: https://github.com/bobmatnyc/trusty-search/compare/v0.3.26...v0.3.27
[0.1.46]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.45...v0.1.46
[0.1.45]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.44...v0.1.45
[0.1.44]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.43...v0.1.44
[0.1.43]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.42...v0.1.43
[0.1.42]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.41...v0.1.42
[0.1.41]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.40...v0.1.41
[0.1.40]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.39...v0.1.40
[0.1.39]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.38...v0.1.39
[0.1.38]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.37...v0.1.38
[0.1.37]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.36...v0.1.37
[0.1.36]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.35...v0.1.36
[0.1.35]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.34...v0.1.35
[0.1.34]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.33...v0.1.34
[0.1.33]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.32...v0.1.33
[0.1.32]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.31...v0.1.32
[0.1.31]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.30...v0.1.31
[0.1.30]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.29...v0.1.30
[0.1.29]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.28...v0.1.29
[0.1.28]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.27...v0.1.28
[0.1.27]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.26...v0.1.27
[0.1.26]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.25...v0.1.26
[0.1.25]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.24...v0.1.25
[0.1.24]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.23...v0.1.24
[0.1.23]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.22...v0.1.23
[0.1.22]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.21...v0.1.22
[0.1.21]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.20...v0.1.21
[0.1.20]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.19...v0.1.20
[0.1.19]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.18...v0.1.19
[0.1.18]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.17...v0.1.18
[0.1.17]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.15...v0.1.16
[0.1.15]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.14...v0.1.15
[0.1.14]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.12...v0.1.13
[0.1.12]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/bobmatnyc/trusty-search/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/bobmatnyc/trusty-search/releases/tag/v0.1.0
