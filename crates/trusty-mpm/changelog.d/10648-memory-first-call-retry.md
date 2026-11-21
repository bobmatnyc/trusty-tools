Fixed
- `tm memory recall` retries once when its first call times out while trusty-memory re-opens an idle palace, and `tm memory remember`/`note`/`forget` warm the palace with a read first and send the write exactly once (#10648).
