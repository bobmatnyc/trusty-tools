Fixed

- A daemon-launched session composes its PM instructions from its own directory: the trusty-tools checkout or worktree it was launched in, otherwise the pinned installed content release. A launch redirected to run in the managed checkout no longer reads that checkout's working tree, and no daemon launch reads a checkout found from the daemon's cwd.
- A session launched in a linked git worktree of the managed checkout runs in that worktree and composes its prompt from that worktree's `content/`. A removed worktree, or one whose git admin directory is gone, falls back to the managed checkout with a logged warning.
