# `main` CI health — are #5084, #5085 and #5162 one problem or three?

**Date:** 2026-08-07 · **Tracking:** [#5167](https://github.com/bobmatnyc/trusty-tools/issues/5167) ·
**Scope:** diagnosis only, no code change.

`main` went red on three tests inside eight hours. This asks whether they share a
cause. **They do not.** One is a real race that has been fixed, one is a
wall-clock ceiling that loses under contention, and one is a nondeterministic
assertion with no timing or load component at all. The recommendations differ per
test and are ranked at the end.

Two results worth reading even if nothing else here is: the wall-clock test
reproduces crate-scoped on a 16-core host, so `--workspace` on a small runner is
not what breaks it; and its ceiling cannot be widened, because on CI hardware the
contended boots run *longer* than the regression the assertion exists to catch.

## What the evidence rests on

Two facts about how this repo runs CI govern every reading below.

**CI is fail-fast.** `.github/workflows/ci.yml`'s `cargo test --workspace` step
passes no `--no-fail-fast`, so cargo stops at the first failing test binary.
"Test A and test B never failed in the same run" is therefore *not* evidence they
are independent — the run may simply have halted before B ran. Every
co-occurrence claim here is qualified by whether the other test was reached.

**Cargo runs test binaries in series, not in parallel.** The job logs show
strictly sequential `running N tests` / `test result:` blocks. The mechanism
proposed in [#5085](https://github.com/bobmatnyc/trusty-tools/issues/5085) —
"multiple daemon-launching test binaries running concurrently" — does not
describe `cargo test`. Real contention comes from *within* one binary: libtest
runs that binary's tests on `nproc` threads, and `#[serial_test::serial]` only
serializes a test against other `#[serial]` tests, never against the several
hundred non-serial ones sharing the cores.

Runner class: `ubuntu-latest`, public repo, image `ubuntu24/20260720.247` — 4
vCPU. The local host is 16-core, a 4× ratio.

One detail that matters for reading any of the numbers below: `tests_4846` is
**not** in `trusty-search`'s lib test binary. It sits under `src/commands/`,
which only the `trusty-search` **bin** target (`src/main.rs`) compiles, so it
runs in that binary's 336-test population (375 locally) and never in the
1517-test lib binary. A `--lib`-scoped rerun of `trusty-search` executes zero of
these tests and reports green.

Binary order in `cargo test --workspace` is alphabetical by package, so
`trusty-common` → `trusty-memory` → `trusty-search`. That fixes what a fail-fast
halt can and cannot hide:

| First failure in | `l2_ranks…` | `a_dead_child…` | `tests_4846::*` |
|---|---|---|---|
| `trusty-common` | FAILED | not reached | not reached |
| `trusty-memory` | observed | FAILED | not reached |
| `trusty-search` | observed | observed | FAILED |

## Run → failing-test correlation table

Every `Test`-job failure involving any of the four tests, all branches,
2026-07-31 → 2026-08-07. Each cell is read from that run's own log, not
inferred. `—` means the binary was never reached because cargo had already
stopped.

| Created (UTC) | Run | SHA | Trigger | `l2_ranks…` | `a_dead_child…` | `disabled_salvage…` | `dead_entries…` |
|---|---|---|---|---|---|---|---|
| 08-07 03:15 | [31143819405](https://github.com/bobmatnyc/trusty-tools/actions/runs/31143819405) | `b8746f59d` | push main | pass | n/a¹ | pass | **FAIL** |
| 08-07 04:05 | [31146334273](https://github.com/bobmatnyc/trusty-tools/actions/runs/31146334273) | `b17e5c163` | PR | pass | **FAIL** | — | — |
| 08-07 04:21 | [31147167085](https://github.com/bobmatnyc/trusty-tools/actions/runs/31147167085) | `873e2c156` | PR | pass | n/a¹ | **FAIL** | pass |
| 08-07 12:47 | [31179690320](https://github.com/bobmatnyc/trusty-tools/actions/runs/31179690320) | `ea92679cd` | PR | pass | **FAIL** | — | — |
| 08-07 13:00 | [31180629217](https://github.com/bobmatnyc/trusty-tools/actions/runs/31180629217) | `bf292f4c3` | push main | pass | **FAIL** | — | — |
| 08-07 15:43 | [31194059352](https://github.com/bobmatnyc/trusty-tools/actions/runs/31194059352) | `7e1298bf2` | push main | pass | pass | **FAIL** | pass |
| 08-07 15:50 | [31194655579](https://github.com/bobmatnyc/trusty-tools/actions/runs/31194655579) | `ca9e488f8` | PR | pass | **FAIL** | — | — |
| 08-07 16:05 | [31195841407](https://github.com/bobmatnyc/trusty-tools/actions/runs/31195841407) | `2e735a8d2` | PR | pass | **FAIL** | — | — |
| 08-07 16:12 | [31196453141](https://github.com/bobmatnyc/trusty-tools/actions/runs/31196453141) | `5ef8f38dc` | PR | pass | **FAIL** | — | — |
| 08-07 16:16 | [31196804849](https://github.com/bobmatnyc/trusty-tools/actions/runs/31196804849) | `d60d86d56` | push main | pass | pass | **FAIL** | pass |
| 08-07 16:35 | [31198275808](https://github.com/bobmatnyc/trusty-tools/actions/runs/31198275808) | `972f3ca1c` | push main | pass | pass | **FAIL** | pass |
| 08-07 16:35 | [31198338772](https://github.com/bobmatnyc/trusty-tools/actions/runs/31198338772) | `4c57d6ebb` | PR | pass | **FAIL** | — | — |
| 08-07 16:58 | [31200118643](https://github.com/bobmatnyc/trusty-tools/actions/runs/31200118643) | `361cd9127` | push main | pass | **FAIL** | — | — |
| 08-07 17:20 | [31201832245](https://github.com/bobmatnyc/trusty-tools/actions/runs/31201832245) | `0793ede7c` | push main | pass | **FAIL** | — | — |
| 08-07 17:20 | [31201846759](https://github.com/bobmatnyc/trusty-tools/actions/runs/31201846759) | `fe26a66b9` | push main | pass | **FAIL** | — | — |
| 08-07 17:48 | [31203993604](https://github.com/bobmatnyc/trusty-tools/actions/runs/31203993604) | `d3cbc9bd4` | PR | pass | **FAIL** | — | — |
| 08-07 17:48 | [31204048152](https://github.com/bobmatnyc/trusty-tools/actions/runs/31204048152) | `edc9f354e` | push main | pass | **FAIL** | — | — |
| **08-07 18:29 — [PR #5119](https://github.com/bobmatnyc/trusty-tools/pull/5119) merges `09405a37e`** | | | | | | | |
| 08-07 18:30 | [31207259771](https://github.com/bobmatnyc/trusty-tools/actions/runs/31207259771) | `b2290ff47` | push main | pass | pass | pass | pass |
| 08-07 18:31 | [31207354637](https://github.com/bobmatnyc/trusty-tools/actions/runs/31207354637) | `c69257bf8` | push main | pass | pass | pass | pass |
| 08-07 18:42 | [31208168393](https://github.com/bobmatnyc/trusty-tools/actions/runs/31208168393) | `fc195579f` | push main | pass | pass | pass | pass |
| 08-07 18:56 | [31209227263](https://github.com/bobmatnyc/trusty-tools/actions/runs/31209227263) | `1ec9c30b6` | push main | pass | pass | pass | pass |
| 08-07 19:14 | [31210634597](https://github.com/bobmatnyc/trusty-tools/actions/runs/31210634597) | `89dfae7f8` | push main | pass | pass | pass | pass |
| 08-07 19:15 | [31210670641](https://github.com/bobmatnyc/trusty-tools/actions/runs/31210670641) | `4a1ed8dfe` | push main | pass | pass | pass | pass |
| 08-07 19:16 | [31210758107](https://github.com/bobmatnyc/trusty-tools/actions/runs/31210758107) | `ddfdef513` | PR | **FAIL** | — | — | — |
| 08-07 19:29 | [31211730995](https://github.com/bobmatnyc/trusty-tools/actions/runs/31211730995) | `5aa67e829` | push main | pass | pass | pass | pass |
| 08-07 19:31 | [31211869265](https://github.com/bobmatnyc/trusty-tools/actions/runs/31211869265) | `c8c4b8124` | push main | pass | pass | pass | pass |
| 08-07 19:39 | [31212434407](https://github.com/bobmatnyc/trusty-tools/actions/runs/31212434407) | `1b61c6125` | PR | pass | pass | pass | pass |

¹ `a_dead_child_is_evicted_and_respawned` did not exist yet on that head. It was
introduced by [#5048](https://github.com/bobmatnyc/trusty-tools/issues/5048),
merged as `3c2d787f7` at 08-07 11:56, and first failed at 04:05 on the branch
that introduced it.

Two earlier `tests_4846` failures not in the table because their logs predate
the window sampled in full — annotation evidence only —
[31029967393](https://github.com/bobmatnyc/trusty-tools/actions/runs/31029967393)
and
[31069995917](https://github.com/bobmatnyc/trusty-tools/actions/runs/31069995917)
each failed on **both** `disabled_salvage…` and `dead_entries…`. Those two are
siblings in one file and they do co-occur.

Not an evidence row: PR
[#5160](https://github.com/bobmatnyc/trusty-tools/pull/5160)'s green `Test`
check ([31211936293](https://github.com/bobmatnyc/trusty-tools/actions/runs/31211936293)).
It touches `CLAUDE.md` only, so `changes.docs_only` was true and the
`cargo test --workspace` step reports `skipped` — the job finished in 12 seconds
and ran no tests. PR #5164's check
([31212434407](https://github.com/bobmatnyc/trusty-tools/actions/runs/31212434407),
17 minutes, step `success`) did run, and is in the table.

### What the table says

Across 18 failures the three tracked tests **never once failed in the same
run**, and in 16 of the 18 the non-failing ones are *observed* passing rather
than merely unreached. `l2_ranks…` passed in all 17 runs where the other two
were the cause; `a_dead_child…` passed in all three runs where `tests_4846` was
the cause.

## Failure rates

Denominator: `Test` jobs whose `cargo test --workspace` step actually executed,
2026-08-07 03:55 → 18:29 (the window in which all three tests existed) — 80 runs,
61 green, 19 red.

| Test | Failures | Denominator | Rate |
|---|---|---|---|
| `a_dead_child_is_evicted_and_respawned` | 12 | 80 | **15%** |
| `tests_4846::disabled_salvage…` | 4 | 65² | **6%** |
| `tests_4846::dead_entries…` | 1 | 65² | ~1.5% |
| `l2_ranks…` | 0 in window; 1 ever | 80 | **≈0.3%**³ |

² 80 minus the 15 runs that halted in an earlier crate, so `trusty-search` never ran.
³ One failure since the test landed on 2026-08-05 (`4237ef543`, PR #4970), against
several hundred executed `Test` jobs.

## Is there a load signal?

Three independent load proxies, measured inside the same jobs.

| Proxy | Passing runs | Failing runs | Separates? |
|---|---|---|---|
| `cargo test --workspace --no-run` build-phase duration | median 456 s (n=65) | median 455 s (n=17) | no |
| `trusty-common` lib binary, 1810 tests | 2.92–3.21 s (n=8) | 2.62–3.29 s (n=10) | no |
| **`trusty-search` bin binary, 336 tests** | **0.62–0.66 s (n=8)** | **0.74, 0.75, 0.77, 1.63 s (n=4)** | **yes, disjoint** |
| **`bm25_supervisor_concurrency` binary** | **0.44–0.57 s (n=9)** | **0.18, 0.19, 0.19, 0.19, 0.50 s (n=5)** | **yes, inverted** |

Whole-job proxies are flat: a failing run is not a slower run overall. The signal
is local to the binary, and it points in *opposite directions* for the two tests.

`trusty-search`'s binary took 15–20% longer (once 2.5×) in every run where
`tests_4846` failed, ranges disjoint from every passing run. That is contention,
and the panic messages agree: the measured `one_walk` baseline in the failures is
14.2–16.6 ms while `boot` came in at 416–435 ms against a 250 ms ceiling —
a boot inflated ~28× over its own per-walk unit cost on a 4-vCPU runner.

The `bm25` binary went the other way: in four of five failures it finished in
**0.18–0.19 s versus 0.44–0.57 s when it passes**. A test that fails 2.5× faster
than it passes did not lose a race to load — it took a shortcut. That is the
signature of `ensure_running`'s fast path returning a dead-but-unreaped child
instead of doing the eviction and respawn work, which is exactly what #5085
suspected and what #5119 fixed.

## Local reproduction

`tests_4846` reproduces locally, crate-scoped, on a 16-core host with no
artificial load. This corrects the standing assumption — in the brief and in
#5084 — that it only fails under `--workspace` on an undersized runner.

```
cargo test -p trusty-search --bin trusty-search        # 20 runs, 1 failure

thread 'commands::start::tests_4846::dead_entries_do_not_consume_the_live_index_budget'
  panicked at crates/trusty-search/src/commands/start/tests_4846.rs:142:5:
warm boot took 369.820125ms with 24 dead entries, but ONE relocation walk costs
53.806583ms — the walk is being repeated per dead entry instead of shared across
the boot (issue #4846). Ceiling was 322.839498ms.
test result: FAILED. 371 passed; 1 failed; 3 ignored; 0 measured; 0 filtered out; finished in 3.87s
```

The binary took **3.87 s** on the failing run against 0.78–2.05 s on the other
19. Two batches of ten: the first, on a busier machine, ran 0.83–3.87 s and
produced the failure; the second, quiet, ran 0.78–0.80 s every time and produced
none. Contention within the one binary is sufficient; neither `--workspace` nor
4 vCPU is required.

`trusty-memory`'s supervisor test is clean over the same kind of sweep —
`cargo test -p trusty-memory --test bm25_supervisor_concurrency`, 10 runs,
`6 passed; 0 failed` every time, 0.91–1.58 s. Six, not three: #5119's regression
tests are in the tree.

`l2_ranks…` was not reproduced locally and, at roughly one failure in several
hundred CI runs, a ten-run sweep would not be expected to catch it.

## Per-test verdict

### [#5085](https://github.com/bobmatnyc/trusty-tools/issues/5085) `a_dead_child_is_evicted_and_respawned` — independent defect, real, fixed

A genuine fast-path/eviction race in `bm25_supervisor`, not contention. The
inverted duration signature above is the direct evidence: the failing path is the
*cheap* path. The 15% rate and the fact that it appeared on the very PR that
introduced the test, before any of the load conditions changed, fit a race that
was always there and only became visible when a test started looking for it.

**Is it genuinely fixed?** Stronger than "did not recur once", weaker than
settled. Since `09405a37e`:

- 16 `Test` jobs executed `cargo test --workspace`; 14 of them reached the
  `trusty-memory` binary (two halted earlier, on `l2_ranks…` and on a
  `trusty-agents` test). All 14 green.
- In the 8 whose logs were read, the binary now reports **6 tests, not 3** —
  #5119 shipped three regression tests alongside the fix, and all six pass.
- At the pre-fix rate of 15%, 14 consecutive clean runs would happen by chance
  about 10% of the time. Suggestive, not conclusive. About 20 clean runs pushes
  that below 5%.

Verdict: fixed, on the mechanism evidence plus 14 clean runs. Re-check the count
after another day of merges before closing the book.

### [#5084](https://github.com/bobmatnyc/trusty-tools/issues/5084) `disabled_salvage_budget…` — contention-sensitive, confirmed

Confirms #5084's conclusion and corrects its scope. Reused from it: the
`trusty-common` 0.29→0.30 reachability argument and the success-path visibility
defect. Two things are new. The load claim is now a same-run measurement rather
than an inference — the owning binary is slower in every failing run, ranges
disjoint from every passing one. And #5084's 40/40 local passes do not
generalise: 20 crate-scoped runs here produced a failure, on 16 cores, with no
`--workspace` and no imposed load.

The sibling `dead_entries_do_not_consume_the_live_index_budget` in the same file
shares the mechanism, fails the same way (5 occurrences), and co-occurred with
`disabled_salvage…` twice. **They are one defect with two symptoms and should be
fixed together** — #5084's title names only one of them.

The assertion's own shape is the problem, and it is worse than a margin that
needs widening. `ceiling = max(one_walk * 2, 250ms)` uses wall clock to prove a
claim about *work done* — "one stat per dead entry, no relocation walk" — and on
this hardware **contention inflates `boot` by more than the bug the assertion
exists to catch does**:

| Quantity | On a CI runner |
|---|---|
| `one_walk`, measured in-run | 14.0–16.6 ms |
| Pre-fix cost — the regression it must catch (`DEAD_ENTRIES` = 24 walks) | ≈ 340–400 ms |
| Observed contended boot on a healthy tree | 416–435 ms; once 833 ms |

The false-red band sits *above* the true-regression band. No fixed ceiling can
separate them. Nor does scaling against a measured baseline: `dead_entries…`
already does exactly that (`max(one_walk * 6, 250ms)`) and still failed, in CI at
50× its own `one_walk` and locally at 6.9×, because `one_walk` is sampled once at
the start of the test and the load during `boot` is not the load during that
sample.

### [#5162](https://github.com/bobmatnyc/trusty-tools/issues/5162) `l2_ranks_similar_low_importance_above_less_similar_high_importance` — undetermined, but not contention

Not load-sensitive, on the evidence available without touching the ranking logic
(that is the concurrent investigation's lane, and this stops at its boundary):

- The test body has no `sleep`, no `Instant`, no spawned thread, no child
  process, and no wall-clock assertion. There is no quantity for load to
  perturb.
- `pad_index` is deterministic given the query vector — its 24 filler vectors sit
  at cosine 0.60 down to 0.14, all below both drawers under test.
- `trusty-common`'s binary runs first, so its runtime is the cleanest load
  measurement available, and it is identical in the failing run (3.00 s) and in
  passing runs (2.92–3.21 s).
- One failure, ever, against several hundred executed jobs — two orders of
  magnitude rarer than the two tests that *are* contention-sensitive, on the same
  runners.

The failing assertion is the **second** one, on `results[1]`, not the first on
`results[0]`. The ranking claim the test exists to prove held; what did not hold
is the exact identity of the runner-up in a 26-vector top-5. Root cause is
deferred to the #5162 investigation.

One adjacent data point worth its attention:
`memory_core::retrieval::tier_c_tests::tier_c_write_without_expiry_gets_the_default_ttl`
failed once in the same module on 08-07 05:10
([31149734441](https://github.com/bobmatnyc/trusty-tools/actions/runs/31149734441)).
Two distinct one-off failures in `memory_core::retrieval::*` inside 14 hours may
be one nondeterminism source rather than two.

## Does the unifying hypothesis hold?

**Refuted as stated.** It is not one systemic problem. It holds for one of the
three, and the evidence for #5085 actively contradicts it.

| Prediction of the hypothesis | Observed |
|---|---|
| The three fail together more often than chance | **Zero** co-occurrence in 18 failures; in 16 the others are observed passing, not merely unreached |
| Failures correlate with slower jobs / resource pressure | Whole-job duration and build-phase duration are flat |
| Each passes reliably when its crate is run scoped | **False for #5084** — 1 failure in 20 crate-scoped local runs, 16 cores, no `--workspace`; #5085 and #5162 did not reproduce |

The one prediction that survived — a local load signal — survived for #5084 only,
and inverted for #5085.

The scoped-rerun prediction failed in the direction that matters. `--workspace`
parallelism on an undersized runner is not what breaks #5084; ~370 sibling tests
in its own binary are enough, on any machine. Fixing the CI configuration would
not have fixed it.

The tempting version of the story was true at the level of *setting*: three
timing-adjacent tests, one 4-vCPU runner, one `--workspace` invocation. It was
false at the level of *cause*. What actually unifies them is thinner and less
useful: this workspace has grown tests whose assertions depend on scheduling, and
a 4-vCPU runner surfaces each of them in its own way.

## Recommendations, ranked by false reds removed per unit of coverage lost

None of these removes coverage. Ranked by effect on false reds.

**1. Assert the invariant, not the clock, in `tests_4846` (fixes #5084 and its
sibling).** Highest yield: 5 of 19 red runs in the sample window, and the only
one of the three that is expected to keep firing. The test's real claim is "one
stat per dead entry, zero relocation walks" — a count, not a duration. Counting
walk invocations (a counter on the salvage path, or an injected probe) makes the
assertion exact and load-immune. Cover both tests in the file; they share the
mechanism. #5084 flags this as most robust and most work and leaves the choice
open; the numbers here close it — the two cheaper options are ruled out in
recommendation 3, so this is the only one that keeps the coverage.

**2. Make the margin visible on the success path (#5084 part 1).** Print `boot`,
`one_walk` and the computed ceiling on pass, not only on panic. Cheap,
independent of recommendation 1, and directly serves "loudly unreliable rather
than quietly retried": today a run at 249 ms and a run at 100 ms are
indistinguishable in the log, so nobody can see the margin eroding. If
recommendation 1 lands, keep this anyway as an observability line.

**3. Do not raise the ceiling, and do not scale it against a baseline.** Both
options #5084 lists as cheaper alternatives are unsafe here, on the numbers in
the #5084 verdict above. A fixed floor high enough to clear the observed
contended boots (416–435 ms, once 833 ms) is also high enough to clear the
pre-fix regression (≈340–400 ms), so the test would go quiet on the exact bug it
was written for — green, and worthless. Baseline scaling is already implemented
in `dead_entries…` and failed anyway. Recording this as ruled out matters more
than it looks: it is the option a reader reaches for first.

**4. Confirm #5085 rather than close it on 14 runs.** Zero cost: re-read the
`a_dead_child` result across the next day's merges. Twenty clean runs takes the
chance-of-coincidence below 5%. If it recurs, the fast-path evidence in this
document narrows where to look immediately.

**5. Leave #5162 to its own investigation, and give it the `tier_c` data point.**
Nothing in the CI history distinguishes its one failing run from the passing
ones, so there is nothing here to act on. A one-in-several-hundred assertion is
worth fixing properly rather than papering over — and at that rate it is not
what is making `main` red.

**Not recommended: `--no-fail-fast`.** It would complete the correlation table
for free, but it also turns every red run into a full-workspace run and would
have added roughly 15 truncated runs' worth of runner time in one day. The
ordering table above recovers most of the same information at zero cost.

**Explicitly not recommended, and not close:** `#[ignore]`, `cfg`-gating,
`--exclude`, `--lib`-narrowing, or an automatic retry on any of these three. A
retry-until-green would have hidden #5085 — a real fail-open race in which the
supervisor believes it has a live child and does not — for as long as it kept
firing at 15%.

## What could not be established, and what would settle it

- **Whether #5085's fix is proven.** 14 clean runs at a 15% prior leaves ~10%
  chance of coincidence. Twenty clean post-`09405a37e` runs settles it; a
  deterministic injection of the unreaped-child state, as #5085 asks for, settles
  it properly.
- **Whether `a_dead_child` and `tests_4846` can fail in the same run.** Cargo's
  fail-fast makes this unobservable in the current configuration, and
  `--no-fail-fast` is not worth its cost. It does not change any verdict here:
  the reverse direction *is* observed, three times, with `a_dead_child` passing.
- **#5162's root cause.** Out of scope by construction — owned by the concurrent
  #5162 investigation. This document establishes only that it is intermittent, is
  not load-correlated, and does not share a cause with the other two.
- **#5084's true rate.** 1 in 20 locally and 5 in 65 in CI are both small
  samples, and the local rate is clearly a function of what else the machine was
  doing. Nothing here establishes a rate that could be tracked as a regression
  metric. Recommendation 1 removes the need for one.
- **Whether other wall-clock or scheduling-dependent assertions are queued behind
  these.** The same sweep surfaced `bounded_python_check_classifies_timeout_apart_from_failure`
  (`trusty-embedderd-py`, 5 failures) and
  `isolated_managed_state_guard_panics_on_production_root` (3) at comparable
  rates. Neither was in scope. An inventory of wall-clock assertions across the
  workspace would say how large the class is.

## Method

- CI history: `gh run list --workflow ci.yml --limit 1000` (2026-07-31 → 2026-08-07,
  1000 runs, 60 failures), then per-run `Test`-job failure annotations via
  `repos/…/check-runs/<id>/annotations` — which carry the per-crate attribution
  `.github/scripts/attribute_test_failures.py` emits, so a full log download is
  needed only where per-test pass/fail inside a run is the question.
- Per-run pass/fail for the four tracked tests: full `Test`-job logs for all 19
  failing runs in the table plus 8 passing controls, matched on the exact
  `test <name> ... ok` / `... FAILED` lines.
- Durations: job and step timings from the Actions jobs API; per-binary
  durations from each run's own `test result: … finished in Xs` lines.
- Local: worktree off `origin/main` at `2a2b6cfb8`, macOS 16-core.
  `cargo test -p trusty-search --bin trusty-search` ×20 (1 failure),
  `cargo test -p trusty-memory --test bm25_supervisor_concurrency` ×10 (0).
  `--bin`, not `--lib`, because `--lib` filters all 375 of these tests out and
  reports `0 passed` green.
- Reused from [#5084](https://github.com/bobmatnyc/trusty-tools/issues/5084)
  without repeating: the 40/40 local pass runs at both SHAs, the
  `trusty-common` 0.29→0.30 reachability argument, and the success-path
  visibility defect. Reused from
  [#5085](https://github.com/bobmatnyc/trusty-tools/issues/5085): the byte-level
  proof that PR #5074 did not cause it, and the 62-run local negative. Its
  proposed contention mechanism is corrected above.
