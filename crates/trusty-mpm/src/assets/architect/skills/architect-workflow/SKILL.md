---
name: architect-workflow
description: Framework skill. Load at the start of a pass, when setting up or moving this repository or an install, when writing a record in local/ or a briefs file, when changing the polling cadence, and at the end of a pass. Covers scaffolding, portability, repository and data rules, the push workflow, the one-live-session rule, the file roles, the cadence and the end-of-pass checklist.
---

# Architect workflow and records (framework)

Kind: framework. Index line in architect.md: load this at pass start, for any record write, and at end of pass.

## Scaffolding
<!-- rule:r-046 -->
- Agent: `architect` (`.claude/agents/architect.md`) is the entry point for one supervision pass.
- Skills: the index at the end of this file. Invoke them rather than re-deriving the loop from this prose.
- Scripts: `scripts/` holds the push hook, the input-box classifier and the fleet restart script. See `scripts/README.md`.
- Observation: the raw `tmux` CLI is the ground truth. An MCP tmux server (e.g. `tmux-mcp-rs`) is optional; if one is loaded, cross-check its session list against `tmux list-sessions` every pass and fall back to the CLI when it under-reports.

## Portability
<!-- rule:r-047 -->
Every rule and observation lives in plain files and palaces: the operating manual (`architect.md`, composed into your instructions by tm), the instance layer (`AGENTS.md`), the skills, the instance files in `local/`, and the `architect`, `owner` and project palaces. Nothing essential lives only in a conversation. To continue from any account or after a context reset: open this directory; with `architect.md` and `AGENTS.md` already in your instructions, read the `owner` palace → `local/goals.md` → `local/state.md` → the recent entries of `local/actions.md`, then run the `tm-fleet-check` skill.

A running heartbeat (cron job or Monitor) does not carry over. It is bound to the session that created it. After a restart, check `local/state.md` for the monitoring status and re-create the heartbeat only if it should be active and no other session runs one.

## Repository and data
<!-- rule:r-048 -->
- This repository has no git remote. Never push it unless the owner explicitly asks. `local/` is tracked in this repository on main; `git -C local` resolves to it.
- Durable records live in the `local/` git repository or in a trusty-memory palace. Durable means briefs, diagnostic output, evidence files and decisions. Write briefs and evidence under `local/briefs/` and commit them at the end of the pass.
- `inbox/` holds only transient queues and logs that scripts regenerate. Nothing in it is a record. `scripts/README.md` ("Retention") says how long each file lives.
- `inbox/briefs` is a symlink to `local/briefs`, because PM sessions hold absolute paths into it. Write new briefs to `local/briefs/`.
- Scratch paths under the system temp directory (agent scratchpads, task outputs) vanish. Copy a file into `local/briefs/` before a record cites it.
- Secret-bearing backups stay out of every repository. Backup rules: `architect-host-admin`.

## Workflow
<!-- rule:r-059 -->
PMs push to the Architect when they need the owner; the Architect does not wait to be asked. The push channel is a Claude Code `Notification` hook in each watched project that appends to `inbox/events.jsonl`. For each push:
1. Read the PM's pane for the exact question and its options.
2. Query that project's own trusty-memory palace and trusty-search index for prior rulings. When context on specs or issue history would help, send a research agent to gather it (do not read those documents yourself).
3. Bring the owner a multiple-choice question with a recommended option first, citing the evidence behind the recommendation.
4. Relay the owner's answer with the two-step send and verify acknowledgement.

Answer directly only at high confidence (see Operating rules). Everything else follows these four steps.

<!-- rule:r-060 -->
**One live Claude session.** The Architect is the only Claude session. PMs send their questions and reports straight to it by cross-session message (SendMessage), and the fleet poller (`scripts/fleet-poll.py`, no LLM) catches dialogs and questions the PMs did not send. The question collector is retired; its log stays as history (`scripts/questions.py list`). The poller also runs the Architect watchdog: if the Architect session is dead, it relaunches one from the latest snapshot. If the session is silent, it only alerts and never starts a second Architect. Design history: `docs/specs/SPEC-SUPERVISOR-01.md`.

<!-- rule:r-082 -->
## Files
- `architect.md` — the operating manual, composed into your instructions by tm; not a file in this directory.
- `AGENTS.md` — the instance layer: this installation's owner (the primary user's profile and a line per other user), host, fleet, workarounds and project rules. Rewrite in place.
- The `owner` palace — the consolidated model of the owner, each entry with a confidence level; updated by the daily dreaming pass.
- `local/goals.md` — ranked goals, success criteria, milestones, the session-to-goal map, capacity split and open optimization proposals. Rewrite in place.
- `local/state.md` — current snapshot: monitoring status, watch set, dashboard, pending decisions, improvement candidates. Rewrite in place; not a history.
- `local/actions.md` — append-only dated log of what the Architect did and sent.
- Each project's palace record — one durable record per watched project, stored with `memory_remember` in that project's palace. Read before responding to that project.
- `local/mandates/<id>.toml` — institutional mandates (schema: `templates/mandates/README.md`).
- `local/briefs/` — briefs, relay attachments and evidence files. Durable; committed in `local/`.
- `inbox/` — transient queues and logs written by scripts; not tracked. `inbox/briefs` links to `local/briefs`.
- `templates/` — starting copies of the `local/` files; `scripts/init.sh` installs them.

<!-- rule:r-083 -->
## End of pass
Update the knowledge base before ending any pass:
- Each project's palace record — new facts, decisions, authorizations, blockers, command lessons.
- `local/state.md` — the dashboard rows that changed.
- `local/actions.md` — one dated entry: what changed, exactly what was sent and where, and what is still pending. An unchanged pass still gets a one-line "verified wait" entry.
- Commit the `local/` files, with any new `local/briefs/` files, in this repository. Never push.

## Cadence
<!-- rule:r-084 -->
Polling is deterministic: no inference unless action is needed. The fleet poller (started by `scripts/start-fleet-poll.sh` in its own tmux session, interval 600 s) captures every PM pane every 10 minutes without an LLM, writes each new condition to `inbox/alerts.jsonl` (dialogs, owner questions, idle PMs, ctx thresholds, plan limits, errors, exited panes, host load/disk/swap, inbox pushes) and wakes this session by typing `[poll HH:MMZ] N new alerts -> <path>` into the Architect's pane only when there is a new alert. A `[poll …]` turn is the Architect's own wake, not an owner message. On every wake: read the new alert lines, capture only the panes they name, act, and update the state. The inbox Monitors stay the fast path for pushes and owner questions. Session crons and Monitors die with the session: re-create them after every relaunch and record them in `local/state.md`. After a restart, check that the poller session is running.
