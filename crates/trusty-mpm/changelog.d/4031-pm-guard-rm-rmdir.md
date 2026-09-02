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
