# trusty-tools test-suite audit — 2026-09-27

Read-only audit per Bob's 2026-09-27 ruling. No code changes, no test runs.
Sources: repo grep, `.github/workflows/ci.yml`, `scripts/check_ignored_tests.sh`,
`scripts/prepublish-ignored-tests.tsv`, `gh run`/`gh issue` on
`bobmatnyc/trusty-tools`, checkout at `main` (`3213241db0`).

## Headline metrics — baseline for the EFFECTIVE TESTS goal

These are the report's own numbers (detail and derivation in §1 and §3 below);
no divergence from the audit brief's estimates was found beyond what each
section already flags inline.

| Metric | Count |
|---|---|
| Ignored tests never run in any CI job (unselected by the ratchet gate) | **151** |
| `#[ignore]` attributes with no reason string | **321** |
| Total `#[ignore]` attributes in the tree | **~442** |
| Wall-clock-bound asserts in tests | **18** |
| `sleep()` calls in test-directory files | **94** |

## 1. Wall-clock asserts + sleep() in tests

**Wall-clock bound asserts** (`.elapsed() < / >` a `Duration`, in any `tests/`
dir or `src/**/tests*`): 18 hits.
- `crates/trusty-agents/tests/support/api_server.rs:257,363,518` — polling
  timeout guards (a), not a correctness assertion on duration.
- `crates/trusty-analyze/tests/stdio_harness.rs:214` — startup poll loop (a).
- `crates/trusty-code/src/permissions/tests/gate_tests.rs:593` — asserts a
  gate call returns inside 10s (b, deliberately timing behavior).
- `crates/trusty-console/tests/search_uds_bridge.rs:635` — asserts a drop
  completes inside 2s (b).
- `crates/trusty-memory/src/tools/tests/write_liveness_tests.rs:38,144` — a
  `SETTLE` bound asserted directly on elapsed time (b, flake-prone — see §6).
