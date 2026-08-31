Fixed

- The `BundledAgents` and `BundledWorkflows` embed trees now carry an explicit
  allowlist, so a stray local file — a `.env`, an editor backup, one of
  `atomic_write`'s `.lock` sidecars — can no longer ship inside the `tagent`
  binary or be written into a user's `~/.trusty-agents/` (#5226). The filter is
  applied twice: `#[include]` globs at build time, and `is_bundled_asset` at
  deploy time, so a binary built before the globs existed still cannot
  materialise a stray file. The bundle stamp hashes the same filtered set.
