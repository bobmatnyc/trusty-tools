//! Unit tests for the hand-written pattern matchers. Their Python twins are
//! checked end to end by the golden corpus (`tests/parity.rs`); these pin the
//! edge shapes each pattern's alternatives must cover.

use super::alerts::{civil_from_days, clean, days_from_civil, iso};
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

// The copies of the civil-date helpers that the consolidation removed from
// il/time.rs, poller/digest.rs and dashboard/derive.rs, kept as references.
fn old_days_time(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn old_days_derive(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn old_civil_time(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The single `days_from_civil` / `civil_from_days` pair agrees with every
/// removed copy over years -800..=2800 (leap and century years, negative
/// days) and round-trips. digest.rs's copy is `old_days_time` written with
/// the sign folded into the addend, so one reference covers both.
#[test]
fn civil_helpers_match_the_removed_copies() {
    for y in -800..=2800_i64 {
        for m in 1..=12 {
            for d in [1, 15, 28, 29, 30, 31] {
                let n = days_from_civil(y, m, d);
                assert_eq!(n, old_days_time(y, m, d), "{y}-{m}-{d}");
                assert_eq!(n, old_days_derive(y, m, d), "{y}-{m}-{d}");
            }
        }
    }
    for z in days_from_civil(-800, 1, 1)..=days_from_civil(2800, 12, 31) {
        let c = civil_from_days(z);
        assert_eq!(c, old_civil_time(z), "day {z}");
        assert_eq!(days_from_civil(c.0, c.1, c.2), z, "day {z}");
    }
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    assert_eq!(
        days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28),
        1
    );
}
