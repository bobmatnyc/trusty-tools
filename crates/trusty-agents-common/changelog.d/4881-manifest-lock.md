Fixed

- serialise the skill manifest's read-modify-write so concurrent deploys stop freezing skills nobody edited (closes [#4881](https://github.com/bobmatnyc/trusty-tools/issues/4881))
  - `deploy_skills_filtered` and the unmanaged-skill adoption now run their whole load-modify-save under a new `with_skill_manifest_lock` sidecar lock, the skill-side counterpart of the agent ledger lock added in #4409
  - `SkillManifest::save_merging` folds in any entries a writer that bypassed the lock published mid-run, instead of dropping them; it never fails or refuses after skill files are on disk, because bytes newer than their recorded checksum are exactly what the deployer reads as a hand-edit and skips forever
  - a manifest that exists but does not parse is no longer treated as an empty one — merging from that default would publish only the current run's entries and drop the rest
  - a mid-loop I/O failure during a deploy no longer skips the manifest save, so a skill written just before the failure stays tm-owned instead of freezing
  - the `flock` critical section is now one implementation (`with_ledger_lock`) shared by the agent and skill ledgers
