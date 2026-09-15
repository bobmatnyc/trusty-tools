Fixed
- Every `tagent` start now drains the legacy `[[listeners]]` table in `~/.trusty-agents/config.toml` into `[[channels]]`. The drain previously ran only from the REPL routing command, so a daemon absorbed the legacy table in memory and left the file unchanged across every restart, and the `route_to` backfill had no channel entry to name the bound assistant in (#7609).
