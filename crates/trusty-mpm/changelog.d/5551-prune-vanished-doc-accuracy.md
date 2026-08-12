Documentation

- Corrected `prune_vanished`'s doc comment in `core/mcp_provenance.rs`, which
  claimed the sweep "retains only entries whose path still exists"
  ([#5551](https://github.com/bobmatnyc/trusty-tools/issues/5551)). It also
  retains an entry it could not probe — a deliberate fail-open, since dropping
  an uncertain provenance claim risks misattributing a later `.mcp.json` at the
  same path. Doc-only; the behavior is unchanged.
