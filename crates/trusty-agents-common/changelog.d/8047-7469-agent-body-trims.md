Changed

- java-engineer, nextjs-engineer, python-engineer and svelte-engineer bodies
  are trimmed back under the 42,000-byte composed-body resident budget by
  collapsing worked code examples into prose rules and dropping illustrative
  (non-rule) scaffolding, so their `OVER_BUDGET_BODY_BASELINES` ratchet
  entries in `trusty-mpm`'s bundle test are removed
  (refs [#8047](https://github.com/bobmatnyc/trusty-tools/issues/8047))
- rust-engineer's Quality Bar gains a dedicated rule placing the
  `check_test_pointers.sh` doc-comment pointer check right after a test
  rename/move, ahead of `cargo test`, instead of only at the closing gate
  (closes [#7469](https://github.com/bobmatnyc/trusty-tools/issues/7469))
