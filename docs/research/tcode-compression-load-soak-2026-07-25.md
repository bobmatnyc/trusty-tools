# tcode Compression Load-Realistic Stress Soak — 2026-07-25

- **Epic:** [#3866](https://github.com/bobmatnyc/trusty-tools/issues/3866) ("continuous prompt-compression effectiveness tracking").
- **Closes the gap left by:** [PR #3887](https://github.com/bobmatnyc/trusty-tools/pull/3887) / issue #3869. That soak scored PASS (224 turns, 0 compaction events, working-context floor 94-95%) but its own report flagged, as the single biggest caveat, that it never stressed the 60%-floor boundary: five of six per-call turns carried near-empty arguments and peak overhead never exceeded ~7% of a 200K window. **This soak is the load-realistic follow-up that report called for.**

## Headline result

**PASS, at the boundary — with a reproducible FAIL just beyond it.**

| | Primary run (shipped default) | Exploratory run (heavier, not shipped) |
|---|---|---|
| Per-call `set_goal` payload sizes | 160 KB / 230 KB / 300 KB | 220 KB / 300 KB / 400 KB |
| `task.run` calls / PM turns | 35 calls / 245 turns | 20 calls / 140 turns |
| `tcode-cadence` events | 126 | 73 |
| `tcode-threshold` (fallback compaction) events | **0** | **0** |
| Working-context floor | **60%** (2 samples at exactly 60%, 34 at 61%) | **48%** (21/73 samples below the 60% target) |
| Compression ratio (tokens_after/tokens_before) min/median | 0.3417 / 0.3493 | not the focus of this run |
| `compression_report.py` verdict | **PASS** | **FAIL** |
| Session fidelity after the soak | **PASS** (goals clean, transcript readable, session resumed and completed a further `task.run` call) | **PASS** (same three checks, even through the breach) |

**The mechanism holds under heavy, load-realistic pressure — but only just, and it genuinely breaks under sufficiently extreme sustained turn sizes.** The primary run drove the measured floor down from #3887's comfortable 94-95% to exactly 60-61%, i.e. essentially zero margin left on two samples. Pushing per-call payload size up by another ~35% reproducibly breached the 60% target (floor 48%, 21 of 73 cadence-fire samples below target) — a genuine, unforced FAIL, not a tuning artifact. Both runs are committed as evidence; the harness's shipped default is the boundary-finding (not the breaking) load level, because that is the more representative "real tool output" magnitude — see [Load profile](#load-profile) below.

## Load profile — what was driven, and why it's representative

**The gap identified in #3887:** `SoakEchoLlmClient` (the original soak's mock PM) issues five near-empty `set_goal`/`clear_goal` turns and exactly ONE ~8 KB oversized turn per `task.run` call. That's nothing like what a real long-lived coding session accumulates — a `cargo test` failure dump, a multi-file `git diff`, or a `grep -r` result set routinely run tens to hundreds of KB, and a real session hits several of these in a row, not once per 32 calls.

**This soak's mock client** (`crates/trusty-code/src/task/mock_llm_soak_load.rs`, `TCODE_MOCK_LLM=echo-soak-load`) keeps the exact same 7-turn-per-call, resumable-`stop`-ending shape #3887 established (required so the harness can call `task.run` on the same `session_id` dozens of times — see that PR's report for why), but sizes **every** `set_goal` turn's argument — not one in six — at a magnitude representative of a real tool-output dump:

- Turn 1 `set_goal`: 160 KB — approximates a large `git diff`.
- Turn 3 `set_goal`: 230 KB — approximates a `grep -r`/search-result dump across a mid-size repo.
- Turn 5 `set_goal`: 300 KB — approximates a `cargo test` failure log with several panicking tests.
- Turns 2/4/6 (`clear_goal`): tiny, as in the original soak — every large contribution is unambiguously attributable to a `set_goal` turn when reading the resulting JSONL.

The payload text itself is **not** a single repeated character (the original soak's `"x".repeat(8000)` approach) — `synthetic_tool_output()` interleaves six realistic line shapes (unified-diff hunks, `grep path:line:match` rows, `cargo test FAILED`/panic/assertion lines) cycling until the target size is reached, so a human reading the committed JSONL/evidence sees plausible-looking content instead of a wall of `x`. **This is a readability choice, not a validated measurement variable.** Every number this soak reports comes from a content-blind path: `estimate_tokens` is `chars().count() / 4` (no parsing), eviction (`enforce_budget`/`resolve_keep_from`) removes whole messages by position not content, and `summarize_span` replaces an evicted span with a small fixed-format placeholder regardless of what was in it. A `"x".repeat(N)` blob of the same byte length would produce byte-for-byte identical `tokens_before`/`tokens_after`/ratio/floor numbers to what is reported below — only `LOAD_PAYLOAD_BYTES` (payload size) drives the result. This also bounds the soak's coverage: it says nothing about how a content-*aware* compaction strategy would behave, because this crate's compaction path has no such strategy to exercise.

**Why this size, specifically:** the three payloads sum to ~172K estimated tokens (chars/4) per call — already past the model's entire 80K-token overhead cap (`CadenceConfig::default().overhead_cap_tokens(200_000)`, the 40%-of-200K-window cap from epic #2343's own worked example) within a SINGLE `task.run` call, not just cumulatively across a long soak. This is what makes the difference from #3887: `cadence::enforce_budget`'s continuous per-turn enforcement now has to do real, repeated work almost every turn (median 7 rounds per cadence-fire event, up to 8 — the active zone's full size), not fire once across the whole run.

An even heavier profile (220/300/400 KB, ~230K tokens/call) was run exploratively to find the actual breaking point — see [Exploratory FAIL run](#exploratory-fail-run) below — but is **not** the harness's shipped default: at that size a single call's raw, pre-compaction content already exceeds the entire 200K context window by itself (up to 305,735 estimated tokens observed), which is a plausible but less common "worst session ever" scenario rather than the representative load this harness is meant to exercise by default.

## Driver

`crates/trusty-code/scripts/compression_load_soak.py` — reuses `compression_soak.py`'s daemon-lifecycle/RPC plumbing (`spawn_daemon`, `rpc_call`, `wait_for_terminal_status`) unchanged, parametrizing `TCODE_MOCK_LLM` to `echo-soak-load` instead of `echo-soak`. Same real `tcode serve --http` daemon, real JSON-RPC wire, real `compression.jsonl` telemetry (Slice A/B) as #3887 — this is deliberately not a new measurement mechanism, only a heavier load profile driven through the existing one.

**Fidelity checks — new in this soak, not present in #3887's harness.** After the load-driving `task.run` calls complete, the driver performs three checks the dispatching task specifically asked for ("does the session still behave correctly after being compressed, or does it lose necessary state?"):

1. **`session.get_goals`** — every `set_goal` in the script is immediately followed by a `clear_goal` for the same slot, so all three touched slots (1, 2, 3) must report empty. A stale slot would mean compaction corrupted or silently dropped a write.
2. **`session.get_transcript`** — must return without an RPC error, with `compaction_events == 0` (the `TranscriptRecord`'s own durable threshold-compaction counter). This is a third independently-*recorded* signal alongside the JSONL's `tcode-threshold` count and `session.get_context_budget`'s `lifetime_compaction_alarm_count` — but all three are written from the SAME `if !transcript.maybe_compact(...) { return; }` call site in `agent_loop/compaction_control.rs`, so they corroborate each other only against a *recording* failure (e.g. best-effort JSONL/log I/O silently dropping a write), never against a *detection* bug in `Transcript::maybe_compact` itself — if that function should have fired and didn't, all three read 0 together and this check cannot tell the difference between "no backstop needed" and "backstop failed to notice."
3. **One more `task.run` call**, issued against the SAME session after all the load-driving calls (and however much compaction pressure they caused), must still complete via `SessionStatus::Finished` — proof the loop is still healthy post-compaction, not silently wedged, erroring, or stuck permanently terminal.

## Results — primary run (shipped default load profile)

Real numbers from `docs/research/evidence/tcode-compression-load-soak-2026-07-25/primary-run/`:

- **245 PM turns** driven (35 `task.run` calls x 7 turns/call), session `aba1109f-9a3a-4634-9d2c-a42992293f06`.
- **126 `tcode-cadence` events**, **0 `tcode-threshold` events** — the reactive fallback compactor never fired even at this load; cadence's continuous enforcement absorbed everything.
- **Working-context floor: 60%** — 2 samples landed at exactly 60%, 34 at 61%, the rest ranging up to 99% (`working_context_pct_after` across all 126 samples). `compression_report.py`'s own verdict: **PASS** (`"never dropped below 60% at any sample"` — technically true, with as little margin as the metric allows).
- **`tokens_before` (pre-enforcement, this call's raw accumulated size):** min 3,336, median 177,113, max 231,083 — the median call already carries ~88% of the entire 200K context window in raw content before cadence compacts it back down; the max single observation (231,083 estimated tokens) *exceeds the whole window* before enforcement acts.
- **Compression ratio (tokens_after/tokens_before):** min 0.3417, median 0.3493, p95/max 1.0 (the p95/max 1.0 entries are turns where nothing needed compacting — a bare pass-through, correctly reported as ratio 1.0 per the report generator's own convention).
- **Overhead/latency:**
  - `duration_ms` (the compaction pass itself, per event): min 0, median 6, max 17 — negligible; sustained heavy load does not make the compaction computation itself expensive.
  - `rounds` (enforce_budget iterations per event): min 1, median 7, max 8 — real, visible work (the mechanism is genuinely earning its keep at this load, unlike #3887's mostly-1-round runs), but cheap in wall time per the `duration_ms` figures above.
  - Per-call wall time (`task.run` round-trip, 7 turns): min 104.7ms, median 170.3ms, mean 240.6ms, max 2020.7ms. The single 2-second outlier is call #2 in every one of this soak's three runs (primary, heavier, and the earlier smoke tests) — a reproducible one-time JIT/allocation warmup on the SECOND call, not a sustained cost; every other call stays under 290ms.
- **Session fidelity: PASS.** `goals_after_soak: []` (all three touched slots clean), `transcript_compaction_events: 0` (a third independently-recorded — not independently-*detected*, see the Driver section above — signal agreeing with zero fallback-compaction fires), `resume_after_soak_status: "finished"` (the session successfully ran one more full task after 245 turns of sustained heavy compaction pressure).
- **RPC cross-check caveat (same one #3887 flagged, confirmed again here):** `session.get_context_budget`, polled once per call, reported `working_context_pct` between 98-99% throughout this run — nowhere near the JSONL's observed 60-61% floor. The RPC snapshot is a coarse once-per-7-turn sample that does not reliably land on the turn where the floor was lowest; **it is not a substitute for the JSONL** for catching a transient dip, a gap this report is making explicit since an operator monitoring only via RPC would see no signal of how close to the boundary the session actually got.

Full generated report: `docs/research/evidence/tcode-compression-load-soak-2026-07-25/primary-run/generated-report.md`.

## Exploratory FAIL run (not the shipped default)

Bumping the per-call payload to 220 KB / 300 KB / 400 KB (~230K estimated tokens/call — more than the entire context window in raw content before any compaction) and running 20 calls (140 turns) reproducibly **breached** the 60% target:

- **Working-context floor: 48%** — 21 of 73 cadence-fire samples landed below the 60% target (as low as 48%, several samples clustered at 49%).
- **`tokens_before` max: 305,735** estimated tokens — over 150% of the 200K window's raw size before enforcement acts.
- **`tcode-threshold` events: still 0.** This is the notable part: even while the cadence-level floor guarantee was being violated, the independent reactive/fallback compactor — the mechanism that's supposed to be the backstop once cadence fails to hold budget — never engaged. `lifetime_compaction_alarm_count` (Slice B's durable never-event alarm) stayed 0 throughout.
- **`compression_report.py`'s own verdict on this run: FAIL** (`"working context dropped below 60% at 21 sample(s) (floor: 48%)"`) — the tooling built for this epic correctly detects and reports its own mechanism's failure; this was not eyeballed.
- **Session fidelity: still PASS.** Goals still clean, transcript still readable with `compaction_events: 0`, and the session still successfully resumed for one more `task.run` call after the breach. **No crash, no corruption, no permanently-wedged session** — the failure mode is specifically "the stated working-context guarantee is violated," not "the session breaks."

Evidence: `docs/research/evidence/tcode-compression-load-soak-2026-07-25/exploratory-fail-run/`.

## Verdict for epic #3866

**Mixed — do not close the epic on this alone.**

1. The #2346 cadence mechanism genuinely holds under a realistically heavy load profile (per-call raw content approaching/exceeding the entire context window), with the measured floor landing right at, not comfortably above, its stated 60% target. This is the honest, load-realistic confirmation #3887 could not provide — the previous 94-95% floor said nothing about behavior anywhere near the actual boundary.
2. The SAME mechanism, pushed only ~35% further, **breaks its own stated guarantee** (floor 48%, 21/73 samples below target) — a real, reproducible negative result, not a fluke or a tuning artifact (`compression_report.py` itself flags it FAIL).
3. **The reactive/fallback compactor (#2308, the mechanism that's supposed to be the backstop) never engages even when cadence's own guarantee is breached.** Under the design this crate documents (`cadence: Some(_)` → threshold compaction should be a "never-event"), that's arguably correct behavior in isolation — but it means there is currently **no safety net** that catches a cadence-level floor breach; the epic's "zero threshold-compaction events" success metric being met is not evidence the floor guarantee held, and this run demonstrates that gap concretely. (Caveat on how we know it's 0: the JSONL row, `TranscriptRecord.compaction_events`, and `lifetime_compaction_alarm_count` all read 0 in agreement, but all three are written from the same `Transcript::maybe_compact` call site — see the Driver section. This measurement rules out a *recording* failure, not a *detection* bug; "no backstop fired" and "a backstop should have fired and silently didn't" are indistinguishable in this data.)
4. **The operator-facing RPC cross-check (`session.get_context_budget`) does not reliably surface either the near-boundary primary run or the breach in the exploratory run** — it reported 98-99%/98-99% throughout both, while the JSONL told the real story. Anyone monitoring compression health via that RPC alone (rather than reading `compression.jsonl` directly) would have no visibility into how close to, or past, the floor a real session actually got.
5. **Session-level fidelity (goal state, transcript integrity, resumability) held in both runs, including through the breach.** Compaction losing free-form context that isn't captured by a structured mechanism like `GoalSlots` remains untested by this harness (the mock script's only stateful surface is goals, which live outside the compactable transcript by design) — a genuine limitation of this soak's coverage, not a claim that all information survives compaction.

**Recommendation:** epic #3866 should stay open against at least two concrete findings from this soak: (a) there is no backstop for a cadence-level floor breach under extreme load, and (b) the RPC observability surface (`session.get_context_budget`) does not reflect the real floor closely enough to be trusted for operator-facing monitoring. Both are new, load-derived findings this report is surfacing for the first time — neither was visible in #3887's mechanism-only PASS.

## Reproduce

```bash
cargo build -p trusty-code --bin tcode
# Primary (shipped default) load profile — 160/230/300 KB per call, 35 calls:
python3 crates/trusty-code/scripts/compression_load_soak.py --calls 35
python3 crates/trusty-code/scripts/compression_report.py <printed compression_jsonl path>
```

To reproduce the exploratory FAIL run — no rebuild needed, `--payload-bytes`
sets `TCODE_SOAK_LOAD_PAYLOAD_BYTES` on the spawned daemon
(`SoakLoadEchoLlmClient::LOAD_PAYLOAD_BYTES_ENV_VAR`):

```bash
python3 crates/trusty-code/scripts/compression_load_soak.py \
  --calls 20 --payload-bytes 220000,300000,400000
```

## Evidence

Committed under `docs/research/evidence/tcode-compression-load-soak-2026-07-25/`:

- `primary-run/` — `compression.jsonl` (126 lines), `context_budget_samples.json` (35 samples), `fidelity_check.json`, `generated-report.md`, `run.log` (the harness's own stderr progress log) for the shipped-default 160/230/300 KB, 35-call run.
- `exploratory-fail-run/` — the same four artifacts (no `run.log`) for the 220/300/400 KB, 20-call run that breached the 60% target.
