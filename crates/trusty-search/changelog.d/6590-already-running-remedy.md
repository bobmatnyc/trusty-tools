Fixed

- `trusty-search start`'s "daemon already running" refusal now names the remedy,
  not just the pid (#6590). When a truncated shutdown leaves launchd respawning
  the old binary, every subsequent start refuses against that orphan — one
  reinstall looped 17 times on a message whose only advice was `trusty-search
  stop`, which the operator had already run. The text now gives the signal to
  send (`kill -TERM <pid>`), the seconds to allow for the index-snapshot flush,
  and a warning against SIGKILL. Under launchd it says `KeepAlive` starts the
  replacement, so the operator does not race it with a manual `start`;
  unsupervised, it names the re-run.
