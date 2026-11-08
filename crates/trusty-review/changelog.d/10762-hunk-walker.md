Fixed
- Inline review comments no longer drop a hunk when a removed line starts with `-- ` (a SQL or Lua comment, shown as `--- …`) or an added line starts with `++ ` (shown as `+++ …`). The diff walker now reads each hunk by its `@@` line counts before it looks for file headers, so findings on the later lines of such a hunk anchor on the correct side instead of reading `off_diff` (#10762).
