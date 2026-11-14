# Vendored calamine

- Upstream: `calamine` 0.36.1 from crates.io.
- `.crate` sha256: `5fa68281b1a76b54a62156474adb06bb380a67e07dd60656e3217152b42183f3`
  (the `checksum` of `calamine 0.36.1` in `workers/okf-extract/Cargo.lock`
  before #11093).
- Wired in by `[patch.crates-io]` in `workers/okf-extract/Cargo.toml` (#11093).

## Changes from upstream

1. `Cargo.toml`: the `zip` requirement is `"9.0.5"` (was `"8.6"`), to set the
   advisory-driven version floor.
2. `src/utils.rs`: `build_zip_path_cache` returns `ZipResult<HashMap<..>>`,
   because zip 9's `ZipArchive::file_names` yields `ZipResult<Cow<str>>`. Adds
   `use zip::result::ZipResult;`. Same shape as upstream calamine 0.37.0.
3. `src/xlsx/mod.rs`, `src/xlsb/mod.rs`: the `build_zip_path_cache` callers
   take `?` (`.unwrap()` in the one `xlsx` unit test).
4. Removed the registry files `Cargo.toml.orig`, `.cargo-ok` and
   `.cargo_vcs_info.json`.
5. Added this file.

No other file differs from the upstream crate. The `picture` feature is not
built by the worker and is not ported to zip 9.

## Removal

Remove when xberg accepts calamine >= 0.37.
Delete the 9 `workers/okf-extract/vendor/calamine/...` rows from
`.line-cap-allowlist.tsv` together with `vendor/`.
