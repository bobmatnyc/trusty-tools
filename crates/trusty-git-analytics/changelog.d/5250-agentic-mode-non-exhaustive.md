Breaking
- `AgenticMode` is now `#[non_exhaustive]` and gained a variant, so external
  `match` sites need a `_ =>` arm. tga goes to 3.0.0: adding a variant to an
  exhaustive public enum is a major break under `cargo-semver-checks`, and the
  attribute keeps the next addition (#5251) a minor bump.
