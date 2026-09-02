Fixed
- The PM bash guard now denies `rm`/`rmdir`/`unlink`/`find … -delete` when the
  resolved target is a filesystem root (`/`, `/root`, `/Users/<name>`,
  `$HOME`), a repository root, a `.git` directory, or a
  `.claude/worktrees`/`.worktrees` entry. Previously no `rm`/`rmdir` verb
  existed anywhere in the classifier, so a PM session — or an agent it
  dispatches — could delete a filesystem root or another session's worktree
  through Bash even though the guard already blocked `Edit`/`Write` on
  source. Ordinary cleanup (`rm stale.txt`, `cargo clean`, `git clean -fd`) is
  unaffected, and `git worktree remove` / `git branch -D` are untouched by
  this rule (#4031).
- The guard now also expands a literal `$PWD`/`${PWD}` target to the tracked
  effective working directory, evaluates the PARENT directory of a
  glob-suffixed target (`rm -rf ~/*`, `rm -rf /Users/<name>/*`, `rm -rf
  .[!.]*`) against the same denylist, and resolves a leading `\` or a
  `command`/`builtin` wrapper (`\rm`, `command rm`) to the real verb — closing
  four bypasses of the rule above found in review (#4031).
