Fixed
- `tm issue epic sync` (and the sync `tm issue transition` runs on a phase) no longer overwrites a tracker edit made while it was reading. It re-reads the body before writing and starts again from fresh reads when the body moved; after three moved rounds it refuses and writes nothing (#9578).
