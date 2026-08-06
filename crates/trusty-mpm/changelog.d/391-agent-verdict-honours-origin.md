Fixed

- `tm catalog apply --prune` now reads an agent's ledger ownership tier, not just its checksum (completes the agent half of [#391](https://github.com/bobmatnyc/trusty-tools/issues/391))
  - the agent manifest records an `Origin` the skill manifest lacks, and `agents::deployer` already honours it — a non-`Bundled` entry is the seed-once tier and is preserved on a checksum mismatch. Prune read the checksum and not the tier, so a pristine user- or registry-owned agent could still be deleted by a bundled exclude rule: the hole the skill side closed by deriving the user tier from its source directory
  - latent today, because nothing writes a non-`Bundled` origin; pinned by a regression test that constructs the ledger entry directly
