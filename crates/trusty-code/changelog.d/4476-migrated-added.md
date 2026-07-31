Added

- **`tcode tui` — the TUI REPL is reachable from the CLI (#4424, DOC-50 §4.1 /
  AC-2.4).** Every DOC-50 MVP slice was merged, but `CodeEngine` had zero
  production call sites: the whole TUI existed only under `cargo test`. The new
  subcommand discovers a running `tcode serve --http` daemon (`TCODE_DAEMON_URL`
  -> `http_addr` file -> `/health` liveness ping), then hands `CodeEngine` to
  `trusty_tui::run::run`. `--project` is optional (omit for a projectless
  session) and is canonicalized at the CLI boundary. Discovery runs BEFORE the
  alternate screen is entered, so a missing daemon prints an actionable
  `tcode tui: no tcode daemon found — start one with \`tcode serve --http\` …`
  and exits nonzero rather than flashing a TUI. This MVP does not auto-spawn a
  daemon (deliberately deferred, DOC-50 §4.1).

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
