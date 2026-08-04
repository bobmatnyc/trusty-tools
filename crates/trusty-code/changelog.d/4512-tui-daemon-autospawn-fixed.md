Fixed

- **`tcode tui` refuses to attach to a daemon serving a different project
  ([#4512](https://github.com/bobmatnyc/trusty-tools/issues/4512)).**
  Auto-attach picks daemons up off a well-known address, so without a check a
  TUI launched in project B would silently drive project A's daemon and every
  session, index, and file operation would land in the wrong repository. The
  client now compares its own project against the binding the daemon reports on
  `/health` and, on a mismatch, fails with an error naming BOTH projects and
  the ways forward. It deliberately does NOT fall back to spawning a competing
  daemon on a port that is already in use. Projectless and project-bound are a
  mismatch in either direction: attaching a projectless TUI to a bound daemon
  would grant it a project nobody named, and attaching a bound TUI to a
  projectless daemon would withdraw the indexing and git affordances the
  operator asked for. A daemon too old to report a binding is refused as well
  (fail-closed — "old build" is no evidence the project is right), with a
  message saying to restart it.
