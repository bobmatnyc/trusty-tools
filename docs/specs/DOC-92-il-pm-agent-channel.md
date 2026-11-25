# DOC-92 — Intent Ledger as the PM-agent channel, for every harness

**Status:** Approved (r2, 2026-11-25)
**Subsystem:** trusty-mpm — `tm hook`, daemon delegation tracker and ledger methods, `tm dbatg`, runtime adapters (`runtime/`); trusty-code (`tcode`) — `delegate_to_agent`, `finish_task`, search and memory clients; trusty-architect — `ledger` (dbatg) model and writer; instructions (`content/agents/BASE-AGENT.md`, `content/skills/tm-delegation-patterns.md`, PM instructions, generated Codex and tcode content)
**Owner:** owner
**Last-updated:** 2026-11-25
**Spec ID:** `SPEC-ILCHAN-01~approved` … `SPEC-ILCHAN-11~approved` (DOC-92); `SPEC-ILCHAN-12~draft` (§12, Phase A)
**DOC-N claim:** `DOC-92`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). The catalog's next-free note on `origin/main` at `fc2dae714c` names `DOC-92`; no file or open PR claims it.
**Builds on:** #11674 / PR #11682, [DOC-82](./DOC-82-portfolio-intent-ledger-dashboard.md), [DOC-89](./DOC-89-compact-handback-v0.md), [DOC-87](./DOC-87-typed-owner-constraints.md), [DOC-81](./DOC-81-multi-runtime-launcher.md) (runtime adapter bundle, Codex §3.2 and §6 B), [ADR-0059](../adr/0059-canonical-agent-behavior-has-generated-host-adapters.md) (content adapters), #11623, #11626, #11625 (native UDS memory and search).

Decision: the Intent Ledger (IL) is the standard PM-agent channel in both directions. The PM writes the brief into the agent's IL task. The agent reads it there and writes its result back as an IL entry. The PM acts on the entry. Chat carries a one-line pointer.
Decision: mpm is the meta harness for every session type: Claude Code, Codex, and trusty-code (`tcode`).

Requirement ids are `IC-n`. "MUST" marks a testable requirement. UNCONFIRMED marks a third-party behaviour no source or capture has proven.

## 1. Goal and non-goals {#SPEC-ILCHAN-01~approved}

**ID:** SPEC-ILCHAN-01~approved

Goal:
- IC-1. The brief for a dispatched agent lives in the PM's ledger as fields of one task.
- IC-2. Each agent run's result lives in the PM's ledger as one result entry bound to that task and dispatch.
- IC-3. A result is never lost. When the write fails, the result is kept in a named spill file and the PM is told why and where in the same turn.
- IC-4. The PM decides its next action from the result entry.
- IC-5. The task, the prompt pointer, the result block, and the write contract are the same for every harness. Only result capture differs, through one adapter per harness.
- IC-6. A Codex or tcode session launched by tm gets the standard instructions, skills, and agents, and uses native memory and search over UDS.

