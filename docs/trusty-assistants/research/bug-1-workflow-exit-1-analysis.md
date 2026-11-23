# BUG-1: Prescriptive Workflow Subprocess Exits Code 1 After Successful Phases

**Date**: 2026-04-26
**Status**: Root cause identified
**Severity**: Medium — the workflow result is on disk but the caller (api/server.rs) treats exit 1 as failure

---

## Summary

The prescriptive workflow subprocess exits with code 1 after all phases complete successfully and correct output is on disk. The root cause is **not** in the engine or the phase loop. It is a false-positive error path triggered by the **fire-and-forget postmortem agent**, which is spawned as a subprocess after the workflow completes, fails immediately with `Error: no NDJSON line on stdin`, and the resulting `writer task panicked` error from `subprocess.rs:455` propagates back through `trigger_postmortem` → `tokio::spawn` task → logged as `WARN postmortem agent dispatch failed`. The spawned postmortem subprocess itself exits with code 1, which is **not** the workflow process's exit code.

However, there is a second, independent code path that **can** cause the workflow process itself to exit 1 after all phases succeed.

---

## Exit Path Trace

### 1. `main()` — `src/main.rs:204`

```rust
async fn main() -> Result<()> {
```

`main` returns `Result<()>`. In Rust, `main() -> Result<()>` returning `Err` causes the runtime to print the error to stderr and exit with code 1. There is no explicit `std::process::exit()` call anywhere in the workflow path.

The dispatch to `run_workflow` is at `src/main.rs:505-513`:
```rust
if let Some(name) = cli.workflow.as_deref() {
    return run_workflow(...).await;
}
```

### 2. `run_workflow()` — `src/main.rs:1179`

Returns `Result<()>`. On the success path it returns `Ok(())` at line 1626. The `?` operators that can cause an early `Err` return after phases complete are:

| Line | Code | Trigger condition |
|------|------|-------------------|
| 1487 | `.context("workflow execution failed")?` | Engine returns `Err` (phase failure) |
| 1595 | `tokio::fs::create_dir_all(parent).await?` | Filesystem error writing extracted code files |
| 1597 | `tokio::fs::write(&dest, &content).await?` | Filesystem error writing extracted code files |
| 1621 | `serde_json::to_string_pretty(&response)?` | JSON serialisation error (only with `--json`) |

**Lines 1595 and 1597 are the most likely source of BUG-1.** After all phases succeed (`engine.run_with_perf` returns `Ok`), `run_workflow` iterates over files extracted from the `code` phase output and writes them to `out_dir`. If any write fails (permissions, disk full, path too long, race with another process), the function returns `Err`, which propagates to `main` and causes exit 1.

### 3. `WorkflowEngine::run_with_perf()` — `src/workflow/engine.rs:380`

Returns `Result<(WorkflowContext, PerfRecord), WorkflowError>`. After all phases complete:

- Lines 1248–1260: Writes `workflow-report.md` — uses `map_err(WorkflowError::Io)?`, can return `Err`.
- Lines 1224–1230: Per-phase file extraction (inside phase loop, `produces_files: true`) — uses `map_err(WorkflowError::Io)?`, can return `Err`.
- Line 1347-1348: `if let Some(err) = first_error { return Err(err); }` — this is the phase failure path, only fires if a phase failed.
- Line 1351: `Ok((ctx, perf_record))` — the success return.

The `WorkflowError::Io` returns from the report write (line 1258) and per-phase file extraction (line 1228) can also cause exit 1, but only if the files are being written inside the engine before control returns to `run_workflow`.

### 4. The Postmortem Subprocess Red Herring

The `Error: no NDJSON line on stdin` in stderr logs (lines 60965 and 63508) is **not** the workflow process's exit. It is the postmortem sub-agent subprocess exiting with code 1 because:

1. `run_workflow` (line 1562-1574) fires `trigger_postmortem` in a background `tokio::spawn` task when mistakes are logged.
2. `trigger_postmortem` (line 2602) calls `SubprocessAgentRunner::new().run("postmortem-agent", &task)`.
3. `subprocess.rs:432-442` spawns a new `open-mpm --agent postmortem-agent` process.
4. The writer task writes the NDJSON task to the subprocess's stdin.
5. The sub-agent at `main.rs:2018-2022` reads stdin; if stdin is empty or closed too fast, line 2022 returns `Err("no NDJSON line on stdin")`.
6. That sub-agent exits 1.
7. Back in `subprocess.rs:455`: `write_res.context("writer task panicked")??` — if the write to stdin failed because the child closed stdin before reading (SIGPIPE / broken pipe), the write task itself fails.
8. `trigger_postmortem` returns `Err`, which is logged as `WARN postmortem agent dispatch failed: writer task panicked`.
9. The `tokio::spawn` in `run_workflow` (line 1569) is fire-and-forget; its error is only logged, never propagated.
10. **The workflow process itself continues and returns `Ok(())`.**

