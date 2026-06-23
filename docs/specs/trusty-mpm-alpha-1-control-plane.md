# DOC-26 — trusty-mpm alpha-1 unified project/session control plane

**Status:** Draft
**Subsystem:** trusty-mpm — control plane / session manager (`crates/trusty-mpm/`)
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-23
**Spec ID:** `SPEC-SESSCTL-01~draft` (DOC-26)
**Builds on:**
- DOC-14 — Session Manager Agent (`docs/specs/session-manager-agent.md`)
- DOC-16 — Sessions TUI Interactive (`docs/specs/sessions-tui-interactive.md`)
- DOC-19 — TELUI Telegram UI (`docs/specs/telui-telegram-ui.md`)
- DOC-20 — Chat-Core (`docs/specs/chat-core.md`)
- Multi-repo session routing (#1517, `docs/specs/multi-repo-session-routing.md`)
- Standalone managed trusty-mpm (#1548, `docs/specs/standalone-managed-trusty-mpm.md`)

**Cross-ref:** issues **#1590** (alpha-1 control plane epic), **#1548** (standalone managed
trusty-mpm), **#1517** (multi-repo session routing), **#1272** (sessions TUI epic), **#1433**
(TELUI), **#1452** (tmux lifecycle reaping foundation).

> **Scope note.** This is a **behavior-contract** spec for the **trusty-mpm alpha-1 unified
> project/session control plane** — the runtime, command surface, session model, and internal
> architecture that unifies all session types under one daemon. It specifies the two execution
> backends (stream-JSON and tmux), the `tm` command surface, the session identity model, the
> multi-observer attach contract, the per-session actor system, the activity observability
> pipeline, the auth/cost model, and the tmux lifecycle discipline. It does **not** specify the
> SM agent brain (DOC-14), the TUI rendering (DOC-16), or the Telegram adapter (DOC-19). The
> trusty-code (tcode) direct-API adapter seam is acknowledged but deferred.

---

## 1. Motivation

trusty-mpm today ships two parallel session paths that have diverged in semantics, naming, and
lifecycle management:

1. **Project sessions** — tmux-backed `claude` processes registered via `POST /sessions`,
   observed through pane capture, driven by `tm` CLI verbs (`run`, `stop`, `send`, etc.).
2. **Managed sessions** — a higher-level abstraction layered on top (issues #1548, #1272),
   with its own lifecycle, summary, and decision infrastructure.

Neither path has a shared runtime adapter: the tmux path is implemented inline throughout the
daemon; a future headless/stream-JSON path for managed sessions has no defined seam to slot
into. The result is duplicated lifecycle code, inconsistent session IDs across the two families,
and no clean story for adding a third backend (e.g. the tcode direct-API adapter).

Epic #1590 is the unification: one **SessionActor + mpsc registry** in the daemon, two
**execution backends behind one runtime-adapter seam**, and a locked `<project-id>-<N>` session
ID convention that is unambiguous across every surface.

### Why NOT the Agent SDK

The Anthropic Agent SDK supports only metered `ANTHROPIC_API_KEY` authentication. trusty-mpm
operators run Claude Code under **Max subscription (OAuth/logged-in) credentials**. Migrating
to the Agent SDK would:

- Force operators onto per-token metered billing instead of flat-rate Max.
- Break the `~/.claude` credential store path that Claude Code uses.
- Lose the `--append-system-prompt-file` PM instruction delivery mechanism that the existing
  session launch path (`bin/tm/commands/launch.rs`) depends on.

The decision is **locked**: trusty-mpm stays a launcher/supervisor, not an Agent SDK consumer.
The **stream-JSON** backend (§3.1) is the path to a warm, long-lived headless `claude` process
without surrendering Max billing.

---

## 2. Scope and non-goals

### 2.1 Alpha-1 scope

- Two execution backends (§3): stream-JSON (default) and tmux (`--tmux` flag).
- Runtime-adapter seam (§3.3): the interface separating the daemon from any backend.
- `tm` command surface (§4): `run`, `connect`, `stop`, `auth`, `list`.
- Session model and IDs (§5): `<project-id>-<sessionNo>` convention.
- Multi-observer / single-writer attach contract (§6).
- Per-session actor system (§7): `SessionActor` + `mpsc` registry.
- Activity observability pipeline (§8): raw capture → regex/NLP → optional OpenRouter.
- Auth and cost model (§9): Max OAuth via `env -u ANTHROPIC_API_KEY`.
- Lifecycle and reaping (§10): continuity with #1452 RAII discipline.
- Implementation phasing (§11): Phases 1–5 mapped to epic #1590.

### 2.2 Non-goals (alpha-1)

- **SM agent brain** — governed by DOC-14; not changed here.
- **TUI / STUI rendering** — governed by DOC-16; this spec touches only the data the TUI
  consumes, not how it is rendered.
- **Telegram / GUI adapters** — governed by DOC-19 and the GUI crate; out of scope.
- **trusty-code (tcode) direct-API adapter** — the seam (§3.3) is specified and the slot is
  reserved, but the implementation is deferred to a follow-on epic.
- **IPC between a parent `claude-mpm` process and the daemon** — the stream-JSON backend is
  the foundation; the IPC layer over it is out of scope for alpha-1.
- **OAuth token refresh** — Max login is assumed to be valid; automated re-login on expiry is
  future work.

---

## 3. Execution backends

### 3.1 Stream-JSON backend (DEFAULT)

The stream-JSON backend runs a **warm, long-lived headless `claude -p` process in
stream-json mode per session**. This is the alpha-1 default for all `tm run` invocations
without `--tmux`.

**Process model:**

```
daemon SessionActor
  └─ spawn: env -u ANTHROPIC_API_KEY claude -p --output-format stream-json
               [--append-system-prompt-file <tmp_pm_prompt>]
               --workdir <project_dir>
  └─ stdin:  newline-delimited JSON messages sent to the session
  └─ stdout: newline-delimited JSON stream-json events read by the actor
  └─ stderr: captured for diagnostics; never forwarded to observers
```

**Key properties:**

- **Warm and long-lived.** The process persists across multiple user messages within a
  session. The daemon actor writes to its stdin and reads its stdout without tearing down
  and respawning per message.
- **Daemon-supervised.** The `SessionActor` monitors the child process. On unexpected exit
  (crash, OOM, etc.) it applies an exponential back-off auto-restart policy, up to a
  configurable `max_restarts` (default: 3) before the session is marked `Failed`.
- **Max OAuth billing.** The process is spawned with `ANTHROPIC_API_KEY` **unset** (§9).
  Claude Code resolves credentials from `~/.claude`, preserving Max subscription flat-rate
  billing.
- **JSON framing.** Messages sent to stdin and events read from stdout follow the Claude
  Code `--output-format stream-json` wire format. The daemon actor never interprets the
  JSON content for LLM reasoning — it exposes raw structured events to observers (§8).

**Reconnect semantics.** If the child process restarts (auto-restart policy), in-flight
message delivery is not guaranteed across the restart boundary. The actor records a
`RestartEvent` in the session event log so observers can detect the gap. This is an
open question captured in §13.1.

### 3.2 Interactive tmux backend (`--tmux` flag)

The tmux backend is activated by passing `--tmux` to `tm run`. It is the pre-existing
foundation (`bin/tm/commands/launch.rs`, `session.rs`) and the only backend used by the
TUI when a TTY is attached.

**Key properties:**

- **tmux session per session.** A dedicated tmux session is created with the naming
  convention `tm:<project-id>:<sessionNo>` (§5.2), so operators can `tmux attach -t
  tm:foo:0` directly.
- **send-keys input injection.** The daemon drives the Claude Code TUI by piping
  send-keys via `tmux send-keys` — NOT via subprocess stdin/stdout. This preserves the
  interactive readline UI that Claude Code provides when it detects a tty.
- **Interactive TUI when a tty is attached.** When a human operator calls `tmux attach`,
  they see the full Claude Code interactive TUI. The daemon and the human share the same
  pane.
- **Output via pane capture.** The daemon reads session output by calling `tmux
  capture-pane -p` periodically (the existing pane-capture path in
  `session_service.rs`). This is the only output source for the tmux backend; there is
  no separate stdout pipe.

**When to use.** The tmux backend is preferred when:
- A human operator wants to attach and interact directly.
- The project requires the full Claude Code interactive TUI behavior.
- Debugging a session interactively without tooling overhead.

### 3.3 Runtime-adapter seam

Both backends implement the same **`SessionBackend` trait** (the runtime-adapter seam).
This seam is the boundary between the `SessionActor` and any concrete execution strategy.

```
trait SessionBackend: Send + 'static {
    /// Send a message/command to the session.
    async fn send(&mut self, msg: SessionInput) -> Result<()>;

    /// Receive the next batch of output events from the session.
    /// Returns None on clean session termination.
    async fn recv(&mut self) -> Option<Result<Vec<SessionEvent>>>;

    /// Gracefully stop the session, draining in-flight work.
    async fn stop(self: Box<Self>) -> Result<()>;
}
```

Concrete implementations in alpha-1:
- `StreamJsonBackend` — wraps the `claude -p --output-format stream-json` child process.
- `TmuxBackend` — wraps `tmux send-keys` + `tmux capture-pane`.

Reserved (deferred, not built):
- `TcodeBackend` — for the trusty-code direct-API adapter. The seam is deliberately
  preserved so this slots in without changing `SessionActor`.

The `AdapterRegistry` prior art (`crates/trusty-agents/src/tm/manager.rs`) informs the
naming and the factory pattern; the alpha-1 seam does not directly reuse that type but
mirrors its design intent.

---

## 4. Command surface

All verbs are available both as `tm` CLI subcommands and as daemon HTTP endpoints
(invoked identically by the TUI, TELUI, and the chat-core nucleus — DOC-20). The
table below specifies the **behavior contract per verb**.

### 4.1 `tm run <project-id>` / `tm run --tmux <project-id>`

**Inputs:** project-id (string), optional `--tmux` flag, optional `--model <model-id>`,
optional `--prompt-file <path>`.

**Preconditions:** project-id is registered in the daemon project registry. If not
registered, the daemon returns `ProjectNotFound`.

**Behavior:**

1. Allocate the next session number N for the project (monotonic, never reused within a
   daemon lifetime).
2. Construct session ID `<project-id>-<N>` (§5.1).
3. Spawn a `SessionActor` in the mpsc registry (§7) with either `StreamJsonBackend`
   (default) or `TmuxBackend` (if `--tmux`).
4. The backend spawns the child process / tmux session.
5. The actor emits a `SessionStarted` event.
6. If a PM prompt file is provided (`--prompt-file`) or derivable from the project
   config, it is delivered via `--append-system-prompt-file` (stream-JSON) or written
   to the tmux session's bootstrap path (tmux).
7. Return the session ID to the caller.

**Postconditions:** session exists in `Running` state; output events are available to
observers (§6).

**Error conditions:** `ProjectNotFound`, `BackendSpawnError` (child process or tmux
session creation failed), `MaxSessionsExceeded` (configurable cap, default 20).

### 4.2 `tm connect <session-id>`

**Inputs:** session-id (string).

**Preconditions:** session exists and is in `Running` or `Paused` state.

**Behavior:**

1. Resolve session-id to the `SessionActor` in the registry.
2. Open an observer channel (§6.1) — the caller receives a stream of `SessionEvent`s.
3. For a write-enabled connection, acquire the write lock (§6.2); if already held, the
   connection is read-only until the lock is released.
4. Stream events to the caller until the session terminates or the caller disconnects.

**Postconditions:** caller is an active observer; write-lock state is updated.

**Error conditions:** `SessionNotFound`, `SessionTerminated`.

### 4.3 `tm stop <session-id>`

**Inputs:** session-id (string), optional `--force` flag.

**Preconditions:** session exists.

**Behavior:**

1. If `--force` is absent: send a graceful stop signal to the backend (§3.3 `stop()`),
   which drains in-flight work. The actor waits up to a `stop_timeout` (default: 30s)
   before escalating to `--force` behavior.
2. If `--force` or timeout elapsed: send SIGKILL to the child process (stream-JSON) or
   `tmux kill-session` (tmux).
3. Emit `SessionStopped` event.
4. Release all observer channels; write-lock is released.
5. Run reaping cleanup (§10.2).

**Postconditions:** session is in `Stopped` state; actor is removed from registry.

**Error conditions:** `SessionNotFound`, `StopTimeout` (timeout elapsed, force applied
automatically).

### 4.4 `tm auth <session-id>`

**Inputs:** session-id (string).

**Preconditions:** session exists and is in `AwaitingAuth` state (the `claude` process is
presenting the Max OAuth login prompt).

**Behavior:**

1. For the **tmux backend**: the auth prompt is visible in the tmux pane; no daemon
   action is needed — the operator attaches via `tmux attach` and completes login
   interactively.
2. For the **stream-JSON backend**: the daemon detects the OAuth prompt in the stdout
   event stream (§8.1 regex detection). It emits a `PendingDecision { kind: Auth }` event
   to all observers. The TUI / operator must resolve this by attaching interactively (a
   temporary tmux window can be opened for auth-only purposes).
3. Once auth is detected as complete (stdout resumes normal output), the actor
   transitions the session from `AwaitingAuth` to `Running`.

**Postconditions:** session is in `Running` state.

**Notes:** In normal operation, `ANTHROPIC_API_KEY` is unset (§9) and `claude` uses the
`~/.claude` credential store, which is populated at first login. Re-authentication is
only needed if Max session tokens expire. This is an edge case; the typical flow is that
auth happens once at workstation setup.

### 4.5 `tm list`

**Inputs:** optional `--project <project-id>` filter, optional `--format <table|json>`.

**Behavior:**

1. Query the mpsc registry for all live `SessionActor` entries.
2. For each entry: return session ID, project ID, backend type, state, uptime, last
   activity timestamp, and last summarized message (if available from §8).
3. Also list registered projects with their session counts.

**Postconditions:** caller receives current snapshot; no state is mutated.

**Output schema (per session row):**

```
session_id:       string          // "<project-id>-<N>"
project_id:       string
backend:          "stream-json" | "tmux"
state:            SessionState    // see §7.2
uptime_secs:      u64
last_activity_ts: DateTime<Utc>
last_summary:     Option<string>  // from activity observability §8
pending_decision: Option<string>  // human-readable prompt if awaiting input
proposed_default: Option<string>  // suggested answer if one exists
```

---

## 5. Session model and IDs

### 5.1 Session ID convention

Session IDs follow the pattern:

```
<project-id>-<sessionNo>
```

Where `<sessionNo>` is a monotonically incrementing integer starting at `0`, scoped
per project and per daemon lifetime. Examples: `trusty-tools-0`, `trusty-tools-1`,
`my-proj-0`.

**Properties:**
- IDs are unique within a running daemon instance.
- IDs are **not** reused within a daemon lifetime (session number is never recycled).
- IDs are stable across attach/detach cycles.
- The project-id component is the key into the project registry; it does not encode
  the git remote or branch (those are attributes, not identity).

### 5.2 tmux naming convention

For sessions using the tmux backend, the tmux session is named:

```
tm:<project-id>:<sessionNo>
```

This ensures:
- `tmux attach -t tm:trusty-tools:0` works directly from any terminal.
- tmux names are distinct from user-created tmux sessions (the `tm:` prefix is reserved
  by trusty-mpm).
- The daemon can discover and adopt existing tmux sessions by scanning for the `tm:`
  prefix (via `POST /sessions/discover`).

### 5.3 Project registry

Projects are registered in the daemon project registry (a persistent JSON file at
`~/.trusty-mpm/projects.json`). Each entry carries:

```
project_id:   string         // user-assigned, URL-safe slug
workdir:      PathBuf        // absolute path to the project root
repo_url:     Option<string> // git remote URL (from #1587 LaunchParams)
ref_:         Option<string> // git ref/branch (from #1587 LaunchParams)
created:      DateTime<Utc>
```

The `repo_url` and `ref_` fields thread through `LaunchParams` per PR #1587 so that the
stream-JSON and tmux backends can pass them to `claude` when relevant.

---

## 6. Multi-observer / single-writer attach contract

### 6.1 Output fan-out to many observers

Session output events are broadcast to **all registered observers** via an unbounded
`broadcast::Sender<SessionEvent>` channel (tokio). Each `tm connect` call receives its
own `broadcast::Receiver` clone. There is no limit on the number of observers.

**Guarantee:** every observer receives every event that was enqueued after it connected.
Events before connection are not replayed (no ring buffer in alpha-1; replay is future
work captured in §13.3).

### 6.2 Single-writer attach model

**At most one** connected client holds the **write lock** at any time. The write lock
grants the ability to call `send()` on the session backend. All other connected clients
are read-only observers.

**Lock acquisition:**
- First `tm connect` call (or first call with `--write` intent) acquires the lock.
- Subsequent `tm connect` calls are automatically read-only if the lock is held.
- The caller that holds the write lock is the **active writer**.

**Lock handoff:**
- When the active writer disconnects (channel closed, client exits, `tm stop` on the
  client side), the write lock is released.
- The daemon does NOT automatically grant the lock to a waiting observer. The next
  `tm connect` with write intent from any client acquires it.
- If two clients simultaneously attempt write-intent connect, the first to arrive wins;
  the second becomes read-only. This is a potential race condition noted in §13.2.

**Graceful degradation if the active writer exits independently:**
- If the Claude Code session itself sends an exit signal (detected in the event stream),
  the actor transitions to `Stopped` and broadcasts a `SessionStopped` event.
- All observers receive the final event and their channels are closed.
- The write lock is released as part of actor teardown.

**Invariant:** the daemon never routes input from two clients to the same session
backend simultaneously.

---

## 7. Internal architecture

### 7.1 SessionActor + mpsc registry

The daemon maintains a **per-session actor system**: one `SessionActor` task per live
session, coordinated through an `mpsc`-based registry (NOT thread-per-session). This
scales to many concurrent projects and sessions without the cost of one OS thread per
session.

**Registry:**

```
SessionRegistry {
    actors: HashMap<SessionId, SessionActorHandle>,
    // SessionActorHandle = mpsc::Sender<ActorCommand>
}
```

`SessionRegistry` is owned by the daemon state and accessed via a `tokio::sync::RwLock`.
Reads (list, status) take a shared lock. Mutations (register, deregister) take an
exclusive lock held only for the HashMap operation, not for the actor's entire lifetime.

**Actor handle:**

```
SessionActorHandle {
    command_tx: mpsc::Sender<ActorCommand>,
    event_tx:   broadcast::Sender<SessionEvent>,
    metadata:   Arc<RwLock<SessionMetadata>>,
}
```

Commands sent to an actor (`ActorCommand`) include: `Send(SessionInput)`, `Stop`,
`ForceStop`, `Subscribe(broadcast::Receiver<SessionEvent>)`.

### 7.2 Lifecycle state machine

Each `SessionActor` follows this state machine:

```
  ┌────────────┐
  │  Starting  │  (backend spawning child process / tmux session)
  └─────┬──────┘
        │ BackendReady
        ▼
  ┌────────────┐     Auth prompt detected
  │  Running   │──────────────────────────► AwaitingAuth
  │            │◄────────────────────────── (auth complete)
  └─────┬──────┘
        │
        ├─────── Stop command ─────────────► Stopping
        │                                        │ drain complete / timeout
        │                                        ▼
        │                                    Stopped
        │
        ├─────── Backend crash ────────────► Restarting (if restarts < max)
        │                                        │ restart success
        │                                        ▼
        │                                    Running
        │
        └─────── max restarts exceeded ───► Failed
```

State transitions emit `SessionEvent`s to the broadcast channel.

### 7.3 Event model

```
enum SessionEvent {
    Started       { session_id, backend, ts },
    Output        { session_id, raw: Bytes, structured: Option<StreamJsonEvent>, ts },
    ActivityParsed{ session_id, kind: ActivityKind, summary: String, ts },
    PendingDecision{ session_id, prompt: String, proposed_default: Option<String>, ts },
    Restarted     { session_id, attempt: u8, ts },
    Stopped       { session_id, reason: StopReason, ts },
    Failed        { session_id, reason: String, ts },
}
```

`Output.structured` carries the parsed `stream-json` event when the stream-JSON backend
is active; it is `None` for the tmux backend (pane captures are unstructured text).

### 7.4 Actor task structure

Each `SessionActor` runs as a `tokio::task::spawn` task executing this loop:

```
loop {
    select! {
        command = command_rx.recv() => { /* handle ActorCommand */ }
        events  = backend.recv()    => { /* forward to broadcast_tx */ }
        _       = crash_watchdog    => { /* trigger Restarting or Failed */ }
    }
}
```

The loop exits when the state machine reaches `Stopped` or `Failed`.

---

## 8. Activity observability

### 8.1 Raw-first principle

The daemon's job is to **expose raw observability**: every byte written by the Claude
Code process is captured and forwarded to observers via `SessionEvent::Output`. No
information is discarded at this layer.

**The inference pipeline is layered on top of raw capture, not substituted for it.**

### 8.2 Regex/NLP parse layer (first pass, always runs)

After each `Output` event, the actor runs a **synchronous, zero-allocation regex/NLP
parse** over the raw text to extract structured state without any LLM call. This
produces `SessionEvent::ActivityParsed` when a recognizable pattern is detected.

Recognized patterns (illustrative, not exhaustive):

| Pattern | `ActivityKind` emitted |
|---------|------------------------|
| `> ` prompt prefix (Claude Code interactive prompt) | `AwaitingInput` |
| `Tool use:` or `Using tool:` | `ToolUse { name }` |
| `Error:` or ANSI error-colored output | `Error { excerpt }` |
| `Tests passed` / `cargo test` output lines | `TestResult { passed, failed }` |
| `git commit` / `git push` confirmation | `GitOp { verb }` |
| OAuth / login prompt text | `AuthPrompt` |
| `Session complete` or exit-code 0 on stream-JSON | `SessionComplete` |

The parse layer uses compiled `Regex` instances (lazily initialized) and runs in O(N)
over the raw bytes with no heap allocations beyond the match result.

### 8.3 OpenRouter LLM classifier (optional, unattended/standalone only)

When the daemon is running in **standalone mode** (no parent `claude-mpm` / SM agent
is connected) and an `OPENROUTER_API_KEY` is configured, the daemon MAY invoke an
OpenRouter LLM to classify activity that the regex layer could not confidently resolve.

**This is explicitly OPTIONAL and a fallback, not the default.** Its use is gated by:
1. `[session_manager].observability.llm_classifier = true` in config (default: `false`).
2. `OPENROUTER_API_KEY` present in environment.
3. No SM agent is currently connected (if an SM agent is connected, it provides the
   inference — the daemon does not call OpenRouter).

When the SM agent IS connected (the normal agentic operation path), the daemon exposes
the raw `SessionEvent` stream and the regex-parsed `ActivityParsed` events to the SM.
The SM agent (a `claude-mpm` with Max-OAuth Claude) provides all higher-order
interpretation. **No OpenRouter key is needed in this path.**

### 8.4 `pending_decision` and `proposed_default` exposure

When the activity parse layer (§8.2) or the LLM classifier (§8.3) detects that the
session is awaiting a decision from the operator, the actor:

1. Sets `SessionMetadata.pending_decision = Some(prompt_text)`.
2. Sets `SessionMetadata.proposed_default = Some(answer)` if the parse layer can
   extract a suggested answer (e.g. the `[Y/n]` default from a tool-use confirmation
   prompt).
3. Emits `SessionEvent::PendingDecision`.

These fields are surfaced in `tm list` output (§4.5) and in the TUI (DOC-16) for both
automated and human resolution.

---

## 9. Auth and cost model

### 9.1 Max OAuth credential path (LOCKED)

All `claude` processes spawned by the daemon (both backends) are launched with
`ANTHROPIC_API_KEY` explicitly **unset** from the child environment:

```bash
env -u ANTHROPIC_API_KEY claude -p --output-format stream-json …
```

This forces `claude` to resolve credentials from `~/.claude` (the Max OAuth credential
store), preserving **flat-rate Max subscription billing** for all sessions.

**Consequences:**
- No per-token spend is incurred by trusty-mpm spawned sessions.
- The operator must have completed Max login (`claude auth login`) once on the
  workstation before spawning sessions.
- `ANTHROPIC_API_KEY` set in the operator's shell environment does NOT bleed into
  spawned sessions — this is intentional and avoids accidental metered-key usage.

### 9.2 Rate-limit contention (Max shared limits)

The primary cost concern for concurrent sessions is **Max subscription rate-limit
contention**: multiple simultaneous `claude` processes sharing the same Max account
compete for the same rate bucket.

Mitigation strategies (alpha-1):
- **Session cap.** A per-daemon `max_concurrent_sessions` cap (default: `5`,
  configurable) prevents unconstrained fan-out.
- **Staggered launch.** When spawning multiple sessions from `tm run` in a loop (e.g.
  from an SM agent decomposing a goal into N tasks), the daemon applies a configurable
  `launch_stagger_ms` delay (default: `2000ms`) between successive spawns.
- **Observability.** Rate-limit errors from the stream-JSON backend's stderr are parsed
  (§8.2) and surfaced as `ActivityKind::RateLimited` events, which the SM agent or
  operator can use to pause/retry.

This is an open risk area; see §13.1.

### 9.3 PM prompt delivery

PM instructions are delivered to the spawned `claude` process via
`--append-system-prompt-file <tmp>` (stream-JSON) or by writing a prompt file into the
tmux session's bootstrap sequence (tmux). This reuses the existing mechanism from
`bin/tm/commands/launch.rs:127-156` and is unchanged from the current tmux path.

---

## 10. Lifecycle and reaping

### 10.1 Continuity with #1452

Epic #1452 shipped the RAII tmux lifecycle discipline that is now the **project
standard**:

- Every tmux session is owned by a single `Session` object.
- The `Drop` impl for `Session` cleans up the tmux session.
- Reaping runs on: normal completion, graceful shutdown, error, DELETE API call, and
  periodic orphan-GC.
- Fire-and-forget tmux sessions are **prohibited**.

This spec extends that discipline to the stream-JSON backend: the `StreamJsonBackend`
holds the `Child` handle to the spawned process and its `Drop` impl sends SIGTERM
(then SIGKILL after `kill_grace_ms`) to ensure no orphaned `claude` processes.

### 10.2 Reaping contract

On session termination (any `StopReason`):

1. `SessionActor` sends the stop signal to the backend.
2. Backend's `stop()` drains in-flight messages and terminates the child (stream-JSON:
   SIGTERM → drain → SIGKILL if needed; tmux: `tmux kill-session`).
3. Actor emits `SessionStopped`.
4. Actor removes itself from `SessionRegistry`.
5. All observer channels are closed (receivers observe `RecvError::Closed`).
6. Write lock is released.

### 10.3 Orphan GC

The daemon runs a periodic orphan-GC task (interval: `orphan_gc_interval_secs`,
default: `60`) that:

1. Scans for tmux sessions matching `tm:*` that are NOT in the `SessionRegistry`.
2. Kills any discovered orphans via `tmux kill-session`.
3. Scans for child processes with `comm = claude` whose parent PID is the daemon
   but whose `SessionId` cannot be resolved in the registry.
4. Sends SIGTERM to any discovered orphan child processes.

Orphan-GC findings are logged at WARN level and reported in `GET /health`.

### 10.4 Graceful daemon shutdown

On daemon SIGTERM (the connection-safe restart convention, CLAUDE.md #534):

1. Stop accepting new `tm run` commands.
2. Send graceful `Stop` command to all live `SessionActor`s.
3. Wait up to `shutdown_drain_timeout_secs` (default: `30`) for all actors to reach
   `Stopped`.
4. Force-stop any actors that have not stopped within the timeout.
5. Exit with code 0.

The `--stdio` MCP proxy reconnects automatically with exponential backoff during daemon
restart (existing behavior, unchanged).

---

## 11. Implementation phasing (epic #1590 map)

| Phase | Scope | Gate |
|-------|-------|------|
| **Phase 1 — foundation** | `SessionBackend` trait, `StreamJsonBackend`, `TmuxBackend` refactored to trait, `SessionActor` skeleton, `SessionRegistry` (mpsc + broadcast), session ID convention `<project-id>-<N>`. | `cargo test -p trusty-mpm` passes; `tm run` works on both backends. |
| **Phase 2 — command surface** | `tm run`, `tm connect`, `tm stop`, `tm auth`, `tm list` wired to the registry. Daemon HTTP endpoints for same verbs. | All five verbs round-trip via `tm` CLI and HTTP. |
| **Phase 3 — observability** | Regex/NLP parse layer (§8.2). `pending_decision` / `proposed_default` exposure. `tm list` shows live state. | `tm list --format json` emits structured per-session rows. |
| **Phase 4 — lifecycle hardening** | Auto-restart policy (§3.1), orphan-GC (§10.3), graceful shutdown (§10.4), `TmuxBackend` RAII reaping on trait. | Orphan-GC test passes; restart test simulates crash + recovery. |
| **Phase 5 — auth + cost model** | `env -u ANTHROPIC_API_KEY` enforced (§9.1). Session cap + stagger (§9.2). Rate-limit detection surfaced as `ActivityKind`. Optional OpenRouter classifier gated by config. | Integration test: spawning 3 sessions simultaneously completes without key leakage. |

Each phase produces a mergeable PR. Phases 1–3 are parallel-workable once Phase 1 lands.

---

## 12. Success criteria and acceptance

The following acceptance tests gate promotion from alpha-1 to beta:

| # | Criterion | How to verify |
|---|-----------|---------------|
| AC-1 | `tm run trusty-tools` spawns a stream-JSON session; `tm list` shows it as `Running`. | Manual + integration test. |
| AC-2 | `tm run --tmux trusty-tools` spawns a tmux session named `tm:trusty-tools:0`; `tmux attach -t tm:trusty-tools:0` shows the Claude Code TUI. | Manual. |
| AC-3 | `tm connect <id>` from a second terminal receives output events while a first terminal holds the write lock; second terminal cannot send. | Integration test. |
| AC-4 | `tm stop <id>` drains in-flight work and removes the session within `stop_timeout`; no orphan process remains. | Integration test + `ps` / `tmux ls` check. |
| AC-5 | Spawned sessions are launched with `ANTHROPIC_API_KEY` unset; `claude` uses `~/.claude` credentials. | Environment inspection in session startup log. |
| AC-6 | Regex parse layer (§8.2) correctly identifies `AwaitingInput`, `AuthPrompt`, and `SessionComplete` from representative stream-JSON fixtures. | Unit test with fixture files. |
| AC-7 | Daemon restarts a crashed stream-JSON session up to `max_restarts`; beyond that marks it `Failed`. | Integration test with forced child process kill. |
| AC-8 | Orphan-GC cleans up a manually created `tm:*` tmux session not in the registry. | Integration test. |
| AC-9 | `OPENROUTER_API_KEY` absent + `llm_classifier = false` (default): no OpenRouter call is made during any session lifecycle. | Assert no outbound HTTP to openrouter.ai in test. |

---

## 13. Open questions and risks

### 13.1 Max rate-limit contention under concurrent sessions

**Risk:** High-parallelism use cases (SM agent decomposing a 10-session goal) may hit
Max rate limits for the shared account. The rate bucket size and replenishment rate for
Max subscriptions are not publicly documented and may change.

**Current mitigation:** session cap (default 5) + stagger (default 2s). These values
are empirically chosen; the right values are unknown until alpha-1 is exercised in
production.

**Open question:** Should the daemon implement a rate-limit backpressure signal that the
SM agent can observe to pause session fan-out? Deferred to beta.

### 13.2 Write-lock handoff race condition

**Risk:** If two clients simultaneously attempt write-intent `tm connect`, the first to
register in the registry wins. If the resolution is not atomic (depends on
`RwLock<SessionRegistry>` acquisition order), both clients might briefly believe they
hold the lock.

**Mitigation planned:** The write-lock acquisition must be an atomic CAS operation inside
a single `RwLock::write()` guard. The current design assumes this; the implementation
must verify it.

**Open question:** Should the daemon support a `--wait-for-lock` flag on `tm connect`
that queues the caller until the lock is available? Deferred.

### 13.3 Stream-JSON reconnect semantics across auto-restart

**Risk:** When the stream-JSON backend auto-restarts after a crash, the new child
process starts a fresh `claude` session. Conversation history is NOT preserved (the
`claude -p` subprocess is stateless between invocations unless the caller re-sends
history). Observers see a `Restarted` event but no gap-fill.

**Open question:** Should the actor re-inject the conversation history (captured from
prior `Output` events) into the restarted child's stdin? This would require the actor
to buffer conversation state, which adds complexity and memory cost.

**Current position:** alpha-1 does NOT attempt history replay on restart. Sessions that
crash are considered partially lost. The SM agent layer (DOC-14) is responsible for
re-launching a session with context if needed.

### 13.4 PM prompt file cleanup

The `--append-system-prompt-file` mechanism writes a temporary file. If the daemon
crashes before the child process reads it, the temp file is orphaned. Current behavior
(inherited from the existing launch path) uses `tempfile`'s drop-on-close semantics
where possible, but race conditions between file write and process spawn exist. This is
a pre-existing issue, not introduced by this spec; tracked separately.
