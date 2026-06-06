# 0003. Daemon process model — one daemon per agent identity, one PM per project, one TPM per session, bounded coding-agent fan-out

- **Status:** Accepted
- **Date:** 2026-05-29
- **Scope:** Crate `open-mpm`
- **Supersedes / Superseded by:** —

## Context

open-mpm's process model was historically ambiguous. The PRD carried an open
question — **"Singleton vs. multi-controller"** — asking whether a second CLI
invocation should always route to the running controller, or whether explicit
multi-controller setups should be supported. The shipped behavior merely
*raced*: two near-simultaneous invocations could both try to bind the project's
`.ctrl.sock` rather than the second reliably auto-routing to the first
(spec PRD FR-1.3; `docs/open-mpm/spec/ARCHITECTURE.md` §1, "Process Model").

The probe-then-bind, anti-clobber socket handling merged in **PR #411** closed
the race *for a single (identity, project) pair* — it is the singleton
**enforcement mechanism** at the socket layer. But PR #411 did not define the
*intended* topology: it left open how many distinct controllers may legitimately
coexist, what the singleton actually scopes over, and how many coding-agent
subprocesses one PM may fan out to. This ADR records the now-decided model so the
enforcement work (#411) and the remaining gaps have a single authoritative
reference.

The decision interacts with three existing pieces of machinery: the UNIX-socket
singleton enforcement (PR #411), the `~/.open-mpm/processes.json` PID lifecycle
tracker (spec FR-9.3), and the subprocess-IPC contract (ADR-0001), over which a
PM talks to each spawned coding agent.

## Decision

open-mpm's process model is the following hierarchy of four process roles:

1. **open-mpm runs as a daemon.** The controller is a long-running daemon
   process, not a foreground-only invocation.

2. **One process per user-facing agent identity.** Distinct user-facing
   agents/assistants — e.g. **CTRL** (the multi-project dispatcher), **Izzie**,
   and **CTO Assistant** — each run as their **own** daemon process. This is
   **not** a single global singleton: multiple controller/daemon processes
   legitimately coexist, one per agent identity.

3. **One PM process per project.** Each project's PM is a singleton process. The
   singleton guarantee is therefore scoped **per `(agent-identity, project)`**,
   not globally — `(CTRL, ~/proj-a)` and `(Izzie, ~/proj-a)` are distinct
   singletons, and so are `(CTRL, ~/proj-a)` and `(CTRL, ~/proj-b)`. The PM
   orchestrates open-mpm's **own native sub-agents** — in-process/subprocess
   runners that speak NDJSON over stdin/stdout (ADR-0001).

4. **One TPM ("tmux PM") process per session.** A TPM is a PM variant that
   orchestrates **external** coding harnesses — third-party CLIs such as
   `claude-code`, `codex`, `aider`, etc. — by driving them inside **tmux**
   panes/sessions rather than as native NDJSON subprocesses. Its cardinality is
   **one TPM per session**.

5. **PM spawns coding-agent subprocesses, capped at ~20.** A single PM may have
   at most a bounded number of coding/sub-agent subprocesses spawned
   concurrently. The documented default cap is **20** (configurable). Requests
   beyond the cap queue / apply backpressure rather than spawning unbounded
   processes.

**PM vs. TPM.** The PM and the TPM are *sibling* roles, not the same role. The
**PM** orchestrates open-mpm's native agents via in-process/subprocess runners
over **NDJSON IPC** (ADR-0001). The **TPM** orchestrates *third-party* harnesses
it does not own by **automating tmux** — creating sessions/panes, sending keys,
capturing pane output, and detecting which harness is running. They differ in
both *what* they drive (native agents vs. external tools) and *how* (NDJSON
subprocesses vs. tmux panes), and in cardinality (PM is per-project; TPM is
per-session).

The singleton **scope** is the key clarification: the per-(agent-identity,
project) singleton is what PR #411's socket enforcement guarantees. The daemon
lifecycle, the per-user-facing-agent process separation, the per-session TPM
role, and the 20-process coding-agent cap are the *intended* model that this ADR
commits to — they are **not yet fully implemented** (the tmux-driving machinery
behind the TPM, however, largely exists; see Consequences).

## Consequences

### Positive

- **Clear lifecycle.** The controller is unambiguously a daemon; start/stop and
  attach/detach semantics have a single defined shape.
- **No global lock.** The singleton scope is per-(identity, project), so the
  product never needs a single machine-wide controller lock. Multiple
  assistants coexist by design.
- **Multiple assistants coexist.** CTRL, Izzie, and CTO Assistant can all run on
  the same machine simultaneously, each as its own daemon, without contending
  for one global controller.
- **Bounded fan-out.** Capping a PM at ~20 concurrent coding-agent subprocesses
  bounds memory, file-descriptor, and CPU pressure, and gives a predictable
  ceiling for the `processes.json` tracker.
- **External harnesses are first-class.** The per-session TPM lets open-mpm
  drive third-party coding tools (claude-code, codex, aider, …) inside tmux
  without forcing them through the native NDJSON subprocess contract, while
  keeping a clean separation from the native PM role.

### Work this implies (current state)

- **Per-(identity, project) singleton enforcement exists** at the socket layer
  via PR #411 (probe-then-bind, anti-clobber socket handling). ✅
- **Daemonization is not yet built.** 🔵 The controller still runs as a
  long-running foreground process hosting the TUI; a true daemon lifecycle
  (detach, supervise, attach) is designed-not-built.
- **Per-identity process separation is not yet built.** 🔵 There is no
  per-identity process registry keyed on agent identity; today's enforcement
  keys on `(project)` socket path, not on `(agent-identity, project)`.
- **The 20-cap is not yet built.** 🔵 No concurrency cap or queue/backpressure
  bounds how many coding-agent subprocesses a PM spawns; enforcing the cap
  requires a semaphore/queue in the dispatch path plus surfacing backpressure to
  the caller.
- **The TPM's tmux-driving machinery largely exists; the per-session TPM
  *role* does not.** 🟡 / 🔵 The `src/tm/` module already implements the
  substrate a TPM needs: `TmManager` (`src/tm/manager.rs`) ties together a
  `TmuxOrchestrator` (live tmux state), an `AdapterRegistry`
  (`src/adapters/`, with concrete pane-output detectors for `claude-code`,
  `codex`, `augment`, `gemini`, plus `shell`/`claude-mpm`/`open-mpm`), and a
  JSON-backed `TmSessionRegistry` (`src/tm/registry.rs`). Real session lifecycle
  is built — `new_session` (`tmux new-session`), `kill_session`,
  `pause_session`/`resume_session` (sending each adapter's pause/resume command
  via tmux), `capture_pane`, `send_message`, `attach_instructions`, and
  `reconcile` — over the `TmProject`/`TmSession` data model (`src/tm/project.rs`)
  with a background idle `TmMonitor` (`src/tm/monitor.rs`) and a `/tm` command
  handler. So the **tmux-driving capability is partially-to-mostly built (🟡)**
  (its tmux integration tests are gated behind tmux availability). What is
  **designed-not-built (🔵)** is its *formalization* as a daemon-managed
  **"1 TPM per session"** process role owned by the identity daemon and tracked
  in `~/.open-mpm/processes.json` alongside the native PM — today `src/tm/` is a
  library/CLI-driven facade, not a supervised per-session process.
- **Interaction with existing machinery:** the per-identity registry should
  extend the `~/.open-mpm/processes.json` tracker (FR-9.3) so each daemon and PM
  is recorded by `(agent-identity, project)`; the socket enforcement (PR #411)
  becomes the per-PM bind guarantee; and each capped coding-agent subprocess
  continues to speak NDJSON over stdin/stdout per **ADR-0001**.

## Cross-references

- **PRD FR-1.3** (singleton / process model), **FR-1.6** (bounded coding-agent
  fan-out), and **FR-1.7** (tmux PM for external harnesses) —
  `docs/open-mpm/spec/PRD.md`.
- **ARCHITECTURE §1, "Process Model"** — `docs/open-mpm/spec/ARCHITECTURE.md`
  (daemon topology / three-tier hierarchy).
- **ADR-0001** — NDJSON-over-stdin/stdout subprocess IPC (the wire protocol each
  capped coding-agent subprocess uses).
- **PR #411** — probe-then-bind, anti-clobber socket handling (the per-(identity,
  project) singleton enforcement mechanism).
