---
spec_refs:
  - id: SPEC-ARCHITECT-01~draft
    path: docs/specs/DOC-83-architect-identity-voice-duties.md
    anchor: SPEC-ARCHITECT-01~draft
---

# DOC-84 — Fleet supervisor: design and decisions

> **Superseded in part by [DOC-83](./DOC-83-architect-identity-voice-duties.md).** The supervisor is now the Architect (DOC-83 §1 requirement A, ruling R3). Identity, voice, duties, the 2.0 native build and the dashboard briefs live in DOC-83. This document stays the design of record for question collection (§3), the question event log (§4), asking the user (§5) and monitoring (§6). Where it says "supervisor", read "Architect"; tmux session names and file paths below are the phase-1 prototype's, and the full rename lands with the native build (DOC-83 N1).

**Status:** Draft (prototype; superseded in part by DOC-83, banner above)
**Subsystem:** `trusty-mpm` — Architect question collector and event log (prototype in the Architect project; native build tracked in [#8436](https://github.com/bobmatnyc/trusty-tools/issues/8436), [#8453](https://github.com/bobmatnyc/trusty-tools/issues/8453), [#8433](https://github.com/bobmatnyc/trusty-tools/issues/8433))
**Owner:** owner
**Last-updated:** 2026-10-24
**Spec ID:** `SPEC-SUPERVISOR-01~draft` … `SPEC-SUPERVISOR-08~draft` (DOC-84). `SPEC-SUPERVISOR-01` is the id the Architect project's records used before the move; it stays valid as the alias for this document, and each section below carries its own `SPEC-SUPERVISOR-NN~draft` id.
**DOC-N claim:** `DOC-84`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). Moved from the Architect project under [#9291](https://github.com/bobmatnyc/trusty-tools/issues/9291). Work started 2026-09-24.

This spec records how the fleet supervisor works and the decisions behind it. The shipped manual `content/instructions/architect/architect.md` is the operating manual; this file is the design of record. Each decision carries its date and source. Add new decisions to §8 and change the section they affect in the same commit. Paths without a `content/` or `crates/` prefix name files in the Architect project repository.

---

## 1. Scope and build path {#SPEC-SUPERVISOR-01~draft}

**ID:** SPEC-SUPERVISOR-01~draft

- **D1 (owner, 2026-09-24).** Prototype the supervisor's scripts (collectors and the rest) in the Architect project. Once a workflow pattern is established, rebuild them in Rust for the built supervisor (trusty-tools; see #8436, #8453, #8433).
- **D2 (owner, 2026-09-24).** If the question collector works, it becomes part of trusty as a crate. The trial evidence (§7) supports that proposal.
- The prototype keeps every durable state in plain files, so any Claude account or restarted session continues from the files alone (manual, Portability).

## 2. Components {#SPEC-SUPERVISOR-02~draft}

**ID:** SPEC-SUPERVISOR-02~draft

| Component | Where | Runs as |
| --- | --- | --- |
| Supervisor | the Architect project's Claude Code session | the Architect's tmux session |
| Push hook | `scripts/notify-supervisor.sh` in each watched project | Claude Code `Notification` hook → `inbox/events.jsonl` |
| Question collector | `scripts/question-collector.py` | the collector's own tmux session (`scripts/start-question-collector.sh`) |
| collector-agent | `.claude/agents/collector-agent.md` | one short `claude -p --agent collector-agent` call per changed pane |
| Question event log | `local/question-events.jsonl` via `scripts/qevents.py` | file (bus + store in the Rust build) |
| Queue tool | `scripts/questions.py` | CLI |

## 3. Question collection {#SPEC-SUPERVISOR-03~draft}

**ID:** SPEC-SUPERVISOR-03~draft

- **D3 (owner, 2026-09-24).** A dedicated agent collects owner questions and writes them to state. The supervisor pulls from state, adds context (memory, search), answers what it can, and asks the user the rest. Answered questions are logged and leave the queue.
- **D4 (owner, 2026-09-24).** It is a defined agent type, `collector-agent`, with its rules embedded in the agent file.
- **D5 (owner, 2026-09-24).** Tracking is per monitored session, so the collector knows what each PM has already asked.
- Implementation choices (supervisor, 2026-09-24):
  - The loop is a script in its own tmux session, not a subagent of the supervisor. A subagent ends when the supervisor restarts, clears or switches account, and a long-running agent loop spends tokens while idle.
  - Per-session tracking is a ledger folded from the event log and passed to each collector-agent call, not one process per session. The agent returns only new questions, plus the ids of open ones the pane now shows answered.
  - The collector reads a pane on each PM push event (debounced 20 s) and sweeps every `tm-*` session each 10 minutes. It skips unchanged panes (hash of the pane without spinners and timers).
  - It never sends keys to any pane.
  - It skips the supervisor's own sessions, itself, and an observer supervisor session whose summaries duplicate PM questions.
  - The start script passes the Claude config-dir variable. The tmux server's environment lacks it, and without it `claude -p` reads a stale default login.
  - collector-agent's tools are limited to Read, and the collector also passes `--tools ""`.

## 4. Question state: event log, pub/sub-ready {#SPEC-SUPERVISOR-04~draft}

**ID:** SPEC-SUPERVISOR-04~draft

- **D6 (owner, 2026-09-24).** Use pub/sub on the trusty message bus for open questions, to allow a multichannel model later (Slack, Telegram).
- The bus (ADR-0005, `trusty-agents-common` events) is an in-process broadcast channel: 1024 slots, no persistence, lagging subscribers skip events. It carries changes; it cannot be the only record of an open question.
- Therefore state is an append-only event log, and the open queue is a view folded from it. The prototype writes the log to a file; the Rust build publishes the same events on the bus and persists them in a store.
- Events (`scripts/qevents.py`):

| Event | Fields |
| --- | --- |
| `question.opened` | id, session, question, options, recommendation, urgent, dialog, reason |
| `question.answered` | id, channel (chat, pane, slack, telegram, …), answer, by |
| `question.relayed` | id, target session, text |
| `question.retired` | id, disposition (asked-user, answered-direct, already-answered, resolved-in-pane, not-a-question), reason |

- Multichannel model: each channel adapter (`trusty-channels` has Slack and Telegram) subscribes to `question.opened` and posts it; an answer on any channel publishes `question.answered`; the supervisor relays it and records `relayed` and `retired`; every channel sees the question close. Adding a channel adds a subscriber and changes nothing else.
- The question id is a hash of session and normalized question text.

## 5. Asking the user {#SPEC-SUPERVISOR-05~draft}

**ID:** SPEC-SUPERVISOR-05~draft

- Every decision goes to the user as options with a recommendation first (manual). The supervisor's own messages end with "Waiting for you: N".
- **D7 (owner, 2026-09-24).** Questions posted only as chat text get lost in the stream, so the queue lives in files, not in the conversation.
- **D8 (owner, 2026-09-24).** A page tied to one Claude account (an artifact) is not acceptable for the queue: it breaks on an account switch.
- **D11 (owner, 2026-09-24).** "Always ask": pending questions go to the user in a dialog as soon as they are ready, batched up to 4, without waiting for "ask".
- An open question dialog blocks the supervisor session; the collector keeps collecting meanwhile, so nothing is lost.
- Relays carry the user's words verbatim; any supervisor reading is labelled as such. A dim next-prompt suggestion in a pane is never a user ruling.

## 6. Monitoring {#SPEC-SUPERVISOR-06~draft}

**ID:** SPEC-SUPERVISOR-06~draft

- The supervisor watches the collector's log for `NEW` questions and errors, and the inbox for non-idle push events (permission prompts, dialogs). Idle events go to the collector, which reads the pane.
- Filtering idle events is allowed only while the collector runs; before it existed, the filter hid an urgent question for ~25 minutes (2026-09-24).
- **D9 (owner, 2026-09-24).** The Codex supervisor is no longer run; its automations stay paused.
- **D10 (owner, 2026-09-24).** Prefer local CLIs over MCP where one exists. For search: `trusty-search search` in the target repo, or `trusty-search query -i <index-id>`. Index ids are listed, never guessed. Search over UDS is trusty-tools #6285 (to be scheduled in trusty-search's next even-patch feature milestone, owner ruling 2026-09-24).

## 7. Trial evidence (collector, from 2026-09-24 08:25Z) {#SPEC-SUPERVISOR-07~draft}

**ID:** SPEC-SUPERVISOR-07~draft

- First start: 5 calls failed with an expired-login error (the config-dir variable was missing); fixed; no PM session was affected.
- Noise: Claude Code's feedback survey was queued once; rules updated.
- Misses: three questions of one PM ticket in the same turn as two captured questions (09-24 08:50Z); added by hand.
- Duplicates: an observer supervisor's summaries; that session is now skipped.
- One extraction timeout (120 s) on a writing-project session, which stalls the serial loop.
- False retirement: a dim input-box suggestion ("Owner picks A", a PM session, 09-24 09:33Z) made collector-agent retire an unanswered question. Fixed in code: the capture drops the input box unless a selection dialog is open. A `resolved-in-pane` retirement is a claim; the supervisor checks it when it matters.

## 8. Decision log {#SPEC-SUPERVISOR-08~draft}

**ID:** SPEC-SUPERVISOR-08~draft

| # | Date | Source | Decision |
| --- | --- | --- | --- |
| D1 | 2026-09-24 | owner | Prototype supervisor scripts in the Architect project; rebuild in Rust once the pattern works |
| D2 | 2026-09-24 | owner | If the collector works, build it into trusty as a crate |
| D3 | 2026-09-24 | owner | A dedicated agent collects questions into state; supervisor enriches, answers or asks; answered ones are logged and leave the queue |
| D4 | 2026-09-24 | owner | collector-agent is a defined agent type with embedded rules |
| D5 | 2026-09-24 | owner | Track questions per monitored session |
| D6 | 2026-09-24 | owner | Pub/sub for open questions (event log now, bus + store in Rust), for multichannel later |
| D7 | 2026-09-24 | owner | Text-only questions get lost; the queue lives in files |
| D8 | 2026-09-24 | owner | No account-bound page for the queue |
| D9 | 2026-09-24 | owner | Codex supervisor retired |
| D10 | 2026-09-24 | owner | Prefer local CLIs (search) over MCP |
| D11 | 2026-09-24 | owner | "Always ask": put pending questions to the user in a dialog as soon as they are ready; do not wait for the user to say "ask" |
