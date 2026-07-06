# claude-mpm vs trusty-mpm — Differences & Install

## The 30-Second Answer

**trusty-mpm** (`tm`) is the **Rust** successor to the Python `claude-mpm` project. Both are session orchestrators, but trusty-mpm is a rebuilt, production-grade system with:
- **Single binary:** `tm` (instead of scattered Python packages)
- **Managed daemons:** Runs trusty-memory and trusty-search as system services, not ad-hoc
- **Per-session git worktrees:** Each session gets its own isolated branch, automatically cleaned up on decommission
- **MCP-native storage:** Memory and search are full MCP servers, not REST wrappers

**Install trusty-mpm now:**
```bash
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
tctl install trusty-mpm
tm --help
```

Then run `tm` in any git repo to launch a session.

For the full install walkthrough, see [Install and Run trusty-mpm (tm) on Your Laptop](./install-and-run-tm.md).

---

## What's Familiar vs. Different

### Familiar: The PM/Agent/Skill Model

Both systems use the same **agent orchestration framework**:

| | claude-mpm | trusty-mpm |
|---|---|---|
| **PM (Project Manager)** | Orchestration, delegation, handoff | Same core logic, Rust + MCP native |
| **Agents** | Specialized task runners (e.g., rust-engineer) | Same ecosystem, now deployed as JSON + embedded |
| **Skills** | Reusable slash commands (e.g., `/verify`, `/code-review`) | Same, deployed alongside agents |
| **Output styles** | Markdown/text formatting templates | Same markup, now in `~/.claude/output-styles/` |

**What this means:** If you're familiar with claude-mpm's delegation patterns, agent selection, and output customization, trusty-mpm works the same way — it's a platform upgrade, not a concept change.

---

## Detailed Differences Table

| Aspect | claude-mpm (Python) | trusty-mpm (Rust) |
|---|---|---|
| **Language/runtime** | Python 3.x (pip install) | Rust compiled binary (`tm`) |
| **Installation** | `pip install claude-mpm` | One-liner: `curl ... \| sh` → `tctl install trusty-mpm` |
| **Distribution** | PyPI | GitHub Releases / Homebrew / `tctl` |
| **Primary binary** | `claude-mpm` (Python entry point) | `tm` / `trusty-mpm` (single binary) |
| **Memory storage** | In-process or external API | Full MCP daemon (`trusty-memory` on port 7070) |
| **Code search** | External REST API only | Full MCP daemon (`trusty-search` on port 7878) |
| **Session management** | Ad-hoc tmux windows; manual cleanup | Managed git worktrees; auto-decommission |
| **Session isolation** | Shared global state | Per-worktree branch + scoped memory palace |
| **MCP servers** | Ad-hoc wrappers | Native MCP stdio + HTTP transports |
| **Configuration** | Scattered YAML/ENV files | Unified `~/.trusty-tools/config/` + project overrides |
| **Daemon lifecycle** | Manual start/stop | System services (launchd/systemd) |
| **Health checks** | Manual queries | `tctl status`, `tm doctor` |

---

## The Three Big Structural Differences

### 1. Installation & Deployment

**claude-mpm (Python):**
```bash
pip install claude-mpm
# Plus: manually install claude-mpm-server if using memory/search APIs
# Plus: manage those server processes yourself
```

**trusty-mpm (Rust):**
```bash
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
tctl install trusty-mpm
# Automatically installs + starts trusty-memory and trusty-search daemons
```

**Impact:** trusty-mpm gives you a **complete platform in one install command**. No manual daemon wrangling, no separate `pip` packages, no PATH hunting.

---

### 2. Session Lifecycle & Git Worktree Model

**claude-mpm:**
- Creates tmux windows for each session.
- Session state is ephemeral; cleanup is manual (`tmux kill-session`).
- All sessions share the main checkout or rely on `[patch.crates-io]` trickery.

**trusty-mpm:**
- Creates a dedicated **git worktree** per session, branched off `main`.
- Session name is semantic (e.g., `.worktrees/tm-myproject-01/`) — stable across restarts.
- Worktree is **automatically decommissioned** when the session ends (branch + directory removed).
- Each session runs in an isolated branch; changes don't leak to main.

**Impact:** You can safely have 5 parallel sessions, each in a different branch, without branch-collision chaos or manual cleanup.

See [Architecture: Memory, Sessions, Search](../../crates/trusty-mpm/docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md) for the full design.

---

### 3. Memory & Search as First-Class Daemons

**claude-mpm:**
- Memory and search are optional, reached via hardcoded ports or ENV variables.
- If the daemon dies, the connection silently times out or errors.
- Port guessing is fragile (port 7070 may be taken → 7071 → …).

