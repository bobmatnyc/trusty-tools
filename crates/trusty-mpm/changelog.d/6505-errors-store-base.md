Fixed

- `GET /api/v1/errors` and `mpm.errors.list` now read their four daemon error
  stores from a base the daemon pins at construction, instead of re-resolving
  the OS data directory on every call. Production resolution is unchanged; a
  daemon built against an explicit framework root reads its stores under that
  root, so two calls can no longer disagree because a process-global
  `TRUSTY_DATA_DIR_OVERRIDE` moved between them (#6505).
