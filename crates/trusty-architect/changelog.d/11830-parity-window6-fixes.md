Fixed

- The scheduler's shadow reseeds its token-watch state from the live one at every shadow daemon start, under the live watcher's lock, and fails closed (keeping its own state) when the live file cannot be read or parsed, so a restart after downtime no longer replays old token alerts (#11826).
- The Rust watchdog applies the background-agent wait rule: an Architect waiting on background agents is not `architect_frozen` until the wait has sat unchanged for 60 minutes (#11826).
- `parity` leaves out `meta_due`, `meta_miss` and `factory_send_maxed`, which a dry-run poll never raises, and prints their counts on their own line (#11826).
- `parity` folds an idle alert into the same episode's previous one when it follows within two polls (a transient screen), and prints the folded counts overall and per kind; it pairs `session_exited` without its per-poller first-seen time, and pairs a `token` event the Python watcher labelled `<project>:<sid>` with the same event under its session name (#11826).
