Fixed

A dispatched agent's worktree is now reclaimed when the session that dispatched
it ends, not only when its `SubagentStop` arrives. `SubagentStop` was the reap's
only trigger, so an agent killed by a session exit or restart, an interrupt, or a
dropped hook POST left its worktree registered and owner-known — and the orphan
sweep skips agent-owned trees by design, so nothing else ever reclaimed it. A
refusal is now logged at `warn!` and each session end reports how many trees it
reclaimed and how many it kept, so a leak the gates decline to clear is visible
instead of silent.
