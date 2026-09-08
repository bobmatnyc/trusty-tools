Fixed

- Intra-doc links in `startup_budget` (`StartupOpenGate`, `release_after_sweep`, `DEFAULT_KEEP_RECENT_SECS`) now resolve under `cargo doc`. The module's own inner `//!` doc referenced its items bare, which resolves against the declaring module's scope rather than the module's own — fixed with `[`name`]: crate::path::to::name` link-reference definitions (#7157).
