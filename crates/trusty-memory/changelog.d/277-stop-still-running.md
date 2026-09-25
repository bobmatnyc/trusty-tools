Fixed
- `trusty-memory stop` exits non-zero when a daemon is still alive after SIGKILL. It used to print a warning and exit 0, so a script that stopped the daemon before an import went on against a live daemon.
