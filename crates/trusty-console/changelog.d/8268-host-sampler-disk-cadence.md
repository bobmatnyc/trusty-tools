Changed
- `serve` gained `--disk-sample-interval` (default 15 s): disk metrics now refresh on their own slower clock while CPU, memory and network keep `--host-sample-interval` (1 s). The per-second volume walk was essentially the console's whole idle CPU cost on macOS.
