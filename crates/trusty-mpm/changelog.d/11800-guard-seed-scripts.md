Fixed

- `token-watch-state.json` and `token-thresholds.json` read as token tallies, not token files, so the pm-guard no longer refuses a command or script that names them; `token.json`, `token-credentials.json` and `token-watch.pem.json` still refuse (#11800).
