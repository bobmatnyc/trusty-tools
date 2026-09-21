Fixed
- `probe_socket` still reports a refused daemon as `Refused` now that a dial failure can arrive wrapped in the shared client's bounded-retry error (#8267).
