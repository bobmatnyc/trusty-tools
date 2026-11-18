Fixed

- A daemon-launched session composes its PM instructions from its own directory: the trusty-tools checkout or worktree it was launched in, otherwise the pinned installed content release. A launch redirected to run in the managed checkout no longer reads that checkout's working tree, and no daemon launch reads a checkout found from the daemon's cwd.
