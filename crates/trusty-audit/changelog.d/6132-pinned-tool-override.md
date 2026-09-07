Added

- An operator can now point the audit chain at a locally built binary for any of
  the four pinned tools, with one documented variable each:
  `TRUSTY_AUDIT_TGA_BIN`, `TRUSTY_AUDIT_SEARCH_BIN`, `TRUSTY_AUDIT_ANALYZE_BIN`
  and `TRUSTY_AUDIT_REVIEW_BIN`. Each takes an absolute path to an executable
  file; precedence is the override, then the pin, with no third branch. A
  variable naming a path that does not exist, is relative, or is not executable
  refuses the run naming the variable, rather than falling back to the pinned
  copy. An overridden tool is also excused from install, which is the case this
  exists for: a version that is merged but not yet published cannot be downloaded
  at all, so a merged fix previously could not be exercised through the chain
  without publishing it. The override is recorded in
  `state/tool-overrides.toml` and stamped into that tool's row of the `index.md`
  Versions table — led by `OVERRIDDEN`, naming the variable and the path, and
  claiming no version — in both the sweep's index and the return package's, so a
  run driven by a local build cannot be mistaken for a pinned one. New public
  module `tool_overrides`, new `tools::RequiredTool::override_env` and
  `tools::unsatisfied_with`, new `AuditError::ToolOverride` variant; the existing
  `TRUSTY_REVIEW_BIN` / `TRUSTY_SEARCH_BIN` / `TRUSTY_ANALYZE_BIN` plumbing onto
  sweep children is unchanged (issue #6132).
