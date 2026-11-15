Fixed
- A disclaimed spawn on macOS now starts its child with `POSIX_SPAWN_CLOEXEC_DEFAULT`, so the child keeps only its stdio and never inherits a stray parent descriptor (#10083).
