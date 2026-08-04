Fixed

- The generated LaunchAgent plist now sets `KeepAlive` to `true` instead of
  `{ SuccessfulExit: false }`, so launchd restarts the daemon after a **clean**
  (exit 0) shutdown as well as a crash. Previously a plain SIGTERM or orderly
  drain left trusty-search down indefinitely with no automatic recovery and no
  alarm, silently degrading every search-backed consumer
  ([#4113](https://github.com/bobmatnyc/trusty-tools/issues/4113)).
  - Deliberate "stop it and leave it stopped" is now expressed through launchd's
    unload path — `launchctl bootout gui/$(id -u)/com.trusty.trusty-search` or
    `trusty-search service uninstall` — which removes the job and therefore
    outranks any `KeepAlive` setting.
  - `trusty-search stop` now says so: on a host where the LaunchAgent is loaded
    it prints that launchd will restart the daemon shortly and names the
    `bootout` command that keeps it stopped.
  - An already-installed plist keeps the old policy until it is regenerated —
    re-run `trusty-search service install` to pick up the change.
