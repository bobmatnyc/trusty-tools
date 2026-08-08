Fixed

- Skill deployment now accepts a directory-shaped skill (`<stem>/SKILL.md`), not only a flat `<stem>.md` (closes [#4949](https://github.com/bobmatnyc/trusty-tools/issues/4949))
  - the source scan tested `entry.file_type()?.is_file()`, so a directory-shaped skill was dropped with no warning on every deploy and the run still reported success
  - every file the skill carries — `metadata.json`, `references/**`, `scripts/**`, at any depth — now deploys and is recorded in the ownership manifest, so a multi-file skill is never half-tracked
  - a directory that carries no `SKILL.md` is reported by name in `DeployStats::skipped` and logged, instead of being skipped silently
  - `skills::tiers::list_source_stems` now calls the deployer's own scan rather than its own copy of the filter, so the planner and the deployer cannot disagree about which skills exist
