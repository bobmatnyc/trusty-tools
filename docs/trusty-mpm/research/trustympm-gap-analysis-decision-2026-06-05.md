# TrustyMPM — Gap Analysis & Roadmap
**Claude MPM → trusty-mpm (Rust functional replacement)**

**Status:** Decision doc (living)  
**Date:** 2026-06-05  
**Owner:** PM  
**Relates to:** epic #380  
**Crate:** trusty-mpm (product brand: "TrustyMPM")  

---

## 1. Purpose & Identity

- **TrustyMPM** = the product brand; `trusty-mpm` stays the crate name and `tm`/`trusty-mpm` the CLI (no rename — renaming the crate/binary is disruptive churn for no functional gain). "TMPM" is informal shorthand only.
- **Architectural model** (confirmed): TrustyMPM WRAPS stock `claude` — it injects PM identity, agents, skills, hooks, and MCP config into Claude Code sessions and coordinates them; it **NEVER calls LLMs itself**.
- **Boundary vs sibling crates:**
  - `trusty-code`/`tcode` is the distinct direct-LLM, per-project harness
  - `open-mpm` is the LLM-calling multi-provider orchestration engine
  - **TrustyMPM is the Claude-Code meta-harness** (= Python claude-mpm's role)

---

## 2. Current State (summary)

**trusty-mpm v0.6.2**, ~44K LOC.

Built: single daemon-per-machine, session registry + auto-discovery, hook relay (`tm hook` → `POST /hooks`), in-session MCP server (6 orchestration tools), agent/skill deployment with `extends:` inheritance + checksum-guarded manifests, instruction assembly pipeline (compile-time concat + runtime merge + project overrides), TUI, Telegram, service discovery (`tm services`), bug-report pipeline.

Canonical PRD/ARCHITECTURE/COMPONENTS specs exist under `docs/trusty-mpm/spec/`.

**The gap is not "build the harness"** — it's "make it faithful + fill the content/ecosystem."

---

## 3. Tier A — Already-ticketed gaps (epic #380, 15 issues)

| Ticket | Theme | Problem | Impact |
|--------|-------|---------|--------|
| #383 | Fidelity-critical | `tm install` overwrites assembled prompt with 4-line stub | Session receives wrong/incomplete prompt |
| #385 | Fidelity-critical | Stale `~/.claude-mpm`/`config.yaml`/`src/claude_mpm/...` Python paths shipped verbatim to Claude | Python paths leak into Rust CLI |
| #389 | Fidelity-critical | Frontmatter parser truncates on `:`, two divergent copies | Agent metadata lost on parsing |
| #390 | Fidelity-critical | No `--model` injection → agent model preference silently ignored | Users cannot override agent model |
| #394 | Fidelity-critical | `config.toml` never read → per-agent models & registry sources inert | Configuration completely ignored |
| #391 | Robustness | No prune of stale deployed files | Disk bloat, orphaned agent cache |
| #392 | Robustness | Non-atomic deploy writes / corrupt-manifest mis-classifies managed files as user-owned | Unsafe upgrades, irreversible state |
| #384 | Robustness | `PM_INSTRUCTIONS_VERSION` marker inert — no staleness/upgrade detection | No upgrade path for prompt schema |
| #387 | Capability/parity | 3-level agent precedence (project>user>remote) — design done, implementation pending | Remote agent registry unusable |
| #388 | Capability/parity | Remote agent registry fetch/cache + offline fallback — design done, implementation pending | Cannot fetch/cache remote agents |
| #393 | Capability/parity | Daemon-side circuit-breaker CB#1–#14 enforcement | Session isolation incomplete |
| #395 | Hygiene | Split 4,442-line `src/bin/tm.rs`, exceeds 500-line cap | File size violation (code smell) |

**Status:** #381, #382, #386 are CLOSED.

---

## 4. Tier B — Claude MPM features ABSENT from trusty-mpm PRD entirely

| ID | Feature | trusty-mpm today | Gap severity |
|----|---------|------------------|--------------|
| B1 | Full agent library (~40 concrete agents: per-language engineers, research, qa/web-qa/api-qa, security, code-analyzer, documentation, ticketing, version-control, memory-manager, agent/skills-manager, refactoring, planner, ops variants) | Only `engineer.md` + 5 base templates bundled | **LARGEST content gap** |
| B2 | Skill/slash-command catalog (~25 `mpm-*` skills: init, doctor, config, status, monitor, organize, postmortem, pr-workflow, git-file-tracking, ticketing-integration, verification-protocols, delegation-patterns, teaching-mode, circuit-breaker-enforcement, session pause/resume, bug-reporting) | Only 2 bundled skills | **Major content gap** |
| B3 | Web monitor dashboard (Socket.IO real-time) | SSE feed + ratatui TUI only | **DEFERRED by decision** |
| B4 | Ticketing agent + workflow (mcp-ticketer, PROJ-123/#123/Linear/GitHub detection, ticket-driven dev, doc attachment) | GitHub bug-report filing only | In scope (full parity required) |
| B5 | Auto-configuration / stack detection (`/mpm-configure --preview`, agent recommendation) | `tm install`/`tm doctor` present | Partial (needs completion) |
| B6 | Postmortem / session analysis (`mpm-postmortem`) | Missing | In scope |
| B7 | Teaching mode (`mpm-teaching-mode`) + organize (`mpm-organize`) | Missing | In scope |
| B8 | Per-agent `memory_routing_rules` (Claude MPM agent JSON declares owned memory categories) | trusty-mpm uses markdown+frontmatter | To be modeled as trusty-memory domain tags |

---

## 5. Tier C — Deliberate divergences (decided)

| ID | Decision | Rationale | Status |
|----|----------|-----------|--------|
| C1 | Static `.claude-mpm/memories/{agent}_memories.md` files DROPPED | trusty-memory MCP is the sole backend (no offline/guaranteed-load fallback) | Locked; consequence: B8 memory-routing becomes trusty-memory domain tags |
| C2 | kuzu-memory dropped | Prefer trusty-memory MCP-only | Confirmed |
| C3 | SSE + TUI instead of Socket.IO web dashboard | TUI-first for initial release | Locked; B3 deferred to Phase 4+ |
| C4 | Markdown+frontmatter agents instead of JSON templates | Markdown is more maintainable + familiar | Locked; extend frontmatter for model defaults + memory-routing tags |

---

## 6. Decisions (locked 2026-06-05)

| Domain | Decision |
|--------|----------|
| Memory backend | trusty-memory MCP-only; no static files |
| Dashboard | TUI-only (defer web); Socket.IO deferred |
| Agent/skill library | Full parity sourced via registry (#387+#388) |
| Ticketing | Full parity (mcp-ticketer + Linear/GitHub support) |
| Agent format | Markdown+frontmatter (extend with model defaults + memory-routing tags) |

---

## 7. Roadmap (phased)

### Phase 0 — Fidelity & correctness
- Tickets: #385, #383, #389, #390, #394, #395
- Outcome: Prompt assembly faithful to input; no Python artifacts; model injection works; config reads properly; CLI respects 500-line cap
- Duration: 2–3 sprints

### Phase 1 — Content parity (the big one)
- Tickets: #387, #388
- Deliverables:
  - Stand up agent/skill source repository (or submodule/registry)
  - Port full ~40-agent + ~25-skill catalog from Python claude-mpm
  - Extend agent frontmatter for per-agent model defaults + memory-routing tags
- Duration: 4–6 sprints (heaviest phase; mostly content, not code)

### Phase 2 — Enforcement & robustness
- Tickets: #393, #391, #392, #384
- Outcome: Circuit-breaker enforcement at daemon; atomic deploys; stale file pruning; upgrade staleness detection
- Duration: 2–3 sprints

### Phase 3 — Ecosystem surfaces
- Deliverables:
  - Full ticketing parity (B4): mcp-ticketer, PROJ/Linear/GitHub detection, doc attachment
  - Auto-config/stack detection (B5): completion of `/mpm-configure --preview` + agent recommendation
  - Postmortem (B6): session analysis + report generation
  - Teaching mode (B7): guided troubleshooting for agents/skills
  - Organize (B7): workspace/session management
- Duration: 3–4 sprints
- Note: Most skill content rides along with Phase 1; postmortem/organize/auto-config need backend work

### Phase 4+ — Deferred (explicit)
- B3 Web dashboard (Socket.IO)
- Further scaling & optimization

---

## 8. Open Questions

1. **Where the agent/skill source lives:**
   - New `bobmatnyc/trusty-mpm-agents` repo?
   - In-workspace crate (e.g. `crates/trusty-mpm-agents`)?
   - Reuse existing `bobmatnyc/claude-mpm-agents` repo?

2. **Registry transport/format + TTL/offline-cache semantics:**
   - Input to #388 design
   - Affects agent deployment friction + offline-mode feasibility

3. **Per-agent memory-routing tags reconcile with MCP-only memory:**
   - How do agent frontmatter `memory_routing_rules` map to trusty-memory domain tags?
   - Storage: in agent manifest or trusty-memory?

4. **Issue tracking for Tier B items:**
   - Should each B1–B8 feature get its own tracking issue under epic #380?
   - Or consolidate into fewer, larger epics (e.g. "Content Parity" epic for B1+B2)?

---

## 9. Success criteria

- All Tier A fidelity gaps closed (Phase 0)
- Agent/skill library reaches ≥90% feature parity with Python claude-mpm (Phase 1)
- Daemon-side circuit-breaker enforcement active (Phase 2)
- Ticketing agent + workflow fully functional (Phase 3)
- Zero Python paths leaking to Claude Code sessions
- TUI operational + accessible for session monitoring
- Instruction assembly pipeline predictable & auditable
