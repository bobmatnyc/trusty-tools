# Projects Feature Foundation Research

**Date**: 2026-05-05  
**Scope**: Existing infrastructure for an expanded `Project` model with: `path`, `git_origin`, `last_active`, `open_issues_count`, `open_prs_count`, `tmux_sessions`.

---

## 1. `/projects` Slash Command

**File**: `src/repl/mod.rs`

The command is handled at **line 695**:

```rust
"/projects" => {
    self.print_projects_into(&mut out).await;
    Ok(true)
}
```

`print_projects_into` is implemented at **line 1000–1037**. What it does today:

1. Prints `"Current project: <name>  (<path>)"` from `self.project_name` / `self.project_dir`.
2. Calls `crate::registry::ProjectRegistry::new()` and calls `.load().await` to get a `HashMap<String, ProjectEntry>`.
3. Sorts entries by name, iterates and prints `"  * <name>  (<path>)"` (marking the current project with `*`).
4. Prints `"\nUse /connect <path> to switch projects."`.

The command renders **name + path only**. No last_active, no git origin, no issue/PR counts, no session counts.

---

## 2. TM Project Tracking

### `TmProject` struct — `src/tm/project.rs`

```rust
pub struct TmProject {
    pub id: String,                    // UUID v4
    pub name: String,                  // dir basename by default
    pub path: PathBuf,                 // absolute project root
    pub session_ids: Vec<String>,      // TmSession IDs
    pub created_at: DateTime<Utc>,
    pub framework: DetectedFramework,  // language/framework detection
    pub sessions: Vec<SessionSummary>, // compact session records
    pub process_state: ProjectProcessState, // PIDs, token count, memory snapshot
}
```

Supporting types (all in `src/tm/project.rs`):

- **`DetectedFramework`** — `language`, `framework`, `package_manager`, `detected_from`, `detected_at`
- **`SessionSummary`** — `session_id`, `name`, `adapter_type` (ClaudeMpm/ClaudeCode/Codex/…), `status`
- **`TmSession`** — full session record including `last_active: DateTime<Utc>` (line 254), with `.touch()` and `.last_active_ago()` helpers

**`TmProject` is missing `last_active` at the project level** — it exists only on individual `TmSession` records.

**Detection** (`src/tm/manager.rs`, line 265–271): `get_or_create_project` calls `detect_framework(path)` via `src/tm/framework.rs` when creating a new project.

**Persistence**: All projects and sessions are stored in `.open-mpm/state/tm_sessions.json` (JSON, schema_version=1, with top-level `"projects"` array and `"sessions"` array). On disk, `TmProject` records carry `session_ids`, `sessions`, `framework`, `process_state`. No `git_origin` field exists anywhere.

---

## 3. Harness Activity Tracking

### State directory layout (`.open-mpm/state/`)

| Path | Contents |
|---|---|
| `sessions.json` | PM session records (id, started_at, workflow, status) |
| `tm_sessions.json` | TM project+session registry (JSON, 23 projects / 27 sessions on disk) |
| `build.json` | Build counter state |
| `usage.json` / `usage.jsonl` | Token/cost usage log |
| `history/entries.jsonl` | Append-only interaction history JSONL |
| `interactions/build<N>.jsonl` | Per-build interaction logs |
| `logs/` | Chat logs (`chat-YYYY-MM-DD.log`) |
| `tasks.json` | Recent task records |
| `runs/` | Perf run JSON files |
| `mistakes/` | Mistake log |

### `ProjectEntry` in global registry — `src/registry/mod.rs`

```rust
pub struct ProjectEntry {
    pub path: PathBuf,
    pub name: String,
    pub last_run: Option<DateTime<Utc>>,      // last workflow execution
    pub status: ProjectStatus,                 // Active | Idle | Removed
    pub last_connected: Option<DateTime<Utc>>, // last CTRL PM spawn (line 56)
    pub pm_count: u64,                         // incremented on every PM start (line 62)
    pub is_self: bool,                         // marks open-mpm's own source tree
}
```

Stored at **`~/.open-mpm/projects.json`** (global, not per-project). Atomic writes via tmp+rename.

**`last_run`** is set when a workflow executes. **`last_connected`** is set by `register_pm_start`. Neither field is called `last_active` but together they cover the concept.

**No `git_origin` field anywhere in `ProjectEntry` or `TmProject`.**

### Activity timestamp write sites

- `src/interaction_log.rs:115` — stamps `timestamp: now_iso8601()` on every turn
- `src/subprocess.rs:227` — `timestamp: chrono::Utc::now()` on subprocess events
- `src/tm/project.rs:301` — `TmSession::touch()` stamps `last_active = Utc::now()`
- `src/tools/tm_tools.rs:90` — reads `s.last_active_ago()` for rendering
- `src/main.rs:980` — records `last_used` timestamp for skill usage

---

## 4. WebUI

**Stack**: Svelte + Vite + Tailwind + TypeScript (confirmed by `svelte.config.js`, `vite.config.ts`, `tailwind.config.js`, `tsconfig.json`). Tauri shell in `ui/src-tauri/`.

### Frontend `Project` interface — `ui/src/stores/app.ts` line 13

```typescript
export interface Project {
  id: string;
  name: string;
  path: string | null;
  status: 'idle' | 'running' | 'error';
}
```

