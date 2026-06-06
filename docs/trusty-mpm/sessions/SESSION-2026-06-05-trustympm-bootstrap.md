# TrustyMPM Bootstrap Session — 2026-06-05

**Metadata:** Date 2026-06-05 · Crate `trusty-mpm` (brand "TrustyMPM") · Relates to epic #380, decision doc PR #768

## 1. Goal & Framing

TrustyMPM evolves the existing `trusty-mpm` crate into the complete Rust functional replacement of the Python "Claude MPM" framework. Architectural model: it **wraps stock `claude`** (injects PM identity/agents/skills/hooks/MCP into Claude Code sessions; never calls LLMs itself). Distinct from:
- **`trusty-code`/`tcode`** — the direct-LLM, per-project harness
- **`open-mpm`** — LLM-calling orchestration engine

Brand = "TrustyMPM"; crate stays `trusty-mpm`, CLI stays `tm`.

## 2. Decisions Locked (2026-06-05)

| Decision | Rationale |
|---|---|
| **Memory:** trusty-memory MCP-only | No static `.claude-mpm/memories` files, no kuzu. Per-agent memory-routing becomes trusty-memory domain tags. |
| **Dashboard:** TUI-only for now | Web dashboard deferred (#771). |
| **Agent/skill catalog:** reuse + lean frontmatter | Author content from `claude-mpm-agents` + `claude-mpm-skills` repos in 5-field trusty-mpm frontmatter (name/role/description/model/extends). Bundle core set for offline; full catalog via registry later (#387/#388). NOTE: `claude-mpm-skills` has ~167 skills — full parity is registry-scale, not hand-porting. |
| **Ticketing:** full parity planned | Tracked in #772. |
| **Conversion strategy:** hand-port curated batches | NOT a Python-schema converter — it wouldn't solve body-curation cost and would couple trusty-mpm to a heavy schema. For #388 registry, host the already-converted trusty-mpm-format catalog rather than fetch-and-convert at runtime. |
| **Per-agent model injection (limitation)** | Claude Code's PreToolUse hook is read-only and cannot mutate Agent tool input, so true per-delegation model selection isn't possible via hook; only session-level `--model` injection is implemented. Tracked in #784. |

## 3. Work Completed (13 PRs Merged, Main Green)

### Specification & Backlog
- Decision doc #768
- Tier-B issues #769–#776
- Follow-up #784
- Bug #801

### Phase 0: Fidelity (5 PRs)
- **#385** — Purge Python-era paths from assets
- **#389** — Unify frontmatter parser, no `:` truncation
- **#383** — `tm install` writes full assembled prompt, not 4-line stub
- **#394** — Load config.toml
- **#390** — Session-level `--model` injection
- **#395** — Split 4,638-line tm.rs into bin/tm/
- **#777, #778, #780, #782, #792** — Follow-up refactors

### Phase 1: Agents (#769) — 36 Concrete Agents Across 3 Increments
- **Increment 1 (#790):** qa, research, ops, security, documentation, data-engineer, version-control, ticketing, code-analyzer
- **Increment 2 (#794):** python/typescript/golang/rust/java/php/ruby engineers; react/nextjs/svelte engineers; web-qa, api-qa
- **Increment 3 (#800):** javascript/phoenix/dart/tauri/web-ui engineers; refactoring, prompt engineers, code-critic; gcp-ops/vercel-ops/local-ops; memory-manager (MCP-only adaptation); mpm-agent-manager, mpm-skills-manager (trimmed)

### Phase 1: Skills (#770) — 11 Guidance Skills
- **#806** — Guidance skills + skill-deployer format fix

### Bonus
- **#802** — trusty-memory `pid_alive` bug fix (closed #801)

### Artifact Growth
- `bundle::ALL` grew from 11 to **57** embedded artifacts

## 4. Critical Learnings / Gotchas

The next session **must know these**:

### 1. macOS Case-Insensitivity Hides `extends:` Casing Bugs
**Issue:** `extends: base-qa` vs file `BASE-QA.md` resolves on macOS (case-insensitive FS) but **fails on Linux CI**.

**Solution:** trusty-mpm's agent resolver now uses a case-folded `SourceMap` (lowercased-stem keyed map).

**Action:** Always validate on Linux CI, not just local macOS. Never rely on filesystem case behavior.

### 2. Skills MUST Deploy as Directories, Not Flat Files
**Issue:** Skills were deployed as flat `~/.claude/skills/<name>.md`, but Claude Code only discovers `~/.claude/skills/<name>/SKILL.md` directories.

**Result:** ALL skills (incl. example-skill, tm-doctor) were silently inert — a pre-existing defect.

**Solution:** `skill_deployer.rs` now writes `<name>/SKILL.md` correctly.

**Action:** Keep this when adding skills. Always test skill discovery in Claude Code after deployment.

### 3. `pid_alive` Sign-Overflow (trusty-memory)
**Issue:** PID range checking: `kill -0 <pid>` with out-of-range PID like `u32::MAX` stringifies to "4294967295", which `kill` parses as `-1`. `kill(-1, 0)` signals all processes and returns success → false "alive" → stale daemon lock never reclaimed.

**Solution:** Fixed with a range guard: `pid == 0 || pid > i32::MAX` → not alive.

**Impact:** Prevents false "daemon still running" locks on stale PIDs.

### 4. CI Gate is Authoritative
**Observation:** The CI gate caught bugs #1 (case-insensitivity) and #2 (skill directory structure) that passed silently on local macOS.

**Action:** Sequence PRs touching `tm.rs`/`bundle.rs` to avoid conflicts. Merge increments via CI-gated review; do not skip CI validation.

## 5. Roadmap Status & Next Steps

### Phase 1 Remaining
- **#387/#388** (3-level agent precedence + remote registry — **HIGH leverage**: needed to distribute the 36-agent/167-skill catalog to binary installs)
- **#776** (frontmatter model + memory-routing tags)
- Remaining niche agents to fully close #769
- Further skill increments (registry-scale)

### Phase 2
- **#393** (daemon-side circuit-breaker enforcement, CB#1–#14)
- **#391/#392** (prune stale deploys + atomic writes/corrupt-manifest recovery)
- **#384** (`PM_INSTRUCTIONS_VERSION` gating)

### Phase 3
- **#772** (full ticketing parity)
- **#773** (auto-config/stack detection)
- **#774** (postmortem)
- **#775** (teaching-mode + organize)

### Deferred
- **#771** web dashboard
- Static memory/kuzu (decided out)

## 6. Upstream Notes

Quality issues observed in source repos during porting (skill name↔slug mismatches, committed `.broken`/cache files, frontmatter inconsistencies) are being filed as bugs on `claude-mpm-agents` / `claude-mpm-skills` separately.

---

**Session Summary Author:** Claude Haiku 4.5  
**Next Session Hooks:** Check case-sensitivity on Linux before merging `tm.rs`/`bundle.rs` PRs; verify skill directory structure in `.claude/skills/`; test agent resolution against both macOS and Linux.
