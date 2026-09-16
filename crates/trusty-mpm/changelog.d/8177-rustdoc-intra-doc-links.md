Documentation

- **`evaluate_main_checkout_write`'s doc links a function that exists.** The
  scratchpad exemption was cited as `root_is_scratchpad_rooted`, which is not a
  symbol in this crate; the predicate it actually calls is
  `write_lands_in_a_scratchpad_clone`. The stale name failed the pre-publish
  rustdoc-links gate.
