# SPEC-SUPERVISOR-01 — fleet supervisor: design and decisions

Status: prototype (this repository). Owner: the owner. Started 2026-09-24.

This spec records how the fleet supervisor works and the decisions behind it.
`architect.md` is the operating manual; this file is the design of record. Each
decision carries its date and source. Add new decisions to §8 and change the
section they affect in the same commit.

## 1. Scope and build path

- **D1 (the owner, 2026-09-24).** Prototype the supervisor's scripts (collectors and
  the rest) in this repository. Once a workflow pattern is established, rebuild
  them in Rust for the built supervisor (trusty-tools; see #8436, #8453, #8433).
- **D2 (the owner, 2026-09-24).** If the question collector works, it becomes part of
  trusty as a crate. The trial evidence (§7) supports that proposal.
- The prototype keeps every durable state in plain files, so any Claude account
  or restarted session continues from the files alone (architect.md, Portability).

## 2. Components

| Component | Where | Runs as |
| --- | --- | --- |
| Supervisor | this directory's Claude Code session | tmux `tm-supervisor` |
| Push hook | `scripts/notify-supervisor.sh` in each watched project | Claude Code `Notification` hook → `inbox/events.jsonl` |
| Question collector | `scripts/question-collector.py` | tmux `tm-supervisor-collector` (`scripts/start-question-collector.sh`) |
| collector-agent | `.claude/agents/collector-agent.md` | one short `claude -p --agent collector-agent` call per changed pane |
| Question event log | `local/question-events.jsonl` via `scripts/qevents.py` | file (bus + store in the Rust build) |
| Queue tool | `scripts/questions.py` | CLI |

## 3. Question collection

- **D3 (the owner, 2026-09-24).** A dedicated agent collects owner questions and
  writes them to state. The supervisor pulls from state, adds context (memory,
  search), answers what it can, and asks the user the rest. Answered questions
  are logged and leave the queue.
- **D4 (the owner, 2026-09-24).** It is a defined agent type, `collector-agent`, with
  its rules embedded in the agent file.
- **D5 (the owner, 2026-09-24).** Tracking is per monitored session, so the collector
  knows what each PM has already asked.
- Implementation choices (supervisor, 2026-09-24):
  - The loop is a script in its own tmux session, not a subagent of the
    supervisor. A subagent ends when the supervisor restarts, clears or
    switches account, and a long-running agent loop spends tokens while idle.
  - Per-session tracking is a ledger folded from the event log and passed to
    each collector-agent call, not one process per session. The agent returns
    only new questions, plus the ids of open ones the pane now shows answered.
  - The collector reads a pane on each PM push event (debounced 20 s) and
    sweeps every `tm-*` session each 10 minutes. It skips unchanged panes
    (hash of the pane without spinners and timers).
  - It never sends keys to any pane.
  - It skips `tm-supervisor`, itself, and `tm-employer-cto` (an observer
    supervisor whose summaries duplicate PM questions).
  - The start script passes `CLAUDE_CONFIG_DIR`. The tmux server's environment
    lacks it, and without it `claude -p` reads the stale `~/.claude` login.
  - collector-agent's tools are limited to Read, and the collector also passes
    `--tools ""`.

## 4. Question state: event log, pub/sub-ready

- **D6 (the owner, 2026-09-24).** Use pub/sub on the trusty message bus for open
  questions, to allow a multichannel model later (Slack, Telegram).
- The bus (ADR-0005, `trusty-agents-common` events) is an in-process broadcast
  channel: 1024 slots, no persistence, lagging subscribers skip events. It
  carries changes; it cannot be the only record of an open question.
- Therefore state is an append-only event log, and the open queue is a view
  folded from it. The prototype writes the log to a file; the Rust build
  publishes the same events on the bus and persists them in a store.
- Events (`scripts/qevents.py`):

| Event | Fields |
| --- | --- |
| `question.opened` | id, session, question, options, recommendation, urgent, dialog, reason |
| `question.answered` | id, channel (chat, pane, slack, telegram, …), answer, by |
| `question.relayed` | id, target session, text |
| `question.retired` | id, disposition (asked-user, answered-direct, already-answered, resolved-in-pane, not-a-question), reason |

- Multichannel model: each channel adapter (`trusty-channels` has Slack and
  Telegram) subscribes to `question.opened` and posts it; an answer on any
  channel publishes `question.answered`; the supervisor relays it and records
  `relayed` and `retired`; every channel sees the question close. Adding a
  channel adds a subscriber and changes nothing else.
- The question id is a hash of session and normalized question text.

## 5. Asking the user

- Every decision goes to the user as options with a recommendation first
  (architect.md). The supervisor's own messages end with "Waiting for you: N".
- **D7 (the owner, 2026-09-24).** Questions posted only as chat text get lost in the
  stream, so the queue lives in files, not in the conversation.
- **D8 (the owner, 2026-09-24).** A page tied to one Claude account (an artifact) is
  not acceptable for the queue: it breaks on an account switch.
- **D11 (the owner, 2026-09-24).** "Always ask": pending questions go to the user in a
  dialog as soon as they are ready, batched up to 4, without waiting for "ask".
- An open question dialog blocks the supervisor session; the collector keeps
  collecting meanwhile, so nothing is lost.
- Relays carry the user's words verbatim; any supervisor reading is labelled
  as such. A dim next-prompt suggestion in a pane is never a user ruling.

## 6. Monitoring

- The supervisor watches the collector's log for `NEW` questions and errors,
  and the inbox for non-idle push events (permission prompts, dialogs). Idle
  events go to the collector, which reads the pane.
- Filtering idle events is allowed only while the collector runs; before it
  existed, the filter hid an urgent question for ~25 minutes (2026-09-24).
- **D9 (the owner, 2026-09-24).** The Codex supervisor is no longer run; its
  automations stay paused.
- **D10 (the owner, 2026-09-24).** Prefer local CLIs over MCP where one exists. For
  search: `trusty-search search` in the target repo, or `trusty-search query
  -i <index-id>`. Index ids are listed, never guessed. Search over UDS is
  trusty-tools #6285 (to be scheduled in trusty-search's next even-patch
  feature milestone, the owner 2026-09-24).

