Fixed
- The Architect dashboard now says when it runs on Python older than 3.11: a `WARNING` line on stderr at startup, a `warnings` list in `/config.json`, and a one-line banner on the page. Before, the Projects view silently lost its TOML stack and config rows (#10606).
- The dashboard LaunchAgent now picks the first `python3` that is 3.11 or newer among its candidates, and falls back to the first `python3` (with a warning from `tm fleet init`) when none is (#10606).
