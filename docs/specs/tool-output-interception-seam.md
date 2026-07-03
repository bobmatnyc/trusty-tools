# Live Tool-Output Interception Seam for Native `tm` Sessions

**DOC-32** | Status: `Draft` | Date: 2026-07-03

**Status:** Draft
**Version:** v1
**Subsystem:** TOOLPROXY
**Owner:** Engineering / Architecture (trusty-mpm)
**Last-updated:** 2026-07-03
**Related:** [mpm-cutover-resume-native-optimization.md §C](./mpm-cutover-resume-native-optimization.md#SPEC-MPM-CUTOVER-03~draft), [token-compression-rtk-ztk.md](../trusty-agents/research/token-compression-rtk-ztk.md)

## Summary

Issue #1944 (closed, PR #1952) established that trusty-mpm's `optimize_tool_output`
compression only ever touches the *observability* copy of a tool result (dashboard,
ring buffer, compacted history) — never the copy the model actually reads. This doc
is the follow-up investigation (#1953) requested to evaluate the two viable seams for
reducing a **native Claude Code session's live tool-output tokens**, quantify realistic
savings with a real spike, and decide whether `SPEC-MPM-CUTOVER-03` should retarget to
one of them.

**Recommendation (see [§ Decision](#decision)):** do not retarget `SPEC-MPM-CUTOVER-03`
yet. The MCP-proxy seam (Option 1) is architecturally viable and reuses shipped code
(`compress_tool_output_async`), but the spike shows the *existing* native filter chain
only compresses 2 of 4 realistic fixture types (cargo/git — not grep/ls), so the
opt-in, steering-only UX described below caps realistic savings well short of the
rtk/ztk README numbers until filter coverage is extended. Recommend a scoped follow-up
spike (see [§ Follow-ups](#follow-up-implementation-issues)) before committing engineering
time to the full MCP proxy.

## Contents

- [Problem](#problem)
- [Option 1: tm-Provided MCP Tool-Output Proxy](#option-1-tm-provided-mcp-tool-output-proxy-spec-toolproxy-01draft)
- [Option 2: Future Upstream Claude Code Hook](#option-2-future-upstream-claude-code-tool-output-transform-hook)
- [Spike: Real Compression Numbers](#spike-real-compression-numbers)
- [Decision](#decision)
- [Not Recommended](#not-recommended)
- [Follow-up Implementation Issues](#follow-up-implementation-issues)
- [References](#references)

## Problem

A native Claude Code session (i.e. any session where the user runs `claude` directly,
with `tm hook` wired in as a `PreToolUse`/`PostToolUse`/`Stop` hook) uses **Claude
Code's own tool loop** — Bash, Read, Grep, and friends are built-in tools whose results
are inserted into the model's context by the harness itself, before any hook fires.

This rules out every post-hoc interception point:

- **`PostToolUse` hooks fundamentally cannot help.** By the time the hook fires, the
  tool result is already in the model's context window. Rewriting the payload at that
  point only affects what gets *logged* (dashboard, ring buffer, compacted session
  history) — never what the model already read. This is exactly what
  `daemon/optimizer.rs::optimize_tool_output` does today, and PR #1952 already
  documents the scope honestly (`tm optimizer status` prints a `scope:` note).
- **The `tm hook` relay makes this doubly true today.** `crates/trusty-mpm/src/bin/tm/commands/misc.rs`
  `hook()` (~line 298) forwards only `{session_id, event, payload: {cwd}}` — it never
  reads stdin, so it never even sees `tool_name`/`tool_input`/`tool_response`, even
  though Claude Code's hook protocol delivers all three on stdin and
  `HookService::process` / `mcp_backend.rs` are already written expecting them
  (`payload.get("tool")`/`get("input")`/`get("output")`). Widening the relay to
  forward stdin JSON would enrich the *observability* copy (see
  [§ Not Recommended](#not-recommended)) but cannot touch the live copy, for the
  reason above.
- **RTK is shipped but scoped to the wrong harness.** `compress_tool_output_async`
  (`crates/trusty-agents/src/compress/tool_output/rtk.rs:84`) genuinely reduces live
  tokens — but only inside `tagent`'s own LLM tool loop
  (`llm/tool_loop/mod.rs:424`), which mediates its own tool calls and can rewrite a
  result before it goes back to the model. Native `tm` sessions don't run that loop;
  they run Claude Code's.
- **ZTK doesn't exist in this workspace.** No `ztk` symbol appears anywhere under
  `crates/`. Treat every "native ztk" reference in `SPEC-MPM-CUTOVER-03` as aspirational
  naming for an in-tree Rust filter tier, not a delivered dependency.

**The only way to reduce live tokens is to rewrite the tool result *before* the model
consumes it.** That means either the tool call itself must go through something other
than Claude Code's built-in tool implementation (Option 1), or Claude Code's harness
must ship a transform hook that runs before context insertion (Option 2, speculative).

## Option 1: tm-Provided MCP Tool-Output Proxy {#SPEC-TOOLPROXY-01~draft}

**ID:** SPEC-TOOLPROXY-01~draft
**Status:** Draft

### Behavior Contract (WHAT)

- **Inputs:** A project has opted in (see UX flow below). The model calls one of a
  small set of tm-provided MCP tools (e.g. `tm_bash`, `tm_read`, `tm_grep`) instead of
  — or steered ahead of — the equivalent Claude Code built-in tool, with the same
  logical arguments (command / path / pattern).
- **Outputs:** The MCP tool executes the underlying operation exactly as the built-in
  would (same subprocess/file-read primitives, same working directory and permission
  surface), runs the raw result through `compress_tool_output_async`, and returns the
  compressed text as the tool result. The model never sees the uncompressed output.
- **Preconditions:** The project has enabled the tm MCP tool-output-proxy server (opt-in,
  per-project — see below) and the calling model has been steered (via CLAUDE.md/skill
  instructions) to prefer the proxied tool names for high-volume-output operations.
- **Postconditions:** Live context tokens for the proxied call reflect the compressed
  size, not the raw size. Compression is infallible (falls back to raw output on any
  filter error) so correctness of tool semantics is never at risk — only the byte size
  of what the model reads changes.
- **Error conditions:** If the model calls the raw built-in tool instead of the proxy
  (no steering, or the model ignores the steering), the call executes exactly as it
  does today — no interception, no compression, **no regression**. This is a no-op
  fallback, not a failure mode: nothing about existing behavior degrades when the proxy
  is unused.

### Architecture Sketch

Reuse two pieces of already-shipped code rather than inventing new plumbing:

1. **MCP server scaffold** — trusty-mpm already hand-rolls JSON-RPC 2.0 MCP servers on
   `trusty_common::mcp` (no `rmcp` SDK anywhere in this workspace). The existing
   pattern in `crates/trusty-mpm/src/mcp/mod.rs` is: an `OrchestratorBackend`-style
   trait with one async method per tool, a `tool(name, description, inputSchema)`
   descriptor builder (see `crates/trusty-mpm/src/mcp/tools/core.rs:29`), and a
   `dispatch()` router wired into `trusty_common::mcp::run_stdio_loop`. A tool-output
   proxy server (or a `tm_bash`/`tm_read`/`tm_grep` tool group added to the *existing*
   trusty-mpm MCP server, rather than standing up a new server process) follows this
   exact convention — no new protocol code needed.
2. **Compression function** — `compress_tool_output_async(tool_name, output)`
   (`crates/trusty-agents/src/compress/tool_output/rtk.rs:84`) is a free async fn with
   zero dependency on `tool_loop` internals. It already does the right thing: try the
   `rtk` subprocess if installed, else fall back to the native filter chain
   (`compress_tool_output` in the parent module). It is directly callable from
   trusty-mpm with a path dependency on `trusty-agents` (or, cleaner, by hoisting
   `compress::tool_output` into `trusty-agents-common` alongside `OutputStyle`, per the
   pattern `SPEC-MPM-CUTOVER-02` already established for the caveman-prompt module —
   see [§ Follow-ups](#follow-up-implementation-issues)).

Sketch:

```rust
// crates/trusty-mpm/src/mcp/tools/tool_proxy.rs (new)
async fn tm_bash(&self, session_id: &str, command: &str) -> Result<Value, String> {
    let raw = execute_bash_with_existing_sandboxing(command).await?; // same primitives as today
    let compressed = trusty_agents_common::compress::compress_tool_output_async("bash", &raw).await;
    Ok(json!({ "output": compressed }))
}
```

The proxy tool must call the **same** subprocess/file-read primitives Claude Code's
built-in tools use today — it is not a new execution path, only a new response-shaping
step wrapped around an unchanged execution step.

### UX Flow

1. **Opt-in, per project.** A project enables the tool-output-proxy tools by adding
   them to its `.mcp.json` (the trusty-mpm MCP server already ships one entry; this adds
   tool descriptors, not a new server process, when hoisted into the existing server).
   Off by default — no project sees new tools or new prompts unless it opts in.
2. **Steering, not replacement.** The proxy tools coexist with Claude Code's built-in
   Bash/Read/Grep; they do not and cannot replace them (see
   [§ Security](#provenance-permission-and-security-tradeoffs) — no known upstream
   mechanism exists to transparently override a built-in tool's implementation from an
   MCP server). Adoption is via prompt steering: a CLAUDE.md/skill snippet instructing
   the model "prefer `tm_bash` over Bash for commands with large output (test runs,
   diffs, greps, directory listings)". This is the same mechanism trusty-mpm already
   uses for skill-based behavior steering elsewhere in the harness.
3. **Silent no-op if steering fails.** If the model ignores the steering (weak prompt,
   context pressure, model variance) and calls the raw built-in tool anyway, nothing
   breaks — the call just runs uncompressed, exactly as it does today with no proxy
   installed. This graceful-degradation property is what makes the opt-in safe to ship
   incrementally.

### Provenance, Permission, and Security Tradeoffs

- **No weakening of sandboxing.** A proxy tool executing Bash on the user's behalf
  needs the **same** sandboxing, working-directory scoping, and permission prompts
  Claude Code already enforces for the native Bash tool. The proxy is a response-shaping
  wrapper, not a new execution surface — it must call into the identical execution
  primitive (subprocess spawn with the same cwd/env/allowlist) that the built-in tool
  uses. Do not implement a parallel, more-permissive execution path "for convenience."
- **Tool provenance changes.** Once a project opts in, some fraction of what were
  built-in-tool calls become trusty-mpm-mediated MCP calls instead. This is a real,
  visible change: the tool name in Claude Code's transcript changes from `Bash` to
  `tm_bash`, and the call now round-trips through the trusty-mpm daemon (localhost
  MCP stdio/loopback, not network-exposed). Audit trail implications: trusty-mpm's own
  observability (dashboard, ring buffer) now sees these calls *twice* — once via the
  proxy tool's own invocation and once via whatever `PostToolUse` hook still fires for
  it — so any follow-up implementation must de-duplicate or clearly distinguish the two
  entries.
- **No silent capability escalation.** Because the proxy tool must reuse the built-in's
  execution primitive, opting in confers no new access the user hasn't already granted
  Claude Code (no new file-system reach, no new network reach). The only thing that
  changes is where compression happens and whose transcript the call shows up under.
- **Consent model matches existing MCP tool consent.** Claude Code already prompts for
  MCP tool permission the same way it does for built-in tools; no new consent UX is
  needed beyond what MCP tool registration already provides.

## Option 2: Future Upstream Claude Code "Tool-Output Transform" Hook

Speculative. If Anthropic ships a hook that runs *before* a tool result is inserted
into the model's context (as opposed to today's `PostToolUse`, which runs after), that
hook could call `compress_tool_output_async` directly with zero provenance/permission
tradeoffs — no new tool names, no steering needed, no proxy execution surface. This
would be strictly better than Option 1 wherever it applies. As of this writing no such
hook exists in the Claude Code harness. This is not actionable now; track the upstream
Claude Code changelog and revisit if/when a pre-context-insertion hook ships.

## Spike: Real Compression Numbers

To quantify realistic savings for Option 1's compression step, `compress_tool_output_async`
was run against four fixtures representative of common high-volume tool outputs
(cargo test noise, a git diff, a `grep -r` result, and an `ls -la` listing). The spike
lives at [`crates/trusty-agents/examples/tool_output_compression_spike.rs`](../../crates/trusty-agents/examples/tool_output_compression_spike.rs)
and is run with:

```
cargo run -p trusty-agents --example tool_output_compression_spike
```

`rtk` was **not** installed in the environment that produced these numbers, so every
fixture ran through the **native fallback chain** (`compress_tool_output`), not the RTK
subprocess. Captured output, verbatim:

```
compression path: native fallback chain (rtk NOT on PATH)

--- cargo test (mostly-passing suite) (tool_name="cargo test") ---
  bytes:    1950 ->    323  (83.4% reduction)
  tokens:    267 ->     41  (84.6% reduction)

--- git diff (multi-file changeset) (tool_name="git diff") ---
  bytes:    1551 ->    710  (54.2% reduction)
  tokens:    180 ->    141  (21.7% reduction)

--- grep -r (many matches) (tool_name="grep") ---
  bytes:   14440 ->  14440  (0.0% reduction)
  tokens:    936 ->    936  (0.0% reduction)

--- ls -la (large directory listing) (tool_name="ls") ---
  bytes:    9149 ->   9149  (0.0% reduction)
  tokens:   1781 ->   1781  (0.0% reduction)

=== Aggregate across 4 fixtures ===
  bytes:   27090 ->  24622  (9.1% reduction)
  tokens:    3164 ->   2899  (8.4% reduction)
```

(Byte counts are `str::len()`; token counts use the crate's existing
`compress::estimate_tokens` heuristic — `words * 1.3` — already used elsewhere in
`trusty-agents` rather than inventing a new bytes/4 estimate for this spike.)

### Interpretation — this is the load-bearing finding

The cargo-test and git-diff fixtures compress well (83.4% / 54.2% byte reduction),
consistent with rtk/ztk's published claims for those command domains. **The grep and
ls fixtures compress 0%** — `compress_tool_output`'s dispatch table
(`crates/trusty-agents/src/compress/tool_output/mod.rs:57`) has filter branches for
`test`/`cargo`/`diff`/`log`/`read`/`cat`/`check`/`clippy`, but **no filter branch
matches `grep` or `ls`** — those tool names fall through to the unconditional
passthrough at the bottom of `compress_tool_output`. This directly contradicts the
informal assumption in `SPEC-MPM-CUTOVER-03`'s "v1 filter granularity" decision, which
lists `git, cargo, ls, grep` as the intended v1 command-domain coverage — that coverage
does not exist in the code today for `ls`/`grep`, only for `git`(diff/log)/`cargo`(test/
check).

Practically: if Option 1 ships today by wiring `compress_tool_output_async` as-is, the
`tm_grep` and `tm_ls` proxy tools would provide **zero** compression benefit over the
raw built-ins — they would add the provenance/permission cost of proxying with no
offsetting token savings, which is a bad trade. The `tm_bash`-wrapping-`cargo test` and
`tm_bash`-wrapping-`git diff` cases are worth shipping; `tm_grep`/`tm_ls` are not, until
filters are added for those command domains (tracked in
[§ Follow-ups](#follow-up-implementation-issues)).

### Rationale (WHY)

Realizes the investigation scope of issue #1953: quantify realistic savings vs.
provenance/permission cost before committing to full implementation. The architecture
in [§ Option 1](#option-1-tm-provided-mcp-tool-output-proxy-spec-toolproxy-01draft) is
sound and reuses shipped code with minimal new surface area, but the spike shows the
"reuse `compress_tool_output_async` as-is" plan only pays off for a subset of the
originally envisioned command coverage. Shipping the MCP proxy with known-0%-reduction
tool wrappers would be dishonest about savings and not worth the security-review cost
of a new tool-provenance surface for those specific tools.

### Implementing Modules

| Module | Role |
|--------|------|
| `crates/trusty-agents/src/compress/tool_output/rtk.rs` | `compress_tool_output_async` — the compression function Option 1 would call. Already shipped, already tested (`compress_tool_output_async_falls_back_when_rtk_absent`). |
| `crates/trusty-agents/src/compress/tool_output/mod.rs` | `compress_tool_output` dispatch table — currently missing `grep`/`ls` filter branches (see Spike finding above). |
| `crates/trusty-agents/examples/tool_output_compression_spike.rs` | This investigation's throwaway benchmark; not a permanent API surface. |
| `crates/trusty-mpm/src/mcp/mod.rs` | Pattern reference for how a new `tm_bash`/`tm_read`/`tm_grep` tool group would be wired (`OrchestratorBackend` trait + `dispatch()`), if/when Option 1 is implemented. |

## Decision

**Do not retarget `SPEC-MPM-CUTOVER-03` to the MCP-proxy seam yet.** Conditions under
which retargeting becomes correct:

1. `compress_tool_output`'s dispatch table gains real filters for `grep` and `ls`
   (currently 0% reduction — see Spike), so the "v1 filter granularity" originally
   promised by `SPEC-MPM-CUTOVER-03` §Locked-Decisions actually exists in code before a
   proxy tool advertises those command names.
2. A follow-up spike re-runs with those filters in place and shows aggregate reduction
   materially above the current 9.1%/8.4% (cargo-test/git-diff-only) baseline — a
   reasonable target given rtk/ztk's own README claims (70–90% on structured command
   output) would be 40–60% aggregate across a realistic 4-fixture mix once grep/ls
   filters exist.
3. The security/provenance review in [§ Option 1](#provenance-permission-and-security-tradeoffs)
   is formalized (a short security-review pass, reusing the `security-review` skill
   convention already used elsewhere in this repo for MCP server installs) before any
   proxy tool ships to a real project, even opt-in.

Until then, `SPEC-MPM-CUTOVER-03` remains correctly scoped to the observability-only
`optimize_tool_output` seam it already targets (per the #1944/PR #1952 scope caveat
already in that section) — that work is still valid and shippable; it just doesn't
solve the live-token problem, and both docs must continue to say so explicitly.

## Not Recommended

- **Widening the `tm hook` relay to forward stdin JSON** (`tool_name`/`tool_input`/
  `tool_response`) is still worth doing **for observability** — `HookService::process`
  and `mcp_backend.rs` are already written expecting a richer payload the relay never
  sends, so this is a real, currently-missing capability. **But it does not solve the
  live-token problem described in this doc.** By the time any hook fires — widened
  relay or not — the tool result is already in the model's context. Do not conflate
  "richer hook payload" work with "live token reduction" work in future tickets; file
  them separately so scope stays honest.
- **Shipping `tm_grep`/`tm_ls` proxy tools before filters exist for those command
  domains.** Confirmed 0% reduction in the spike above — pure provenance/permission
  cost with no offsetting benefit.
- **Claiming transparent built-in tool interception.** No known upstream Claude Code
  mechanism lets an MCP server transparently replace/proxy the built-in Bash/Read/Grep
  tools' results before context insertion. Any future doc or ticket that assumes this
  is possible should be corrected to the steering-based UX described in this doc, or
  wait for Option 2.

## Follow-up Implementation Issues

If the conditions in [§ Decision](#decision) are later met, file these (not filed by
this investigation):

1. **Add `grep`/`ls` (and ideally `find`/`rg`) filter branches to
   `compress_tool_output`** in `crates/trusty-agents/src/compress/tool_output/mod.rs`,
   matching the domain-aware filtering pattern already used for `git diff`/`cargo
   test`. Prerequisite for any proxy tool covering those commands to be worth shipping.
2. **Re-run the spike** (`tool_output_compression_spike.rs`, or a successor) after (1)
   lands, and update this doc's numbers.
3. **Hoist `compress::tool_output` into `trusty-agents-common`**, mirroring the
   `OutputStyle` hoist already locked in `SPEC-MPM-CUTOVER-02`, so trusty-mpm can call
   `compress_tool_output_async` without a full `trusty-agents` path dependency.
4. **Implement `tm_bash`/`tm_read`/`tm_grep` MCP tools** in a new
   `crates/trusty-mpm/src/mcp/tools/tool_proxy.rs` module, following the
   `OrchestratorBackend` + `tool()` descriptor pattern in `crates/trusty-mpm/src/mcp/mod.rs`
   and `tools/core.rs`. Reuse the exact execution primitives the built-in tools use —
   no new sandboxing surface.
5. **Add a CLAUDE.md/skill steering snippet** to tm project templates, instructing the
   model to prefer the proxy tools for high-volume-output operations, with the same
   drafting rigor already applied to other steering snippets in this repo.
6. **Security review** of the tool-proxy MCP server before it ships to any real
   project, even opt-in — reuse the `security-review` skill's MCP-install checklist
   (provenance, permission classification, credential exposure) as the starting
   checklist, extended to cover the provenance/audit-trail double-counting concern
   noted in [§ Option 1](#provenance-permission-and-security-tradeoffs).
7. **De-duplicate observability entries** for proxied calls (the same logical tool
   call would otherwise appear twice — once as the MCP tool invocation, once via
   `PostToolUse` — in the dashboard/ring buffer) as part of (4).

## References

- [`mpm-cutover-resume-native-optimization.md` §C `SPEC-MPM-CUTOVER-03`](./mpm-cutover-resume-native-optimization.md#SPEC-MPM-CUTOVER-03~draft) — the spec section this doc's Decision applies to.
- [`token-compression-rtk-ztk.md`](../trusty-agents/research/token-compression-rtk-ztk.md) — background survey of rtk/ztk techniques and current implementation status.
- `crates/trusty-agents/src/compress/tool_output/rtk.rs` — `compress_tool_output_async`.
- `crates/trusty-agents/src/compress/tool_output/mod.rs` — native filter dispatch table.
- `crates/trusty-agents/examples/tool_output_compression_spike.rs` — this doc's spike.
- `crates/trusty-mpm/src/bin/tm/commands/misc.rs` — the `hook()` relay (~line 298).
- `crates/trusty-mpm/src/daemon/services/hook_service.rs` — `HookService::process` (~line 230).
- `crates/trusty-mpm/src/daemon/mcp_backend.rs` — hook-event ingestion (~line 180).
- `crates/trusty-mpm/src/mcp/mod.rs`, `crates/trusty-mpm/src/mcp/tools/core.rs` — MCP server + tool-descriptor pattern reference.
- Issue #1944 (closed), PR #1952 — the prior investigation this doc follows up on.
- Issue #1953 — this investigation.
