Added
- `tm fleet init` seeds `scripts/architect_config.py` and a `local/architect.json` config with no owner values; the loader reads owner name, repos, dashboard port, memory palace and caps, lets a variable override the file, and fails on malformed JSON or a wrong type instead of falling back to defaults (Refs #10161)
