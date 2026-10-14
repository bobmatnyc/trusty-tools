# Current Architect state

Snapshot, rewritten in place each pass. History goes in `local/actions.md`.

## Heartbeat and inbox bookkeeping
- monitoring: PAUSED (set to ACTIVE with the cron job id and Monitor task id once started)
- observation channel: tmux CLI
- socket: <from `tmux display-message -p '#{socket_path}'`>
- last_consumed_ts: <newest inbox event ts acted on, e.g. 1970-01-01T00:00:00Z>
- poll_count: 0
- last full poll: never
- NEXT IDS G01 P01 A01 D01 T01 S01

## Watch set
Sessions the Architect manages. Everything else is observed only if it signals.
- <tm-session-name> — <project owner/repo>

## Dashboard

| Project | tmux session / pane | Context | Current observation / next action |
| --- | --- | --- | --- |

## Pending for the owner
- none

## Improvement candidates
- none

## Host performance
- <latest load, CPU idle, memory free, swap, tmux priority, plan usage>

## Context refresh queue
- empty
