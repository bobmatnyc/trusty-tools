Fixed
- `cargo install` invocations (`perform_upgrade` / `perform_upgrade_captured`,
  and everything routed through them — the installer's fallback paths, the
  in-daemon `upgrade` commands, and both MCP `upgrade` tools) now run under a
  cargo ownership guard: any cargo-untracked binary already sitting at the
  destination is atomically renamed aside first, restored on cargo failure,
  and restored when cargo's already-installed skip writes nothing. Without
  the guard, a downloader-placed binary makes `cargo install` exit 101 with
  "binary already exists in destination" (#5777, #4964 Phase 2). The guard
  threads each crate's FULL binary set via the new canonical
  `bin_resolve::installed_binaries` table, so alias and multi-binary crates
  (`tctl`, `tm`, `trusty-embedderd`, `tagent`, …) are covered — not just the
  binary named after the crate.

Changed
- `candidate_bin_dirs` (the install-dir list behind
  `verify_installed_binary`) now returns the single canonical cargo bin dir
  (`$CARGO_HOME/bin`, falling back to `~/.cargo/bin`). `~/.local/bin` is no
  longer consulted as an install destination — with every write path flipped
  onto the canonical dir (#4964 Phase 3), a stale legacy copy there must not
  satisfy the post-upgrade health gate. Legacy copies on `PATH` are still
  found via the `which` fallback (#5777).
