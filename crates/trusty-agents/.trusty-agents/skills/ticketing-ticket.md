---
name: ticketing-ticket
description: Ticket templates and label conventions for bug/feature/chore work
tags: [ticketing, github, templates]
---

# Skill: ticketing-ticket

## Ticket Templates

### Bug Report

```markdown
**What happened:** <one sentence>

**Expected:** <what should happen>
**Actual:** <what does happen>

**Steps to reproduce:**
1.
2.
3.

**Environment:** Rust <version>, trusty-agents <version>

**Acceptance criteria:**
- [ ] Bug no longer reproducible
- [ ] Regression test added
```

### Feature Request

```markdown
## Summary
<one paragraph goal>

## Acceptance Criteria
- [ ] <testable condition>
- [ ] <testable condition>

## Size: S / M / L
```

### Chore / Maintenance

```markdown
## What and Why
<one paragraph>

## Done When
- [ ] <specific outcome>
```

### Research Spike

```markdown
## Question
<the question to answer>

## Why this matters
<one paragraph context>

## Deliverable
- [ ] Decision document at `docs/research/<topic>.md`
- [ ] Recommendation summarized in this issue
```

## Label Convention

| Label      | Use                                              |
|------------|--------------------------------------------------|
| `bug`      | Defect fix                                       |
| `feature`  | New capability                                   |
| `chore`    | Maintenance, refactor, cleanup                   |
| `research` | Investigation, spike, analysis                   |
| `infra`    | CI, config, build system                         |
| `poc`      | Proof-of-concept work (likely throwaway)         |
| `epic`     | Parent issue grouping child tickets              |

## Title Conventions

- Start with an action verb: **Add**, **Fix**, **Remove**, **Refactor**, **Document**, **Investigate**.
- Be specific: "Fix login 500 on empty password" beats "Fix login bug".
- Keep under ~70 characters when possible — long titles break GitHub UI layout.

## Sizing Rubric

- **S** — 1-2 hour tweak, ~50 LOC, no new tests needed.
- **M** — Half-day to one-day; touches 1-3 files; needs tests.
- **L** — Multi-day; new module / cross-cutting refactor; design discussion advisable. Consider splitting into an epic.

## Anti-patterns

- Vague titles ("Improve performance") — what specifically improves, by how much?
- Acceptance criteria that are not testable ("works well")
- Bug reports without reproduction steps
- Mixing two unrelated changes into one ticket — split them
