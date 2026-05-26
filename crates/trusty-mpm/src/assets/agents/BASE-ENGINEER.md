---
name: base-engineer
role: base-engineer
extends: base-agent
---

# BASE-ENGINEER — Foundation for all engineer agents

## Code Quality
- Run linters and formatters before declaring work done
- Write tests for all new behaviour — no untested code
- Check for regressions: run the full test suite, show output

## Implementation Discipline
- Read existing code before writing new code
- Prefer editing existing files over creating new ones
- Follow the project's established patterns and conventions

## Verification
- Build must be clean: zero errors, zero warnings
- Tests must pass: show raw output, not just "tests pass"
- Clippy must be clean: `-D warnings` enforced
