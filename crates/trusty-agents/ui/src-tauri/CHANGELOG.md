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
  switch relabels only later bubbles. Attribution reflects who ANSWERED, not
  who was asked: when a turn delegates (base "Assistant" hands a weather
  question to Izzie), the server reports the responding specialist in
  `PmResponse.responder_agent` and the bubble is relabeled to that agent on
  `task-complete`. The roster switcher likewise shows friendly display names
  (sourced from the new `GET /api/agents` `display_name` field) rather than
  dispatchable slugs.

- **Clearable "Recent tasks" (#3737, epic #3052):** the sidebar's "Recent
  tasks" panel gains a two-step "Clear" affordance (arm → confirm) wired
  through a new `clear_recent_tasks` command to `DELETE /api/tasks`. It clears
  finished tasks only and keeps any in-flight task visible; it is two-step
  because the task history is persisted across restarts.

### Fixed

- **Cmd+Q now actually reaps the sidecar; sidecar self-exits if the GUI dies
  any other way (#3734):** the #3372 reap was DEFERRED onto an async task
  (`prevent_exit()` + `spawn` + `handle.exit(0)`), which never ran on macOS
  Cmd+Q — that quit is a synchronous AppKit teardown that exits the process in
  ~3ms, before the task is polled, so the sidecar was orphaned and kept holding
  port 8765 (live-reproduced). The quit reap is now SYNCHRONOUS: the
  `RunEvent::ExitRequested`/`Exit` handler blocks on `kill_sidecar` for a short
  bounded window (500ms) so SIGTERM→(wait)→SIGKILL completes before control
  returns to AppKit — no `prevent_exit`/`handle.exit` dance. As a guaranteed
  backstop for every GUI failure mode the Tauri event loop can miss (Cmd+Q race,
  crash, external SIGTERM/SIGKILL), the GUI now spawns the sidecar with
  `--parent-pid <gui_pid>`, arming the sidecar's own parent-death watchdog
  (trusty-agents #3734). The tray "Quit" item is simplified to route through the
  same single synchronous reap.

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
