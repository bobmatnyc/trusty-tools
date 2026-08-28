Documentation

- The `service::rpc` module headers link their own items again. `error` and
  `reads` each carry a `///` doc on their `pub mod` line, which merges with the
  module's `//!` header and makes rustdoc resolve the whole doc in
  `service::rpc` — so `rpc_error_from_http`, `RpcError`, `CODE_UNAVAILABLE`,
  `CODE_UNAVAILABLE_PERMANENT` and `register` rendered as dead literal text and
  denied `cargo doc`. Each header now ends with fully-qualified reference
  definitions.