**trusty-mpm:**
- **Memory** (`trusty-memory`) and **Search** (`trusty-search`) are always-on core services.
- Ports are auto-discovered from daemon discovery files, never guessed.
- Failures are explicit and detectable.
- Both are full MCP servers (stdout/JSON-RPC, not ad-hoc REST).

**Impact:** More reliable session provisioning, better error messages, zero silent failures.

---

## What's the Same: Agent & Skill Ecosystem

Both systems run the same **agent catalog** and **skill library**. When you `tm run` a task or launch a session, you get:

- 55+ agents deployed (rust-engineer, python-engineer, research, QA, security, etc.)
- 260+ skills available (verify, code-review, simplify, loop, schedule, etc.)
- Output styles you can customize (how results are formatted)
- Delegation patterns you're used to (agent selection, PM orchestration)

**What differs:** Where they live and how they're deployed.

| | claude-mpm | trusty-mpm |
|---|---|---|
| **Agent location** | `~/.claude/agents/` (user config) | `~/.claude/agents/` (same) + bundled framework docs |
| **Agent deployment** | Manual YAML editing or PM updates | Provisioned by `tm load` / `tm install` |
| **Skill location** | `~/.claude/skills/` (user config) | `~/.claude/skills/` (same) |
| **Output styles** | `~/.claude/output-styles/` (user config) | Same + trusty-mpm system style |

---

## Migration Path: From claude-mpm to trusty-mpm

If you have an existing Claude Code project that was set up with claude-mpm or standalone:

1. **Install trusty-mpm:**
   ```bash
   curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
   tctl install trusty-mpm
   ```

2. **Navigate to your project:**
   ```bash
   cd ~/your-project
   ```

3. **Provision a trusty-mpm-managed workspace:**
   ```bash
   tm load
   ```

4. **Start a new session:**
   ```bash
   tm
   ```

Your project is now managed by trusty-mpm. All future sessions will run under the trusty-mpm framework with git worktrees, managed memory, and persistent search indexes.

---

## Architecture & Design

For a deep dive into trusty-mpm's architecture, read:

- **[WHAT-IS-TRUSTY-MPM.md](../../crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md)** — Official identity doc. Clarifies that this is **not** the Python `claude-mpm` project. Addresses common confusion.

- **[ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md](../../crates/trusty-mpm/docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md)** — Three-part deep dive:
  1. Memory over MCP/JSON-RPC (daemon discovery, no port guessing)
  2. Session↔Worktree 1:1 model (semantic naming, automatic cleanup)
  3. Per-worktree search indexes (isolated, scoped to projects)

- **[Root README](../../README.md)** — Overview of all 21 crates, including trusty-search, trusty-memory, trusty-analyze, and the agent orchestration platform.

---

## Frequently Asked Questions

### Q: Can I keep using claude-mpm if I prefer?

Yes. claude-mpm continues to work. trusty-mpm is an **upgrade path**, not a forced migration. That said, trusty-mpm's daemon management and session isolation make it easier to work with multiple projects in parallel.

### Q: Will trusty-mpm replace claude-mpm?

Over time, yes. trusty-mpm is production-grade with all the robustness of a Rust rewrite. We recommend migrating new projects to trusty-mpm and gradually retiring the Python version.

### Q: What if I have a claude-mpm project and want to keep it?

You can run both side-by-side. Each uses separate config directories (`~/.trusty-tools/config/` for trusty-mpm; claude-mpm's config elsewhere). Projects can opt in to trusty-mpm by running `tm load` in their directory, or stay with claude-mpm — your choice.

### Q: Is there a GUI for trusty-mpm?

Not yet, but `trusty-console` (in development) will provide a web dashboard. For now, use the CLI: `tctl`, `tm`, and `tm doctor`.

### Q: How do I get help with trusty-mpm?

1. Run `tm doctor` to diagnose health issues.
2. Check [Install and Run trusty-mpm](./install-and-run-tm.md) for common troubleshooting.
3. Read the architecture docs linked above.
4. File an issue: https://github.com/bobmatnyc/trusty-tools/issues

### Q: Does trusty-mpm work on Windows?

Currently, trusty-mpm targets macOS and Linux. Windows support via WSL2 is on the roadmap.

---

## Next Steps

- **Install trusty-mpm:** [Install and Run trusty-mpm on Your Laptop](./install-and-run-tm.md)
- **Deep dive:** Read [WHAT-IS-TRUSTY-MPM.md](../../crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md) and [ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md](../../crates/trusty-mpm/docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md)
- **Explore the full platform:** See the [root README](../../README.md) for trusty-search, trusty-memory, trusty-analyze, and agent orchestration
