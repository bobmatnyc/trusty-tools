Removed

- The unused `commands::daemon_lock` module (`acquire_lock`, `DaemonLock`, `read_lock_pid`, `pid_alive`, `lock_file_path`). Nothing has called it since the daemon moved to a Unix socket (#6286), and its empty-file window let two callers both take the lock; the socket's bind lock is the daemon lock now (refs [#8759](https://github.com/bobmatnyc/trusty-tools/issues/8759))
