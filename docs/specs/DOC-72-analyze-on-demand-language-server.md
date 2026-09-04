---
spec_refs: []
---

# DOC-72 — trusty-analyze On-Demand Language-Server Capability

**Status:** Draft. Design record only — no code in this PR.
**Spec ID:** `SPEC-ANALYZELSP-01~draft` … `SPEC-ANALYZELSP-11~draft`
**Subsystem:** `trusty-analyze` — the language-server supervisor, the versioned
result store, and the new socket/MCP methods (`src/service/rpc.rs`,
`src/core/tool_registry.rs`, `src/lang/`); `trusty-common` — the on-demand entry
point clients start the daemon through (`src/uds/on_demand.rs`);
`trusty-mpm` — the bundled `analyze` skill (`src/assets/skills/`);
`trusty-agents-common` — the coding-agent instruction layer
(`src/assets/agents/BASE-ENGINEER.md`); `trusty-review` — the audit path's only
client of trusty-analyze (`src/report/analyze_adapter.rs`).
**Owner:** Bob Matsuoka
**Last-updated:** 2026-09-01
**DOC-N claim:** `DOC-72`, scan-before-claim per DOC-38 §4.1.
`docs/specs/README.md`'s catalog note names DOC-72 as the next free number after
DOC-71; a repo-wide grep for `DOC-72` returned only that note;
`scripts/check_doc_numbers.sh` reported 132 docs / 126 claims, 3 grandfathered,
0 violations before this file; one open pull request exists and it claims no
`DOC-N`.
**Builds on:** the owner ruling of 2026-09-01 on
[#6589](https://github.com/bobmatnyc/trusty-tools/issues/6589) (§0); the
on-demand/idle-exit process model landed by
[#6350](https://github.com/bobmatnyc/trusty-tools/issues/6350)
(`trusty_common::uds::OnDemandAnalyze`, `trusty-analyze`'s `serve_with_idle`);
[ADR-0032](../adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)
(no sibling daemon owns an HTTP surface; UDS is the inter-service transport),
which supersedes [ADR-0018](../adr/0018-loopback-only-doctrine.md);
[ADR-0005](../adr/0005-harness-event-bus.md) and
[ADR-0019](../adr/0019-unified-ipc-messaging-on-event-bus.md) for the control bus.
**Cross-ref:** issues
[#6606](https://github.com/bobmatnyc/trusty-tools/issues/6606) (this spec),
[#6589](https://github.com/bobmatnyc/trusty-tools/issues/6589) (the A/B trials),
[#6287](https://github.com/bobmatnyc/trusty-tools/issues/6287) (the removal of
analyze's HTTP surface and its event channel),
[#6018](https://github.com/bobmatnyc/trusty-tools/issues/6018) (the diagnostics
deadline budget); [DOC-67](./DOC-67-tga-audit-mode.md) §5 (the audit sweep and
its three seams); [DOC-68](./DOC-68-audit-handoff-package.md) (the auditor
client).

---

## 0. Owner ruling

> "So maybe we don't use Claude code's plugin. It becomes part of analyze and is
> used judiciously."
>
> — Bob, 2026-09-01, on #6589

> "Let's build it into analyzer as we discussed. It should be a tool available
> when needed. Let's include it in the analyze skill and instruction the base
> coding agent to use it for architecture, refactoring, performance analysis. It
> should also be part of the audit toolkit."
>
> — Bob, 2026-09-01, scope expansion

> "1. Configurable option, on my default (linters too); 2. Node or Bun,
> recommend installing in scaffolding. 3. Study more, do intentional analysis.
> 4. Relay. 5. Recommend. 6. Cap per server 7. Experiment."
>
> — Bob, 2026-09-01, answering §10's seven questions in order

§10 records each answer against the question it settles. The rulings are folded
into §2, §4, §7, §8, §9, and §11, so no section still describes an open choice
the owner has made.

---

## Why

Two A/B trials measured the Claude Code language-server plugin on this repo, and
neither found a benefit.

- **Navigation workload.** Six tasks, three runs, two arms. The model invoked the
  LSP tool **0 times in 18 LSP-on runs**. Correctness was 15/18 with the plugin
  and 14/18 without, inside the run-to-run spread.
- **Rust edit workload.** Three tasks, one rep, two arms. **0 invocations in 6
  runs.** Average wall time was **142 s with the plugin on and 84 s with it
  off** — the plugin cost about 40% and returned nothing.

Two mechanisms explain the result.

- The plugin spawns its own `rust-analyzer` per Claude session. Indexing
  therefore never amortizes: every session pays the cold-start cost and the
  session ends before the index is warm.
- Navigation is already served. The `trusty-search` AST/graph index answers
  definitions, references, and call chains today (`search_kg`,
  `get_call_chain`), so the plugin was offering a slower second path to answers
  the agent could already get.

What a language server uniquely supplies is **type-checked semantics**: an
answer that required resolving types, traits, overloads, and imports. That is
worth having. It is not worth having resident.

This spec therefore moves the capability into `trusty-analyze`, where the
process is one per workspace rather than one per session, is started on demand,
exits when idle, and is called only when a caller has a question the AST index
cannot answer.

---

## {#SPEC-ANALYZELSP-01~draft} 1. Scope

**What the language server adds over the AST index.**

- Type-checked diagnostics — errors a parser cannot see because they need a type
  or a trait resolved.
- Overload, trait-impl, and import resolution — which `foo` this call site
  actually binds to.
- Signature-change impact — every call site a changed signature breaks.
- Type-at-position — the concrete inferred type of an expression.

**What it does not replace.**

- **The batch checkers stay the merge-chain gate.** `cargo check`, `pyright`,
  and `tsc --noEmit` remain the verdict a merge waits on. A language server
  reports what its incremental view believes; a merge needs a full compile. The
  LSP never becomes the gate.
- **Navigation stays on `trusty-search`.** Plain definitions, references, and
  call chains route to the AST/graph index. The LSP is asked only when the AST
  answer is ambiguous — an overload set, a trait dispatch, a re-export.
- **The existing `analyze.diagnostics` method is unchanged.** It dispatches
  external linters (`clippy`, `ruff`, `biome`) per index and keeps its own
  deadline budget (#6018). The LSP methods are additive and separately named
  (§5).

**The gap this closes, concretely.** Today `trusty-analyze` runs `ruff` for
Python (`src/core/tool_impls/python.rs`) and `biome` for TypeScript
(`src/core/tool_impls/typescript.rs`). Both are linters. Neither type-checks. A
Python or TypeScript repository analysed by this daemon gets no type verdict at
all, while Rust gets one through `cargo clippy`, which compiles.

**No Claude Code plugin, anywhere.** `tm` installs and provisions no Claude Code
language-server plugin, and none is an opt-in path to this capability. The three
plugins the #6589 trials used — `rust-analyzer-lsp`, `pyright-lsp`,
`typescript-lsp` — were uninstalled on 2026-09-02. Every mention of a plugin in
this document is historical: the A/B trials measured one, and that measurement is
why the capability moved here (§0, §Why, §7). The capability ships as an analyze
function reached the two ways §5 fixes.

**What the measured evidence covers.** §7's numbers come from trials where the
language server was available through the Claude Code plugin and the model was
never told to call it — 0 of 30 explicit invocations. They size the cost of
availability. They say nothing about the value of the deliberate calls this
section specifies. Phase 0 measures that (§9).

---

## {#SPEC-ANALYZELSP-02~draft} 2. Lifecycle

One language-server process per workspace per language.

- The server is spawned **on demand**, by the first call that needs it, inside
  the process model #6350 already established. `trusty-analyze` itself is
  started by `trusty_common::uds::OnDemandAnalyze::ensure_running`, and exits on
  its own idle window through `serve_with_idle`. The language server nests
  inside that: it is supervised by the analyze daemon, not by a client.
- The supervisor is `trusty_common::uds::supervisor::UdsServiceSupervisor`, keyed
  by `(workspace_root, language)`. Reuse the existing supervisor; do not write a
  second process manager (root `CLAUDE.md`, common-entry-point rule).
- **Idle timeout.** A language server exits after its own idle window, default
  10 minutes, configurable per language. Its window must be shorter than or
  equal to the analyze daemon's, so the daemon never idles out holding a live
  child.
- **Never auto-started by session provisioning.** No `tm` session launch, no
  Claude session start, and no agent dispatch spawns a language server. Only a
  call from §6's trigger list does. This is the direct expression of the owner
  ruling.
- **Failure never fails open.** A missing binary, a refused spawn, or a server
  that never answers reaches the caller as a typed error, matching
  `on_demand.rs`'s existing contract. A caller that wants to degrade decides that
  visibly; nothing decides it silently on the caller's behalf.

**Binary provisioning.** `trusty-installer` installs the language servers **and
the existing linters** — `ruff` and `biome` — as a configurable option that is
**ON by default** (owner ruling, §10 Q1). With the option off the installer
installs nothing and the daemon falls back to today's behaviour: probe each
binary, record the unavailable ones, and report them, exactly as
`tool_registry.rs` does now. Provisioning changes who installs the tools; it
never changes the daemon's probe-and-report contract.

**Config key shape.** The option lives in the installer's own config, under the
`~/.trusty-tools/<crate>/config.yaml` convention `trusty_common::crate_config`
fixes (#1220 — YAML, not TOML):

```yaml
# ~/.trusty-tools/trusty-installer/config.yaml
analysis_tools:
  install: true                    # default ON (§10 Q1)
  language_servers: [rust-analyzer, pyright, typescript-language-server]
  linters: [ruff, biome]
  js_runtime: auto                 # auto | bun | node | none
```

**Installer scaffolding step.** The tools join the data-driven `PREREQS` table in
`crates/trusty-installer/src/commands/prereqs/`, and run through the
check-confirm-offer-install-reverify orchestration that `prereqs/phase.rs`
already implements, with `prereqs/hints.rs` supplying the per-platform install
command and `prereqs/exec_heartbeat.rs` keeping a long install visible (#3821).
No second installer path — this is one more table entry per tool, gated on
`analysis_tools.install`.

**JavaScript runtime.** `pyright` and `typescript-language-server` are npm
packages and need a JavaScript runtime. Either Node or Bun serves (owner ruling,
§10 Q2), and the scaffolding installs one, so **phases 3 and 4 are not gated on
the machine already having a runtime**.

Selection rule, in order:

1. If Bun is present, use Bun. It starts faster, and these servers are spawned on
   demand rather than kept resident, so start-up cost is paid on every use.
2. Otherwise, if Node is present, use Node.
3. If neither is present, **install Node**. Both servers are published and tested
   against Node, so a fresh install should land on the runtime their maintainers
   support rather than one that emulates it. `js_runtime` overrides the rule when
   an operator wants a specific runtime.

| Language | Server | Provisioning note |
|---|---|---|
| Rust | `rust-analyzer` | Install **per pinned toolchain**: `rustup component add rust-analyzer --toolchain <pinned>`. A `mise` shim resolves `rust-analyzer` to `rustup run <toolchain>`; with `RUSTUP_TOOLCHAIN` pinned and the component present only on `stable`, the server exits 1 with `infinite recursion detected`. Installing without `--toolchain` looks correct from `$HOME` and stays broken inside the repository. This is the crash the #6589 navigation A/B found, and Claude Code swallowed it as `outcome=ok`. |
| Python | `pyright` | npm package; needs a Node runtime on the machine. |
| TypeScript | `typescript-language-server` | npm package; needs a Node runtime on the machine. |

---

## {#SPEC-ANALYZELSP-03~draft} 3. Determinism

Every result carries the exact state it was computed against.

- **Document version.** The LSP `textDocument/didChange` version number of every
  file the answer depended on.
- **Workspace snapshot.** `<git head sha>+<dirty hash>`, where the dirty hash is
  a stable digest of the working tree's modified and untracked file set. Two
  calls that return the same snapshot saw the same bytes.

Both stamps ride on every response and on every published event (§4).

**The barrier.** `analysed_at(version)` is a **blocking call with a deadline**
(the PM recommendation, adopted by the owner — §10 Q5). It returns exactly one of:

```
Ready                          # the server has applied `version`
TimedOut { version_seen }      # the deadline expired; this is how far it got
```

It never returns a partial state, and **callers do not poll**. A polling loop
burns a round trip per iteration and invents its own backoff; one held connection
with a server-side deadline is simpler for a gate and gives the caller a single
place to handle failure. A gate calls the barrier first and then reads results,
so the results it reads provably describe the edit under review.

**Refusing stale answers.** A call naming a version the server has already moved
past returns a typed `Stale { requested, current }` error. It never returns the
current answer relabelled, and never returns the old answer. A caller that wants
the newer state re-asks for it explicitly. This is the same rule as §2's
never-fail-open: a wrong answer that looks right is worse than a refusal.

---

## {#SPEC-ANALYZELSP-04~draft} 4. Asynchrony

Diagnostics arrive when the server finishes, not when the caller asks. Reads and
notifications are therefore separate.

- **Publication.** The daemon publishes results as control-bus events. The
  envelope is ADR-0005's `HarnessEvent` — `{source, session, seq, at, payload}` —
  and the payload adds `{language, workspace, uri, document_version, snapshot,
  kind, items}`. `kind` is `diagnostics` or `references`. ADR-0019 governs the
  bus; ADR-0005 supplies this envelope, because these are telemetry events with
  no per-recipient acknowledgment, not addressed messages.
- **Consumers read latest-at-version.** A subscriber keeps the newest result for
  each `(uri, document_version)` and discards anything older. Nothing replays a
  superseded diagnostic set.
- **Reads stay synchronous.** §5's methods answer from the versioned store. A
  caller that cannot subscribe polls the barrier and then reads.

**Transport: relay through `trusty-console`** (owner ruling, §10 Q4).
`trusty-analyze` takes **no Cargo edge on `trusty-agents-common`**, so
`HarnessEvent` is a shape this daemon mirrors by field name, never a type it
imports. The relay reuses the console↔service seam that already exists rather
than reviving the `/sse` route #6287 removed.

**Analyze side — a bounded ring, read by cursor.** Published events land in an
in-memory ring buffer per `(workspace, language)`, each stamped with a
process-monotonic `seq`. One new socket method drains it:

```
analyze.lsp_events  { since_seq, max }
  -> { events: [ … ], next_seq, dropped }
```

`dropped` reports ring overflow, so a slow reader sees a gap marker instead of
silently missing events — the same contract as ADR-0005's `Lag`. The ring is
memory only. A restart loses undrained events, and that is correct: §5's read
methods are the durable answer, and the events are a notification that an answer
changed.

**Console side — poll, cache, republish.** `trusty-console` polls
`analyze.lsp_events` over UDS, holding the cursor, in the poller that already
polls this daemon's `console_metrics`
(`crates/trusty-console/src/metrics_poller.rs`, which backs
`GET /api/console/metrics/analyze`). It republishes on two routes:

| Route | Shape |
|---|---|
| `GET /api/console/events/analyze/lsp?since_seq=<n>` | JSON: `{events, next_seq, dropped}` — the cursor form, for a programmatic consumer |
| `GET /api/console/events/analyze/lsp/stream` | SSE: one event per element, for the dashboard |

This keeps ADR-0032 intact — analyze binds no socket but its own UDS one, and
console remains the only HTTP surface — and matches ADR-0035's direction, where
console aggregates over UDS and clients read console.

---

## {#SPEC-ANALYZELSP-05~draft} 5. API surface

The methods are JSON-RPC over the daemon's Unix socket, and are mirrored as MCP
tools. Transport is UDS only, per ADR-0032 — no HTTP listener, no port, no
network bind.

The existing socket method `analyze.diagnostics` runs the batch linters, so the
new methods take an `analyze.lsp_` prefix rather than colliding with it. MCP tool
names are bare snake_case, matching `complexity_hotspots` and `find_smells`.

| Socket method | MCP tool | Parameters | Returns |
|---|---|---|---|
| `analyze.lsp_diagnostics` | `lsp_diagnostics` | `{scope: file\|workspace, path?, version}` | type-checked diagnostics at `version` |
| `analyze.lsp_references` | `lsp_references` | `{symbol, version}` | resolved reference sites |
| `analyze.lsp_definition` | `lsp_definition` | `{symbol, version}` | the resolved definition site |
| `analyze.lsp_call_hierarchy` | `lsp_call_hierarchy` | `{symbol, direction: incoming\|outgoing, depth, version}` | the resolved call tree |
| `analyze.lsp_implementations` | `lsp_implementations` | `{symbol, version}` | concrete implementations of a trait, interface, or abstract method |
| `analyze.lsp_type_at` | `lsp_type_at` | `{path, line, column, version}` | the inferred type at a position |
| `analyze.lsp_impact` | `lsp_impact` | `{symbol, new_signature, version}` | every call site the signature change breaks |
| `analyze.lsp_analysed_at` | `lsp_analysed_at` | `{version, timeout_ms}` | `Ready` or `TimedOut{version_seen}` — the blocking barrier of §3 |
| `analyze.lsp_events` | — | `{since_seq, max}` | the event cursor of §4; console polls it, agents do not |

- Every response carries `document_version` and `snapshot` (§3).
- Every method takes `version` and refuses a stale one (§3).
- `lsp_impact` is the refactoring primitive: it is `references` plus the type
  check that says which of those sites stop compiling.

### 5.1 Two access paths, one implementation

`trusty-analyze` owns the language-server lifecycle (§2) and answers every call.
Nothing else supervises a server, and no second implementation of these methods
exists. Two paths reach it.

**(a) Direct.** A `trusty-analyze lsp <tool> …` CLI subcommand, plus the
`analyze.lsp_*` RPC above over the daemon's socket. The subcommand is a thin
client: it dials the socket through `trusty_common::uds::OnDemandAnalyze`, like
every other analyze client, and prints the response. This is the path for an
operator, a script, and the merge-chain gate.

```
trusty-analyze lsp diagnostics --scope workspace --version <n>
trusty-analyze lsp impact --symbol <sym> --new-signature <sig> --version <n>
trusty-analyze lsp analysed-at --version <n> --timeout-ms <ms>
```

**(b) With search.** `trusty-search` fronts the same functions on its own tool
surface, so an agent already navigating with `search_kg`, `search_similar`, and
`get_call_chain` asks for type-checked answers in the same tool family instead of
switching daemons mid-investigation. The search-side names take the `search_lsp_`
prefix, matching that surface's existing `search_*` convention:

| Search-side tool | Fronts |
|---|---|
| `search_lsp_references` | `analyze.lsp_references` |
| `search_lsp_definition` | `analyze.lsp_definition` |
| `search_lsp_diagnostics` | `analyze.lsp_diagnostics` |
| `search_lsp_impact` | `analyze.lsp_impact` |

**Ownership is not shared.** `trusty-analyze` owns the lifecycle, the versioned
store, the caps, and the answers. `trusty-search` only fronts them: it holds no
language-server process, caches no result, and adds no semantics of its own. It
reaches analyze over the socket using the adapter pattern `trusty-review` already
uses (`crates/trusty-review/src/report/analyze_adapter.rs`, which dials through
`OnDemandAnalyze`) — a third client of an existing seam, not a new one.

The four fronted tools are the navigation-adjacent subset, because that is what a
caller already in the search surface is doing. `lsp_call_hierarchy`,
`lsp_implementations`, `lsp_type_at`, and `lsp_analysed_at` stay analyze-only;
the barrier in particular belongs to the gate, not to a navigation flow. Whether
that split holds is one of the things phase 0 observes (§9).

---

## {#SPEC-ANALYZELSP-06~draft} 6. Judicious-use policy

The measured cost of calling this on reflex is 40% wall time for nothing
(§Why). The policy is therefore part of the contract, not advice.

**Triggers that may call it.**

- After a multi-file edit in a typed language, once, when the edit is complete.
- Architecture mapping: `lsp_call_hierarchy` and `lsp_implementations` when the
  AST graph returns an ambiguous or dispatch-dependent answer.
- Refactoring: `lsp_references` and `lsp_impact` before an edit to size it, and
  after the edit to confirm no call site was missed.
- Performance analysis: `lsp_call_hierarchy` to find hot paths, joined with
  `analyze.complexity_hotspots` to rank them.
- The merge gate for a Python or TypeScript repository that has **no CI type
  check** of its own. A repository whose CI already runs `pyright` or
  `tsc --noEmit` does not need this at the gate.
- A review, when the operator asked for it with `--analyze` — never on a bare
  review run (§8e).
- An explicit operator request.

**One trigger is not judicious — it is mandatory.** The audit always consumes
this capability (§8d). §8's consumers table states each caller's obligation and
what it does when the capability is unavailable.

**Triggers that must not call it.**

- Every session start. Nothing provisions a language server (§2).
- Every single-file edit. The batch linter already covers this and costs less.
- Headless one-shot tasks. The 0/18 and 0/6 invocation counts came from exactly
  this shape of work.
- Plain navigation. That routes to `trusty-search` (§1).

---

## {#SPEC-ANALYZELSP-07~draft} 7. Resource budget

Measured in the #6589 trials, 2026-09-01. Method: headless `claude -p`, sonnet,
three edit tasks per language, two arms per task (plugin on, plugin off), with
the language-server process sampled by `ps` every 5 s. Each language ran against
its own corpus, named in the table.

| Language | Server | Corpus | Peak RSS | Mean CPU | Time to first ready | Wall, on vs off | Explicit invocations |
|---|---|---|---|---|---|---|---|
| Rust | `rust-analyzer` | `trusty-tools`, 21-crate workspace | 1683 MB | 15.8% | not recorded | 98 s vs 82 s | 0/8 |
| Python | `pyright-langserver` | `pypa/packaging` @ `b9d249f` | ~180 MB (168–197) | ~2% | 10–18 s | 34 s vs 47 s | 0/6 |
| TypeScript | `typescript-language-server` | `colinhacks/zod` v3.23.8 | ~61 MB | ~0% | not recorded | 40 s vs 36 s | 0/6 |

Reading the table:

- **Rust dominates the budget.** rust-analyzer's peak RSS is roughly 9× pyright's
  and 27× tsserver's, and its mean CPU is the only one that registers. The
  per-server cap below follows from that spread.
- **The Rust wall figures differ from §Why's, and both are correct.** This leg
  measured 98 s on vs 82 s off; leg 1's full task set measured 142 s vs 84 s.
  Different task sets, same direction — the plugin costs time.
- **Python's arm-A wall time is not a plugin win.** 34 s on vs 47 s off looks
  like one, but arm B carried a 77 s outlier over n=3. `pyright` and `pytest`
  results were identical between arms.
- **No arm changed an outcome.** `tsc` and `jest` were identical between the
  TypeScript arms too.

**What these numbers establish, and what they do not.** Across all three
languages the model made **0 of 30 explicit LSP tool calls**. Passive post-edit
diagnostics did attach in 4 of 6 Python and TypeScript arm-A trials, and changed
no outcome in any of them. The table therefore measures **the cost of a language
server being available under the plugin, not the benefit of one being called
deliberately**. Measuring the second is phase 0's job (§9); these numbers are its
baseline, per the owner's ruling to study further (§10 Q3).

**Initial per-server caps.** The cap is per server (owner ruling, §10 Q6):

| Server | Initial cap | Headroom over measured peak |
|---|---|---|
| `rust-analyzer` | 3 GB | ~1.8× |
| `pyright-langserver` | 512 MB | ~2.6× over the 197 MB high sample |
| `typescript-language-server` | 256 MB | ~4.2× |

These are **initial values, tuned in phase 0 and phase 2** — not ceilings derived
from the measurement. The headroom is wider than a measured-peak-plus-50% rule
would give, because one trial leg on one corpus per language is a thin basis for
a hard limit, and a cap that fires on a legitimately larger workspace costs more
than one set slightly high. The daemon samples each child's RSS against its own
cap.

There is **no aggregate budget** across servers. A per-server cap fails one
language and leaves the others working; a shared budget would let a heavy Rust
workspace starve an unrelated Python one, and would make the failure depend on
what else happened to be running.

**What happens when a cap is exceeded: refuse, never swap.** The daemon kills the
offending server, marks that one `(workspace, language)` pair unavailable for a
cool-off window, and returns a typed `CapacityExceeded` error to callers of that
pair during it. Every other supervised server keeps serving. Callers fall back to
the batch checker. The daemon does not queue, does not retry in a loop, and does
not let the machine page — a swapping analysis daemon is worse than no analysis
daemon, and this repository has already paid for that lesson in `trusty-search`.

---

## {#SPEC-ANALYZELSP-08~draft} 8. Deliverables

Five deliverables. The tools alone are not the capability; nothing calls them
unless the skill, the instructions, and the consuming pipelines say when to.

**(a) The tools.** §5's eight methods on `trusty-analyze`'s socket, mirrored as
MCP tools, with the lifecycle of §2, the stamps of §3, and the events of §4 —
reachable both ways §5.1 fixes: the `trusty-analyze lsp` CLI subcommand and the
`search_lsp_*` tools `trusty-search` fronts.

**(b) The analyze skill.** There is **no bundled `analyze` skill today** — the
bundled skills live in `crates/trusty-mpm/src/assets/skills/` (flat
`<name>.md`, with an optional `<name>/references/` directory) and mirror into
`crates/trusty-code/src/assets/skills/<name>/SKILL.md`. This deliverable creates
`analyze.md` there. It documents each tool of §5 and, for each, when to call it
and when not to — §6 is the skill's spine, not an appendix. It teaches the two
access paths of §5.1 — the `trusty-analyze lsp` subcommand and the
`search_lsp_*` tools — and names no plugin, because none exists (§1).

**(c) The base coding-agent instructions.**
`crates/trusty-agents-common/src/assets/agents/BASE-ENGINEER.md` is the layer
every engineer agent inherits (`agent_assets.rs:178` registers it; `engineer.md`
and every language engineer extend it). It gains a short section telling the
agent to use these tools for:

- **architecture mapping** — `lsp_call_hierarchy`, `lsp_implementations`;
- **refactoring impact** — `lsp_references` and `lsp_impact`, before the edit to
  size it and after to confirm it;
- **performance analysis** — `lsp_call_hierarchy` hot paths joined with
  `analyze.complexity_hotspots`.

Each is written against the surface the agent actually has: `search_lsp_*` when
it is already navigating in the search tools, and `trusty-analyze lsp` otherwise
(§5.1). The instructions name no plugin.

The section states the restraint in the same breath: not on every edit, not on
session start, not for plain navigation. `BASE-AGENT.md` is not the right home —
it is inherited by non-coding agents that have no use for this.

**(d) The audit toolkit — REQUIRED.** The audit consumes analyze; that is settled
(owner ruling, 2026-09-01: "required for audit"). The tools reach it through the
seam DOC-67 §5 already fixes. tga never calls trusty-analyze
directly; `trusty-review`'s `HttpAnalyzeMetricsSource`
(`crates/trusty-review/src/report/analyze_adapter.rs`) is the single client, and
it already dials the socket through `OnDemandAnalyze`. So the LSP fetches are
added to that adapter's existing fetch set, alongside the `/quality` fetch DOC-67
§8 adds — **not** as a new stage in `run_full_sweep`. In sweep terms the work
happens inside stage 9, `report`, which is the stage that invokes
`trusty-review report --analyze`. Adding a tenth sweep stage would put tga in
direct contact with trusty-analyze, which DOC-67 §5 seam 3 forbids.

**Which of the eight tools the audit consumes is decided by experiment** (owner
ruling, §10 Q7); phase 2 of §9 runs it. That the audit consumes analyze at all is
not part of that experiment.

**The audit must not fail open.** `trusty-review report --analyze` is fail-open
today by design: an unreachable daemon prints a warning, pushes a
`trusty-analyze data unavailable` string into `model.gaps`, and falls through to
the built-in scan (`cli_report.rs:108-121`, `:248-264`). That is right for a
review (§8e) and wrong for an audit, where a silently scan-only dimension reads
as an assessed one.

On the audit path, an unavailable capability produces one of two outcomes, never
a silent fallback:

- **Fail loudly** — the sweep's stage 9 aborts with the reason, when the audit
  was invoked in a mode that requires complete dimensions.
- **Mark the dimension INCOMPLETE** in the report, naming the reason verbatim.

The reasons to distinguish: the server is not provisioned, the workspace's
language is unsupported, the per-server RSS cap was exceeded (§7), or the
`analysed_at` barrier timed out (§3). Each is a different remedy, so each is
reported as itself rather than as one generic gap — the same absent-versus-
undeterminable distinction [ADR-0045](../adr/0045-distinguish-absent-from-undeterminable-on-destructive-paths.md)
draws for destructive paths, applied here to a report's evidence.

> **Stale reference in DOC-67.** DOC-67 §5's diagram labels the analyze edge
> `HTTP :7879`. #6287 retired that listener and ADR-0032 forbids it; the adapter
> is UDS today. This spec does not edit DOC-67 — the correction belongs in a
> DOC-67 revision.

**(e) The review path — OPTIONAL, on request.** A review fetches analyze's LSP
results when asked and not otherwise (owner ruling, 2026-09-01: "an option for
review if requested").

**The flag already exists.** `trusty-review report --analyze`
(`crates/trusty-review/src/cli_report.rs:120-121`, epic #2445) is off by default,
and a bare run stays scan-only. This deliverable does not add a flag — it adds
the LSP fetches to what that flag already fetches, through the same
`HttpAnalyzeMetricsSource` adapter §8d uses. One flag, one adapter, two fetch
sets that differ only in whether the caller is a review or an audit.

**Unavailable is reported, never fatal.** The review keeps its fail-open
behaviour: it names the shortfall in the report and finishes. The existing gap
string is the precedent — `"trusty-analyze data unavailable — … Findings,
complexity, and health factors are not assessed, not clean"` — and the LSP
shortfall is reported the same way, saying which capability was missing. A review
run without `--analyze` reports nothing at all about it, because nothing was
asked for.

### Who calls this, and under what obligation

| Consumer | Obligation | Behaviour when unavailable |
|---|---|---|
| Audit (`tga audit` → `trusty-review report --analyze`, stage 9) | **Required** (§8d) | Fail loudly, or mark the dimension INCOMPLETE with the specific reason |
| Review (`trusty-review report --analyze`) | **Optional, on request** (§8e) | Report the shortfall in the report and finish |
| Coding agents (`search_lsp_*`, `trusty-analyze lsp`) | **Judicious** — §6's trigger list | Fall back to the batch checker; the error is typed, never silent (§2) |
| Session provisioning | **Never** (§2) | Not applicable; nothing is started |

---

## {#SPEC-ANALYZELSP-09~draft} 9. Rollout

Five phases, each independently shippable, each behind an explicit flag until its
acceptance criteria are met.

**Phase order is repo-first.** The coding agents working in this repository write
Rust, so Rust is where the capability earns or fails its keep here. This inverts
the gap-first order (#6606's outline put Python first because it has no compiler
step at all); the gap is real and Python is phase 3, not dropped.

### Phase 0 — Intentional analysis

Not a build phase. A measurement phase that decides what phase 1 optimises for
(owner ruling, §10 Q3: "study more, do intentional analysis").

The existing A/B numbers are the **baseline, not the verdict**. Both trials
measured a plugin the model never chose to invoke — 0/18 and 0/6 — so they
measure availability, not usefulness. Phase 0 measures the queries under
deliberate use, where the caller is told to use them.

**Queries under test.** `lsp_references`, `lsp_call_hierarchy`,
`lsp_implementations`, `lsp_impact`.

**Coding-agent tasks.** Three shapes, drawn from §8c's instruction set, run on
this repository:

| Task | What the agent is asked to do |
|---|---|
| Architecture mapping | Map the call and implementation structure of a named subsystem |
| Refactor impact | Size and then execute a signature change across crates |
| Performance hot-path | Find the hot path into a named function and rank it by complexity |

**Metrics.** Per task and arm: invocation count (did the agent call the tool when
told to), correctness delta against a grep-and-read ground truth, wall time, and
peak RSS of the language server.

**Decision rule.** Phase 1 proceeds when deliberate use shows a correctness gain
on at least one task shape at acceptable wall time and RSS. If deliberate use
also shows no gain, the finding is that the AST index answers these questions
adequately, and the capability narrows to type-checked diagnostics only — §1's
first bullet — dropping the navigation-adjacent tools. Either outcome is a
result; neither cancels the spec.

### Phase 1 — `rust-analyzer`

**Not time-boxed** (owner ruling, §10 Q3). Phase 0 supplies the intent; phase 1
builds against it.

- Ships behind an explicit flag, default off.
- Carries the mise-shim / pinned-toolchain provisioning caveat of §2 as a
  hard install requirement, with a probe that fails loudly rather than
  installing a shim that recurses.
- **Acceptance:** a supervised server starts on demand and exits on idle; the
  barrier of §3 returns `Ready` only after the named version is applied;
  `lsp_impact` finds a call site `cargo check` also flags, on a seeded signature
  change; §7's Rust row is re-measured under deliberate tool use, not plugin
  availability; the crash Claude
  Code swallowed as `outcome=ok` surfaces here as a typed error.

### Phase 2 — Audit tool-selection experiment

Which of §5's eight tools the audit consumes is decided by running them (owner
ruling, §10 Q7). The experiment takes three repositories of increasing size,
runs a DD report twice each — once with the LSP fetches in
`analyze_adapter.rs`'s set and once without — and records, per tool, the added
report findings, the added wall time, and the added peak RSS. A tool enters the
audit's default set when it changes a report's findings on at least one
repository at a cost that scales; a tool that only slows the sweep is left out
and stays available on request. Acquisition-sized corpora are the point of the
size ladder: a tool that pays for itself on one workspace can be untenable
across fifty.

- **Acceptance:** the per-tool table is filled for all three repositories, and
  `analyze_adapter.rs`'s default fetch set names only the tools that earned a
  place.

### Phase 3 — `pyright`

- Closes the largest capability gap: Python has no compiler step in this daemon
  today, only `ruff` (§1).
- **Acceptance:** phase 1's criteria on a Python workspace; `lsp_diagnostics`
  reports a type error `ruff` does not; §7's Python row is re-measured under
  deliberate tool use; a Python
  repository with no CI type check gets a gate verdict from §6's review trigger.

### Phase 4 — `typescript-language-server`

- **Acceptance:** phase 1's criteria on a TypeScript workspace;
  `lsp_diagnostics` agrees with `tsc --noEmit` on a seeded type error; §7's
  TypeScript row is re-measured under deliberate tool use.

**Across all phases.** The batch checkers remain the merge-chain gate,
navigation remains on `trusty-search`, and no phase makes a language server
resident or session-provisioned.

---

## {#SPEC-ANALYZELSP-10~draft} 10. Resolved decisions

All seven questions this section originally asked were answered by the owner on
**2026-09-01**. The question text is kept so the ruling reads against what was
asked.

**Q1 — Provisioning.** *Does `trusty-installer` install these third-party
servers, or does the daemon only probe and report as it does for `ruff` and
`biome` today? The installer provisions no third-party linter now, so this is a
new responsibility either way.*

> **Ruled 2026-09-01:** "Configurable option, on my default (linters too)."
> The installer provisions the language servers **and** `ruff` and `biome`,
> under a config option defaulting to ON. With the option off the daemon keeps
> today's probe-and-report behaviour. Applied in §2.

**Q2 — Node runtime.** *`pyright` and `typescript-language-server` are npm
packages. Is a Node runtime an acceptable requirement on an operator machine, or
should phases 2 and 3 be gated on the machine already having one?*

> **Ruled 2026-09-01:** "Node or Bun, recommend installing in scaffolding."
> Either runtime serves and the scaffolding installs one, so no phase is gated
> on a pre-existing runtime. §2 fixes the selection rule: prefer Bun when
> present, else Node when present, else install Node.

**Q3 — Rust at all.** *The Rust edit A/B measured 0/6 invocations and a 40%
slowdown from the plugin. Phase 1 is Rust because this repository is Rust.
Should phase 1 instead be a time-boxed evaluation that can be abandoned, rather
than a shipped flag?*

> **Ruled 2026-09-01:** "Study more, do intentional analysis."
> Phase 1 is **not** time-boxed. A new phase 0 measures the queries under
> deliberate use first, and the existing A/B numbers are its baseline rather
> than its verdict. Applied in §9.

**Q4 — Event transport.** *#6287 removed this daemon's event channel and
ADR-0032 leaves it no HTTP surface. Does `trusty-analyze` take a Cargo edge on
`trusty-agents-common` for `HarnessEvent`, or does it relay through
`trusty-console`?*

> **Ruled 2026-09-01:** "Relay."
> Events relay through `trusty-console`, and `trusty-analyze` takes **no** Cargo
> edge on `trusty-agents-common`. §4 fixes the ring buffer, the
> `analyze.lsp_events` cursor method, and the two console routes.

**Q5 — Barrier semantics.** *Is `analysed_at` a blocking call with a deadline, or
a poll the caller loops on? A blocking call is simpler for a gate and holds a
connection open for the duration.*

> **Ruled 2026-09-01:** "Recommend."
> The owner adopted the recommendation stated in the question: a blocking call
> with a deadline, returning `Ready` or `TimedOut{version_seen}`, with no polling
> loops. Applied in §3 and §5.

**Q6 — Cap granularity.** *Is the §7 hard cap per language server, or one budget
across every server the daemon supervises?*

> **Ruled 2026-09-01:** "Cap per server."
> One RSS ceiling per language-server process, no aggregate budget. Exceeding it
> kills that one server and refuses callers of that pair; every other server
> keeps serving. Applied in §7.

**Q7 — Audit depth.** *Which of §5's tools does the audit actually consume?
Running all eight over an acquisition-sized corpus is a different cost profile
from running them over one workspace.*

> **Ruled 2026-09-01:** "Experiment."
> A dedicated phase decides it by measurement across three repositories of
> increasing size. Applied in §8d and as phase 2 of §9.

No question in this spec is open. What remains unsettled is measurement: §7's
per-server caps are initial values rather than derived ceilings, phase 0's
decision rule has not been run, and phase 2's per-tool table is empty.

---

## {#SPEC-ANALYZELSP-11~draft} 11. Implementation issues to cut

To be filed by `ticketing` once the owner accepts this spec. One line each; each
references #6606.

1. **Phase 0 — intentional analysis (§9).** Run the four queries against the
   three coding-agent task shapes, record invocations, correctness delta, wall
   time, and RSS, and apply the decision rule. Blocks nothing structurally, but
   its outcome sets phase 1's target. First issue to cut.
2. `trusty-analyze`: language-server supervisor keyed by `(workspace, language)`,
   reusing `UdsServiceSupervisor`, with per-language idle exit (§2).
3. `trusty-analyze`: versioned result store — document version plus
   `<git head>+<dirty hash>` snapshot on every result (§3).
4. `trusty-analyze`: `analyze.lsp_analysed_at` as a blocking barrier returning
   `Ready | TimedOut{version_seen}`, plus the `Stale` refusal (§3).
5. `trusty-analyze`: the seven read methods of §5 on the socket, plus their MCP
   descriptors.
6. `trusty-analyze`: the `trusty-analyze lsp <tool>` CLI subcommand, a thin
   client dialing the socket through `OnDemandAnalyze` (§5.1a).
7. `trusty-search`: front `search_lsp_references`, `search_lsp_definition`,
   `search_lsp_diagnostics`, and `search_lsp_impact` on its tool surface, calling
   analyze over the socket via the `analyze_adapter` pattern. No language-server
   process, no cache, no added semantics; transport follows ADR-0032, adding no
   new listener (§5.1b).
8. `trusty-analyze`: the §4 event ring buffer and the `analyze.lsp_events` cursor
   method, with `dropped` on overflow.
9. `trusty-console`: poll `analyze.lsp_events` in `metrics_poller.rs` and
   republish on `GET /api/console/events/analyze/lsp` (cursor JSON) and
   `…/lsp/stream` (SSE) (§4). No Cargo edge from analyze to
   `trusty-agents-common`.
10. `trusty-analyze`: per-server RSS cap, kill, cool-off, and the
    `CapacityExceeded` error (§7).
11. `trusty-installer`: add the analysis tools to the `PREREQS` table behind
    `analysis_tools.install` (default ON) — the three language servers plus
    `ruff` and `biome`, with `rust-analyzer` installed per pinned toolchain and a
    probe that detects the mise-shim recursion (§2).
12. `trusty-installer`: the JavaScript-runtime scaffolding step — Bun if present,
    else Node if present, else install Node, with the `js_runtime` override (§2).
13. `trusty-mpm`: create the bundled `analyze` skill documenting each tool and its
    trigger policy (§8b).
14. `trusty-agents-common`: add the tool-use section to `BASE-ENGINEER.md` (§8c).
15. `trusty-review`: extend `analyze_adapter.rs`'s fetch set with the LSP fetches
    consumed by both the audit and `--analyze` (§8d, §8e).
16. `trusty-review`: the audit's hard requirement — replace the fail-open
    fallback on the audit path with fail-loud or an INCOMPLETE dimension naming
    the specific reason (not provisioned, unsupported language, RSS cap,
    barrier timeout) (§8d).
17. `trusty-review`: surface the LSP shortfall through the existing `--analyze`
    fail-open gap path, so a requested review that cannot get semantics says so
    and still finishes (§8e).
18. **Phase 2 — audit tool-selection experiment (§9).** Run the three-repository
    size ladder with and without the LSP fetches, fill the per-tool table, and
    set `analyze_adapter.rs`'s default fetch set from the result.
19. Phase 1 acceptance run: re-measure §7's Rust row under deliberate tool use
    and record the A/B against the batch checker.
20. Phase 3 (`pyright`) and phase 4 (`typescript-language-server`), one issue
    each, gated on phase 1's acceptance (§9).
