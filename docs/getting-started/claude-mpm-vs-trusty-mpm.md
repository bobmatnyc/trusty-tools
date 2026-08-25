# Coming from claude-mpm — Quick Comparison

> **Migrating to trusty-mpm?** See the full step-by-step guide at [/claude-mpm-migration](https://trustytools.dev/claude-mpm-migration), which covers daemon setup, tmux sessions, agent deployment, and kuzu-memory data migration.

## trusty-mpm is not a version of claude-mpm

They are unrelated codebases. trusty-mpm is not a fork, a port, or a rewrite of claude-mpm — there is no shared code, the languages differ (Rust and Python), the maintainers differ, and they ship through different channels (crates.io and Homebrew, versus PyPI). What they share is an idea: a project-manager session that delegates work to specialised agents.

The similar names cause real confusion. Read [`WHAT-IS-TRUSTY-MPM.md`](../../crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md) for the canonical identity document.

---

## Key Differences at a Glance

| Aspect | claude-mpm (Python) | trusty-mpm (Rust) |
|---|---|---|
| **Primary binary** | `claude-mpm` (Python entry point) | `tm` / `trusty-mpm` (single Rust binary) |
| **Installation** | `pip install claude-mpm` | `curl ... \| sh` → `tctl install trusty-mpm` |
| **Language/Runtime** | Python 3.x | Rust compiled binary |
| **Memory storage** | In-process or external API | Full MCP daemon (`trusty-memory`) |
| **Code search** | External REST API only | Full MCP daemon (`trusty-search`) |
| **Daemon lifecycle** | Manual start/stop | System services (launchd/systemd) |
| **Session management** | Ad-hoc tmux; manual cleanup | Main checkout by default, `--worktree` opt-in; auto-cleanup |
| **Session isolation** | Shared global state | Mechanically enforced write boundary + scoped memory |
| **Configuration** | Scattered YAML/ENV | Unified `~/.trusty-tools/config/` + project overrides |
| **Agent/Skill ecosystem** | 55+ agents, 260+ skills (same roster, different deployment) | Same ecosystem, bundled with the binary |

---

## The PM/Agent/Skill Model Stays the Same

Both systems use the same delegation framework. If you know claude-mpm's patterns — how the PM orchestrates, how agents specialize, how skills reuse instructions — you already know how to drive trusty-mpm. The platform is an upgrade, not a concept change.

---

## For Architecture & Design Details

- **[WHAT-IS-TRUSTY-MPM.md](../../crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md)** — Canonical identity doc.
- **[ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md](../../crates/trusty-mpm/docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md)** — Memory over MCP/JSON-RPC, Session↔Worktree 1:1 model, per-worktree search indexes.
- **[Root README](../../README.md)** — Overview of all 21 crates in the trusty-* ecosystem.

---

## Ready to Migrate?

Start here: [**Moving off claude-mpm to trusty-mpm: Full Step-by-Step Guide**](https://trustytools.dev/claude-mpm-migration)
