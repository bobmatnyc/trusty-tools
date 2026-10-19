# tm-workflow — phases and doctrine

## The 5-Phase Model and Its Dispatch Briefs

This table is canonical for **whether** a phase runs and carries each skip
condition — moved here from the instruction package by #7423, which keeps only
the phase names and the conditional rule. Where a phase runs, its gate is
blocking — "conditional" governs entry, never rigour (#4594); which findings
block is the Completion Standard's fix bar (`completion-standard.md`). A project may
replace the whole workflow section via a `WORKFLOW` marker in its `CLAUDE.md`
(see "Customizing the Workflow" in `SKILL.md`) if its delivery process differs.

| Phase | `subagent_type` | Gate | Skip When |
|-------|-------|------|-----------|
| 1. Research | `research` | Findings documented | User provides explicit instructions, simple task, language/approach known |
| 2. Code Analysis | `code-analyzer` | APPROVED / NEEDS_IMPROVEMENT / BLOCKED | Change is < 100 lines, no architectural impact, and not High risk |
| 3. Implementation | `engineer` (per lang detect) | Tests pass, files tracked, changelog entry added | Docs-only/CI-only change |
| 4. QA | `web-qa` / `api-qa` / `qa` | All criteria verified with evidence | Engineer self-verified (ran full test suite, raw output shown), user says "no QA" |
| 5. Documentation | `documentation` | Docs updated | No public API changes, internal refactor only |

**Phase 1 — Research** (`research`). Required for ambiguous requirements,
multiple possible approaches, or an unfamiliar codebase. Skipped when the user
gave an explicit command or the task is simple operational work.

```
Task: Analyze requirements for [feature]
Return: Technical requirements, gaps, measurable criteria, approach
```

**Phase 2 — Code Analysis** (`code-analyzer`, sonnet — NOT `code-critic`, which
is a separate agent).

```
Task: Review proposed solution
Use: think/deepthink for analysis
Return: Approval status with specific recommendations
```

Both `code-analyzer` and `code-critic` run the Fail-Open Check
(`code-review-standards`) against every failure branch touched, and name
which of its three outcomes each branch lands in: fail-open (defect),
fail-closed (correct), or reported degradation — the failure carried to the
caller in a tested response field (also correct, distinct from fail-open).

Decision: APPROVED → Implementation. NEEDS_IMPROVEMENT → back to Research.
BLOCKED → escalate to the user.

**Phase 3 — Implementation** (the language-specific engineer where one exists).
Requirements: complete code, error handling, test proof, and the changelog entry
described in `changelog.md`. Skip only for docs-only/CI-only changes.

**Phase 4 — QA.** Routing: `api-qa` for APIs, `web-qa` for UI, `qa` otherwise.
The gate itself is `tm-verification-protocols`.

**Phase 5 — Documentation** (`documentation`). Skipped for an internal refactor
with no public API change. A spec edit driven by an owner ruling carries a
dated cross-reference at the change site — `// <date>: <owner ruling
summary>, see #<DOC-or-issue>` — mirroring the `// #NNNN:` ticket-attribution
convention for code. A spec change from an owner ruling with no dated
cross-reference fails review.

### Running the Phases

Moved out of the instruction package by #7423, which keeps the phase names and
the conditional rule.

- Don't force 5 phases when 2 will do. After each phase: `git status` →
  `git add` → `git commit`.
- On failure: 1 re-delegate with more context → 2 escalate to Research → 3 block
  and require user input.

### Override Commands

| User says | Effect |
|---|---|
| "Skip workflow" | Bypass the phase sequence |
| "Go directly to [phase]" | Jump to that phase |
| "No QA needed" | Skip phase 4 (not recommended) |
| "Emergency fix" | Bypass Research |

Honour the override and name the bypassed gate in the completion report, so the
missing evidence is visible rather than implied.

### Verification Gates Are Not an Override Target

The verification-gate contract in `tm-verification-protocols` is a
project-independent invariant enforced by CB#8. A custom `WORKFLOW` override can
change *when* QA runs, never *whether* a completion claim requires evidence.

## Sprint, then Harden — the Rest of the Doctrine

The instruction package states the two phases, where to spend the verification
budget, and the hard line (never turn red green by deleting coverage). One
derived rule lives here because it applies only at a specific moment:

- **Branch = workstream, and it is durable. Worktree = writer, and it is
  ephemeral.** Keep worktrees short-lived; keep branches workstream-scoped.

Slow feature release *causes* too many things in flight. Shortening time-to-land
is the fix; capping WIP treats the symptom. The round limit that replaced "3+
review rounds is evidence to close and fold" is in the Completion Standard (`completion-standard.md`).
