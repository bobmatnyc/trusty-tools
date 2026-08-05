Fixed

- serialise the skill manifest's read-modify-write so concurrent deploys stop freezing skills nobody edited (closes [#4881](https://github.com/bobmatnyc/trusty-tools/issues/4881))
  - `deploy_skills_filtered` and the unmanaged-skill adoption now run their whole load-modify-save under a new `with_skill_manifest_lock` sidecar lock, the skill-side counterpart of the agent ledger lock added in #4409
  - `SkillManifest::save_if_current` refuses a save whose base snapshot has gone stale rather than clobbering a newer record — defence in depth for any writer that bypasses the advisory lock
  - the `flock` critical section itself is now one implementation (`with_ledger_lock`) shared by the agent and skill ledgers
