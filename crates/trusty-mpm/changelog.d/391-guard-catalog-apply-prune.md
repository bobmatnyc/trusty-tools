Fixed

- `tm catalog apply --prune` no longer deletes user content (closes [#391](https://github.com/bobmatnyc/trusty-tools/issues/391))
  - it removed any managed skill directory the current include/exclude rejected, with no checksum comparison, no frozen check and no backup — the only skill-mutating path with none of the guards `deploy_one_file` and `skill_repair` already have
  - a hand-edited (frozen) agent or skill is now left in place, as is a skill directory holding any file trusty-mpm did not deploy — `remove_dir_all` would have taken that file too
  - a skill the user-custom tier supplies is never pruned, whatever the bundled include/exclude says: `deploy_all_skill_tiers` deploys that tier in full and exempt from those rules, but its ledger entry is indistinguishable from a bundled one, so a checksum gate alone waved a pristine user skill through. The tier is derived from the live `~/.trusty-mpm/skills/` source rather than a new manifest field, so no existing ledger entry has to be guessed at
  - everything actually deleted is copied to `~/.trusty-mpm/backup-catalog-prune-<timestamp>/` first, and the command now prints what it kept and why
  - prune judges skill STEMS, not raw ledger keys: an `include` rule matching a stem but not its carried `<stem>/references/*.md` key used to drop that file's ledger entry while leaving the file on disk, after which the deployer read it as user-owned and never updated it again
