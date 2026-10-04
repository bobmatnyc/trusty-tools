# Class / Module Documentation

> **Part of**: [Documentation Style](../SKILL.md)
> **Category**: documentation
> **Reading Level**: Intermediate

## Purpose

A class, struct, trait, or module doc states the **contract the type
upholds** — invariants, thread-safety, lifecycle — not its field list. The
compiler (or the type system, in a typed language) already shows the field
list; documentation earns its cost by stating what the type system does
*not* enforce.

## Required Elements

- The Why/What/Test triple, plus a usage example.
- Invariants and thread-safety notes, stated explicitly wherever they are
  **not** obvious from the type signature — this is exactly where
  documentation value is highest and most often skipped.

A worked example (Python, adapted from the `api-documentation` skill):

```python
class UserManager:
    """
    Manages user accounts and authentication.

    Attributes:
        db: Database connection instance
        cache: Redis cache for session management

    Example:
        >>> manager = UserManager(db=get_db(), cache=get_cache())
        >>> user = manager.create_user("john@example.com", "password")

    Thread Safety:
        This class is thread-safe. Multiple threads can safely call
        methods concurrently.

    Note:
        All passwords are automatically hashed using bcrypt before
        storage. Never pass pre-hashed passwords to methods.
    """
```

The same shape applies to a Rust `struct`/`trait`: Why/What/Test in the
`///` block, an `# Examples` section, and an explicit note on any invariant
or thread-safety property the type system doesn't already guarantee.

## Spec Link-Back Rule

Where a project uses spec-linked documentation, place the `# Spec References`
block above the `struct`/`trait`/`impl`, immediately after Why/What/Test,
when the type's behavior is genuinely spec-governed. Not every type needs
this — a plain data-transfer struct with no governing spec should carry no
block. When in doubt whether a type *should* be spec-linked, that doubt is
itself worth surfacing (a comment, a follow-up issue) rather than resolved by
guessing a link.

## Agentic Considerations

An agent editing a method on this type needs the *invariant* stated at the
type level, so it doesn't have to re-derive it by reading every call site.
State what the compiler does not already enforce; do not restate what it
does — a `pub id: u64` field needs no doc comment explaining "this is a u64
holding the id."

## Anti-Patterns

- **Documenting field types.** Redundant with the struct definition the
  reader is already looking at.
- **Omitting invariants enforced only by convention, not the compiler.**
  This is the single most common and most costly omission — the type system
  can't catch a caller violating it, so the doc comment is the only
  enforcement mechanism available.
