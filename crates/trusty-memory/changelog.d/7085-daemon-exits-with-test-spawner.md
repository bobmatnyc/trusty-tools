Fixed

- A `serve --foreground` daemon spawned by a test no longer survives a SIGKILL of the test process. The daemon arms `trusty_common::parent_death` when the spawner stamped `TRUSTY_EXIT_WITH_PARENT` and exits within a second of being reparented; 102 orphaned debug-build daemons holding 12.6 GB had accumulated because `DaemonGuard`'s `Drop` never runs when the parent is killed outright. Launchd-supervised and hand-run daemons are untouched — nothing arms unless the env var is set (#7085).
