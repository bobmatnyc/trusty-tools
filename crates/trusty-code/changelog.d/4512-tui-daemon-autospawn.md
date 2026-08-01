Changed

- **`tcode tui` now starts its own daemon instead of exiting when none is
  running ([#4512](https://github.com/bobmatnyc/trusty-tools/issues/4512)).**
  The subcommand shipped in #4424 (PR #4433) requiring an already-running
  `tcode serve --http`: discovery failed, an actionable "start one first"
  message printed, and the command exited. DOC-50 §4.1 had deferred auto-spawn
  to Phase 2; that deferral is reversed — an interactive command that demands
  the operator hand-start a background service first is not a shippable
  first-run experience.
  - Discovery is UNCHANGED: `TCODE_DAEMON_URL`, then the `http_addr` discovery
    file, each verified with a `GET /health` liveness ping. A live daemon is
    attached to exactly as before.
  - A missing or stale discovery file now spawns `tcode serve --http` as a
    child, forwarding `--project <path>` when the TUI has one and omitting it
    for a projectless session so the daemon's binding matches the TUI's. The
    binary is resolved via `std::env::current_exe()` (`cli::tcode_exe::resolve`),
    so a locally built binary spawns itself rather than a stale `PATH` copy.
    Readiness reuses the shared
    `trusty_common::daemon_guard::spin_until_ready` spinner rather than a fourth
    hand-rolled poll loop, raced against the child's exit so a daemon that fails
    to bind is reported immediately instead of spinning out the 20s budget.
  - **Ownership is tracked and enforced.** A daemon `tcode tui` spawned is owned
    by it and stopped when the REPL exits — SIGTERM plus a 5s grace period so
    axum drains in-flight requests (the connection-safe restart convention,
    #534), escalating to SIGKILL only if it ignores SIGTERM. A pre-existing
    daemon the TUI merely attached to is NEVER killed on exit. Teardown runs
    even when the REPL itself failed, so a spawned daemon cannot outlive its
    TUI.
  - A `TCODE_DAEMON_URL` that is set but unreachable is still an ERROR, not a
    spawn: starting a daemon at the default port would silently ignore an
    address the operator named explicitly. The message names the dead URL and
    both ways forward.
  - The spawned daemon's stdout/stderr go to
    `{data_dir}/trusty-code/tui-spawned-daemon.log` rather than being inherited
    (which would scribble across the alternate screen) or null-ed (which would
    make a failed startup undiagnosable); startup errors name that file.
  - New `cli::daemon_autospawn` holds the whole policy, keeping `cli::tui` the
    thin wiring file it was. `tui_client::discovery` gained `lookup_daemon` +
    `Lookup`/`Source`, the source-aware primitive `discover_daemon_url` is now
    a wrapper over — needed because auto-spawn must distinguish an explicit
    instruction it has to obey from a stale file it may replace.
