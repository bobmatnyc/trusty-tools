//! Claude Code account rate-limit segment for `tm statusline` (#2140).
//!
//! Why: Pro/Max subscribers have no built-in visibility into how close they
//! are to Claude Code's rolling usage limits; the `statusLine` hook payload
//! already carries `rate_limits.five_hour.used_percentage` (session window)
//! and `rate_limits.seven_day.used_percentage` (weekly window) once the first
//! API response of the session has landed, so surfacing them costs nothing
//! extra to probe.
//! What: deserializes the optional `rate_limits` object, renders
//! `⏳<5h%> 📅<7d%>` with each half colored independently by its own
//! percentage, and omits whichever half (or the whole segment) is absent.
//! Test: `usage_segment_both_present`, `usage_segment_only_five_hour`,
//! `usage_segment_only_seven_day`, `usage_segment_none_when_absent`,
//! `usage_color_boundaries`.

use serde::Deserialize;

/// Claude Code account rate-limit windows from the `statusLine` hook payload.
///
/// Why: Pro/Max accounts report two independent rolling windows; either may be
/// absent (e.g. before the first API response in a session), so both fields
/// are optional and default to `None` rather than a misleading `0%`.
/// What: mirrors the hook's `rate_limits.five_hour` / `rate_limits.seven_day`
/// keys; unknown sibling fields are ignored (no `deny_unknown_fields`).
/// Test: `usage_segment_both_present`, `usage_segment_none_when_absent`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateLimits {
    #[serde(default)]
    pub(crate) five_hour: Option<RateWindow>,
    #[serde(default)]
    pub(crate) seven_day: Option<RateWindow>,
}

/// A single rate-limit window's usage percentage.
///
/// Why: `used_percentage` is `Option<f64>` (not a bare `f64` defaulting to
/// `0.0`) so a window object present without the field still degrades to
/// "absent" rather than rendering a false `0%`.
/// What: the only field consumed today; other hook keys on the window object
/// (e.g. a reset timestamp) are ignored.
/// Test: `usage_segment_both_present`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RateWindow {
    #[serde(default)]
    pub(crate) used_percentage: Option<f64>,
}

/// Color tier for a single rate-limit window's usage percentage.
///
/// Why (#2140): each window is colored independently of the other so a
/// near-exhausted 5-hour session window is flagged even while the 7-day
/// weekly window is still comfortably low, and vice versa.
/// What: a three-tier enum consumed by [`colorize_usage`]; kept separate from
/// the ANSI-emitting code so the threshold decisions are independently
/// unit-testable.
/// Test: `usage_color_boundaries`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageColor {
    Normal,
    Amber,
    Red,
}

/// Decide the color tier for a rate-limit usage percentage (0-100 scale).
///
/// Why (#2140): centralising the amber/red thresholds as a pure function
/// keeps them independently unit-testable and gives future callers one place
/// to retune the boundaries.
/// What: clamps `pct` to `[0.0, 100.0]` (stale/malformed hook data degrades to
/// the nearest valid tier rather than panicking) and returns
/// [`UsageColor::Red`] at `>= 80.0`, [`UsageColor::Amber`] at `>= 50.0`, else
/// [`UsageColor::Normal`].
/// Test: `usage_color_boundaries`, `usage_color_clamps_out_of_range`.
pub(crate) fn usage_color(pct: f64) -> UsageColor {
    let pct = pct.clamp(0.0, 100.0);
    if pct >= 80.0 {
        UsageColor::Red
    } else if pct >= 50.0 {
        UsageColor::Amber
    } else {
        UsageColor::Normal
    }
}

/// Wrap `text` in an amber or red ANSI foreground escape per [`usage_color`];
/// return it unchanged when `Normal`.
///
/// Why (#2140): matches the hand-rolled-ANSI approach used for the ctx%
/// segment (`compaction.rs`, #2098) rather than the `colored` crate — `tm
/// statusline`'s stdout is always a pipe consumed by Claude Code's status-bar
/// renderer, never a real terminal, so `colored`'s TTY/`NO_COLOR`
/// autodetection would be actively wrong here (see #1858).
/// What: wraps `text` as `"\x1b[33m{text}\x1b[0m"` (amber) or
/// `"\x1b[31m{text}\x1b[0m"` (red) per [`usage_color`]; `text` unchanged when
/// `Normal`.
/// Test: `colorize_usage_amber`, `colorize_usage_red`, `colorize_usage_normal`.
pub(crate) fn colorize_usage(text: &str, pct: f64) -> String {
    match usage_color(pct) {
        UsageColor::Red => format!("\u{1b}[31m{text}\u{1b}[0m"),
        UsageColor::Amber => format!("\u{1b}[33m{text}\u{1b}[0m"),
        UsageColor::Normal => text.to_string(),
    }
}

