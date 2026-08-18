# trusty-tools source-level code audit — 2026-08-17

Inferred code audit of the `trusty-tools` workspace, run against the repository
itself with its own tooling.

- **Working tree analyzed:** `ee8b078e7` (local `main` at run time)
- **Report branched from:** `3c60ae31a` (`origin/main`)
- **Index build:** `2026-08-17T00:21:36Z`, 5,491 files walked, 0 skipped

Every citation below was verified to resolve at `3c60ae31a`, not only at the
commit the analyzer saw.

## 1. Scope and method

### In scope

The inferred code audit: trusty-search indexes the working tree, trusty-analyze
computes complexity and smells over that index, and the findings are synthesized
from those two feeds.

### Out of scope, by owner instruction

The tga (git-analytics) leg entirely — no commit or contributor statistics, no
ticketing-board data, no JIRA/Linear. **How it was disabled:** the normal driver
for this audit is `tga audit`, which shells out to `trusty-review report`. That
wrapper was never invoked; `trusty-review report` was driven directly instead, so
no git-analytics collector ran. `tga` is also not installed on this machine
(`command -v tga` → not found), so the leg could not have run even by accident.

### Tooling and versions

| Component | Version | Role |
|---|---|---|
| trusty-search daemon | 0.46.0 | corpus index (binary on PATH is 0.45.1 — skewed) |
| trusty-analyze daemon | 0.9.0 | complexity, smells, diagnostics (0.9.1 available) |
| trusty-review | 0.16.0 | report assembly — **failed, see §4** |
| trusty-audit (`taudit`) | 0.1.0 | present, not used |
| tga | not installed | excluded by instruction |

Machine state at run time: load average 6.02 rising to 10.66 on 10 cores, with
two `rustc` processes at ~97% CPU from three other concurrent agents. This
directly shaped one decision, recorded in §2.

## 2. Coverage and confidence

**A green exit proves nothing here.** Both analysis feeds are fail-open by
design, and both fail-opened during this run while exiting 0. The accounting
below is what was affirmatively confirmed populated, by reading raw output.

| Feed | Status | Evidence |
|---|---|---|
| trusty-search lexical + KG | **Populated** | 85,269 chunks over 5,491 files |
| trusty-search semantic (vector) | **Failed** | live vector store holds 0 vectors |
| trusty-analyze complexity | **Populated (on retry)** | 62,754 chunks over 4,033 files |
| trusty-analyze smells | **Aggregate only** | count available; per-item breakdown not |
| Linter diagnostics | **Empty** | `tools_run: []`, `diagnostics: []` |
| trusty-review synthesis | **Failed** | exit 1, LLM provider unreachable |
| tga / git-analytics | **Excluded** | owner instruction |

### What fraction of the repo was actually examined

Complexity was computed over **4,033 files and 62,754 chunks**, split by adapter
as 61,757 Rust, 480 TypeScript, 266 Python, 251 JavaScript. Against 4,052 tracked
`.rs` files, Rust coverage is effectively complete.

**Structural coverage is therefore high; semantic coverage is zero.** Three
distinct capabilities contributed nothing:

