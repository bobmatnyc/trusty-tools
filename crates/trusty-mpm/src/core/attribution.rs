//! The trusty-mpm attribution footer, and how it reaches a commit or PR body.
//!
//! Why: the footer used to be restated in instruction prose — the PM package,
//! BASE-AGENT, the skills, and this crate's `CLAUDE.md` all carried the literal
//! string, so every session paid for it on every turn and any one copy could
//! drift. Claude Code delivers the same text from its own `attribution` setting
//! (`attribution.commit` / `attribution.pr`, which deprecated
//! `includeCoAuthoredBy` in v2.0.62), so tm writes the setting instead and the
//! prose says nothing (#6807).
//! What: [`ATTRIBUTION_FOOTER`] is the single definition. The lib seeds it into
//! the tm-owned `settings.json` via
//! [`crate::core::standalone::settings_defaults::ensure_settings_defaults`], and
//! `tm pr open`'s body validator checks the same constant so a hand-written body
//! is held to the text the setting produces.
//! Test: `attribution_footer_is_the_expected_literal`,
//! `ensure_settings_defaults_seeds_attribution`.

/// The attribution footer that ends every trusty-mpm commit message and PR body.
///
/// Why/What/Test: see the module doc; asserted by
/// `attribution_footer_is_the_expected_literal`.
pub const ATTRIBUTION_FOOTER: &str =
    "🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools";

#[cfg(test)]
mod tests {
    use super::ATTRIBUTION_FOOTER;

    /// The footer is a verbatim contract shared by the settings seed and the
    /// `tm pr open` body validator — pin the literal so drift is a test failure
    /// rather than two silently different strings.
    #[test]
    fn attribution_footer_is_the_expected_literal() {
        assert_eq!(
            ATTRIBUTION_FOOTER,
            "🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools"
        );
    }
}
