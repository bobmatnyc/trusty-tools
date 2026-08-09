Changed

- A session now starts in the project's main checkout instead of provisioning
  its own git worktree. The project's `worktree` setting no longer decides this
  — it decides whether the AGENTS a session dispatches get isolated, and a
  project registered `worktree: true` still isolates them exactly as before.
