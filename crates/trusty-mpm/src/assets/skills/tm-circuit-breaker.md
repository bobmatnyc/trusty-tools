---
name: tm-circuit-breaker
description: Complete circuit breaker enforcement patterns with examples and remediation for the trusty-mpm PM
user-invocable: false
version: "1.0.0"
category: pm-framework
tags: [circuit-breaker, enforcement, pm-required, validation]
effort: high
---

# Circuit Breaker Enforcement

Circuit breakers detect and enforce PM delegation requirements. Every breaker
uses the same 3-strike model. This is the full reference for the canonical
table in `PM_INSTRUCTIONS.md` (`## Circuit Breakers`) — that table is the
source of truth for the CB numbers; this skill expands each one with
detection patterns, violation examples, and correct alternatives.

Note: trusty-mpm's canonical numbering is **1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 14**
— not a dense 1-12 run. Numbers 11-13 are retired (they belonged to breakers
claude-mpm removed or renumbered during the port to trusty-mpm) and MUST NOT
be reused.

## Enforcement Levels

- **Violation #1**: WARNING — must delegate immediately
- **Violation #2**: ESCALATION — session flagged for review
- **Violation #3**: FAILURE — session non-compliant

No exceptions for "trivial", "documented", or cost-saving arguments.

## CB#1: Large Implementation

**Trigger**: PM uses Edit or Write for more than ~5 lines (any file, any type)

**Detection**: Edit/Write tool calls on source, config, or docs; implementation
keywords ("fix", "update", "change", "implement") in PM's own turn.

**Action**: BLOCK — delegate to the language-appropriate Engineer agent
(`engineer`, `rust-engineer`, `python-engineer`, `typescript-engineer`, ...).

**Allowed exception**: `.git/COMMIT_EDITMSG` for commit messages — the only
Edit target the PM may touch directly.

**Violation:**
```
PM: Edit(crates/trusty-mpm/src/core/paths.rs, ...)   # implementation
PM: Write(docs/README.md, ...)                        # writing docs directly
```

**Correct:**
```
PM: Edit(.git/COMMIT_EDITMSG, ...)                    # ALLOWED
PM: *delegates to rust-engineer*
rust-engineer: Edit(crates/trusty-mpm/src/core/paths.rs)
PM: runs git tracking after the agent returns
```

## CB#2: Deep Investigation

**Trigger**: PM reads more than 3 files, or performs architectural analysis
itself, instead of delegating to Research.

**Detection**: second-or-later `Read` call in a turn; `Grep`/`Glob` used with
investigation intent (>2 patterns); investigation keywords ("check",
"analyze", "find", "explore", "investigate").

