Fixed
- Qualified the `docgen` module's own doc links to `assert_region` and
  `sync_region`, which rustdoc could not resolve as bare names. #5744's
  `#![deny(rustdoc::broken_intra_doc_links)]` turned them into errors, and
  because the `docgen` feature is enabled only from `[dev-dependencies]` the
  workspace-wide link gate never documented the module — so the two broke
  `scripts/check_contracts.sh` on `main` for everyone instead.
