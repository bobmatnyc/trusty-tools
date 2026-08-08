Fixed

- retiring a bundled skill now removes its deployed copy instead of orphaning it ([#5224](https://github.com/bobmatnyc/trusty-tools/issues/5224))
  - the skill deployer only ever writes, so a skill the binary stopped shipping kept its deployed directory and its ledger entry forever. Claude Code went on loading text that was deliberately removed, and the orphaned ledger key made `tm doctor`'s `skill_staleness` check report `Unknown` — a check reporting `Unknown` has stopped protecting anything
  - `tm install` and every session launch now sweep all three deploy tiers. A skill is retired only when no live source has it: not the compiled-in bundle, not the resolved bundled source, not `~/.trusty-mpm/skills/`, not the synced catalog, and not the project-custom stems already on disk
  - a user-tier or hand-placed project-tier skill is never removed, and a retired copy the operator edited — or one sharing its directory with a file trusty-mpm never deployed — keeps every file; only the ledger claim is released
  - deselecting a skill via a harness manifest still leaves its deployed copy alone; that remains `tm catalog apply --prune`'s business
