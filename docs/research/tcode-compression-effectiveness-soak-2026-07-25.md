# tcode Compression-Effectiveness Soak — 2026-07-25

- **Issue:** [#3869](https://github.com/bobmatnyc/trusty-tools/issues/3869) ("trusty-code: compression-effectiveness soak harness + report", epic #3866 Slice C).
- **Epic being scored:** [#2343](https://github.com/bobmatnyc/trusty-tools/issues/2343) ("Infinite Sessions") — stated success metric: *"a 500+ turn interactive session with `compaction_events == 0` and working context never below 60%"*.
- **Telemetry this report reads from:** Slice A ([#3867](https://github.com/bobmatnyc/trusty-tools/issues/3867), landed via PR #3880, `4a5c4b6b`) + Slice B ([#3868](https://github.com/bobmatnyc/trusty-tools/issues/3868), landed via PR #3885, `c003c63a`) — `crates/trusty-code/src/agent_loop/telemetry.rs`'s `CompressionEvent` JSONL schema and the `compaction_alarm.log` never-event alarm.
- **Verification method:** a real `tcode serve --http` daemon subprocess, driven over its actual JSON-RPC wire (never the Rust API directly) by a new harness script, producing a real `compression.jsonl` for a real `session_id`, scored by a new report generator with its own unit tests. Raw evidence committed alongside this report — see [Evidence](#evidence) below.

## Headline result

| | |
|---|---|
| **Verdict** | **PASS** |
| Session id | `7ae5f495-9931-4025-8de6-672f37489646` |
| Total PM turns driven | 224 (32 `task.run` calls x 7 turns/call) |
| `tcode-cadence` events | 28 |
| `tcode-threshold` events (`compaction_event: true`) | **0** |
| Working-context floor (JSONL, per cadence-fire sample) | **95%** (28 samples, all ≥ 95%) |
| Working-context floor (RPC cross-check, per-call sample) | **94%** (32 samples, all ≥ 94%) |
| Compression ratio (tokens_after / tokens_before) — min / median / p95 / max | 0.3010 / 0.5340 / 0.7260 / 0.9990 |
| Lifetime compaction-alarm count (Slice B, `session.get_context_budget`) | **0** throughout |

Both of epic #2343's targets are met on this run: working context never dropped below 60% (it never dropped below 94%), and the threshold (backstop) compactor never fired once. See [Caveats](#caveats-read-before-treating-this-as-final) — this soak proves the mechanism functions and repeatedly compresses under a large-argument turn, but does not stress the 60% floor anywhere near its boundary; see that section for why.

## Harness

**Why a scripted mock agent, not a real model** (per issue #3869's explicit preference): a real-model soak of 200+ turns burns real tokens/dollars for no benefit here — the goal is exercising the *cadence mechanism* (turn counting, span compaction, continuous budget enforcement), not model quality. `crates/trusty-code/tests/*_e2e.rs` already establishes the precedent of driving the daemon via `TCODE_MOCK_LLM=<variant>` for deterministic, offline, zero-cost e2e coverage; this harness follows the same pattern with a new variant built for sustained multi-call soaking.

**New mock LLM variant — `TCODE_MOCK_LLM=echo-soak`** (`crates/trusty-code/src/task/mock_llm_soak.rs`, wired into `crates/trusty-code/src/task/mock_llm.rs`'s existing `build_llm_client` seam): every *other* scripted client in this crate (`EchoLlmClient`, `FanoutEchoLlmClient`, etc.) plays a short, fixed script (4-6 calls) and errors once exhausted — fine for a single `task.run`, wrong for a soak. `task::protocol::task_run` rebuilds the LLM client fresh on **every** `task.run` call, so `SoakEchoLlmClient` only needs to script ONE call's worth of turns: 6 `set_goal`/`clear_goal` tool calls (the PM registry's own goal-slot tools — no delegation, no external service dependency) followed by a bare `stop`. One of the six tool calls carries a deliberately **oversized** `text` argument (~8 KB) specifically to exercise `cadence::enforce_budget`'s *continuous*, every-turn enforcement path — not just the scheduled every-8-turns cadence fire — per the issue's explicit requirement that a soak with no oversized turns doesn't test the mechanism the epic cared about most.

**Why the script ends in a bare `stop` (a design constraint discovered empirically):** `session::registry::SessionRegistry::begin_execution` permanently rejects further `task.run` calls once a session reaches `SessionStatus::Failed`/`Cancelled`/`DeadlineExceeded` — only a `Finished` session may resume. The very first version of this harness let each call run all the way to `max_turns` (8, `AgentLoopConfig::default`), which maps `TurnCapExceeded` onto `Failed` — the very first repeat `task.run` call then failed with `session ... is already terminal`. The fix (now the shipped design): 7 turns per call, always ending in a natural no-tool-calls stop, comfortably under the 8-turn cap, so every call completes via `Ok` → `SessionStatus::Finished` → resumable.

**Driver** (`crates/trusty-code/scripts/compression_soak.py`, stdlib-only Python, no new Rust example/binary needed): spawns `tcode serve --http --port 0` rooted at a throwaway one-agent project fixture (`.claude/agents/pm.md`, `model: bedrock/us.anthropic.claude-sonnet-4-6` — resolves to a 200K-token context window via `provider::routing::resolve_context_window`, matching epic #2343's own worked example of an 80K/40% overhead cap and 120K/60% working floor at that window size), with `TCODE_MOCK_LLM=echo-soak` and `TCODE_TELEMETRY_DATA_DIR` pointed at an isolated output directory (so this synthetic run's numbers never land in a real `~/.trusty-code/compression.jsonl`). Sequence:

1. `session.create` — mints the session.
2. `task.run(session_id=..., mode="daily-driver")` x32, polling `session.status` to a terminal state after each call before issuing the next (required — a second overlapping run is rejected).
3. `session.get_context_budget` after every call — an independent cross-check of the JSONL telemetry, sourced from `SessionRegistry`'s own cached `ContextBudgetSnapshot`, not from re-reading the JSONL.
4. `session.cancel` at the end (cleanup; the session was already terminal).

Ran against `HarnessMode::DailyDriver` with `cadence: Some(CadenceConfig::default())` (`cadence_turns: 8`, `max_overhead_fraction_pct: 40`) — the exact mode/config combination epic #2343's guarantee applies to (`task::executor::run_and_record`'s PM loop is the one call site in the crate that ever sets `cadence: Some(_)`; Parity mode and `cadence: None` runs are out of scope, same carve-out `maybe_compact_transcript`'s own docs state).

## Report generator

`crates/trusty-code/scripts/compression_report.py` — a stdlib-only Python reducer (`load_events` / `compute_ratio_stats` / `compute_context_floor` / `compute_compaction_count` / `compute_verdict` / `render_markdown`), runnable independently against a previously-captured JSONL (no daemon, no re-running the soak) per the issue's explicit "iterate on report formatting without re-paying the 200-turn cost" acceptance criterion. Unit-tested against a hand-crafted fixture in `crates/trusty-code/scripts/compression_report_test.py` (13 tests, `python3 -m unittest compression_report_test`): the ratio-distribution math, working-context floor detection (including a sample flagged below 60%), the compaction count, and — the issue's explicit regression case — that a single `compaction_event: true` row flips an otherwise-healthy run's verdict to FAIL.

The generator's raw output for this run is committed verbatim at `docs/research/evidence/tcode-compression-effectiveness-soak-2026-07-25/generated-report.md`; the numbers above are drawn from it.

## Cross-check: JSONL vs. RPC-observed working context

Two independent sources agree on the key finding:

- **JSONL** (`tcode-cadence` events, `working_context_pct_after`): sampled at every actual cadence fire (28 samples) — floor 95%, no sample below 95%.
- **RPC** (`session.get_context_budget`, polled once per `task.run` call, 32 samples): a coarser sample — one snapshot per 7-turn call, capturing whichever turn's measurement was cached last (not necessarily a cadence-fire turn) — floor 94%, no sample below 94%. Of the 32 per-call snapshots, 4 show `compaction_fired: true` (cadence actually did work on that call's final sampled turn); `lifetime_compaction_alarm_count` (Slice B's durable threshold-compaction alarm) stayed **0** across every single sample — corroborating the JSONL's `tcode-threshold` event count of zero.

The two sources' small floor difference (95% vs. 94%) is expected: they sample different turns within each call, not the same event twice.

## `#3843` cross-check (requested by the dispatching task)

[#3843](https://github.com/bobmatnyc/trusty-tools/issues/3843) ("ws-summary cadence-freeze past 200 turns") is a `trusty-agents` bug in the `agents-ws-summary` surface (`crates/trusty-agents/src/ctrl/pm_task/dispatch/classification.rs`), a **different crate and a different compression surface** than this harness exercises. Per issue #3869's own explicit scope note, covering `agents-ws-summary` is out of scope for this slice ("a Slice-D-adjacent follow-up," not this one) — this harness only drives `trusty-code`'s `tcode-cadence`/`tcode-threshold` surfaces. **This run has no evidence either way on #3843** — it simply never touches that code path. #3843 remains open and unrelated to this report's PASS verdict.

## Caveats (read before treating this as final)

1. **Scripted-agent representativeness — the biggest one.** `SoakEchoLlmClient` issues small, bounded tool-call arguments (a handful of short strings plus one ~8 KB oversized argument per call) — nothing like the token volume a REAL coding session accumulates (large `cargo test` output, multi-file `git diff`s, big `grep`/search results, long file reads, `delegate_to_agent` round-trips with a real engineer transcript). This run's overhead never got past ~7% of the 200K window (peak ~11.5K tokens before a compaction pass), nowhere near the 80K/40% cap the epic's floor guarantee is actually about. **This soak proves the cadence mechanism functions correctly and keeps compressing under a deliberately oversized turn — it does NOT prove the 60% floor holds under realistic token pressure**, because no run here got anywhere close to that boundary. A follow-up soak that replays REAL tool-output volumes (or a real-model run against an actual multi-file coding task) is the natural next step to stress the floor for real; this run establishes the harness and the mechanism-level PASS, not a load-realistic stress test.
2. **`session_id` nulls on non-PM paths** — a known design note from PR #3880's review: `AgentLoop::with_session_id` is only wired on the daemon-session PM loop (`task::executor::run_and_record`), so any OTHER `cadence: Some(_)` call site (there is currently exactly one — the PM loop — so this is presently theoretical) would emit `session_id: null` telemetry. This run's 28/28 events all carry the real session id, confirming the PM path is correctly attributed; it says nothing about paths this harness doesn't exercise.
3. **RPC polling granularity.** `session.get_context_budget` was polled once per 7-turn `task.run` call (32 samples across 224 turns), not every turn — it captures whichever turn's snapshot was cached last, not necessarily the turn cadence fired on. The JSONL (28 samples, one per actual fire) is the authoritative source for the ratio/floor numbers behind the PASS verdict; the RPC samples are a corroborating cross-check, not the primary measurement.
4. **7 turns/call, not 8.** `AgentLoopConfig::default().max_turns` is 8; this harness's script deliberately uses 7 so every call ends in a natural stop rather than hitting the cap (see the Harness section above for why that distinction matters for session reusability). `cadence_turns` stays at its default of 8, so cadence fires land at slightly different points within each call as the cumulative turn count crosses each multiple of 8 — visible in the 28-fires-across-32-calls ratio above.

## Evidence

All committed under `docs/research/evidence/tcode-compression-effectiveness-soak-2026-07-25/`:

- `compression.jsonl` — the real, unmodified telemetry Slice A/B's instrumentation wrote for this run (28 lines).
- `context_budget_samples.json` — the harness's own `session.get_context_budget` RPC cross-check (32 samples).
- `generated-report.md` — `compression_report.py`'s raw output against the JSONL above, scoped to this run's `session_id`.

Reproduce end to end:

```bash
cargo build -p trusty-code --bin tcode
python3 crates/trusty-code/scripts/compression_soak.py --calls 32
# -> prints {"session_id": ..., "compression_jsonl": ..., ...}
python3 crates/trusty-code/scripts/compression_report.py <compression_jsonl> --session-id <session_id>
```

Report generator unit tests (no daemon needed):

```bash
cd crates/trusty-code/scripts && python3 -m unittest compression_report_test -v
```
