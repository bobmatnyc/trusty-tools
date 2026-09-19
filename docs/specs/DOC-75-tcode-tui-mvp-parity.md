---
spec_refs:
  - id: SPEC-TTUI-04~draft
    path: docs/specs/DOC-50-tcode-tui-claude-code-clone.md
    anchor: SPEC-TTUI-04~draft
  - id: SPEC-TCPARITY-01~draft
    path: docs/specs/DOC-76-tcode-tui-parity-map.md
    anchor: SPEC-TCPARITY-01~draft
---

# DOC-75 — trusty-code v0.7.0 · Claude Code TUI Parity + PM-Delegated Coding Tasks

**Status:** Draft
**Spec ID:** `SPEC-TCMVP-01~draft` … `SPEC-TCMVP-06~draft` (DOC-75)
**Subsystem:** `trusty-code` — `tcode tui`, `trusty-code-tui` shared REPL crate, `tui_client::engine`, `task::executor` tool registry
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-09-19
**DOC-N claim:** `DOC-75`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md). Verified free: `docs/specs/README.md`'s catalog note ("Next free `DOC-N` = `DOC-75`", recorded 2026-09-11) is current — the only hit for `DOC-75` under `docs/specs/**` was that note itself, and no open pull request claims it.
**Builds on:** [ADR-0063](../adr/0063-tui-is-the-primary-interactive-surface.md) — the TUI is trusty-code's primary interactive surface. [DOC-50](./DOC-50-tcode-tui-claude-code-clone.md) §4 ([`SPEC-TTUI-04~draft`](./DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-04~draft)) — the TUI's own phasing/MVP-scope section, which this document narrows to a milestone cut. [DOC-76](./DOC-76-tcode-tui-parity-map.md) — the researched, file:line-cited parity map this document's cut is drawn from; see it for the underlying comparison, not repeated here. Epic [#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939) — the tracking issue this spec is the record for. [#2063](https://github.com/bobmatnyc/trusty-tools/issues/2063) — daemon hardening the interactive loop depends on.
**Milestone:** [trusty-code v0.7.0 · Claude Code TUI parity + PM-delegated coding tasks](https://github.com/bobmatnyc/trusty-tools/milestone/87)

---

## 1. Purpose {#SPEC-TCMVP-01~draft}

Solo run, one sentence: a user sits in a repo, launches `tcode tui`, asks for
a code change, watches a solo engineer-class agent read, search, edit, and
run commands with permission prompts, and resumes the session tomorrow.

Delegate run, one sentence: the same user launches `tcode tui --delegate`,
asks for a small change, and watches a PM that holds no filesystem tools of
its own route the work through the shared agent roster — research, then
engineer, then qa — ending with the real changed-file list and test output.

This document is the spec of record for milestone
[87](https://github.com/bobmatnyc/trusty-tools/milestone/87) — which
capability gaps close each track, in what order, and what "done" means. It
narrows epic [#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939),
whose own scope note already restricts a broader closed epic
([#3411](https://github.com/bobmatnyc/trusty-tools/issues/3411)) to "the
narrower slice needed for an owner-drivable PM run."

## 2. Design decision: solo agent by default, delegation opt-in and implemented {#SPEC-TCMVP-02~draft}

Claude Code's interactive loop is one agent that reads, edits, runs shell
commands, and asks permission directly — no delegation layer. Before
[#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184),
`tcode tui`'s default path was a PM that never codes: it held
`delegate_to_agent`, `finish_task`, `set_goal`/`clear_goal`, and no
filesystem tools (`crates/trusty-code/src/task/executor.rs:477-517`). A plain
"review this code" prompt broke on that path because the PM's system prompt
claimed `glob`/`grep`/`list_dir` tools it had never registered —
[#4602](https://github.com/bobmatnyc/trusty-tools/issues/4602), closed
2026-09-16.

**Decision:** the interactive session runs the no-delegate solo agent by
default ([#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184),
closed); PM delegation is an opt-in mode — `tcode tui --delegate`, or
`delegate: true` on `session.create`. [#4602](https://github.com/bobmatnyc/trusty-tools/issues/4602) is the evidence for the default:
the delegate-first default could not perform the basic read/edit loop, and
matching Claude Code's shape needs an agent with real tools in hand, not a
router.

**Delegate mode is implemented, not forthcoming.** `--delegate` on
`tcode tui` mints the pre-[#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184) delegating PM
(`crates/trusty-code/src/main.rs:163-173`,
`crates/trusty-code/src/cli/tui.rs:79,90-92`); `session.create`'s `delegate`
param does the same over RPC
(`crates/trusty-code/src/session/protocol.rs:236,306-308`); the PM registry
still carries `delegate_to_agent` whenever `no_delegate` is false
(`crates/trusty-code/src/task/executor.rs:495-502`).

**Owner ruling, 2026-09-19 (session tm-code):** milestone
[87](https://github.com/bobmatnyc/trusty-tools/milestone/87) is retitled
"trusty-code v0.7.0 · Claude Code TUI parity + PM-delegated coding tasks" and
gains a second track: running a simple coding task through the trusty-mpm
workflow shape in `tcode tui --delegate` — a PM that never codes, delegating
to the shared agent roster. This reverses §5's prior exclusion of PM
delegation from the MVP. The solo agent stays the interactive default;
delegation stays opt-in. Ruling comment:
[#7939 (comment)](https://github.com/bobmatnyc/trusty-tools/issues/7939#issuecomment-5741928287).
Ordered delegation cut:
[#7939 (comment)](https://github.com/bobmatnyc/trusty-tools/issues/7939#issuecomment-5741947036).

## 3. Parity target {#SPEC-TCMVP-03~draft}

State as of 2026-09-19. This table is the milestone-relevant slice; the full
comparison it is drawn from, including rows not in this cut, lives in
[DOC-76](./DOC-76-tcode-tui-parity-map.md) §1 (functional zones) and §5
(gaps table).

| Capability | Claude Code behavior | tcode state | Issue |
|---|---|---|---|
| Default agent identity | Single agent edits directly | Present — solo agent, no delegation | [#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184) (closed) |
| Read/search/edit tools reachable by the default agent | Present | Present | [#4602](https://github.com/bobmatnyc/trusty-tools/issues/4602) (closed) |
| Startup identity (project, session, model) | Present | Present | [#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164) (closed) |
| Persistent statusline | Present | Absent. Scope corrected 2026-09-16: statusline + subagent panel (`/tasks`) + shift-tab permission-mode cycle — not statusline alone | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) |
| Prompt history recall (up/down) | Present | Present | [#8181](https://github.com/bobmatnyc/trusty-tools/issues/8181) (closed) |
| Session resume | Present | Absent — `engine.rs::setup` always calls `session.create` | [#8185](https://github.com/bobmatnyc/trusty-tools/issues/8185) |
| Tool-call card expand/collapse | Present | Present | [#4596](https://github.com/bobmatnyc/trusty-tools/issues/4596) (closed) |
| Permission prompt (allow once/session/deny) | Present | Present | `crates/trusty-code-tui/src/widgets/permission_prompt.rs` ([#3422](https://github.com/bobmatnyc/trusty-tools/issues/3422)) |
| Streaming output, Ctrl-C turn interrupt | Present | Present | `crates/trusty-code-tui/src/run/mod.rs`, `app/reduce.rs` |
| PM routes research → engineer → qa (delegate mode) | N/A — Claude Code has no PM router | Absent — `pm`'s routing text does not yet name `research`/`qa` as delegation targets | [#8287](https://github.com/bobmatnyc/trusty-tools/issues/8287) |
| PM cannot write source in delegate mode | N/A | Broken — the ask-gate on write/edit can be bypassed by the delegating PM | [#8288](https://github.com/bobmatnyc/trusty-tools/issues/8288) |
| Todo checklist (delegate mode) | `TodoWrite` populates a visible checklist | Absent — no `TodoWrite`-shaped tool; roster `todos` is always empty | [#8235](https://github.com/bobmatnyc/trusty-tools/issues/8235) |
| Subagent panel | N/A — Claude Code has no PM-router concept to panel | Absent — delegation events render as inline chat-log lines only (`crates/trusty-code-tui/src/app/reduce.rs::apply_delegation_started`/`apply_delegation_finished`), no standing list | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) |
| Structured `finish_task` result | N/A | Partial — the payload carries structured fields but the TUI flattens them to one text blob | [#8204](https://github.com/bobmatnyc/trusty-tools/issues/8204) |
| Evidence in transcript (delegate mode) | N/A | Absent — completion accepts a free-text claim, not the real test-result output | [#8289](https://github.com/bobmatnyc/trusty-tools/issues/8289) |

[#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) and
[#8204](https://github.com/bobmatnyc/trusty-tools/issues/8204) each serve
both tracks: the same subagent panel and the same `finish_task` parsing back
the solo path's tool-card rendering and the delegate path's routing
visibility — see §4's two tables.

## 4. Ordered cut {#SPEC-TCMVP-04~draft}

Two tracks, ordered independently. Closed items are marked closed; open
items follow, parity issues before bugs.

### 4a. Parity track

| # | Item | Size | Status | Note |
|---|---|---|---|---|
| 1 | [#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184) — no-delegate solo agent as default | M | closed | closes §2 directly |
| 2 | [#4602](https://github.com/bobmatnyc/trusty-tools/issues/4602) — tool-prompt/registry parity | S | closed | regression test per the issue's own suggestion |
| 3 | [#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164) — startup identity | S | closed | |
| 4 | [#8181](https://github.com/bobmatnyc/trusty-tools/issues/8181) — prompt history recall | S | closed | |
| 5 | [#4596](https://github.com/bobmatnyc/trusty-tools/issues/4596) — tool-call card expand/collapse | S | closed | |
| 6 | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) — persistent statusline | M | open | corrected scope 2026-09-16: statusline + subagent panel + `/tasks` + shift-tab permission-mode cycle |
| 7 | [#8185](https://github.com/bobmatnyc/trusty-tools/issues/8185) — session resume | M | open | |
| 8 | [#8205](https://github.com/bobmatnyc/trusty-tools/issues/8205) — bind the cwd's repository as the project by default | — | open | PR [#8247](https://github.com/bobmatnyc/trusty-tools/pull/8247) open, `Refs #8205` |
| 9 | [#8203](https://github.com/bobmatnyc/trusty-tools/issues/8203) — refuse or restart when the daemon's build differs from the client's | — | open | |
| 10 | [#8204](https://github.com/bobmatnyc/trusty-tools/issues/8204) — parse structured `finish_task` fields into dedicated slots | — | open | |
| 11 | [#8221](https://github.com/bobmatnyc/trusty-tools/issues/8221) — default workstream named after the project directory | — | open | |
| 12 | [#8222](https://github.com/bobmatnyc/trusty-tools/issues/8222) — multi-line input (Shift+Enter/backslash) and bracketed paste | — | open | |
| 13 | [#8223](https://github.com/bobmatnyc/trusty-tools/issues/8223) — `/` command palette with completion | — | open | |
| 14 | [#8207](https://github.com/bobmatnyc/trusty-tools/issues/8207) — cancel reports cancelled while the daemon task keeps running | — | open | bug |
| 15 | [#8237](https://github.com/bobmatnyc/trusty-tools/issues/8237) — permission scrollback line drops newlines from a multi-line command | — | open | bug |
| 16 | [#8238](https://github.com/bobmatnyc/trusty-tools/issues/8238) — a turn can end with no final assistant message | — | open | bug |
| 17 | [#8240](https://github.com/bobmatnyc/trusty-tools/issues/8240) — keystrokes queued while a turn is in-flight lose newlines | — | open | bug |

### 4b. Delegation track

Ordered per the 2026-09-19 ruling's [delegation cut](https://github.com/bobmatnyc/trusty-tools/issues/7939#issuecomment-5741947036).

| # | Item | Size | Note |
|---|---|---|---|
| 1 | [#8227](https://github.com/bobmatnyc/trusty-tools/issues/8227) — supporting agents (research, documentation, ticketing, version-control) present in the roster and behaving per their cards | — | first: the PM's routing text (row 2) can only name delegation targets that already work |
| 2 | [#8287](https://github.com/bobmatnyc/trusty-tools/issues/8287) — PM routing text names `research` and `qa` as delegation targets | S | depends on row 1 |
| 3 | [#8235](https://github.com/bobmatnyc/trusty-tools/issues/8235) — `TodoWrite`-shaped session checklist tool; roster `todos` populated | — | runs parallel with row 2 |
| 4 | [#8204](https://github.com/bobmatnyc/trusty-tools/issues/8204) — parse structured `finish_task` fields into dedicated slots | — | needed before row 6 can carry test output in a dedicated slot |
| 5 | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) — persistent statusline and subagent panel | M | the panel is where a delegate run's research/engineer/qa steps become visible |
| 6 | [#8289](https://github.com/bobmatnyc/trusty-tools/issues/8289) — `finish_task` verification requires real test-result output in the transcript | S | depends on row 4's structured slots |
| 7 | [#8288](https://github.com/bobmatnyc/trusty-tools/issues/8288) — delegate-mode PM can bypass the ask-gate on write/edit of bound project source | M | closes before the §6 delegate run can claim "the PM never writes source" |
| 8 | [#8201](https://github.com/bobmatnyc/trusty-tools/issues/8201) — native trusty-memory and trusty-search tools registered for the interactive session by default | — | last: brings the delegate session's tool set to parity with the solo path once rows 1–7 land |

## 5. Explicitly out of MVP {#SPEC-TCMVP-05~draft}

PM delegation is no longer on this list — see §2's 2026-09-19 ruling. v0.7.0
extends tcode's own `pm.md` agent card and `delegate_to_agent` tool; it does
not run trusty-mpm's instruction-assembly or delegation pipeline inside
tcode (see [DOC-76](./DOC-76-tcode-tui-parity-map.md) §2 for the two
stacks' differences).

Out of this milestone: the `/model` picker, degraded-terminal fallback,
IDE/hooks/plugins/MCP authoring, cost/billing UI. The subagent panel is not
on this list — it is in scope, folded into
[#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) (§4a row 6,
§4b row 5).
Also out: full issue→PR workflow parity and per-run cost reporting
([#8127](https://github.com/bobmatnyc/trusty-tools/issues/8127)), a shared
instruction/agent/skill catalog between tcode and trusty-mpm
([#2892](https://github.com/bobmatnyc/trusty-tools/issues/2892),
[#2074](https://github.com/bobmatnyc/trusty-tools/issues/2074)), and
agent-class certification
([#7944](https://github.com/bobmatnyc/trusty-tools/issues/7944)).

Two capabilities retargeted off `trusty-code` on owner confirmation
2026-09-16 are also out of this MVP: session management parity with `tm`
(list/rename/pause/resume/multi-client attach, no tmux —
[#8208](https://github.com/bobmatnyc/trusty-tools/issues/8208)) and daemon
heartbeat frames
([#8209](https://github.com/bobmatnyc/trusty-tools/issues/8209)). Both now
track against `trusty-agents`; see
[DOC-76](./DOC-76-tcode-tui-parity-map.md) §4.

## 6. Exit criterion {#SPEC-TCMVP-06~draft}

The milestone closes when every issue in §4 is `status:tested` and closed on
an installed build (`cargo install`, never `cp`, per the workspace macOS
rule), and both §1 runs are recorded against that build:

- **Solo run.** Launch, request a change, watch the solo agent work through
  permission prompts, quit, relaunch, resume.
- **Delegate run.** Launch `tcode tui --delegate`, ask for a small change,
  and watch the PM hand context to `research`, code to `engineer`, and
  verification to `qa` — each step visible in the subagent panel — ending
  with the real changed-file list and test output in the transcript, the PM
  never writing source itself.

## Related

[DOC-76](./DOC-76-tcode-tui-parity-map.md) — the researched parity map this
document's cut is drawn from. [ADR-0063](../adr/0063-tui-is-the-primary-interactive-surface.md)
— TUI as primary surface. [DOC-50](./DOC-50-tcode-tui-claude-code-clone.md) —
the TUI's own functional spec. Epic
[#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939) — tracking
issue. [#2063](https://github.com/bobmatnyc/trusty-tools/issues/2063) —
daemon hardening this milestone assumes.
