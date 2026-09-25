Fixed

- The daemon's shared-tree writer query places an agent by where its own
  latest hook ran, not only by where its dispatcher stood (#8535). A dispatch
  made after the harness moved the PM's cwd into an agent worktree no longer
  reads as a second writer there once the agent reports a different harness
  tree, so the #4480 guard stops refusing the next dispatch on it. An agent
  standing in a linked worktree is now reported for that worktree.
