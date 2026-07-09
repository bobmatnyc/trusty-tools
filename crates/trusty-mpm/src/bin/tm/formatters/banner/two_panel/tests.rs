use super::*;
use crate::formatters::info_box::{CommitLine, DaemonInfo, WelcomeData};

fn base_data() -> WelcomeData {
    WelcomeData {
        project: "owner/repo".to_string(),
        workspace: "/home/user/projects/repo".to_string(),
        user: "alice".to_string(),
        reconnecting: false,
        session_name: String::new(),
        daemon: DaemonInfo::default(),
        recent_commits: vec![],
        memory_status: "(not detected)".to_string(),
        search_status: "(not detected)".to_string(),
        review_status: "(not detected)".to_string(),
    }
}

fn reconnect_data() -> WelcomeData {
    WelcomeData {
        reconnecting: true,
        session_name: "tmpm-my-proj".to_string(),
        ..base_data()
    }
}

fn data_with_commits() -> WelcomeData {
    WelcomeData {
        recent_commits: vec![CommitLine {
            sha: "abc1234".to_string(),
            age: "2h".to_string(),
            subject: "fix: something important".to_string(),
        }],
        ..base_data()
    }
}

/// Fixed, deterministic art source for tests.
///
/// Why: issue #2224 — tests must never depend on the operator's
/// `~/.trusty-mpm/banner.txt`. Shading the embedded compile-time default
/// directly (bypassing `load_and_shade_banner`'s disk read) gives every
/// test in this module the same art regardless of machine state.
/// What: returns `shade_image(DEFAULT_BANNER_ART, false)` — colour is
/// disabled so callers get plain-text rows matching the `set_override(false)`
/// convention already used throughout this test module.
/// Test: exercised by every test below that calls
/// `render_two_panel_banner_with_image`.
fn test_image() -> (Vec<String>, usize) {
    super::super::shade_image(super::super::source::DEFAULT_BANNER_ART, false)
}

/// Single-box banner renders without panic; every line starts with a border char.
#[test]
fn two_panel_compose_alignment() {
    colored::control::set_override(false);
    let data = base_data();
    let (image_lines, image_cols) = test_image();
    let result = render_two_panel_banner_with_image(&data, 120, false, &image_lines, image_cols);
    assert!(result.is_some(), "wide terminal should produce a banner");
    let out = result.unwrap();
    for line in out.lines() {
        let bare = strip_ansi(line);
        if bare.is_empty() {
            continue;
        }
        let first = bare.chars().next().unwrap_or(' ');
        assert!(
            matches!(first, '╭' | '╰' | '│'),
            "each line must start with a box corner or side: {bare:?}"
        );
    }
    colored::control::unset_override();
}

/// Title bar must embed the crate version.
#[test]
fn single_box_title_bar_contains_version() {
    colored::control::set_override(false);
    let data = base_data();
    let (image_lines, image_cols) = test_image();
    let out = render_two_panel_banner_with_image(&data, 120, false, &image_lines, image_cols)
        .expect("wide terminal produces banner");
    let first_line = out.lines().next().unwrap_or("");
    let bare = strip_ansi(first_line);
    assert!(
        bare.contains(env!("CARGO_PKG_VERSION")),
        "title bar must contain CARGO_PKG_VERSION: {bare:?}"
    );
    assert!(
        bare.starts_with('╭'),
        "title bar must start with ╭: {bare:?}"
    );
    colored::control::unset_override();
}

/// Shaded art rows match the actual line count of the embedded default.
#[test]
fn image_clip_correct_rows() {
    let (lines, _cols) = super::super::shade_image(super::super::source::DEFAULT_BANNER_ART, false);
    let raw_count = super::super::source::DEFAULT_BANNER_ART.lines().count();
    assert_eq!(
        lines.len(),
        raw_count,
        "shade_image must return one row per art line: expected {raw_count}, got {}",
        lines.len()
    );
    assert!(raw_count >= 2, "default art must have at least 2 lines");
}

/// Each shaded row has the same display width (auto-sized to art max width).
#[test]
fn image_clip_correct_cols() {
    let (lines, expected_cols) =
        super::super::shade_image(super::super::source::DEFAULT_BANNER_ART, false);
    assert!(expected_cols > 0, "auto-sized cols must be > 0");
    for (i, line) in lines.iter().enumerate() {
        let bare = strip_ansi(line);
        let width = bare.width();
        assert_eq!(
            width, expected_cols,
            "row {i} display width ({width}) must equal auto-sized cols ({expected_cols})"
        );
    }
}

