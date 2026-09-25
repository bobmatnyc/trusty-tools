Changed
- **Breaking API change:** `RemoveOutcome` gains the public field `user_entry_remains` and is now `#[non_exhaustive]`, so code outside the crate can no longer build it with a struct literal. The next trusty-gworkspace release is 0.3.0 (#8539).
