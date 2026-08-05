Fixed

- the provisioner's base checkout now excludes `.worktrees/` in `.git/info/exclude`, as the in-project spawn path already did — without the entry `git clean -ffd` run in the project directory deletes every live session worktree and its uncommitted work (single-force `git clean -fd` skips them; the second `-f` does not) ([#4270](https://github.com/bobmatnyc/trusty-tools/issues/4270))