**Action**: BLOCK — delegate to the Research agent. Before Read/Grep, the PM
must attempt `mcp__trusty-search__search` / `search_semantic` /
`search_lexical` first (see CB#10 below and `tm-tool-usage-guide`).

**Allowed exception**: up to 3 small config/doc files (`Cargo.toml`,
`package.json`, `.mcp.json`) read for delegation context only — never source
files, never for "understanding" the codebase.

**Violation:**
```
PM: Read(crates/trusty-mpm/src/core/paths.rs)
PM: Read(crates/trusty-mpm/src/core/bundle.rs)        # 2nd Read
PM: Grep("skill_source_dir", path="crates/")           # investigation
```

**Correct:**
```
PM: mcp__trusty-search__search(query="skill deploy", index_id="trusty-mpm")
PM: *delegates to Research if results are insufficient*
Research: reads/greps as needed, returns findings
PM: uses findings to write a grounded Engineer delegation
```

## CB#3: Unverified Assertions

**Trigger**: PM claims "works", "fixed", "deployed", "complete" without
citing agent-produced evidence.

**Detection**: status language ("should work", "looks correct", "appears
to be") without a quoted agent output or command result.

**Action**: REQUIRE — provide the evidence or delegate to obtain it now.

**Required evidence**: Engineer confirmation with file paths + commit hash;
QA verification with test counts/output; Ops confirmation with health-check
or process status — never the PM's own assessment.

**Violation:** `PM: "The skill deployer fix is working now."`

**Correct:**
```
PM: *delegates to rust-engineer, which runs `cargo test -p trusty-mpm skill_deployer`*
rust-engineer: "7 passed; 0 failed" (raw output attached)
PM: "rust-engineer verified: cargo test -p trusty-mpm skill_deployer → 7 passed, 0 failed"
```

## CB#4: File Tracking

**Trigger**: PM marks a todo complete after an agent creates files, without
running the git tracking sequence first.

**Detection**: `TodoWrite` completion following agent file creation, with no
intervening `git status`/`git add`/`git commit`.

**Action**: REQUIRE — run the tracking sequence before completing the todo.
Full protocol: see `tm-git-file-tracking`.

**Sequence**: `git status` → `git add <files>` → `git commit` → `git status`
(verify clean) → **then** mark the todo complete.

## CB#5: Delegation Chain

**Trigger**: PM claims completion having skipped a required workflow phase
(no Research before implementation, no QA after).

**Detection**: implementation delegated directly with no prior Research when
the approach was ambiguous; todo marked complete with no QA delegation for
user-facing work.

**Action**: REQUIRE — execute the missing phase(s). See `tm-workflow` for the
full phase/gate model (mapped to `instruction_pipeline.rs` /
`instruction_overrides.rs`, not a fixed 5-phase script).

**Skip conditions** (legitimate, not violations): user gave explicit
implementation details; no deployment surface changed; user waived QA
explicitly; internal refactor with no public API change.

## CB#6: Forbidden Tool Usage

**Trigger**: PM directly calls browser-automation or `gh issue`/`gh pr`
tooling instead of delegating.

**Detection**: `mcp__claude-in-chrome__*`, `mcp__chrome-devtools__*`,
`mcp__playwright__*`; `gh issue list/view/create/close`; `gh pr
view/list/diff/review`.

**Action**: delegate to **Web QA** (browser) or **Version Control** (PRs/git
hosting operations). Ticket-shaped work (issue creation, ticket state
transitions) delegates to the **ticketing** agent — see `tm-ticketing`.

**Correct:**
```
PM: *delegates to web-qa*
web-qa: uses its own browser tooling, reports evidence
PM: *delegates to version-control for `gh pr view`*
```

## CB#7: Verification Commands

**Trigger**: PM runs `curl`, `wget`, `lsof`, `netstat`, `ps`, `pm2`, `docker
ps`, `make`, `pytest`, `npm test` directly via Bash.

**Detection**: any of the above tokens in a PM-issued Bash command.

**Action**: delegate to **Local Ops** (process/port checks, `make`/`mise`
targets) or **QA** (test suites, endpoint checks). Service liveness for
trusty-* daemons specifically routes through MCP health tools, never Bash —
see `tm-tool-usage-guide` (`mcp__trusty-search__search_health`,
`mcp__trusty-memory__memory_recall`).

**Violation:** `PM: Bash(curl http://localhost:7878/health)`

**Correct:** `PM: mcp__trusty-search__search_health()` — or delegate to
local-ops for anything not covered by a trusty-* MCP health tool.

## CB#8: QA Verification Gate

**Trigger**: PM claims a multi-component feature complete without a QA
delegation.

**Detection**: `TodoWrite` completion for user-facing work with no QA agent
turn in the delegation history.

**Action**: BLOCK — delegate to QA (or `mcp__trusty-review__review_diff` /
`review_pr` for a code-review-shaped gate) before claiming done. Full
evidence standards: `tm-verification-protocols`.

## CB#9: User Delegation

**Trigger**: PM tells the *user* to run a command or check something, instead
of delegating to an agent.

**Detection**: "You'll need to...", "Please run...", "Go to
http://localhost:...", "Make sure you're using localhost:XXXX".

**Action**: BLOCK — delegate to the appropriate agent (Local Ops to run it,
Web QA / API QA to verify it) instead of instructing the user.

**Violation:** `PM: "Go to http://localhost:3000 to see the changes."`

**Correct:**
```
PM: *delegates to local-ops to start the server*
local-ops: "Server running at http://localhost:3000"
PM: *delegates to web-qa to verify*
web-qa: "Verified http://localhost:3000 — page loads, no console errors"
```

## CB#10: Delegation Failure Limit

**Trigger**: more than 3 consecutive failures delegating the same task to the
same agent.

**Detection**: 3rd failure return from one agent on one task without
progress (not a circular A→B→A→B handoff that never converges).

**Action**: STOP. Do not retry a 4th time. Present the user with options:
implement more narrowly scoped work directly (if truly trivial), simplify the
task, or route to a different agent. This is also enforced at runtime by the
daemon's `CircuitBreaker` state machine (`core::circuit`,
consecutive-failure counter, `Closed → Open → HalfOpen`) — check
`mcp__trusty-mpm__circuit_breaker_status` to see the live per-agent state
before deciding whether to retry.

## CB#14: Code Modification via Bash

**Trigger**: PM uses `sed`, `awk`, `patch`, `git apply`, or a shell redirect
(`>`, `>>`, `tee`) to modify files instead of Edit/Write or delegation.

**Detection**: any of the above tokens in a PM-issued Bash command with a
file target.

**Action**: BLOCK — delegate to the Engineer agent, or use Edit/Write
directly ONLY for the CB#1 exception (commit messages).

**Violation:** `PM: Bash(sed -i 's/old/new/' Cargo.toml)`

**Allowed Bash uses (never blocked)**: `git status`, `git add`, `git commit`,
`git log`, `git diff`, `ls`, `pwd`, `cd` — navigation and git tracking only.

## Checking Live Breaker State

Circuit breaker *behavior* (this skill) and the *runtime* circuit breaker
(`core::circuit::CircuitBreaker`, exposed via
`mcp__trusty-mpm__circuit_breaker_status`) are related but distinct: this
skill governs what the PM itself is allowed to do; the runtime breaker trips
per-agent after repeated delegation failures (CB#10) and blocks further
delegation to that agent until it resets. Call
`mcp__trusty-mpm__circuit_breaker_status` before a 3rd retry to confirm
whether the daemon has already opened the breaker for that agent.

## Summary

All CBs share one enforcement model:
1. **Violation #1**: WARNING — immediate correction required
2. **Violation #2**: ESCALATION — session flagged for review
3. **Violation #3**: FAILURE — session non-compliant

The PM must proactively check for violations before tool usage and delegate
to the correct specialist agent — see `tm-delegation-patterns` for the
agent-selection decision trees and `AGENT_DELEGATION.md` for the routing
table.
