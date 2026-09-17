---
spec_refs:
  - id: SPEC-TCMVP-03~draft
    path: docs/specs/DOC-75-tcode-tui-mvp-parity.md
    anchor: SPEC-TCMVP-03~draft
  - id: SPEC-TTUI-04~draft
    path: docs/specs/DOC-50-tcode-tui-claude-code-clone.md
    anchor: SPEC-TTUI-04~draft
---

# DOC-76 — trusty-code TUI vs Claude Code: Interactive-Loop Parity Map

**Status:** Draft
**Spec ID:** `SPEC-TCPARITY-01~draft` … `SPEC-TCPARITY-04~draft` (DOC-76)
**Subsystem:** `trusty-code` — `tcode tui`, `trusty-code-tui` shared REPL crate, `tui_client::engine`
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-09-16
**DOC-N claim:** `DOC-76`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md).
Verified free: `docs/specs/README.md`'s catalog note ("Next free `DOC-N` =
`DOC-76`", recorded 2026-09-16) is current — no file under `docs/specs/**`
other than that note self-labels or claims `DOC-76`, and no open pull request
claims it.
**Builds on:** [DOC-75](./DOC-75-tcode-tui-mvp-parity.md) — the milestone-87
MVP cut this map is drawn from; DOC-75 §4 stays the ordered cut, this document
is the full comparison surface behind it. [DOC-50](./DOC-50-tcode-tui-claude-code-clone.md)
§4 ([`SPEC-TTUI-04~draft`](./DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-04~draft))
— the TUI's own phasing/scope section. Epic
[#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939).
**Milestone:** [trusty-code MVP · Claude Code TUI parity](https://github.com/bobmatnyc/trusty-tools/milestone/87)

---

## 1. Purpose {#SPEC-TCPARITY-01~draft}

This document maps every capability a user meets in Claude Code's interactive
loop against `tcode tui`'s current state, one row per capability, with the
issue tracking the gap. [DOC-75](./DOC-75-tcode-tui-mvp-parity.md) narrows
this map to the ordered milestone-87 cut; it does not repeat the map itself.
A row here that DOC-75 also lists is the same fact, not a second opinion —
this document is the source, DOC-75 the cut drawn from it.

## 2. Parity baseline {#SPEC-TCPARITY-02~draft}

Owner ruling, 2026-09-16: the baseline for an **instruction-adherence**
comparison is trusty-mpm — a `tm`-launched session — not vanilla Claude Code.
Every session this workspace actually runs carries the BASE_PM/BASE-AGENT
instruction stack `tm` provisions; scoring `tcode tui` against vanilla Claude
Code's instruction-following would measure it against a baseline nobody in
this workspace runs. Rows below that compare *user-facing shape* (a tool
registered, a prompt rendered, a keybinding present) use Claude Code as the
UX reference, as before. A row that would compare *instruction adherence*
uses a `tm`-launched session as the baseline instead; no such row is scored
in this revision — none of the gaps in §3 turn on instruction-following, and
this section records the ruling for the first spec or PR that adds one.

## 3. Parity map {#SPEC-TCPARITY-03~draft}

State as of 2026-09-16.

| Capability | Claude Code / baseline behavior | `tcode tui` state | Issue |
|---|---|---|---|
| Default agent identity | Single agent edits directly | Absent — PM delegates, holds no tools (`crates/trusty-code/src/task/executor.rs:477-517`) | [#8184](https://github.com/bobmatnyc/trusty-tools/issues/8184) |
| Read/search/edit tools reachable by the default agent | Present | Closed 2026-09-16 — PM registry now carries `glob`/`grep`/`read_file` | [#4602](https://github.com/bobmatnyc/trusty-tools/issues/4602) (closed) |
| Implicit project binding | A git-repo cwd is the project; a non-repo cwd has no such concept | In progress — open PR [#8230](https://github.com/bobmatnyc/trusty-tools/pull/8230) adds implicit cwd-repository binding via `find_git_root`; a non-repo cwd stays projectless; `$HOME` and any repository enclosing it are refused as a default; `--project <dir>` still binds any directory, `--projectless` opts out | [#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164), PR [#8230](https://github.com/bobmatnyc/trusty-tools/pull/8230) |
| Session id visibility | N/A — Claude Code has no server-mediated session id | PR [#8230](https://github.com/bobmatnyc/trusty-tools/pull/8230): the session id is internal — logged to the daemon log and the session record for recovery, never displayed by default | [#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164) |
| Startup connect line | N/A | PR [#8230](https://github.com/bobmatnyc/trusty-tools/pull/8230): names the bound home directory (or "projectless") and the active workstream, path middle-elided when long | [#8164](https://github.com/bobmatnyc/trusty-tools/issues/8164) |
| Persistent statusline | Present — model, cwd/project, context/cost | Absent. Scope corrected 2026-09-16 to: `⏵⏵ bypass permissions on (shift+tab to cycle) · /tasks to see subagents`, Shift+Tab cycling the permission mode, `/tasks` opening the subagent panel. Session id, cost, and context are **not** part of this scope | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) |
| Subagent panel (`/tasks`) | N/A — Claude Code has no delegation layer | Absent; folded into #8182's scope, not a separate deferral | [#8182](https://github.com/bobmatnyc/trusty-tools/issues/8182) |
| Prompt history recall (up/down) | Present | Absent | [#8181](https://github.com/bobmatnyc/trusty-tools/issues/8181) |
| Session resume | Present | Absent — `engine.rs::setup` always calls `session.create` | [#8185](https://github.com/bobmatnyc/trusty-tools/issues/8185) |
| Tool-call card expand/collapse | Present | Partial — cards render, no expand/collapse | [#4596](https://github.com/bobmatnyc/trusty-tools/issues/4596) |
| Permission prompt (allow once/session/deny) | Present | Present | `crates/trusty-code-tui/src/widgets/permission_prompt.rs` ([#3422](https://github.com/bobmatnyc/trusty-tools/issues/3422)) |
| Streaming output, Ctrl-C turn interrupt | Present | Present | `crates/trusty-code-tui/src/run/mod.rs`, `app/reduce.rs` |

## 4. Explicitly deferred to trusty-agents {#SPEC-TCPARITY-04~draft}

Two capabilities on the original epic slice were retargeted off
`trusty-code` on owner confirmation 2026-09-16 and are out of the
milestone-87 MVP:

- Session management parity with `tm` (list, rename, pause, resume, stop,
  multi-client attach; no tmux) — retargeted to `trusty-agents`.
  [#8208](https://github.com/bobmatnyc/trusty-tools/issues/8208).
- Daemon heartbeat frames carrying liveness/build/task state to connected
  clients — retargeted to `trusty-agents`.
  [#8209](https://github.com/bobmatnyc/trusty-tools/issues/8209).

Neither issue is tracked against `trusty-code` any further; a future
`trusty-code`-side parity gap in this area needs a new issue, not a reopening
of #8208/#8209.

## Related

[DOC-75](./DOC-75-tcode-tui-mvp-parity.md) — the milestone-87 MVP cut drawn
from this map. [DOC-50](./DOC-50-tcode-tui-claude-code-clone.md) — the TUI's
own functional spec. Epic
[#7939](https://github.com/bobmatnyc/trusty-tools/issues/7939) — tracking
issue.
