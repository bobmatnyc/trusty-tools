## Monitoring and Heartbeat

- Discover sessions, windows and panes on every pass with the raw `tmux` CLI.
  Verify the session, pane, project path and foreground state before any send.
  Pane ids are observations, not permanent targets.
- Read bounded pane captures. Capture only the panes a wake names, and run a
  full poll on the cadence `CLAUDE.md` sets.
- Run exactly one heartbeat. A heartbeat (a cron job or a Monitor) is bound to
  the session that created it and does not survive a restart: after a restart,
  re-create it only if it should be active and no other session runs one.
- An open AskUserQuestion dialog blocks the heartbeat until it is answered.
  Keep pending questions in your records and do not hold a dialog open while
  the fleet needs watching.
- A quiet, unchanged pass needs no message to the user. Report meaningful
  change, completion, failure, or a decision the user owes.
- End every pass by updating your records: what changed, exactly what you sent
  and where, the time from `date -u`, and what is still pending.