/// Non-space art characters emit truecolor escapes when color is enabled.
#[test]
fn image_shading_emits_truecolor() {
    // Issue #1858: `shade_image` used to read the process-global
    // `colored::control` override internally, so this test raced every
    // sibling test in this file that flips the same global via
    // `set_override(false)`. Now that `use_color` is an explicit
    // parameter, this test needs no global mutation at all — it is fully
    // deterministic and immune to parallel-test interleaving.
    let (lines, _) = super::super::shade_image(super::super::source::DEFAULT_BANNER_ART, true);
    let any_colored = lines.iter().any(|l| l.contains("\x1B[38;2;"));
    assert!(
        any_colored,
        "shaded art must emit truecolor escapes when color is enabled"
    );
}

/// Auto-sizing picks up dimensions from a custom art string.
#[test]
fn image_autosize_uses_art_dimensions() {
    let custom_art = "ABC\nDE\nFGHI\n";
    let (lines, cols) = super::super::shade_image(custom_art, false);
    // "FGHI" is the widest line: 4 chars.
    assert_eq!(cols, 4, "max width of 'FGHI' is 4");
    assert_eq!(lines.len(), 3, "three non-empty lines");
    // Each row must be padded to cols=4.
    for (i, line) in lines.iter().enumerate() {
        let bare = strip_ansi(line);
        assert_eq!(bare.width(), 4, "row {i} must be padded to 4 display cols");
    }
}

/// The right-column content has no inner-box border chars (╭╮╰╯).
#[test]
fn right_col_no_inner_border() {
    colored::control::set_override(false);
    let data = base_data();
    let (image_lines, image_cols) = test_image();
    let out = render_two_panel_banner_with_image(&data, 120, false, &image_lines, image_cols)
        .expect("banner");
    for line in out.lines() {
        let bare = strip_ansi(line);
        if bare.starts_with('╭') || bare.starts_with('╰') {
            continue;
        }
        let inner: String = bare.chars().skip(1).collect();
        let inner_trimmed = inner.trim_end_matches('│');
        for ch in ['╭', '╮', '╰', '╯'] {
            assert!(
                !inner_trimmed.contains(ch),
                "inner border char {ch:?} must not appear inside content rows: {bare:?}"
            );
        }
    }
    colored::control::unset_override();
}

/// Reconnecting label appears in the stacked narrow-fallback box.
#[test]
fn reconnect_label_in_narrow_box() {
    colored::control::set_override(false);
    let data = reconnect_data();
    let (image_lines, image_cols) = test_image();
    let out = render_two_panel_banner_with_image(&data, 80, true, &image_lines, image_cols)
        .expect("narrow box");
    let bare = strip_ansi(&out);
    assert!(
        bare.contains("Reconnecting..."),
        "narrow box must contain 'Reconnecting...' label: {bare:.200}"
    );
    colored::control::unset_override();
}

/// Reconnecting label is absent from a normal (non-reconnect) narrow banner.
#[test]
fn narrow_box_no_reconnect_label_when_not_reconnecting() {
    colored::control::set_override(false);
    let data = base_data();
    let (image_lines, image_cols) = test_image();
    let out = render_two_panel_banner_with_image(&data, 80, false, &image_lines, image_cols)
        .expect("narrow box");
    let bare = strip_ansi(&out);
    assert!(
        !bare.contains("Reconnecting..."),
        "normal narrow box must not contain 'Reconnecting...' label"
    );
    colored::control::unset_override();
}

/// Both panels reach equal height (shorter one is padded).
#[test]
fn two_panel_shorter_panel_padded_to_equal_height() {
    let data = data_with_commits();
    colored::control::set_override(false);
    let (image_lines, image_cols) = test_image();
    let out = render_two_panel_banner_with_image(&data, 130, false, &image_lines, image_cols)
        .expect("wide banner");
    for line in out.lines() {
        let bare = strip_ansi(line);
        if bare.starts_with('│') {
            assert!(
                bare.ends_with('│'),
                "content line must end with │: {bare:?}"
            );
        }
    }
    colored::control::unset_override();
}

