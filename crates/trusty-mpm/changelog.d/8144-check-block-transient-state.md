Documentation

- `code-review-standards` adds a Check Block Transient-State Coverage check: a Terraform `check` block reviewed only against the final steady-state plan misses failures that appear only during resource replacement (Refs [#8144](https://github.com/bobmatnyc/trusty-tools/issues/8144))
