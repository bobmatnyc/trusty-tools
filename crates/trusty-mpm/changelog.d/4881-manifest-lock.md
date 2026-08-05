Fixed

- take the skill ledger lock in the two trusty-mpm writers that also read-modify-write it — `tm doctor --fix-skills` repair and `tm catalog apply --prune` (closes [#4881](https://github.com/bobmatnyc/trusty-tools/issues/4881))
  - a tier directory that does not exist is skipped before the lock, so a repair never creates an empty `skills/` directory in a project it was only inspecting
  - skill-drift auditing no longer counts the lock sidecar as deployed content when deciding whether a manifest-less tier is populated
