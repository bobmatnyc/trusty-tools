Added

- `tm ls -a` / `tm ls --attached` lists only the sessions a tmux client is attached to right now
  - "attached" is tmux's own `#{session_attached}`, surfaced by the daemon as the per-session `attached` flag — a session that is live and reported `active` with nobody connected to it is EXCLUDED, which is the whole point of the flag
  - forces the static table for the same reason `--all` does: a session you already have a client on is the one target the picker cannot usefully connect you to
  - matching nothing prints `no attached sessions` (or `no attached sessions for <slug>` when scoped) and exits 0 — an empty result reads as a real answer, not a failure or an empty table
  - no effect on `--json` (the raw daemon response stays complete, matching `--all` and the filter terms) or on `--projects`; `tm sessions ls` and `tm f` are unchanged
  - `-a` NARROWS here, inverting the `ls -a` / `docker ps -a` convention where it widens; `--all` remains long-only and the two flags are independent
