Fixed
- A memory read (`memory_recall`, `memory_list`, `palace_info`) whose first exchange times out while trusty-memory re-opens an idle palace is retried once, and `tm memory remember`/`note`/`forget` warm the palace with a read first and send the write exactly once (#10648).
