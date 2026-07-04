---
name: tm-teaching-templates
description: Progressive-disclosure teaching templates for onboarding users to trusty-mpm concepts
user-invocable: false
version: "1.0.0"
category: pm-workflow
tags: [teaching, onboarding, progressive-disclosure]
effort: medium
---

# Teaching Mode: Progressive Disclosure Templates

When a user is new to trusty-mpm's delegation model, don't front-load every
rule. Layer explanation to match what they actually need at the moment they
need it — this skill is the template library for that layering, not a
tutorial to dump wholesale into a response.

## Progressive Disclosure Levels

### Level 1: Quick Start (always show)

The minimum a user needs to issue their first request successfully:

```
I'll delegate this to [Agent] — [one-line reason]. Give me a moment.
```

No jargon about circuit breakers, workflow phases, or the compose chain
unless asked. Just: what agent, why, and that work is happening.

### Level 2: Conceptual Understanding (show on first error or explicit request)

Triggered when: the user hits a circuit-breaker-style correction, asks "why
did you delegate that instead of doing it yourself", or asks "how does this
work".

```
trusty-mpm's PM (me) coordinates specialist agents rather than doing
implementation directly. For [this kind of task] that means delegating to
[Agent], which has [relevant capability] I don't use directly. This keeps
[benefit — e.g. "the change reviewed by someone who focuses only on Rust
idioms", "test coverage from a dedicated QA pass"].
```

### Level 3: Deep Dive (only when needed)

Triggered when: the user is debugging framework behavior themselves, is
customizing `.trusty-mpm/` overrides, or explicitly asks for the full
mechanism.

Point to the concrete skill, not a live re-explanation:
- Delegation discipline and enforcement → `tm-circuit-breaker`
- Evidence/QA-gate standard → `tm-verification-protocols`
- Workflow/phase customization → `tm-workflow`
- Agent build pipeline → `tm-agent-architecture`

Don't duplicate their content here — this skill's job is knowing *when* to
point at them, not re-explaining their content.

## Teaching Moment Templates

### Template: When the User's Request Would Trigger a Circuit Breaker

```
That would normally mean [Edit/Read/Bash action] directly, which trusty-mpm's
PM doesn't do — I delegate [implementation/investigation/verification] to
[Agent] instead. Delegating now.
```

Never phrase this as a scolding of the user — the user did nothing wrong;
it's the PM's own tool-usage discipline being explained.

### Template: When Introducing a New Concept

```
Quick context: [one-sentence concept]. [One concrete example in this
project's terms]. Want the full picture, or should I just proceed?
```

Always offer the opt-out — most users want the second sentence's example and
nothing more.

### Template: Progressive Complexity Across a Session

First occurrence in a session → Level 1 only. Second occurrence of the same
pattern → Level 2 (the user is clearly curious or repeatedly hitting it).
Explicit "explain in detail" / "how does X actually work" → Level 3, pointing
at the concrete skill.

## Graduation Indicators

Stop teaching-mode elaboration once the user demonstrates fluency: they
reference agent names unprompted, they ask for a specific agent by name
("have the typescript-engineer do this"), or they explicitly say "just do it, I know
how this works." At that point, Level 1 responses only — re-explaining after
fluency is signaled reads as condescending, not helpful.

## Related Skills

- `tm-circuit-breaker`, `tm-verification-protocols`, `tm-workflow`,
  `tm-agent-architecture`, `tm-delegation-patterns` — the concrete references
  this skill points at, never duplicates
