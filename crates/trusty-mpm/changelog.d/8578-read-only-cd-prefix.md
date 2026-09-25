Fixed

- `tm hook --pm-guard` lets a read-only agent (`research`, `code-critic`,
  `code-analyzer`, `security`, `Explore`, `Plan`) run one leading
  `cd <dir> && <read>`, so it can scan a worktree other than the PM's cwd.
  The directory must be a plain path: a `$VAR`, `~`, `$(…)` or backtick is
  refused. What follows the `cd` is judged exactly as it would be alone, so
  `cd <dir> && rm …` and `cd <dir> && git diff > file` stay refused, and no
  other `&&`, `;` or `|` chaining is widened. The refusal text now names
  `git -C <dir>` and the `cd` prefix as the ways to read another tree.
  Refs #8578.
