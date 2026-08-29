Added

- `TRUSTY_SEARCH_SOCKET` pins the trusty-search daemon's socket path for a probe,
  replacing `TRUSTY_SEARCH_ADDR`. It exists for a test rig or a CI job pointing
  at a daemon it started itself; `TRUSTY_DATA_DIR_OVERRIDE` is process-global and
  would redirect every other trusty-* client in the same process.
