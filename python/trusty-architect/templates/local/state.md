# State

The Architect's running state, rewritten in place at the end of every pass.
History goes in `local/actions.md`.

- poll_count: 0
- last_consumed_ts: none
- last_alert_ts: none
- last full poll: never
- observation channel: tmux CLI

## Heartbeat

- monitoring: PAUSED (set ACTIVE, with the cron job id and the Monitor task
  id, once started)
- tmux socket: <from `tmux display-message -p '#{socket_path}'`>

## Watch set

The sessions you manage are listed in `CLAUDE.md`. Everything else is
observed only when it signals. Note here a session added or dropped this pass
until `CLAUDE.md` carries it.

None.

## Dashboard

| Project | tmux session / pane | Context | Current observation / next action |
| --- | --- | --- | --- |

## Pending questions

None.

## Context-refresh queue

None.

## Improvement candidates

None.
