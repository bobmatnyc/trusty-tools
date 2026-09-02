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
- Verb detection for `rm`/`rmdir`/`unlink`/`find` no longer enumerates wrapper
  words at all — it scans every token of a Bash segment for the verb itself,
  so `env -i rm -rf /root`, `nice rm -rf /root`, `exec rm -rf /root`, and
  every other wrapper (known or future) deny the same as the plain form. A
  delete verb whose target cannot be resolved (an unparseable segment, or a
  bare invocation with no argument) now denies rather than silently allows.
  The denylist also covers bare container roots (`/Users`, `/home`,
  `/Volumes`, `/private`, `/var`, `/etc`, `/usr`, `/opt`, `/Library`,
  `/System`, `/Applications`, and `$HOME`'s parent), not just a single user's
  home. `shell_lex::git_subcommand` now shares the same wrapper-skip helper as
  the `rm` guard's `cd`-tracker, so `command git apply -`, `command git
  worktree remove --force <path>`, and `\git reset --hard` reach the existing
  git guards where their plain forms already denied (#4031).
- A specific user's home root now denies on Linux too — `rm -rf
  /home/<name>` and its glob-suffixed form, previously recognized only as
  `/Users/<name>` on macOS (#4031).
