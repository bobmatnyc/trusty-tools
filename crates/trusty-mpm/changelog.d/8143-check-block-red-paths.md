Documentation

- `code-review-standards` adds a Check Block Red-Path Coverage check: a Terraform `check` block on a scoped data source needs evidence for both the assertion-failure and read-failure paths, not the assertion path alone (Refs [#8143](https://github.com/bobmatnyc/trusty-tools/issues/8143))