/// Build the `⏳<5h%> 📅<7d%>` account-usage segment.
///
/// Why (#2140): the segment must degrade gracefully — before the first API
/// response of a session, or on a non-Pro/Max account, `rate_limits` (or
/// individual windows within it) are simply absent from the hook payload;
/// showing a false `0%` would be misleading, so each half (and the segment as
/// a whole) is omitted rather than defaulted.
/// What: renders `⏳<pct>%` from `five_hour.used_percentage` and `📅<pct>%`
/// from `seven_day.used_percentage` when present (rounded to the nearest
/// integer, colored independently via [`colorize_usage`]), joins present
/// halves with a space, and returns `None` when both are absent.
/// Test: `usage_segment_both_present`, `usage_segment_only_five_hour`,
/// `usage_segment_only_seven_day`, `usage_segment_none_when_absent`.
pub(crate) fn usage_segment(rate_limits: Option<&RateLimits>) -> Option<String> {
    let rate_limits = rate_limits?;

    let five_hour = rate_limits
        .five_hour
        .as_ref()
        .and_then(|w| w.used_percentage)
        .map(|pct| colorize_usage(&format!("\u{23f3}{}%", pct.round() as i64), pct));
    let seven_day = rate_limits
        .seven_day
        .as_ref()
        .and_then(|w| w.used_percentage)
        .map(|pct| colorize_usage(&format!("\u{1f4c5}{}%", pct.round() as i64), pct));

    let parts: Vec<String> = [five_hour, seven_day].into_iter().flatten().collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(pct: f64) -> RateWindow {
        RateWindow {
            used_percentage: Some(pct),
        }
    }

    #[test]
    fn usage_segment_both_present() {
        let rl = RateLimits {
            five_hour: Some(window(24.0)),
            seven_day: Some(window(41.0)),
        };
        let seg = usage_segment(Some(&rl)).expect("segment");
        assert_eq!(seg, "\u{23f3}24% \u{1f4c5}41%");
    }

    #[test]
    fn usage_segment_only_five_hour() {
        let rl = RateLimits {
            five_hour: Some(window(24.0)),
            seven_day: None,
        };
        let seg = usage_segment(Some(&rl)).expect("segment");
        assert_eq!(seg, "\u{23f3}24%");
    }

    #[test]
    fn usage_segment_only_seven_day() {
        let rl = RateLimits {
            five_hour: None,
            seven_day: Some(window(41.0)),
        };
        let seg = usage_segment(Some(&rl)).expect("segment");
        assert_eq!(seg, "\u{1f4c5}41%");
    }

    #[test]
    fn usage_segment_none_when_absent() {
        // No rate_limits object at all.
        assert_eq!(usage_segment(None), None);

        // rate_limits present but both windows absent.
        let rl = RateLimits {
            five_hour: None,
            seven_day: None,
        };
        assert_eq!(usage_segment(Some(&rl)), None);

        // A window object present but missing used_percentage is still absent.
        let rl = RateLimits {
            five_hour: Some(RateWindow {
                used_percentage: None,
            }),
            seven_day: None,
        };
        assert_eq!(usage_segment(Some(&rl)), None);
    }

    #[test]
    fn usage_color_boundaries() {
        assert_eq!(usage_color(0.0), UsageColor::Normal);
        assert_eq!(usage_color(49.0), UsageColor::Normal);
        assert_eq!(usage_color(49.999), UsageColor::Normal);
        assert_eq!(usage_color(50.0), UsageColor::Amber);
        assert_eq!(usage_color(79.0), UsageColor::Amber);
        assert_eq!(usage_color(79.999), UsageColor::Amber);
        assert_eq!(usage_color(80.0), UsageColor::Red);
        assert_eq!(usage_color(100.0), UsageColor::Red);
    }

    #[test]
    fn usage_color_clamps_out_of_range() {
        assert_eq!(usage_color(-10.0), UsageColor::Normal);
        assert_eq!(usage_color(150.0), UsageColor::Red);
    }

    #[test]
    fn colorize_usage_normal() {
        let seg = colorize_usage("\u{23f3}24%", 24.0);
        assert_eq!(seg, "\u{23f3}24%", "no ANSI wrap below 50%");
    }

    #[test]
    fn colorize_usage_amber() {
        let seg = colorize_usage("\u{23f3}62%", 62.0);
        assert_eq!(seg, "\u{1b}[33m\u{23f3}62%\u{1b}[0m");
    }

    #[test]
    fn colorize_usage_red() {
        let seg = colorize_usage("\u{1f4c5}85%", 85.0);
        assert_eq!(seg, "\u{1b}[31m\u{1f4c5}85%\u{1b}[0m");
    }
}
