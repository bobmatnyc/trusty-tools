Documentation

- `launchd_labels`'s module header links `RETIRED_SERVICES` again. The header is
  merged with the `///` doc on `pub mod launchd_labels;` in `lib.rs`, so rustdoc
  resolves it in the crate root, where a bare `RETIRED_SERVICES` does not exist —
  the link rendered as dead literal text and denied the crate's `cargo doc` build.
  It now has the same `crate::`-rooted reference definition `SERVICES` and
  `canonical_label` were given in #5753.
