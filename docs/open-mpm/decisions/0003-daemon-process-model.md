# 0003. Daemon process model — one daemon per agent identity, one PM per project, bounded coding-agent fan-out

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

open-mpm's process model is the following three-tier hierarchy:

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
   singletons, and so are `(CTRL, ~/proj-a)` and `(CTRL, ~/proj-b)`.

4. **PM spawns coding-agent subprocesses, capped at ~20.** A single PM may have
   at most a bounded number of coding/sub-agent subprocesses spawned
   concurrently. The documented default cap is **20** (configurable). Requests
   beyond the cap queue / apply backpressure rather than spawning unbounded
   processes.

The singleton **scope** is the key clarification: the per-(agent-identity,
project) singleton is what PR #411's socket enforcement guarantees. The daemon
lifecycle, the per-user-facing-agent process separation, and the 20-process
coding-agent cap are the *intended* model that this ADR commits to — they are
**not yet implemented**.

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
- **Interaction with existing machinery:** the per-identity registry should
  extend the `~/.open-mpm/processes.json` tracker (FR-9.3) so each daemon and PM
  is recorded by `(agent-identity, project)`; the socket enforcement (PR #411)
  becomes the per-PM bind guarantee; and each capped coding-agent subprocess
  continues to speak NDJSON over stdin/stdout per **ADR-0001**.

## Cross-references

- **PRD FR-1.3** (singleton / process model) and the new **FR-1.6** (bounded
  coding-agent fan-out) — `docs/open-mpm/spec/PRD.md`.
- **ARCHITECTURE §1, "Process Model"** — `docs/open-mpm/spec/ARCHITECTURE.md`
  (daemon topology / three-tier hierarchy).
- **ADR-0001** — NDJSON-over-stdin/stdout subprocess IPC (the wire protocol each
  capped coding-agent subprocess uses).
- **PR #411** — probe-then-bind, anti-clobber socket handling (the per-(identity,
  project) singleton enforcement mechanism).
