# demo-crate

A bare `src/…` citation in a crate README is crate-relative, so the gate
resolves it against this directory rather than against the root.

- `src/lib.rs` exists here and must pass.
- `src/core/migration/m001.rs` does not and must fail — the case issue #5147
  found five times in the trusty-search agent instructions.
