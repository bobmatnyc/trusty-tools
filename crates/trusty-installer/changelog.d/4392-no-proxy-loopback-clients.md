Fixed
- `tctl ensure`, `tctl port`, the readiness wait and project setup no longer
  fail against a healthy daemon when `HTTP_PROXY` is exported. `ensure`'s
  shared `build_client` now routes through
  `trusty_common::http_client::loopback_client_builder`, which applies
  `.no_proxy()`. `probe_http`'s own client routes through the same entry point,
  so the property this crate proved for `/health` in #4246 cannot drift from
  the rest of the workspace's loopback callers (#4392).
