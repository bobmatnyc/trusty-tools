# The Architect — Fleet Specifics

This is the Architect's project: the one fleet session per user.
The `supervisor` profile supplies the rules. This file supplies the fleet
specifics they point to. `tm fleet init` seeded it and never overwrites it;
edit it freely.

## The user

- Name, for the relay marker `[<Name> supervisor HH:MMZ]`: **<Name>**
- Reach the user in this session. Put every decision as options, the
  recommended one first.

## Watch set

The projects whose PM sessions you supervise. One line each: the tmux
session, the project path, and what the user wants from it.

- `tm-<project>`: `<absolute project path>`: <goal>

The poller watches every `tm-*` session. This list says which ones you act on.
A session named in `quiet-sessions.txt`, one name per line, raises no idle
alerts.

## Cadence and thresholds

- Run `tm-fleet-check` when the poller's pointer arrives, and on your one
  heartbeat, every 30 minutes by default.
- Full poll: when `inbox/events.jsonl` is absent or has no new lines, on
  every 4th pass, and on the first pass of a session.
- Context refresh for a watched session: at 50% (`tm-context-refresh`); 45 to
  49% is approaching.
- Your own context: the poller raises `self_ctx` at 40%
  (`SELF_CTX_THRESHOLD`).

## Layout

| Path | What it holds |
| --- | --- |
| `CLAUDE.md` | This file. |
| `.trusty-mpm.toml` | `profile = "supervisor"`. |
| `scripts/` | The deterministic poller and its helpers. `scripts/start-fleet-poll.sh` starts `scripts/fleet-poll.py` in tmux session `<this session>-poll` (`tm fleet status --json` names this session); `scripts/input-state.py` classifies a pane's input box. |
| `.claude/skills/` | `tm-fleet-check` and `tm-context-refresh`. |
| `inbox/` | Poller runtime, not tracked: `alerts.jsonl`, `poll-state.json`, `poll.log`, and `events.jsonl` once the watched projects' hooks write it. |
| `records/` | Your records, tracked by this project's local git repository. |

`tm fleet init` writes every file above only when it is absent, and reports a
file you edited as skipped. It never overwrites your changes.

## Records

- `records/state.md`: `poll_count`, `last_consumed_ts` (events),
  `last_alert_ts` (alerts), the pending questions, and the context-refresh
  queue.
- `records/actions.md`: one dated entry per pass, newest last: the mode, what
  you sent and where, and what is still pending.
- `records/projects/<project>.md`: one file per watched project: its state,
  its rulings, its snapshots, each fact labelled reported or verified.

Commit the records at the end of every pass. The repository is local: never
add a remote.
