# clean

Every citation here resolves inside this fixture root: `docs/reference/clean.md`
(this page), `docs/reference/` (a directory), `docs/reference/clean.md:3` (a
line that exists) and `docs/reference/clean.md#clean` (an anchor the existence
check strips before resolving).

```
see `crates/does-not-exist/src/nowhere.rs` for details
```

The fenced block above is not scanned, and its broken citation must not be
reported — a fence quotes sample text, and its own delimiter line would
otherwise parse as an inline code span.
