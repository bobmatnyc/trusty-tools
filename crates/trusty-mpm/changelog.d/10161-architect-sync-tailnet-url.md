Added
- The Architect's `scripts/architect_config.py` reads a `tailnet_url` key from `local/architect.json`, overridden by `DASHBOARD_TAILNET_URL`: an `http://` or `https://` dashboard origin, empty by default; any other value fails with an error that names the key, never the value (Refs #10161)
