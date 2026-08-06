Changed

- Checkout/worktree discipline moved out of root `CLAUDE.md` and into `tm-pr-workflow`'s "Worktree Discipline" section, per the owner's ruling that checkout behavior belongs in the workflow skill, not the project instructions
  - generic rules (main checkout inspection-only, branch off `origin/main` not local main, one branch per reviewable PR outcome, worktree-is-ephemeral/branch-is-durable, cleanup order, subagent confinement) now live in the skill so every project using `tm-pr-workflow` gets them, not just this repo
  - the Rust/macOS-specific bits (the `cargo install` cdhash cache hazard, install-from-worktree commands) moved to `docs/reference/worktree-discipline.md`, which already existed and is already linked from `CLAUDE.md`
  - `CLAUDE.md` keeps a 3-line pointer naming the skill and the reference doc in place of the ~95-line section
