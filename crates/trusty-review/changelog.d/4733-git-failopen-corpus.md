Security

- a repository whose `git ls-files` merely FAILED no longer contributes a `.gitignore`-blind directory walk to the corpus sent to the LLM (closes [#4733](https://github.com/bobmatnyc/trusty-tools/issues/4733))
  - the walk consults a fixed skip-dir list and nothing else, so a repo with a stale worktree gitlink, `detected dubious ownership`, or an unreadable `.git` handed its ignored `.env` straight to the model; only a CORROBORATED "no repository here" still walks
  - the exit code is not a classifier — git exits 128 for every fatal alike — so the stderr is matched on the parenthesised `not a git repository (or any of the parent directories)` form only, and even that is corroborated against an ancestor `.git` witness before it is believed. `fatal: not a git repository: (null)` (a stale worktree pointer) contains the shorter phrase while meaning the opposite
  - a repository with an EMPTY tracked corpus is now an empty corpus, not a walk trigger: nothing committed yet still means a live `.gitignore`
  - anything git could not answer degrades loudly to no measured baseline rather than a leaky one
