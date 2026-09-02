# Changelog — trusty-code

All notable changes to trusty-code are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [0.5.1] — 2026-09-02

### Changed

- `fs_browse`'s registry `repo_url` parse delegates to
  `trusty_common::github_path::parse_remote_url` instead of splitting the URL
  itself (#6657). A GitLab-style subgroup path is still rejected, so a
  registry entry can never fabricate an owner containing a slash. A
  port-qualified host (`https://git.example.com:8443/owner/repo`) now resolves
  to `owner/repo` rather than being rejected: it was rejected only because the
  local split treated the port's colon as the host/path boundary, and the shared
  parser does not.

## [0.5.0] — 2026-08-31

### Breaking

- `SessionRegistry::memory_sink_for` now takes `self: &Arc<Self>` instead of `&self` ([#2425](https://github.com/bobmatnyc/trusty-tools/issues/2425)), so it can mint a `Weak<SessionRegistry>` back-reference for the memory-durability observer it now attaches to each session's sink. Every in-workspace caller already held an `Arc<SessionRegistry>`, so no call site changed, but an out-of-tree caller holding a bare `&SessionRegistry` no longer compiles. `trusty-code` is not currently publishable to crates.io regardless (its `trusty-tui` dependency has no crates.io release), and its declared `0.3.0` already sits a full minor version ahead of the `0.2.0` last published — that gap predates this change and already carries other breaking signatures accumulated since `0.2.0` under Cargo's 0.x rule. This change lands inside that same unreleased `0.3.0` line, so no further version bump is owed for it alone; `bash scripts/check_semver.sh --crate trusty-code` confirms the receiver-type change via its advisory INVENTORY arm (`method_receiver_type_changed`) rather than failing a gate.
- **`TurnMemorySink` and `RecallSessionTool` take the trusty-memory socket path rather than a base URL** (#6286, ADR-0032), and `memory_envelope::call_tool_wrapped` takes a `&Path`. `TurnMemorySink::base_url` is `socket`
- The env override that pins the daemon is `TRUSTY_MEMORY_SOCKET`, not `TRUSTY_MEMORY_URL` — it names a socket path now, because there is no listener for a URL to address

### Added

- Synced `BASE-AGENT.md` from trusty-mpm: a "Never Directly Monitor a
  Declarative Process" rule — run gates in their quiet/filtered form, re-run
  only the failing case with full output, and never `gh pr checks --watch`
- Synced `BASE-AGENT.md` from trusty-mpm: a "No Subagent Fan-Out" rule — a
  delegated agent does its own work or reports back to its dispatcher; it
  never spawns its own subagents. The Agent/Task tool is reserved for the
  top-level PM/orchestrator
- Synced `BASE-AGENT.md` from trusty-mpm: "Agent-Authored Prose", extending
  the PM's "Write Plainly" register to review verdicts, reports, ticket/PR
  body text, and generated documentation
- Synced `BASE-AGENT.md` from trusty-mpm: Agent-Authored Prose adds "never
  announce the register you're writing in" — no heading or preamble
  labelling the writing as plain, honest, direct, candid, blunt, or
  unvarnished
- `health` (JSON-RPC and `GET /health`) now carries an `incremental_index`
  object with `dropped_batches`, `seconds_since_last_drop`,
  `truncated_batches`, and `seconds_since_last_truncation`
  ([#2798](https://github.com/bobmatnyc/trusty-tools/issues/2798)). The
  write/edit tool executors hand every successful write to a bounded background
  index pool, which loses work two ways once a degraded trusty-search daemon
  backs it up: it refuses a batch outright when full (a drop, nothing ran), or
  accepts one and then cuts it short at the 30s per-batch budget (a truncation —
  part of it landed and the files it had not reached are abandoned). Both were
  previously only a log line, so a sustained episode reported exactly as healthy
  as no saturation at all, and reporting only drops would still read `0`
  throughout an episode of repeated truncations. The two stay separate fields
  because they need different fixes. All four are additive — a client that reads
  only `server`/`version`/`status`/`pid`/`binding` is unaffected.
- **Assistant turns stream token-by-token to attached clients (issue #4425,
  epic #3696 Gap B).** `ToolEventSink::agent_message` is now called repeatedly
  per text turn — once per content chunk with `done: false`, then once with
  `done: true` and an empty delta marking the bubble complete — instead of once
  with the whole turn. A `session.attach`ed client (and therefore `tcode tui`)
  renders the assistant's words as they are produced rather than as one paste
  when the turn ends. A tool-only turn still emits nothing. Streaming engages
  only when a sink is attached: the `run-task` CLI path and every scripted test
  keep taking the blocking call unchanged, so their wire request is
  byte-identical to before.
- **`tcode tui` — the TUI REPL is reachable from the CLI (#4424, DOC-50 §4.1 /
  AC-2.4).** Every DOC-50 MVP slice was merged, but `CodeEngine` had zero
  production call sites: the whole TUI existed only under `cargo test`. The new
  subcommand discovers a running `tcode serve --http` daemon (`TCODE_DAEMON_URL`
  -> `http_addr` file -> `/health` liveness ping), then hands `CodeEngine` to
  `trusty_tui::run::run`. `--project` is optional (omit for a projectless
  session) and is canonicalized at the CLI boundary. Discovery runs BEFORE the
  alternate screen is entered, so any daemon-resolution failure lands on a
  normal terminal rather than flashing a TUI. As shipped this MVP did not
  auto-spawn a daemon; that was reversed in the same release — see the #4512
  entry below.

- **`Event::AgentMessageDelta` now has a producer (streaming epic #3696,
  Gap A / Slice 1).** The event contract landed in #3701, but no production
  code ever constructed it — `AgentMessageDelta` existed only in `events.rs`
  and its own tests, so every streaming consumer had nothing to consume. The
  agent loop now emits one `AgentMessageDelta` per assistant turn through the
  event sink (`agent_loop/sink.rs`, `task/sink.rs`,
  `session/registry_events.rs`), so assistant text reaches the session event
  stream as it is produced rather than only at turn end. This is the emit side
  only; the TUI and GUI consumers land separately (Slices 2 and 3).

- **The TUI renders assistant text as it streams (streaming epic #3696,
  Slice 2).** `forward_session_event` now maps `Event::AgentMessageDelta` onto
  `ReplEvent::AssistantOutput`, reusing the same chunk-append machinery
  `Message`/`AgentMessage` already drive — `done: false` appends to the live
  bubble, `done: true` finalizes it — so no new rendering path was needed in
  `trusty-tui`. An agent turn finishing is explicitly NOT terminal for the SSE
  pump: one agent completing does not end the session stream.

- **`ticketing` is now a bundled, dispatchable roster agent (#4027; epic
  #4021 bridge track).** `crates/trusty-code/src/assets/agents/ticketing.md`
  is a byte-for-byte port of `crates/trusty-mpm/src/assets/agents/ticketing.md`
  — the same treatment `research.md` already gets — so trusty-agents' widened
  cross-product `dispatch_task` bridge (#4026) reaches ONE roster instead of
  growing a second dispatch leg into trusty-mpm (the owner's OQ-4 ruling).
  `tcode run-task ticketing "<task>"` resolves through the existing
  `resolve_agent` embedded tier with no CLI change; the roster is now 33
  dispatchable agents and `EMBEDDED_TM_AGENT_SOURCES` 34 entries. It is a
  PARITY copy, not a pinned deviation: it carries no tcode-only `tools:`
  restriction, so `scripts/check_agent_assets.sh` keeps it in lockstep with
  upstream automatically. Its non-coding property is enforced where the owner
  put enforcement — the bridge's fail-closed `NON_CODING_TARGETS` floor in
  `crates/trusty-agents/src/tools/cross_product.rs` — not by an asset-level
  allowlist a direct `tcode run-task` invocation would bypass anyway.
- **`session.get_context_budget` now surfaces the real working-context
  floor, not just a point-in-time snapshot (#3912).** The load-realistic
  compression soak (epic #3866, PR #3909) proved the RPC's cached
  `working_context_pct` can read 98-99% while the durable
  `compression.jsonl` telemetry recorded a real floor of 48-60% for the
  SAME session — a coarse once-per-`task.run`-call poll reliably misses
  the turn where the floor was lowest. Added
  `agent_loop::telemetry::session_working_context_floor` (scans the
  durable JSONL, scoped to one session) and two new response fields,
  `working_context_pct_low_water_mark` / `working_context_pct_sample_count`,
  computed fresh on every `session.get_context_budget` query — mirroring
  `lifetime_compaction_alarm_count`'s existing "read fresh at query time"
  pattern exactly.
- **Standing regression guard for the #3902 `TCODE_TELEMETRY_DATA_DIR` race
  class (issue #3925).** PR #3920's fix for #3902 was verified only by the
  author's local 40x loop (2/40 failures pre-fix, 0/40 post-fix) — a manual
  process that lived nowhere in CI, so a FUTURE loop-integration test could
  reintroduce the same "forgot to inject `telemetry_data_dir`" omission and
  pass a single ordinary `cargo test` run before flaking later (exactly how
  #3902 reached `main` in the first place). New
  `agent_loop::tests::compression_telemetry_tests::concurrent_agent_loops_survive_data_dir_env_hammering`
  deterministically manufactures the exact attack instead of hoping a
  repeat-loop trips it: while holding `telemetry::DATA_DIR_ENV_LOCK` for its
  own body, it races a background task that rewrites
  `TCODE_TELEMETRY_DATA_DIR` with zero synchronization against four
  concurrent scripted `AgentLoop` runs, each injecting its own
  `telemetry_data_dir`. Because an injected loop never consults the env var,
  every run must see exactly its own cadence records regardless of how
  aggressively the global churns — validated to fail deterministically (not
  probabilistically) when one scenario's injection is reverted. Mirrors the
  identical pattern applied to trusty-agents' `EventStore`/`$HOME` race
  (issue #3922); kept as two crate-local tests rather than one shared
  harness since the two crates have no shared test-support crate and the
  domain types differ enough that sharing code would cost more than it
  saves.

- **A cadence-level 60%-floor breach now has a durable backstop alarm
  (#3911).** The load-realistic soak (epic #3866, PR #3909) proved that
  when `cadence::enforce_budget` exhausts every eligible entry and still
  exceeds the overhead cap (a real guarantee violation — floor 48% in the
  soak's exploratory run), NOTHING durable reacted: the #2308 threshold
  compactor's own alarm (`lifetime_compaction_alarm_count`) stayed at 0
  because it is keyed to a mechanically independent, laxer 75%-of-window
  trigger cadence's own breach never crosses. Added
  `agent_loop::telemetry::record_cadence_floor_breach` (writes a
  `tcode-cadence-floor-breach` JSONL row plus a durable alarm line) and a
  new `lifetime_cadence_floor_breach_count` field on
  `session.get_context_budget`, elevated the prior in-process `warn!` to
  `error!` at the call site. Re-running both `compression_load_soak.py`
  profiles confirms the backstop now fires on every observed breach (2/2
  in the primary run, 21/21 in the exploratory run) — see PR for the full
  residual-limits discussion (this is an alarm, not additional
  compaction capacity: the floor itself is unchanged).

- **(#3911 post-review fix) The floor-breach JSONL row no longer
  double-counts a real breach as two floor samples.** A breach writes BOTH
  a `tcode-cadence` row (the real measurement) and a
  `tcode-cadence-floor-breach` alarm row for the SAME turn; the breach row
  previously also carried its own `working_context_pct_after`, so any
  consumer aggregating "samples with a percentage" — `compression_report.py`'s
  `compute_context_floor` and `session_working_context_floor` (#3912) alike
  — counted one real breach twice (measured: 21 real breaches reported as
  42 below-target samples). `record_cadence_floor_breach` now always writes
  `None` for that field; the paired `tcode-cadence` row remains the one
  authoritative sample. Corrected re-run: the exploratory profile now
  correctly reports 21 below-target samples (not 42), matching the
  original soak's finding.

- `compression_report.py` now additionally reports the #3911 backstop's
  own fire count in a dedicated section, flagging a floor breach with
  zero backstop fires as a FINDING.
- **Load-realistic compression-effectiveness stress soak (epic #3866,
  follow-up to #3869/PR #3887).** New `TCODE_MOCK_LLM=echo-soak-load` mock
  LLM (`task::mock_llm_soak_load::SoakLoadEchoLlmClient`) sizes every
  `set_goal` turn (not one in six) at a realistic tool-output magnitude
  (~160-300 KB, approximating a `git diff`/`grep` dump/`cargo test` failure
  log), driven by a new harness
  (`crates/trusty-code/scripts/compression_load_soak.py`, reusing
  `compression_soak.py`'s daemon/RPC plumbing) that adds a session-fidelity
  check (`session.get_goals`/`session.get_transcript`/one more `task.run`)
  after the load-driving calls. Result: the measured working-context floor
  drops from #3887's comfortable 94-95% to exactly 60-61% under this load —
  right at the epic's target boundary — and a reproducibly heavier profile
  (not shipped as the default) breaches it outright (floor 48%). Full
  writeup: `docs/research/tcode-compression-load-soak-2026-07-25.md`.
- **Compression-effectiveness soak harness + scored report (epic #3866,
  Slice C #3869).** New `TCODE_MOCK_LLM=echo-soak` mock LLM
  (`task::mock_llm_soak::SoakEchoLlmClient`) drives a deterministic,
  offline PM loop for a 200+-turn soak against a real `tcode serve --http`
  daemon (`crates/trusty-code/scripts/compression_soak.py`), and a new
  stdlib-only report generator
  (`crates/trusty-code/scripts/compression_report.py`, unit-tested against
  a hand-crafted fixture) scores the resulting `compression.jsonl` against
  epic #2343's targets (working context >= 60% at all times,
  `compaction_events == 0`). First run's report:
  `docs/research/tcode-compression-effectiveness-soak-2026-07-25.md`
  (PASS).

- **Durable compression-effectiveness telemetry (epic #3866, Slice A #3867 +
  Slice B #3868).** New `agent_loop::telemetry` module appends one JSONL
  line per compression event to `~/.trusty-code/compression.jsonl` — one for
  every `tcode-cadence` fire (`AgentLoop::maybe_cadence_compress`) and every
  `tcode-threshold` fire (`AgentLoop::maybe_compact_transcript`), with
  `ts`/`session_id`/`surface`/`tokens_before`/`tokens_after`/`ratio`/
  `working_context_pct_after`/`overhead_pct_after`/`compaction_event`/
  `duration_ms`/`rounds` fields. Emission is best-effort (never fails the
  compaction it instruments). A threshold-compaction fire under
  `cadence: Some(_)` also appends a durable line to
  `~/.trusty-code/compaction_alarm.log` — the never-event alarm epic #2343
  expects to stay empty in steady state — and `session.get_context_budget`'s
  response gains an additive `lifetime_compaction_alarm_count` field
  (`ContextBudgetSnapshot`) reflecting that log's lifetime, cross-session,
  cross-restart line count, so the never-event check is reachable from the
  one RPC that's already the front door for budget state instead of a
  second `session.get_transcript` call.
- `BASE-AGENT.md` now carries the agent-facing half of the shared-working-tree
  rule: a file-mutating agent works in its own worktree and never
  `git checkout`/`git switch` in a directory it was handed, because a
  concurrently-dispatched sibling shares that git HEAD. It names
  `tm hook --pm-guard` as the enforcement so the deny message and the prose point
  at each other (#4480).
- **`GET /health` (and the `health` JSON-RPC method) now report the daemon's
  project binding and pid
  ([#4512](https://github.com/bobmatnyc/trusty-tools/issues/4512)).**
  Additive only — `server`, `version`, and `status` are unchanged, so existing
  probes keep working. A daemon binds exactly one `ProjectBinding` at
  `serve::build_router` time and holds it for its whole life, but published
  nothing about it, so a client had no way to tell which project a daemon it
  found was serving. `binding` uses the same `{state, root}` wire shape
  `Session` already serialises.
- Embedded `BASE-AGENT.md` bans ending a gate chain in a pipe — `cargo test … | tail` exits 0 on a failing suite — and gives the canonical redirect-then-echo form (mirrors trusty-mpm, [#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837))
- `POST /auth/sse-ticket?path=<stream>` mints a single-use ticket that expires in 30 seconds, for browser `EventSource` clients — which cannot send a header, and for which putting the durable token in the query string would write it into every access log. Minting requires the credential, so a ticket is never a way in; it is refused for any path but the two SSE streams, and the ticket it returns opens a `GET` of that exact path and nothing else, so a ticket read from a trace log cannot be spent on `POST /rpc` ([#5439](https://github.com/bobmatnyc/trusty-tools/issues/5439))

### Fixed

- Synced `code-review-standards` and the `code-critic` agent from trusty-mpm:
  `Promote` is no longer a filing engine — the default is fix-in-the-surfacing-PR
  or drop; `Promote` is reserved for genuinely separable, schedulable work,
  and the critic never files an issue itself or instructs anyone to
- Synced the `code-production-process` skill (and its `stage-architect.md`/
  `stage-critic.md` references) from trusty-mpm: the Architect stage no
  longer describes an "optional Phase 2 design critic pass" for
  `code-critic` — that contradicted the ratified code-critic dispatch
  standard. Early interface review routes to `code-analyzer` instead
- Removed a stale `.open-mpm/workflows/<name>.toml` fallback mention from the
  (still-unimplemented) `run-workflow` help text and corrected
  `build_info.rs`'s doc-comment example path, which is caller-supplied and
  was never actually `.open-mpm/state/build.json`.
- Retry transient inference failures inside the current assistant turn with a
  fixed three-attempt budget, while preventing retries after streamed text is
  visible and keeping the existing run deadline and re-delegation boundary.
- `session.get_transcript` reports a per-session `memory_durability` status — lifetime failed turns, the current streak, and the latest failure's category and time — and the registry logs a warning at the first and third consecutive failed turn, so a session whose durable history is thinning no longer looks identical to a healthy one ([#2425](https://github.com/bobmatnyc/trusty-tools/issues/2425))
- Turn-recorder warnings name a closed failure category instead of quoting the memory daemon's error text and `memory_remember` `reason` back, either of which can carry a preview of the very credential the #2520 secret gate refused to store ([#2425](https://github.com/bobmatnyc/trusty-tools/issues/2425))
- An outcome the reorder reconciler refuses is counted in `memory_durability.unrecorded_outcomes` and reported by the observer instead of being discarded, so exceeding the bound no longer erases the degradation signal it was meant to surface ([#2425](https://github.com/bobmatnyc/trusty-tools/issues/2425))
- **The test suite no longer registers its fixture directories in the
  developer's live trusty-search daemon, which was writing index files back
  into the sandbox and failing unrelated `run_task` tests
  ([#3036](https://github.com/bobmatnyc/trusty-tools/issues/3036),
  [#3195](https://github.com/bobmatnyc/trusty-tools/issues/3195)).**
  `trusty_common::search_index` refuses daemon writes while
  `running_under_test_harness()` holds (#4255), but that answers for the
  running process, and `env!("CARGO_BIN_EXE_tcode")` is `target/<profile>/tcode`
  — outside `deps/` — so a spawned child looked like a real user invocation and
  warmed its `--project` into whatever daemon it discovered. The daemon then
  wrote `.gitignore` and `.trusty-search/{index.redb,hnsw.usearch,…}` into the
  test's own tempdir, at a moment nobody controlled, breaking whichever
  before/after diff assertion was open — which is why a different test failed
  each run, only on a machine with a daemon, and never under
  `--test-threads=1`. `tests/support/mod.rs` set `TRUSTY_TEST_HARNESS=1` on the
  two children it owned; 28 call sites across 24 test functions built their own
  `Command` and did not. All of them now go through
  the one guarded constructor `support::tcode_command()`,
  and `no_test_spawns_the_tcode_binary_unguarded` fails the build if a new test
  names the binary directly. One `cargo test -p trusty-code` used to leave 3
  new indexes behind in the operator's daemon; it now leaves none.
- **A failed `chat_stream` handshake no longer punches a hole in the
  debug-capture turn sequence (issue #4425, review finding 1).**
  `DebugCaptureLlmClient::chat_stream` reserves its `turn_index` before opening
  the stream, and an open failure used to propagate with `?` — so the index was
  consumed but never written, leaving a permanent GAP in
  `TCODE_DEBUG_TRANSCRIPT`'s sequence and losing the only record of the request
  that failed. The blocking `chat` path records in all cases; the streaming path
  now matches it exactly. Pinned by
  `stream_open_failure_records_its_reserved_turn_index`, which asserts the
  recorded indices are contiguous from zero across a failed stream-open and a
  following successful turn.
- **A streaming failure is captured with its ORIGINAL error variant, not a
  re-wrapped `Transport` (issue #4425, review finding 2).** The stream's error
  branch synthesised `InferenceError::Transport(e.to_string())` for the record —
  the one variant that is always retryable and never an alarm — so a
  missing-config or auth failure appeared in the transcript as a transient
  network blip and `is_retryable`/`is_alarm` became underivable from the
  capture. The record now carries the real error, and each failure record gains
  explicit `error_retryable` / `error_alarm` booleans so classification is
  readable without re-parsing Display text (the record cannot hold an
  `InferenceError` directly — the shared enum is not `Serialize`).
- **A multi-provider adapter answers capability questions for the provider the
  request actually routes to (issue #4425, review finding 3).**
  `OpenAiCompatClient` and `DispatchingLlmClient` both hard-wired
  `capabilities()` to OpenRouter's profile even though they route per request
  across Fireworks, Together, AtlasCloud, and Bedrock — reporting
  `detailed_usage_accounting: true` and OpenAI-dialect tooling for backends that
  honour neither, and handing compaction OpenRouter's 200K context tier for a
  128K backend. Both now override the new model-aware
  `InferenceAdapter::capabilities_for(model)`, resolving through the SAME gate
  their `chat`/`chat_stream` route on, so capabilities can never disagree with
  where the request is sent. `BedrockChatClient` and the
  `delegating_adapter_identity!` decorators forward it, so a recorder or
  debug-capture wrapper cannot collapse the routing back to one provider. The
  model-free `capabilities()` still answers for the routing default — the
  backend an unprefixed slug genuinely reaches — and is documented as such.
  #4426 builds Bedrock streaming on this surface.
- **The TUI's initial session-events connect could hang forever, bypassing the
  reconnect budget (#3494).** `build_http_client()` sets `connect_timeout(5s)`,
  which bounds only the TCP handshake — a daemon that accepts the connection but
  never sends response headers (wedged, deadlocked, or slow-lorising) left
  `pump_session_events`'s `GET /sessions/{id}/events` `.send().await` pending
  indefinitely. `SESSION_STREAM_MAX_RECONNECTS` never applied, because that
  budget only starts counting once `.send().await` resolves. A per-request
  `.timeout(CONNECT_TIMEOUT)` (10s) now bounds connect-through-headers only; the
  body read stays governed by `SSE_IDLE_TIMEOUT`. A timed-out connect surfaces as
  an ordinary transport error, so it counts against the reconnect budget like
  every other failure rather than being a silent-hang path of its own.

- **`TurnCapExceeded` permanently un-resumed a session (#3888).** A
  session whose `task.run` call exhausted its per-call
  `AgentLoopConfig::max_turns` budget fell through
  `task::executor::run_and_record`'s terminal-status match into
  `SessionStatus::Failed`, which `SessionRegistry::begin_execution`
  permanently rejects — directly regressing epic #2343's infinite-sessions
  goal. Added a new resumable `SessionStatus::TurnCapExceeded` status
  (wire value `turn_cap_exceeded`), mapped `AgentLoopError::TurnCapExceeded`
  onto it instead of the `Failed` catch-all, and extended
  `begin_execution`'s resume condition to accept it exactly like
  `Finished` — a capped session's PM transcript (already persisted
  unconditionally) now continues on its next `task.run`.
- **`TCODE_TELEMETRY_DATA_DIR` cross-test race closed via dependency
  injection (issue #3902).** Several `agent_loop` loop-integration tests
  (`budget_tests::cadence_emits_context_budget_snapshot`,
  `tests::cadence::cadence_fires_in_daily_driver_when_configured`,
  `tests::cadence::cadence_fire_logs_info`,
  `tests::cadence::cadence_none_threshold_fire_does_not_log_error`,
  `tests::daily_driver_mode_compacts_long_running_loop`) triggered a real
  cadence or threshold-compaction fire without isolating
  `telemetry::default_data_dir()`'s process-global `TCODE_TELEMETRY_DATA_DIR`
  env var behind `telemetry::DATA_DIR_ENV_LOCK`. Under `cargo test`'s
  parallel scheduling, one of these un-isolated fires could land its
  telemetry write inside a DIFFERENT, properly-isolated test's temp
  directory — reproduced locally at ~5% (2/40) under default parallelism,
  matching PR #3896 CI's `cadence_disabled_writes_no_cadence_telemetry`
  failure exactly (same spurious `tcode-cadence` record). New
  `AgentLoopConfig::telemetry_data_dir` field lets a caller inject this
  loop's own write target directly instead of relying on the shared env
  var; every loop-integration test that needs isolation now sets it, so
  concurrent test runs can no longer race each other over telemetry writes.
  Production behaviour is unchanged (`None` falls back to
  `telemetry::default_data_dir()` exactly as before). A `#[cfg(test)]`-only
  guard in `AgentLoop::telemetry_data_dir()` now panics on the FIRST run of
  any FUTURE test that reaches a real telemetry write with neither
  `telemetry_data_dir` injected nor the env var set, turning the next
  instance of this omission into a deterministic failure instead of a ~5%
  flake — this guard immediately caught a genuinely new instance of the
  same omission in `task::executor::tests::spawn_task_run_turn_cap_exceeded_is_resumable`
  (landed on `main` via the unrelated #3898, concurrently with this fix),
  which is now closed the same way: `TaskRunParams` gains a
  `telemetry_data_dir` field (`None` on the one production call site,
  `task::protocol::task_run`) threaded into `run_and_record`'s
  `AgentLoopConfig`, and the shared `executor_tests.rs::params()` test
  helper now injects an isolated dir unconditionally so no future test
  built on it can reintroduce the omission either.

- **Test-only ambient-daemon leaks closed for two `run_task` tests (issue
  #2914).** `spawns_indexing_thread_for_non_git_project_path` and
  `background_indexing_invokes_readiness_observer` call
  `ensure_project_indexed_in_background` directly, bypassing the
  `execute_run_task` wrapper's `isolate_ambient_daemons()` guard (#3361) — on
  a machine with a live trusty-search daemon they registered their tempdir
  fixture against it for real. Both now install the same isolation guard.
  Production behaviour (task-start indexing still opts in to
  `allow_sensitive_path: true` for its own working project, issue #2747) is
  unchanged — see `trusty-common`'s changelog for the shared-helper fix this
  pairs with.
- **`tcode tui` refuses to attach to a daemon serving a different project
  ([#4512](https://github.com/bobmatnyc/trusty-tools/issues/4512)).**
  Auto-attach picks daemons up off a well-known address, so without a check a
  TUI launched in project B would silently drive project A's daemon and every
  session, index, and file operation would land in the wrong repository. The
  client now compares its own project against the binding the daemon reports on
  `/health` and, on a mismatch, fails with an error naming BOTH projects and
  the ways forward. It deliberately does NOT fall back to spawning a competing
  daemon on a port that is already in use. Projectless and project-bound are a
  mismatch in either direction: attaching a projectless TUI to a bound daemon
  would grant it a project nobody named, and attaching a bound TUI to a
  projectless daemon would withdraw the indexing and git affordances the
  operator asked for. A daemon too old to report a binding is refused as well
  (fail-closed — "old build" is no evidence the project is right), with a
  message saying to restart it.
- **Boot reconciliation prunes dangling workstream `session_ids` (issue
  #4579).** `WorkstreamStore` persists each workstream's `session_ids`, but
  `SessionRegistry` is in-memory only, so after a daemon restart every
  persisted id referenced a session that no longer exists and was never cleaned
  up. `reconcile_on_boot` now takes the live session-id set and drops, from
  every record, any id absent from it — a live id is always kept, and no
  workstream record is ever removed (AC-6.1 unchanged). At the real boot site
  the registry is empty, so all stale references are pruned; the change is
  persisted only when something actually changed, and never touches a record's
  `updated_at`.
- **A missing `OPENROUTER_API_KEY` again reports an actionable error (issue
  #4614).** OpenRouter is tcode's default route, so it is the first credential
  a new operator has to set — yet since #4436 deleted the `convert::map_error`
  bridge, an absent key surfaced as the shared resolver's bare
  `MissingCredential { provider: OpenRouter }` ("no credential resolved for
  provider openrouter"), which names nothing the operator can act on.
  `build_adapter` now guards the OpenRouter route explicitly, exactly as it
  already did for Fireworks, Together, and AtlasCloud, and returns
  `MissingConfig` naming the env var and the three ways to set it. This is the
  contract both `llm::client::build_adapter` and `llm::dispatch::chat` have
  documented throughout; only the code had drifted.
- **The missing-credential tests no longer pass without asserting anything
  (issue #4614).** All four `missing_*_key_errors_*` tests returned early
  whenever the corresponding API key was present in the ambient environment, so
  on any developer machine holding a real key they reported `ok` having
  executed zero assertions — which is why the regression above went unseen
  locally and was visible only in CI. Each test now clears its own key for the
  duration of the test body (`#[serial]`, restored on drop) so the assertions
  run unconditionally, and no live API call is reachable.
- **Turn recorder no longer mints one memory palace per session (issue
  #4638).** `SessionRegistry::memory_sink_for` derives the recorder's palace id
  from the session's project root, so a session bound to a `tempfile::TempDir`
  produced a per-RUN-unique id (`t-tmp<random>` on macOS) and the drain task's
  `ensure_palace` step (#2424) auto-created that palace on the live
  trusty-memory daemon at the first turn. Nothing could ever read it back — the
  root it was keyed to is swept when the run ends — so the orphans accumulated
  at ~250-300/day: 5,667 in three weeks, 97.8% of every palace on the affected
  machine, which turned trusty-memory's O(n) full-registry HTTP handlers
  (#4637) into a ~90-minute `GET /api/v1/status`. The sink now carries an
  explicit `PalaceCreation` entitlement, `Allowed` only for a durable project
  root and `Forbidden` for one under a system temp root, making `palace_create`
  structurally unreachable from an ephemeral root rather than merely unlikely.
  N sessions therefore mint at most ONE palace — the project's — reused by
  every session on that project and shared with the PM catch-up digest, with
  sessions distinguished inside it by `chat_turn_append`'s `session_id` and
  `memory_remember`'s `session:<id>` tag exactly as `recall_session` (#2348)
  already expects.
  - Recording itself is unchanged: a `Forbidden` sink is still constructed (so
    the PM's `recall_session` tool surface does not change for a temp-rooted
    run) and still dual-writes into a palace that already exists — only
    bringing a NEW one into being is withheld. When its palace is absent the
    turn is dropped rather than written, since `memory_remember` against a
    missing palace fails `-32603` anyway; the probe is re-issued each turn so a
    palace created out-of-band mid-session is picked up.
  - The dominant real-world source was the crate's own test suite: dozens of
    tests across `task::executor_tests`, `task::protocol_tests`, and
    `tests/*_e2e.rs` drive sessions against `TempDir` project roots while the
    sink resolves the LIVE daemon URL, so every `cargo test -p trusty-code` on
    a machine with a running trusty-memory daemon permanently leaked palaces.
    The fix is inside the daemon rather than in test-harness env isolation, so
    it also protects an end user who binds a session to a temp directory.
  - Pre-existing `t-tmp*` palaces are untouched — this change never deletes
    anything and degrades gracefully if it encounters one (a `Forbidden` sink
    whose palace happens to exist simply writes into it).
- The project picker scans the configured workspace root instead of a hardcoded `~/trusty-mpm-projects`, so retargeting the root via `TRUSTY_MPM_WORKSPACE_ROOT` or `workspace_root_template` no longer makes every real checkout look missing (#5203).
- `derive_palace_id_for_project` routes through `trusty_common::palace_resolve` instead of probing the git remote itself and calling the pure three-level core, so a session's turns land in a project's PINNED palace rather than the derived one ([#5811](https://github.com/bobmatnyc/trusty-tools/issues/5811))
- An unset or malformed `RUST_LOG` now filters at `info` instead of dropping everything below `error`, so the `warn!` decision points the logging convention requires are visible on a default developer machine; `DEFAULT_LOG_LEVEL` is the real fallback rather than advisory documentation ([#5928](https://github.com/bobmatnyc/trusty-tools/pull/5928))
- **`tcode serve --http` now routes through the shared origin guard, so a web
  page cannot drive or read the daemon cross-origin
  ([#6003](https://github.com/bobmatnyc/trusty-tools/issues/6003)).**
  `serve::http::build_axum_router` called
  `trusty_common::server::with_standard_middleware` — permissive CORS
  (`allow_origin(Any)`) and no origin guard — while every sibling daemon used
  the guarded stack. The daemon defaults to a fixed loopback port and `tcode
  tui` auto-spawns it, so any page open in the operator's browser could POST
  cross-origin to `/rpc`, `/tasks`, `/sessions`,
  `/sessions/{id}/messages`, `/agents` and the workstream write routes, and read
  `GET /sessions/{id}/transcript` plus both SSE streams — conversation content.
  The router now calls `with_guarded_middleware_same_origin_cors` with
  `SelfOrigins::default()`: a write carrying a foreign `Origin` gets `403`
  before any handler runs, and the CORS policy reflects only same-machine
  origins so a foreign page cannot read the `GET` surface either. Callers that
  send no `Origin` — `tcode tui`'s own client, the console reverse proxy,
  `curl` — are unaffected, and a loopback `Origin` still passes. This is CSRF
  and read-disclosure defence, not caller authentication;
  [#5439](https://github.com/bobmatnyc/trusty-tools/issues/5439) stays open for
  that.
- The catch-up tests' unreachable-daemon constant is a socket path. `UNREACHABLE_MEMORY_SOCKET` still held `"http://127.0.0.1:1"` after the trusty-memory HTTP-to-UDS migration; `pm_catchup_context_with_socket` takes a `PathBuf`, so that string coerced into the working-directory-relative path `http:/127.0.0.1:1`. The dial failed either way, but by accident of the working directory rather than by the guarantee the doc comment claimed. It now uses `/nonexistent/trusty-memory/trusty-memory.sock`, the same literal `memory_rpc::UNREACHABLE_PLACEHOLDER` and `run_task::tests::isolate_ambient_daemons` use ([#6286](https://github.com/bobmatnyc/trusty-tools/issues/6286))

### Performance

- `session.get_context_budget` no longer rescans the multi-session
  `compression.jsonl` on every call; each session's working-context low-water
  mark is retained incrementally as measurements arrive. The durable JSONL is
  unchanged and remains the offline-history source (#3948).
- embedded agent assets track trusty-mpm's trimmed `skills:` frontmatter, cutting the skill bodies the harness renders into every tcode agent dispatch (see [#4642](https://github.com/bobmatnyc/trusty-tools/issues/4642))
  - the four pinned `tools:`-restricted copies (`qa`, `web-qa`, `code-critic`, `code-analyzer`) were reconciled by hand and re-pinned; their read-only deviation is unchanged

### Changed

- The embedded `BASE-AGENT.md` copy tracks trusty-mpm's consolidation of the
  throat-clearing and framing-opener prose rules into one significance-framing
  ban, keeping the two byte-identical.
- `ticketing` agent's default model tier is now `sonnet`, up from `haiku`,
  matching the trusty-mpm bundled default (kept byte-parity by
  `scripts/check_agent_assets.sh`). Duplicate-detection and scope-boundary
  judgement are judgement calls, not clerical ones.
- **trusty-code no longer defines its own LLM/provider abstraction; it consumes
  the shared one (issue #4425, epic #4429).** `llm::LlmClientTrait` is deleted
  and every call site now depends on
  `trusty_common::inference::InferenceAdapter` — the same trait trusty-review
  and the shared OpenRouter/Fireworks/Together/AtlasCloud/Bedrock adapters
  already implement. With it go trusty-code's duplicate wire types
  (`ChatRequest`, `ChatResponse`, `ChatMessage`, `ToolDefinition`, `UsageBlock`,
  `LlmError`) and the `llm::convert` bridge that existed solely to translate
  between the two copies; `crate::llm` now re-exports the shared types, so
  existing `crate::llm::…` import paths are unchanged. This is what unlocked
  streaming: the shared trait already carried `chat_stream` with native SSE,
  and the deleted local trait had no streaming method at all. Net −478 lines
  across the two crates (−2,409 removed, +1,931 added, most of the additions
  being the shared `StreamAssembly` and its tests).
  `LlmError` is renamed to `InferenceError` and its `ApiError` variant to `Api`
  (rlib-consumer-visible; no in-tree consumer outside trusty-code).
- **`OpenAiCompatClient`, `DispatchingLlmClient`, `BedrockChatClient`, the
  transcript recorder, and the debug-capture decorator all implement
  `chat_stream` explicitly.** The trait's default would have buffered through
  `chat`, which would have silently disabled streaming on exactly the
  production paths (`run_task`, `task::executor`, and anything run with
  `TCODE_DEBUG_TRANSCRIPT` set) that wrap the transport in a decorator.
- **A `chat_stream` handshake failure is propagated, never silently retried as
  a blocking call.** A degraded-but-working tcode would make "is streaming
  working?" unanswerable from the outside.
- **Bedrock streaming is the buffered fallback for now.** A `bedrock/*` turn
  arrives as one content delta plus the terminal one — the pre-existing
  behaviour, with no regression. Its real `ConverseStream` transport is #4426,
  and lands entirely inside `trusty_common::inference::bedrock`.
- `BASE-AGENT.md` now directs agents to a per-PR changelog fragment file
  (`<package>/changelog.d/<number>-<slug>.md`) in preference to editing a shared
  `## [Unreleased]` section ([#4476](https://github.com/bobmatnyc/trusty-tools/issues/4476))
  - The fallback is conditioned on the project having no `changelog.d/` at all,
    rather than on the directory existing at that moment. A release used to
    delete the directory, which sent the very next PR back to editing
    `## [Unreleased]` and then blocked the following release.
- **`events_connect_hang_is_bounded_by_connect_timeout_not_infinite` waits out
  `CONNECT_TIMEOUT` on Tokio's virtual clock** — 10.01s to 0.01s. The pause is
  taken only AFTER the listener confirms it accepted the pump's connection, so
  the #3494 "TCP accepted, headers never sent" precondition is established for
  real before time is faked. A blanket `#[tokio::test(start_paused = true)]`
  here is NOT equivalent and was rejected on measurement: it lost the accept on
  6 of 10 runs (Tokio auto-advances while the handshake is still in flight),
  quietly downgrading this to a plain connect-timeout test while still passing,
  because both failure modes produce a "connection failed" reason. With the
  ordering above the emitted reason string is byte-identical to the real-clock
  run, and removing `.timeout(CONNECT_TIMEOUT)` from `pump_session_events`
  still fails the test.
- **`main.rs` split: the `--legacy-in-process` run-task path moved to
  `cli::legacy_run_task` (issue #4434).** `main.rs` had reached 498 of the
  mechanically-enforced 500-SLOC production cap after #4424 added
  `Command::Tui`, so the next change to the file — any change — would have
  failed `scripts/check_line_cap.sh` before it started. The legacy in-process
  wrapper (`run_task`) and the two helpers only it uses
  (`validate_agent_name`, `build_llm_client`), together with their tests and
  the `TCODE_ENGINEER_MODEL` constant, now live in
  `crates/trusty-code/src/cli/legacy_run_task.rs` next to every other
  subcommand handler; `main.rs` is 335 SLOC of clap definitions plus
  dispatch. Behaviour-preserving: no test expectation changed, and the moved
  code is byte-identical apart from its module docs.
- **`tcode tui` now starts its own daemon instead of exiting when none is
  running, and never stops it
  ([#4512](https://github.com/bobmatnyc/trusty-tools/issues/4512)).**
  The subcommand shipped in #4424 (PR #4433) requiring an already-running
  `tcode serve --http`: discovery failed, an actionable "start one first"
  message printed, and the command exited. DOC-50 §4.1 had deferred auto-spawn
  to Phase 2; that deferral is reversed — an interactive command that demands
  the operator hand-start a background service first is not a shippable
  first-run experience.
  - Discovery is UNCHANGED: `TCODE_DAEMON_URL`, then the `http_addr` discovery
    file, each verified with a `GET /health` liveness ping. A live daemon
    serving the same project is attached to exactly as before.
  - A missing or stale discovery file now spawns `tcode serve --http` as a
    child, forwarding `--project <path>` when the TUI has one and omitting it
    for a projectless session so the daemon's binding matches the TUI's. The
    binary is resolved via `std::env::current_exe()` (`cli::tcode_exe::resolve`),
    so a locally built binary spawns itself rather than a stale `PATH` copy.
    Readiness reuses the shared
    `trusty_common::daemon_guard::spin_until_ready` spinner rather than a fourth
    hand-rolled poll loop, raced against the child's exit so a daemon that fails
    to bind is reported immediately instead of spinning out the 20s budget.
  - **Quitting the TUI never stops the daemon — including one the TUI itself
    started.** The tcode daemon owns PM lifecycle, agent dispatch, and agent
    communication, and CLIs/TUIs *attach* to it, so a client exiting must not
    destroy live PM or agent work (owner directive, 2026-08-01). There is no
    client-side teardown of any kind: no ownership tracking, no SIGTERM, no
    `kill_on_drop`. A daemon `tcode tui` spawned keeps running afterwards
    exactly like one started by hand. Quiescence-gated idle exit — a daemon
    that stops itself once it has no attached clients AND no active PM/agent
    sessions — is separate follow-up work and is deliberately not implemented
    here.
  - A `TCODE_DAEMON_URL` that is set but unreachable is still an ERROR, not a
    spawn: starting a daemon at the default port would silently ignore an
    address the operator named explicitly. The message names the dead URL and
    both ways forward.
  - The spawned daemon's stdout/stderr go to
    `{data_dir}/trusty-code/tui-spawned-daemon.log` rather than being inherited
    (which would scribble across the alternate screen) or null-ed (which would
    make a failed startup undiagnosable); startup errors name that file.
  - New `cli::daemon_autospawn` holds the whole policy, keeping `cli::tui` the
    thin wiring file it was. `tui_client::discovery` gained `lookup_daemon` +
    `Lookup`/`Source`, and `discover_daemon_url` is now a wrapper over it —
    needed because auto-spawn must distinguish an explicit instruction it has
    to obey from a stale file it may replace.
- Credential resolution now imports from `trusty_common::credentials` instead of
  `trusty_common::inference::credentials`, which was deprecated in the same
  change (see [#4564](https://github.com/bobmatnyc/trusty-tools/issues/4564)).
  Import-path churn only — no behaviour, precedence, or credential surface
  changes in this crate.
- `BASE-AGENT.md` gains two prose rules and repoints its voice standard at the output style rather than the retired `sections/core.md` block ([#4574](https://github.com/bobmatnyc/trusty-tools/issues/4574))
  - don't justify the restraint — "I don't know yet" is the whole answer; the trailing "I'm not going to guess" is process narration in a caveat's costume
  - no trailing emphatic negation — "— not before" restates the sentence by negating its opposite
  - kept byte-identical to `crates/trusty-mpm/src/assets/agents/BASE-AGENT.md`, as it has always been
- Embedded `rust-engineer` agent asset re-synced with its trusty-mpm source,
  which now scopes the Quality Bar to the crate under change
  (`cargo test -p <crate>`) and states the "scope is for speed, never for hiding
  a failure" rule. Byte-parity with `crates/trusty-mpm/src/assets/agents/` is
  preserved.
- The embedded `code-review-standards` skill picks up the three-way review-finding disposition, keeping it byte-identical to its trusty-mpm source. Every finding ends as `Fix here`, `Parent`, or `Promote`; the verdict template's Findings table carries a Disposition column and a blank cell is an incomplete review; and an APPROVE verdict does not generate tickets for its non-blocking observations (#4633).
- The bundled `git-workflow` skill now shows a throwaway worktree
  (`git worktree add .claude/worktrees/baseline-$$ origin/main`) as the way to
  get a temporary clean tree, and its "Stashing Work" section saves under a ref
  and restores by that ref rather than a bare `git stash` followed by a blind
  `pop`. Kept byte-identical to trusty-mpm's copy of the same skill (#4730).
- the embedded agent-catalog mirror tracks trusty-mpm's roster change
  ([#4760](https://github.com/bobmatnyc/trusty-tools/issues/4760))
  - `ops.md` is removed — it was superseded by `local-ops` upstream and
    deleted from the bundle there
  - `elixir-engineer.md` is added, covering general Elixir and OTP work
  - `phoenix-engineer.md` narrows to the Phoenix web layer, restoring
    byte-parity with its trusty-mpm source
- Bundled agent assets no longer prescribe a blocking CI wait: `BASE-AGENT.md`, `BASE-ENGINEER.md`, and `local-ops.md` now direct agents to push, take a ONE-SHOT `gh pr checks` / `gh pr view` status read, report, and end the turn ([#4792](https://github.com/bobmatnyc/trusty-tools/issues/4792))
  - `gh pr checks --watch` is forbidden — it streams check output into the agent's context. Own-gate commands (builds, test suites) still block in the foreground
  - Kept byte-identical to the `trusty-mpm` asset originals
- Refreshed the embedded `BASE-AGENT.md` agent asset to match trusty-mpm's terse rewrite, keeping the two copies byte-identical as `scripts/check_agent_assets.sh` requires. Register only — every rule is preserved, and the file gains the two new prose rules (no praise for the user; delete the framing opener)
- **MSRV raised to Rust 1.94** (was 1.91). `aws-config` >= 1.9.0 and
  `aws-sdk-bedrockruntime` >= 1.136.0, published 2026-07-08, declare
  `rust-version = "1.94.1"`; because those are unpinned caret ranges in the
  workspace manifest, `cargo install` **without `--locked`** re-resolves into
  them and then refuses to build on rustc below 1.94.1 — the reported
  `cargo install trusty-code` failure on rustc 1.91.1. Users on rustc
  1.91-1.93 must `rustup update` before installing any `trusty-*` crate. See
  [ADR-0029](../../docs/adr/0029-msrv-1-94-and-edition-policy.md)
  ([#4928](https://github.com/bobmatnyc/trusty-tools/pull/4928))
- embedded `BASE-AGENT.md` picks up the graduated-verbosity reporting rule — sparse on a clean pass, detailed when something went wrong — keeping byte-parity with the trusty-mpm source
- re-synced the mirrored `ticketing` agent asset with trusty-mpm: reopen a closed ticket for a recurrence before creating a new one, and write ticket bodies sparsely (defect, evidence, resolution)
- re-synced the mirrored `ticketing` agent asset with trusty-mpm: label every issue at creation with a type, one or more component/crate labels, and a priority only when the issue text asserts severity; leave the milestone unset unless scheduling into an open release
- the embedded `BASE-AGENT.md` copy tracks trusty-mpm's new `Never Narrate a Wait` section, keeping the two byte-identical for `scripts/check_agent_assets.sh` ([#5019](https://github.com/bobmatnyc/trusty-tools/pull/5019))
- `events::EVENT_LINE_PREFIX` is re-exported from
  `trusty_agents_common::events` rather than declared here. The emitted value is
  unchanged (`__OMPM_EVENT__ `); the second copy is what let the session
  manager's instructions drift onto a marker nothing emits
  ([#5129](https://github.com/bobmatnyc/trusty-tools/issues/5129)).
- `events::format_event_line` is now public, so the exact line `emit` writes to
  stderr is testable without capturing real stderr.
- The embedded `ticketing` and `BASE-AGENT` assets track their trusty-mpm originals byte-for-byte after the #5202 workflow/ticketing consolidation ([#5202](https://github.com/bobmatnyc/trusty-tools/issues/5202)). `ticketing` now owns the Issue end to end and no Pull Request operation at all — including the PR title and body, which previously read as its "bookkeeping" — and its deduplication step produces one of four named dispositions (`COMMENT`, `REOPEN`, `NEW REGRESSION`, `NO TICKET`) instead of reopening unconditionally on any recurrence.
- The embedded `BASE-AGENT.md` copy tracks trusty-mpm's new ban on
  borrowed-metaphor jargon — "load-bearing" and its category — keeping the two
  byte-identical (#5372).
- `BASE-AGENT.md` states the "honest" ban it previously only referenced,
  covering the word in every position including a heading modifier such as
  `<noun>, stated honestly:`, and folds the never-announce-your-register bullet
  into it. Byte-parity with trusty-mpm's copy is preserved (#5420).
- **`tcode serve --http` now requires a credential on every route.** The daemon bound loopback and merged its routes with no caller check, so any process on the machine — including a page in the operator's browser, which reaches `127.0.0.1` from inside that browser — could read sessions and transcripts and drive the mutation, task, filesystem, and catalog routes. `run_http` establishes a `0600` token under `resolve_data_dir("trusty-code")` before it binds, and refuses to serve if it cannot; a request without `Authorization: Bearer <token>` gets `401` with an empty body. The `tcode tui` engine and `TcodeConnector` read the same file, so no configuration is needed on a machine where both run as the operator. `TCODE_DAEMON_TOKEN` names the credential directly for a client that cannot read that directory — client-side only, and never sent to a non-loopback `TCODE_DAEMON_URL`. This is the hardening that stands until the listener is replaced by a Unix socket fronted by trusty-console (ADR-0032, the 2026-08-19 owner ruling): a `0600` file is an OS-user and browser-origin boundary, not isolation from an untrusted process running as the same uid ([#5439](https://github.com/bobmatnyc/trusty-tools/issues/5439))
- **`GET /health` discloses only `{"status":"ok"}` to an unauthenticated caller.** It used to answer `pid`, the bound project's absolute root path, the version, and the index counters to anyone. The route stays public so trusty-console's gateway can still poll liveness holding no credential; a caller that presents the credential gets the full payload unchanged, which is what keeps `tcode tui`'s auto-attach binding check (#4512) working ([#6472](https://github.com/bobmatnyc/trusty-tools/issues/6472))
- `code-critic`'s embedded copy is reconciled with trusty-mpm's upstream
  "post the verdict as a COMMENT-type GitHub review" change, reworded to fit
  its read-only `tools:` restriction: it hands the caller the exact
  `gh pr review --comment` command instead of running it directly, since this
  agent has no `bash`/`gh` tool. Re-pinned in `scripts/agent-asset-pins.tsv`
  per the E4 staleness guard (`scripts/check_agent_assets.sh`).
- The 30 embedded agent `.md` copies that duplicated trusty-mpm's are deleted;
  `assets` now embeds them from `trusty_agents_common::agent_assets`. The
  dispatchable roster and every agent's content are unchanged. The four
  deliberately forked copies (`code-analyzer`, `code-critic`, `qa`, `web-qa`,
  which add a read-only `tools:` restriction) and the four tcode-only defaults
  stay local — those forks are still pinned against the shared source by
  `scripts/check_agent_assets.sh`.
- The bundled `git-workflow` skill's throwaway-worktree section now says that under trusty-mpm orchestration worktree removal is PM-executed and `tm hook --pm-guard` denies an agent's `git worktree remove`. This is a doctrine sync, not new trusty-code work: the file is byte-identical to `trusty-mpm`'s copy of the same skill, and leaving the paragraph out of one copy would leave two versions of one document disagreeing about a rule the guard now enforces (Refs #5791).
- **MCP protocol primitives now come from the `trusty-mcp` crate instead of `trusty_common::mcp`** — imports move from `trusty_common::mcp::…` to `trusty_mcp::…`, and the `trusty-common/mcp` feature is replaced by a direct `trusty-mcp` dependency. The trusty-memory client is a separate case: `trusty_common::mcp::memory_rpc` became `trusty_common::memory_rpc`, still reached through the `catchup` feature. No behaviour change (ADR-0040, [#5803](https://github.com/bobmatnyc/trusty-tools/issues/5803))
- trusty-review is removed from `DEFAULT_HTTP_PORT`'s port-collision guard table. It binds a Unix socket rather than a TCP port since #6277 (ADR-0032), so reserving 7891 against it would only forbid a future daemon a free port. Comment-and-table only; `tcode serve --http` still defaults to 7882 ([#6277](https://github.com/bobmatnyc/trusty-tools/issues/6277))
- `tcode serve --http`'s `known_siblings` port-collision guard drops its 7879
  row: trusty-analyze no longer binds a port (#6287, ADR-0032), and a guard
  naming a port nothing binds refuses a value that is free.
- Removed the `trusty-mpm-supervisor` 7881 row from the `known_siblings` port-collision guard; that listener is retired and the port is free (Refs #6288).
- The `trusty-tui` dependency is renamed to `trusty-code-tui` (#6311), and the
  `tcode tui` client's imports move from `trusty_tui::` to
  `trusty_code_tui::`. The dependency still resolves to the same in-workspace
  path crate at the same version, so `tcode tui` behaves exactly as before.

### Documentation

- Repaired every broken rustdoc intra-doc link in this crate and added
  `#![deny(rustdoc::broken_intra_doc_links)]` to its crate root(s), so a new
  one fails the build instead of shipping as dead text on docs.rs (#5744).

## [0.3.0] — 2026-07-21

### Added

- **Claude Code plugin support, Phase 1: local-directory agents + skills
  (issue #3539, DOC-51).** New `crate::plugins` module auto-scans
  `<project_root>/.claude/plugins/<plugin>/` (honoring an optional
  `.claude-plugin/plugin.json`'s `name`/`agents`/`skills` overrides) and
  surfaces each plugin's `agents/*.md` and `skills/<name>/SKILL.md` in
  `agents.list`/`skills.list` under a new `plugin` tier, namespaced
  `<plugin>:<name>` and resolvable by that name via `agents::resolve_agent`
  and the `use_skill` skill resolver. Plugin entries are additive only — the
  namespaced key can never collide with (and so never overrides) a project
  or embedded/bundled name, and plugin skills are independent of the
  bundled-vs-project whole-catalog-replacement threshold (PR #3465) for
  `skills.list`. A plugin agent's unsupported trusty-mpm-style frontmatter
  fields (`effort`/`maxTurns`/`memory`/`isolation`/`disallowedTools`) are
  dropped with a warning rather than failing the load; an `extends:` chain
  is treated as leaf-only (warned, not composed). Phase 1 is local-directory
  agents + skills only — no marketplace/git fetch, no commands/hooks/MCP
  (later phases, tracked against #3539). Plugin content is treated as
  HOSTILE input throughout (code-critic PR #3547 review): a `plugin.json`
  `agents`/`skills` override that is absolute, contains `..`, or resolves
  outside the plugin directory is rejected and falls back to the default
  convention; every namespaced `<plugin>:<name>` dispatch/resolution path
  (`use_skill`, `delegate_to_agent`, `agent_config_exists`, `tcode run-task`)
  validates both segments against the existing `[a-z0-9-]+`
  (<= 64 chars) safe charset before ever building a filesystem path, making
  a traversal payload syntactically impossible to construct. A namespaced
  plugin agent is also now genuinely delegatable end-to-end — the PM's
  `delegate_to_agent` tool, the CLI, and the pre-flight existence gate all
  accept the `<plugin>:<name>` shape (previously only `agents::resolve_agent`
  itself could resolve one; every caller in front of it rejected `:`
  outright). A further hardening pass (code-critic re-review) closed a
  leaf-file-identity gap (CWE-59): a discovered `agents/*.md` or
  `skills/<name>/SKILL.md` — or the `skills/<name>` directory itself — that
  is a symlink escaping the plugin's `agents_dir`/`skills_dir` is now
  rejected via `plugins::path_is_contained` (canonicalize + containment,
  applied at both the listing and resolve/dispatch paths for both agents
  and skills) before its content is ever read, closing a host-file
  disclosure vector the directory- and name-level guards above didn't
  cover. `runner::agent_config_exists`'s pre-flight gate now also requires
  a namespaced plugin agent to resolve CLEANLY (not merely be found) to
  count as "exists".
- **Markdown transcript endpoint for dev observability (issue #3526).** New
  `GET /sessions/{id}/transcript.md` (`crate::serve::rest::sessions`) renders
  a session's full transcript as a readable `text/markdown` document —
  a header (session id, workstream id, project, task, mode, status, start/
  export timestamps, turn count, cost) then one section per turn, each
  `tool_calls` entry rendered as its own `` - `ROLE` ran: <tool> `` bullet so
  a runaway loop stays visible line-by-line. This makes a run inspectable in
  local dev independent of the GUI (`curl
  http://127.0.0.1:7882/sessions/<id>/transcript.md`) and is the single
  source of truth for the Markdown the Foundry GUI's "Download transcript"
  button saves. Session-scoped, loopback-only (no new bind — rides the
  existing daemon listener), `404` on an unknown id like the JSON transcript
  route.
- **Agent/Skill catalog management endpoints (issue #3449).** New `agents.*`/
  `skills.*` JSON-RPC methods (`crate::agents::protocol`,
  `crate::skills::protocol`) plus their REST twins — `GET`/`POST /agents`,
  `DELETE /agents/{name}` and the `skills` equivalent
  (`crate::serve::rest::agent_catalog`/`skill_catalog`) — back the Foundry
  GUI's new Agents/Skills management tabs. `GET /agents` returns the union of
  the embedded roster (32 agents incl. `pm`) and the resolved disk tier
  (`project` when bound, `user` for `~/.claude/agents` when projectless),
  disk overriding embedded by name; `POST`/`DELETE` manage the disk tier
  only, refusing to shadow or delete an embedded name (`403`) and refusing
  to overwrite an existing disk file (`409`). `GET /skills` returns whatever
  will ACTUALLY resolve for the project at `task.run` time — the bundled
  catalog when the project has no (or an empty) `.claude/skills/`, or
  EXCLUSIVELY that directory's entries once it has at least one, mirroring
  `discover_skill_metadata`'s whole-catalog-replacement semantics rather
  than a per-name overlay. There is no user-level skill tier
  (`crate::skills::protocol`'s docs explain why), so `POST`/`DELETE /skills`
  require a bound project (`400` when projectless). All names are validated
  against `[a-z0-9-]+` (also the path-traversal guard).
  **Code-critic PR #3465 review fixes (same-day, before merge):** both
  creates now use an atomic `O_CREAT|O_EXCL` create
  (`agents::protocol::write_new_file`) instead of an exists-then-write
  pre-check, closing a TOCTOU race where two concurrent creates of the same
  name could silently clobber each other (HIGH 1/HIGH 2); the 409 conflict
  is minted as `-32009 already_exists` via a new `RpcError::already_exists`
  constructor rather than reusing the workstreams' `-32008 active_conflict`
  literal (LOW); and `agents.list` surfaces an unparseable disk file as
  `tier: "broken"` instead of silently showing the shadowed embedded entry
  as healthy — `resolve_agent`'s disk-wins rule means dispatch of that name
  will fail, and the catalog must not misreport it (MEDIUM).
  **Code-critic PR #3465 RE-review fix:** `skills.list` was still doing a
  per-name bundled ∪ disk overlay, unlike the corrected `agents.list` — so a
  project with one custom skill reported all ~28 bundled skills as
  available even though `FsSkillResolver` discards the entire bundled
  catalog the moment disk has anything, making every bundled name
  unresolvable at runtime. Fixed to match the resolver's actual
  whole-catalog-replacement behavior (MEDIUM).

### Fixed

- **`run_task::tests` are hermetic again — no longer corrupted by an ambient local `trusty-memory`/`trusty-search` daemon (issue #3361).** `execute_run_task` background-indexes the project via `trusty-search` and seeds the PM prompt via `trusty-memory`'s catch-up digest; on a machine with either daemon actually running, both were discovered and contacted for real, and the live `trusty-search` daemon would register + reindex the test's tempdir project, writing its own colocated storage (`.trusty-search/schema_version.json`) INSIDE the sandbox. That extra file corrupted the before/after diff `no_changes_yields_no_changes_exit`, `missing_disk_pm_config_falls_back_to_embedded_pm`, and `exit_code_reflects_run_failure` assert on. `run_task::tests` now installs a one-time, process-lifetime isolation (via the existing `TRUSTY_MEMORY_URL`/`TRUSTY_DATA_DIR_OVERRIDE` production override seams) before the first call to `execute_run_task`, so no ambient daemon is ever reachable from this suite again, regardless of what happens to be running on the developer's machine.
- **`agents.list`/`GET /agents` no longer surfaces the 5 `BASE-*` composition-template agents (issue #3465 follow-up).** `base-agent`, `base-engineer`,
  `base-ops`, `base-qa`, `base-research` exist only to be `extends:`-ed by
  concrete agents and were never meant to be dispatched — they were leaking
  into the Foundry GUI's Agents tab and the start-working agent selector
  whenever a project's disk `.claude/agents/` dir had real `BASE-*.md` files
  on it (trusty-mpm's own bundle installs them verbatim). New
  `agents::protocol::is_base_agent` (backed by a single centralized
  `crate::assets::BASE_AGENT_NAMES` list, matched case-insensitively) filters
  both the embedded and disk halves of the listing; `resolve_agent`/
  `load_md_agent` are untouched, so a leaf agent's `extends: base-engineer`
  chain still composes exactly as before — only the user-facing catalog an
  operator browses hides them.
- **`CodeEngine`'s workstream-event SSE loop no longer stalls silently or loops forever (Slice 6, closes #3418, part of epic #3411).**
  - `pump_session_events` reset its reconnect-attempt counter on every merely-successful TRANSPORT-level reconnect, before the inner loop ever observed whether the STREAM made progress. Against a daemon that accepts the connection but closes the stream immediately with no data every time, that made the retry budget un-exhaustible: the function looped forever and `handle_input` never returned. The counter now only resets on genuine progress (an actual data payload), so `SESSION_STREAM_MAX_RECONNECTS` is a real bound again.
  - Every exhaustion path in `pump_session_events` (a non-2xx status, a transport error, a clean-but-premature stream close, an idle timeout) now sends a `done: true, is_error: true` `AssistantOutput` before returning, instead of some paths (the clean-close case) silently returning `Ok(())` — indistinguishable from a genuinely successful turn. This is the epic #3411 deferred Slice 3 review item: a terminal SSE failure now surfaces a visible error in the TUI rather than a stalled spinner with no error text.
  - `run_workstream_subscription` discarded `refresh_workstream_cache`'s return value, so it never actually sent `ReplEvent::WorkstreamUpdated` after an activation change — only `EngineState`'s own internal cache was refreshed, not `ReplApp::active_workstream` (which the status line renders from). It now forwards the refreshed summary as `WorkstreamUpdated`, and also pushes the status line's `Workstream` segment via `ReplEvent::StatuslineUpdate` (`EngineState::statusline_segments`), clearing it (an empty segment list) on deactivation.

### Changed

- **`fs.list_projects` / `GET /projects` re-sourced from trusty-mpm's shared
  project registry (issue #3435).** The project-picker roster's PRIMARY
  source is now a loopback call to the mpm daemon's `GET /api/v1/projects`
  (`http://127.0.0.1:7880`, overridable via `TRUSTY_MPM_URL`); the prior
  filesystem scan (`fs_browse::roster`, issue #3365) is demoted to
  SECONDARY — local-only/unregistered candidates (e.g. a `bakeoff-l1`
  scratch checkout) still appear, never silently dropped, now flagged via a
  new additive `registered: bool` field on each roster entry. An
  unreachable mpm daemon degrades gracefully to the filesystem-only roster
  rather than erroring, and is now caller-distinguishable via a new
  additive `source: "registry" | "fs_only"` field on the roster
  (code-critic PR #3439 review, HIGH 2 — the #3363 lesson: "nothing
  registered" and "registry unreachable" must not collapse into the same
  shape). The merge also normalizes both sides (`std::fs::canonicalize`)
  before comparing paths, so a case-differing or symlink-indirected path to
  the SAME checkout no longer double-lists it (code-critic PR #3439 review,
  HIGH 1), and `repo_url` parsing now rejects compound remainders (GitLab
  subgroups, a port-qualified host) instead of fabricating a bogus
  multi-level lookup path (code-critic PR #3439 review, MEDIUM 1). New
  module: `crates/trusty-code/src/fs_browse/mpm_registry.rs`. Spec: DOC-39
  §5.8.1.

### Fixed

- **`CodeEngine`'s workstream-activation subscription no longer drops
  deactivation events (code-critic HIGH, PR #3436).** The daemon
  legitimately publishes `WorkstreamActivationChanged{new_active_id: None,
  ...}` when the active workstream is deactivated with no replacement
  (DOC-48 §4.2/§4.3); the subscription's `if let` previously matched only
  `new_active_id: Some(..)`, silently dropping the `None` case — no cache
  refresh, no user-visible signal, a stale status line/picker indefinitely.
  Both arms now refresh `EngineState`'s workstream cache; the `None` case
  surfaces a `StatusMessage` (the shared `ReplEvent::WorkstreamActivationChanged`
  declares `new_active_id` as a non-optional `String`, so it cannot carry
  this state without a `trusty-tui` change).
- **The `StatusMessage` fallback above is now a structured event (closes #3452, part of epic #3411).** `trusty-tui`'s `ReplEvent::WorkstreamActivationChanged.new_active_id` is now `Option<String>`, matching the wire shape exactly; `workstream_subscription.rs`'s deactivation arm now emits `ReplEvent::WorkstreamActivationChanged { new_active_id: None, prior_id: Some(..) }` instead of free text, so a UI can structurally clear its "active workstream" indicator.

### Added

- **`CodeEngine::commands()`/`picker()` implement `trusty-tui`'s
  synchronous `TuiEngine` accessors (Slice 1.5, #3428).** Previously shipped
  as inherent methods (ahead of #3428 landing); now real trait-impl
  overrides, so the shared TUI's generic `E: TuiEngine` path picks up the
  real implementations instead of silently falling back to the trait's
  default "supply nothing." Backed by the same `std::sync::Mutex`-guarded
  caches populated in `setup()`/on workstream changes.

- **`tui_client::CodeEngine` — `trusty-tui` engine adapter for `tcode tui`
  (issue #3415, DOC-50 §3.3/§3.4, epic #3411 Slice 3).** A thin
  `trusty_tui::TuiEngine` implementation driving a long-lived
  `tcode serve --http` daemon over pooled HTTP + SSE: daemon discovery
  (`TCODE_DAEMON_URL` env var → the `http_addr` discovery file → a
  `/health` liveness ping), `session.create`/`task.run` for chat turns
  streamed back via `GET /sessions/{id}/events`, `session.cancel` on
  cancellation (never a client-side-only stop), and a long-lived
  `GET /workstreams/{id}/events` subscription translating
  `WorkstreamActivationChanged` into `ReplEvent`s. `crate::serve::discovery`
  adds the daemon-side write half of the discovery file (`http_addr` under
  `resolve_data_dir("trusty-code")`), following the same convention
  `trusty-memory`/`trusty-search` already use rather than inventing a new
  JSON-shaped one.

### Fixed

- **Every daemon-default task run (including every GUI-initiated run) failed
  instantly with "unknown agent 'pm'" (closes #3437).** `task.run` defaults an
  omitted `agent_name` to the literal `"pm"`, but no embedded `pm` agent
  existed in `assets::DEFAULT_AGENTS` and no disk `~/.claude/agents/pm.md`
  existed either — 100% of daemon-default runs failed agent resolution before
  a single turn executed. Added an embedded `pm` orchestrator/default agent
  (`assets/agents/pm.md`, tcode's 4th self-contained default alongside
  `engineer`/`qa-agent`/`code-reviewer`) plus a drift guard
  (`task::protocol::DEFAULT_TASK_RUN_AGENT_NAME`, referenced from both the
  `task.run` default and a dedicated test) so the default literal can never
  again point at a name the embedded roster doesn't carry.
- **LLM calls failed with `API error 400: "opus is not a valid model ID"` on
  daemon task runs (closes #3438).** Several embedded agents' `.md`
  frontmatter (and any `resource_tier`-composed agent) declares its model as
  a bare Claude CLI alias (`opus`/`sonnet`/`haiku`) rather than a concrete
  provider slug; `provider::routing::resolve_model` — the single function
  every call site resolves its final model slug through — now normalizes
  these three known aliases to a concrete OpenRouter slug
  (`provider::routing::normalize_model_alias`) before returning, so no bare
  alias ever reaches a provider unnormalized regardless of which tier
  (`RunContext` override, `[agent].model`, `[llm].model_override`) it came
  from.
- **`serve::DEFAULT_HTTP_PORT` moved from 7881 to 7882 (closes #3364).** The
  old default silently collided with `trusty-mpm`'s supervisor metrics
  listener (`DEFAULT_METRICS_ADDR`, also 7881) — the supervisor's generic
  `/health` masked the collision by answering `{"status":"ok"}`, so both
  ops probes and the `trusty-code-gui` client saw a false-healthy daemon
  while every real `tcode` route 404'd. `trusty-code-gui`'s hardcoded
  `DEFAULT_DAEMON_URL` moves in lockstep; a new cross-crate test
  (`default_daemon_url_matches_tcode_default_http_port` in
  `trusty-code-gui/src/state.rs`) pins the two together. See
  `docs/architecture/port-assignments.md` for the full workspace port table
  this fix introduces.

### Added

- **`fs.list_projects` RPC + `GET /projects` REST route (issue #3365).** A
  small, read-only, loopback-only project roster for the GUI's
  workstream-first project-picker modal — best-effort scans
  `~/trusty-mpm-projects/<owner>/<repo>` two levels deep (falling back to a
  flat `~` scan), filtered to git repos, capped at 200 entries, never a
  caller-facing error. `crates/trusty-code/src/fs_browse/roster.rs`,
  `crates/trusty-code/src/serve/rest/projects.rs`.
- **`workstream.rename` RPC + REST verb (issue #3300, DOC-48 §5.1, Phase C).**
  `workstream.rename{id, name} -> Workstream` overwrites a workstream's name
  and refreshes `updated_at`; `-32002 not_found` for an unknown `id`. REST
  twin: `POST /workstreams/{id}/rename` (JSON body `{"name": string}`),
  mapped `404` for the same case. Renaming a closed workstream is allowed —
  closure only rejects new session bindings (§4.4), not label edits. DOC-48
  §5.1 marked this verb "future, Phase C" when Phase 1A shipped; this is
  that phase's first caller, the GUI workstream switcher.
- **`tcode workstream` / `tcode ws` CLI family (issue #3296, DOC-48 §5.4,
  epic #3292).** `list [--include-closed]` (tmux-`list-sessions`-style
  table: id prefix, state, session count, humanized age, name — `*` marks
  the active row), `get <id>` (raw JSON), `create [--name NAME]`,
  `activate <id> [--force]`, `deactivate` (no-arg; resolves and clears
  whichever workstream is active, or reports a clean no-op), and
  `close <id>`, all thin JSON-RPC clients over the daemon's `workstream.*`
  surface (#3294/#3295). `ws` is a clap alias for the whole family.
  `activate` without `--force` while a different workstream is active
  surfaces the `-32008 active_conflict` error as a clear message naming the
  active workstream and suggesting `--force`, instead of the raw RPC error
  text. ID arguments require the full UUID — the RPC layer has no
  prefix-matching support, so none is built client-side. Auto-streaming
  from the newly-active workstream on `activate` is deferred until the
  workstream-level SSE aggregation route (issue #3297) lands.
- **Workstream activation lock: `workstream.activate`/`workstream.deactivate`
  RPC + REST, `ActiveConflict` (issue #3294, DOC-48 §5/§6, epic #3292).**
  Daemon-enforced singleton active-workstream invariant on top of #3293's
  `WorkstreamStore`: `workstream.activate{id, force?}` activates a
  workstream, is idempotent when re-activating the already-active one, and
  fails with the new `-32008 active_conflict` JSON-RPC error (`data.active_id`
  carries the conflicting id; REST maps it to HTTP `409`) when a DIFFERENT
  workstream is active and `force` was omitted; `force: true` deactivates the
  prior workstream and switches, reporting both ids.
  `workstream.deactivate{id}` clears the pointer only when `id` is the
  currently-active workstream — otherwise an idempotent no-op, per spec.
  REST twins: `POST /workstreams/{id}/activate` and
  `POST /workstreams/{id}/deactivate`.
- **Workstream CRUD RPC/REST surface (issue #3295, DOC-48 §5, Phase 1A for
  epic #3292).** New JSON-RPC methods `workstream.create{name?}`,
  `workstream.get{id}`, `workstream.list{include_closed?}`,
  `workstream.close{id}`, sharing the SAME `SharedWorkstreamStore` handle
  #3294's activation methods use. `workstream.list` returns
  `{active_workstream_id, workstreams: [...]}`, each record paired with its
  computed `state` (`active`/`idle`/`closed`); `include_closed` defaults to
  `false`. `workstream.close` marks the record closed and, per §4.4,
  auto-deactivates it if it was the active workstream — the store gains a
  `WorkstreamStore::close` primitive for this. REST wrappers
  (`POST`/`GET /workstreams`, `GET /workstreams/{id}`,
  `POST /workstreams/{id}/close`) mirror the existing `rest::sessions`/
  `rest::tasks` pattern; paths are intentionally unprefixed (not DOC-48
  §5.2's literal `/api/v1/workstreams`) to match every other REST resource
  group this crate ships. `tcode serve`'s router now loads and
  boot-reconciles a project-scoped `WorkstreamStore` (`build_router` is now
  `async`/fallible) and shares it across every `workstream.*` handler behind
  a `tokio::sync::Mutex`. `WorkstreamActivationChanged` SSE emission and the
  CLI (#3296) remain follow-up tickets.
- **Workstream domain model + flat JSON storage + boot reconciliation
  (issue #3293, DOC-48 §2/§3, Phase 1A foundation for epic #3292).** New
  `workstreams` module: `Workstream`/`WorkstreamId`/`WorkstreamState` (state
  is computed, never persisted — active iff its id equals the store's
  `active_workstream_id` pointer) and `WorkstreamStore`, an atomic
  temp+rename flat-JSON store at `~/.trusty-code/workstreams-{slug}-{hash}.json`
  keyed off the daemon's `ProjectBinding`. `reconcile_on_boot` restores the
  persisted active pointer on restart and is strictly non-destructive — no
  workstream record is ever deleted, and a pointer whose target vanished is
  simply cleared. Activation-lock RPC semantics, the RPC/REST surface, the
  CLI, and SSE aggregation are follow-up tickets (#3294–#3297) building on
  this foundation.
- **Per-call project binding on `task.run` (issue #3178, DOC-39 §5.5 /
  AC-16.2).** `task.run` (and `POST /tasks`) now accept an optional `project`
  path, resolved through the exact same `ProjectBinding::resolve` helper
  `session.create` uses — an invalid path (missing, not a directory) maps to
  `-32003 invalid_argument`/`400` identically on both surfaces. Omitting
  `project` keeps today's process-boot-time binding unchanged (back-compat).
  This converges the two project-binding entry points epic #3174 needs for
  the project-first "pick a project and just start prompting" flow.
  **Follow-up (code-critic HIGH finding, PR #3189):** when `session_id`
  reuses an existing session, `project` may only RESTATE that session's own
  persisted binding root — a `project` naming a DIFFERENT root is rejected
  with `-32003 invalid_argument`/`400` rather than silently executing the run
  against a project `session.status`/`session.list` would never agree the
  session is bound to (`SessionRegistry` has no binding-update path).
- **REST-pollable search/recall audit trail (issue #3072).** New
  `session.get_search_audit` RPC method and `GET /sessions/{id}/search-audit`
  REST route return a session's retained, capped (200 records) history of
  `search_code`/`recall_session` activity — the data source for DOC-39 §4.7's
  Search tab (10d) and #3027's monitor card — mirroring the RPC+REST parity
  pattern established by `session.get_agents` (#2962) and
  `session.get_context_budget` (#3015).
- **CI staleness guard for the embedded tm-agent copies (issue #2958 Slice
  E4).** `scripts/check_agent_assets.sh` + `.github/workflows/agent-assets.yml`
  diff `crates/trusty-code/src/assets/agents/*.md` against their source in
  `crates/trusty-mpm/src/assets/agents/`: the 29 byte-parity files must stay
  byte-identical to trusty-mpm's source, and the 4 Slice-E3 deliberate
  deviations (`qa.md`, `code-critic.md`, `code-analyzer.md`, `web-qa.md` —
  `tools:` restriction + reworded read-only prose) are pinned by trusty-mpm
  SOURCE hash in `scripts/agent-asset-pins.tsv` instead of byte-compared, so
  an upstream edit behind one of them fails the gate for deliberate
  reconciliation rather than silently drifting. `--update`/`--force-add`
  re-pin after an intentional reconciliation, mirroring
  `scripts/check_line_cap.sh`'s ratchet ethos (refuses to add a new deviation
  silently).

### Fixed

- the embedded 31-agent `DEFAULT_AGENTS` roster (#2958) is now actually
  reachable from every real CLI dispatch path, not just `agents::load_all_agents`'s
  dir-wide scan. `runner::in_process::InProcessAgentRunner::load_agent`,
  `run_task::execute_run_task`'s and `task::executor`'s PM-config loads,
  `run_task::resolve_agent_model_slug`, and
  `tools::delegate::DelegateToAgentTool`'s pre-flight check/hint previously
  read `<agents_dir>/<name>.md` directly off disk with no embedded
  fallback, so `tcode run-task engineer ...` and `tcode run-task
  rust-engineer ...` both failed with "agent source not found" on a fresh
  project with no `.claude/agents/`. All five now route through a single
  new `agents::resolve_agent` helper (disk always wins when present, even
  when it fails to parse — no silent fallback to a same-named embedded
  agent) and `agents::available_agent_names` (disk ∪ embedded, for
  "available agents" hints). This is the last gap in the #2958 arc; closes
  #3046, completing #2958.

### Added

- **Pollable context-budget snapshot — cache + `session.get_context_budget`
  RPC + `GET /sessions/{id}/budget` REST route (#3015).** Closes the gap
  behind PR #3014's GUI status bar rendering "budget: unavailable":
  `record_context_budget` now caches a `ContextBudgetSnapshot`
  (`crate::events`) on the session entry, mirroring how `record_index_readiness`
  caches `IndexReadinessSnapshot`, so a client that attaches or reconnects
  after a turn's `Event::ContextBudget` already fired can still retrieve it.
  New `session.get_context_budget` JSON-RPC method (`session::protocol_budget`)
  returns a tagged `ContextBudgetQuery` — `{"status":"recorded", ...}` or
  `{"status":"never_recorded"}`, never a bare `null` — and a thin
  `GET /sessions/{id}/budget` REST route in `serve::rest::sessions` forwards
  to it, following the exact `session.get_readiness`/`GET .../readiness`
  precedent.

- **REST resource gateway — `task.run`/`fs.list_dir`/`session.get_agents`
  routes (#2983, #587, Slices 4-6).** Three more thin `axum` route groups on
  `tcode serve --http`, each calling `rest::respond` (or the new
  `tasks::respond_accepted` for the one route with a non-`200` success
  status) against its JSON-RPC twin — zero duplicated business logic:
  - `POST /tasks` -> `task.run` (`202 Accepted` — this route is
    deliberately asynchronous: it reserves the execution slot synchronously
    and returns before the LLM run itself completes, and unlike `POST
    /sessions` it does not always mint a brand-new resource — a
    `session_id` in the body reuses an existing one — so `201`'s framing
    does not fit uniformly)
  - `GET /fs?path=..&include_hidden=..` -> `fs.list_dir` (the daemon-side
    folder picker; error mapping already distinguishes `404 not_found`,
    `400 invalid_argument` for a non-directory path, `403
    permission_denied`, and `500 internal` via the existing
    `rpc_error_to_status`, no new mapping needed)
  - `GET /sessions/{id}/agents` -> `session.get_agents` (nested under
    `/sessions/{id}`, not a bare `GET /agents`, because the RPC method
    requires a `session_id` — there is no roster independent of a session;
    this slots into the same family as Slice 2's other session-scoped read
    routes)

  New `crate::serve::rest::tasks`/`rest::fs`/`rest::agents` modules, merged
  into `crate::serve::http::build_axum_router` alongside `POST /rpc`,
  `GET /health`, `GET /sessions/{id}/events`, and the Slice 2/3 `session.*`
  REST routes.

### Fixed

- `catchup::tests::*` no longer mutates the process-global `TRUSTY_MEMORY_URL`
  env var to point at a fixed, unreachable `127.0.0.1:19999` — under
  `cargo test`'s default parallelism, that unguarded `unsafe { set_var(..) }`
  (with no matching cleanup) leaked into every other test in the same lib
  binary that resolves a trusty-memory URL via `pm_catchup_context`
  (e.g. `run_task::tests::repeated_llm_errors_trigger_redelegation_cap_not_pm_turn_cap`),
  producing false-red gates on unrelated PRs. `pm_catchup_context` is now
  split into a thin env-resolving wrapper and a testable
  `pm_catchup_context_with_memory_url(project_dir, memory_url)` that takes the
  target URL as a parameter, so tests thread in a guaranteed-unreachable
  address directly with no shared mutable global, no lock, and no cleanup
  required (closes #3003).
- the unified-diff applier (`tools/fs/edit_format/diff.rs`) no longer errors
  on a `git diff`-style `\ No newline at end of file` footer marker inside a
  hunk body — the marker is metadata, not content, so it no longer fails the
  whole apply. Its *position* (after a `-`, `+`, or ` ` line) is also tracked
  so the applier picks the OUTPUT's trailing-newline state correctly instead
  of always copying the original file's, which silently corrupted the
  trailing byte on either direction of a no-trailing-newline state change
  (closes #2150).
- the delegated engineer's tool registry (`task::executor::ProjectToolFactory`)
  now registers `use_skill` when a skills catalog resolved, matching what its
  system prompt advertises via `with_skills_catalog` — previously the tool
  call failed with "no tool registered" (closes #2152).

### Added

- **Embedded tm agent catalog as in-memory assets (#2958, epic #2892 Slice
  E2).** Bundled 33 `.md` assets byte-for-byte from trusty-mpm's agent
  catalog under `assets/agents/`: the 5 `BASE-*` extends templates
  (`BASE-AGENT`, `BASE-ENGINEER`, `BASE-OPS`, `BASE-QA`, `BASE-RESEARCH`) and
  28 coding-relevant roster agents (`api-qa`, `code-analyzer`, `code-critic`,
  `dart-engineer`, `data-engineer`, `documentation`, `golang-engineer`,
  `java-engineer`, `javascript-engineer`, `local-ops`, `nextjs-engineer`,
  `ops`, `phoenix-engineer`, `php-engineer`, `prompt-engineer`,
  `python-engineer`, `qa`, `react-engineer`, `refactoring-engineer`,
  `research`, `ruby-engineer`, `rust-engineer`, `security`, `svelte-engineer`,
  `tauri-engineer`, `typescript-engineer`, `web-qa`, `web-ui-engineer`),
  exposed via the new `assets::EMBEDDED_TM_AGENT_SOURCES` name->content
  table. New `agents::md_loader::project_embedded_md_with_extends` resolves
  an agent's `extends:` chain entirely against that table (e.g.
  `rust-engineer` -> `base-engineer` -> `base-agent`) using
  `trusty_agents_common::agents::builder_in_memory` (Slice E1, #3013) — no
  filesystem access. Not yet wired into `assets::DEFAULT_AGENTS` or
  `agents::load_all_agents`'s dispatchable fallback; that roster expansion is
  a separate, later slice (E3).
- **31-agent embedded roster wired into the composition layer (refs #2958,
  epic #2892 Slice E3).** `assets::DEFAULT_AGENTS` grows from 3 to 31
  entries: the original `engineer`/`qa-agent`/`code-reviewer` plus the 28
  coding-relevant tm agents Slice E2 bundled. `agents::load_embedded_default_agents`
  now routes each of the 28 through the new `EmbeddedAgent::Composed`
  variant, resolving `extends:` chains via `agents::md_loader::project_embedded_md_with_extends`
  against `assets::EMBEDDED_TM_AGENT_SOURCES`; the original 3 keep the
  existing flat `EmbeddedAgent::Direct` path. The 5 `BASE-*` templates remain
  extends-sources only — never dispatchable (`assets::tests::base_templates_are_never_dispatchable`).
  mpm's own `engineer` agent stays excluded from the roster (upstream #2958
  decision) specifically to avoid colliding with tcode's own `engineer`
  default; `assets::tests::no_name_collisions_across_the_31_agent_roster`
  pins that no collision exists across the final 31.
  **Known gap, NOT fixed in this slice (#3046):** the embedded fallback this
  roster feeds (`agents::load_all_agents`'s empty/invalid-disk branch) has no
  reachable production caller today — a live CLI reproduction on a fresh
  project showed `tcode run-task rust-engineer` AND `tcode run-task engineer`
  (one of the original 3, predating this slice) both fail with "agent source
  not found". The composition layer and the 31-agent roster definition are
  complete and covered by `assets::tests::*`/`agents::tests::*` (which call
  `load_all_agents`/`load_embedded_default_agents` directly), but the CLI
  wiring that would let a fresh project actually dispatch one of these 31
  names end-to-end is tracked separately as #3046 and intentionally out of
  scope here.
  **Tools-restriction deviation (Bob's 2026-07-18 ruling):** four
  reviewer-intent roster agents — `qa`, `code-critic`, `code-analyzer`,
  `web-qa` — now carry an explicit restrictive `tools:` frontmatter override
  in their tcode copy (`read_file`, `grep`, `glob`, `list_dir`,
  `search_code`, `use_skill`, `finish_task` — no `write_file`/`edit`/`bash`,
  mirroring tcode's own `code-reviewer` default), deliberately deviating
  those four files from byte-parity with trusty-mpm's source. A code-critic
  review round on the same PR additionally found the prose in `web-qa.md`,
  `qa.md`, and `code-analyzer.md` still instructed write/bash actions the new
  allowlist denies (creating test scripts, running `CI=true npm test`,
  `ps aux` monitoring, generating-and-running a review script); all three
  were reworded in this slice to a genuinely read-only frame — findings plus
  concrete, ready-to-run recommendations handed off to an engineer/ops/CI to
  execute, never executed by the agent itself. `documentation` and `research`
  stay byte-identical and unrestricted per the same ruling — a future E4 CI
  staleness guard (diffing tcode's copies against trusty-mpm's source) must
  whitelist the `tools:` line AND the reworded prose sections in these files.
- **REST resource gateway — `session.*` write routes (#2983, #587, Slice 3).**
  New `POST`/`PUT`/`DELETE` routes on `tcode serve --http`, each a thin
  `axum` handler calling `rest::respond` (or the new `respond_created` for
  the one route that returns a non-`200` success status) against its
  `session.*` JSON-RPC twin — zero duplicated business logic:
  - `POST /sessions` -> `session.create` (`201 Created` — the only route in
    this slice that mints a brand-new resource; every other route below
    acts on an existing one and keeps `200` on success)
  - `POST /sessions/{id}/messages` -> `session.send`
  - `POST /sessions/{id}/cancel` -> `session.cancel`
  - `PUT /sessions/{id}/goal` -> `session.set_goal`
  - `DELETE /sessions/{id}/goal` -> `session.clear_goal`

  An unknown `id` returns a real HTTP `404` with a `session_not_found`
  JSON-RPC error envelope; a malformed JSON body never reaches a handler at
  all — axum's `Json` extractor rejects it (`400`/`422`) before any
  `session.*` method runs. New `crate::serve::rest::sessions_write` module
  (tests split into a sibling `sessions_write_tests.rs`, mirroring
  `session::registry`'s `registry_tests.rs` convention, purely for the
  500-SLOC production cap), merged into
  `crate::serve::http::build_axum_router` alongside `POST /rpc`,
  `GET /health`, `GET /sessions/{id}/events`, and Slice 2's read routes.
- **REST resource gateway — `session.*` read routes (#2983, #587, Slice 2).**
  New `GET` routes on `tcode serve --http`, each a thin `axum` handler calling
  `rest::respond` (new: wraps `rest::call` + `rpc_error_to_status` into the
  `Result<Json<Value>, (StatusCode, Json<Response>)>` shape every REST handler
  returns) against its `session.*` JSON-RPC twin — zero duplicated business
  logic:
  - `GET /sessions` -> `session.list`
  - `GET /sessions/{id}` -> `session.status`
  - `GET /sessions/{id}/transcript` -> `session.get_transcript`
  - `GET /sessions/{id}/readiness` -> `session.get_readiness`
  - `GET /sessions/{id}/goals` -> `session.get_goals`

  An unknown `id` returns a real HTTP `404` with a `session_not_found`
  JSON-RPC error envelope (never a 200-wrapped error), matching the existing
  `GET /sessions/{id}/events` convention. New `crate::serve::rest::sessions`
  module, merged into `crate::serve::http::build_axum_router` alongside
  `POST /rpc`/`GET /health`/`GET /sessions/{id}/events`.
- **REST resource gateway bridge, no routes yet (#2983, #587, Slice 1).** New
  `crate::serve::rest` module: `rest::call(router, method, params, ctx)` drives
  a synthetic JSON-RPC `Request` through the existing `Router::dispatch` seam
  and unwraps the `Response` envelope into `Result<Value, RpcError>`, and
  `rest::rpc_error_to_status(&RpcError) -> StatusCode` maps `RpcError` codes
  onto real HTTP statuses (`not_found`/`session_not_found` -> 404,
  `permission_denied` -> 403, `invalid_argument`/`invalid_params` -> 400,
  `internal` -> 500, anything unmapped -> 500). This reuses trusty-memory's
  "Pattern C" (one shared handler, many transports) so a future REST route and
  its JSON-RPC method twin always run the exact same handler — zero business-
  logic duplication. `serve::mod` now declares `mod rest;` but wires no axum
  routes; concrete resource routes land in S2-S6.
- **`session.get_agents` — live, eviction-safe agent-roster RPC (DOC-39 §5.4,
  closes #2962).** New JSON-RPC method `session.get_agents(session_id) ->
  { agents: [{agent_id, name, model, state, task, todos, files_changed}] }`,
  replacing the client-side SSE-fold §5.4 named "a Phase-1 loan, not a
  design." Backed by an ALWAYS-RETAINED per-session agent map
  (`SessionEntry::agents`, `registry.rs`) — NOT a fold over the
  capacity-bounded ring buffer: `SessionRegistry::record` (the same critical
  section that pushes every `agent`/`agent_id`-carrying event —
  `ToolStarted`/`ToolFinished`/`ToolError`/`SearchPerformed`/
  `MemoryRecalled`, since #2898 — onto the ring) also updates this map, which
  is evicted only when the session itself goes away, never by ring capacity.
  This closes a code-critic HIGH found on an earlier cut of this PR that
  folded the ring buffer directly on every call: a long-running agent's
  `ToolStarted` could age out of the (default 1000-entry) ring before any
  later attributed event for that `agent_id` landed, silently vanishing that
  agent from the roster — indistinguishable from "never spawned" while it
  may still be running. `state` is `"running"` while an agent's last known
  event is an unmatched `ToolStarted`, `"idle"` otherwise.
  `model`/`task`/`todos`/`files_changed` are deferred (`null`/`[]`) — see
  `session::registry::agents`'s module docs for exactly why each isn't
  populated from today's event stream. Implementation lives in new sibling
  files `session/registry_agents.rs` (the query) and
  `session/protocol_agents.rs` (the RPC handler), mirroring
  `session.get_readiness`'s split. This closes the last MISSING API item in
  DOC-39 §5.1.
- **Embedded default agents converted from TOML to `.md`+frontmatter (#2897,
  epic #2892, Slice C).** The three bundled default agents
  (`engineer`/`qa-agent`/`code-reviewer`, #2895) are now authored as
  `crates/trusty-code/src/assets/agents/*.md` instead of `*.toml`; the retired
  TOML files are deleted. The embedded fallback in `agents::load_all_agents`
  now projects each `.md` string via a new
  `agents::md_loader::project_embedded_md`, which shares the exact same
  frontmatter -> `AgentConfig` mapping (`project_to_agent_config`) that the
  disk `.md` loader (`load_md_agent`) uses — the only difference is that the
  embedded path skips `compose_agent`'s `extends:`-chain resolution (there is
  no source directory for a compiled-in `&'static str` to resolve against,
  and none of the three defaults declare `extends:`), calling
  `agent_metadata_from_str`/`extract_body` directly on the raw string
  instead. Every default's projected `AgentConfig` is field-identical to what
  the retired TOML produced — same `name`, `model` (`None`), `max_tokens`,
  `tools.allowed` (exact allowlist), and `system_prompt.content` (the prose
  body is byte-identical modulo the shared `.md` body-extraction path's
  pre-existing trailing-whitespace trim). Purely additive/non-breaking: the
  TOML loader for user `.claude/agents/*.toml` configs (`AgentConfig::load`,
  `AgentConfig::from_toml_str`) is untouched; TOML retirement there is a
  later slice (D).
- **Markdown+frontmatter `.md` agent loader, dark-launched alongside TOML
  (#2897, epic #2892, Slice B).** `agents::md_loader::load_md_agent` composes
  a `.md` agent source file's `extends:` chain via
  `trusty-agents-common::agents::builder::compose_agent` (Slice A, #2952) and
  projects the result onto tcode's existing `AgentConfig`: `model` ->
  `agent.model`, `max_tokens` -> `llm.max_tokens`, `tools:
  Option<Vec<String>>` -> `ToolsConfig.allowed` (a direct map — both sides
  share identical `None`=all-allowed / `Some([])`=deny-all /
  `Some(list)`=allowlist semantics), and the composed prose body ->
  `system_prompt.content`. The composed frontmatter's HR-1 role-derived
  `initialPrompt` enrichment (keyed off trusty-mpm's role table, which
  collides by name with tcode's `engineer`/`qa` roles) is guaranteed to never
  leak into `system_prompt.content` — the body extraction only reads past the
  closing frontmatter fence and the public `AgentMetadata` projection has no
  `initial_prompt` field at all. `discover_agents`/`load_all_agents` now scan
  and load both `*.toml` and `*.md` agent files; when both exist for the same
  agent name, the `.toml` deterministically wins (a warning is logged). Purely
  additive and behavior-preserving — the TOML loader is unchanged; TOML
  retirement is a later slice (D).
- **Engineer receives `use_skill` on the `--legacy-in-process`/`run_task` path (#2942).** The `python-engineer` role's tool registry (`ProjectToolFactory` in `run_task/mod.rs`) now conditionally registers `UseSkillTool` alongside its other project tools when a `SkillResolver` is available, so the engineer can fetch a skill's full body mid-turn instead of relying solely on the catalog summary baked into its system prompt.
- **Native skill discovery on the `--legacy-in-process`/bake-off `run-task` path (#2924).** The `use_skill` tool and the `.claude/skills/` catalog now reach the PM's prompt and tool registry on `run_task::execute_run_task`, not just the daemon/thin-client path — a PM run through `run-task` can now discover and load project skills the same way a daemon session already could.
- **Embedded default agents & skills, disk-first with embedded fallback
  (#2895).** A project with no `.claude/agents/` and/or `.claude/skills/`
  (or an empty one) previously started with zero agents and zero skills.
  `crates/trusty-code/src/assets/` now bundles three native-TOML default
  agents (`engineer`, `qa-agent`, `code-reviewer`) plus trusty-mpm's 28
  universal skills (format-identical `SKILL.md` files, reused verbatim;
  `tm-*` orchestration skills excluded since they drive trusty-mpm MCP tools
  tcode does not have) at compile time via `include_str!`.
  `agents::load_all_agents` falls back to the embedded set when the
  *parsed* result is empty (a directory with only unparseable TOML still
  falls back, not just a missing/empty one); `skills::discover_skill_metadata`
  falls back when the disk scan is empty. Either way, any successfully
  discovered disk config — even a single file — is used as-is and never
  merged with the embedded defaults.
- **Stable per-spawn `agent_id` for event attribution (DOC-39 AC-13.1/13.2).**
  Agent attribution on the event stream was keyed only by `agent: String` (the
  agent-config name), so two concurrently-delegated same-named agents (e.g.
  two `python-engineer` delegations) were indistinguishable. Every
  agent-attributed event (`ToolStarted`/`ToolFinished`/`ToolError`,
  `SearchPerformed`, `MemoryRecalled`, `AgentSpawned`/`AgentStarted`/
  `AgentDone`/`AgentFailed` — the latter four have the field defined now but
  are not yet emitted by any production call site) now also carries
  `agent_id`: a UUID v4 minted once per delegation spawn
  (`runner::in_process::InProcessAgentRunner::run_pipeline`)
  and a stable, session-scoped id for the PM/root agent
  (`task::executor::run_and_record`). Additive and non-breaking — `agent`
  is unchanged and unremoved; `#[serde(default)]` on `agent_id` keeps old
  recorded transcripts deserializing (as an empty string, not a sentinel).

### Changed

- **BREAKING — the TOML agent loader is retired; `.claude/agents/*.toml` is
  no longer read at all (#2897, epic #2892, Slice D).** `AgentConfig::load`
  and `AgentConfig::from_toml_str` are deleted, along with the `toml` crate
  dependency. `discover_agents` now globs `*.md` ONLY — a project whose
  `.claude/agents/` still holds `.toml` files gets a LOUD, aggregated
  `tracing::warn!` naming every orphaned file and pointing at the new
  converter script, then those files are skipped (never parsed, never
  erroring). If disk holds nothing but orphaned `.toml` (or nothing at all),
  `load_all_agents` falls back to the embedded `engineer`/`qa-agent`/
  `code-reviewer` defaults exactly as it does for an empty directory — a
  project is never left with zero agents. **Migration:** run
  `scripts/migrate-tcode-agents-toml-to-md.py <path-to-.toml-or-dir>` to
  convert existing `.toml` agents to the `.md`+frontmatter format (dark-launched
  in Slice B, #2897) that has been the primary format since Slice C; delete
  the `.toml` sources once you've reviewed the generated `.md`. This
  completes the #2897 epic (#2892): TOML -> `.md` is now a one-way door.
- **trusty-code now depends on `trusty-agents-common`; `ServiceTier`,
  `RunContext`, and `HistoryMessage` are re-exported instead of redeclared
  (#2893, epic #2892).** `crates/trusty-code/src/tools/traits.rs` re-declared
  ~150 lines of trait/type definitions that already exist in
  `trusty-agents-common` (the same crate `trusty-agents` already re-exports
  these from). `ServiceTier` and `RunContext` were byte-identical;
  `HistoryMessage` was a strict field subset. All three are now
  `pub use trusty_agents_common::...` re-exports — no behavior change.
  `ToolExecutor`/`ToolResult` and `AgentRunner`/`AgentOutput` intentionally
  stay LOCAL: tcode's `ToolResult` carries a `telemetry` field (#2862) and
  `AgentOutput` carries a `finish_status` field (#2683, whose `usage:
  TokenUsage` also carries a `cost_usd` field, #50) that the shared
  definitions lack, and unifying them would require either a much larger
  events-module move or changes to every `trusty-agents`/`cto-assistant`
  call site — out of scope for this behavior-preserving refactor.
- **BREAKING — projectless mode + one typed project binding (UI Phase-1).**
  "Project" was one concept split across two disagreeing API surfaces:
  `task.run` took a REQUIRED `project: PathBuf` (`task/protocol.rs:48`), while
  `session.create` took an untyped, free-form `project: Option<String>` LABEL
  (`session/protocol.rs:157`) that was never validated, never bound, and
  disconnected from the path `task.run` demanded — a label that could not be
  indexed and a path that could not be omitted, i.e. two halves of one missing
  object. Because the path was required, a **projectless** workstream was
  inexpressible, and the shell's entry screen (spec DOC-39 §4.2/§5.5 screen 7a,
  "Open a project" — a workstream that exists BEFORE a project is chosen) was
  literally unimplementable. Both surfaces now converge on one typed
  `binding::ProjectBinding` with the spec's **three** states:

  | State | Indexing | Git affordances |
  |---|---|---|
  | `projectless` — no directory bound (chat/planning) | none | none |
  | `directory` — bound, non-git (#2728/#2747) | **yes** | none |
  | `git_repo` — bound git worktree | yes | full |

  **Binding is NOT gated on `.git`.** The design proposal's own text ("binds the
  moment work touches files in a git repo") is wrong and contradicts shipped
  behaviour: it would exclude the non-git working dirs (#2728) and OS temp dirs
  (#2747) we deliberately support. A non-git directory BINDS and INDEXES; the
  git/non-git split decides only which git affordances are offered, never
  whether the project binds. Git detection is now a single implementation
  (`binding::is_git_worktree`), which `run_task::diff` delegates to, so a
  `git_repo` binding can never disagree with the diff strategy.

  What projectless does with the three things that assumed a project — each a
  defined behaviour, never a panic: indexing is **skipped**; the project-scoped
  memory palace is **skipped** (`memory_sink_for` takes `Option<&Path>` and
  returns `None` — the scratch root is deliberately NOT substituted, which would
  mint an orphaned palace per run); and the fs/bash tools are rooted at an
  **ephemeral scratch dir**, discarded when the run ends and logged at `warn` to
  stderr so a projectless write is observable rather than silent. `CLAUDE.md`,
  catch-up, skills, and the `settings.json` mode tier already degraded to
  "absent" and needed no projectless special-casing.

  Breaking-change surface, and why each is safe or deliberate:
  - **`task.run`'s JSON-RPC params are UNCHANGED** — its `project` was a
    `register()` argument (daemon-scoped), never a request field, so no
    JSON-RPC caller breaks. Its response gains a `binding` object.
  - **Rust API (breaking):** `serve::{build_router, run_stdio, run_http}` and
    `task::protocol::register` take `ProjectBinding` instead of `PathBuf`;
    `TaskRunParams.project` → `.binding`; `SessionRegistry::create`'s third
    param is a `ProjectBinding`; `mode::resolve_mode` takes `Option<&Path>`.
  - **`session.create` wire (breaking, deliberate):** `project` is now a project
    PATH, not a decorative label — a string that names no directory returns
    `-32003 invalid_argument`. Erroring is the point: silently accepting a
    "project" that binds and indexes nothing is the exact failure this
    reconciliation ends. Omitting `project` remains valid and means projectless.
  - `Session` gains a typed `binding`; its `project` field survives as a
    display label but is now DERIVED from the binding (`ProjectBinding::label`),
    so the two can never again disagree. `binding` is `#[serde(default)]`, so an
    older payload without it reads back as projectless.

  Being breaking at the Rust-API and `session.create`-semantics level, the next
  release must be **0.3.0** (pre-1.0: minor = breaking), not a patch bump.
  ([#2855](https://github.com/bobmatnyc/trusty-tools/pull/2855), spec DOC-39
  §4.2/§5.5, ACs 2.1–2.4 / 16.1–16.3)

### Added

- **`tcode serve --project` is now OPTIONAL** — omit it to serve projectless.
  Given, it must be an existing directory (validated at the boundary with an
  actionable hint, rather than failing later as a confusing per-task error); it
  need NOT be a git repo. A projectless daemon resolves agents from the
  user-level `~/.claude/agents` rather than the process CWD, which would
  silently bind a directory the operator never chose.

- **Daemon-side directory inspection API (`fs.list_dir`) for the UI's project
  picker (UI Phase-1, screen 7a)** — a new `fs.*` JSON-RPC namespace on `/rpc`,
  alongside `session.*`/`task.*`. `fs.list_dir(path?, include_hidden?)` returns
  `{path, display_path, parent, entries[{name, path, is_dir, is_git_repo}]}`.
  The UI is a thin client — no UI target touches the filesystem directly,
  **including Tauri** (which could, but must not: that would let the desktop
  build do something the web build cannot, forking one UI into two behaviours) —
  so browsing local disk to pick a project has to be a daemon call. One response
  serves both jobs: `display_path`/`parent` drive the breadcrumb and
  up-navigation, while `is_git_repo` is simultaneously 7a's `git` badge and the
  discriminator for the three-state project binding model (projectless → non-git
  dir → git repo). A non-git directory is a first-class success with
  `is_git_repo: false`, never an error (#2728). `~` is expanded and paths
  canonicalized server-side, matching the `expand_tilde` convention already used
  in `trusty-mpm` and `trusty-agents`; `path` defaults to `~` so the projectless
  cold start is just `fs.list_dir({})`.
  - Git-ness handles **both** on-disk shapes: `.git` as a directory, and `.git`
    as a FILE carrying a `gitdir:` pointer — which is what a linked worktree (and
    a submodule) has. An `is_dir()`-only check badges every worktree as non-git;
    that is the bug PR #2839 hit in `build.rs`, and this workspace's own
    checkouts are linked worktrees. Detection is two `stat`s (plus one small read
    for the rare `.git`-file) rather than a `git rev-parse` subprocess per entry,
    because this runs across every row of a directory on an interactive picker's
    hot path.
  - Errors are typed and distinguishable rather than stringly, reusing the vision
    spec's existing §13.2 taxonomy instead of inventing `fs`-specific codes:
    `-32002 not_found` (no such path), `-32003 invalid_argument` (path is a
    file), `-32001 permission_denied` (OS refusal), `-32603 internal` (other IO).
    New `RpcError::not_found`/`permission_denied` constructors back the first
    two.
  - **No path guard, deliberately** — no denylist, sandbox root, or permission
    layer, and no macOS TCC state machine. tcode is a local app: the daemon runs
    as the user with the user's own entitlements, so a listing discloses nothing
    `ls` would not. Decisively, the same API already exposes `task.run`, which
    executes arbitrary code as the user — guarding browse while that sits one
    method away is ceremony, not security. An OS-level refusal is reported as an
    ordinary typed error. Recorded as a module doc comment so it is not later
    "fixed".

- **`index_readiness` event — a warming index is no longer indistinguishable from
  "ready, zero hits" (UI Phase-1, follows [#2784](https://github.com/bobmatnyc/trusty-tools/issues/2784)).**
  The per-lane index readiness `trusty_common::search_readiness` already computed
  at task start was stderr-only, so no API consumer — including tcode's own SPA —
  could reach it. The daemon path now publishes it as `Event::IndexReadiness`
  (replayable through the session ring buffer, streamed over
  `GET /sessions/{id}/events`). Its `state` field (`"ready"` | `"warming"` |
  `"unavailable"`) is the fix for the concrete failure this addresses: during
  semantic warm-up a search returns EMPTY, which looks identical to a fully-ready
  index that genuinely has no match — opposite meanings that led a model to
  conclude "nothing there" and hand-explore to the wrong target. A UI must not
  render "no results" unless `state == "ready"`. Per-lane
  `lexical_ready`/`semantic_ready`/`graph_ready` flags, `lifecycle_status`, and
  `chunk_count` ship alongside. The CLI (`run-task`) path is unchanged and stays
  log-only; `probe_index_readiness`'s fail-open contract is preserved — a `None`
  probe reports `state: "unavailable"` (also not evidence of absence) rather than
  going silent.
- **`context_budget` event — the Infinite Sessions guarantee is now renderable
  (epic [#2343](https://github.com/bobmatnyc/trusty-tools/issues/2343)).**
  `agent_loop::cadence` enforces "working context >= 60%, session overhead <= 40%"
  every single turn and returned a `CadenceOutcome` documented as existing "for
  observability/tests" — which the sole call site then discarded, so nothing could
  observe the guarantee it enforces. The outcome now carries the REAL measured
  `overhead_tokens` (`enforce_budget` returns the `estimate_total_tokens` value it
  already had to compute, rather than dropping it) and is published as
  `Event::ContextBudget`: context window, measured overhead, cap, and the derived
  `working_context_pct`/`overhead_pct` a live budget meter renders, plus
  `compaction_fired`/`compaction_rounds` to make a compaction legible as *removed
  from context, not from the record*. Emitted only by the PM's persistent-session
  loop — cadence's PM-only gating (`AgentLoopConfig.cadence` defaults `None`) is
  unchanged, so a delegated engineer loop never emits.
- **Agent attribution on every tool event (UI Phase-1 API)** — `ToolStarted`,
  `ToolFinished`, and `ToolError` now carry an `agent` field naming the agent
  that dispatched the call. Previously a client could only guess whether a tool
  call came from the PM or a delegated engineer by interleaving the event stream
  against `AgentSpawned`/`AgentDone` ordering — fragile inference that breaks as
  soon as their calls overlap, and the reason the UI could not answer "which
  agent drove this change?". The name is a per-call PARAMETER on
  `agent_loop::ToolEventSink` rather than sink state, because ONE
  `Arc<dyn ToolEventSink>` is shared by the PM's loop and every delegated
  sub-agent's loop (`task::executor::run_and_record` clones the same handle into
  both), so a sink carrying its own name could only ever report one of them. The
  dispatching `AgentLoop` is the only layer that knows its own identity, so it
  passes it per call — declared via the new `AgentLoop::with_agent`, wired at
  both production sites (PM: `params.agent_name`, default `"pm"`; delegated
  sub-agents: the runner's `agent_name`). `agent` is the agent NAME, matching the
  key the rest of the taxonomy already uses (`AgentSpawned.agent`,
  `PmDelegating.agent`, `LlmRequested.agent_name`), so the UI can join them
  directly; loops that declare no agent emit the documented
  `events::UNATTRIBUTED_AGENT` (`"unknown"`) sentinel rather than an empty string
- **`search_performed` event — structured search telemetry** — a new `Event`
  variant carrying `{agent, lane, query, hit_count, latency_ms}`, emitted by
  `search_code` ALONGSIDE (never instead of) the generic tool events, so existing
  consumers see an unchanged stream. `lane` is the lane trusty-search ACTUALLY
  served, not the mode the model requested: a `semantic`/`symbol` query against a
  still-building index transparently retries on the lexical lane
  ([#2783](https://github.com/bobmatnyc/trusty-tools/issues/2783)) and now
  reports `lane: "lexical"` rather than claiming a semantic search that never
  ran. `hit_count` is `null` when the payload shape could not be counted — never
  a misleading `0`, which would read as "no hits". The fail-open paths (absent
  daemon, no index) attach no telemetry at all: they never reached a lane, so
  there is no search to report
- **`memory_recalled` event — structured recall telemetry with the `injected`
  flag** — a new `Event` variant carrying
  `{agent, query, results: [{score, injected}]}`, emitted by `recall_session`.
  `injected: false` means exactly "recalled but not entered into context": the
  tool drops WHOLE lowest-scored results to fit its token budget, and that
  decision — plus every result's score — previously died inside the tool's
  rendered text. This is what lets a UI show which memories reached the model and
  which were held back
- **`ToolResult::with_telemetry` / `ToolResult::telemetry`** — the seam that
  carries a tool's structured account (`tools::telemetry::ToolTelemetry`) to the
  agent loop, which forwards it to the new `ToolEventSink::tool_telemetry` hook.
  Tools stay pure functions of their arguments — no sink, no session, no agent
  name — so attribution keeps ONE source of truth and each tool stays unit-
  testable without a bus. `ToolResult::Success` is now a struct variant
  (`{text, telemetry}`); `ToolResult::ok`/`content`/`is_error` are unchanged, so
  no tool call site moved. `tool_telemetry` has a default no-op body, leaving
  every pre-existing sink impl compiling untouched
- **Build provenance in `--version` and `tcode_report.json`** — `tcode --version`
  now prints the git SHA and commit date alongside the semver
  (`tcode 0.2.0 (b20adfca 2026-07-16)`), and `tcode_report.json` carries a
  `build` object (`{version, commit, commit_date}`); the human `run-task`
  summary gains a matching `build:` header line. Previously a run's artifacts
  could not be attributed to the binary that produced them — a semver alone
  collapses every commit on a branch into one string — which during the
  2026-07-16 L4 validation forced provenance to be reverse-inferred from
  `cargo install`'s mtime reset and produced a WRONG "bug still recurs"
  conclusion, retracted only after a dedicated forensic check. Provenance is
  captured by `build.rs` from `git rev-parse --short HEAD` / `git log -1`, using
  the COMMIT date rather than a build wall-clock so rebuilds stay reproducible,
  and degrades to `"unknown"` (never a build failure, never `null`) outside a
  git checkout — the crates.io-tarball path
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))

### Fixed

- **P1: the re-delegation cap counted ALL delegations, not retries — killing
  runs before the build started.** `MAX_REDELEGATIONS = 3` was documented as
  bounding *retries after failure*, but the counter incremented on every
  engineer invocation run-wide and was never reset per call, so a *successful*
  delegation permanently consumed budget a later one then lacked. A PM using
  delegation for legitimate reconnaissance — the normal PM shape — was silently
  guillotined: the 4th call was refused **without the inner runner ever being
  invoked** (zero engineer turns, no code written), and the latched signal then
  fired `run_task`'s `with_stop_signal`, halting the PM loop at the next turn
  boundary and making it unrecoverable by design. L4 bake-off run-6 issued 3
  read-only recon delegations and had its 4th — the actual build — refused,
  ending `partial`/exit 6 with 0/9 tests and 0/5 deliverables; runs 3/4/5
  succeeded only by luck of issuing one fewer recon call. Measured cost: 2-in-7
  total-loss runs caused by the harness rather than the model. The cap now
  counts retries: `MAX_REDELEGATIONS` is checked against a counter **local to
  each delegation**, reset at loop entry, so a successful delegation never
  spends a later one's budget. Exhausting it is now **recoverable** — it no
  longer stops the PM loop, matching the precedent set by #2683/#2805's
  post-completion refusal. The cap's actual purpose is preserved by a new
  run-wide `MAX_FAILED_INVOCATIONS = 12` ceiling (4× the per-delegation
  budget): a genuinely failing engineer stays bounded, and only that ceiling —
  a condition no fresh delegation could clear — latches the loop-stopping
  signal. Not a #2805 regression; #2805 fixed the *symptom* and remains correct
  ([#2852](https://github.com/bobmatnyc/trusty-tools/issues/2852))
- **The re-delegation cap was completely silent.** It emitted no log line
  anywhere — run-6's stderr was 5 lines with no mention of it — so the only
  evidence a run had been killed was a label buried in the report's `task`
  field, discoverable only by cross-run forensic comparison. Both the
  per-delegation retry exhaustion and the run-wide ceiling now `warn!` (to
  stderr, never stdout) naming the attempt count and, for the ceiling, that the
  PM loop is being stopped
  ([#2852](https://github.com/bobmatnyc/trusty-tools/issues/2852))
- **`build.rs` re-ran on every single build, in every tree.** Its lone
  `cargo:rerun-if-changed=.git/HEAD` directive was a relative path, which Cargo
  resolves against the *package* root — i.e. `crates/trusty-code/.git/HEAD`,
  which does not exist even in a normal checkout. Cargo treats a missing
  watched path as perpetually changed, so the script never cached. Paths are
  now resolved absolutely via `git rev-parse --git-path` (also making them
  correct in a linked worktree, where `.git` is a *file* and HEAD lives in the
  worktree's gitdir) and emitted only when they exist
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))
- **Guard against a silently stale embedded SHA when the branch ref is packed.**
  Emitting any `rerun-if-changed` opts a script out of Cargo's default
  "re-run when any package file changes" heuristic, so only declared paths are
  watched. With the branch ref packed (`git gc --auto` / `git pack-refs`) no
  loose ref file exists to watch, and a subsequent commit touches neither
  `HEAD`'s content nor any other watched path — so the script would not re-run
  and the embedded SHA/date would go stale until a `cargo clean`, which is the
  very failure this ticket exists to eliminate. The append-only reflog
  (`logs/HEAD`) is now watched as well: it is touched by effectively every
  ref-changing operation regardless of whether the ref is packed or loose,
  closing the gap without reverting to an unconditional re-run
  ([#2823](https://github.com/bobmatnyc/trusty-tools/issues/2823))

### Changed

- **Deliverable-completeness guidance in the `BASE` preamble (bumped to
  `1.8.0`)** — makes completion of a task's full required-artifact SET robust to
  turn-budget variance on large tasks. On the bake-off L4 task, completion was
  nondeterministic across identical runs: run-2's engineer consumed its entire
  40-turn budget (transcript turns 9..=48) and opened its FINAL turn with "Now
  let me create the README and ARCHITECTURE files", so `ARCHITECTURE.md` was
  never written — while run-3 finished the same task in 29 turns. The provided
  suite passed 9/9 in both, so the code was never the problem: documentation was
  queued LAST, behind ~130 self-authored tests, which made it the only
  deliverable exposed to turn-budget variance. The preamble now tells the model
  to enumerate the required-artifact set as a checklist up front and track what
  remains, treats task-named documentation as a deliverable rather than an
  epilogue, orders required documents at the point the design settles (once the
  code exists and the project's own suite passes) and ahead of any discretionary
  work, forbids starting discretionary work while a required artifact is still
  missing, scales self-authored tests to the stated requirements, points the
  multi-document tail at the `write_files` batch tool (run-2 spent 19 of 40
  turns on one-file-per-turn writes), and requires a final checklist sweep
  before finishing. No turn-budget change: run-2 spent MORE turns than the runs
  that completed, so the budget was not the binding constraint. (#2824)

---

## [0.2.0] — 2026-07-16

### Added

- Surface trusty-search index readiness to daily-driver task sessions: after warming the project's index at task start, `ensure_project_indexed_in_background` now probes `GET /indexes/{id}/status` (via the shared `trusty_common::search_readiness::log_index_readiness`) and emits one stderr line describing which lanes are ready — `warn` while semantic embedding is still warming (expect lexical-only results), `info` once it is ready — so a session is never silently querying a not-yet-ready index ([#2784](https://github.com/bobmatnyc/trusty-tools/issues/2784))
- **Redundant full-suite test re-run suppression (`redundant_run`)** — the
  complement to the `verify_gate` (#2279): where that guarantees the suite runs
  at least once, this stops it running more than needed. When an identical
  `bash` test command already passed and no code has changed since (no
  `write_file`/`write_files`/`edit` and no other shell command in between), the
  agent loop short-circuits the re-run with an explanatory sentinel result
  instead of spawning the suite, so the delegated engineer stops burning turns
  on repeated "one final run to confirm" passes. Opt-in via
  `AgentLoop::with_redundant_run_suppression`, attached at the single
  delegated-engineer construction site alongside the verify gate. The `BASE`
  preamble additionally instructs the model that one clean run after the last
  code change is sufficient and not to repeat identical confirmation runs
  (preamble bumped to `1.7.0`). (#2682)

---

### Changed

- `search_code`: when the semantic (or symbol) lane returns `STAGE_NOT_READY`
  because the vector/KG index stage is still building on a freshly-indexed repo,
  the tool now transparently retries via the always-available lexical lane and
  returns those (degraded) hits instead of an empty "use grep/glob" fallback —
  so conceptual discovery still works during the embedding warm-up window
  (#2783).

### Fixed

- Bound PM re-delegation so a degenerate delegate loop cannot mislabel a
  complete run. Once a delegated engineer reports an explicit successful
  `finish_task` completion, the PM's `delegate_to_agent` tool refuses any
  further re-delegation (nudging it to `finish_task` instead), and
  `run_task`'s report assembler now reports `success`/`no_changes` — never
  `partial`/exit-6 or `deadline_exceeded` — for a run the engineer already
  completed, regardless of how the PM's own loop later terminated. This closes
  the data-integrity bug where a gratuitous post-`finish_task` re-verify round
  that ran out of turns/time corrupted run status and telemetry on a
  fully-passing, all-tests-green run (#2683).

---

## [0.1.0] — 2026-07-09

### Added

- Initial crates.io release.
