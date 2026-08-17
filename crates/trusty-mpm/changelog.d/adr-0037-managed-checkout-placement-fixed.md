Fixed

- **A launch with no worktree request now runs in the managed checkout (`<workspace-root>/<owner>/<repo>`), not in whatever directory `tm` was typed in** — launching from an unmanaged clone switches to the managed checkout and provisions it when absent. The unmanaged tree is never written to, and a managed checkout that cannot be provisioned fails the launch instead of falling back to it (ADR-0037 terminology clarification)
- **An explicit `--worktree` launch whose worktree cannot be established now fails** — it previously logged a warning and spawned a session with no worktree at all, reporting success for a placement nobody asked for
