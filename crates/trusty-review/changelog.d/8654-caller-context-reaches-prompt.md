Fixed

- PR context now reaches the reviewer from `trusty-review run` and the MCP
  `review_diff` tool. `review_diff` passes its `context` argument as the PR
  description instead of writing it onto the diff, where the parser dropped it
  as unattributable content.
- A diff whose first line starts with `# Context:` no longer loses that block.
  The lines before the first file header are read as the PR description and
  removed from the diff. Other unattributable preambles still log the
  "could not be attributed" warning.
- Each caller-context field (PR description, PR discussion, referenced code) is
  capped at 64,000 characters before the reviewer and verifier see it, on
  every surface. A cut field ends with a `[... truncated: N more characters
  omitted ...]` marker.