/// Very narrow terminal (< MIN_WIDTH_BOX) returns None.
#[test]
fn two_panel_narrow_fallback() {
    let data = base_data();
    let (image_lines, image_cols) = test_image();
    assert!(
        render_two_panel_banner_with_image(
            &data,
            MIN_WIDTH_BOX - 1,
            false,
            &image_lines,
            image_cols
        )
        .is_none(),
        "very narrow terminal must return None"
    );
}

/// Terminals narrower than MIN_WIDTH_TWO_PANEL but ≥ MIN_WIDTH_BOX get a stacked box.
#[test]
fn two_panel_stacked_on_medium_width() {
    let data = base_data();
    let (image_lines, image_cols) = test_image();
    let result = render_two_panel_banner_with_image(&data, 80, false, &image_lines, image_cols);
    assert!(
        result.is_some(),
        "80-col terminal must produce a stacked box (not None)"
    );
}

/// At the wide-mode threshold boundary, no panic.
#[test]
fn two_panel_at_threshold_boundary() {
    let data = base_data();
    let (image_lines, image_cols) = test_image();
    let _ = render_two_panel_banner_with_image(
        &data,
        MIN_WIDTH_TWO_PANEL - 1,
        false,
        &image_lines,
        image_cols,
    );
    let _ = render_two_panel_banner_with_image(
        &data,
        MIN_WIDTH_TWO_PANEL,
        false,
        &image_lines,
        image_cols,
    );
}

/// Version string must appear in the output (in the title bar).
#[test]
fn two_panel_version_present() {
    colored::control::set_override(false);
    let data = base_data();
    let (image_lines, image_cols) = test_image();
    let out = render_two_panel_banner_with_image(&data, 120, false, &image_lines, image_cols)
        .expect("two-panel");
    let bare = strip_ansi(&out);
    assert!(
        bare.contains(env!("CARGO_PKG_VERSION")),
        "version must appear in banner: {bare:.100}"
    );
    colored::control::unset_override();
}

/// The two-panel banner must not repeat the version in the right panel:
/// the title bar already shows it.  The full banner has exactly one copy.
#[test]
fn right_panel_omits_version_line() {
    colored::control::set_override(false);
    let data = base_data();
    let version = env!("CARGO_PKG_VERSION");
    let (image_lines, image_cols) = test_image();
    let out = render_two_panel_banner_with_image(&data, 120, false, &image_lines, image_cols)
        .expect("wide banner");
    let bare = strip_ansi(&out);
    let count = bare.matches(version).count();
    assert_eq!(
        count, 1,
        "version string must appear exactly once (title bar only); found {count}"
    );
    colored::control::unset_override();
}

/// Reconnecting state shows the reconnecting label in the wide layout.
#[test]
fn two_panel_reconnecting_shows_indicator() {
    colored::control::set_override(false);
    let data = reconnect_data();
    let (image_lines, image_cols) = test_image();
    let out = render_two_panel_banner_with_image(&data, 120, true, &image_lines, image_cols)
        .expect("two-panel");
    let bare = strip_ansi(&out);
    assert!(
        bare.contains("Reconnecting..."),
        "reconnecting indicator must appear: {bare:.200}"
    );
    colored::control::unset_override();
}

/// `strip_ansi` correctly removes SGR colour escapes.
#[test]
fn strip_ansi_removes_color_codes() {
    let colored = "\x1B[38;2;183;65;14mhello\x1B[0m world";
    let bare = strip_ansi(colored);
    assert_eq!(bare, "hello world");
}

/// Wide layout has a `│` vertical divider between columns; every line is
/// exactly `term_width` display columns wide.
#[test]
fn wide_layout_has_vertical_divider() {
    colored::control::set_override(false);
    let data = base_data();
    let (image_lines, image_cols) = test_image();
    let out = render_two_panel_banner_with_image(&data, 120, false, &image_lines, image_cols)
        .expect("wide banner");
    let mut checked = 0usize;
    for line in out.lines() {
        let bare = strip_ansi(line);
        if bare.is_empty() {
            continue;
        }
        assert_eq!(bare.width(), 120, "line width mismatch: {bare:?}");
        if bare.starts_with('│') {
            let chars: Vec<char> = bare.chars().collect();
            let inner = &chars[1..chars.len().saturating_sub(1)];
            assert!(inner.contains(&'│'), "no interior │ divider in: {bare:?}");
            checked += 1;
        }
    }
    assert!(checked > 0, "no content lines found in wide banner");
    colored::control::unset_override();
}
