//! Shared trusty splash art + per-glyph shading (issue #3326).
//!
//! Why: the compact block-robot "TRUSTY" wordmark art was previously owned
//! solely by the `tm` binary (`trusty-mpm/src/bin/tm/formatters/banner/`),
//! so `trusty-agents`' REPL startup splash drifted onto its own unrelated
//! glyphs over time. Centralising the art text and its per-character color
//! mapping here gives every trusty-* binary one source of truth: `tm`'s
//! raw-ANSI launch banner and `trusty-agents`' ratatui REPL splash both read
//! from this module, so a future art update lands in both places at once
//! instead of being copy-pasted (and inevitably drifting again).
//! What: [`TRUSTY_SPLASH_ART`] (the embedded ASCII/block-art text) and
//! [`shade_bucket`] (glyph → amber/rust RGB triple). Deliberately
//! dependency-free — a `&str` constant plus a pure `match` — so depending on
//! it never pulls an extra crate into either binary's build. Each consumer
//! owns its own *rendering* (raw ANSI escapes for `tm`'s terminal output,
//! `ratatui::style::Color::Rgb` spans for `trusty-agents`' TUI) — only the
//! art data and the color-bucket rule are shared.
//! Test: `splash_art_is_nonempty`, `splash_art_contains_wordmark_glyphs`,
//! `shade_bucket_buckets_block_glyphs_amber`,
//! `shade_bucket_default_bucket_for_unclassified_glyph` below.

/// Embedded trusty splash art: three compact block-robot faces plus the
/// dot-matrix "TRUSTY" wordmark.
///
/// Why: single source of truth for the trusty brand splash, shared by `tm`'s
/// launch/connect banner and `trusty-agents`' REPL startup splash (#3326) so
/// the two binaries can never silently diverge again.
/// What: the raw ASCII/block-drawing text, byte-identical to what `tm`
/// previously shipped as its own embedded `image.txt` default.
/// Test: `splash_art_is_nonempty`, `splash_art_contains_wordmark_glyphs`.
pub const TRUSTY_SPLASH_ART: &str = include_str!("banner_art.txt");

/// Map a glyph to its rust-palette RGB triple.
///
/// Why: bot outline chars should appear bright amber so the compact robot
/// faces read clearly against a dark terminal background; accent glyphs use
/// mid-rust to add depth without competing with the main structure. The
/// splash renders everything — robot faces and the block-letter `TRUSTY`
/// wordmark — in a single amber/orange tone against black, so the wordmark
/// strokes join the same bright-amber bucket as the robot glyphs rather than
/// falling through to the darker "unclassified" default.
/// What: five buckets —
///   • amber `(224,140,60)`: block, half-block, and face glyphs used in the
///     current compact-splash art (`▄ ▀ █ ▓ ▌ ▐ ▟ ▙`), face glyphs
///     (`◉ ◔ ◕ • ◡ ▿ ⌣ ● ◻ ^ ⢀ ✲ ⡀`) from current and legacy `tm` art, plus
///     the dot-matrix wordmark pixel (`▪`), box-drawing head-outline chars
///     (`┌ ─ ┐ │ └ ┘ ┬ ┴ ├ ┤ ╷ ╵ ╶ ╴`), and `«…»` guillemets retained from a
///     superseded `tm` giant-robot generation so a not-yet-refreshed stale
///     seed still renders correctly;
///   • mid-rust `(205,100,30)`: medium-ink marks;
///   • dark rust `(120,50,10)`: fine punctuation marks;
///   • base rust `(183,65,14)`: everything else.
/// Test: `shade_bucket_buckets_block_glyphs_amber`,
/// `shade_bucket_default_bucket_for_unclassified_glyph`.
pub fn shade_bucket(c: char) -> (u8, u8, u8) {
    match c {
        // Amber — block structure: half-block, full-block, shade chars
        '▄' | '▀' | '█' | '▓' | '▌' | '▐' | '▟' | '▙'
        // Dot-matrix wordmark pixel (superseded `tm` giant-robot generation)
        | '▪'
        // Box-drawing chars (superseded `tm` rectangular robot heads + legacy kawaii bots)
        | '┌' | '─' | '┐' | '│' | '└' | '┘' | '┬' | '┴' | '├' | '┤' | '╷' | '╵' | '╶' | '╴'
        // Face glyphs — block robots (current + all legacy generations)
        | '◉' | '◔' | '◕' | '•' | '◡' | '▿' | '⌣'
        | '^' | '●' | '⢀' | '✲' | '⡀' | '◻'
        // Legacy dense glyphs from previous art
        | 'I' | '∏' | '♦' | '∇' | '√' | '≥' | '≤' | '@' | '#'
        // '«'/'»' and 'T','r','u','s','t','y' are needed only while a
        // pre-refresh legacy `tm` `«Trusty»` text wordmark, or a legacy
        // dot-matrix wordmark's guillemets, may still be on disk — kept so
        // that art renders correctly until it is transparently refreshed to
        // the current default.
        | 'T' | 'r' | 'u' | 's' | 't' | 'y' | '«' | '»' => {
            (224, 140, 60) // bright amber
        }
        // Medium — moderate ink
        'i' | 'l' | '!' | '<' | '>' | '+' | '=' | '/' | '\\' | '≈' | '∫' | '∑' => {
            (205, 100, 30) // mid rust
        }
        // Light — fine punctuation marks
        '.' | ',' | ':' | ';' | '°' | '⋆' | '◦' | '~' | '-' => {
            (120, 50, 10) // dark rust
        }
        // Unclassified — base rust
        _ => (183, 65, 14),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The embedded art must never be empty and must contain full-block
    /// glyphs (the robot-face outline structure).
    #[test]
    fn splash_art_is_nonempty() {
        assert!(!TRUSTY_SPLASH_ART.trim().is_empty());
        assert!(
            TRUSTY_SPLASH_ART.contains('█'),
            "splash art should contain full-block chars"
        );
    }

    /// The embedded art must contain the dot-matrix "TRUSTY" wordmark row.
    #[test]
    fn splash_art_contains_wordmark_glyphs() {
        assert!(
            TRUSTY_SPLASH_ART.contains('▀') && TRUSTY_SPLASH_ART.lines().count() >= 7,
            "splash art should retain the multi-row robot + wordmark layout"
        );
    }

    /// Block-structure and face glyphs bucket to bright amber.
    #[test]
    fn shade_bucket_buckets_block_glyphs_amber() {
        assert_eq!(shade_bucket('█'), (224, 140, 60));
        assert_eq!(shade_bucket('◉'), (224, 140, 60));
        assert_eq!(shade_bucket('▪'), (224, 140, 60));
    }

    /// A glyph outside every named bucket falls through to base rust.
    #[test]
    fn shade_bucket_default_bucket_for_unclassified_glyph() {
        assert_eq!(shade_bucket('Z'), (183, 65, 14));
    }
}
