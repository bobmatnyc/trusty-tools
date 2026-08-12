Fixed

- `kg-rebuild` no longer prints `[ok]` for a palace whose triple assertions all failed. `rebuild_one` hardcoded `error: None`, so every failed `kg.assert` lived only in a `tracing::warn!` line and the summary read the same whether all triples landed or none did — the state a running daemon produces, since a read-only snapshot open rejects every assert. Failures are now counted and the summary carries `N of M extracted triple(s) failed to assert; first: <subject> --<predicate>-->: <error>` (#5531).
