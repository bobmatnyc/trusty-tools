Added
- `console_metrics` reports this daemon's own usage at schema version 5: `ram_bytes` (physical footprint), `disk_bytes` (the palace store's allocated size, matching `du -s`), `data_root`, an optional `ram_heap_bytes` / `ram_file_backed_bytes` / `ram_compressed_bytes` split where the OS supplies one, and a per-palace `disk_bytes`. Additive — a console built against schema 4 reads unchanged.
