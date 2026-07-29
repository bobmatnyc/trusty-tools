<!-- PURPOSE: 5-phase workflow execution details -->

# PM Workflow Configuration

## Sprint, then Harden (governs how hard every gate below is applied)

Work runs in two phases, not one blended one. Which phase you are in decides how
much verification ceremony the 5-phase sequence below actually gets.

> "We should sprint to a target (feature complete on a local version), then
> test/fix carefully. The slow feature release means we have too many things in
> flight."

1. **SPRINT** — drive to feature-complete on a local version. Testing/CI used
   judiciously: targeted tests while developing, no CI iteration loops,
   no critic round on narrow changes.
2. **HARDEN** — once feature-complete, test and fix carefully:
   full suite, critic, release gates. Publish only after that.

**The causal claim, which is the point of the doctrine:** slow feature release
*causes* too many things in flight — it is not a separate problem. Shortening
time-to-land is the fix; managing WIP count directly (caps, purges) treats the
symptom.

Derived rules:

- Spend the verification budget where blast radius is real — destructive paths,
  SemVer/release, security — and cut ceremony everywhere else.
- **The hard line that must never be crossed while going fast:
  never turn red green by deleting coverage.** No `#[ignore]`, no cfg-gating a
  failing test, no `--exclude`, no narrowing to `--lib`. Going fast is a licence
  to run fewer gates, never to make a failing gate report success.
- A branch that has drawn **3+ review rounds is evidence to close and fold**, not
  to attempt round 4. Worked example: #4202 → #4207.
- Branch = workstream, and it is durable. Worktree = writer, and it is ephemeral
  and short-lived. Keep worktrees short-lived; keep branches workstream-scoped.

## Mandatory 5-Phase Sequence

### Phase 1: Research (CONDITIONAL)
**Agent**: Research
**When Required**: Ambiguous requirements, multiple approaches possible, unfamiliar codebase
**Skip When**: User provides explicit command, task is simple operational (start/stop/build/test)
**Output**: Requirements, constraints, success criteria, risks
**Template**:
```
Task: Analyze requirements for [feature]
Return: Technical requirements, gaps, measurable criteria, approach
```

### Phase 2: Code Analysis Review (MANDATORY)
**Agent**: code-analyzer (sonnet model)
**Output**: APPROVED/NEEDS_IMPROVEMENT/BLOCKED
**Template**:
```
Task: Review proposed solution
Use: think/deepthink for analysis
Return: Approval status with specific recommendations
```

**Decision**:
- APPROVED → Implementation
- NEEDS_IMPROVEMENT → Back to Research
- BLOCKED → Escalate to user

### Phase 3: Implementation
**Agent**: Selected via delegation matrix
**Requirements**: Complete code, error handling, basic test proof, CHANGELOG.md
entry for the changed package (one bullet per user-visible change, under
`## [Unreleased]`) — skip only for docs-only/CI-only changes

### Phase 4: QA (MANDATORY)
**Agent**: API QA (APIs), Web QA (UI), qa (general)
**Requirements**: Real-world testing with evidence

**Routing**:
```python
if "API" in implementation: use "API QA"
elif "UI" in implementation: use "Web QA"
else: use qa
```

### QA Verification Gate (BLOCKING)

**No phase completion without verification evidence.**

| Phase | Verification Required | Evidence Format |
|-------|----------------------|-----------------|
| Research | Findings documented | File paths, line numbers, specific details |
| Code Analysis | Approval status | APPROVED/NEEDS_IMPROVEMENT/BLOCKED with rationale |
| Implementation | Tests pass | Test command output, pass/fail counts |
| Deployment | Service running | Health check response, process status, HTTP codes |
| QA | All criteria verified | Test results with specific evidence |

### Forbidden Phrases (All Phases)

These phrases indicate unverified claims and are NOT acceptable:
- "should work" / "should be fixed"
- "appears to be working" / "seems to work"
- "I believe it's working" / "I think it's fixed"
- "looks correct" / "looks good"
- "probably working" / "likely fixed"

### Required Evidence Format

```
Phase: [phase name]
Verification: [command/tool used]
Evidence: [actual output - not assumptions]
Status: PASSED | FAILED
```

### Example

```
Phase: Implementation
Verification: pytest tests/ -v
Evidence:
  ========================= test session starts =========================
  collected 45 items
  45 passed in 2.34s
Status: PASSED
```

### Phase 5: Documentation Agent
**Agent**: Documentation Agent
**When**: Code changes made
**Output**: Updated docs, API specs, README

## Git Security Review (Before Push)

**Mandatory before `git push`**:
1. Run `git diff origin/main HEAD`
2. Delegate to Security for credential scan
3. Block push if secrets detected

**Security Check Template**:
```
Task: Pre-push security scan
Scan for: API keys, passwords, private keys, tokens
Return: Clean or list of blocked items
```

## Commits, Issues & PRs (Shipped Defaults)

See `PM_INSTRUCTIONS.md` § "Commits & Issues" (canonical). In short, overriding
any harness default:

- Every commit message and PR body ends with the trusty-mpm attribution footer:
  `🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools`.
  Never emit `🤖 Generated with Claude Code` or a `Co-Authored-By: Claude …`
  trailer.
- Every `gh issue create` / `gh pr create` uses `--assignee @me --label
  trusty-mpm` (create the label if missing), so a trusty-mpm session can
  identify the issues/PRs it owns in a multi-harness repo.

## Publish and Release Workflow

**CRITICAL**: PM MUST DELEGATE all version bumps and releases to Local Ops. PM never edits version files (pyproject.toml, package.json, VERSION) directly.

**Note**: Release workflows are project-specific and should be customized per project. See the Local Ops agent memory for this project's release workflow, or create one using `/mpm-init` for new projects.

For projects with specific release requirements (PyPI, npm, Homebrew, Docker, etc.), the Local Ops agent should have the complete workflow documented in its memory file.

## Structural Delegation Format

```
Task: [Specific measurable action]
Agent: [Selected Agent]
Requirements:
  Objective: [Measurable outcome]
  Success Criteria: [Testable conditions]
  Testing: MANDATORY - Provide logs
  Constraints: [Performance, security, timeline]
  Verification: Evidence of criteria met
```

## Override Commands

User can explicitly state:
- "Skip workflow" - bypass sequence
- "Go directly to [phase]" - jump to phase
- "No QA needed" - skip QA (not recommended)
- "Emergency fix" - bypass research
