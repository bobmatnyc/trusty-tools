Fixed

- `register_project_index_never_bypasses_sensitive_path_denylist` opts into real daemon writes with `TRUSTY_ALLOW_PRODUCTION_STATE=1`, so it keeps receiving the `POST /indexes` body it asserts on now that test processes are barred from writing to a live trusty-search daemon (refs [#4255](https://github.com/bobmatnyc/trusty-tools/issues/4255))
  - the override is scoped to the test and points at its own loopback socket, never the operator's daemon
