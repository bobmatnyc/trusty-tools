---
name: Bug Report
about: Report a bug or unexpected behavior
title: "[BUG] "
labels: bug
---

## Describe the Bug

A clear and concise description of what the bug is.

## Steps to Reproduce

**Exact reproduction command** (the literal shell/`gh`/`tm` invocation that
triggers the bug — required):

```
paste the exact command here
```

Narrative steps, if the command alone does not capture the setup:
1. ...
2. ...
3. ...

## Expected Behavior

What you expected to happen.

## Actual Behavior

What actually happened.

## Acceptance criteria

<!--
**Acceptance criteria.** Every bug, feature, task and epic issue states one to
four testable acceptance criteria, each a check that can fail for a named
reason; an epic states them on its phase issues. They set the good-enough bar:
the issue closes when each criterion has evidence. Anything beyond the bar
becomes a follow-up issue filed at close, with its own criteria and the
`stretch` label; it never widens this issue's scope. The closing note lists
every criterion with its evidence, plus the follow-up issue numbers or "none".
-->

1. ...

## Environment

- **OS:** (macOS / Linux / Windows)
- **Rust Version:** (output of `rustc --version`)
- **Crate(s) Affected:** (e.g., `trusty-search`, `trusty-memory`)
- **Version(s):** (crate version or git commit)

## Logs or Error Messages

If applicable, include relevant error output:

```
paste error logs here
```

## Additional Context

Any other context about the problem (e.g., system configuration, recent changes, workarounds).

---

**Labels:** Add `P0`, `P1`, or `P2` priority and the affected crate label (e.g., `trusty-search`).
