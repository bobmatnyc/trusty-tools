//! Unit tests for the hand-written pattern matchers. Their Python twins are
//! checked end to end by the golden corpus (`tests/parity.rs`); these pin the
//! edge shapes each pattern's alternatives must cover.

use super::alerts::{clean, iso};
use super::matchers::*;

#[test]
fn ansi_strip_matches_python_alternatives() {
    let csi = "\u{1b}[38;5;114mA\u{1b}[0m\u{1b}[?25lB";
    assert_eq!(strip_ansi(csi), "AB");
    let osc = "\u{1b}]8;id=x;https://e\u{1b}\\link\u{1b}]8;;\u{7}!";
    assert_eq!(strip_ansi(osc), "link!");
    assert_eq!(strip_ansi("\u{1b}(Bx\u{1b}]open"), "x\u{1b}]open");
    assert_eq!(
        strip_sgr("\u{1b}[2mdim\u{1b}[0m \u{1b}[1A"),
        "dim \u{1b}[1A"
    );
}

#[test]
fn box_top_rule_accepts_a_short_label() {
    let rule = "─".repeat(12);
    assert!(is_rule(&format!("  {rule}  ")));
    assert!(!is_rule(&"─".repeat(9)));
    assert!(is_box_top_rule(&format!("{rule} 1-01/2026 ─")));
    assert!(is_box_top_rule(&rule));
    assert!(!is_box_top_rule(&format!("{rule} {} ─", "x".repeat(31))));
    assert!(!is_box_top_rule(&format!("{rule} label")));
    assert!(!is_box_top_rule(&format!("{rule}x ─")));
}

#[test]
fn timers_sub_removes_each_shape() {
    assert_eq!(
        strip_timers("ran 3m 2s · ↓ 12.0k tokens at 7:36 AM"),
        "ran  ·  at "
    );
    assert_eq!(strip_timers("a12s 12s 12:3"), "a12s  12:3");
    assert!(is_option(" ❯ 1. Yes") && !is_option("❯ 1 Yes"));
    assert!(has_waiting("✻ Waiting for 2 background agents") && !has_waiting("Waiting for x"));
    assert!(is_footer_noise("  TM 1.7.4 ●") && is_footer_noise("   /rc  "));
    assert_eq!(find_pct("| ctx 47% |", "ctx ", true, false), Some(47));
    assert_eq!(find_pct("xctx 47%", "ctx ", true, false), None);
    assert_eq!(find_pct("⏳ 30% 📅40%", "📅", false, true), Some(40));
}

#[test]
fn clean_redacts_like_python() {
    assert_eq!(
        clean("export  API_KEY=abc123\nthen"),
        "export API_KEY=<redacted> then"
    );
    let hex = "a1".repeat(16);
    assert_eq!(clean(&format!("sha {hex} done")), "sha <redacted> done");
    assert_eq!(clean(&"x".repeat(300)).chars().count(), 200);
    assert_eq!(iso(0.0), "1970-01-01T00:00:00Z");
    assert_eq!(iso(1_759_000_000.9999996), "2025-09-27T19:06:41Z");
}
