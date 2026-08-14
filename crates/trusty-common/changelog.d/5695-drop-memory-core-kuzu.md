Removed

- The `memory-core-kuzu` feature and the `memory_core::store::kuzu` module it
  gated ([#5695](https://github.com/bobmatnyc/trusty-tools/issues/5695)). No
  workspace member enabled the feature, so it was reachable only through
  `--all-features` — the path `cargo-semver-checks` takes — where it forced a
  cmake source build of `kuzu` and stopped the SemVer gate from running at all.
  Nothing was lost with it: the feature-gated body was a single `warn!()` behind
  a `TODO(kuzu)`, `query()` and `recall()` returned an empty vec whether or not
  the feature was on, and no `use kuzu::` existed anywhere in the tree.
  `KuzuSource`, `KuzuDatabase`, and the unconditional `discover()` /
  `default_roots()` scanners go with the module; they had no caller outside the
  module's own tests and were never re-exported from `store`.
- The `kuzu` workspace dependency, now that nothing declares it.
