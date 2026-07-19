# Method / Function Documentation

> **Part of**: [Documentation Style](../SKILL.md)
> **Category**: documentation
> **Reading Level**: Intermediate

Methods and functions share one reference here because their documentation
contract is identical — the difference is only whether the callable is bound
to a type. Everything below applies equally to a free function and a method.

## Purpose

Document the **contract** — preconditions, postconditions, error
conditions — for a callable, not its algorithm. A caller needs to know what
to pass and what comes back; they don't need (and shouldn't have to read) how
the result is computed.

## Required Elements

**For non-obvious or API-heavy callables:**
- **Why/What/Test**: `Why` states the motivation, `What` states the mechanical
  behavior, `Test` names the actual test(s) proving it — as backtick-quoted
  names a mechanical checker can verify still exist (see "The `Test:` Pointer", below).
- A one-line summary in third-person-singular-present ("Returns…", "Computes
  …") — the rustdoc convention, portable to any docstring style.
- Full sentences, not fragments.
- An `# Examples` section with a runnable snippet wherever the call isn't
  self-evident from the signature.
- `# Errors` / `# Panics` sections (or your language's equivalent) whenever
  the function can fail or panic.

**For trivial callables (getters, obvious one-liners):**
- A single-line summary is sufficient. One-line `Test:` pointers may be omitted
  when an adjacent test module's names are self-evident.

## The Reconciled Four-Axis Example

Why/What/Test and a Spec-References link-back are not competing
conventions — they compose. `Why` and `What` stay exactly what your project's
convention defines; `Test` is a third, orthogonal axis pointing at executable
proof; `Spec References` is a fourth, *additive* axis — a separate block
within the same doc comment, present only when a spec genuinely governs the
item:

```rust
/// Why: <motivation>
/// What: <mechanical description>
/// Test: `test_name_a`, `test_name_b`
///
/// # Spec References
/// - [`SPEC-FOO-01~draft`](docs/specs/foo.md#SPEC-FOO-01~draft)
pub fn my_function() { … }
```

Add the `# Spec References` block only when a spec genuinely governs this
callable — never to satisfy a checklist.

## The `Test:` Pointer

Where the project mechanically checks `Test:` pointers against real test
names (this repo's `scripts/check_test_pointers.sh`), the pointer is a direct
jump target from a doc claim to its executable proof — closing the loop
between "this is documented to do X" and "here is where X is verified." A
`Test:` pointer that rots (renamed or deleted test, doc comment left stale)
is exactly the failure mode that check exists to catch — two real incidents
in this repo's history were caught only by human reviewers before the gate
existed.

## Spec Link-Back Rule

The `# Spec References` block in a doc comment is the primary use case for
spec-linked documentation in code: a function implementing a specific spec
clause declares it, immediately below Why/What/Test, in the file's native
comment idiom (Rust `///`, Python docstring, JSDoc, etc. — see your project's
SLD convention for the exact per-language grammar; this reference restates
none of it).

## Agentic Considerations

State preconditions and postconditions explicitly — this is contract-driven
documentation, and it feeds directly into contract-driven testing: an agent
deriving tests from a function's stated contract needs the contract written
down, not re-derived from the implementation. An agent modifying this
function later needs to know what it must preserve; the doc comment is where
that's recorded. A `Test:` pointer additionally gives that agent a
mechanically-verified jump target straight to the coverage, rather than a
manual search.

## Anti-Patterns

- **Documenting *how* instead of *what*.**
  ```python
  # Bad: documents HOW (implementation)
  def sort_users(users):
      """Uses bubble sort algorithm to sort users."""

  # Good: documents WHAT (contract)
  def sort_users(users):
      """Returns users sorted alphabetically by name."""
  ```
  The caller doesn't care about the algorithm; they care about the
  guarantee.
- **A rotting `Test:` pointer** — a doc comment naming a test that was
  renamed or deleted, silently decoupling the claim from its proof.
- **Restating parameter types the signature already shows.** If the
  signature is `fn discount(price: f64, pct: f64) -> f64`, the doc doesn't
  need "price is a float" — it needs the units, the valid range, and what
  happens at the boundary.
- **Defensive-reasoning paragraphs in the doc comment.** Inline issue
  history, design-decision backstory, and architectural rationale belong in
  linked ADRs or issues (`// See issue #1234 for the full context`), not
  in the doc comment itself. The doc comment should state the **what** and
  **why** for the current behavior; deep history stays one hop away.
