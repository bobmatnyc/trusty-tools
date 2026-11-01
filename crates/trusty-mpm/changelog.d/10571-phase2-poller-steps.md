Added
- The seeded Architect poller runs the Architect watchdog, the Intent Ledger cadence jobs (`il-cron.py`) and the clear-signal check (`ctx-watch.py`) every cycle, and `tm fleet init` seeds `local/il-schedule.json` with the prune, rotate and size-check jobs and the clear thresholds, keeping an edited copy (#10571).