## 7. Trial evidence (collector, from 2026-09-24 08:25Z)

- First start: 5 calls failed with "OAuth session expired" (missing
  `CLAUDE_CONFIG_DIR`); fixed; no PM session was affected.
- Noise: Claude Code's feedback survey was queued once; rules updated.
- Misses: CRM #593 Q1/Q3 in the same turn as two captured questions
  (09-24 08:50Z); added by hand.
- Duplicates: tm-employer-cto's summaries; that session is now skipped.
- One extraction timeout (120 s) on tm-writing, which stalls the serial loop.
- False retirement: a dim input-box suggestion ("the owner picks A", tm-tga-01, 09-24 09:33Z)
  made collector-agent retire an unanswered question. Fixed in code: the capture
  drops the input box unless a selection dialog is open (32e404c). A
  `resolved-in-pane` retirement is a claim; the supervisor checks it when it
  matters.

## 8. Decision log

| # | Date | Source | Decision |
| --- | --- | --- | --- |
| D1 | 2026-09-24 | the owner | Prototype supervisor scripts here; rebuild in Rust once the pattern works |
| D2 | 2026-09-24 | the owner | If the collector works, build it into trusty as a crate |
| D3 | 2026-09-24 | the owner | A dedicated agent collects questions into state; supervisor enriches, answers or asks; answered ones are logged and leave the queue |
| D4 | 2026-09-24 | the owner | collector-agent is a defined agent type with embedded rules |
| D5 | 2026-09-24 | the owner | Track questions per monitored session |
| D6 | 2026-09-24 | the owner | Pub/sub for open questions (event log now, bus + store in Rust), for multichannel later |
| D7 | 2026-09-24 | the owner | Text-only questions get lost; the queue lives in files |
| D8 | 2026-09-24 | the owner | No account-bound page for the queue |
| D9 | 2026-09-24 | the owner | Codex supervisor retired |
| D10 | 2026-09-24 | the owner | Prefer local CLIs (search) over MCP |
| D11 | 2026-09-24 | the owner | "Always ask": put pending questions to the user in a dialog as soon as they are ready; do not wait for the user to say "ask" |
