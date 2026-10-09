Fixed
- The `host-metrics` feature now enumerates Linux tmpfs mounts, so `host_metrics::mount_for_path` measures a path on a tmpfs `/tmp` instead of answering `None`, and the trusty-mpm worktree disk guard no longer refuses every worktree on such a host (#9523).
- trusty-console's disk gauge now lists and counts tmpfs mounts on Linux.
