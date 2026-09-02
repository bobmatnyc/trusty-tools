Changed

- Bundled `tm-*` skills are deployed to the user tier only. The three sites that
  wrote a project's `.claude/skills/` — `ensure_project_skill_tier`,
  `session_launch::skills`, and `sync_assets` — now decline the bundled tier via
  `project_skill_tier::bundled_excluded_from_project_tier`; the project tier
  still takes user-custom skills and still never overwrites a project-custom one.
  Owner ruling 2026-09-01, the same principle #4448 settled for bundled agents
  (#6586).
- `tm doctor` gains `skill_project_tier`, which warns when a bundled skill is
  still deployed at a project's own tier and names `tm doctor --fix-skills`. It
  never deletes: a bundled-named file there could be a project-custom skill the
  operator wrote (#6586).