- **Vector search returned nothing.** The semantic lane self-reports `failed`:
  the corpus holds 85,093 chunks and an embedder is wired, but the live vector
  store holds 0 vectors — the HNSW snapshot was discarded at warm-boot. To the
  tool's credit it reports this as `failed` rather than `ready`, deliberately, so
  queries stay on the working lexical lane (issue #4707). No similarity-based or
  semantic-duplication finding in this report; none was possible.
- **No linter diagnostics.** See §4.3.
- **No LLM synthesis.** See §4.4.

So this audit sees *structure and complexity*, and does not see *meaning*. It can
say a function is a 118-branch dispatcher; it cannot say two functions duplicate
each other, or that a name misleads.

### Deliberately skipped

A workspace-wide `cargo clippy --workspace --all-targets` would have supplied the
missing linter feed. **It was not run.** At the decision point the 1-minute load
average was **10.66 on 10 cores** with two `rustc` processes already at ~97%,
from three other agents. A full workspace clippy build on 21+ crates under that
contention would have taken tens of minutes and degraded the other agents' work.
This is a stated limit, not a silent one: the linter dimension of this audit is
absent, and §4.3 records that the tool's own linter feed returned empty
independently of this choice.

## 3. Findings about the code

The workspace is in good structural health. Mean cyclomatic complexity is **2.62**
and **90.1% of analyzed chunks grade A**. There is no broad quality problem. What
follows is the thin tail, and it is genuinely thin.

### 3.1 Complexity concentrates in hand-rolled dispatchers

The single strongest signal is a *pattern*, not any one function: nearly every
top-complexity **single function** in the workspace is a manual dispatch table —
a long `match` mapping a string or enum onto a handler. Ranked by cyclomatic
complexity:

| Cyclo | Location | Function |
|---:|---|---|
| 118 | `crates/trusty-common/src/tickets/server.rs:88` | `dispatch` |
| 92 | `crates/trusty-search/src/main.rs:1117` | `run` |
| 90 | `crates/trusty-common/src/symgraph/symbol.rs:143` | `classify` |
| 79 | `crates/trusty-agents/src/inspection/task_signals.rs:49` | `TaskSignals::extract` |
| 69 | `crates/trusty-search/src/core/chunker/classify.rs:36` | `classify_node` |
| 67 | `crates/trusty-agents/src/runtime/startup.rs:81` | `run_startup_init` |
| 65 | `crates/trusty-agents/src/runtime/mode_dispatch.rs:78` | `dispatch_cli_mode` |
| 64 | `crates/trusty-gworkspace/src/server.rs:39` | `handle_tool_call` |
| 61 | `crates/trusty-mpm/src/tui/event_loop.rs:32` | `run_loop` |

`tickets/server.rs:88` is the outlier worth attention: 118 branches in a single
236-line `async fn dispatch`. It is the MCP tool-dispatch surface, so every added
tool widens it, and a dispatcher is exactly the shape where a missed arm is a
silent behavioral gap rather than a compile error.

These are consistent with a codebase that favors explicit matching over
registries, which is a defensible style choice — the SLOC cap keeps the files
themselves small. The finding is that the *branch* count, not the line count, is
what has grown, and the SLOC cap does not measure branches.

### 3.2 The headline "cyclo=140" is an aggregate, not a function

The analyzer's top-ranked hotspot is
`crates/trusty-common/src/memory_core/store/hnsw_store.rs:342-945` at cyclo=140.
Line 342 is `impl HnswStore {` — this is an **impl-block aggregate spanning 603
lines**, not one function. The same is true of the #3 entry
(`crates/trusty-common/src/tickets/api/backends/jira/backend.rs:23`,
`impl Backend for JiraBackend`) and the #14 entry
(`crates/trusty-mpm/src/session_manager/manager.rs:216`, `impl SessionManager`).

Roughly two-thirds of the top-60 hotspot list is impl-block or file-level
aggregates rather than functions. **Read the raw hotspot list with that in mind**
— treating those numbers as per-function complexity would badly overstate the
problem. That is a property of the tool's output, and it is recorded again in
§4.5.

### 3.3 A generated vendor file is committed twice

`docs/design/UI/support.js` and `docs/design/archive/gui-v1/support.js` are
**byte-identical** (md5 `450f2a9297cd55032eb905780de3016b`, 1,841 lines each) and
both are tracked by git. The file's own first line reads
`// GENERATED from dc-runtime/src/*.ts — do not edit.`

Two consequences. It is 1,841 lines of generated code duplicated in the tree, and
it contributes **4 of the top-50 complexity hotspots** — `createRuntime` (cyclo=64)
and `createHelmetManager` (cyclo=62), each counted twice. Excluding generated and
archived assets from the analyzed corpus would measurably sharpen the hotspot
signal at no cost.

### 3.4 The no-`unwrap()`-in-libraries rule is well observed — verified, not assumed

Counting `.unwrap()` outside inline `#[cfg(test)]` modules gives **370 sites**
across all crate sources, concentrated in the binary/daemon crates where
`anyhow` is the convention (`trusty-mpm` 152, `trusty-search` 136).

A naive grep that does not exclude test modules reports **5,000+** — a 13×
overstatement. That inflated figure is what an audit would report by default, and
it would be wrong.

For `trusty-common`, the library crate where the rule binds hardest, there are
**9 sites**, and inspecting each one individually leaves nothing to fix:

- 4 are `Mutex::lock().unwrap()` poison propagation, the standard idiom
  (`embedder_client/stdio.rs:582`, `:882`; `tickets/api/backends/linear/client.rs:62`, `:95`)
- 2 are `node.child(i).unwrap()` inside `for i in (0..node.child_count()).rev()`
  (`symgraph/parser.rs:270`, `:313`) — the index is bounded by `child_count()`
  itself, so these cannot panic. The adjacent line 307 uses `utf8_text(...).unwrap_or("")`
- 1 is a doc comment quoting the pattern, not code (`embedder/test_env.rs:45`)
- 2 remain minor (`error_capture/fingerprint.rs:78`, `symgraph/test_colocation.rs:71`)

**No action recommended.** This is recorded because "9 unwraps in the shared
library" reads like a finding until each is read, and none survives reading.

### 3.5 SLOC cap: clean

`scripts/check_line_cap.sh` exits 0 — 4,052 tracked `.rs` files measured, 4
allowlisted, **0 violations**. This is a verified pass, not an unexamined section.

## 4. Findings about the audit tooling

These are defects in the audit pipeline, not in the code under audit. They are
listed separately because several of them would have caused this report to
understate the repository's problems while exiting 0.

### 4.1 🔴 The complexity feed returns "0 chunks" nondeterministically, with exit 0

The **first** invocation of the audit's primary command returned, at exit 0:

```
Index: trusty-tools | chunks: 0 | avg cyclomatic: 0.00 | %A: 0.0% | smells: 0
Analyzed 0 chunks across 0 files
Top 60 complexity hotspots:
```

The identical command, re-run, returned real data after ~67 seconds:

```
Index: trusty-tools | chunks: 85269 | avg cyclomatic: 2.62 | %A: 90.1% | smells: 46658
Analyzed 62754 chunks across 4033 files
```

An empty hotspot list under the heading `Top 60 complexity hotspots:` is
indistinguishable, to a reader or a script, from "this repository has no
complexity hotspots." **They are opposite claims.** Had the exit code been
trusted, this report would have asserted the workspace was clean.

Isolation: the failure is specific to this index, not global. `trusty-common`,
`trusty-search`, `trusty-analyze`, `claude-mpm` (61,373 chunks) and `APEX`
(77,689 chunks) all returned real data on the first attempt. `trusty-tools` is
the largest index on the daemon at 2.3 GB, and the cold pass takes ~67s — long
enough that a first request appears to return before the corpus is resident.

### 4.2 🔴 The smells endpoint reports `total: 0, truncated: false` on ~1 call in 3

Six identical requests to `GET /indexes/trusty-tools/smells`:

```
run 1: total=43305  returned=20000  truncated=True
run 2: total=0      returned=0      truncated=False
run 3: total=43305  returned=20000  truncated=True
run 4: total=43305  returned=20000  truncated=True
run 5: total=0      returned=0      truncated=False
run 6: total=43305  returned=20000  truncated=True
```

Every response is HTTP 200. The zero responses carry `truncated: false`, which
positively asserts the result is complete — a consumer has no field to check that
would reveal the answer is wrong. This is worse than 4.1: there is no retry
signal, and a caller that fires once records "no smells."

### 4.3 🔴 The linter feed returns empty while reporting success

`GET /indexes/trusty-tools/diagnostics` returns HTTP 200 with:

```json
{"diagnostics": [], "tools_run": [], "total": 0, "truncated": false,
 "tools_unavailable": ["ruff","biome","staticcheck","pmd","rubocop",
                       "phpstan","swiftlint","detekt","clang-tidy","roslyn"]}
```

All 10 tools named unavailable are linters for languages this workspace does not
primarily use. **`clippy` is absent from that list** — `ClippyTool::is_available()`
tests `which::which("cargo")`, and cargo is installed, so clippy was considered
available. Yet `tools_run` is empty.

So on a 4,000-file Rust workspace the diagnostics feed ran zero linters,
reported zero of its available linter's findings, and returned 200 with an
explicit-looking `tools_unavailable` list that implies the *rest* worked. The
`ClippyTool` implementation is a per-file on-demand tool
(`crates/trusty-analyze/src/core/tool_impls/rust.rs:37`) and is never driven
across an index; its own failure path is `tracing::debug!` (line 43), below the
default log level.

### 4.4 The report pipeline cannot run at all without a reachable LLM

`trusty-review report --manifest … --analyze` **exits 1**:

```
Error: inference is required for a due-diligence report, and this run produced none.
    the LLM provider failed: transport error: Bedrock Converse SDK error
    (model=us.anthropic.claude-sonnet-4-6, region=us-west-2): service error
```

`review_health` corroborates: `"inference": "unreachable"`, `"dry_run": true`.
The `--synthesize` flag is documented as "deprecated and ignored (#5454):
synthesis is now unconditional", so there is no supported way to obtain the
deterministic report the `report` subcommand's own help text describes as
"Deterministic only (M1): no LLM synthesis." **This report was therefore
assembled by hand from the verified feeds**, not produced by `trusty-review`.

Separately, the `--analyze` fetch failed inside that run:

```
WARN --analyze: fetch failed; falling back to scan index_id="trusty-tools"
  error=trusty-analyze transport error: GET .../indexes/trusty-tools/diagnostics
```

The adapter's `FETCH_TIMEOUT` is **15 seconds**
(`crates/trusty-review/src/report/analyze_adapter.rs`), and the `/diagnostics`
endpoint on this index is highly variable — one measured run completed in 9.0s,
while three consecutive runs exceeded 120s in total. On a large index this feed
will time out and fail open more often than not.

**Credit where due:** this is the one place the pipeline degraded *loudly and
correctly*. It emitted:

> `--analyze gap: trusty-analyze unreachable — no analysis pass ran for:
> trusty-tools. Those applications are described from the repository scan alone;
> their findings, complexity, and health factors are not assessed, not clean.`

"not assessed, not clean" is exactly the right distinction, and it is the
behavior the other three feeds should adopt.

### 4.5 Two lesser defects

- **`/smells` does not return smells.** All 43,305 records carry
  `match_reason: "enumerate"` and the payload is a plain chunk enumeration —
  21,114 of the records are `.md` files, alongside 3 `.pdf` and 1 `.docx`. The
  aggregate smell count (46,658) comes from a different path and does not
  reconcile with this endpoint's 43,305. **No per-smell breakdown by kind was
  obtainable.** The taxonomy has only four members — `LongFunction` (>50 lines),
  `DeepNesting` (>4), `TooManyParams` (>5), `MissingDocstring`
  (`crates/trusty-analyze/src/types/complexity.rs:120-123`) — and at those
  thresholds `MissingDocstring` alone would plausibly account for most of the
  46,658. **The raw smell count should not be quoted as a defect count.**
- **Hotspot ranks mix functions with impl blocks** without distinguishing them;
  see §3.2. Entries display `(-)` where a function name would be.

Version skew observed: `trusty-search` binary 0.45.1 against daemon 0.46.0;
`trusty-analyze` 0.9.0 with 0.9.1 available. The analyze daemon's `/mcp` route
404s although it exists in the checked-out source.

## 5. What this audit does NOT tell you

- **Nothing about correctness.** No test was run, no build was performed. Every
  finding is structural.
- **Nothing about linting.** The tool's linter feed returned empty (§4.3) and the
  compensating workspace clippy was deliberately skipped under machine load (§2).
  Whether this workspace is clippy-clean is **unknown**, not "fine".
- **Nothing semantic.** The vector lane holds 0 vectors, so no duplicate-logic,
  similarity, or naming findings were possible.
- **Nothing about history, ownership, or process.** The tga leg was excluded by
  instruction.
- **Nothing about security.** No dependency audit, secret scan, or taint analysis
  was part of this pipeline.
- **Nothing about the 14 commits** between the analyzed tree (`ee8b078e7`) and
  this branch's base (`3c60ae31a`). Citations were re-verified against the base;
  the *metrics* were not recomputed.
- **The smell dimension is aggregate-only.** No per-kind or per-file smell finding
  is made, because the data was not retrievable (§4.5).

The single most important caveat: **§4.1, §4.2 and §4.3 each independently
produce an empty result that exits 0 and reads as "clean".** Any future run of
this pipeline that reports few findings should be assumed broken until its raw
output is opened and confirmed populated.
