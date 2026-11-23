# Workflow Engine

Declarative multi-phase orchestration for open-mpm. Lets users describe a
pipeline as JSON instead of writing imperative Rust code that calls each
agent in turn.

> Background research: `docs/research/workflow-engine-design.md`,
> `docs/research/agent-decomposition-patterns.md`.

## Goals

1. **Declarative**: a workflow is data, not code
2. **Composable**: phases can run sequentially, in parallel, or in waves
3. **Observable**: each phase emits structured progress + perf telemetry
4. **Crash-safe**: interrupted runs leave the workspace in a recoverable state
5. **Worktree-isolated**: parallel phases get their own git worktree to
   prevent cross-contamination

## Workflow definition

```json
{
  "name": "prescriptive",
  "description": "research → plan → code → qa → observe",
  "phases": [
    { "name": "research", "agent": "research-agent", "context_template": "…" },
    { "name": "plan",     "agent": "plan-agent",     "produces_files": true },
    { "name": "code",     "agent": "code-agent",     "wave_loop": true },
    { "name": "qa",       "agent": "qa-agent" },
    { "name": "observe",  "agent": "observe-agent" }
  ]
}
```

User-facing reference: [`docs/user/configuration.md`](../user/configuration.md#workflow-json).

## Execution model

### Phase loop

```rust
for phase in &workflow.phases {
    let prompt = resolver::render(&phase.context_template, &context);
    let output = runner.run(phase.agent, prompt).await?;
    if phase.produces_files {
        ipc::extract_files_from_content(&output, out_dir)?;
    }
    context.set(phase.name, output);
    perf.record(phase, &output);
    progress::emit(phase.name, "done");
}
```

### Wave loop

When `wave_loop: true` and the previous phase produced an
`assignments.json` file, the engine reads it, computes a topological
ordering of file dependencies, and spawns one sub-agent per file in
each wave (parallelizing within a wave).

```
assignments.json:
  { "files": [
      { "path": "src/a.py", "deps": [] },
      { "path": "src/b.py", "deps": ["src/a.py"] },
      { "path": "src/c.py", "deps": ["src/a.py"] }
  ]}

→ Wave 1: a.py
→ Wave 2: b.py, c.py (parallel)
```

### Parallel subtasks

When `parallel_subtasks: ["frontend", "backend"]` is set, the engine
spawns each subtask concurrently via `FuturesUnordered`. Each subtask
runs the same agent with a different `{{subtask}}` template variable.

### Worktree protection

When `worktree_protection: true`, each parallel subtask runs in its own
git worktree under `.open-mpm/state/worktrees/<phase>/<label>/`. The
`WorktreeManager` creates them on entry and cleans them up on exit (or
on the next run if the previous run was interrupted).

## Template resolution

`src/workflow/resolver.rs` substitutes `{{var}}` patterns in
`context_template`:

| Variable | Source |
|---|---|
| `{{task}}` | The user's original task string |
| `{{out_dir}}` | The `--out-dir` path |
| `{{phase_name}}` | The current phase's name |
| `{{<phase_name>}}` | The output of any previous phase |
| `{{subtask}}` | The current parallel subtask label (when applicable) |
| `{{file_path}}` | The current wave-loop file (when applicable) |

## Progress streaming

The engine emits to stderr at each phase transition:

```
__OMPM_PROGRESS__ {"phase":"plan","status":"running","ts":"2026-04-24T12:00:00Z"}
__OMPM_PROGRESS__ {"phase":"plan","status":"done","duration_ms":4321}
```

The API server's `run_task` reads these lines off the child's stderr and
appends them to the in-memory `PmResponse.phases_completed` so polling
clients (web UI, Tauri) can render a live timeline.

## Perf telemetry

`PerfCollector` writes a single JSON file per run to
`docs/performance/runs/<timestamp>-build<N>.json` containing:

```json
{
  "run_id": "uuid",
  "build": 187,
  "workflow": "prescriptive",
  "phases": [
    {
      "name": "research",
      "duration_ms": 12345,
      "tokens": { "input": 1024, "output": 512, "cache_read": 0, "cache_write": 0 },
      "model": "anthropic/claude-sonnet-4-6",
      "files_written": []
    }
  ],
  "total_cost_usd": 0.0234
}
```

These files are committed (small JSON, one per run) so the harness has
a long-running history of performance and cost.

## Auto-push

When `auto_push.enabled = true`, after a successful run the engine:

1. Stages changes under `out_dir` and any tracked file in the workspace
2. Creates a commit with a message derived from the task
3. Pushes to the configured branch (or skips if no remote)

## Ticket management

When `ticket_management.enabled = true`, the engine wraps each phase
with `TicketManager` calls (via the `gh` CLI):

- Phase start → comment on the ticket "starting <phase>"
- Phase done → comment "completed <phase>"
- Phase fail → reopen the ticket with the error context

## Failure recovery

If a phase fails, the engine:

1. Records the failure in `mistake_log.rs`
2. Emits `__OMPM_PROGRESS__` with `status:"error"`
3. Returns `WorkflowError` to the caller, which can choose to retry
4. CTRL's Taskmaster persona retries up to 2 times with adjusted context
   before escalating

## Why JSON, not TOML or YAML?

- JSON is the lingua franca for machine-generated configs (the LLM-
  generated `assignments.json` is also JSON)
- Avoids YAML's whitespace footguns
- Avoids TOML's nested-array verbosity for the `phases` list
- Round-trips cleanly through `serde_json`

## Future work

- Conditional phases (`run_if: "{{plan.success}}"`)
- Cross-workflow composition (`include: "other-workflow.json"`)
- Streaming partial results between phases (currently each phase fully
  completes before the next starts)
