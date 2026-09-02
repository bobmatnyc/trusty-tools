Changed

- Bundled `tm-*` skills are deployed to the user tier only — they no longer land
  in `<project>/.claude/skills/`. The three sites that wrote that directory
  (`ensure_project_skill_tier`, `session_launch::skills`, `sync_assets`) decline
  the bundled tier via `project_skill_tier::bundled_excluded_from_project_tier`;
  the project tier still takes user-custom skills and still never overwrites a
  project-custom one. Owner ruling 2026-09-01, the same principle #4448 settled
  for bundled agents (#6586).
- `session_launch::skills` and `tm sessions sync-assets` deploy the bundled
  roster to the managed user tier instead, the destination
  `managed_config::ensure_managed_config_dir` already wrote on a daemon spawn. A
  bundled skill therefore still refreshes on every session prep and every
  sync-assets run, one tier up (#6586).
- `tm doctor`'s `deployment` check reads the managed user tier for bundled
  skills. It reported every bundled skill missing on a complete project before,
  and its `tm validate --repair` path can now close the gaps it reports — the
  probe and the repair had been reading and writing different tiers (#6586).
- A bundled skill left in a project tier by an older install no longer counts
  toward deployment completeness (#6586).
