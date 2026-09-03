Changed

- `InstallArgs` exposes only `output` and `force` publicly again. The 17 flag
  fields #5216 added at the unpublished 6.0.1 are `pub(crate)`: they are read by
  `commands::install` and `commands::install_flags` and by nothing else, and
  `main.rs` pattern-matches the whole struct without touching a field (#6744).
  This does not clear the 6.0.0 baseline — a struct that was exhaustively
  constructible cannot gain a private field without a major bump either, so
  `bash scripts/check_semver.sh --crate tga` still reports BREAK and tga still
  owes 7.0.0 at its next publish. What it buys is that the flag after this one
  costs nothing: from 7.0.0 on, `InstallArgs` is no longer constructible from
  outside the crate, so adding a field to it stops being a public-API change.
