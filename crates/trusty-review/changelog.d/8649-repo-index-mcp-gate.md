Fixed

- A `default-features = false` build no longer carries the `review_pr` per-call index resolver as dead code, which failed `clippy -D warnings` for any consumer that does not enable `mcp` ([#8649](https://github.com/bobmatnyc/trusty-tools/issues/8649))
