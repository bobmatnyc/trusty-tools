Added

- `verbs::configured_interval` reads the poll cadence from `poll_interval_s` in `local/architect.json` (or the file `ARCHITECT_CONFIG` names), defaulting to 60 s, the same lookup as `architect_config.py`. A bad value, malformed file or missing named file is a usage error, never the default.
