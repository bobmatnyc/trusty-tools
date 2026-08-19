Documentation

- `http_client`'s module docs no longer break `cargo doc` for the whole
  workspace (#6027). The module carries docs in two places — the `//!` header in
  `http_client.rs` and the `///` block on `pub mod http_client;` in `lib.rs` —
  and rustdoc merges both into one doc string whose link-resolution scope comes
  from the first fragment, the `lib.rs` one. Every bare `[`loopback_client`]`-style
  link in the `//!` header therefore resolved against the crate root and was not
  found, and `#![deny(rustdoc::broken_intra_doc_links)]` turned that into
  `error: could not document trusty-common`. The five links are now
  crate-absolute; `blocking_loopback_client_builder` is intentionally left
  unlinked because it does not exist when the `blocking-http` feature is off.
