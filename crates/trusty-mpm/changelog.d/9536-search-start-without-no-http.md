Fixed
- The default `tm services` manifest starts and restarts trusty-search with plain `trusty-search start`. It passed `--no-http`, which `trusty-search start` now rejects (#9536).