Non-goals:
- Agents get no general ledger write access.
- No change to `il_auto_apply` (#11505).
- No change to the Architect's datg IL in phases 1-2.
- No hook rewrites a dispatch prompt.
- OpenCode, Gemini CLI, and Cursor (DOC-81 C, D; #7342).
- Session-to-session relays.

## 2. Today {#SPEC-ILCHAN-02~approved}

**ID:** SPEC-ILCHAN-02~approved

- **Claude Code.** #11674 moves the task id only. No agent reads a brief from the ledger or writes a result to it. The dbatg `Task` has no `done_when`, `pct`, `remaining`, or `constraints`.
- **Codex.** tm cannot launch Codex. `RuntimeKind` has `ClaudeCode` and `Tcode` only (`crates/trusty-mpm/src/runtime/mod.rs:197-204`), and #7343 (Codex adapter) was closed as idle. This checkout has a hand-made Codex install from trusty-mpm 1.5.24 (`.codex/config.toml` `developer_instructions`, 37 `.codex/agents/*.toml`, `SessionStart`/`SubagentStart` hooks in `.codex/hooks.json`), git-ignored and marked advisory pending #7344. tm uses `trusty-common::codex_config` only to register MCP servers in `~/.codex/config.toml`. Codex documents `SubagentStop` with `agent_id`, `agent_type`, `agent_transcript_path`, `last_assistant_message`, and `decision: block` to continue, and `PreToolUse` matching `spawn_agent` (Codex hooks docs). Multi-agent is an experimental flag. Hooks outside the managed set wait for `/hooks` trust review (DOC-81 §3.2).
- **tcode.** tm launches `tcode run-task engineer <task> --project <cwd>` with `TM_MANAGED_SESSION_ID` (`crates/trusty-mpm/src/runtime/tcode.rs:161-168`), a one-shot run with no PM instructions. tcode's own PM dispatches through `delegate_to_agent` (`crates/trusty-code/src/tools/delegate.rs`). A sub-agent can end with the schema-checked `finish_task` call (`tools/finish_task.rs`). Memory uses the trusty-memory UDS socket (`session/memory_sink.rs`, `tools/recall_session.rs`). Search spawns `trusty-search serve` as a stdio MCP child (`tools/trusty_search.rs`), not native UDS. Agents load from `.trusty-code/agents/` and `.claude/agents/`. ADR-0059's generated content adapters are not started.
- **Native memory and search.** #11623 moved Claude Code mpm sessions to native UDS commands. #11625 (21 MCP-only verbs, must close before 2.0) is open.

## 3. Harness-neutral core and flow {#SPEC-ILCHAN-03~approved}

**ID:** SPEC-ILCHAN-03~approved

These parts are the same for every harness:
- the dbatg task with brief fields (§6.1);
- the 2-line prompt pointer:
  ```
  Task: <id>@MMDD
  IL: tm dbatg brief <id>
  ```
- the fenced `il` result block, then the last line `Task: <id>@MMDD | done: <text>` (§6.2);
- one daemon method, `ledger.result.submit`, that parses the block, checks the binding, writes the entry, and answers `written`, `spilled`, or `rejected` (§6.3);
- one spill directory and one `tm dbatg replay` (§5).

Flow:
1. **PM writes the brief.** `tm dbatg add` (kind task) with `--done-when` and optional `--constraint`. The store allocates `<code>-T<nn>`.
2. **PM dispatches with the pointer.** `tm dbatg dispatch <id>` prints the pointer. The dispatch text is the pointer plus at most one line.
3. **Adapter binds.** At dispatch the adapter sends `ledger.dispatch.bind {harness, run_key, task, session}`. The daemon checks that the task exists, is `open` or `running`, and has `done_when`. It sets `running` and `to=<agent type>`, and records the absolute store path.
4. **Agent reads the brief.** `tm dbatg brief <id>` resolves the store from the code registry (`<data dir>/dbatg/codes.json`), not from cwd. It prints the title, goal `finish`, `done_when`, constraints, the last 2 results, and the block template.
5. **Agent ends with the block and the pointer line.** Long evidence goes to a file named in `refs`.
6. **Adapter captures and submits.** The adapter sends the raw final message and the binding key to `ledger.result.submit`. The agent never writes the store.
7. **PM is notified** in the harness's own way (§4): one line, `IL <id> r<n>: [STATUS] pct <p> — written`, or the failure line.
8. **PM acts** with `tm dbatg close <id> --from-result r<n>`, a resume, or a new dispatch, and cites the entry id.

Chat cost: every harness returns the sub-agent's final message to the PM, so the block (at most 12 lines) and the pointer line reach chat (approved decision 5).

Resume: a resumed agent keeps its `agent_id`, so the binding still applies. The resume text MUST start with the same `Task:` line. Each resumed stop writes the next entry on the same task.

## 4. Capture adapters {#SPEC-ILCHAN-04~approved}

**ID:** SPEC-ILCHAN-04~approved

Every adapter MUST do the following:
- bind at dispatch;
- submit once per sub-agent stop, with `request_id` (§6.3);
- on a `spilled` or `rejected` answer, or an unreachable daemon, show the PM one line naming the cause and the spill path;
- block or continue the sub-agent once when the block is missing (where the harness allows it).

| | Claude Code (now) | Codex | tcode |
|---|---|---|---|
| Dispatch event | PreToolUse(Agent/Task) `tm hook` (`on_dispatch_locked`) | PreToolUse matcher `spawn_agent` through `tm hook --runtime codex`; the prompt field name is UNCONFIRMED (capture item X1) | `delegate_to_agent` executor, in process, before the runner starts |
| `run_key` | `tool_use_id` | `agent_id` (no tool-use id in Codex docs) | tcode delegation id |
| Capture event | SubagentStop `last_assistant_message` | SubagentStop `last_assistant_message` | runner return in `delegate_to_agent`: the sub-agent's final text, or `finish_task` `summary` |
| Missing block | `decision: block` once (marker, #11674) | `decision: block` once; Codex turns the reason into a continuation | append one retry turn with the block template, once |
| PM notice | SubagentStop `systemMessage` | SubagentStop `systemMessage`; rendering UNCONFIRMED (X2) | one line appended to the `ToolResult` the PM receives |
| Resume | SendMessage | `send_input` / `resume_agent`, UNCONFIRMED (X3) | re-delegation to the same agent name with the same pointer |
| Write path | `tm hook` → daemon UDS; in-process writer if the daemon is down | same as Claude Code | tcode → tm daemon UDS (`trusty-common` `uds`); tcode links no ledger writer |
| Precondition | hook installed | managed `CODEX_HOME` with hooks pre-registered and trusted; multi-agent flag on | tcode launched by tm with `TM_MANAGED_SESSION_ID` |

A Codex session whose hooks are inactive (DOC-81 B6) MUST report `IL capture inactive` at launch. Its dispatches MUST NOT be bound, so no result can be expected and then silently lost.

tcode writes the IL natively: its PM loop owns both the dispatch and the return, so no hook is involved.

## 5. Failure behaviour (all adapters) {#SPEC-ILCHAN-05~approved}

**ID:** SPEC-ILCHAN-05~approved

Every failure writes a spill file `<data dir>/dbatg/spill/<session>/<harness>-<run_key>-<seq>.json`, holding the raw submit payload, and shows the PM one line. The block is also in chat, so the PM can act in the same turn. `tm dbatg replay` re-submits spills and is idempotent.

| Case | Behaviour |
|---|---|
| Dispatch without pointer | Phases 1-2: warn, unbound, no entry. Phase 3: the adapter refuses the dispatch with the remedy `tm dbatg add ... && tm dbatg dispatch <id>`. |
| Unknown, closed, or `done_when`-less task | Warn (phases 1-2) or refuse (phase 3), naming the task and the missing field. |
| Agent cannot read the brief | Agent ends `done: [BLOCKED] brief unreadable: <error>`, `pct: 0`; the entry is written. |
| No block, or a malformed block | Continue the agent once. On the next stop write status `UNPARSED`, keep `pct`, and spill the raw message. |
| Wrong task, or a write field (`new`, `remind`, `close`, `decided`) | Reject the write fields. On a task mismatch, write nothing to the named task, spill, set verdict `mismatch`. |
| Store missing | Spill; `IL write failed: no store`. |
| Store locked (writer exit 3) | Retry once, then queue to `outbox.jsonl` (#9760) and spill; `IL write queued: lock`. |
| `state.json` over 16 KB | Append the entry to `log.jsonl`; skip the row update; `IL row not updated: store over 16 KB — prune`. |
| Entry over cap | Truncate to the caps, set `truncated: true`, spill the full message. |
| Daemon unreachable | Claude Code and Codex: in-process writer, else spill. tcode: spill (tcode has no writer). |
| Codex hooks inactive | Session-level `IL capture inactive` at launch; no binding (§4). |

No path drops a result without a spill file and a PM-visible line (AC-6).

## 6. Schemas {#SPEC-ILCHAN-06~approved}

**ID:** SPEC-ILCHAN-06~approved

### 6.1 Brief (typed fields on the dbatg `Task`)

| Field | Type, cap | Required | Note |
|---|---|---|---|
| `id`, `goal`, `title`, `status`, `to` | existing | yes | `running` means dispatched |
| `done_when` | one line, ≤300 chars | for dispatch | same cap as datg |
| `constraints` | ≤5 entries, each ≤240 chars | no | DOC-87 ref `C:<id>` or one line of text |
| `pct`, `remaining`, `last_result`, `runs` | int 0-100; ≤240; `r<n>`; int | no | copied from the latest entry |

### 6.2 Result block (agent) and entry (store, `log.jsonl`, `kind: result`)

The block uses the ILIO packet grammar: field order and the 240-character line cap.

```il
task: <id>@MMDD
done: [VERIFIED|NEEDS-ATTENTION|BLOCKED] <what is true now>
pct: <0-100>
remaining: <what is left, or none>
evidence: <command => verbatim final line, or file:line>   (1-4 lines)
open: <question | owner=<who>, or none>                     (0-2 lines)
next: <the PM's next action as the agent sees it>
refs: <path to long evidence>                               (0-2 lines)
```

Entry fields: `kind`, `result_id` (`r<n>` per task), `task`, `harness` (`claude-code|codex|tcode`), `run_key`, `agent_id`, `agent_type`, `seq`, `at`, `status`, `done`, `pct`, `remaining`, `evidence[]`, `open[]`, `next`, `refs[]`, `verdict` (ok/missing/mismatch/unbound), `request_id`, `truncated`, `spill`. Block cap 12 lines; entry cap 4 KB.

### 6.3 Binding and daemon methods

- Binding: `{harness, run_key, agent_id, task, store_path, session, dispatched_at}`. It extends `Delegation.ledger_task` and `StopBinding`.
- `ledger.dispatch.bind`, `ledger.result.submit {harness, run_key, agent_id, agent_type, final_message, request_id}`, `ledger.result.list`.
- `request_id = <harness>:<agent_id or run_key>:<seq>:<sha256(block)[0..12]>`.

## 7. Codex and tcode sessions launched by tm {#SPEC-ILCHAN-07~approved}

**ID:** SPEC-ILCHAN-07~approved

| Need | Codex today | Codex adds | tcode today | tcode adds |
|---|---|---|---|---|
| Launch | none in tm | `RuntimeKind::Codex`, the DOC-81 B1, B3, B5, B6 subset: `tm session new --runtime codex`, per-launch `CODEX_HOME`, context through a generated `AGENTS.override.md` | `tcode run-task engineer` one-shot (`tm session new --runtime tcode`) | An interactive PM mode, built in Phase A (§12) |
| Instructions | hand-made `developer_instructions` (1.5.24) | generated from the canonical PM and BASE-AGENT text into `CODEX_HOME` and `AGENTS.override.md` at each launch, with a version stamp | tcode's embedded preamble | the same canonical text, generated into `.trusty-code/` on the test branch |
| Agents | hand-made `.codex/agents/*.toml` | generated `CODEX_HOME/agents/*.toml` | `.claude/agents/`, `.trusty-code/agents/` | generated `.trusty-code/agents/` |
| Skills | hand-made copies | generated Agent Skills `SKILL.md` set | `.trusty-code/skill-refs` | generated from the same source |
| Hooks | `SessionStart`, `SubagentStart` (hand-made) | `tm hook --runtime codex` for PreToolUse(`spawn_agent`), SubagentStop, UserPromptSubmit (turn stamp), pre-trusted in the managed home | n/a (in process) | n/a |
| Memory | MCP registration only | instructions and hooks name native `tm memory` verbs over UDS; MCP stays for non-tm callers | UDS (native) | none |
| Search | MCP registration only | native `tm search` / `trusty-search` CLI over UDS | stdio MCP child | trusty-search UDS client from `trusty-common` |
| IL | none | §4 Codex adapter | none | §4 tcode adapter |

All generated content comes from one canonical source per ADR-0059. The launcher writes only under the per-launch home and `.trusty-code/`; the operator's `~/.codex` is byte-identical before and after a launch (DOC-81 B5).

## 8. Decisions and open questions {#SPEC-ILCHAN-08~approved}

**ID:** SPEC-ILCHAN-08~approved

Approved 2026-11-25 (r1 decisions 1-6):
1. dbatg is the store of record.
2. The brief is fields on the Task row; #9759's brief role is not needed.
3. Constraints are typed, ≤5 × 240; DOC-87 refs where an owner constraint exists.
4. G0b is a narrow grant: the harness appends a result and updates `pct`, `remaining`, `last_result`, `runs` on the bound task only. `il_auto_apply` is unchanged; the `il_agent.py` and `datg_store.py` RULES say "the harness records your result block".
5. Block plus pointer in chat; a file-drop channel is reconsidered in phase 3.
6. For a bound dispatch the result block replaces `## Verification Results` (DOC-89 successor).

Kept from r1 (no decision needed): the PM allocates ids; results append to `log.jsonl` with the latest values copied to the row; resume writes the next entry; no session-record fields, so #11498 does not block; an agent's identity is its binding; depth is 1.

New in r2:
- **Parser location.** The block is parsed once, in the daemon (`ledger.result.submit`). Adapters send raw text, so the three harnesses cannot disagree on the grammar.
- **tcode write path.** tcode submits through the tm daemon over UDS and links no ledger writer, which keeps one writer per store.
- **tcode completion.** The block may sit in `finish_task`'s `summary` or in the final text; `finish_task` gains no IL fields. One wire format for all harnesses.
Approved 2026-11-25 (r2 decisions 7-9):
7. **Codex multi-agent.** tm-launched Codex sessions turn on the experimental multi-agent flag so `spawn_agent` exists, on the test branch `test/il-multi-harness` only.
8. **Codex launch scope.** #7343 stays closed. The Codex launch subset (DOC-81 B1, B3, B5, B6) is tracked under #11819. The rest of DOC-81 B (permission mapping, pane cues) stays in the backlog.
9. **Generated content.** A narrow generator for Codex and tcode (instructions, agents, skills) from the existing canonical files, on the test branch only. It does not wait for ADR-0059's full canonical-source migration. The hand-made `.codex/` install stays live and untouched until after 2.0.

Approved 2026-11-25 (decision 10):
10. **Interactive tcode PM mode.** tm launches `tcode run-task` one-shot today. The interactive PM mode is in scope as Phase A (§12). No Phase A build starts until the Phase A plan is approved.

Open captures (UNCONFIRMED until a dated capture on the designated test host):
- X1: the Codex PreToolUse `spawn_agent` input field that holds the prompt.
- X2: how Codex shows a SubagentStop `systemMessage` to the parent.
- X3: Codex resume verbs (`send_input` / `resume_agent`) and whether `agent_id` survives them.

## 9. Surface changes (names only) {#SPEC-ILCHAN-09~approved}

**ID:** SPEC-ILCHAN-09~approved

- **CLI `tm dbatg`:** `add`/`update` `--done-when`, `--constraint`; `dispatch`; `brief`; `results`; `close --from-result`; `ack` (phase 3); `replay`.
- **CLI `tm session new --runtime codex|tcode`:** launch as in §7. `tm hook --runtime codex` normalises Codex payloads into the existing hook handlers.
- **MCP `dbatg`:** read ops `brief`, `results` only.
- **Daemon:** `ledger.dispatch.bind`, `ledger.result.submit`, `ledger.result.list`; a binding store keyed by `(harness, run_key)` and `agent_id`.
- **Hooks (Claude Code, Codex):**
  - PreToolUse: validate the pointer and bind.
  - SubagentStop: submit.
  - UserPromptSubmit: unread results line (phase 3).
  - `il_auto_apply`: unchanged.
  - `datg.py check_dispatch`: retired for dbatg PMs.
- **tcode:** `delegate_to_agent` binds and submits; the trusty-search client moves to UDS. The interactive PM session mode is Phase A scope (§12).
- **Instructions:**
  - BASE-AGENT section "Intent Ledger brief and result".
  - PM instructions (#9763 phase).
  - `tm-delegation-patterns` (pointer, resume).
  - `il_agent.py` and `datg_store.py` RULES.
  - Generated Codex and tcode copies of all of the above.

## 10. Acceptance criteria {#SPEC-ILCHAN-10~approved}

**ID:** SPEC-ILCHAN-10~approved

Core (each harness runs the same fixtures through its adapter):
- AC-1. `tm dbatg add` stores typed `done_when` and `constraints`; it refuses over-cap values and names the cap.
- AC-2. `tm dbatg brief <id>` prints the same output from a worktree as from the main checkout.
- AC-3. A dispatch carrying the pointer sets the task `running` and records a binding with `harness` and `run_key`.
- AC-4. A stop whose message holds a valid block writes exactly one entry with every §6.2 field and updates the four row fields.
- AC-5. The same stop submitted twice writes one entry.
- AC-6. For each §5 row, a test injects the failure and asserts: a spill file exists; the PM-visible line names it; `replay` then writes exactly one entry.
- AC-7. A block with `close`, `new`, `remind`, or `decided` changes no task status.
- AC-8. A resume of the same agent writes `r<n+1>`, `seq` 2, on the same task.
- AC-9. PostToolUse(Agent) with the same block writes nothing.
- AC-10. A live round trip on tm-dogfood (Claude Code): add → dispatch → brief → block → entry → `close --from-result`, with verdict `ok`.

Codex and tcode (test branch):
- AC-11. The block parser exists once: a grep finds no IL block parser in `tm hook`, the Codex adapter, or tcode outside the daemon method.
- AC-12. In a tcode PM session (`session.create {mode: "pm"}`, driven by `session.send`), a `delegate_to_agent` dispatch carrying the pointer writes one entry through the tm daemon. With the daemon stopped, it writes a spill file and the PM's next turn names it.
- AC-13. Codex: `tm session new --runtime codex` starts Codex with generated instructions, agents, and skills under a per-launch `CODEX_HOME`. `~/.codex` is byte-identical before and after.
- AC-14. Codex: the managed hooks are active at launch, or the launcher prints `IL capture inactive` and binds nothing.
- AC-15. Codex: a `spawn_agent` dispatch with the pointer writes one entry from SubagentStop.
- AC-16. A Codex session and a tcode session each run `tm memory recall` and a native search over UDS, with no trusty-memory or trusty-search MCP server in the session's config.
- AC-17. Live check on the designated test host for AC-12 to AC-16, with dated captures for X1-X3.
- AC-18. Phase 3: a dispatch without a pointer from a PM in refuse mode is refused with the remedy text.

## 11. Rollout and delivery {#SPEC-ILCHAN-11~approved}

**ID:** SPEC-ILCHAN-11~approved

| Phase | Scope | Where | Rung | ACs |
|---|---|---|---|---|
| 0 | Align the id regex (#11135). Decisions 1-10 are approved. | — | 1 | — |
| 1 | One PM (tm-dogfood), Claude Code. Brief fields, `dispatch`, `brief`, `results`, `replay`, daemon bind/submit, SubagentStop adapter, spill. Agent instructions ship in the `brief` output; BASE-AGENT does not change yet. Gated by store config `agent_channel = on`. | main | 5 | 1-10 |
| A | tcode parity with Claude Code, APIs only, with evals (§12). | main, unreleased | 5; 6 for the RPC and tool-schema surface | A1-A8 |
| B | Former phase T: tcode IL adapter; Codex launch subset, generated content, Codex adapter; native UDS memory and search for both; live check on the designated test host. | test branch `test/il-multi-harness` | 5 for the adapters; 6 for the CLI and daemon method surface | 11-17 |
| 2 | All tm-launched Claude Code PMs: bind validation (warn), resume, `close --from-result`, PM instructions, delegation patterns, BASE-AGENT. | main | 5; embedded assets add their bundle tests | 8 |
| 3 | Refuse mode, unread digest, datg/Architect parity, `task_ingest` credit, file-drop decision; merge phase B after 2.0 ships. | main | 5 | 18 |

Phase B delivery:
- This spec is merged to `docs/specs/` first.
- Phase B lives on the test branch, rebased on `origin/main` after each phase-1 or phase-2 merge.
- The live check runs on the designated test host.
- No release contains phase A or phase B until after 2.0. No Phase A build starts until it is approved.
- Phase 1 is unchanged and ships on main.

Subsumes: #11133 phases 2-5; #11036 for bound dispatches; the agent half of #9995; #9759's brief role; the Codex launch subset of #7343 (DOC-81 B1, B3, B5, B6), tracked under #11819.
Depends on: #11135 (phase 0); #9760 (phase 1 lock path); #10892 and DOC-87 (constraint refs); #11625 (21 native verbs, before Codex and tcode can drop MCP fully; due before 2.0); #7344 (Codex guard coverage, tracked, not blocking IL capture); #11439 (phase 3); #9763 (phase 2); #10961 (phase 2).
Not blocking: #10153, #11498. Tracking: #11819. Parent: epic #9756; Codex launch also under #9243.

## 12. Phase A: trusty-code PM parity with Claude Code {#SPEC-ILCHAN-12~draft}

**ID:** SPEC-ILCHAN-12~draft

Tracked in #11819. Phase A comes before any mpm integration of trusty-code (tc, bin `tcode`). Phase A makes tc core match Claude Code (CC) for everything the PM workflow uses. It ships APIs only, with no UI work. Phase A adds no new tc issues; all work is tracked under #11819. Nothing from Phase A or Phase B is released before 2.0.

Decision 10 (§8) is GO: the interactive PM mode is Phase A scope. Phases A and B replace the former phase T.

Phase A is done when the §12.2 evals pass on the same agents in CC and tc.

### 12.1 Parity matrix

Evidence paths are under `crates/trusty-code/src/`. "Deliverable" names an API or contract only.

| Capability | Claude Code | tc today (evidence) | Gap | Phase A deliverable | Parity eval |
|---|---|---|---|---|---|
| Interactive PM mode | Multi-turn PM session; each user turn runs the PM loop over one transcript | The TUI creates a session with `delegate: false`, the solo agent (`tui_client/engine.rs:353-358`, #8184). `session.send` only records a `SessionInput` event (`session/registry.rs:464-477`). `task.run` runs one task, with `agent_name` defaulting to `pm` (`task/protocol.rs:1-24`). | No persistent multi-turn PM loop driven by `session.send` | `session.create {mode: "pm"}`; `session.send` runs one PM turn on the session transcript; `TurnStarted`/`TurnDone` events | E1, E2 |
| PM-communication APIs | The PM talks through tool calls; the harness reaches the PM through hooks and pane input | UDS JSON-RPC: `session.create/list/status/cancel/events/send`, `task.run`, `agents.list`, `workstream.*`; `session.attach` is refused over the socket (`serve/uds.rs:160`) | No API to message a running agent, list live agents, or inject context into the next PM turn | `agent.list`, `agent.message {agent_id, text}`, `agent.cancel`, `session.inject_context {text}` (next-turn context, the `additionalContext` equivalent) | E2 |
| Subagent dispatch with `subagent_type` and `model` | `Agent`/`Task` takes `subagent_type`, `prompt`, optional `model` | `delegate_to_agent {agent_name, task}` (`tools/delegate.rs:49, 275-296`); model only from agent config (`agents/config.rs:64-75`); in-process runner (`runner/mod.rs:1-14`) | No per-dispatch `model`; no `agent_id` returned | `delegate_to_agent` gains optional `model` and returns `agent_id`; `agent_name` accepts every CC `subagent_type` in the deployed set | E3 |
| SendMessage resume | Continue a running or finished agent with its context | None; the retry path re-runs a fresh agent (`run_task/redelegation.rs:25`) | No resume by id | `send_to_agent {agent_id, message}` tool and `agent.message` RPC; the agent transcript is kept per `agent_id` for the session's life | E4 |
| Background agents and completion notices | `run_in_background`; the PM gets a task notification when the agent ends | `execute` awaits the runner (`tools/delegate.rs:298` onward) | No background run; no notice | `delegate_to_agent {background: true}` returns at once; `AgentDone` (`events.rs:533`) queues a notification that is delivered at the next PM turn boundary | E4 |
| Trusty events built in | n/a (CC exposes hooks, not a bus) | Process-wide broadcast bus with a per-session `seq` (`events.rs:1-25`); `Event` enum (`events.rs:229`) has `ToolStarted`, `AgentSpawned`, `AgentDone`, `PermissionRequested` and others | No pre-tool veto point; no turn-end or compaction events | `ToolPre` (veto window), `TurnDone`, `CompactStarted`/`CompactDone`; every CC hook point maps to one event | E5 |
| Hooks as pub-sub subscribers | `PreToolUse`, `PostToolUse`, `SubagentStop`, `Stop`, `UserPromptSubmit`, `SessionStart` run commands with JSON on stdin; may block or add context | "No hooks support" (`lib.rs:20-21`); manifest `hooks` key deferred (`plugins/mod.rs:91`) | No subscriber that can block or add context | `hooks.subscribe {events, command, blocking, timeout_ms}`, also read from the `.claude/settings.json` hooks block. The payload has the CC stdin shape, the answer has the CC output shape (`decision`, `reason`, `additionalContext`, `systemMessage`). A subscriber timeout fails per subscriber config. | E5 |
| Models through the Bedrock connector | Anthropic API or Bedrock | Bedrock Converse through `trusty_common::inference` (`llm/bedrock.rs:1-6`, `provider/bedrock.rs:61`); also OpenRouter and others (`provider/`) | No CC model aliases (`sonnet`, `opus`, `haiku`) mapped to Bedrock ids | Alias table `model_alias → bedrock model id`; PM, agents and evals route through the Bedrock connector only | E1-E6 |
| Same agents as CC | `.claude/agents/*.md` | Reads `.claude/` agents (`lib.rs:18-19`) and `.trusty-code/agents/` (`agents/md_loader.rs:108`); maps CC tool names (`agents/claude_tools.rs:42-46`) | Not proven: no check that tc loads the exact set tm deploys for CC | `agents.list` reports source path and hash per agent; equal to the CC-deployed set | E6 |
| Same skills, `Skill` tool | `Skill(skill=…)` loads `SKILL.md` | `use_skill` with a metadata catalog (`tools/skill.rs:1-8, 33`); plugin skills (`plugins/skills.rs`) | Tool name differs; skill set not proven equal | Tool alias `Skill` → `use_skill`; `skills.list` with hashes, equal to the CC set | E6 |
| Output styles and system-prompt append | Output styles; `--append-system-prompt[-file]` | Fixed four-section assembler, with no style or append slot (`prompt/assembler.rs:1-12`) | No style or append | Assembler sections `output_style` and `append_system_prompt`; `session.create` accepts both | E1 |
| MCP servers | `.mcp.json` and settings | Isolated per-server spawn (`mcp/spawn.rs:1-8`), config and trust (`mcp/config.rs`, `mcp/trust.rs`) | Parity not measured | None beyond an eval; mpm sessions use native UDS memory and search (§7) | E6 |
| Slash commands | Built-ins (`/clear`, `/compact`) and `.claude/commands/*.md` | TUI engine-routed MVP commands (`tui_client/engine.rs:581`) | No API path; no project commands | `session.command {name, args}` for `clear`, `compact` and project `.claude/commands` | E2 |
| Permission modes | `default`, `acceptEdits`, `plan`, `bypassPermissions` | `Default`, `AllowAsks` (`permissions/gate.rs:107-111`) | No `plan`, `accept-edits` or `bypass` | `PermissionMode` gains `plan`, `accept-edits`, `bypass`, with the same allow and deny results as CC | E5 |
| Worktree isolation | `isolation: "worktree"` per agent | The session binds to a worktree (`binding/mod.rs:19, 60`); no per-agent tree | No per-agent isolation | `delegate_to_agent {isolation: "worktree"}` creates a tree per agent and reports its path and branch | E3 |
| Context compaction | Auto and `/compact` | Non-destructive compaction (`agent_loop/compaction.rs:1-12`, #2070) | No API trigger; no events | `session.compact`; `CompactStarted`/`CompactDone` events | E2 |
| TodoWrite / tasks | `TodoWrite` list | `todo_write` (`tools/checklist.rs:44`), store (`task/todo_store.rs`) | Name and schema differ | Tool alias `TodoWrite` with the CC schema | E3 |
| Tool set | `Read`, `Edit`, `Write`, `Bash`, `Grep`, `Glob` | `tools/fs/{read,edit,write,grep,glob}.rs`, `tools/bash/mod.rs:42`, `write_files` (`tools/fs/write_files.rs:34`) | Behaviour parity not measured | None beyond an eval | E3 |
| Structured completion | Free final message | `finish_task` (`tools/finish_task.rs:39`) | n/a | Accepts the §6.2 block in `summary` (§8) | E3 |

### 12.2 Eval plan

The same agents run the same tasks in CC and tc. Both harnesses use the same Bedrock model id, so the model is held constant. Agents come from the bundled set (`content/agents/`): `engineer`, `research`, `qa`, `code-critic`, `documentation`.

Tasks: 4 per agent, 20 in all. Each task is a fixture with a deterministic verifier script.
- `engineer`: fix a seeded bug in a fixture crate (the test goes green); add a small feature against a spec.
- `research`: answer a codebase question with `file:line` evidence, checked against a key.
- `qa`: write a failing-then-passing test for a seeded defect.
- `code-critic`: review a seeded diff with 3 known defects.
- `documentation`: write a doc section that passes the doc gates.
- Two tasks per agent exercise the PM path. A PM session dispatches the agent in the background, resumes it once, and ends with the §6.2 block.

Each task runs 3 times per harness, so there are 120 runs. Runs record the bake-off metadata: commit, binary hash, task revision, model id (`bakeoff/mod.rs:1-12`).

| Eval | Proves | Metric |
|---|---|---|
| E1 | PM mode, styles, Bedrock routing | Session completes; the style is applied (golden check on the assembled prompt) |
| E2 | PM APIs, slash, compaction | Scripted API sequence answers as specified |
| E3 | Dispatch, model, worktree, tools, todo | Task success; tool-call correctness |
| E4 | Resume, background, notices | Resumed agent keeps its context; notice delivered once |
| E5 | Events, hooks, permission modes | The same `tm hook` command gives the same verdicts on recorded CC payloads replayed in tc |
| E6 | Same agents, skills, MCP | Hash sets equal; skill load succeeds per skill |

Metrics per run:
- task success (verifier pass);
- tool-call correctness (schema-valid calls / all calls; wrong-tool calls);
- handback block validity (daemon parser verdict `ok`);
- input and output tokens;
- wall time;
- turns.

Pass threshold (open, see Open decisions). Recommended:
- tc task success ≥ CC minus 5 points per agent, and ≥ CC overall;
- tool-call correctness ≥ 98%;
- block validity ≥ 95% and not below CC;
- tokens and wall time ≤ CC + 20% (the bake-off default tolerance, `bakeoff/compare.rs:30`);
- E2, E4, E5 and E6 pass at 100%.

Eval model (open, see Open decisions). Recommend running CC on Bedrock with the same model id as tc. A CC run on the Anthropic API would mix a provider change into the harness comparison.

### 12.3 Revised phases

| Phase | Scope | Depends on | Where | Rung |
|---|---|---|---|---|
| 1 | Claude Code IL round trip on tm-dogfood (unchanged) | #11135 | main | 5 |
| A | tc parity (§12.1) and evals (§12.2); no mpm integration | — | main, unreleased (see Open decisions) | 5 (process lifecycle, shared inference crate); 6 for the RPC and tool-schema surface; no UI |
| B | Former Phase T: tc writes the IL natively, tm launches Codex with standard content, native UDS memory and search, live check | 1, A (evals passed), #11625 | test branch `test/il-multi-harness` | 5; 6 for CLI and daemon methods |
| 2 | All tm-launched CC PMs (unchanged) | 1 | main | 5 |
| 3 | Refuse mode, parity with the existing IL, merge of B | 2, B, 2.0 shipped | main | 5 |

Landing branch (open, see Open decisions). Recommend main, crate-scoped to trusty-code with no tm wiring, unreleased until after 2.0. A long-lived branch for a whole crate's parity work drifts from main.

### 12.4 Acceptance criteria changes

AC-12 is replaced with:
- **AC-12.** In a tc PM session (`session.create {mode: "pm"}`, driven by `session.send`), a `delegate_to_agent` dispatch carrying the pointer writes one entry through the tm daemon. With the daemon stopped, it writes a spill file and the PM's next turn names it.

AC-17 now reads "live check on the designated test host".

New Phase A criteria:
- **AC-A1.** Two `session.send` calls on one PM session run two PM turns over one transcript (E1).
- **AC-A2.** `delegate_to_agent {background: true}` returns an `agent_id` at once. `AgentDone` delivers exactly one notification at the next turn boundary (E4).
- **AC-A3.** `send_to_agent` on a finished agent continues with its prior transcript; a fact stated in run 1 is recalled in run 2 (E4).
- **AC-A4.** A recorded CC `PreToolUse` payload replayed through `hooks.subscribe` makes the same `tm hook` command return the same verdict. A blocking `deny` stops the tool (E5).
- **AC-A5.** Every model call in an eval run goes through the Bedrock connector, and the run metadata names one model id (E1-E6).
- **AC-A6.** `agents.list` and `skills.list` hash sets equal the CC-deployed sets (E6).
- **AC-A7.** `plan` mode refuses every write tool, and `accept-edits` allows edits without an ask (E5).
- **AC-A8.** The §12.2 thresholds are met and the evidence carries bake-off metadata.

### 12.5 Open decisions

Tracked in #11819.
- Eval pass threshold (§12.2).
- Eval model: CC on Bedrock with tc's model id, or the Anthropic API (§12.2).
- Landing branch for Phase A: main, or a long-lived branch (§12.3).
