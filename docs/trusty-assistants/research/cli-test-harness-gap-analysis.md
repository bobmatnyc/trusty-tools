# CLI Test Harness Gap Analysis

**Date**: 2026-04-24  
**Scope**: Design a CLI-based test harness with Project object, interaction memory, PM-to-PM messaging.

---

## 1. Existing Test Infrastructure

### What exists

- `tests/integration/run_bakeoff.sh` — invokes `./open-mpm --workflow prescriptive --task-file task.txt`, checks that `out/*.py` files appear. No assertions on content; pass/fail is file-existence only.
- `tests/harness/run_inspection.sh` — spawns `open-mpm inspect --task "..." [--dry-run]`, parses JSON stdout with `python3 -c`, compares `registry.best_match` against an expected agent name. Good pattern: JSON output + external parser.
- `src/inspection/mod.rs` — `inspect_report()` is a pure fn (no I/O), testable directly. `run_inspect_dry_run()` splits I/O from logic. Good precedent.
- `cargo test` — many unit tests in `src/`, all using `tempfile::tempdir()` for isolation. Pattern: testable inner routines (`append_to`, `search_in`) that take explicit paths instead of `$HOME`.
- **No `Project` struct anywhere** in `src/`. The closest thing is `PmHandle` in `ctrl/mod.rs` which wraps a `PathBuf project_path` and a `tokio::mpsc` channel to a PM actor. Not exported for test use.
- **No CLI invocation wrapper** in the test suite. Shell scripts build the binary, then shell-exec it. No Rust-level `Command` harness exists.

### Gap

There is no `Project` struct usable from test code. No way to programmatically spawn a PM, send it tasks, and assert on the response from within `cargo test`. The inspection dry-run pattern (pure fn + JSON) is the best existing template.

---

## 2. Existing Messaging / IPC

### What exists

**PM ↔ Sub-agent (intra-process delegation)**  
`src/ipc/mod.rs` — `IpcMessage` enum: `Task { id, task, history?, session_reset? }` / `Result { id, content, summary?, usage? }` / `Error { id, error }`. Serialized as NDJSON. Robust, well-tested, extensible.

**CTRL ↔ PM (in-process, same binary)**  
`src/ctrl/mod.rs` — `PmHandle` wraps `mpsc::Sender<PmMsg>` (enum: `Task { text, reply: oneshot::Sender<Result<String>> }` / `Shutdown`). This is NOT IPC; it is tokio actor messaging within a single process. No wire format.

**PM ↔ PM (cross-project, network)**  
`src/bus/mod.rs` — `MessageBus`: Unix domain sockets at `~/.open-mpm/sockets/<project_id>.sock`. Publishes `BusEnvelope { source_project, target_project?, message: Value }` as NDJSON over the socket. `send_to(target, payload)` connects, writes one line, closes. `subscribe()` returns a `broadcast::Receiver`. `list_running()` probes live sockets. CTRL's relay task prints incoming envelopes to stderr but does NOT forward them to a PM actor (the `dispatch_task` call is missing from the relay — it only logs).

**HTTP API**  
`src/api/server.rs` — `POST /api/task` submits a workflow to a subprocess. No PM-to-PM routing; it is single-project.

### Gap

The bus transport exists and is functional. What is missing:
1. The CTRL relay does not forward inbound bus messages to the active PM actor.
2. There is no message log — envelopes are fire-and-forget; they are not persisted anywhere.
3. There is no way for test code to subscribe to a bus and assert on delivered messages.

---

## 3. Existing Memory / Session Storage

### What exists

**`src/session_record.rs`**  
`SessionRecord` — one row per completed workflow run stored in `~/.open-mpm/sessions/runs.jsonl`. Fields: `timestamp, project_path, task, task_level, workflow, status, score, cost_usd, duration_mins, files_modified, build_id`. Append-only JSONL. `search()` does case-insensitive substring grep on task/project_path/status. No structured interaction history (no per-turn PM↔user exchanges).

**`src/memory/session_store.rs`**  
Exists as a file but not read in this analysis (see note below — it is separate from `session_record`).

**CTRL in-memory fallback**  
`ctrl/mod.rs` — `Arc<Mutex<Vec<String>>>` for `memory_store` / `memory_recall`. Volatile; lost on restart. Only stores freeform strings. Not keyed by project.

**`src/skills/global_cache.rs`**  
SHA-256 content-addressed cache with JSON index. Pattern reusable: atomic write (tmp+rename), hash-keyed content store, metadata index. Applicable for interaction log storage.

**kuzu-memory**  
Wired as MCP tool (`mcp__kuzu-memory__*`) in CTRL's tool ecosystem. No direct Rust integration in the codebase; CTRL falls back to the in-memory vec when kuzu is unreachable.

