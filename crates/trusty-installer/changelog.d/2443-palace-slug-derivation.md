Fixed

- `ensure`'s basename-derived palace name is clamped to `PALACE_ID_MAX_LEN`, so a project directory with a long name no longer posts a name the daemon's format gate rejects ([#2443](https://github.com/bobmatnyc/trusty-tools/issues/2443))
