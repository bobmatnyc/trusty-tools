Fixed

- `install_and_activate` gained a forced variant. A deploy replaces the binary
  behind a byte-identical plist, so the unchanged-unit skip that removes the
  reinstall outage would have let `make deploy` finish without ever activating
  what it built — launchd kept running the old image (#4868)
- Rollback no longer reports success it did not achieve, and no longer takes
  down a daemon it should have left alone. Liveness is captured before the plist
  is overwritten, because launchd keeps a job registered after its plist file is
  deleted — so "no previous plist" never meant "nothing was running". The
  restoring bootstrap's result is now checked, and a failed restore says the
  service is down instead of claiming it was preserved (#4868)
