Fixed
- The poller raises `plan-limit` only for the harness's own usage-limit banner at the start of a line, not for chat text such as "GitHub rate limit resets at 06:38Z".
- One idle episode raises one `idle` alert: a single not-idle read between two idle reads at most 150 s apart keeps the episode's epoch.
- An idle epoch that has already alerted does not alert again on a changed screen until that screen has itself been idle for 10 minutes.
- Token-watch alerts keep a session's resolved tmux name when the name lookup fails for one cycle, so the `ctx_main` label and the Architect's bucket swap stay correct.