So the postmortem path does **not** cause the workflow's exit 1.

---

## Most Likely Root Causes of Exit 1 After Successful Phases

### Root Cause A (Primary): Post-engine file write failure — `src/main.rs:1595–1597`

```rust
// src/main.rs:1591-1598
let files = ipc::extract_files_from_content(code_output);
for (filename, content) in files {
    let dest = dir.join(&filename);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;   // line 1595 — exits 1 on Err
    }
    tokio::fs::write(&dest, &content).await?;        // line 1597 — exits 1 on Err
}
```

This executes **after** `engine.run_with_perf` returns successfully. Any I/O error here causes `run_workflow` to return `Err`, and `main` exits 1. Because this is the fallback extraction path for legacy workflow configs without `produces_files: true`, it runs even when the engine already wrote the files (idempotent but still prone to I/O errors on the second write).

### Root Cause B (Secondary): Workflow-report write failure — `src/workflow/engine.rs:1258`

```rust
// src/workflow/engine.rs:1252-1261
let target = dir.join("workflow-report.md");
if let Some(parent) = target.parent() {
    tokio::fs::create_dir_all(parent).await.map_err(WorkflowError::Io)?;
}
tokio::fs::write(&target, report).await.map_err(WorkflowError::Io)?;  // line 1260
```

This runs at the very end of `run_with_perf`, after all phases succeed. An I/O error here returns `WorkflowError::Io`, which propagates through `run_workflow:1487` as `Err` and causes exit 1.

---

## Evidence from Stderr Log

The stderr log at `~/.open-mpm/logs/api-stderr.log` shows no post-phase-completion errors on successful runs — all the `Error: workflow execution failed` entries are genuine phase failures (research-agent exiting 1 due to Bedrock API errors, or OpenRouter 402). The `Error: no NDJSON line on stdin` entries are the postmortem subprocess, not the workflow process.

This means BUG-1 is likely a **transient I/O error** (not consistently reproducible from logs alone) on the post-engine file write at `src/main.rs:1595-1597` or the report write at `src/workflow/engine.rs:1258-1260`.

---

## Recommended Fix

Convert the post-engine fallback file writes in `run_workflow` to non-fatal (log + continue) since the engine already writes these files when `produces_files: true`:

```rust
// src/main.rs:1591-1598 — change from fatal ? to non-fatal warn
let files = ipc::extract_files_from_content(code_output);
for (filename, content) in files {
    let dest = dir.join(&filename);
    if let Some(parent) = dest.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::warn!(error = %e, path = %dest.display(), "fallback: failed to create dir");
            continue;
        }
    }
    if let Err(e) = tokio::fs::write(&dest, &content).await {
        tracing::warn!(error = %e, path = %dest.display(), "fallback: failed to write extracted file");
    }
}
```

Similarly, the workflow-report write at `engine.rs:1258-1260` could be made non-fatal since the output files are already on disk and the report is supplementary.

---

## Files Referenced

- `/Users/masa/Projects/open-mpm/src/main.rs:1487` — engine call with `?`
- `/Users/masa/Projects/open-mpm/src/main.rs:1562-1574` — postmortem fire-and-forget spawn
- `/Users/masa/Projects/open-mpm/src/main.rs:1591-1598` — **primary suspect**: post-engine fallback file writes
- `/Users/masa/Projects/open-mpm/src/main.rs:1621` — JSON output path (only with `--json`)
- `/Users/masa/Projects/open-mpm/src/main.rs:2016-2022` — sub-agent stdin read (source of "no NDJSON" error)
- `/Users/masa/Projects/open-mpm/src/workflow/engine.rs:1248-1261` — **secondary suspect**: workflow-report write
- `/Users/masa/Projects/open-mpm/src/subprocess.rs:432-455` — writer task and "writer task panicked" error
- `/Users/masa/Projects/open-mpm/src/api/server.rs:1032-1036` — server treats exit 1 as error
