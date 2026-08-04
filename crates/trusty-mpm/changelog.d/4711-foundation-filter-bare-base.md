Fixed

- the delegation roster no longer admits prompt fragments as delegatable agents (closes [#4711](https://github.com/bobmatnyc/trusty-tools/issues/4711))
  - `is_foundation_file` now matches a bare `base.md`, not only `base-*.md`
  - a file with no `name:` frontmatter is excluded outright — Claude Code dispatches by `name:`, so the old file-stem fallback advertised targets the harness could never resolve
