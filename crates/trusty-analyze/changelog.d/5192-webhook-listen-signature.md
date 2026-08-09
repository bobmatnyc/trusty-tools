Breaking
- `webhook_listener::run` now takes a `TrustySearchClient`, which it needs to run the analysis pipeline. Callers must pass the client they already build from `--search-url` (#5192).
