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

**Binary provisioning.** The installer provisions the server binaries; the daemon
probes and reports, exactly as `tool_registry.rs` does for `ruff` and `biome`
today.

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

**The barrier.** `analysed_at(version)` is a synchronous call. It returns only
when the server has applied the named document version, or it returns a
timeout — never a partial state. A gate calls the barrier first and then reads
results, so the results it reads provably describe the edit under review.

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

**Transport is an open question (§10, Q4).** #6287 removed `AnalyzerEvent`, the
broadcast channel, and the `/sse` route from this daemon, and ADR-0032 leaves it
with no HTTP surface to stream from. `trusty-analyze` also has no Cargo edge on
`trusty-agents-common`, so `HarnessEvent` is not reachable from it today. Adding
that edge, or relaying through `trusty-console`, is a decision this spec does not
make.

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
| `analyze.lsp_analysed_at` | `lsp_analysed_at` | `{version, timeout_ms}` | the barrier of §3 |

- Every response carries `document_version` and `snapshot` (§3).
- Every method takes `version` and refuses a stale one (§3).
- `lsp_impact` is the refactoring primitive: it is `references` plus the type
  check that says which of those sites stop compiling.

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
- The review or merge gate for a Python or TypeScript repository that has **no
  CI type check** of its own. A repository whose CI already runs `pyright` or
  `tsc --noEmit` does not need this at the gate.
- An explicit operator request.

**Triggers that must not call it.**

- Every session start. Nothing provisions a language server (§2).
- Every single-file edit. The batch linter already covers this and costs less.
- Headless one-shot tasks. The 0/18 and 0/6 invocation counts came from exactly
  this shape of work.
- Plain navigation. That routes to `trusty-search` (§1).

---

## {#SPEC-ANALYZELSP-07~draft} 7. Resource budget

Per-language expected footprint. **The cells marked TBD are filled from the
#6589 trials**, which measure RSS and CPU per server on this repository; the
spec is not accepted with them empty.

| Language | Server | Steady RSS | Peak RSS | Cold index | CPU at idle |
|---|---|---|---|---|---|
| Rust | `rust-analyzer` | TBD | TBD | TBD | TBD |
| Python | `pyright` | TBD | TBD | TBD | TBD |
| TypeScript | `typescript-language-server` | TBD | TBD | TBD | TBD |

**The hard cap.** A per-language RSS ceiling is configured, defaulting to the
measured peak plus 50%. The daemon samples each child's RSS.

**What happens when a cap is exceeded: refuse, never swap.** The daemon kills the
offending server, marks that `(workspace, language)` pair unavailable for a
cool-off window, and returns a typed `CapacityExceeded` error to every caller
during it. Callers fall back to the batch checker. The daemon does not queue,
does not retry in a loop, and does not let the machine page — a swapping analysis
daemon is worse than no analysis daemon, and this repository has already paid for
that lesson in `trusty-search`.

---

## {#SPEC-ANALYZELSP-08~draft} 8. Deliverables

Four deliverables. The tools alone are not the capability; nothing calls them
unless the skill and the instructions say when to.

**(a) The tools.** §5's eight methods on `trusty-analyze`'s socket, mirrored as
MCP tools, with the lifecycle of §2, the stamps of §3, and the events of §4.

**(b) The analyze skill.** There is **no bundled `analyze` skill today** — the
bundled skills live in `crates/trusty-mpm/src/assets/skills/` (flat
`<name>.md`, with an optional `<name>/references/` directory) and mirror into
`crates/trusty-code/src/assets/skills/<name>/SKILL.md`. This deliverable creates
`analyze.md` there. It documents each tool of §5 and, for each, when to call it
and when not to — §6 is the skill's spine, not an appendix.

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

The section states the restraint in the same breath: not on every edit, not on
session start, not for plain navigation. `BASE-AGENT.md` is not the right home —
it is inherited by non-coding agents that have no use for this.

**(d) The audit toolkit.** The tools become an analysis input to the audit,
through the seam DOC-67 §5 already fixes. tga never calls trusty-analyze
directly; `trusty-review`'s `HttpAnalyzeMetricsSource`
(`crates/trusty-review/src/report/analyze_adapter.rs`) is the single client, and
it already dials the socket through `OnDemandAnalyze`. So the LSP fetches are
added to that adapter's existing fetch set, alongside the `/quality` fetch DOC-67
§8 adds — **not** as a new stage in `run_full_sweep`. In sweep terms the work
happens inside stage 9, `report`, which is the stage that invokes
`trusty-review report --analyze`. Adding a tenth sweep stage would put tga in
direct contact with trusty-analyze, which DOC-67 §5 seam 3 forbids.

> **Stale reference in DOC-67.** DOC-67 §5's diagram labels the analyze edge
> `HTTP :7879`. #6287 retired that listener and ADR-0032 forbids it; the adapter
> is UDS today. This spec does not edit DOC-67 — the correction belongs in a
> DOC-67 revision.

---

