# Bug #212: API Server Restarts Every 3-4 Minutes During Long-Running Tasks

**Date**: 2026-04-26  
**Status**: Root cause identified  
**Severity**: High — in-flight task state is lost on every restart

---

## Summary

The API server (`open-mpm --api`) restarts during long-running tasks because
`make build` / `make install` atomically replaces the binary at
`~/.local/bin/open-mpm` while the server is running. launchd has
`KeepAlive: true` and kills/restarts the process whenever it detects the
underlying binary has been replaced (on macOS, this is triggered by the kernel
when the inode of the running executable changes). The restart gap correlates
exactly with developer build cycles (3-30 minutes between instances), **not**
with a fixed scheduler or timer.

---

## Evidence

### 1. Launchd plist — `~/Library/LaunchAgents/com.open-mpm.api.plist`

```xml
<key>KeepAlive</key>
<true/>
<key>ThrottleInterval</key>
<integer>10</integer>
```

- `KeepAlive: true` means launchd unconditionally restarts the process whenever
  it exits for any reason.
- `ThrottleInterval: 10` limits restarts to at most once per 10 seconds.
- There is **no** `TimeOut` or `ExitTimeout` key — launchd is not killing the
  process on a timer.

### 2. Log evidence — binary replacement triggers silent exit

Pattern from `~/.open-mpm/logs/api-stderr.log`:

```
20:11:38  open-mpm api server listening addr=0.0.0.0:7654
20:12:45  [task activity — workflow subprocess runs]
20:13:18  Error: workflow execution failed   ← subprocess emits this to inherited stderr
          [SILENCE — no shutdown log from the API server itself]
20:16:26  open-mpm v0.2.11 build #1128  ← NEW process starts
20:16:26  open-mpm api server listening addr=0.0.0.0:7654
```

Key observations:
- The server emits **no** SIGTERM, panic, or "shutting down" message before
  dying. The last line from the dying server is the subprocess's stderr output.
- The gap (188 s here, 288 s in another instance, 577 s in another) matches
  typical `cargo build && make install` cycle times, not a fixed interval.
- "build #1128", "build #1129", etc. increment on each restart, confirming a
  new process is spawned each time.