- `crates/trusty-memory/tests/mcp_stdio_tools.rs:890` — asserts <2s (b).
- `crates/trusty-mpm/tests/memory_verbs_socket.rs:251`,
  `tm_hook_idle_parking.rs:93` — poll-loop deadlines (a); the former is the
  socket flake in [issue #8464](https://github.com/bobmatnyc/trusty-tools/issues/8464).
- `crates/trusty-search/tests/benchmark_*.rs` (3 files), `corpus_*quarantine*.rs`
  (2 files) — reindex-timeout polling (a) and a save-debounce check (a).
- `crates/trusty-search/src/core/indexer/rehydrate_tests.rs:60`
  (`detached_rehydrate_survives_caller_cancellation`) failed once with "must
  not re-scan; took 125ms" in an unrelated rung-5 run on 2026-09-27, passed
  on rerun.

Close to the audit brief's "~15"; the extra 3 are `src/**/tests/` files the
brief's own grep likely excluded by only scanning top-level `tests/`.

**`sleep()` calls in test-directory files** (measured directly, not the
brief's ~343 estimate — see Prompt feedback): 94 calls across 60 files
(substring "sleep" incl. non-paren mentions: 135). Classification by sample
(most files inspected):
- (a) **condition polling** (majority) — `tokio::time::sleep(POLL_INTERVAL)`/
  `POLL`/`quiet` inside a retry loop: `trusty-search/tests/benchmark_open_mpm.rs:414`,
  `trusty-memory/tests/orphan_reap_7085.rs:111,139,424`,
  `trusty-mpm/tests/*` (daemon_dual_serve.rs:123, e2e/harness.rs:201,
  test_session_lifecycle.rs:284), `trusty-console/tests/*_uds_bridge.rs`,
  `trusty-crate-contracts/tests/*`. These should mostly be condition-polling
  helpers already (`tm wait`-style loops) — a handful use a bare
  `sleep(fixed_ms)` with no retry check, which is the pattern to convert.
- (b) **deliberately testing time / fixed settle windows**: `trusty-audit`'s
  `a_hung_child_is_killed_and_recorded` (600s literal, see §4);
  `trusty-mpm/tests/tm_hook_pm_guard.rs:1928` (`sleep(10s)` to simulate a
  silent/stalled process); `trusty-mcp/tests/daemon_bridge_json_rpc_uds.rs:96`
  (`sleep(300s)`); `trusty-mpm/session_manager_mvp.rs:2639` (200ms settle).
- (c) **unclear without deeper read**: `trusty-mcp/tests/single_flight_exclusion.rs:385`
  (`sleep(3600s)` — almost certainly a "never returns" sentinel, not a real wait).

## 2. Status-asserted-without-side-effect (sample, [#8499](https://github.com/bobmatnyc/trusty-tools/issues/8499) round-3 pattern)

Scanned `trusty-agents/src/api/server/tests/*` (route handlers) and
`trusty-mpm/src/daemon/managed_routes/*`.
- `crates/trusty-agents/src/api/server/tests/event_tickets.rs:176`
  (`bearer_token_opens_event_stream`) — asserts `StatusCode::OK` on the SSE
  upgrade only; never asserts an event is actually delivered on the stream.
- `crates/trusty-mpm/src/daemon/managed_routes/prune.rs:592` — the "CONTROL:
  absent is legitimate" branch asserts `ok.status() == OK` but not the
  response body (`dry_run`/pruned-count echo the adjacent test at line
  ~601-620 DOES check) — this one branch is the asymmetric case.
- Counter-examples (correct pattern) for contrast:
  `trusty-agents/src/api/server/tests/agent_patch.rs:362-368` checks status
  AND decodes the body AND asserts specific fields.

Most `assert_eq!(resp.status(), StatusCode::OK)` call sites in
`trusty-agents/src/api/server/tests/` (111 occurrences of status-code-style
assertions; 15+ of the `StatusCode::OK` form sampled) go on to decode and
assert the body in the same test — the crate is largely disciplined. The
`trusty-memory/src/tools/tests/` and `trusty-mpm` hook-handler tests showed
no bare `is_ok()`-without-follow-up matches in this sample; a full sweep
of MCP tool handlers was not completed within budget (see Prompt feedback).

## 3. `#[ignore]`d tests — canonical numbers from `scripts/prepublish-ignored-tests.tsv`

This manifest is the authoritative, actively-maintained count (own ratchet
gate, `pre-publish.yml` job `ignored-tests`), not a stale doc:
- **164** ignored tests enumerable on the Linux CI runner (excl. 4 Tauri UI
  crates); **13 rows selected and RUN** by the gate (HuggingFace ONNX model
  download or public GitHub-release read only — needs no key/daemon/binary);
  **151 unselected** (ratchet baseline).
- **~442** `#[ignore]` attributes exist in the tree total (nextest "tests"
  vs. raw attribute count differ because some belong to non-default-run
  binaries); **321 of those carry no reason string** — my own anchored
  attribute-line grep independently counted 169 with an unambiguous
  single-line match plus ~121 reasoned strings sampled, consistent with the
  manifest's order of magnitude; the manifest's own count is the one to cite.

Groups (from the manifest's own classification + my `#[ignore = "..."]`
sample of 121 reasoned attributes):
- **Still needed — network/credential gated**: OPENROUTER/ANTHROPIC/
  FIREWORKS/TOGETHER/ATLASCLOUD/OPENAI/AWS API keys; live `gh`+network;
  `api.linear.app`; live GitHub PR. Correctly unselected — CI holds none of
  these keys.
- **Still needed — local daemon/binary/venv**: live trusty-search on :7878,
  compiled `trusty-search`/`trusty-analyze`/`trusty-embedderd` binary, a
  bootstrapped Python/torch or `kuzu`/`kuzu_memory` venv, a real tmux host.
  Structurally can't run on a bare CI checkout.
- **Still needed — mutually exclusive pair**: "requires claude binary absent
  from PATH" vs "only runs when tmux is absent from PATH" — no single runner
  satisfies both; this is a real, permanent split, not a bug.
- **Helper/re-entry, not a test**: `crates/trusty-memory/tests/orphan_reap_7085.rs`'s
  re-entry helper, `trusty-mpm/tests/projects_json_concurrency.rs`'s two
  `#[ignore]`d child-process helpers, `trusty-search/tests/benchmark_open_mpm*.rs`'s
  RSS-sample helper. These count in the ~442 but are not really "skipped
  coverage" — they're subprocess entry points.
- **Likely stale / candidates for reclassification**: none confirmed dead by
  this pass — the manifest itself states classifying the 321 unreasoned
  `#[ignore]`s "is real work that does not belong in the PR that builds the
  gate" (i.e., deliberately deferred, not abandoned). No CI job was found
  that runs any set beyond the manifest's 13 rows, confirming the brief's
  premise: everything outside those 13 truly never executes in CI.

## 4. Slowest tests — CI shard 5 (from `gh run view --log`, run 36317030683, 2026-09-27 11:50 UTC, shard job 108613678462)

Shard wall-clock: shard5 **1627s** (11:51:20–12:18:27) vs shard2 **1233s**,
shard8 **1243s**, shard1 **1187s**, shard7 **1224s**, shard6 **1116s**,
shard4 **1181s**, shard3 **1103s** — shard5 is ~400-500s over the pack.

**Root cause, found directly in the nextest PASS log**: one test —
`trusty-audit::run::run_tests::a_hung_child_is_killed_and_recorded`
(`crates/trusty-audit/src/run.rs:2795`) — runs **600.108s**, ~37% of the
shard's total wall time, and lands last (4735/4735), so it is on the
shard's critical path. Its own source
(`crates/trusty-audit/src/run.rs:2798,2807`) spawns a stub that does
`sleep 600` and calls `sweep_with_budget(..., Duration::from_millis(200), ...)`
expecting the 200ms budget to kill the child — but the log timing shows the
test actually waits out the full 600s sleep, meaning **the kill/timeout path
is not honored under CI** (works "by accident" only because the test still
passes on the assertions once the child eventually exits). This is a real
functional bug in the timeout/kill mechanism, not just a slow test.
Three more tests exceed 10s: `trusty-agents::tools::analysis::trace_flow::tests::trace_flow_missing_entry_errors`
(25.6s), `trusty-analyze::on_demand::an_mcp_session_outlives_the_idle_window`
(12.7s), `trusty-code::tui_client::engine_state::session_events_tests::session_stream_silence_is_bounded_not_infinite`
(11.2s) — all deliberately time-based (§1b), smaller payoff.

## 5. Open flaky/hermeticity issues

No `flaky-test` label exists in the repo; `area:test-hermeticity` (17 open,
matches the brief) is the operative one, plus workstream label
`ws/1.7.2-ci-flakes` (0 open at query time — recently drained). All 17,
one-line status:
- [#8770](https://github.com/bobmatnyc/trusty-tools/issues/8770) vector_gap backfill test times out under full-suite load
- [#8765](https://github.com/bobmatnyc/trusty-tools/issues/8765) wide_bracket_runs_scan_in_linear_time — 1s limit sits just above
  normal debug-build time (this IS the wall-clock-bound-flake pattern, §1)
- [#8635](https://github.com/bobmatnyc/trusty-tools/issues/8635) compress tool_output test flakes under parallel load
- [#8634](https://github.com/bobmatnyc/trusty-tools/issues/8634) vector_quant test flakes on exact-equality recall assertion
- [#8627](https://github.com/bobmatnyc/trusty-tools/issues/8627) grounding index test flakes with ETXTBSY under parallel load
- [#8581](https://github.com/bobmatnyc/trusty-tools/issues/8581) rate-limit kill leaves unreverted resources (enhancement, not a flake fix)
- [#8538](https://github.com/bobmatnyc/trusty-tools/issues/8538) replace real GitHub logins used as test fixtures
- [#8464](https://github.com/bobmatnyc/trusty-tools/issues/8464) memory_verbs_socket flakes with ENOTCONN — stub-server race
- [#8463](https://github.com/bobmatnyc/trusty-tools/issues/8463) health_catalog stalled-read test hung 36 min at 0% CPU, no timeout
- [#8345](https://github.com/bobmatnyc/trusty-tools/issues/8345) trusty-mpm: 53 integration-test binaries each link the whole
  library — consolidation, not a flake, but CI-minutes relevant
- [#8311](https://github.com/bobmatnyc/trusty-tools/issues/8311) spawn-path/palace-alias writes ignore named root, leaking 42k test
  project dirs
- [#8284](https://github.com/bobmatnyc/trusty-tools/issues/8284) search_index_confirm flakes under CI shard load
- [#8225](https://github.com/bobmatnyc/trusty-tools/issues/8225) parity_doctor flakes under parallel lib tests; env mutation w/o serial
- [#7085](https://github.com/bobmatnyc/trusty-tools/issues/7085) test-spawned `serve --foreground` daemons survive parent kill
- [#5937](https://github.com/bobmatnyc/trusty-tools/issues/5937) spawn_startup_tasks_populates_pin_map hangs indefinitely (binary
  target, invisible to `--lib`/fail-fast)
- [#5914](https://github.com/bobmatnyc/trusty-tools/issues/5914) tm_hook_pm_guard fails non-deterministically under machine load
- [#5328](https://github.com/bobmatnyc/trusty-tools/issues/5328) bounded_python_check races subprocess spawn

## 6. Test density (sample — see Prompt feedback on scope)

Read-only tooling could not run `cargo nextest list`/the project's SLOC
script, so this is `#[test]`/`#[tokio::test]` attribute counts per crate
(src+tests, `.md`/`Cargo.toml` excluded) as a size signal, not a verified
tests-per-1k-line ratio:

| crate | test attrs | open bugs (grep of §5 list) |
|---|---|---|
| trusty-mpm | 12307 | [#8770](https://github.com/bobmatnyc/trusty-tools/issues/8770), [#8765](https://github.com/bobmatnyc/trusty-tools/issues/8765), [#8635](https://github.com/bobmatnyc/trusty-tools/issues/8635) (agents-common not mpm—excl), [#8581](https://github.com/bobmatnyc/trusty-tools/issues/8581), [#8538](https://github.com/bobmatnyc/trusty-tools/issues/8538), [#8464](https://github.com/bobmatnyc/trusty-tools/issues/8464), [#8463](https://github.com/bobmatnyc/trusty-tools/issues/8463), [#8345](https://github.com/bobmatnyc/trusty-tools/issues/8345), [#8311](https://github.com/bobmatnyc/trusty-tools/issues/8311), [#8284](https://github.com/bobmatnyc/trusty-tools/issues/8284), [#8225](https://github.com/bobmatnyc/trusty-tools/issues/8225), [#5914](https://github.com/bobmatnyc/trusty-tools/issues/5914) |
| trusty-agents | 4233 | ([#8635](https://github.com/bobmatnyc/trusty-tools/issues/8635) is trusty-agents-common) |
| trusty-common | 3348 | [#8627](https://github.com/bobmatnyc/trusty-tools/issues/8627) (audit), [#8284](https://github.com/bobmatnyc/trusty-tools/issues/8284) (common) |
| trusty-search | 2770 | [#8634](https://github.com/bobmatnyc/trusty-tools/issues/8634) |
| trusty-review | 2375 | — |
| trusty-code | 2317 | — |
| trusty-git-analytics | 1678 | — |
| trusty-audit | 1180 | [#8627](https://github.com/bobmatnyc/trusty-tools/issues/8627) |
| trusty-memory | 1111 | [#7085](https://github.com/bobmatnyc/trusty-tools/issues/7085), [#5937](https://github.com/bobmatnyc/trusty-tools/issues/5937) |

trusty-mpm's raw test count dwarfs every other crate (consistent with
[#8345](https://github.com/bobmatnyc/trusty-tools/issues/8345) noting 53
integration-test binaries), which is the CI-minutes driver behind the
ratchet in `ci-gates.md`/8-way sharding.

## RANKED FIX LIST (top ~15)

1. **Fix `sweep_with_budget`'s child-kill path not honoring its timeout under
   CI** — crate trusty-audit, file `crates/trusty-audit/src/run.rs`
   (`a_hung_child_is_killed_and_recorded` + the production kill code it
   covers, referenced at lines 334, 887). Payoff: ~600s/shard (~37% of
   shard 5, ~10 min off the CI critical path) + closes a real production
   defect (a hung child audit process may not actually die on schedule).
   Size: M (needs root-causing the signal/kill path on the runner, not just
   shortening the test).
2. **[#8765](https://github.com/bobmatnyc/trusty-tools/issues/8765)**
   `wide_bracket_runs_scan_in_linear_time` 1s wall-clock bound —
   convert to a work-unit/pass-count assertion. trusty-mpm. Payoff: flake
   reduction, already in progress per recent commits on this branch's log.
   Size: S.
3. **[#8463](https://github.com/bobmatnyc/trusty-tools/issues/8463)**
   health_catalog stalled-read test hangs 36min, no timeout —
   trusty-mpm. Payoff: removes a worst-case 36-minute CI stall. Size: S-M
   (add a bounded wait).
4. **[#5937](https://github.com/bobmatnyc/trusty-tools/issues/5937)**
   spawn_startup_tasks_populates_pin_map hangs indefinitely,
   invisible to `--lib`/fail-fast — trusty-memory. Payoff: closes a
   fail-fast blind spot (masks failures in binary-target tests downstream).
   Size: S-M.
5. **[#8464](https://github.com/bobmatnyc/trusty-tools/issues/8464)**
   memory_verbs_socket ENOTCONN stub-server race — trusty-mpm.
   Payoff: flake reduction on a named, reproducible race. Size: S.
6. **[#8225](https://github.com/bobmatnyc/trusty-tools/issues/8225)**
   parity_doctor / memory_unreachable_is_fail — env mutation
   without `#[serial]` — trusty-mpm. Payoff: flake reduction, converts an
   already-diagnosed root cause into a fix. Size: S.
7. **[#7085](https://github.com/bobmatnyc/trusty-tools/issues/7085)**
   test-spawned `serve --foreground` daemons survive parent kill —
   trusty-memory. Payoff: stops daemon leakage across CI runs (resource +
   hermeticity). Size: M.
8. **[#8311](https://github.com/bobmatnyc/trusty-tools/issues/8311)**
   spawn-path/palace-alias writes leak 42k test project dirs —
   trusty-memory/trusty-common. Payoff: disk/hermeticity, large blast
   radius given the leak count. Size: M.
9. **write_liveness_tests.rs `elapsed() < SETTLE`** (§1b,
   `crates/trusty-memory/src/tools/tests/write_liveness_tests.rs:38,144`) —
   convert to condition polling. Payoff: removes 2 timing-flake sites in a
   low-density crate (memory is already the sparsest-tested crate per §6).
   Size: S.
10. **[#8634](https://github.com/bobmatnyc/trusty-tools/issues/8634)**
    vector_quant exact-equality recall assertion — trusty-search.
    Payoff: flake reduction on a quantization test. Size: S.
11. **[#8627](https://github.com/bobmatnyc/trusty-tools/issues/8627)**
    grounding index ETXTBSY under parallel load — trusty-audit.
    Payoff: flake reduction; touches the same crate as fix #1, good to batch.
    Size: S.
12. **[#8284](https://github.com/bobmatnyc/trusty-tools/issues/8284)**
    search_index_confirm deadline flake under shard load —
    trusty-common. Payoff: flake reduction on a shared-library test. Size: S.
13. **prune.rs:592** status-only assertion on the CONTROL branch (§2) — add
    a body/dry-run-echo assertion matching its sibling test. Payoff: closes
    one concrete [#8499](https://github.com/bobmatnyc/trusty-tools/issues/8499)-pattern gap. Size: XS.
14. **event_tickets.rs:176** `bearer_token_opens_event_stream` — assert an
    SSE event is actually delivered, not just the 200 upgrade. Payoff:
    closes another concrete [#8499](https://github.com/bobmatnyc/trusty-tools/issues/8499)-pattern gap in the auth-critical SSE path.
    Size: XS.
15. **[#8345](https://github.com/bobmatnyc/trusty-tools/issues/8345)**
    53 integration-test binaries each linking the whole trusty-mpm
    library — measure, then consolidate. Payoff: largest single lever on
    trusty-mpm's outsized 12307-test / 8-shard footprint, but needs
    measurement first. Size: L (own project, not a batch candidate).

**Proposed batch of 3-5 for one builder PR**: items **2, 5, 6, 9, 13** —
all trusty-mpm/trusty-memory test-only changes (rung 2 on the test ladder:
flake fix / fixture / harness), no production-code risk, each independently
small (S/S/S/S/XS), and 4 of 5 already have an open, diagnosed issue to
close. Item 1 (the 600s audit bug) and item 15
([#8345](https://github.com/bobmatnyc/trusty-tools/issues/8345)) are each
large enough to be their own PR and should not be batched with the rest.

## Prompt feedback

Read-only pm-guard blocks `sort`/`uniq`/`--include=*.rs`/`sed -n <script>`
beyond print, and any `>` redirect — every count above needed hand-rolled
anchored regexes or the trusty-search MCP grep instead, which cost real
turns; a read-only agent brief this data-heavy would benefit from an
allowlisted `sort`/`uniq`/`awk -F` for exactly this kind of aggregation.
Item 6's "tests per 1k non-comment lines" needs the project's own SLOC
counter, which is a `bash scripts/…` invocation the guard refuses outright —
I substituted a raw test-attribute count and flagged the gap; consider
pre-computing that ratio into a checked-in artifact `research` agents can
read instead of shelling out.
