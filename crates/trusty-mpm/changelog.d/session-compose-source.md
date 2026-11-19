Fixed

- A daemon-launched session composes its PM instructions from its own directory: the trusty-tools checkout or worktree it was launched in, otherwise the pinned installed content release. A launch redirected to run in the managed checkout no longer reads that checkout's working tree, The installed release outranks a checkout found from the daemon's cwd, which serves only when nothing is installed, so a session never fetches the content release where it did not before.
- A session launched in a linked git worktree of the managed checkout runs in that worktree and composes its prompt from that worktree's `content/`. A removed worktree, or one whose git admin directory is gone, falls back to the managed checkout with a logged warning.
