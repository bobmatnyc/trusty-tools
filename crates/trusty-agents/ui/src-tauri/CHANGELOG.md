# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- **Live Slack mirror reaches the desktop shell (#3752):** `sse_bridge.rs` now
  also forwards the two Slack-mirror event kinds (`slack_message_received` /
  `slack_reply_sent`) from the sidecar's `/api/events` as a `slack-event` Tauri
  event carrying the raw event object. Without this the `SlackMirror` pane would
  populate only in browser mode (the Tauri shell skips App.svelte's browser SSE
  bridge on `isDesktop()`), leaving the packaged demo app blank; App.svelte's
  `onMount` listens for `slack-event` and calls `pushSlackEvent`, a no-op in
  browser mode so exactly one push happens per event in each transport.

- **Live token streaming in the chat reply bubble (desktop + browser):** when
  the backend streams a reply (the new `agent_message_delta` SSE event), the
  in-flight assistant bubble now grows token-by-token instead of showing a
  spinner until the whole response lands. In **browser** mode `App.svelte`'s
  EventSource bridge forwards each fragment on a dedicated `task-delta` web-bus
  channel; in **Tauri desktop** mode — which never runs the browser EventSource
  — a new Rust-side SSE client (`src-tauri/src/sse_bridge.rs`) connects to the
  sidecar's `/api/events`, parses the frames, and re-emits the same `task-delta`
  Tauri event, so `ChatView` handles both transports identically. ChatView
  accumulates fragments (a new `lib/chatStream.ts` `StreamAccumulator`),
  suppresses the polling loop's "Running…" ticks while a task is streaming so
  they can't clobber the live text, keeps per-message speaker attribution
  (#3739) truthful mid-stream via the delta's `agent` field, and — on the
  terminal `done` marker or `task-complete` — finalizes the accumulator and
  replaces the accumulation with the authoritative `PmResponse` narrative
  (dedupe, no double render; progress ticks resume so a fallback's second
  blocking call isn't frozen). The poll loop remains the fallback whenever a
  stream is unavailable or fails. Unit-tested in `lib/chatStream.test.ts`
  (delta mapping, ordered accumulation, dedupe lifecycle) and
  `sse_bridge::tests` (frame parsing across split/multi-frame chunks).

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
  any other way (#3734):** the #3372 reap only matched `RunEvent::ExitRequested`
  and deferred the work onto an async task. In this tray app the window is never
  destroyed on Cmd+Q (close only hides it), so tao maps that quit to
  `applicationWillTerminate:` and emits `RunEvent::Exit`, NOT `ExitRequested` —
  #3728's handler never fired, and even if it had, the deferred task never ran
  before the ~3ms process teardown. The sidecar was orphaned and kept holding
  port 8765 (live-reproduced). Now the handler matches BOTH `ExitRequested` and
  `Exit` and reaps SYNCHRONOUSLY: it blocks on `kill_sidecar` for a short bounded
  window (500ms) so SIGTERM→(wait)→SIGKILL completes before control returns to
  AppKit — no `prevent_exit`/`handle.exit` dance. The tray "Quit" item routes
  through the same single synchronous reap.
- **Guaranteed backstop + no stale-sidecar adoption (#3734):** the GUI now
  spawns the sidecar with `--parent-pid <gui_pid>`, arming the sidecar's own
  parent-death watchdog (trusty-agents #3734) so no GUI failure mode the event
  loop can miss (crash, SIGKILL) leaves an orphan. The health probe is now
  parentage-aware: it accepts a sidecar as "healthy" only when it reports it is
  parented to THIS GUI (via the new `/api/health` `ppid` field), so a fresh GUI
  can no longer silently adopt — or mark itself ready against — a reparented
  orphan of a previous GUI still answering on the port during the watchdog's
  poll window. `ensure_api_server` then spawns its own sidecar and the boot
  retry heals once the orphan releases the port.

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
