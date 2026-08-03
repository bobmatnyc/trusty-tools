Changed

- `rust-engineer` agent now scopes its Quality Bar to the crate under change
  (`cargo test -p <crate>`, not a bare unscoped `cargo test`), matching the
  crate-scoped-gate guidance the PM already ships in `core.md`. A crate-scoped
  run finishes well under the 10-minute tool timeout; a workspace run does not.
  - Adds a change-class table for widening scope deliberately, and an explicit
    "scope is for speed, never for hiding a failure" rule: narrowing the scope
    you *run* is fine, shrinking the coverage that *exists* — `#[ignore]`,
    `cfg`-gating, `--exclude`, dropping to `--lib` — is never allowed.
