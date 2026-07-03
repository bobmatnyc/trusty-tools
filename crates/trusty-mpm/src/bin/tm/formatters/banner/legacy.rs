//! Historical embedded-default banner art, kept for stale-seed detection.
//!
//! Why: `source::load_banner_art` seeds `~/.trusty-mpm/banner.txt` on first
//! run so operators can discover and customise it. That seed permanently
//! shadows any later change to the embedded default — a user who installed
//! `tm` months ago never sees new shipped art because their on-disk file
//! wins over `DEFAULT_BANNER_ART` forever, even though they never edited it.
//! Recording every previous embedded default here lets `source` distinguish
//! "the user never touched this file" (content matches a known legacy
//! default byte-for-byte) from "the user customised this file" (anything
//! else), so only the former case can be safely refreshed.
//! What: three constants — `LEGACY_PRE_1907` (the block-robot art shipped
//! from #1829 through just before #1907, with no wordmark line), `LEGACY_1907`
//! (the same art plus the `«Trusty»` wordmark line, shipped by #1907), and
//! `LEGACY_GIANT_ROBOT_1933` (the giant-robot art with dot-matrix `«TRUSTY»`
//! wordmark shipped by #1933, superseded by the compact splash redesign in
//! this change). `KNOWN_LEGACY_DEFAULTS` is the list `source::refresh_if_legacy`
//! scans against.
//! Test: `legacy_pre_1907_is_nonempty`, `legacy_1907_is_nonempty`,
//! `legacy_giant_robot_1933_is_nonempty`, `legacy_defaults_are_distinct` below;
//! refresh behaviour is covered by `banner_source_refresh_on_legacy_match` in
//! `source::tests`.

/// Embedded default art shipped from #1829 (runtime-editable banner) through
/// just before #1907 (the `«Trusty»` wordmark addition) — no wordmark line.
pub(crate) const LEGACY_PRE_1907: &str = "\n ▄████▄     ▄████▄     ▄████▄\n█ ◉  ◉ █   █ ◔  ◕ █   █ •  • █\n█  ◡   █   █  ▿   █   █  ⌣   █\n█ ▄▄▄▄ █   █ ▓▓▓▓ █   █ ████ █\n ▀████▀     ▀████▀     ▀████▀\n";

/// Embedded default art shipped by #1907 — the pre-1907 block robots plus the
/// `«Trusty»` wordmark line, superseded by the giant-robot redesign in #1933.
pub(crate) const LEGACY_1907: &str = "\n ▄████▄     ▄████▄     ▄████▄\n█ ◉  ◉ █   █ ◔  ◕ █   █ •  • █\n█  ◡   █   █  ▿   █   █  ⌣   █\n█ ▄▄▄▄ █   █ ▓▓▓▓ █   █ ████ █\n ▀████▀     ▀████▀     ▀████▀\n\n           «Trusty»\n";

/// Embedded default art shipped by #1933 — the giant-robot redesign with
/// full-height bodies and a dot-matrix `«TRUSTY»` wordmark, superseded by the
/// compact splash redesign here.
pub(crate) const LEGACY_GIANT_ROBOT_1933: &str = "\n         ▄▄                    ▄▄                    ▄▄\n         ██                    ██                    ██\n       ▄████▄                ▄████▄                ▄████▄\n   ┌────────────┐        ┌────────────┐        ┌────────────┐\n   ▐│  ◉   ◉   │▌        ▐│  ◔   ◕   │▌        ▐│  •   •   │▌\n   ▐│          │▌        ▐│    ▿     │▌        ▐│          │▌\n   ▐│   ◡◡◡    │▌        ▐│   ▪▪▪    │▌        ▐│   ⌣⌣⌣    │▌\n   │▄▄▄▄▄▄▄▄▄▄▄▄│        │▓▓▓▓▓▓▓▓▓▓▓▓│        │████████████│\n   └──────┬─────┘        └──────┬─────┘        └──────┬─────┘\n       ██████                ██████                ██████\n      ▄▀▀▀▀▀▀▄              ▄▀▀▀▀▀▀▄              ▄▀▀▀▀▀▀▄\n     ██████████            ██████████            ██████████\n\n            « ▪▪▪▪▪ ▪▪▪▪  ▪   ▪  ▪▪▪▪ ▪▪▪▪▪ ▪   ▪ »\n           «    ▪   ▪   ▪ ▪   ▪ ▪       ▪   ▪   ▪  »\n          «     ▪   ▪▪▪▪  ▪   ▪  ▪▪▪    ▪    ▪ ▪    »\n           «    ▪   ▪  ▪  ▪   ▪     ▪   ▪     ▪    »\n            «   ▪   ▪   ▪  ▪▪▪  ▪▪▪▪    ▪     ▪   »\n";

/// Every previously embedded default, oldest first.
///
/// Why: a single slice lets `source::refresh_if_legacy` iterate without the
/// caller needing to know how many legacy generations exist.
/// What: `[LEGACY_PRE_1907, LEGACY_1907, LEGACY_GIANT_ROBOT_1933]`.
/// Test: `legacy_defaults_are_distinct`.
pub(crate) const KNOWN_LEGACY_DEFAULTS: &[&str] =
    &[LEGACY_PRE_1907, LEGACY_1907, LEGACY_GIANT_ROBOT_1933];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_pre_1907_is_nonempty() {
        assert!(!LEGACY_PRE_1907.trim().is_empty());
        assert!(LEGACY_PRE_1907.contains('█'));
        assert!(
            !LEGACY_PRE_1907.contains("Trusty"),
            "pre-1907 art must not contain the wordmark"
        );
    }

    #[test]
    fn legacy_1907_is_nonempty() {
        assert!(!LEGACY_1907.trim().is_empty());
        assert!(LEGACY_1907.contains("«Trusty»"));
    }

    #[test]
    fn legacy_giant_robot_1933_is_nonempty() {
        assert!(!LEGACY_GIANT_ROBOT_1933.trim().is_empty());
        assert!(LEGACY_GIANT_ROBOT_1933.contains('█'));
        assert!(
            LEGACY_GIANT_ROBOT_1933.contains('▪'),
            "giant-robot art must contain the dot-matrix wordmark pixel"
        );
    }

    #[test]
    fn legacy_defaults_are_distinct() {
        assert_ne!(LEGACY_PRE_1907.trim(), LEGACY_1907.trim());
        assert_ne!(LEGACY_1907.trim(), LEGACY_GIANT_ROBOT_1933.trim());
        assert_eq!(KNOWN_LEGACY_DEFAULTS.len(), 3);
    }
}
