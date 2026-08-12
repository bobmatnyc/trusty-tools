Changed

- `code-critic` now posts its verdict (APPROVE/WARN/BLOCK) as a COMMENT-type
  GitHub review (`gh pr review --comment`) instead of a plain PR comment, so
  it groups under the PR's Reviews section and carries the same finding table
  and file:line citations as before. `--approve` and `--request-changes` are
  explicitly not used — both fail for a PR authored by the same identity the
  agent runs as; a failed review post is now reported as a loud failure
  rather than silently falling back to a plain comment.
