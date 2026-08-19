Fixed
- `trusty-analyze health`, `daemon status` and the `setup` readiness poll no
  longer report a healthy daemon as DOWN when `HTTP_PROXY` is exported. All
  three build through `trusty_common::http_client`, which applies `.no_proxy()`
  (#4392).
