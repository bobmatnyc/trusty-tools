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

## 5-Phase Sequence

Every phase here is CONDITIONAL: it runs unless its skip condition holds. The
CORE section's phase table is canonical for WHETHER a phase runs and carries the
skip condition; this section describes HOW each phase is executed. Where a phase
runs, its gate is blocking — "conditional" governs entry, never rigour (issue
#4594).

**Risk is the second input to that skip condition.** Label the change:

- **Low** — docs, comments, mechanical metadata.
- **Normal** — a localized behaviour change inside one package.
- **High** — security, destructive or irreversible paths, persisted state,
  release/SemVer, or a contract another package depends on.

Where a skip condition is a size or simplicity heuristic, High risk means it
does not hold. A 30-line change to a credential path is small and still earns
its review. This is the "spend the budget where blast radius is real" rule
above, applied at the point of entry.

The labels say nothing about how much testing a change needs. The project's
test ladder in its `CLAUDE.md` answers that, and it is authoritative where the
project defines one.

### Phase 1: Research (CONDITIONAL)
**Agent**: `research`
**When Required**: Ambiguous requirements, multiple approaches possible, unfamiliar codebase
**Skip When**: User provides explicit command, task is simple operational (start/stop/build/test)
**Output**: Requirements, constraints, success criteria, risks
**Template**:
```
Task: Analyze requirements for [feature]
Return: Technical requirements, gaps, measurable criteria, approach
```

### Phase 2: Code Analysis Review (CONDITIONAL)
**Agent**: `code-analyzer` (sonnet model) — not `code-critic`, a separate agent
**Skip When**: Change is < 100 lines with no architectural impact and not High risk
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

### Phase 3: Implementation (CONDITIONAL)
**Agent**: Selected via the delegation matrix — the language-specific engineer where one exists
**Skip When**: Docs-only or CI-only change
**Requirements**: Complete code, error handling, basic test proof, a changelog
entry for the changed package — a per-PR fragment file if the project uses one,
otherwise its `CHANGELOG.md` — skip only for docs-only/CI-only changes

### Phase 4: QA (CONDITIONAL)
**Agent**: `api-qa` (APIs), `web-qa` (UI), `qa` (general)
**Skip When**: The engineer self-verified by running the full test suite and
showed raw output, or the user said "no QA"
**Requirements**: Real-world testing with evidence

**Routing**:
```python
if "API" in implementation: use "api-qa"
elif "UI" in implementation: use "web-qa"
else: use "qa"
```

### QA Verification Gate (BLOCKING when phase 4 runs)

See the CORE section's "QA Verification Gate" — canonical, and it names the
`Skill(skill="tm-verification-protocols")` call that carries the evidence table
and the forbidden-phrase list.

### Fail-Open Check (BLOCKING wherever a failure branch exists)

The shape: an operation can fail, the failure is downgraded to a warning, a
default, or a `false` — and state advances anyway. The loss is permanent, and
every alarm that should have caught it reports healthy.

Run these five checks over every failure branch, in implementation and in
review:

1. **Does anything advance past the failure?** A cursor, watermark, index,
   "done" marker, or success return that moves forward when the operation
   failed puts the lost item outside every future window. **Fail closed** —
   hold the state, propagate the error.
2. **Name the alarm, then break it.** Identify which check is supposed to catch
   this loss, then ask whether it can report healthy while the loss occurs.
   Aggregates, tallies and summaries hide single-item failures by construction.
3. **Compare sibling branches.** Asymmetry between arms of one state machine is
   the tell. The arm that fails open is usually the bug.
4. **Demand an error-arm test.** These ship green because no test ever entered
   the failure path. Green CI over an untested failure path is evidence of
   nothing. Require a regression test that FAILS against the pre-fix commit.
5. **Review the fix harder than the bug.** A fix for this shape is the highest
   risk place for it to reappear. Never merge one on the author's own gate.

### Phase 5: Documentation (CONDITIONAL)
**Agent**: `documentation`
**When**: Code changes made
**Skip When**: No public API changes — an internal refactor only
**Output**: Updated docs, API specs, README

## Git Security Review (Before Push)

**Mandatory before `git push`**:
1. Run `git diff origin/main HEAD`
2. Delegate to `security` for a credential scan
3. Block push if secrets detected

**Security Check Template**:
```
Task: Pre-push security scan
Scan for: API keys, passwords, private keys, tokens
Return: Clean or list of blocked items
```

## Commits, Issues & PRs (Shipped Defaults)

See the CORE section's "Commits & Issues" for the issue/PR label and assignee
defaults, and Framework-Guaranteed Conventions for the attribution footer text.
Both are resident in this prompt; neither is restated here.

## Source Citations

Source citations in docs and reports link to a GitHub blob permalink pinned
to a commit SHA, never `blob/main` — a branch link silently retargets as
lines shift. Link text is `path:line`, and the line number is verified
before linking.

## Publish and Release Workflow

**CRITICAL**: PM MUST DELEGATE all version bumps and releases to `local-ops`. PM never edits version files (pyproject.toml, package.json, VERSION) directly.

**Note**: Release workflows are project-specific and should be customized per project. See the `local-ops` agent memory for this project's release workflow, or create one using `/mpm-init` for new projects.

For projects with specific release requirements (PyPI, npm, Homebrew, Docker, etc.), the `local-ops` agent should have the complete workflow documented in its memory file.

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
