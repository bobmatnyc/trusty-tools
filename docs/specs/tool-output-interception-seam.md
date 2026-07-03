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
is the follow-up investigation (#1953) requested to evaluate the viable seams for
reducing a **native Claude Code session's live tool-output tokens**, quantify realistic
savings with a real spike, and decide whether `SPEC-MPM-CUTOVER-03` should retarget to
one of them.

> **⚠️ Revision (2026-07-03, same investigation).** The original version of this doc
> claimed no pre-context-insertion interception seam exists in the Claude Code harness,
> and framed a "future tool-output-transform hook" (old Option 2) as purely
> speculative — "As of this writing no such hook exists." **That claim was incomplete.**
> The sibling `claude-mpm` Python project already ships a working, production-hardened
> seam of exactly this shape, for Bash specifically — see [§ Problem](#problem) and the
> new [§ Option 0](#option-0-pretooluse-bash-command-rewrite-in-tms-own-hook-relay-spec-toolproxy-00draft)
> below. The correction is scoped precisely: the seam is real and shipped **for Bash
> only**; it still does not exist for structured/native tools (Read/Grep/Glob), which
> have no subprocess to rewrite.

**Recommendation (see [§ Decision](#decision)):** do not retarget `SPEC-MPM-CUTOVER-03`
to the full MCP proxy (Option 1) yet. Instead, prototype the cheaper **Option 0** —
`tm hook`'s own `PreToolUse` Bash command-rewrite — first. It has real prior art
(claude-mpm's ztk integration proves the seam works in production), reuses
already-shipped pieces (`tm hook` relay, `compress_tool_output_async`), and needs no
new MCP tools, steering, or tool-provenance change, since Bash still executes as Bash.
Option 1 (the MCP-proxy seam) remains architecturally viable and reuses shipped code,
but the spike shows the *existing* native filter chain only compresses 2 of 4 realistic
fixture types (cargo/git — not grep/ls), so its opt-in, steering-only UX caps realistic
savings well short of the rtk/ztk README numbers until filter coverage is extended, and
it stays the more expensive path (new tool provenance, new consent UX, new MCP surface).
Option 1 remains the right eventual path for Read/Grep/Glob coverage, which Option 0
cannot reach (no subprocess to wrap or pipe for native/structured tools).

## Contents

- [Problem](#problem)
- [Option 0: PreToolUse Bash Command-Rewrite in tm's Own Hook Relay](#option-0-pretooluse-bash-command-rewrite-in-tms-own-hook-relay-spec-toolproxy-00draft)
- [Option 1: tm-Provided MCP Tool-Output Proxy](#option-1-tm-provided-mcp-tool-output-proxy-spec-toolproxy-01draft)
- [Option 2: Future Upstream Claude Code Hook for Structured Tools](#option-2-future-upstream-claude-code-post-execution-transform-hook-readgrepglob)
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
- **ZTK doesn't exist in this workspace** — but a working equivalent exists in a
  sibling project, and it proves the seam is real. No `ztk` symbol appears anywhere
  under `crates/`; the `codejunkie99/ztk` Zig binary itself is not vendored, shelled
  out to, or ported into `trusty-tools`. Treat every "native ztk" reference in
  `SPEC-MPM-CUTOVER-03` as aspirational naming for an in-tree Rust filter tier, not a
  delivered dependency **in this workspace**. However, the sibling Python project
  `claude-mpm` (installed locally at
  `~/.local/share/uv/tools/claude-mpm/lib/python3.13/site-packages/claude_mpm/`) already
  ships a production-hardened `PreToolUse` hook (`hooks/ztk_hook.py`) that, on every
  `Bash` tool call, rewrites `tool_input["command"]` from `<cmd>` to
  `<ztk_path> run <cmd>` and returns the rewrite via
  `hookSpecificOutput.updatedInput` (`_build_ztk_response_impl`, `hooks/ztk_hook.py`
  ~lines 742-813). Claude Code then **executes the rewritten command** — the external
  `ztk` binary wraps the subprocess and filters stdout in-flight, so only the
  already-compressed output is ever captured as the Bash tool's result. This *is* a
  genuine pre-context-insertion seam: compression happens as a side effect of *what
  gets executed*, not post-hoc filtering of output that's already in context. A comment
  in the sibling `hooks/llmlingua_hook.py` (~lines 3-7) makes the same distinction
  explicitly, contrasting itself with ztk: "Unlike ztk — which rewrites Bash *commands*
  via PreToolUse to strip noise before output is generated — LLMLingua operates on
  already-generated *text*." Routing is wired via `hooks/pretooluse_dispatcher.py`
  (~lines 160-179), which dispatches `Bash` events to `ztk_hook.build_ztk_response`; a
  `--no-ztk` CLI flag / `CLAUDE_MPM_DISABLE_ZTK` env var gates it
  (`cli/parsers/run_parser.py` ~263-268, `ztk_hook.py` ~45, ~610-611, ~745-747). **This
  corrects the earlier framing of this doc's old Option 2** ("no pre-context-insertion
  hook exists") — it exists today, for Bash, outside this workspace, with real prior
  art. See [§ Option 0](#option-0-pretooluse-bash-command-rewrite-in-tms-own-hook-relay-spec-toolproxy-00draft)
  for what an equivalent inside `tm hook` would look like, and
  [§ Option 2](#option-2-future-upstream-claude-code-post-execution-transform-hook-readgrepglob)
  for the narrower gap that remains (structured/native tools with no subprocess to
  rewrite).

**The only way to reduce live tokens is to rewrite the tool result *before* the model
consumes it.** For Bash, that seam already exists (see above) and claude-mpm's ztk
integration proves it works in production; [§ Option 0](#option-0-pretooluse-bash-command-rewrite-in-tms-own-hook-relay-spec-toolproxy-00draft)
sketches `tm`'s own equivalent. For everything else, the tool call itself must go
through something other than Claude Code's built-in tool implementation (Option 1), or
Claude Code's harness must ship a post-execution transform hook that runs before
context insertion for those tool types (Option 2, still speculative for structured
tools — see that section for the precise scope).

## Option 0: PreToolUse Bash Command-Rewrite in tm's Own Hook Relay {#SPEC-TOOLPROXY-00~draft}

**ID:** SPEC-TOOLPROXY-00~draft
**Status:** Draft — recommended next prototype (see [§ Decision](#decision))

### Precedent

claude-mpm's ztk integration (cited in full in [§ Problem](#problem)) proves this seam
works in production: rewrite the Bash *command* at `PreToolUse` time so that whatever
subprocess Claude Code spawns already produces filtered output. `tm hook` could do the
same thing without depending on the external ztk Zig binary at all, by piping through a
`tm`-owned compression subcommand instead.

### Behavior Contract (WHAT)

- **Inputs:** A `PreToolUse` hook event for `tool_name == "Bash"`, delivered by Claude
  Code on the hook process's **stdin** as JSON (`tool_name`, `tool_input.command`,
  session/context fields). `tm hook` does not read this today (see Architecture Sketch).
- **Outputs:** `tm hook` responds with `hookSpecificOutput.updatedInput` containing a
  rewritten `command` field. Claude Code substitutes the rewritten command for the
  original before executing it — the model still sees a `Bash` tool call, just with a
  different, tm-mediated command string.
- **Preconditions:** The project has `tm hook` wired in as a `PreToolUse` hook (already
  standard for `tm`-managed sessions). No new opt-in surface beyond what's already
  required to use `tm hook` at all — a meaningful UX simplification versus Option 1.
- **Postconditions:** The Bash subprocess Claude Code actually runs already has its
  output filtered in-flight; the tool result the model reads reflects the compressed
  size, matching Option 1's postcondition but without a new tool name in the
  transcript — the call still shows up as `Bash`.
- **Error conditions:** Same fail-open requirement as everywhere else in this doc and
  as already locked in `SPEC-MPM-CUTOVER-03`'s Locked-Decisions #4 (infallible,
  bounded, in-process where possible). If the rewrite cannot be constructed safely
  (e.g., the command matches an exclusion — see Tradeoffs below), `tm hook` must return
  no `updatedInput` and let the original command run unmodified — a silent no-op, not a
  failure.

### Architecture Sketch

`tm hook`'s relay (`crates/trusty-mpm/src/bin/tm/commands/misc.rs`, `hook()` function,
~lines 298-359) currently **discards stdin entirely** — it reads only the
`CLAUDE_HOOK_EVENT` and `CLAUDE_SESSION_ID` environment variables and POSTs
`{session_id, event, payload: {cwd}}` to the daemon. It never touches stdin, which is
where Claude Code actually delivers `tool_name`/`tool_input` for `PreToolUse` events.
Implementing Option 0 requires:

1. **Read stdin JSON on `PreToolUse` events.** Parse `tool_name` and, when it's
   `"Bash"`, `tool_input.command`.
2. **Decide how to filter.** Two sub-approaches, evaluated for cost/complexity:
   - **(i) Shell out to an external strategy, mirroring ztk's own architecture.**
     Wrap the command with an external compressing proxy binary (analogous to
     `ztk run <cmd>`). Rejected: this re-introduces exactly the external-binary
     dependency `SPEC-MPM-CUTOVER-03`'s Locked-Decision #1 already forbids ("Do not
     shell out to or bundle the external `codejunkie99/ztk` Zig binary... Keep it
     pure-Rust, single-install clean"), and reintroduces the same global-wrap failure
     class that caused the SAM-build incident cited below.
   - **(ii) Ship tm's own output-filtering subcommand and pipe through it.** Since
     RTK/native compression here is Rust-native and can't "wrap" a subprocess the way
     ztk's Zig binary does, have `tm hook` return
     `hookSpecificOutput.updatedInput` with `command` rewritten to
     `<original cmd> | tm compress --tool bash` — i.e., `tm` ships its own filtering
     binary/subcommand, and the pipe is the interception point, achieving the same
     seam as ztk without depending on any external binary. This subcommand would
     reuse `compress_tool_output_async`
     (`crates/trusty-agents/src/compress/tool_output/rtk.rs:84`) as the actual filter
     — already proven reusable and free of `tool_loop` internals (see
     [§ Option 1 Architecture Sketch](#option-1-tm-provided-mcp-tool-output-proxy-spec-toolproxy-01draft)).
     **Recommended sub-approach** — pure-Rust, in-tree, consistent with
     `SPEC-MPM-CUTOVER-03`'s locked decisions, and requires no new external dependency.

Sketch (sub-approach ii):

```rust
// crates/trusty-mpm/src/bin/tm/commands/misc.rs (extend hook())
// On PreToolUse with tool_name == "Bash":
let rewritten = format!("{original_command} | tm compress --tool bash");
Ok(json!({
    "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "updatedInput": { "command": rewritten }
    }
}))
```

### Tradeoffs

- **Shell-pipe composition risk.** Appending `| tm compress --tool bash`
  unconditionally can break commands that use `|` in ways sensitive to pipeline
  structure, subshells, or that rely on a specific exit code from the *last* command
  in a pipeline (`set -o pipefail` interactions), or long-PATH/argv-size edge cases.
  This is the **same class of problem** ztk itself hit and had to defend against: its
  `_ORCHESTRATOR_EXCLUSIONS` set (`hooks/ztk_hook.py` ~lines 49-69) skips wrapping for
  `make`, `sam`, `rake`, `gradle`, `gradlew`, `mvn`, `ant`, `cdk`, `terraform` — build
  orchestrators that spawn long subprocess chains where a global wrap once broke a SAM
  build (exit-2 on E2BIG / long-PATH, looked like success because the orchestrator
  printed partial success before a later stage silently never ran). This exact
  incident is **already cited in this repo** as prior art:
  `docs/specs/mpm-cutover-resume-native-optimization.md:178` and
  `docs/specs/SPEC-INSTALLER-01.md:150` both point to it as the reason trusty-mpm's own
  compression must stay fail-open/in-process/no-shell-out. Option 0 must ship an
  exclusion list **from day one**, not add one reactively after a similar incident —
  there is no excuse for repeating a documented failure mode.
- **Scope limited to Bash only**, same as ztk itself. Read/Grep/Glob remain uncovered —
  there's no subprocess to wrap or pipe for native/structured tools. See
  [§ Option 2](#option-2-future-upstream-claude-code-post-execution-transform-hook-readgrepglob).
- **No new MCP tools, no steering, no provenance change.** The tool name in the
  transcript stays `Bash` — cheaper UX than Option 1, which requires opt-in MCP tool
  registration, CLAUDE.md/skill steering, and a new consent surface.
- **Reuses existing infrastructure rather than standing up a new server.** Extends
  `tm hook`'s existing relay (already wired as a `PreToolUse` hook for every
  `tm`-managed session) instead of a new MCP server process.
- **Requires widening the relay to read stdin JSON** — currently discarded (see
  Architecture Sketch). This is the same widening flagged in
  [§ Not Recommended](#not-recommended) as "worth doing for observability"; Option 0
  reframes it as also being a **prerequisite for live-token reduction**, not merely an
  observability nice-to-have (see that section's revision note).

## Option 1: tm-Provided MCP Tool-Output Proxy {#SPEC-TOOLPROXY-01~draft}

**ID:** SPEC-TOOLPROXY-01~draft
**Status:** Draft — remains the right eventual path for Read/Grep/Glob coverage, which
Option 0 cannot reach (see [§ Decision](#decision) for sequencing).

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

## Option 2: Future Upstream Claude Code Post-Execution Transform Hook (Read/Grep/Glob)

**Scope corrected by this revision.** The original version of this section claimed no
pre-context-insertion hook exists at all — that was incomplete. `PreToolUse` +
`hookSpecificOutput.updatedInput` already exists and already provides a real
pre-context-insertion seam **for Bash**, proven in production by claude-mpm's ztk
integration (see [§ Problem](#problem) and [§ Option 0](#option-0-pretooluse-bash-command-rewrite-in-tms-own-hook-relay-spec-toolproxy-00draft)).
That mechanism works by rewriting the *input* (the command) before execution, which
only helps because Bash's "result" is the stdout/stderr of a subprocess that hasn't run
yet at hook time.

That trick does not generalize to structured/native tools. Read, Grep, and Glob are not
shell invocations — there is no command string to rewrite that would cause their result
to come back pre-filtered; their result is produced directly by Claude Code's own
built-in implementation, not a subprocess `tm hook` could interpose on. For those
tools, what would actually be needed is a hook that runs *after* the tool executes but
*before* the result is inserted into the model's context (as opposed to today's
`PostToolUse`, which runs strictly after context insertion) — a genuine
"transform-the-output" hook, not a "rewrite-the-input" hook. **As of this writing, no
such hook exists in the Claude Code harness for any tool type**, and no comparable
prior art (in claude-mpm or elsewhere) was found during this investigation. If Anthropic
ships one, it could call `compress_tool_output_async` directly with zero
provenance/permission tradeoffs — no new tool names, no steering needed, no proxy
execution surface — and would be strictly better than Option 1 wherever it applies.
This remains genuinely speculative and not actionable now; track the upstream Claude
Code changelog and revisit if/when a post-execution, pre-context-insertion hook ships
for built-in tools generally.

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

**Prototype Option 0 before committing to Option 1.** Option 0 (`tm hook`'s own
`PreToolUse` Bash command-rewrite) is now the recommended next step: it is cheaper (no
new MCP tools, no steering, no tool-provenance change — Bash still executes as Bash,
just with output piped through a filter), reuses proven pieces (`tm hook` relay +
`compress_tool_output_async`), and has real prior art (claude-mpm's ztk) proving the
seam works in practice. **Do not retarget `SPEC-MPM-CUTOVER-03` to the full MCP-proxy
seam (Option 1) yet.** Conditions under which committing to Option 1 becomes correct:

1. The Option 0 prototype spike (see [§ Follow-ups](#follow-up-implementation-issues))
   lands and quantifies real savings/tradeoffs for the Bash-only case, establishing
   whether the pipe-composition risk (see [§ Option 0 Tradeoffs](#tradeoffs)) is
   manageable with a day-one exclusion list.
2. `compress_tool_output`'s dispatch table gains real filters for `grep` and `ls`
   (currently 0% reduction — see Spike), so the "v1 filter granularity" originally
   promised by `SPEC-MPM-CUTOVER-03` §Locked-Decisions actually exists in code before a
   proxy tool advertises those command names. This condition applies to Option 1
   regardless of Option 0's outcome, since Option 0 cannot cover Read/Grep/Glob at all.
3. A follow-up spike re-runs with those filters in place and shows aggregate reduction
   materially above the current 9.1%/8.4% (cargo-test/git-diff-only) baseline — a
   reasonable target given rtk/ztk's own README claims (70–90% on structured command
   output) would be 40–60% aggregate across a realistic 4-fixture mix once grep/ls
   filters exist.
4. The security/provenance review in [§ Option 1](#provenance-permission-and-security-tradeoffs)
   is formalized (a short security-review pass, reusing the `security-review` skill
   convention already used elsewhere in this repo for MCP server installs) before any
   proxy tool ships to a real project, even opt-in.

Until then, `SPEC-MPM-CUTOVER-03` remains correctly scoped to the observability-only
`optimize_tool_output` seam it already targets (per the #1944/PR #1952 scope caveat
already in that section) — that work is still valid and shippable, and the Option 0
prototype (if it lands) would be tracked as a new, separate seam rather than folded
into `SPEC-MPM-CUTOVER-03`'s existing scope. Both docs must continue to be explicit
about which seam(s) are actually wired up at any given time.

## Not Recommended

- **~~Widening the `tm hook` relay to forward stdin JSON does not solve the live-token
  problem~~ — corrected by this revision.** The original text here claimed the relay
  widening (`tool_name`/`tool_input`/`tool_response` on stdin) was "worth doing for
  observability" but "does not solve the live-token problem... by the time any hook
  fires... the tool result is already in the model's context." That was true of
  `PostToolUse` but **incomplete as a blanket statement about hooks in general**:
  `PreToolUse` fires *before* execution, so reading its stdin payload and returning
  `hookSpecificOutput.updatedInput` (Option 0) *does* affect the live copy for Bash.
  The relay widening is still needed for richer `PostToolUse` observability *and* is
  now also a **direct prerequisite for Option 0** — don't read this bullet as license to
  skip stdin-reading work; read [§ Option 0](#option-0-pretooluse-bash-command-rewrite-in-tms-own-hook-relay-spec-toolproxy-00draft)
  instead.
- **Shipping `tm_grep`/`tm_ls` proxy tools before filters exist for those command
  domains.** Confirmed 0% reduction in the spike above — pure provenance/permission
  cost with no offsetting benefit. Applies to Option 1; also applies to any future
  Option 0 extension that tried to cover grep/ls via a Bash-invoked `grep`/`ls` command
  (the underlying dispatch-table gap is the same).
- **Claiming transparent built-in tool interception for Read/Grep/Glob.** No known
  upstream Claude Code mechanism lets an MCP server, or a `PreToolUse` command-rewrite,
  transparently replace/proxy those built-in tools' results before context insertion —
  Option 0's command-rewrite trick is Bash-specific because Bash is the only built-in
  tool whose "result" is generated by a subprocess with a rewritable invocation. Any
  future doc or ticket that assumes transparent interception is possible for
  Read/Grep/Glob should be corrected to the steering-based UX described in Option 1, or
  wait for Option 2.

## Follow-up Implementation Issues

If the conditions in [§ Decision](#decision) are later met, file these (not filed by
this investigation):

0. **Prototype `tm hook` `PreToolUse` Bash command-rewrite (Option 0) as a spike.**
   Widen the relay to read stdin JSON on `PreToolUse` events (see
   [§ Option 0 Architecture Sketch](#architecture-sketch)), implement a `tm compress
   --tool bash` subcommand wrapping `compress_tool_output_async`, rewrite the command
   to pipe through it, and ship a day-one exclusion list for orchestrator commands
   (`make`/`sam`/`rake`/`gradle`/`gradlew`/`mvn`/`ant`/`cdk`/`terraform` — mirroring
   ztk's `_ORCHESTRATOR_EXCLUSIONS`). Measure real savings and pipe-composition
   breakage rate before deciding whether to productionize. **This is the recommended
   near-term follow-up, ahead of the Option 1 items below.**
1. **Add `grep`/`ls` (and ideally `find`/`rg`) filter branches to
   `compress_tool_output`** in `crates/trusty-agents/src/compress/tool_output/mod.rs`,
   matching the domain-aware filtering pattern already used for `git diff`/`cargo
   test`. Prerequisite for any proxy tool (Option 1) — or any Option 0 extension
   covering these commands — to be worth shipping.
2. **Re-run the spike** (`tool_output_compression_spike.rs`, or a successor) after (1)
   lands, and update this doc's numbers.
3. **Hoist `compress::tool_output` into `trusty-agents-common`**, mirroring the
   `OutputStyle` hoist already locked in `SPEC-MPM-CUTOVER-02`, so trusty-mpm can call
   `compress_tool_output_async` without a full `trusty-agents` path dependency. Needed
   by both Option 0 (the `tm compress` subcommand) and Option 1.
4. **(Deprioritized — was next, now sequenced after the Option 0 spike.) Implement
   `tm_bash`/`tm_read`/`tm_grep` MCP tools** in a new
   `crates/trusty-mpm/src/mcp/tools/tool_proxy.rs` module, following the
   `OrchestratorBackend` + `tool()` descriptor pattern in `crates/trusty-mpm/src/mcp/mod.rs`
   and `tools/core.rs`. Reuse the exact execution primitives the built-in tools use —
   no new sandboxing surface. Only worth filing once (0) shows Option 0 alone isn't
   sufficient for Bash coverage, and this is still required for Read/Grep/Glob
   regardless.
5. **(Deprioritized, same reason as (4).) Add a CLAUDE.md/skill steering snippet** to
   tm project templates, instructing the model to prefer the proxy tools for
   high-volume-output operations, with the same drafting rigor already applied to
   other steering snippets in this repo. Only needed for Option 1 — Option 0 needs no
   steering.
6. **Security review** of whichever seam ships first — the tool-proxy MCP server
   (Option 1) or the `PreToolUse` Bash rewrite (Option 0) — before it reaches any real
   project, even opt-in. Reuse the `security-review` skill's MCP-install checklist
   (provenance, permission classification, credential exposure) as the starting
   checklist; for Option 1, extend it to cover the provenance/audit-trail
   double-counting concern noted in [§ Option 1](#provenance-permission-and-security-tradeoffs).
   For Option 0, extend it to cover the shell-pipe composition risk noted in
   [§ Option 0 Tradeoffs](#tradeoffs).
7. **De-duplicate observability entries** for proxied calls under Option 1 (the same
   logical tool call would otherwise appear twice — once as the MCP tool invocation,
   once via `PostToolUse` — in the dashboard/ring buffer) as part of (4). Not
   applicable to Option 0, since the tool name stays `Bash` and there's no second
   invocation to de-duplicate.
8. **Update `docs/trusty-agents/research/token-compression-rtk-ztk.md`** to mention
   ztk's `PreToolUse` invocation mechanism (see [§ References](#references)) — that doc
   currently covers only ztk's internal Zig-side filtering techniques (comptime filter
   dispatch, TTL session cache, stderr routing, etc.), not *how ztk gets invoked* in the
   harness that runs it. This doc's [§ Problem](#problem) and
   [§ Option 0](#option-0-pretooluse-bash-command-rewrite-in-tms-own-hook-relay-spec-toolproxy-00draft)
   sections are the authoritative source for that mechanism until this follow-up lands.

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
- **claude-mpm (sibling project, external to this repo)** — local install path
  `~/.local/share/uv/tools/claude-mpm/lib/python3.13/site-packages/claude_mpm/`. Cited
  as prior art for [§ Option 0](#option-0-pretooluse-bash-command-rewrite-in-tms-own-hook-relay-spec-toolproxy-00draft),
  not as a `trusty-tools` dependency:
  - `hooks/ztk_hook.py` — `build_ztk_response` / `_build_ztk_response_impl`
    (~lines 717-813) — the `PreToolUse` Bash command-rewrite; `_ORCHESTRATOR_EXCLUSIONS`
    (~lines 49-69); `verify_ztk_binary` self-test sentinel round-trip (~lines 188-239).
  - `hooks/llmlingua_hook.py` (~lines 3-7) — comment contrasting ztk's PreToolUse
    command-rewrite with LLMLingua's PostToolUse text-transform.
  - `hooks/pretooluse_dispatcher.py` (~lines 160-179) — routes `Bash` events to
    `ztk_hook.build_ztk_response`.
  - `cli/parsers/run_parser.py` (~lines 260-268) — `--no-ztk` / `--ztk` CLI flags.
- `docs/trusty-agents/research/token-compression-rtk-ztk.md` — background survey; per
  [§ Follow-ups](#follow-up-implementation-issues) item 8, still needs its own update to
  cover ztk's *invocation* mechanism (this doc's Option 0/Problem sections), not just
  its internal Zig-side filtering techniques (which is all that doc currently covers).
- `docs/specs/mpm-cutover-resume-native-optimization.md:178` and
  `docs/specs/SPEC-INSTALLER-01.md:150` — existing internal citations of the ztk
  SAM-build incident, used as prior art in [§ Option 0 Tradeoffs](#tradeoffs).
