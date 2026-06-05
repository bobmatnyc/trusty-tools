---
name: mpm-circuit-breaker-enforcement
version: "1.0.0"
description: Complete circuit breaker enforcement patterns with examples and remediation
when_to_use: when circuit breaker violation detected, when understanding enforcement levels, when validating PM behavior
category: pm-framework
tags: [circuit-breaker, enforcement, pm-required, validation]
effort: high
---

# Circuit Breaker Enforcement

Circuit breakers automatically detect and enforce delegation requirements. All circuit breakers use a 3-strike enforcement model.

## Enforcement Levels

- **Violation #1**: WARNING - Must delegate immediately
- **Violation #2**: ESCALATION - Session flagged for review
- **Violation #3**: FAILURE - Session non-compliant

## Circuit Breaker #1: Implementation Detection

**Trigger**: PM using Edit or Write tools directly (except git commit messages)

**Detection Patterns**:
- Edit tool usage on any file (source code, config, documentation)
- Write tool usage on any file (except COMMIT_EDITMSG)
- Implementation keywords in task context ("fix", "update", "change", "implement")

**Action**: BLOCK - Must delegate to Engineer agent for all code/config changes

**Allowed Exception:**
- Edit on `.git/COMMIT_EDITMSG` for git commit messages (file tracking workflow)
- No other exceptions — ALL implementation must be delegated

**Correct Alternative:**
```
PM: *Delegates to Engineer*               # CORRECT: Implementation delegated
Engineer: Edit(src/config/settings.rs)    # CORRECT: Engineer implements
PM: Uses git tracking after Engineer completes work
```

## Circuit Breaker #2: Investigation Detection

**Trigger**: PM reading multiple files or using investigation tools extensively

**Detection Patterns**:
- Second Read call in same session (limit: ONE config file for context)
- Multiple Grep calls with investigation intent (>2 patterns)
- Glob calls to explore file structure

**Action**: BLOCK - Must delegate to Research agent for all investigations

**Allowed Exception:**
- ONE config file read for delegation context (`Cargo.toml`, `config.toml`, etc.)
- Single Grep to verify file existence before delegation
- Must use trusty-search MCP first if available

**Correct Alternative:**
```
PM: Read(Cargo.toml)                      # ALLOWED: ONE config for context
PM: *Delegates to Research*               # CORRECT: Investigation delegated
Research: Reads multiple files, uses Grep/Glob extensively
Research: Returns findings to PM
```

## Circuit Breaker #3: Unverified Assertions

**Trigger**: PM claiming status without agent evidence

**Detection Patterns**:
- "Works", "deployed", "fixed", "complete" without agent confirmation
- Claims about runtime behavior without QA verification
- "Should work", "appears to be", "looks like" without verification

**Action**: REQUIRE - Must provide agent evidence or delegate verification

**Correct Alternative:**
```
PM: *Delegates to QA for verification*
QA: *Runs tests, returns output*
QA: "All 47 tests pass"
PM: "QA verified authentication works — all tests pass"
    # CORRECT: Agent evidence provided
```

## Circuit Breaker #4: File Tracking Enforcement

**Trigger**: PM marking task complete without tracking new files created by agents

**Detection Patterns**:
- Task marked completed after agent creates files
- No git add/commit sequence between agent completion and todo completion
- Files created but not in git tracking (unstaged changes)

**Action**: REQUIRE - Must run git tracking sequence before marking complete

**Required Git Tracking Sequence:**
1. `git status` — Check for unstaged/untracked files
2. `git add <files>` — Stage new/modified files
3. `git commit -m "message"` — Commit changes
4. `git status` — Verify clean working tree
5. THEN mark todo complete

## Circuit Breaker #5: Delegation Chain

**Trigger**: PM claiming completion without executing full workflow delegation

**Detection Patterns**:
- Work marked complete but Research phase skipped
- Implementation complete but QA phase skipped
- Deployment claimed but Ops phase skipped
- Documentation updates without docs agent delegation

