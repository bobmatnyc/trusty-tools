---
spec_refs:
  - id: SPEC-TTUI-04~draft
    path: docs/specs/DOC-50-tcode-tui-claude-code-clone.md
    anchor: SPEC-TTUI-04~draft
  - id: SPEC-TCPARITY-01~draft
    path: docs/specs/DOC-76-tcode-tui-parity-map.md
    anchor: SPEC-TCPARITY-01~draft
---

# DOC-75 — trusty-code TUI MVP: Claude Code Interactive-Loop Parity

**Status:** Draft
**Spec ID:** `SPEC-TCMVP-01~draft` … `SPEC-TCMVP-06~draft` (DOC-75)
**Subsystem:** `trusty-code` — `tcode tui`, `trusty-code-tui` shared REPL crate, `tui_client::engine`, `task::executor` tool registry
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-09-16
**DOC-N claim:** `DOC-75`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md). Verified free: `docs/specs/README.md`'s catalog note ("Next free `DOC-N` = `DOC-75`", recorded 2026-09-11) is current — the only hit for `DOC-75` under `docs/specs/**` was that note itself, and no open pull request claims it.
**Builds on:** [ADR-0063](../adr/0063-tui-is-the-primary-interactive-surface.md) — the TUI is trusty-code's primary interactive surface. [DOC-50](./DOC-50-tcode-tui-claude-code-clone.md) §4 ([`SPEC-TTUI-04~draft`](./DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-04~draft)) — the TUI's own phasing/MVP-scope section, which this document narrows to a milestone cut. [DOC-76](./DOC-76-tcode-tui-parity-map.md) — the researched, file:line-cited parity map this document's cut is drawn from; see it for the underlying comparison, not repeated here. Epic [#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939) — the tracking issue this spec is the record for. [#2063](https://github.com/bobmatnyc/trusty-tools/issues/2063) — daemon hardening the interactive loop depends on.
**Milestone:** [trusty-code MVP · Claude Code TUI parity](https://github.com/bobmatnyc/trusty-tools/milestone/87)

---

## 1. Purpose {#SPEC-TCMVP-01~draft}

MVP, one sentence: a user sits in a repo, launches `tcode tui`, asks for a
code change, watches a solo engineer-class agent read, search, edit, and run
commands with permission prompts, and resumes the session tomorrow.

This document is the spec of record for milestone
[87](https://github.com/bobmatnyc/trusty-tools/milestone/87) — which
capability gaps close the MVP, in what order, and what "done" means. It
narrows epic [#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939),
whose own scope note already restricts a broader closed epic (#3411) to "the
narrower slice needed for an owner-drivable PM run."

## 2. Design decision: solo agent by default, delegation opt-in {#SPEC-TCMVP-02~draft}

Claude Code's interactive loop is one agent that reads, edits, runs shell
commands, and asks permission directly — no delegation layer. `tcode tui`'s
default path today is a PM that never codes: it holds `delegate_to_agent`,
`finish_task`, `set_goal`/`clear_goal`, and no filesystem tools
(`crates/trusty-code/src/task/executor.rs:477-517`). A plain "review this
code" prompt broke on this path because the PM's system prompt claimed
`glob`/`grep`/`list_dir` tools it had never registered —
[#4602](https://github.com/bobmatnyc/trusty-tools/issues/4602), closed
2026-09-16.

**Decision:** the interactive session runs the no-delegate solo agent by
default ([#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184)); PM
delegation becomes an opt-in mode — `tcode tui --delegate`, or `delegate:
true` on `session.create` — layered on once the solo path is solid. #4602 is
the evidence for the decision: the delegate-first default could not perform
the basic read/edit loop, and matching Claude Code's shape needs an agent
with real tools in hand, not a router.

## 3. Parity target {#SPEC-TCMVP-03~draft}

State as of 2026-09-16. This table is the MVP-relevant slice; the full
comparison it is drawn from, including rows not in the MVP cut, lives in
[DOC-76](./DOC-76-tcode-tui-parity-map.md) §1 (functional zones) and §5
(gaps table).

| Capability | Claude Code behavior | tcode state | Issue |
|---|---|---|---|
| Default agent identity | Single agent edits directly | Absent — PM delegates, holds no tools | [#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184) |
| Read/search/edit tools reachable by the default agent | Present | Absent — PM registry has no `glob`/`grep`/`read_file` | [#4602](https://github.com/bobmatnyc/trusty-tools/issues/4602) (closed) |
| Startup identity (project, session, model) | Present | Absent — one-line connect message only | [#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164) |
| Persistent statusline | Present | Absent. Scope corrected 2026-09-16: statusline + subagent panel (`/tasks`) + shift-tab permission-mode cycle — not statusline alone | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) |
| Prompt history recall (up/down) | Present | Absent | [#8181](https://github.com/bobmatnyc/trusty-tools/issues/8181) |
| Session resume | Present | Absent — `engine.rs::setup` always calls `session.create` | [#8185](https://github.com/bobmatnyc/trusty-tools/issues/8185) |
| Tool-call card expand/collapse | Present | Partial — cards render, no expand/collapse | [#4596](https://github.com/bobmatnyc/trusty-tools/issues/4596) |
| Permission prompt (allow once/session/deny) | Present | Present | `crates/trusty-code-tui/src/widgets/permission_prompt.rs` ([#3422](https://github.com/bobmatnyc/trusty-tools/issues/3422)) |
| Streaming output, Ctrl-C turn interrupt | Present | Present | `crates/trusty-code-tui/src/run/mod.rs`, `app/reduce.rs` |

## 4. Ordered MVP cut {#SPEC-TCMVP-04~draft}

| # | Item | Size | Note |
|---|---|---|---|
| 1 | [#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184) — no-delegate solo agent as default | M | closes §2 directly |
| 2 | [#4602](https://github.com/bobmatnyc/trusty-tools/issues/4602) — tool-prompt/registry parity | S | closed 2026-09-16; regression test per the issue's own suggestion |
| 3 | [#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164) — startup identity | S | |
| 4 | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) — persistent statusline | M | corrected scope 2026-09-16: statusline + subagent panel + `/tasks` + shift-tab permission-mode cycle |
| 5 | [#8181](https://github.com/bobmatnyc/trusty-tools/issues/8181) — prompt history recall | S | |
| 6 | [#8185](https://github.com/bobmatnyc/trusty-tools/issues/8185) — session resume | M | |
| 7 | [#4596](https://github.com/bobmatnyc/trusty-tools/issues/4596) — tool-call card expand/collapse | S | half of epic slice 3: rendering ships already, expand/collapse remains |

## 5. Explicitly out of MVP {#SPEC-TCMVP-05~draft}

PM delegation, the `/model` picker, degraded-terminal fallback,
IDE/hooks/plugins/MCP authoring, cost/billing UI. The subagent panel is no
longer on this list — it is in scope, folded into #8182 (§4 row 4).

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
rule), and one end-to-end run of the §1 definition sentence — launch, request
a change, watch the solo agent work through permission prompts, quit,
relaunch, resume — is recorded against that build.

## Related

[DOC-76](./DOC-76-tcode-tui-parity-map.md) — the researched parity map this
document's cut is drawn from. [ADR-0063](../adr/0063-tui-is-the-primary-interactive-surface.md)
— TUI as primary surface. [DOC-50](./DOC-50-tcode-tui-claude-code-clone.md) —
the TUI's own functional spec. Epic
[#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939) — tracking
issue. [#2063](https://github.com/bobmatnyc/trusty-tools/issues/2063) —
daemon hardening this milestone assumes.
