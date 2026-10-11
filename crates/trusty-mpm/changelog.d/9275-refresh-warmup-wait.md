Fixed
- `tm memory import --refresh` waits up to 180 s for a trusty-memory daemon whose embedder is still warming instead of failing every skipped drawer, and fails at once, naming the error, when the daemon reports that its warm-up failed (#9275).
