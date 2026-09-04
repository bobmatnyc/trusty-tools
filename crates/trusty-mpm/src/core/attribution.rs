//! The trusty-mpm attribution footer, and how it reaches a commit or PR body.
//!
//! Why: the footer used to be restated in instruction prose — the PM package,
//! BASE-AGENT, the skills, and this crate's `CLAUDE.md` all carried the literal
//! string, so every session paid for it on every turn and any one copy could
//! drift. Claude Code delivers the same text from its own `attribution` setting
//! (`attribution.commit` / `attribution.pr`, which deprecated
//! `includeCoAuthoredBy` in v2.0.62), so tm writes the setting instead and the
//! prose says nothing (#6807).
//! What: [`ATTRIBUTION_FOOTER`] is the single definition, seeded into two
//! settings tiers because they reach different launches:
//! [`crate::core::standalone::settings_defaults::ensure_settings_defaults`]
//! writes the tm-owned `CLAUDE_CONFIG_DIR` copy, which only the `claude` child
//! tm spawns can see (`CLAUDE_CONFIG_DIR` is set per-command and never
//! exported), and
//! `core::session_launch::settings::write_output_style` (private to that
//! module, so not linkable) writes the
//! project-tier `.claude/settings.json`, which any `claude` launched in the
//! project reads. Both seed absent-only. `tm pr open`'s body validator checks
//! the same constant so a hand-written body matches what the setting produces.
//!
//! Per-key fallback is what makes seeding both keys safe. Claude Code's
//! settings reference, under `attribution`: "Once you set `commit` or `pr`,
//! Claude Code ignores the deprecated `includeCoAuthoredBy` setting and uses
//! its default text for whichever of the two you left unset." Setting both
//! leaves no key on a default. Verified against the docs at
//! <https://code.claude.com/docs/en/settings>; `attribution` deprecated
//! `includeCoAuthoredBy` in Claude Code v2.0.62, and 2.1.260 is the version
//! this was developed against.
//! Test: `attribution_footer_is_the_expected_literal`,
//! `ensure_settings_defaults_seeds_attribution`,
//! `write_output_style_seeds_attribution`.

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
