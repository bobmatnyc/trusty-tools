Fixed

- the dream cycle no longer panics when it caps drawer text that contains multi-byte UTF-8 — CJK, Cyrillic, emoji, or accented Latin (refs [#5187](https://github.com/bobmatnyc/trusty-tools/issues/5187))
  - root cause: `merge_into` capped merged drawer content with `String::truncate(500)`. `truncate` asserts its argument is a char boundary, so whenever byte 500 of the merged string landed inside a multi-byte char it panicked with `assertion failed: self.is_char_boundary(new_len)`, killing a `tokio-rt-worker` inside the shipped `com.trusty.memory` daemon mid-consolidation
  - the same defect was present a second time in the semantic pass: the failure log for a canonical drawer sliced `&content[..content.len().min(80)]`, so an error-path log statement could itself panic the pass
  - both sites now route through one `char_safe_prefix` helper that rounds the cap DOWN to the nearest char boundary via `str::floor_char_boundary`. The cut is char-aligned, never grapheme-aligned — a combining mark can be separated from its base letter, which stays valid UTF-8 and cannot panic
