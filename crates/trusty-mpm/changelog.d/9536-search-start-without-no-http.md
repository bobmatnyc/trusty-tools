Fixed
- The default `tm services` manifest starts and restarts trusty-search with plain `trusty-search start`. It passed `--no-http`, which `trusty-search start` now rejects (#9536).
- `tm services` strips `--no-http`, `--port <n>` and `--port=<n>` from a `trusty-search start` in a loaded services.yaml's trusty-search `start_cmd` and `restart_cmd`, so a manifest written by trusty-mpm 1.8.0–1.8.2 still restarts trusty-search. It warns once and leaves the file unchanged (#9536).
