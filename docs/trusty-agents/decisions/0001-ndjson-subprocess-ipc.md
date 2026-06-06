# 0001. NDJSON-over-stdin/stdout subprocess IPC for sub-agents

- **Status:** Accepted
- **Date:** 2026-05-29
- **Scope:** Crate `open-mpm`
- **Supersedes / Superseded by:** —

## Context

open-mpm's controller (CTRL) and per-project PM actors delegate work to
sub-agents. File- and shell-touching agents must run with **OS-level isolation**
so that a panicking or runaway agent cannot corrupt the controller's address
space; read-only agents can run in-process for speed. The isolated agents need a
simple, language-agnostic, debuggable wire protocol between the PM and each
sub-agent process.

The architecture spec (`docs/open-mpm/spec/ARCHITECTURE.md` §2, "IPC Model")
documents the chosen design: each subprocess runs with **stdin piped, stdout
piped (one JSON object per line), stderr inherited**, with the codec in
`src/ipc/mod.rs` (`IpcMessage`). The PM uses **separate tokio read and write
tasks** to avoid the classic pipe deadlock. Messages are correlated by a `uuid`
`id`; the sub-agent emits exactly one `result`/`error` line and then exits.
This was validated against the subprocess-IPC research
(`docs/open-mpm/research/subprocess-ipc-patterns.md`), which favors NDJSON over
stdin/stdout with separate read/write tasks and `kill_on_drop`.

## Decision

We will use **newline-delimited JSON (NDJSON) over stdin/stdout** as the IPC
protocol between a PM and its subprocess sub-agents. Each message is a single
JSON object on one line. The wire types are:

- **PM → sub-agent:** `{ "type": "task", "id": "<uuid>", "task": "…", "history": [...] }`
- **sub-agent → PM (success):** `{ "type": "result", "id": "<uuid>", "content": "…", "summary": "…", "usage": {...} }`
- **sub-agent → PM (failure):** `{ "type": "error", "id": "<uuid>", "error": "…", "status": "error" }`

**stderr is reserved for logs** (inherited by the PM's logger), keeping stdout a
clean protocol channel. The PM reads and writes on separate tokio tasks. A
sub-agent emits exactly one result/error line, then exits — giving the
subprocess runner its crash-isolation guarantee.

## Consequences

- **Positive:** crash isolation — a panicking file/shell agent dies alone; the
  controller reads the closed pipe and surfaces an error.
- **Positive:** the protocol is human-readable and language-agnostic, so an
  agent could in principle be implemented outside Rust.
- **Positive:** `stderr`-for-logs mirrors the workspace-wide rule that daemons
  never log to stdout (which carries framed protocol data).
- **Known gap:** the `summary` field exists in the wire format, but the PM today
  consumes full `content` rather than a compressed summary — there is no
  `attempt_completion`-style summarization step yet (spec FR-2.4, marked
  designed-not-built).
- **Neutral:** read-only agents bypass IPC entirely via the in-process runner;
  this ADR governs only the subprocess path.
