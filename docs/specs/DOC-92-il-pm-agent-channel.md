# DOC-92 — Intent Ledger as the PM-agent channel, for every harness

**Status:** Approved (r2, 2026-11-25)
**Subsystem:** trusty-mpm — `tm hook`, daemon delegation tracker and ledger methods, `tm dbatg`, runtime adapters (`runtime/`); trusty-code (`tcode`) — `delegate_to_agent`, `finish_task`, search and memory clients; trusty-architect — `ledger` (dbatg) model and writer; instructions (`content/agents/BASE-AGENT.md`, `content/skills/tm-delegation-patterns.md`, PM instructions, generated Codex and tcode content)
**Owner:** owner
**Last-updated:** 2026-11-25
**Spec ID:** `SPEC-ILCHAN-01~approved` … `SPEC-ILCHAN-11~approved` (DOC-92)
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
| Launch | none in tm | `RuntimeKind::Codex`, the DOC-81 B1, B3, B5, B6 subset: `tm session new --runtime codex`, per-launch `CODEX_HOME`, context through a generated `AGENTS.override.md` | `tcode run-task engineer` one-shot (`tm session new --runtime tcode`) | `tcode run-task` one-shot, unchanged. An interactive tcode PM mode is a stretch item (§8) |
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

Held for the owner:
10. **Interactive tcode PM mode.** tm launches `tcode run-task` one-shot. An interactive PM session mode is new surface and is not part of phase T. It is a stretch item pending the owner. Phase T drives the tcode IL round trip through `tcode run-task`.

Open captures (UNCONFIRMED until a dated capture on the Studio):
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
- **tcode:** `delegate_to_agent` binds and submits; the trusty-search client moves to UDS. The interactive PM session mode is a stretch item (§8, decision 10).
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
- AC-12. tcode: a `delegate_to_agent` dispatch with the pointer writes one entry through the tm daemon. With the daemon stopped, it writes a spill file, and the PM's `ToolResult` names it.
- AC-13. Codex: `tm session new --runtime codex` starts Codex with generated instructions, agents, and skills under a per-launch `CODEX_HOME`. `~/.codex` is byte-identical before and after.
- AC-14. Codex: the managed hooks are active at launch, or the launcher prints `IL capture inactive` and binds nothing.
- AC-15. Codex: a `spawn_agent` dispatch with the pointer writes one entry from SubagentStop.
- AC-16. A Codex session and a tcode session each run `tm memory recall` and a native search over UDS, with no trusty-memory or trusty-search MCP server in the session's config.
- AC-17. Live check on the Studio for AC-12 to AC-16, with dated captures for X1-X3.
- AC-18. Phase 3: a dispatch without a pointer from a PM in refuse mode is refused with the remedy text.

## 11. Rollout and delivery {#SPEC-ILCHAN-11~approved}

**ID:** SPEC-ILCHAN-11~approved

| Phase | Scope | Where | Rung | ACs |
|---|---|---|---|---|
| 0 | Align the id regex (#11135). Decisions 1-9 are approved; decision 10 is held. | — | 1 | — |
| 1 | One PM (tm-dogfood), Claude Code. Brief fields, `dispatch`, `brief`, `results`, `replay`, daemon bind/submit, SubagentStop adapter, spill. Agent instructions ship in the `brief` output; BASE-AGENT does not change yet. Gated by store config `agent_channel = on`. | main | 5 | 1-10 |
| T | tcode adapter (through `tcode run-task`); Codex launch subset, generated content, Codex adapter; native UDS memory and search for both; Studio live check. | test branch `test/il-multi-harness` | 5 for the adapters; 6 for the CLI and daemon method surface | 11-17 |
| 2 | All tm-launched Claude Code PMs: bind validation (warn), resume, `close --from-result`, PM instructions, delegation patterns, BASE-AGENT. | main | 5; embedded assets add their bundle tests | 8 |
| 3 | Refuse mode, unread digest, datg/Architect parity, `task_ingest` credit, file-drop decision; merge phase T after 2.0 ships. | main | 5 | 18 |

Phase T delivery:
- This spec is merged to `docs/specs/` first.
- Phase T lives on the test branch, rebased on `origin/main` after each phase-1 or phase-2 merge.
- The live check runs on the Studio.
- No release contains phase T until after 2.0.
- Phase 1 is unchanged and ships on main.

Subsumes: #11133 phases 2-5; #11036 for bound dispatches; the agent half of #9995; #9759's brief role; the Codex launch subset of #7343 (DOC-81 B1, B3, B5, B6), tracked under #11819.
Depends on: #11135 (phase 0); #9760 (phase 1 lock path); #10892 and DOC-87 (constraint refs); #11625 (21 native verbs, before Codex and tcode can drop MCP fully; due before 2.0); #7344 (Codex guard coverage, tracked, not blocking IL capture); #11439 (phase 3); #9763 (phase 2); #10961 (phase 2).
Not blocking: #10153, #11498. Tracking: #11819. Parent: epic #9756; Codex launch also under #9243.
