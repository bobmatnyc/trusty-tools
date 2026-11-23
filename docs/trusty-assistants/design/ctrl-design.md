# CTRL Design

CTRL (Control) is open-mpm's interactive multi-project session manager
and the default mode of the binary. It hosts the **Taskmaster** persona —
an autonomous coordination controller that drives tasks to completion
across multiple PM actors.

> Implementation: `src/ctrl/mod.rs`.

## Goals

1. **One REPL, many projects**: switch between project contexts without
   restarting the binary
2. **Autonomous task management**: when a phase fails, retry with
   adjusted context before escalating
3. **Cross-project coordination**: relay messages between PMs running in
   different projects via a UNIX-socket message bus
4. **Self-awareness**: when running from open-mpm's own checkout, expose
   `self_project_status` and `initiate_self_task` so the user can run
   improvement tasks against the harness itself
5. **Doc-aware answers**: a built-in `search_docs` tool over the project's
   `docs/` tree so the assistant can answer "how does X work?" without
   round-tripping every query to a heavyweight LLM call

## Architecture

```
                ┌───────────────────────────────┐
                │  CTRL REPL (main thread)      │
                │  • prompt loop                │
                │  • slash commands             │
                │  • LLM turn dispatch          │
                └──────────────┬────────────────┘
                               │
                               ├──── per-project ──────► PM Actor (tokio task)
                               │                          │
                               │                          ├── mpsc<PmMsg> channel
                               │                          ├── runs llm::chat_with_tools
                               │                          └── delegates to sub-agents
                               │
                               ├──── docs_index ───────► Arc<Mutex<Option<DocsIndex>>>
                               │                          │
                               │                          └── built in spawn_blocking
                               │                              after REPL starts
                               │
                               └──── MessageBus ──────► UNIX socket pub/sub
                                                          (~/.open-mpm/sockets/ctrl.sock)
```

## State

```rust
struct Ctrl {
    pms: HashMap<String, PmHandle>,           // keyed by project path
    active: Option<String>,                   // currently focused PM
    bus: Option<Arc<MessageBus>>,             // inter-project relay
    connected_pms: Arc<Mutex<HashMap<String, mpsc::Sender<PmMsg>>>>,
    memory: Arc<Mutex<Vec<String>>>,          // in-memory fallback memory
    self_project: Option<PathBuf>,            // detected self-checkout
    docs_index: Arc<Mutex<Option<Arc<DocsIndex>>>>,  // #187
}

struct PmHandle {
    name: String,
    project_path: PathBuf,
    tx: mpsc::Sender<PmMsg>,
    task: JoinHandle<()>,
    status: Arc<Mutex<String>>,               // "running" | "idle" | "error"
    last_message: Arc<Mutex<String>>,
}
```

## PM actor protocol

```rust
enum PmMsg {
    Task { text: String, reply: oneshot::Sender<Result<String>> },
    Shutdown,
}
```

Each PM actor is a tokio task that owns a long-lived LLM session for one
project path. CTRL sends `Task` messages and awaits the reply. The actor
loops until it receives `Shutdown` or its mpsc receiver closes.

## Tool registry

CTRL builds a per-turn tool registry containing only tools relevant to
the Taskmaster persona:

| Tool | Purpose |
|---|---|
| `start_pm(path)` | Connect to a project, spawning a PM actor if needed |
| `list_projects()` | Read `~/.open-mpm/projects.json` |
| `task_status()` | Snapshot of all PM handles' status + last_message |
| `self_project_status()` | Version + last 3 commits of the open-mpm checkout |
| `initiate_self_task(task)` | Queue a self-improvement task |
| `memory_store(key, value)` | In-memory fallback (Kuzu MCP not assumed) |
| `memory_recall(key)` | Read back from in-memory store |
| `search_docs(query)` | TF-IDF search over `<project>/docs/` (#187) |

The registry is rebuilt fresh for each LLM turn. Side-effects (like
"start a PM after this turn returns") are deferred via `Arc<Mutex<Option>>`
slots that the caller drains after the LLM returns.

## search_docs tool (#187)

### Why

CTRL needs to answer "how does open-mpm work?" questions without:
- Hitting an LLM for every doc lookup (slow, expensive)
- Depending on a vector DB (zero-ops principle)
- Reading every Markdown file from disk per query

### How

A TF-IDF index built once at CTRL startup:

1. `tokio::spawn` a background task
2. `tokio::task::spawn_blocking` walks `<self_project_or_cwd>/docs/`
3. Reads every `*.md`, tokenizes, computes term frequencies
4. Computes IDF over the corpus, derives L2-normalized TF-IDF vectors
5. Installs the index into `Arc<Mutex<Option<Arc<DocsIndex>>>>`

The tool falls back gracefully: if the index isn't installed yet, it
returns "docs index not yet built (try again in a moment)".

### Output

```json
[
  { "path": "user/quickstart.md", "title": "Quickstart", "snippet": "…", "score": 0.84 },
  { "path": "design/goals.md",    "title": "Project Goals", "snippet": "…", "score": 0.41 }
]
```

The CTRL LLM uses these results to compose its answer with citations.

## Slash commands

```
/help                  — list commands
/connect <PATH>        — start (or switch to) a PM session for PATH
/disconnect            — leave the active PM (it keeps running)
/status                — list all PM sessions
/exit | /quit          — leave CTRL
```

Non-slash input is routed to either:

- The active PM actor (if `connect` has been used), or
- A CTRL-level LLM turn (Taskmaster persona) that can choose to call
  `start_pm` to auto-route to a project.

## Inter-project message bus

`src/bus.rs` exposes a UNIX-socket pub/sub bus. CTRL starts its own bus
on `~/.open-mpm/sockets/ctrl.sock` so other open-mpm processes can
discover and message it. Inbound envelopes:

1. Print to stderr (`[BUS] from <project>: <message>`)
2. Append to `~/.open-mpm/sessions/pm-messages.jsonl` (audit log)
3. Forward to a connected PM actor when `target_project` matches a
   registered handle's basename

## Self-project detection (#182)

`detect_self_project` walks ancestors looking for a `Cargo.toml` whose
`[package].name == "open-mpm"`. When found:

- Registers the path in `ProjectRegistry` as `self_project = true`
- Sets `Ctrl::self_project = Some(path)`
- Augments the system prompt: "You are running inside your own project at
  `<path>`. You can check your own status with `self_project_status()`
  and initiate development tasks with `initiate_self_task(task)`."

## Why an actor model?

Each project's PM holds a long-lived conversation with its LLM (skill
injections, CLAUDE.md context, history compaction). Spinning these up
on every user message would be expensive and stateless. The actor pattern
lets us:

- Keep one PM per project alive for the duration of the CTRL session
- Avoid `Mutex<HashMap<…, PmState>>` contention (each actor owns its own state)
- Scale to dozens of concurrent PMs cheaply (tokio tasks are lightweight)

## Why a separate Taskmaster persona?

CTRL's job is meta: it routes work to PMs and reports on their progress.
A normal PM persona (which thinks in terms of single-task delegation)
doesn't fit. The Taskmaster persona is tuned for:

- Proactive status updates
- Retrying failures up to 2 times before escalating
- Tracking task state across PMs
- Post-task debriefs (what was built, test results, retries, cost)

See `CTRL_SYSTEM_PROMPT` in `src/ctrl/mod.rs` for the full text.

## Future work

- Persistent PM state across CTRL restarts (currently in-memory only)
- Hot-reload of agent TOMLs without dropping PM sessions
- A CTRL-level audit log mirroring `pm-messages.jsonl` for slash-command
  history
- Token budget for `search_docs` snippets (currently fixed at 300 chars)
