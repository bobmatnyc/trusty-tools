# DOC-22 — Multi-Project / Multi-Repo Aware Session Manager (NL→repo routing)

**Status:** Draft
**Subsystem:** trusty-mpm — session-manager / routing layer
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-20
**Spec ID:** `SPEC-MULTIREPO-01~draft` (DOC-22)
**Builds on:** DOC-14 — Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`,
the `SessionControl` and `ManagedSessionId` contract); DOC-16 — Interactive Sessions TUI
(`docs/specs/sessions-tui-interactive.md`, the STUI session-listing and active-session UX);
DOC-19 — TELUI (`docs/specs/telui-telegram-ui.md`, the Telegram multi-session routing and
focused-session paradigm); DOC-20 — Chat-Core (`docs/specs/chat-core.md`, the shared
`TrustyCommand` dispatch nucleus and resolver contract).
**Cross-ref:** the session-manager core (`crates/trusty-mpm/src/session_manager/`),
`SessionRecord.repo_url` and `SessionRecord.branch` (`crates/trusty-mpm/src/session_manager/record.rs`),
`SessionControl::LaunchParams` (`crates/trusty-mpm/src/core/sm/control.rs`), the managed-session
MCP tools (`session_new`, `session_send`, etc., `crates/trusty-mpm/src/mcp/`), the config
schema (`trusty-mpm.yaml`, `~/.trusty-tools/trusty-mpm/config.yaml`), and epic **#1517**
(multi-project awareness), issues **#1433** (chat-core seam), **#1405** (session routing),
**#832** (multi-repo), **#1272** (SM TUI).

> **Scope note.** This is a **behavior-contract** spec for the **multi-repo routing and
> project-awareness layer** that sits **on top of** the already-merged session-manager core
> (DOC-14, `SessionManager`, `SessionRecord`, lifecycle hooks). It specifies: (1) a named
> project registry keyed by project name with `repo_url`, `default_branch`, and metadata;
> (2) an NL-task→repo resolver that maps free-text input (GitHub URLs, ticket IDs, keywords)
> to `(project_name, repo_url, ref)` with confidence scoring; (3) session↔project binding
> that tracks which live session serves which project; (4) Telegram multi-project UX
> (focused-session routing, `/new` spawn guidance, `/fleet` grouping). It does **not**
> re-spec the session lifecycle, the provisioner, the Telegram primitives (those are DOC-19),
> or the cross-repo task coordination later phase. The routing layer is **harness-agnostic**
> and must work through the existing runtime-adapter seam so `tcode` and other runtimes can
> adopt it later. This spec defines the routing contract; implementing modules (project registry,
> resolver, Telegram routing) are the **what**; the **how** is left to phase planning.

---

## 1. Motivation

Before chat-core, every UI surface (the Telegram bot, the TUI dashboard, the `tm` CLI)
translated operator intent into daemon HTTP calls independently. Three things described
the same verb set and could silently drift:

Today, a developer running trusty-mpm with multiple active projects (e.g. `trusty-tools`,
`trusty-search`, a private app repo) manages them through bare session UUIDs and manual
git-clone steps:

- `/sessions` lists all active sessions without project context.
- To spawn a new session for a specific repo, the operator must manually `cd`, clone, or
  remember the exact `repo_url` and pass it via a hypothetical `--repo` flag.
- Telegram `/send` to a session requires knowing which session is "for" which project — no
  guidance.
- A ticket reference (`PROJ-123`, `#456`) or GitHub URL has no automatic routing path;
  the operator must manually identify the project and resolve it to a session.
- When a session starts in repo `X`, there is no programmatic way to ask "which sessions
  are active for repo `Y`?" or "which project does this session serve?"

**Result:** multi-repo workflows are tedious, error-prone, and lack natural-language intent
routing.

The routing layer solves this by:
1. **Registering projects** — a named index (`my-app → repo_url + branch + metadata`)
2. **Resolving NL intent** — mapping `"PROJ-123"`, `"main branch of my-app"`, a GitHub URL,
   or a keyword onto `(project, repo_url, ref)`
