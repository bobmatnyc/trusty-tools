Documentation

- The `daemon::rpc` module headers link their own items again. Each submodule
  carries a `///` doc on its `pub mod` line, which merges with the module's `//!`
  header and makes rustdoc resolve the whole doc in `daemon::rpc` — so `register`,
  `METHODS`, `DaemonError` and every `super::` path written inside a header
  rendered as dead literal text and denied `cargo doc`. Each header now ends with
  `crate::`-rooted reference definitions. `api.rs`'s links to
  `TmuxService::spawn_claude` and `PairingService::reset` are qualified the same
  way; `daemon::socket`'s private `build_router` is a code span, since docs.rs
  cannot link a private item.
