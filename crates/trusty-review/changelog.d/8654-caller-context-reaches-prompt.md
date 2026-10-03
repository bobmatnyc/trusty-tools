Fixed

- PR context now reaches the reviewer from `trusty-review run` and the MCP
  `review_diff` tool. `run` takes `--pr-description`, `--pr-discussion` and
  `--referenced-code`, each as literal text or `@<path>`; an unreadable file
  fails the run. `review_diff` passes its `context` argument as the PR
  description instead of writing it onto the diff, where the parser dropped it
  as unattributable content.
- A diff whose first line starts with `# Context:` no longer loses that block.
  The lines before the first file header are read as the PR description and
  removed from the diff. Other unattributable preambles still log the
  "could not be attributed" warning.
