Fixed

- The pm-guard no longer refuses a script whose prose names `op` before a later backtick or `$`. In shell text a bare `op` now trips the run-time-subcommand rule only when its subcommand slot is `$`, after any option and its value (`op --vault x $SUB` still refuses); a Python argv list keeps its earlier reading. The seeded `il_remind.py` passes (#11800).
- `token-watch-state.json` and `token-thresholds.json` read as token tallies, not token files; `token.json`, `token-credentials.json` and `token-watch.pem.json` still refuse (#11800).
