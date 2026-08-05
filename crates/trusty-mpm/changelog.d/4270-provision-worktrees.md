Changed

- new sessions provisioned from a repo URL now get their worktree at `<project-root>/.worktrees/<name>`, following the git convention, instead of nesting it inside a `.base` bare clone (closes [#4270](https://github.com/bobmatnyc/trusty-tools/issues/4270))
  - the base checkout is now the project directory's own clone, the same one the in-project spawn path establishes, so both paths share one base clone per project
  - existing `.base` stores are left exactly as they are — still discoverable via `git worktree list`, still working — and provisioning refuses loudly rather than moving or deleting one; retiring `.base` stays a manual operator step
  - a project directory holding git or trusty-mpm state (`.git`, `.base`, or `.worktrees`) is never offered for `mv`-aside recovery and never renamed by the old-layout migrator; the presence probe treats an unreadable entry as present, so a permissions blip cannot read as "no git dir"
  - the base clone's `.worktrees/` is excluded in `.git/info/exclude`, as the in-project path already does — without it `git clean -ffd` in the base deletes every session worktree including uncommitted work (single-force `clean` skips them; `-ff` does not)
