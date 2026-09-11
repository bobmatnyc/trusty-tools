Fixed

- `BASE-ENGINEER.md`'s "Dependency Verification" section now names a fresh or unbuilt monorepo worktree's `Cannot find module '@scope/…'` wall as a missing-build precondition, not a type error (#7118).
- The same "Dependency Verification" section now states the once-before-first-gate install/build sequence, and that `isolation: "worktree"` provisions no dependencies (#7381).
- `BASE-AGENT.md`'s silent-skip verification bullet now names the Turborepo/Nx cache-hit false green — a `Cached: N cached` summary with N>0 re-ran nothing — and requires `Cached: 0 cached` or a forced (`--force`) run before trusting test counts (#7117).
- `version-control.md`'s merge section now covers the worktree/base-branch collision where `gh pr merge --delete-branch` fails post-merge with `fatal: '<branch>' is already used by worktree at <path>`, and gives the `tm pr merge --no-delete-branch` / confirm-then-`gh api -X DELETE` sequence (#7104).
- `BASE-ENGINEER.md`'s "Escape-Sensitive Edits" byte-exact-verification bullet now applies to every Write/Edit call, not only shell-routed ones, and names the git binary-reclassification failure signature (#7229).
