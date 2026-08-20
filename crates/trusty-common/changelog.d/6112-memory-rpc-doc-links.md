Documentation

- `memory_rpc`'s own module doc (`//!`) no longer breaks `cargo doc` for the
  whole workspace. As with #6027's `http_client` fix, `memory_rpc.rs` carries
  docs in two places — the `//!` header in the file and the `///` block on
  `pub mod memory_rpc;` in `lib.rs` — and rustdoc merges both into one doc
  string whose link-resolution scope comes from the first fragment, the
  `lib.rs` one. The four bare `[`resolve_memory_base_url`]`-style links in the
  `//!` header therefore resolved against the crate root and were not found,
  which `#![deny(rustdoc::broken_intra_doc_links)]` turned into
  `error: could not document trusty-common` under the full-feature build
  `check_contracts.sh` runs. The four links are now crate-absolute
  (`memory_rpc::…`).