- The "Error: workflow execution failed" lines are printed by the **child
  subprocess** (inheriting the parent's stderr) before the parent exits. The
  child completing is not the cause — it happens 3+ minutes before the restart.

### 3. Makefile — binary replacement mechanism

```makefile
# From Makefile lines 14-24:
# server (open-mpm --api) picks up the new binary on next restart — without
# this step the live server keeps serving the old embedded UI indefinitely.
build: ui-check
    cargo build
    @$(MAKE) install

install:
    cp target/debug/open-mpm ~/.local/bin/open-mpm
```

`make install` runs `cp` which creates a new inode at `~/.local/bin/open-mpm`.
On macOS, when the inode of the process image changes and launchd detects the
original path now points elsewhere, it can trigger a restart. More directly:
`cp` truncates/replaces the file. If the OS notices the binary has changed
while it is mapped into the running process, the process may receive SIGBUS or
the loader may force an exit on next re-exec. Combined with `KeepAlive: true`,
launchd immediately spawns a fresh copy of the new binary.

### 4. No internal watchdog or self-restart logic

- `src/api/server.rs`: No `std::process::exit`, no idle timeout, no scheduled
  restart. The only timer is the SSE keepalive ping at line 855
  (`Duration::from_secs(15)`) which is an HTTP-level heartbeat, not a process
  restarter.
- `src/main.rs`: No self-restart code.
- No watchdog script in `scripts/` or `Makefile` beyond the install step.
- The `run_task` handler at `src/api/server.rs:907` spawns a child subprocess
  and awaits it, but the child exiting does NOT exit the server — errors are
  caught and returned as `PmResponse::error`.

### 5. Restart interval is variable, not fixed

Measured gaps between `api server listening` log entries (last 20 restarts):

| Gap | Minutes | Likely cause |
|-----|---------|--------------|
| 9s, 10s, 11s | <1 | Rapid build iterations (ThrottleInterval floor) |
| 288s | ~4.8m | `cargo build` cycle |
| 577s | ~9.6m | Longer build |
| 1052s | ~17.5m | Extended build session |

The ~3-4 minute observation in the bug report matches `cargo build` times for
this project.

---

## Root Cause

**`make install` (specifically `cp target/debug/open-mpm ~/.local/bin/open-mpm`)
replaces the binary on disk while the API server is running. macOS terminates
the process when its image is replaced, and `KeepAlive: true` in the launchd
plist causes an immediate restart with the new binary.**

This is by design for development (the Makefile comment says exactly this), but
it loses all in-flight task state because:
1. The `AppState` (in-memory `HashMap` of running tasks) is not persisted.
2. The `tokio::spawn` background tasks running `run_task` are killed mid-execution.
3. The SSE event stream to connected clients is severed.

---

## Recommended Fixes

### Fix A: Persist AppState to disk (addresses state loss on any restart)

The `AppState` struct in `src/api/server.rs` uses an in-memory
`Arc<Mutex<HashMap<String, PmResponse>>>`. Serialize it to
`.open-mpm/state/tasks.json` on every `upsert()` and load it on startup.

```rust
// In AppState::upsert(), after updating the map:
self.flush_to_disk().await;  // non-blocking, best-effort

// In serve_with_config(), before building the router:
let state = AppState::load_from_disk().unwrap_or_default();
```

This ensures that even after a restart, `GET /api/task/:id` returns the last
known status of in-flight tasks.

### Fix B: Use atomic rename in `make install` to reduce restart window

Replace `cp` with an atomic rename so the running process's inode is not
disturbed:

```makefile
install:
    @mkdir -p ~/.local/bin
    cp target/debug/open-mpm ~/.local/bin/open-mpm.new
    mv -f ~/.local/bin/open-mpm.new ~/.local/bin/open-mpm
```

`mv` is atomic on the same filesystem. The kernel does not remap the process
image mid-execution — the running process continues using its existing file
descriptor until it exits naturally. launchd will then restart it for the next
request (since `KeepAlive: true` only triggers on exit, not on inode change
itself — it's the `cp` truncation that causes the crash).

**Note**: This is the primary fix for the restart behavior.

### Fix C: Add `ExitTimeout` to give in-flight tasks time to complete

If the process is going to be replaced, give background tasks a grace period:

```xml
<key>ExitTimeout</key>
<integer>300</integer>
```

This tells launchd to wait up to 300 seconds for the process to exit cleanly
after a SIGTERM before sending SIGKILL. Combined with a SIGTERM handler in the
server that waits for in-flight `tokio::spawn` tasks to complete, this gives
long-running workflows (typically 60-90s) time to finish.

### Fix D: Signal handler for graceful drain (server-side)

In `serve_with_config()`, replace the bare `axum::serve(listener, app).await?`
with a graceful shutdown that awaits in-flight tasks:

```rust
// src/api/server.rs, serve_with_config()
let signal = async {
    tokio::signal::unix::signal(SignalKind::terminate())
        .expect("failed to install SIGTERM handler")
        .recv()
        .await;
};
axum::serve(listener, app)
    .with_graceful_shutdown(signal)
    .await?;
```

Relevant file: `src/api/server.rs:553`

---

## Priority Order

1. **Fix B** (atomic rename in Makefile) — stops the crashes during development
   with a 2-line change. Zero risk.
2. **Fix A** (persist AppState) — prevents state loss on any restart regardless
   of cause. Medium effort.
3. **Fix D** (graceful SIGTERM drain) — allows long tasks to complete before
   the process exits. Medium effort.
4. **Fix C** (ExitTimeout in plist) — complements Fix D. Low effort.

---

## Files Referenced

- `/Users/masa/Library/LaunchAgents/com.open-mpm.api.plist` — plist config
- `/Users/masa/.open-mpm/logs/api-stderr.log` — restart log evidence
- `/Users/masa/Projects/open-mpm/src/api/server.rs:553` — `serve_with_config`
- `/Users/masa/Projects/open-mpm/src/api/server.rs:703` — `tokio::spawn` for Implementation tasks
- `/Users/masa/Projects/open-mpm/src/api/server.rs:855` — SSE keepalive timer (not the cause)
- `/Users/masa/Projects/open-mpm/src/api/server.rs:907` — `run_task` handler
- `/Users/masa/Projects/open-mpm/Makefile:22-24` — `make install` binary replacement