### Gap

No structure for per-project, per-session interaction memory (PM↔user turn log). `SessionRecord` captures run-level metadata only, not the exchange. There is no `InteractionLog` or equivalent.

---

## Design Brief: Recommended Approach

### Q1 — Minimal `Project` struct for clean CLI invocation

**Gap**: None exists.  
**Recommendation**: Model `Project` after `PmHandle` but make it test-constructible.

```
Project {
    root: PathBuf,                   // canonical project dir
    binary: PathBuf,                 // path to open-mpm binary (override in tests)
    config_dir: PathBuf,             // defaults to root/.open-mpm
}

impl Project {
    fn run_task(&self, task: &str) -> Result<CliOutput>
    fn run_inspect(&self, task: &str) -> Result<InspectReport>
    fn run_workflow(&self, workflow: &str, task: &str) -> Result<PmResponse>
}

struct CliOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
}
```

`Project::run_task` wraps `tokio::process::Command` in the same pattern the API server's `run_task` already uses (`Command::new(binary).arg("--task").arg(task).current_dir(root)`). Tests use `tempfile::tempdir()` for `root` and `env!("CARGO_MANIFEST_DIR")` + `target/debug/open-mpm` for `binary`. No changes to production code needed — this is a pure test-support type, best placed in `tests/support/project.rs`.

---

### Q2 — Where to store PM-to-PM messages so CTRL can query them

**Gap**: Messages vanish after delivery; no log.  
**Recommendation**: Append PM-to-PM envelopes to `~/.open-mpm/sessions/pm-messages.jsonl` using the same pattern as `session_record::append_to`. Define:

```
PmMessageRecord {
    timestamp: String,     // ISO-8601
    source_project: String,
    target_project: Option<String>,
    message: Value,        // raw BusEnvelope.message
    direction: "outbound" | "inbound",
}
```

Append in two places: `MessageBus::send_to` (outbound) and `accept_loop` / `handle_connection` (inbound). CTRL's `/sessions` search can be extended to query `pm-messages.jsonl` with the same `search_in` pattern. The file stays small (one line per cross-PM signal) and requires no schema migration.

---

### Q3 — Best transport for PM↔PM messaging

**Gap**: Bus exists but relay is incomplete; no test-accessible subscription.  
**Recommendation**: Keep the Unix domain socket bus as the primary transport. It already works. Three targeted fixes:

1. Complete the CTRL relay — after printing the incoming envelope, check if `target_project` matches a connected PM key and call `ctrl.dispatch_task(text)` on it.
2. Add a `subscribe_and_drain(timeout)` helper to `MessageBus` for test assertions: connect a raw `UnixStream`, send a probe, await the broadcast receiver with a short deadline.
3. Do NOT add HTTP or named pipes. The bus already handles peer discovery via `list_running()`. Shared JSONL is sufficient for the persistence layer (Q2). Adding a third transport increases surface area for no gain.

For test harness specifically: tests that need PM↔PM assertions should start two `Project` instances, call `MessageBus::start` on each, send via `send_to`, then `subscribe()` and assert with `tokio::time::timeout`.

---

### Q4 — Test assertions against CLI output

**Gap**: Shell scripts use `python3 -c` to parse JSON; no Rust-level assertion helpers.  
**Recommendation**: Follow the existing `inspect_report()` precedent — all subcommands that tests need to assert on should emit structured JSON to stdout. Extend `PmResponse` (already JSON-serializable) as the universal test output envelope. Add to `Project`:

```
impl Project {
    fn assert_task_contains(&self, task: &str, expected: &str) -> Result<()>
    fn assert_agent_selected(&self, task: &str, agent: &str) -> Result<()>
    fn assert_file_created(&self, relative_path: &str) -> Result<()>
}
```

These methods call `run_task` / `run_inspect`, parse stdout as JSON, and assert with `anyhow::ensure`. No shell required. Existing shell scripts stay for smoke tests; Rust-level assertions live in `#[tokio::test]` functions in `tests/integration/`.

---

## Summary Table

| System | Current State | Biggest Gap | Recommended Fix |
|---|---|---|---|
| Test infra / Project | Shell scripts only; no Rust wrapper | No `Project` struct for programmatic spawn+assert | Add `tests/support/project.rs` wrapping `tokio::process::Command` |
| PM↔PM messaging | Bus transport works; relay incomplete; no persistence | No message log; relay doesn't forward to PM actor | Append to `pm-messages.jsonl`; complete relay dispatch |
| Memory / interaction | `SessionRecord` = run-level only; CTRL memory = volatile Vec | No per-session turn log | Add `InteractionLog` appending to per-project JSONL, reusing `session_record` patterns |
