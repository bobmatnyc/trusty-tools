---
name: architect-workflow
description: Architect skill. Load at the start of a pass, when setting up or moving this project, when writing a record in local/ or a brief, when changing the polling cadence, and at the end of a pass. Covers scaffolding, portability, repository and data rules, the push workflow, the one-live-session rule, the file roles, the cadence and the end-of-pass checklist. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, records, workflow, cadence]
effort: high
---

# architect-workflow — Passes, Records and Cadence

Load this at pass start, for any record write, and at the end of a pass.
Every path below is relative to this project's root, the directory that holds
its `CLAUDE.md`.

## Scaffolding

<!-- rule:r-046 -->
- Skills: the skill index at the end of your operating manual names one
  skill per step. Invoke them rather than re-deriving the loop from prose.
  `tm-fleet-check` is one supervision pass.
- Scripts: `scripts/` holds the deterministic poller
  (`scripts/fleet-poll.py`, started by `scripts/start-fleet-poll.sh`), the
  input-box classifier (`scripts/input-state.py`), the relay send
  (`scripts/relay.sh`) and the double-post guard (`scripts/posted.sh`).
  `tm fleet init` seeded them and never overwrites an edited copy.
- Observation: the raw `tmux` CLI is the ground truth. An MCP tmux server is
  optional; if one is loaded, cross-check its session list against
  `tmux list-sessions` every pass and fall back to the CLI when it
  under-reports.

## Portability

<!-- rule:r-047 -->
Every rule and observation lives in plain files in this directory: the
instructions trusty-mpm composes for you, the owner's `AGENTS.md`, this
project's `CLAUDE.md`, the skills in `.claude/skills/`, and your records in
`local/`. Nothing essential lives only in a conversation. To continue from any
account or after a context reset: open this directory, read `AGENTS.md`,
`CLAUDE.md`, `local/instance.md`, `local/user.md`, `local/owner/`,
`local/goals.md`, the Intent Ledger digest (the handoff:
`python3 scripts/datg.py --project <il-dir> digest`, where `<il-dir>` is
your Intent Ledger directory under local/), `local/state.md` and the recent
entries of `local/actions.md`, then run `tm-fleet-check`. Read
`local/state.md` with `head -60` (the monitoring snapshot; the handoff is the
digest) and `local/actions.md` with
`tail -c 3000`, never whole. Take the time from one `date -u` per pass, and
read alerts only from the poller's wake line, never by pairing `date -u` with
a tail of the alerts file.

A running heartbeat (a cron job or a Monitor) does not carry over. It is bound
to the session that created it. After a restart, check `local/state.md` for
the monitoring status and re-create the heartbeat only if it should be active
and no other session runs one.

## Repository and data

<!-- rule:r-048 -->
- This project's repository has no git remote. Never push it, and never add a
  remote, unless the owner explicitly asks.
- Durable records live in `local/` or in a trusty-memory palace. Durable means
  briefs, diagnostic output, evidence files and decisions. Write briefs and
  evidence under `local/briefs/` and commit them at the end of the pass.
- `inbox/` holds only transient queues and logs that the scripts regenerate.
  Nothing in it is a record, and it is not tracked.
- Scratch paths under the system temp directory (agent scratchpads, task
  outputs) vanish. Copy a file into `local/briefs/` before a record cites it.
- Secret-bearing backups stay out of every repository.

## Workflow

<!-- rule:r-059 -->
PMs push to you when they need the owner; you do not wait to be asked. The
push channel is a Claude Code `Notification` hook in each watched project
that appends to `inbox/events.jsonl` (absent until the hooks are installed;
then every pass is a full poll). For each push:
1. Read the PM's pane for the exact question and its options.
2. Query that project's own trusty-memory palace and trusty-search index for
   prior rulings. When context on specs or issue history would help, send a
   research agent to gather it; do not read those documents yourself.
3. Bring the owner a multiple-choice question with the recommended option
   first, citing the evidence behind the recommendation.
4. Relay the owner's answer with `scripts/relay.sh` and verify the
   acknowledgement.

Answer directly only at high confidence (`architect-operating-rules`).
Everything else follows these four steps.

<!-- rule:r-060 -->
**One live Claude session.** The Architect is the only Claude session that
supervises this fleet. PMs send their questions and reports straight to it by
cross-session message, and the fleet poller (`scripts/fleet-poll.py`, no LLM)
catches dialogs and questions the PMs did not send. If your session is
silent, the poller only alerts; it never starts a second Architect, and
neither do you.

<!-- rule:r-082 -->
## Files

- `AGENTS.md`: the owner's instance layer: this installation's owner, host,
  fleet, workarounds and project rules. The owner writes it; you never do.
- `CLAUDE.md`: the fleet specifics: the watch set, your relay name, the
  cadence and the context bar.
- `local/instance.md`: the instance facts you find and keep current: hosts,
  builder slots, accounts, services, known quirks.
- `local/user.md`: the primary user's profile. Rewrite in place.
- `local/users/`: one profile per other user, same template. Rewrite in place.
- `local/owner/`: the consolidated model of the owner, each entry with a
  confidence level (`architect-owner-model`).
- `local/goals.md`: ranked goals, success criteria, milestones, the
  session-to-goal map, the capacity split and open optimization proposals.
  Rewrite in place.
- `local/state.md`: the current snapshot: monitoring status, watch set,
  pending decisions, the context-refresh queue. Rewrite in place; it is not a
  history. Keep it under 0.4 MB: at the end of a pass, if
  `wc -c local/state.md` exceeds 400000, move superseded watch sections to
  an archive file beside it in `local/`.
- `local/actions.md`: the append-only dated log of what you did and sent.
- `local/projects/<project>.md`: one durable record per watched project,
  started from `local/projects/_template.md`. Read it before responding to
  that project.
- `local/mandates/`: institutional mandates, one TOML file each; the schema
  is `local/mandates/README.md`.
- `local/briefs/`: briefs, relay attachments and evidence files. Durable;
  committed with `local/`.
- `inbox/`: transient queues and logs written by the scripts; not tracked.

<!-- rule:r-083 -->
## End of pass

Update the records before ending any pass:
- `local/projects/<project>.md`: new facts, decisions, authorizations,
  blockers, command lessons.
- `local/state.md`: the rows that changed.
- `local/actions.md`: one dated entry: what changed, exactly what was sent
  and where, and what is still pending. An unchanged pass still gets a
  one-line "verified wait" entry.
- Commit these files, with any new `local/briefs/` files, in this project's
  repository. Never push.

## Cadence

<!-- rule:r-084 -->
Polling is deterministic: no inference unless action is needed. The fleet
poller, started by `scripts/start-fleet-poll.sh` in its own tmux session (this
session's name plus `-poll`), captures every PM pane on its interval without
an LLM, writes each new condition to `inbox/alerts.jsonl` (dialogs, owner
questions, idle PMs, the context bar, plan limits, errors, exited panes, host
load, disk and swap, inbox pushes) and wakes this session by typing
`[poll HH:MMZ] N new alerts -> <path>` into your pane only when there is a new
alert. A `[poll …]` turn is your own wake, not an owner message. On every
wake: read the new alert lines, capture only the panes they name, act, and
update the state. Session crons and Monitors die with the session: re-create
them after every relaunch and record them in `local/state.md`. After a
restart, check that the poller session runs.

## Related skills

- `tm-fleet-check`: one supervision pass.
- `tm-context-refresh`: the refresh at the context bar.
