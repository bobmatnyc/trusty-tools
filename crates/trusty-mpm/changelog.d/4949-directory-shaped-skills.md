Fixed

- User-tier skills authored as a directory (`~/.trusty-mpm/skills/<name>/SKILL.md`) now deploy into every project (closes [#4949](https://github.com/bobmatnyc/trusty-tools/issues/4949))
  - they were silently rejected by the source scan, so `duetto-design-system` and `cto-kb-ingest` were hidden from every project's skill manifest
  - a skill directory with no `SKILL.md` is now named in the deploy warning instead of vanishing
