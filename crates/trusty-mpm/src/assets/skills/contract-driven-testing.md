---
name: contract-driven-testing
description: Derive a three-level test pyramid directly from a function's Code Contracts (preconditions, postconditions, invariants) — contract-targeted unit tests, property-based tests, precondition-violation tests, plus the no-contracts fallback and the contract review checklist. Loaded by the code-critic agent.
user-invocable: false
version: "1.0.0"
category: agent-reference
tags: [contracts, testing, property-based-testing, code-critic]
---

# Contract-Driven Testing

When a function under review carries Code Contracts (preconditions,
postconditions, invariants — see the project's `## Code Contracts`
convention), derive a three-level testing pyramid directly from them. **The
contracts ARE the test plan** — this is a mechanical translation, not a
creative exercise.

The examples below show the same pyramid in three ecosystems (Python +
`icontract`/`hypothesis`, Rust + `quickcheck`/`proptest`, TypeScript +
`fast-check`). Use whichever matches the project under review; the structure
generalizes to any language with an assertion library and a property-testing
framework.

## Level 1 — Contract-Targeted Unit Tests

- Read every precondition, postcondition, and invariant on the function under
  test.
- Write one focused test per postcondition. Derive the test name from the
  postcondition itself so the intent is legible without reading the body.
- Cover both the happy path and the boundary conditions for each
  postcondition.

**Python:**
```python
# Given postcondition: len(result) == min(k, len(items))
def test_top_k_result_length_equals_min_k_items():
    assert len(top_k([1.0, 2.0, 3.0], k=2)) == 2   # k < len
    assert len(top_k([1.0, 2.0, 3.0], k=10)) == 3  # k > len
```

**Rust:**
```rust
// Given postcondition: result.len() == k.min(items.len())
#[test]
fn top_k_result_length_equals_min_k_items() {
    assert_eq!(top_k(&[1.0, 2.0, 3.0], 2).len(), 2);   // k < len
    assert_eq!(top_k(&[1.0, 2.0, 3.0], 10).len(), 3);  // k > len
}
```

**TypeScript:**
```typescript
// Given postcondition: result.length === Math.min(k, items.length)
test("top_k result length equals min(k, items.length)", () => {
  expect(topK([1.0, 2.0, 3.0], 2)).toHaveLength(2);   // k < len
  expect(topK([1.0, 2.0, 3.0], 10)).toHaveLength(3);  // k > len
});
```

## Level 2 — Property-Based Tests from Contracts

- Translate each postcondition into a property assertion that holds for
  *every* generated input, not just the hand-picked examples in Level 1.
- Encode preconditions as generator constraints — constrain what the
  generator produces, don't filter/reject after the fact (rejection sampling
  wastes cases and can hide precondition bugs).
- Use the property-testing framework native to the stack: `hypothesis`
  (Python), `fast-check` (TypeScript/JavaScript), `quickcheck` or `proptest`
  (Rust), or the equivalent for the language under review.

**Python (hypothesis):**
```python
from hypothesis import given
import hypothesis.strategies as st

@given(
    items=st.lists(st.floats(allow_nan=False), min_size=1),  # encodes: len(items) > 0
    k=st.integers(min_value=1),                              # encodes: k > 0
)
def test_top_k_all_postconditions(items, k):
    result = top_k(items, k)
    assert len(result) <= len(items)                  # postcondition 1
    assert len(result) == min(k, len(items))          # postcondition 2
    assert all(x in items for x in result)            # postcondition 3
```

**Rust (proptest):**
```rust
proptest! {
    #[test]
    fn top_k_all_postconditions(
        items in prop::collection::vec(any::<f64>(), 1..50), // len(items) > 0
        k in 1usize..20,                                      // k > 0
    ) {
        let result = top_k(&items, k);
        prop_assert!(result.len() <= items.len());            // postcondition 1
        prop_assert_eq!(result.len(), k.min(items.len()));    // postcondition 2
        prop_assert!(result.iter().all(|x| items.contains(x))); // postcondition 3
    }
}
```

**TypeScript (fast-check):**
```typescript
import fc from "fast-check";

test("topK satisfies all postconditions", () => {
  fc.assert(
    fc.property(
      fc.array(fc.double(), { minLength: 1 }), // len(items) > 0
      fc.integer({ min: 1 }),                  // k > 0
      (items, k) => {
        const result = topK(items, k);
        expect(result.length).toBeLessThanOrEqual(items.length); // postcondition 1
        expect(result.length).toBe(Math.min(k, items.length));   // postcondition 2
        expect(result.every((x) => items.includes(x))).toBe(true); // postcondition 3
      },
    ),
  );
});
```

**Tip:** where the language's contract library supports it (e.g. Python's
`icontract-hypothesis`), auto-generate strategies directly from the contract
decorators instead of hand-writing the generator constraints.

## Level 3 — Precondition Violation Tests

- Write one negative test per distinct precondition.
- Verify the contract fails loudly with a useful message — not a silent
  wrong answer, not a generic panic with no context.
- Verify that valid inputs never trigger a spurious violation (a
  false-positive precondition check is its own bug).

Assert on the contract library's violation/failure type for the language
under review — `icontract.ViolationError` in Python, a `panic!`/`Result::Err`
from a `debug_assert!`/contract macro in Rust, a thrown error from the
contract wrapper in TypeScript. The structure generalizes across all three;
only the exception/assertion mechanism changes.

**Python:**
```python
def test_top_k_rejects_empty_items():
    with pytest.raises(icontract.ViolationError, match=r"len\(items\) > 0"):
        top_k([], k=1)

def test_top_k_rejects_nonpositive_k():
    with pytest.raises(icontract.ViolationError):
        top_k([1.0, 2.0], k=0)
```

**Rust:**
```rust
#[test]
#[should_panic(expected = "items must be non-empty")]
fn top_k_rejects_empty_items() {
    top_k(&[], 1);
}

#[test]
#[should_panic(expected = "k must be positive")]
fn top_k_rejects_nonpositive_k() {
    top_k(&[1.0, 2.0], 0);
}
```

## When There Are No Contracts (Yet)

If the function under review has no explicit contracts:

1. Flag it for the engineer if the function is complex or safety/security
   critical — contracts should have been written alongside the
   implementation, not after.
2. Infer implicit contracts from the function's docstring/doc comment and its
   observable behavior.
3. Write property-based tests against the inferred properties anyway — the
   absence of a formal contract doesn't excuse the absence of a test.

## Contract Review Checklist

When reviewing contracts written by the engineer:

- [ ] Postconditions on return-value functions reference the result;
      postconditions on mutating functions reference the mutated object.
- [ ] Relational postconditions are present (ordering, identity, structural
      relationship to inputs) — not just "a result exists."
- [ ] No side effects in contract expressions (contracts must be pure).
- [ ] Pre-call state is captured (`old()` or the language's equivalent) where
      a mutation postcondition needs to reference it.
- [ ] Every precondition has a corresponding Level 3 violation test.