Minimal — id, name, path, status only. No git_origin, no last_active, no issue counts.

The store (`projects` writable, line 49) starts with a single CTRL entry. Projects are added via `addProject()` (line 184). Active project derived store at line 104.

### API server — `src/api/server.rs`

**Framework**: `axum` (confirmed line 26-35).

**Existing routes** (lines 547–560):

```
POST /api/task
GET  /api/task/:id
GET  /api/tasks
POST /api/clear-context
GET  /api/health
GET  /api/config
GET  /api/docs/search
GET  /api/events     (SSE)
GET  /               (SPA index)
GET  /*path          (SPA static assets)
```

**No `/api/projects` endpoint exists.** The UI currently manages the project list purely client-side.

---

## 5. GitHub Integration

**Ticketing module**: `src/ticketing/` with backends `github.rs`, `gh_cli.rs`, `linear.rs`, `jira.rs`.

### Configuration

- `GITHUB_TOKEN` + `GITHUB_REPO` env vars (`src/ticketing/mod.rs` lines 219–220)
- `GlobalConfig.github` section (`src/mcp/config.rs` line 61) with `[[github.identities]]` support
- `github_identity(name)` method at `src/mcp/config.rs` line 205

### GitHub REST client — `src/ticketing/github.rs`

Uses raw `reqwest`. Issues URL at line 75: `GET https://api.github.com/repos/{owner}/{repo}/issues`. The `list_tickets` implementation (line 283) filters by state. **No PR listing endpoint** — only issues are queried.

### `gh` CLI client — `src/ticketing/gh_cli.rs`

`GhCliClient` at line 50 shells out to the `gh` CLI. `list_tickets` (line 340) wraps `gh issue list`. **No `gh pr list` call** in this client currently.

### `TicketingClient` trait — `src/ticketing/mod.rs` line 43

```rust
async fn list_tickets(&self, filter: TicketFilter) -> Result<Vec<Ticket>>;
```

No PR-specific method on the trait. Open PR count would need a new trait method or a separate `gh pr list --json number --state open | jq length` shell call.

### Git remote detection

**None found.** There is no call to `git remote get-url origin` anywhere in `src/`. The `git_origin` field you want to add to `Project` would need to be newly implemented — likely via `git2` crate or a `Command::new("git").args(["remote","get-url","origin"])` subprocess call.

---

## 6. State Persistence

| Store | Format | Location | Used for |
|---|---|---|---|
| Global project registry | JSON (`HashMap<String, ProjectEntry>`) | `~/.open-mpm/projects.json` | Cross-project list, last_run, last_connected |
| TM sessions + projects | JSON (`{schema_version, sessions: [...], projects: [...]}`) | `.open-mpm/state/tm_sessions.json` | TmProject + TmSession records |
| Session log | JSON array | `.open-mpm/state/sessions.json` | PM session start records |
| Interaction history | NDJSON (append-only) | `.open-mpm/state/history/entries.jsonl` | Turn-level history |
| Usage | JSONL | `.open-mpm/state/usage.jsonl` | Token/cost accounting |

**No SQLite.** Everything is JSON/JSONL. No `projects.json` inside the per-project `.open-mpm/` (only the global `~/.open-mpm/projects.json`).

---

## Gap Analysis for the Target `Project` Model

| Field | Status | Location to add |
|---|---|---|
| `path` | Exists in `ProjectEntry.path` and `TmProject.path` | Already present |
| `git_origin` | **Missing everywhere** | Add to `ProjectEntry`; populate via `git remote get-url origin` subprocess at registration time |
| `last_active` | Indirect via `last_run` / `last_connected` in `ProjectEntry`; at session level in `TmSession.last_active` | Add derived field to `ProjectEntry` (max of last_run / last_connected) or rename |
| `open_issues_count` | **Missing** — `list_tickets` can enumerate but no count field stored | Add to `ProjectEntry`; populate by calling `gh issue list --state open --json number \| jq length` via `GhCliClient` or GitHub REST |
| `open_prs_count` | **Missing** — no PR listing in current trait | Add `list_prs`/`count_open_prs` to `TicketingClient` trait; implement in `gh_cli.rs` via `gh pr list --state open` |
| `tmux_sessions` | Exists in `TmProject.sessions` (`Vec<SessionSummary>`) | Join `TmProject` data when building the expanded `Project` view |

### Recommended approach

1. **Enrich `ProjectEntry`** (in `src/registry/mod.rs`) with `git_origin: Option<String>`, `open_issues: Option<u32>`, `open_prs: Option<u32>`. These are cached/stale-acceptable values, refreshed at PM start or on demand.
2. **Populate `git_origin`** in `register` / `register_pm_start` via a non-blocking `tokio::process::Command` call to `git -C <path> remote get-url origin`.
3. **Add `/api/projects` route** in `src/api/server.rs` (axum, line 547) returning `Vec<ProjectEntry>` enriched with session data from TM registry.
4. **Add PR count** to `GhCliClient` via `gh pr list --state open --json number` (consistent with existing `gh issue list` pattern in `list_tickets`).
5. **Derive `last_active`** from `max(last_run, last_connected)` — no schema change needed, just a computed field in the API response.
6. **Extend the frontend `Project` interface** in `ui/src/stores/app.ts` to match the new backend response shape.