3. **Binding sessions to projects** — tracking which live session serves which project
4. **Guided UX** — Telegram `/new` walks the operator through project/ref/task selection;
   `/fleet` groups sessions by project; a focused session receives `/send` input by default

This spec defines **what** the routing layer must do; **how** it integrates with the daemon,
MCP tools, and Telegram is left to phase planning.

---

## 2. Scope and non-goals

**IN SCOPE:**
- Project registry API and persistence (`project_list`, `project_register`, `project_get`)
- NL-to-project resolver: strategy, confidence scoring, disambiguation flow
- Session↔project binding index (built on `SessionRecord.repo_url`)
- Telegram multi-project UX patterns (focused-session routing, `/new` flow, `/fleet` grouping)
- Specification of seam gaps that block routing adoption (e.g. `LaunchParams.repo_url`)

**OUT OF SCOPE (already designed or deferred):**
- Session lifecycle (`SessionManager`, `SessionControl`) — DOC-14 and merged
- Workspace provisioning — merged; the routing layer **consumes** it
- Telegram primitives (callbacks, keyboards, HTML parse) — DOC-19
- Cross-repo task coordination, dependency tracking, broadcast — a later phase (DOC-17 north-star)
- Harness runner (`trusty-agents`) integration — future phase

---

## 3. Background and current state

### 3.1 Session Manager core (merged)

`SessionManager` in `crates/trusty-mpm/src/session_manager/` provides:
- Create/get/list/send_input/inject/observe/stop/resume/decommission operations
- Disk-backed store at `~/.trusty-mpm/sessions.json`
- `SessionRecord` (id, tmux_name, cwd, task, state, workspace_path, **repo_url**, **branch**)
- Workspace provisioner (clones `repo_url@ref` into `~/.trusty-mpm/workspaces/<project>/<id>/`)
- MCP tools: `session_new`, `session_stop`, `session_resume`, `session_decommission`, `session_activity`, `session_send`

`SessionRecord` already tracks `repo_url` and `branch` per session (file:
`crates/trusty-mpm/src/session_manager/record.rs`).

### 3.2 Session Control seam (`SessionControl`, `LaunchParams`)

