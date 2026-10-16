# Block / Inline Comments

> **Part of**: [Documentation Style](../SKILL.md)
> **Category**: documentation
> **Reading Level**: Beginner

## Purpose

A block or inline comment explains **why** a specific non-obvious line or
block does what it does — it never restates the line in English. This is the
single highest-leverage rule in this entire skill for context-window
economy: a redundant comment is pure token tax with zero information gain,
for a human reviewer or an agent alike.

## Required Elements

A trailing or preceding comment, used only where the code is genuinely
tricky — per the Google C++/Go style guidance: comment coding tricks,
non-obvious ordering, or why an alternative approach wasn't chosen. Not on
every line, and not by default.

## Spec Link-Back Rule

Generally none. Block comments are too fine-grained for a spec anchor — DOC-38
has no block-granularity reference form. If a block genuinely implements a
specific spec clause, that context belongs in the *enclosing function's*
`# Spec References` block (see [method-function.md](method-function.md)), not
a block-level one.

## Agentic Considerations

Reserve inline comments for exactly the cases a diff reviewer — human or
agent — would otherwise stop and ask "wait, why?" A comment that doesn't
clear that bar is subtracting value: it adds tokens to every future read of
this file without adding information.

## Anti-Patterns

```python
# Bad: Obvious comment adds no value
i = i + 1  # Increment i

# Good: Comment explains WHY
i = i + 1  # Skip header row
```

(Verbatim from the `api-documentation` skill — reuse this example; it is the
canonical illustration of the rule.)

- **Comments that duplicate the very next line.** If the comment and the code
  say the same thing in two languages, delete the comment.
- **Comments that describe *how* instead of *why*.** A *how* comment goes
  stale the moment the implementation changes; a *why* comment survives
  refactors that change the how but not the reason.
