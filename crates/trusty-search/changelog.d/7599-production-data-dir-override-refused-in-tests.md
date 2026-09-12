Fixed

- Refuse a `TRUSTY_DATA_DIR` that names the operator's production data directory when the process is a `cargo test` binary, falling back to the isolated per-process test directory. The override was read before the #4255 test-harness guard, so anyone who exports it — the documented isolated-instance workflow — had no guard at all and every persisting handler under test wrote fixture indexes and roots into the live `indexes.toml` and `roots.toml`. Any other override is honoured unchanged (#7599).
