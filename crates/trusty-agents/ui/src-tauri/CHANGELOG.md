# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- **Per-message agent attribution in the chat stream (#3737, epic #3052):**
  every assistant bubble is now labeled with the display name of the persona
  that produced it — "Assistant" for the default tools-armed path, or the
  roster persona's friendly name ("Izzie", "CTO Assistant") when one is
  selected — stamped per-message at send time, so a mid-conversation persona
  switch relabels only later bubbles. The roster switcher likewise shows
  friendly display names (sourced from the new `GET /api/agents` `display_name`
  field) rather than dispatchable slugs.

### Fixed

- **The `tagent --api` sidecar is now reaped on quit (#3372):** quitting the
  desktop app (tray "Quit", Cmd+Q, app-menu Quit, or an AppleScript `quit`)
  previously left the sidecar running and holding its fixed port, so repeated
  GUI restarts accumulated orphaned processes and port conflicts. The
  `RunEvent::ExitRequested` path no longer merely signals the child and returns
  without awaiting the reap. Termination is now graceful-then-forced: a SIGTERM
  lets the sidecar unbind its listener, and if it hasn't exited within a short
  grace window it is SIGKILLed; either way the child is `wait`ed (reaped) before
  the app actually exits, so no orphan survives a normal quit. An
  already-exited child is handled without panicking. The new `terminate_child`
  helper carries this logic and is unit-tested against real child processes
  (no display server required).
