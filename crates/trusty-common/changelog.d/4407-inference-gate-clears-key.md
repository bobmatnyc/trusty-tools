Fixed

- `semantic_consolidation::inference_available_false_without_key` no longer
  fails on any machine that exports a real `OPENROUTER_API_KEY` (refs [#4407](https://github.com/bobmatnyc/trusty-tools/issues/4407), [#3451](https://github.com/bobmatnyc/trusty-tools/issues/3451)).
  The test asserts the inference gate stays closed with no key configured, but
  `inference_available("", false)` falls back to reading that variable from the
  process environment — so "absent from the ambient shell" was an unstated
  precondition, and its `#[serial]` group excluded concurrent test writers while
  doing nothing about the environment the suite inherits. It now CLEARS the
  variable for its body via an `EnvVarGuard::clear` (restored on drop), which is
  what it always claimed to test.
