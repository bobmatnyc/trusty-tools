# Architectural Drift Detection

**Applies to**: agent routing, skill injection, agent file creation  
**See also**: `docs/architecture/agent-skill-design.md`

---

## What is Drift

Drift is a change that violates the documented architecture without a deliberate decision to change the architecture. In open-mpm, drift most commonly takes one of three forms:

**Agent proliferation.** New `*-engineer.toml` or `*-engineer.md` files appear in `.open-mpm/agents/` without a justification entry in `agent-skill-design.md`. The canonical example: six language-specialist agent files were added during a routing bug fix (see below).

**Skill bypass.** An agent sets `skills = []` or omits the `skills` key when it should use `skills = ["auto"]`. This prevents `SkillRegistry` from injecting language/framework context and silently degrades output quality.

**Routing shortcuts.** The `AgentRegistry::best_match` scoring logic is modified to hard-code an agent name, or a new route is added in `src/main.rs` / `src/workflow/engine.rs` outside of the registry, bypassing the documented signal → score → inject pipeline.

---

## Drift Checklist

Run this before merging any PR that touches `.open-mpm/agents/`, `.open-mpm/skills/`, or `src/agents/`:

- [ ] No new `*-engineer.md` or `*-engineer.toml` files unless `docs/architecture/agent-skill-design.md` contains a written justification for that specialist.
- [ ] New language support is a skill file under `.open-mpm/skills/languages/`, not a new agent file.
- [ ] `engineer.toml` still has `skills = ["auto"]`.
- [ ] `cargo test` passes with 0 failures, including `registry_best_match_uses_engineer_for_non_python`.
- [ ] `./tests/harness/run_inspection.sh` passes 15/15.
- [ ] Routing dry-run for a non-Python task returns `engineer` (see commands below).

---

## Detection Commands

**Check for unexpected agent files.**

```bash
ls .open-mpm/agents/
```

Expected files as of 2026-04-26:

```
bedrock-engineer.toml
claude-code-engineer.toml
code-agent.toml
ctrl.toml
docs-agent.toml
engineer.toml
gpt-engineer.toml
gpt5-codex-engineer.toml
local-ops-agent.toml
observe-agent.toml
plan-agent.toml
pm.toml
postmortem-agent.toml
python-engineer.toml
qa-agent.toml
research-agent.toml
```

Any file matching `*-engineer.toml` that is not in this list is a drift candidate. Verify it has a justification entry in `agent-skill-design.md` before accepting the PR.

**Check routing for a Rust task.**

```bash
open-mpm inspect --task "Write a Rust module" --dry-run | jq .registry.best_match
# Expected: "engineer"
```

**Check routing for a Go task.**

```bash
open-mpm inspect --task "Implement a Go gRPC server" --dry-run | jq .registry.best_match
# Expected: "engineer"
```

**Check routing for a Python task.**

```bash
open-mpm inspect --task "Write a FastAPI endpoint" --dry-run | jq .registry.best_match
# Expected: "python-engineer"
```

**Run the full inspection suite.**

```bash
./tests/harness/run_inspection.sh
# Expected: 15/15 pass
```

**Check engineer still uses auto skill injection.**

```bash
grep 'skills' .open-mpm/agents/engineer.toml
# Expected line: skills = ["auto"]
```

**Spot any agent missing skills configuration.**

```bash
grep -rL 'skills' .open-mpm/agents/*.toml
```

Agents without a `skills` line may be intentionally skill-free (e.g., `ctrl.toml`) — verify case by case.

---

## How Drift Happened: The Canonical Example

A routing bug caused non-Python language tasks (Rust, Go, TypeScript) to be misrouted away from the `engineer` agent. The investigation correctly identified that routing was broken but incorrectly diagnosed the cause.

The fix applied: create `rust-engineer.toml`, `golang-engineer.toml`, etc. so that language-specific tasks would have a matching specialist to route to.

The correct fix: the `AgentRegistry::best_match` disqualifier logic was not correctly excluding language-mismatched specialists, or the `TaskSignals::extract` keyword detection was not firing. The fix belonged in `src/agents/registry.rs` or `src/inspection/task_signals.rs`.

Why the wrong fix was applied: adding agent files looks like "adding the missing piece." It is faster to write a TOML file than to trace a scoring bug through `best_match`. The result appeared to work — tasks routed correctly — but only because the new specialists happened to score higher, not because the underlying logic was fixed.

This pattern — fixing a routing problem by adding an agent instead of fixing the router — is the canonical form of well-intentioned drift in open-mpm.

---

## Fix Procedure

If drift is detected:

1. **Remove the agent file.**
   ```bash
   git rm .open-mpm/agents/<drifted-agent>.toml
   ```

2. **Check if a skill file should be added or updated instead.**  
   If the agent was added to handle a language, the right artifact is `.open-mpm/skills/languages/<language>-idiomatic.md`. See `agent-skill-design.md` for the correct pattern.

3. **Fix routing in source if needed.**  
   - `src/inspection/task_signals.rs` — verify `TaskSignals::extract` detects the language keyword.  
   - `src/agents/registry.rs` — verify the `best_match` disqualifier and scoring logic are correct.  
   - Run `cargo test` and confirm `registry_best_match_uses_engineer_for_non_python` passes.

4. **Update harness tests.**  
   If `run_inspection.sh` was failing before the drift was introduced, add a test case for the affected task type so future regressions are caught immediately.

5. **Update this document.**  
   Add the new drift instance to the examples section if it represents a novel pattern not already covered here.