`SessionControl` in `crates/trusty-mpm/src/core/sm/control.rs` defines the unified control API.
`LaunchParams` struct (currently) has:
- `workdir: String` (the working directory to launch into)
- `model: Option<String>` (runtime selector)
- `prompt: Option<String>` (initial task description)
- `goal_id: Option<String>` (SM-8 correlation)
- `ephemeral: Option<bool>` (session lifetime hint, #1508)

**SEAM GAP:** `LaunchParams` lacks `repo_url` and `ref` fields. The routing resolver must
thread these through `LaunchParams` (or an extended variant) so the provisioner receives
both the repo and the target ref. This is a known incompleteness; the routing layer
unblocks it.

### 3.3 Telegram managed-fleet commands (merged)

The Telegram bot (`crates/trusty-mpm/src/telegram/`) implements:
- `/launch`, `/fleet`, `/msend`, `/answer`, `/activity`, `/resume`, `/decommission`
- Single-operator auth and token from `.env.local`
- Inline keyboards for session lists with callback routing
- Chat-core seam (`TrustyCommand` → `CommandExecutor`)

**Current limitation:** routing is flat (all sessions in one list); no project awareness
or focused-session paradigm.

### 3.4 Chat-core nucleus (merged, DOC-20)

`CommandExecutor` in `crates/trusty-mpm/src/client/executor/` dispatches `TrustyCommand`
onto daemon HTTP calls. The resolver in `client/resolver.rs` applies a three-precedence
match (id-exact → name-exact → name-prefix) to resolve ambiguous session references.

The routing layer **extends** this resolver to incorporate project-aware precedence
(e.g. prefer sessions in the currently-focused project).

### 3.5 Config storage

`~/.trusty-tools/trusty-mpm/config.yaml` (currently) holds daemon/adapter settings.
The routing layer adds a `projects:` block:

```yaml
projects:
  - name: trusty-tools
    repo_url: https://github.com/trusty-inc/trusty-tools.git
    default_branch: main
    description: Workspace consolidation of trusty-* AI tools
    tags: [rust, workspace]
  
  - name: trusty-search
    repo_url: https://github.com/trusty-inc/trusty-search.git
    default_branch: main
    tags: [rust, search-daemon]
```

---

## 4. Requirements

### 4.1 Project Registry {#SPEC-MULTIREPO-01-registry}

**MR-1:** The system SHALL maintain a **named project registry** persisted to
`~/.trusty-mpm/projects.json`, mapping project name to metadata.

**Intent:** A project is the stable unit of organization; naming allows operators to
reference projects in natural language without memorizing URLs.

**What:** A `Project` struct with:
- `name: String` (unique identifier, e.g. `"trusty-tools"`)
- `repo_url: String` (full Git URL, e.g. `https://github.com/trusty-inc/trusty-tools.git`)
- `default_branch: String` (canonical branch, e.g. `"main"`)
- `description: Option<String>` (searchable metadata)
- `tags: Vec<String>` (categories, e.g. `["rust", "workspace"]`)
- `created_at: DateTime<Utc>`
- `last_used_at: Option<DateTime<Utc>>`

Persistence: JSON file at `~/.trusty-mpm/projects.json` (parallel to `sessions.json`).
Auto-register on boot: when daemon starts, scan `~/.trusty-mpm/sessions.json` and extract
`(repo_url, branch)` pairs; if a pair is new, create a temporary `Project` entry with
inferred `name` (e.g. from the repo URL last path component) and mark it as `provisional`.

**Accepting:** MCP tool `project_register(name, repo_url, default_branch, description?, tags?)`.
MCP tool `project_list() → Vec<Project>`.
MCP tool `project_get(name_or_url) → Option<Project>`.

**Test:** Unit tests verify JSON round-trip, uniqueness by name, and auto-registration
on daemon boot from session history.

---

### 4.2 NL-to-Project Resolver {#SPEC-MULTIREPO-01-resolver}

**MR-2:** The system SHALL implement a **natural-language task↔project resolver** that
maps free-text input onto `(project_name, repo_url, default_branch)` with a confidence
score and a structured disambiguation API.

**Intent:** Operators should not have to know exact URLs or project names; a ticket ID,
GitHub URL, or keyword hint is enough.

**What:** A resolver function with signature (pseudo-Rust):

```rust
pub fn resolve_project(
    query: &str,           // free-text: "PROJ-123", "#456", "github.com/org/repo", "main branch", keyword
    projects: &[Project],
    sessions: &[SessionRecord],
) -> Result<ProjectResolution, ResolverError>

pub struct ProjectResolution {
    pub matches: Vec<ProjectMatch>,     // ordered by confidence (0.0…1.0)
    pub primary: ProjectMatch,          // highest-confidence match (or error if ambiguous)
}

pub struct ProjectMatch {
    pub project: Project,
    pub confidence: f32,                // 0.0 (no match) … 1.0 (certain)
    pub reason: ResolutionReason,       // why this matched
}

pub enum ResolutionReason {
    ExactNameMatch,                     // "trusty-tools" → project named "trusty-tools"
    UrlMatch,                           // "github.com/trusty-inc/trusty-tools" → project URL
    TicketLookup,                       // "PROJ-123" → looked up in issue tracker (future)
    KeywordMatch,                       // "search" → project with tag "search-daemon"
    EmbeddingMatch,                     // semantic similarity in description
}
```

**Matching strategy (ordered by precedence):**
1. **Exact name match** — query equals a project name (confidence 1.0)
2. **URL match** — query is a GitHub URL or URL-like; extract the repo name and match
   against project `repo_url` (confidence 0.95)
3. **Ticket lookup** (optional Phase 2) — query matches ticket-ID pattern (`PROJ-123`, `#456`);
   look up in the issue tracker and resolve to project (confidence varies, typically 0.8)
4. **Keyword match** — query words appear in project `name`, `description`, or `tags`
   (confidence 0.6–0.8 depending on match specificity)
5. **Embedding match** (optional Phase 2) — encode query and project descriptions as embeddings;
   find nearest-neighbor (confidence varies)
6. **Fallback** — no automatic match; offer disambiguation picker to operator

**Disambiguation:** If multiple matches exist with confidence > threshold (e.g. 0.6), return
the highest-confidence match but expose all matches to the caller. The Telegram routing UX
or other adapters can then present a picker: "Did you mean `trusty-tools` or `trusty-search`?"

**Accepting:** MCP tool `resolve_project(query, resolver_settings?) → ProjectResolution`.
Optional: `resolve_project_interactive(query, operatorChoice?) → Project` (for Telegram picker flow).

**Test:** Unit tests for exact-name, URL, keyword matching. Integration test with session history.
Mock ticket lookup for Phase 1 (return `Unimplemented`). Embedding match is deferred to Phase 2.

---

### 4.3 Session↔Project Binding {#SPEC-MULTIREPO-01-binding}

**MR-3:** The system SHALL track the relationship between a live session and a project
for routing and fleet-management purposes.

**Intent:** Given a session, we must know which project it serves so `/fleet` can group by
project and focused-session routing can default to sending input to the focused session's
project.

**What:** `SessionRecord` already contains `repo_url` and `branch`. The binding is implicit:
a session belongs to a project if its `repo_url` matches a `Project.repo_url`. A lookup
function resolves this:

```rust
pub fn resolve_session_project<'a>(
    session: &SessionRecord,
    projects: &'a [Project],
) -> Option<&'a Project>
```

An **active-session index** (computed on each `project_list` / session-list operation) groups
sessions by project:

```rust
pub struct ProjectFleet {
    pub project: Project,
    pub active_sessions: Vec<SessionRow>,
    pub last_activity: Option<DateTime<Utc>>,
}
```

A **focused-session** binding (per adapter, e.g. per Telegram chat) tracks which session
is currently the target of operator input. The chat-core resolver (DOC-20) **extends**
to respect focused-session state:

1. If a session reference is ambiguous and a focused session exists, prefer the focused
   session's project's sessions.
2. If no session reference is given (bare `/send <msg>` in Telegram), route to the focused
   session (or error if none).

**Accepting:** `SessionManager::project_for_session(id, projects) → Option<Project>`.
`SessionManager::fleet_by_project(projects) → Vec<ProjectFleet>`.
Adapter state: `focused_session_id: Option<ManagedSessionId>` (stored in adapter, not daemon).

**Test:** Unit tests verify matching by `repo_url`, fleet grouping, and focused-session
precedence in the resolver.

---

### 4.4 Telegram Multi-Project UX {#SPEC-MULTIREPO-01-telegram-ux}

**MR-4:** Telegram routing SHALL implement guided multi-project workflows: `/new` spawn flow,
focused-session paradigm, and `/fleet` grouping.

**Intent:** Operators without project context should be walked through the choice; active
sessions should be grouped by project for clarity; a "focused" session receives `/send`
input by default.

**What:**

#### 4.4.1 `/new` spawn flow (guided)

`/new` command SHALL prompt the operator through a sequence:

1. **Project selection** — inline keyboard picker from `project_list()`.
   - Display: `project.name` + `project.description` (truncated if long)
   - Buttons: one per project; callback `new:project:<name>`
2. **Ref selection** — once project chosen, inline keyboard for branch/tag selection.
   - Display: default branch + recent branches from session history for that project
   - Buttons: default branch + 3–5 recent; callback `new:ref:<name>:<branch>`
3. **Task description** — text input for `prompt` (existing `/new` flow, unchanged)
4. **Confirmation** — two-step confirm before spawn (to prevent accidents)

Adaptation: DOC-19 (TELUI-6, TELUI-9) defines the Telegram `/new` verb; this spec owns
the **multi-project semantics** (project/ref picker); TELUI owns the Telegram primitives.

#### 4.4.2 Focused-session routing

An operator can **focus** on a session by:
- Clicking a session in the `/fleet` list (callback `focus:<id>`)
- Running `/send` on a specific session (unfocus any prior, focus this one)
- Spinning up a new session via `/new` (auto-focus it)

When a session is **focused**:
- The pinned statusline (DOC-19 §3.4) highlights it (e.g. **bold** in the message or a
  `[⭐ FOCUSED]` indicator)
- Bare `/send <msg>` without a session reference routes to the focused session's `/send`
  MCP endpoint
- Operator input in the chat (plain text) is routed to the focused session (existing behavior,
  unchanged in implementation; clarified in spec)
- `/msend <n> <msg>` still allows inline addressing by ordinal `<n>` (DOC-19 §2)

Per-chat state: the Telegram adapter stores `focused_session_id: Option<ManagedSessionId>`
(in-memory or in adapter-local store; **not** persisted to the daemon).

#### 4.4.3 `/fleet` grouping by project

`/fleet` command SHALL return an inline-keyboard list grouped by project:

```
📦 trusty-tools (3 active)
├─ 🟢 Session A (main)
├─ 🟢 Session B (feature/X)
└─ ⏳ Session C (summarizing)

📦 trusty-search (1 active)
└─ 🟢 Session D (main)

📦 Provisional projects (1 active)
└─ 🟢 Session E
```

- Sections: one per project (from `fleet_by_project()`)
- Per-session row: status glyph + session name + branch (from `SessionRecord.branch`)
- Buttons: one per session; callback `focus:<id>` → focus that session, re-render the list

Pagination: if the total list exceeds ~12 rows (or Telegram's keyboard limits), use `‹ Prev`
/ `Next ›` pagination (existing DOC-19 pattern).

**Accepting:** `/fleet` uses existing MCP `session_list` + new `fleet_by_project()` helper.
Rendering is adapter-specific (Telegram primitives); seam is `CommandExecutor.execute(ManagedFleet)`.

---

### 4.5 Cross-Repo Coordination (later phase, scope marker) {#SPEC-MULTIREPO-01-coord}

**MR-5:** Cross-repo multi-session coordination (e.g. a task spanning multiple projects,
broadcast messaging, dependency tracking) is a **north-star goal** (DOC-17, AUTONOMY_POLICY.md)
and is **explicitly deferred** to a later phase.

**Intent:** Establish the scope boundary and link to future work.

**What:** When a multi-project task is spawned (e.g. "deploy trusty-tools and run integration
tests against trusty-search"), the operator or agent should be able to:
- Create a correlated group of sessions across multiple projects
- Send a broadcast message to all sessions in the group
- Track dependencies (e.g. "Session B must complete before Session C starts")

**Phase timing:** This is **not** in Phase 1 (MR-1 through MR-4). It requires:
- Extended `SessionRecord.correlation` model (tracking multi-session groups)
- Broadcast messaging seam in `SessionManager`
- Dependency-resolution logic in `trusty-agents` (SM-8 harness loop)

**Accepting:** Future spec section or new DOC-NN spec. Link to DOC-17 (harness runner)
and AUTONOMY_POLICY.md for context.

---

### 4.6 Harness-Agnostic Routing Seam {#SPEC-MULTIREPO-01-harness}

**MR-6:** The routing layer SHALL remain independent of the session runtime (trusty-mpm,
trusty-agents, `tcode`, future runtimes) and **SHALL NOT** assume MPM-specific types
in the resolver or binding logic.

**Intent:** As `tcode` and other runtimes integrate with multi-project workflows, the
resolver and binding must remain portable.

**What:**
- The resolver and registry are **runtime-agnostic** — they operate on `(repo_url, branch,
  project_metadata)` tuples, not on `SessionRecord` or MPC-specific types.
- The binding logic is **one function** that takes a project list and a session-like
  struct (any struct with `repo_url` field) and returns the matching project.
- Adapters (Telegram, CLI, TUI, future Web/Slack, `tcode`'s control surface) ALL call the
  same resolver and binding functions, so the routing semantics are consistent across all
  runtimes.

**Seam gap (known):** The `SessionControl::LaunchParams` struct lives in `crates/trusty-mpm/src/core/sm/`
and is MPM-specific. To make routing portable, the resolver **output** is `(project_name, repo_url,
branch)` — which every runtime's launcher can consume independently. MPM's launcher adapts
this into a `LaunchParams` by threading `repo_url` and `branch` through `LaunchParams` (see MR-7 below).
Future runtimes can do similar adaptation at their boundary.

**Accepting:** Router module is placed in `crates/trusty-common/` or a new `crates/trusty-routing/`
(shared across all runtimes). MPM's `SessionManager` and `SessionControl` adapters call the router
at the boundary.

---

### 4.7 SessionControl LaunchParams Extension (seam unblock) {#SPEC-MULTIREPO-01-launchparams}

**MR-7:** `SessionControl::LaunchParams` (in `crates/trusty-mpm/src/core/sm/control.rs`) SHALL
be extended with `repo_url` and `ref` fields to support routing-driven session spawn.

**Intent:** The routing resolver outputs `(project_name, repo_url, branch)`; the session launcher
must thread these through to the provisioner so the workspace is cloned from the correct repo and ref.

**What:** Add to `LaunchParams`:
```rust
pub struct LaunchParams {
    // existing fields
    pub workdir: String,
    pub model: Option<String>,
    pub prompt: Option<String>,
    pub goal_id: Option<String>,
    pub ephemeral: Option<bool>,
    
    // NEW: routing-driven spawn
    pub repo_url: Option<String>,       // e.g. "https://github.com/trusty-inc/trusty-tools.git"
    pub ref_: Option<String>,           // e.g. "main", "feature/X", "v1.2.3"
}
```

Semantics: if `repo_url` and `ref_` are provided, the provisioner uses them to clone/checkout
the workspace. If absent, the provisioner falls back to current behavior (e.g. use `workdir`
as a local path or infer from session metadata).

**Accepting:** Update `LaunchParams` struct, update all callers to pass `None` for the new fields
(backward-compatible), and update the provisioner's spawn logic to honor them.

**Test:** Unit test verifies provisioner clones from the given repo/ref when both fields are `Some`.

---

## 5. Architecture and data model

### 5.1 Layered routing flow

Intent flows top-to-bottom; data flows back up:

```
┌─────────────────────────────────────────────────────────────┐
│  Adapter (Telegram / CLI / TUI / Web)                        │  thin: parse → resolve → dispatch
│    "/send <msg>" or "PROJ-123"        ──────►                │
│    Project picker / confirmation      ◄──────                │
└──────────┬─────────────────────────────▲─────────────────────┘
           │ NL query                     │ ProjectResolution
           │                              │ or focused session
           ▼                              │
┌─────────────────────────────────────────────────────────────┐
│  NL-to-Project Resolver + Binding                           │  one strategy per input type
│    resolve_project(query, projects, sessions)               │
│    resolve_session_project(session, projects)               │
│    fleet_by_project(sessions, projects)                     │
└──────────┬─────────────────────────────▲─────────────────────┘
           │ (project_name, repo_url, ref)    │ ProjectMatch / error
           │ or focused_session               │
           ▼                              │
┌─────────────────────────────────────────────────────────────┐
│  Project Registry + Session Index                           │  JSON persistence + boot auto-register
│    Project { name, repo_url, default_branch, … }           │
│    SessionManager.project_for_session(id, projects)        │
└─────────────────────────────────────────────────────────────┘
```

### 5.2 Data structures (pseudo-Rust)

```rust
/// A named project in the registry.
#[derive(Serialize, Deserialize, Clone)]
pub struct Project {
    pub name: String,
    pub repo_url: String,
    pub default_branch: String,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Resolver output.
pub struct ProjectResolution {
    pub matches: Vec<ProjectMatch>,
    pub primary: ProjectMatch,
}

pub struct ProjectMatch {
    pub project: Project,
    pub confidence: f32,         // 0.0 … 1.0
    pub reason: ResolutionReason,
}

pub enum ResolutionReason {
    ExactNameMatch,
    UrlMatch,
    TicketLookup,
    KeywordMatch,
    EmbeddingMatch,
}

/// Active sessions grouped by project.
pub struct ProjectFleet {
    pub project: Project,
    pub active_sessions: Vec<SessionRow>,
    pub last_activity: Option<DateTime<Utc>>,
}
```

### 5.3 Resolver algorithm (simplified)

```
fn resolve_project(query, projects, sessions):
  matches ← []
  
  // 1. Exact name match
  for project in projects:
    if project.name == query:
      matches.push(ProjectMatch { project, confidence: 1.0, reason: ExactNameMatch })
  
  if matches not empty:
    return ProjectResolution { matches, primary: matches[0] }
  
  // 2. URL match
  if query is URL-like:
    extracted_repo ← extract_repo_name(query)
    for project in projects:
      if project.repo_url contains extracted_repo:
        matches.push(ProjectMatch { project, confidence: 0.95, reason: UrlMatch })
  
  if matches not empty:
    return ProjectResolution { matches, primary: matches[0] }
  
  // 3. Keyword match
  query_words ← split(query)
  for project in projects:
    score ← 0.0
    for word in query_words:
      if word in project.tags:
        score += 0.3
      if word in project.description:
        score += 0.2
      if word in project.name:
        score += 0.4
    if score > threshold (0.3):
      matches.push(ProjectMatch { project, confidence: score, reason: KeywordMatch })
  
  // Sort by confidence descending
  matches.sort_by(|a, b| b.confidence.cmp(&a.confidence))
  
  if matches not empty:
    return ProjectResolution { matches, primary: matches[0] }
  
  // No match; return error with disambiguation or fallback
  return Err(ResolverError::NoMatch { suggestions: [] })
```

---

## 6. Phased rollout and integration with epic #1517

Epic **#1517** (multi-project awareness) drives the implementation. This spec maps to
work items WI-2 through WI-7 (estimated Phase 1 + Phase 1B):

| Phase | Work Item | Requirement | Status |
|-------|-----------|-------------|--------|
| 1A | WI-2 | MR-1: Project registry (JSON, auto-register, MCP tools) | **CRITICAL** |
| 1A | WI-3 | MR-2: NL-to-project resolver (name/URL/keyword matching) | **CRITICAL** |
| 1A | WI-4 | MR-7: Extend `LaunchParams` with `repo_url`, `ref_` | **CRITICAL** (seam unblock) |
| 1A | WI-5 | MR-3: Session↔project binding and `fleet_by_project()` | **CRITICAL** |
| 1B | WI-6 | MR-4: Telegram `/new` spawn flow (project picker → ref picker → task) | **HIGH** |
| 1B | WI-7 | MR-4: Telegram `/fleet` grouping + focused-session routing | **HIGH** |
| 2+ | — | MR-5: Cross-repo coordination (deferred, depends on DOC-17) | — |

**Critical path:** WI-2, WI-3, WI-4 must complete before WI-5; WI-5 must complete before
WI-6 and WI-7. All Phase 1A items are parallelizable once WI-2 (registry) is sketched.

**Dependency notes:**
- WI-4 (LaunchParams) unblocks the provisioner from using resolver output.
- WI-5 (binding) depends on both WI-2 (registry) and existing `SessionRecord.repo_url`.
- WI-6 and WI-7 depend on DOC-20 (chat-core) already being merged and stable.

---

## 7. Open questions and future work

1. **Ticket lookup resolution (MR-2, Phase 2):** How does the resolver look up a ticket ID
   (e.g. `PROJ-123`) and map it to a project? Do we integrate with GitHub Issues API, JIRA,
   or a custom ticketing backend? Deferred to Phase 2; Phase 1 resolves tickets as keyword
   matches only (if `PROJ` tag exists, fall back to keyword search).

2. **Embedding-based resolver (MR-2, Phase 2):** When keyword matching is insufficient,
   should the resolver use embedding-based semantic search on project descriptions? This
   requires an embedding model (e.g. from trusty-embedderd or trusty-search). Deferred to
   Phase 2; Phase 1 stops at keyword matching.

3. **Project metadata persistence:** Should project metadata (e.g. `last_used_at`) be updated
   every time a session for that project is created? This could cause churn in the JSON file.
   Decision: yes, but use a debounced write (write every 30 s or on daemon shutdown) to
   minimize disk I/O.

4. **Focused-session lifetime:** Should the focused session ID be:
   - (a) Persisted to the adapter's local store (e.g. per-chat SQLite) so it survives
     adapter restart?
   - (b) Ephemeral (reset on adapter restart)?
   
   Recommendation: (a) for Telegram (users expect state to persist across bot restarts);
   (b) for CLI (ephemeral sessions are expected). Let adapters decide.

5. **Provisional project cleanup:** Auto-registered "provisional" projects (from session
   history) that are never formally registered should be cleaned up periodically. Define:
   - Cleanup trigger (e.g. daemon startup or a periodic task)
   - Retention policy (e.g. keep if a session for that project is still active, or if
     it was used in the last 7 days)

6. **Seam gap verification:** Before Phase 1A concludes, verify that:
   - `LaunchParams` extension (MR-7) propagates through all callers without breaking
     existing code (backward-compatible default `None`).
   - Provisioner respects the new fields and clones from the given repo/ref.
   - Regression tests confirm that existing spawn paths (without repo_url/ref) still work.

7. **Relationship to DOC-17 (harness runner):** The harness runner (DOC-17) may need to
   spawn sessions across multiple projects for a single task. Should the routing layer
   provide a `spawn_correlated_sessions(projects: Vec<Project>, …) → Vec<SessionId>`
   API? Or should the harness orchestrate this itself? Deferred to cross-repo coordination
   phase (MR-5); clarify in DOC-17 or the phase planning.

---

## 8. References

- **DOC-14:** Session Manager (SM) Agent — `docs/specs/session-manager-agent.md`
- **DOC-16:** Interactive Sessions TUI — `docs/specs/sessions-tui-interactive.md`
- **DOC-17:** Autonomous Multi-Session Managed Harness Runner — `docs/specs/harness-runner-vision.md`
- **DOC-19:** TELUI: Telegram UI for trusty-mpm — `docs/specs/telui-telegram-ui.md`
- **DOC-20:** Chat-Core: Shared Command Nucleus — `docs/specs/chat-core.md`
- **Epic #1517:** Multi-project awareness and session routing
- **Issue #1433:** Chat-core seam and adapter integration
- **Issue #1405:** Session routing and cross-project context
- **Issue #832:** Multi-repo support
- **Issue #1272:** SM TUI and adapter feature parity
- **AUTONOMY_POLICY.md:** Harness autonomy and multi-session task correlation
- **Crates:**
  - `crates/trusty-mpm/src/session_manager/` — SessionManager, SessionRecord
  - `crates/trusty-mpm/src/core/sm/control.rs` — SessionControl, LaunchParams
  - `crates/trusty-mpm/src/client/` — Chat-core nucleus, CommandExecutor
  - `crates/trusty-mpm/src/telegram/` — Telegram adapter

---

**End of DOC-22**
