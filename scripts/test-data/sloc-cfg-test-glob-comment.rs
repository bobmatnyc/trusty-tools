//! Fixture: comment markers inside string literals inside a `#[cfg(test)]`
//! mod. The module is excluded; only the 3 production lines below count.
//!
//! Two distinct mechanisms, both of which broke region detection before the
//! matcher read literals:
//!   - `"https://…"` — pass 1 sees the `//` and truncates the line, eating a
//!     closing brace that was on it.
//!   - `"**/*.rs"` — pass 1 sees the `/*` and opens a block comment that never
//!     closes, blanking every line to EOF INCLUDING the `#[cfg(test)]`
//!     attribute, so the region is not merely mis-sized, it is invisible.
//!
//! The second is why region detection reads its own string-aware re-strip of
//! the raw lines rather than pass 1's output. Glob patterns make it ordinary:
//! `crates/trusty-search/src/{commands/integrate.rs, core/repo_config.rs,
//! service/grep.rs}` all hit it today.

pub fn parse() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_globs() {
        let u = "https://example.test/a"; assert!(!u.is_empty()); }

    #[test]
    fn glob() {
        let g = "**/*.rs";
        assert!(!g.is_empty());
        assert_eq!(parse(), 1);
    }
}
