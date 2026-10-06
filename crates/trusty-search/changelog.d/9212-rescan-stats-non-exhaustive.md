Changed
- `RescanStats` is now `#[non_exhaustive]`, so a later counter is not a breaking change. Code outside the crate can no longer build it with a struct literal; start from `RescanStats::default()` (#9212).