## {#SPEC-ANALYZELSP-09~draft} 9. Rollout

Three phases, each independently shippable, each behind an explicit flag until
its acceptance criteria are met.

**Phase order is repo-first.** The coding agents working in this repository write
Rust, so Rust is where the capability earns or fails its keep here. This inverts
the gap-first order (#6606's outline put Python first because it has no compiler
step at all); the gap is real and Python is phase 2, not dropped.

### Phase 1 — `rust-analyzer`

- Ships behind an explicit flag, default off.
- Carries the mise-shim / pinned-toolchain provisioning caveat of §2 as a
  hard install requirement, with a probe that fails loudly rather than
  installing a shim that recurses.
- **Acceptance:** a supervised server starts on demand and exits on idle; the
  barrier of §3 returns only after the named version is applied; `lsp_impact`
  finds a call site `cargo check` also flags, on a seeded signature change; the
  §7 table's Rust row is filled from measurement; the crash Claude Code
  swallowed as `outcome=ok` surfaces here as a typed error.

### Phase 2 — `pyright`

- Closes the largest capability gap: Python has no compiler step in this daemon
  today, only `ruff` (§1).
- **Acceptance:** phase 1's criteria on a Python workspace; `lsp_diagnostics`
  reports a type error `ruff` does not; the §7 Python row is filled; a Python
  repository with no CI type check gets a gate verdict from §6's review trigger.

### Phase 3 — `typescript-language-server`

- **Acceptance:** phase 1's criteria on a TypeScript workspace;
  `lsp_diagnostics` agrees with `tsc --noEmit` on a seeded type error; the §7
  TypeScript row is filled.

**Across all phases.** The batch checkers remain the merge-chain gate,
navigation remains on `trusty-search`, and no phase makes a language server
resident or session-provisioned.

---

## {#SPEC-ANALYZELSP-10~draft} 10. Open questions for the owner

1. **Provisioning.** Does `trusty-installer` install these third-party servers,
   or does the daemon only probe and report as it does for `ruff` and `biome`
   today? The installer provisions no third-party linter now, so this is a new
   responsibility either way.
2. **Node runtime.** `pyright` and `typescript-language-server` are npm packages.
   Is a Node runtime an acceptable requirement on an operator machine, or should
   phases 2 and 3 be gated on the machine already having one?
3. **Rust at all.** The Rust edit A/B measured 0/6 invocations and a 40% slowdown
   from the plugin. Phase 1 is Rust because this repository is Rust. Should
   phase 1 instead be a time-boxed evaluation that can be abandoned, rather than
   a shipped flag?
4. **Event transport.** #6287 removed this daemon's event channel and ADR-0032
   leaves it no HTTP surface. Does `trusty-analyze` take a Cargo edge on
   `trusty-agents-common` for `HarnessEvent`, or does it relay through
   `trusty-console`?
5. **Barrier semantics.** Is `analysed_at` a blocking call with a deadline, or a
   poll the caller loops on? A blocking call is simpler for a gate and holds a
   connection open for the duration.
6. **Cap granularity.** Is the §7 hard cap per language server, or one budget
   across every server the daemon supervises?
7. **Audit depth.** Which of §5's tools does the audit actually consume? Running
   all eight over an acquisition-sized corpus is a different cost profile from
   running them over one workspace.

---

## {#SPEC-ANALYZELSP-11~draft} 11. Implementation issues to cut

To be filed by `ticketing` once the owner accepts this spec. One line each; each
references #6606.

1. `trusty-analyze`: language-server supervisor keyed by `(workspace, language)`,
   reusing `UdsServiceSupervisor`, with per-language idle exit (§2).
2. `trusty-analyze`: versioned result store — document version plus
   `<git head>+<dirty hash>` snapshot on every result (§3).
3. `trusty-analyze`: `analyze.lsp_analysed_at` barrier and the `Stale` refusal
   (§3).
4. `trusty-analyze`: the seven read methods of §5 on the socket, plus their MCP
   descriptors.
5. `trusty-analyze`: publish diagnostics and reference results as control-bus
   events — blocked on open question 4 (§4).
6. `trusty-analyze`: per-language RSS cap, kill, cool-off, and the
   `CapacityExceeded` error (§7).
7. `trusty-installer`: provision `rust-analyzer` per pinned toolchain, with a
   probe that detects the mise-shim recursion — blocked on open question 1 (§2).
8. `trusty-mpm`: create the bundled `analyze` skill documenting each tool and its
   trigger policy (§8b).
9. `trusty-agents-common`: add the tool-use section to `BASE-ENGINEER.md` (§8c).
10. `trusty-review`: extend `analyze_adapter.rs`'s fetch set with the audit's LSP
    fetches (§8d).
11. Phase 1 acceptance run: fill the §7 Rust row from measurement and record the
    A/B against the batch checker.
12. Phase 2 (`pyright`) and phase 3 (`typescript-language-server`), one issue
    each, gated on phase 1's acceptance (§9).