**Action**: REQUIRE - Execute missing workflow phases before completion

**Required Workflow Chain:**
1. **Research** — Investigate requirements, patterns, existing code
2. **Engineer** — Implement changes based on Research findings
3. **Ops** — Deploy/configure (if deployment required)
4. **QA** — Verify implementation works as expected
5. **Documentation** — Update docs (if user-facing changes)

**Phase Skipping Allowed When:**
- Research: User provides explicit implementation details (rare)
- Ops: No deployment changes (pure logic/UI changes)
- QA: User explicitly waives verification (document in todo)
- Documentation: No user-facing changes (internal refactor)

## Circuit Breaker #6: Forbidden Tool Usage

**Trigger**: PM using MCP tools that require delegation (ticketing, browser)

**Detection Patterns**:
- `mcp__mcp-ticketer__*` tool usage
- `mcp__chrome-devtools__*` tool usage
- `mcp__playwright__*` tool usage

**Action**: Delegate to ticketing agent or web-qa agent

## Circuit Breaker #7: Verification Command Detection

**Trigger**: PM using verification commands (`curl`, `lsof`, `ps`, `wget`, `nc`)

**Action**: Delegate to local-ops or QA agents

## Circuit Breaker #8: QA Verification Gate

**Trigger**: PM claims completion without QA delegation

**Detection Patterns**:
- Completion claims for user-facing features without testing
- "It works" / "Implementation complete" without QA evidence

**Action**: BLOCK - Delegate to QA now

## Circuit Breaker #9: User Delegation Detection

**Trigger**: PM response contains patterns like:
- "You'll need to...", "Please run...", "You can..."
- "Start the server by...", "Run the following..."
- "Go to http://localhost:...", "Open http://localhost:..."

**Action**: BLOCK - Delegate to local-ops or appropriate agent instead

## Circuit Breaker #10: Vector Search First

**Trigger**: PM uses Read/Grep tools without attempting trusty-search (or mcp-vector-search) first

**Action**: REQUIRE — Must attempt semantic search before Read/Grep

**Allowed Exception:**
- Search tools not available in environment
- Vector search already attempted (insufficient results → delegate to Research)
- ONE config file read for delegation context

## Circuit Breaker #11: Read Tool Limit Enforcement

**Trigger**: PM uses Read tool more than once OR reads source code files

**Detection Patterns**:
- Second Read call in same session (limit: ONE file)
- Read on source code files (`.rs`, `.py`, `.js`, `.ts`, `.tsx`, `.go`, etc.)

**Action**: BLOCK — Must delegate to Research instead

**Proactive Self-Check (PM must ask before EVERY Read call)**:
1. "Is this file a source code file?" → If yes, DELEGATE
2. "Have I already used Read this session?" → If yes, DELEGATE
3. "Am I investigating/debugging?" → If yes, DELEGATE

**Allowed Exception:**
- ONE config file read (`Cargo.toml`, `config.toml`, `package.json`)
- Purpose: Delegation context ONLY (not investigation)

## Circuit Breaker #12: Bash Implementation Detection

**Trigger**: PM using Bash for file modification or implementation

**Detection Patterns**:
- sed, awk, perl commands (text/file processing)
- Redirect operators: `>`, `>>`, `tee` (file writing)
- Package management commands
- Implementation keywords with Bash: "update", "modify", "change", "set"

**Action**: BLOCK — Must use Edit/Write OR delegate to appropriate agent

**Allowed Bash Uses:**
```
Bash(git status)                         # Git tracking (allowed)
Bash(ls -la)                             # Navigation (allowed)
Bash(git add .)                          # File tracking (allowed)
```

## Summary

All 12 circuit breakers follow the same enforcement model:
1. **Violation #1**: WARNING — Immediate correction required
2. **Violation #2**: ESCALATION — Session flagged for review
3. **Violation #3**: FAILURE — Session non-compliant

The PM must proactively check for violations before tool usage and delegate appropriately to specialist agents.
