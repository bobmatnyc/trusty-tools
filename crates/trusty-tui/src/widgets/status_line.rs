//! The bottom statusline — new in this slice, rendering
//! [`crate::model::StatuslineSegment`] instead of tagent's hardcoded,
//! cost-formula-laden statusline.
//!
//! Why: tagent's `repl/tui/status.rs` (not migrated — see its module-level
//! callout) hardcodes an OpenRouter pricing formula and TM/claude-mpm
//! session-count chunks directly into the render function. DOC-50 §3.2
//! Slice 1.5 (already on `main`) replaced that with the engine-populated
//! [`crate::model::StatuslineSegment`] enum specifically so the shared crate
//! never needs to know what a "cost" or a "session count" means for a given
//! product — it only needs to render whichever segments the engine pushed
//! via `ReplEvent::StatuslineUpdate`. This module is that renderer, the
//! piece Slice 1.5 didn't add because it had no widget layer yet.
//!
//! Slice 1.5's original segment set had no way to reproduce tagent's actual
//! statusline *chrome* — the bold-cyan `"[trusty-agents] "` bracket prefix,
//! green `"✓ … All systems go."` success framing, and the bold/dim
//! distinction between its `TM:`/`MPM:` session-count chunks
//! (`status.rs:36-53,220-240,245-278`) — which would have made Slice 10's
//! cutover unable to meet a "zero UX regression" bar. [`crate::model`] now
//! carries [`crate::model::StatuslineEmphasis`] for exactly this; this
//! module owns turning that hint into an actual `ratatui::style::Style`
//! (kept out of `crate::model` on purpose — see that enum's doc comment).
//!
//! What: [`build_statusline`] turns `app.statusline` into one styled `Line`;
//! [`draw_statusline`] renders it into a `Frame`.
//!
//! # Spec References
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — §3.2, the generalization layer this renders.
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 4 deliverable (§5, Slice 4): the status-line widget.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::ReplApp;
use crate::model::{StatuslineEmphasis, StatuslineSegment};

/// Separator rendered between adjacent segments.
const SEPARATOR: &str = " · ";

/// Map a [`StatuslineEmphasis`] to the `ratatui::style::Style` it renders
/// as.
///
/// Why: split out of [`segment_spans`] so the emphasis→style mapping is one
/// place, independently testable, and so `Custom`/`Chrome` share it rather
/// than duplicating a match.
/// What: a deliberately small, fixed palette — this widget owns concrete
/// colors; `crate::model::StatuslineEmphasis` only names semantic intent
/// (see that enum's doc comment for why the split exists).
/// Test: [`tests::emphasis_style_maps_every_variant`].
fn emphasis_style(emphasis: StatuslineEmphasis) -> Style {
    match emphasis {
        StatuslineEmphasis::Normal => Style::default(),
        StatuslineEmphasis::Dim => Style::default().add_modifier(Modifier::DIM),
        StatuslineEmphasis::Bold => Style::default().add_modifier(Modifier::BOLD),
        StatuslineEmphasis::Success => Style::default().fg(Color::Green),
        StatuslineEmphasis::Warning => Style::default().fg(Color::Yellow),
        StatuslineEmphasis::Error => Style::default().fg(Color::Red),
        StatuslineEmphasis::Accent => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    }
}

/// Render one [`StatuslineSegment`] as its display spans.
///
/// Why: split out of [`build_statusline`] so each segment kind's rendering
/// is independently testable and so a future segment variant is a one-arm
/// addition here rather than a change to the join/separator logic.
/// What: each variant gets a short, glance-able rendering; `Custom` renders
/// `"{label}: {value}"` styled per its `emphasis`; `Chrome` renders `text`
/// verbatim styled per its `emphasis` — the seam a product uses to
/// reproduce tagent's bracket-prefix/success-framing/bold-vs-dim chrome
/// (see the module doc comment).
/// Test: [`tests::segment_spans_render_each_variant`],
/// [`tests::segment_spans_custom_applies_emphasis`],
/// [`tests::segment_spans_chrome_renders_unlabeled_styled_text`].
fn segment_spans(seg: &StatuslineSegment) -> Vec<Span<'static>> {
    match seg {
        StatuslineSegment::SessionId(id) => vec![Span::raw(format!("session:{id}"))],
        StatuslineSegment::Model { provider, name } => {
            vec![Span::raw(format!("{provider}:{name}"))]
        }
        StatuslineSegment::Project(name) => vec![Span::raw(name.clone())],
        StatuslineSegment::Workstream { id, name } => {
            vec![Span::raw(format!("WS: {name} ({id})"))]
        }
        StatuslineSegment::Cost(formatted) => vec![Span::styled(
            formatted.clone(),
            Style::default().add_modifier(Modifier::DIM),
        )],
        StatuslineSegment::Custom {
            label,
            value,
            emphasis,
        } => vec![Span::styled(
            format!("{label}: {value}"),
            emphasis_style(*emphasis),
        )],
        StatuslineSegment::Chrome { text, emphasis } => {
            vec![Span::styled(text.clone(), emphasis_style(*emphasis))]
        }
    }
}

/// Build the full statusline `Line` from `app.statusline`.
///
/// Why: kept pure (no `Frame`) so tests can assert on rendered text without
/// a terminal backend.
/// What: joins every segment's spans with [`SEPARATOR`]; an empty segment
/// list renders an empty `Line` (the statusline row collapses to blank,
/// never to placeholder text a product didn't ask for).
/// Test: [`tests::build_statusline_joins_segments_with_separator`],
/// [`tests::build_statusline_empty_when_no_segments`].
pub fn build_statusline(app: &ReplApp) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, seg) in app.statusline.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                SEPARATOR,
                Style::default().fg(Color::DarkGray),
            ));
        }
        spans.extend(segment_spans(seg));
    }
    Line::from(spans)
}

/// Render the statusline into `area`.
pub fn draw_statusline(f: &mut ratatui::Frame, app: &ReplApp, area: Rect) {
    let line = build_statusline(app);
    f.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn segment_spans_render_each_variant() {
        assert_eq!(
            line_text(&Line::from(segment_spans(&StatuslineSegment::SessionId(
                "a1b2".into()
            )))),
            "session:a1b2"
        );
        assert_eq!(
            line_text(&Line::from(segment_spans(&StatuslineSegment::Model {
                provider: "anthropic".into(),
                name: "claude-sonnet-5".into(),
            }))),
            "anthropic:claude-sonnet-5"
        );
        assert_eq!(
            line_text(&Line::from(segment_spans(&StatuslineSegment::Project(
                "trusty-tools".into()
            )))),
            "trusty-tools"
        );
        assert_eq!(
            line_text(&Line::from(segment_spans(&StatuslineSegment::Workstream {
                id: "a1b2c3d4".into(),
                name: "Token rotation".into(),
            }))),
            "WS: Token rotation (a1b2c3d4)"
        );
        assert_eq!(
            line_text(&Line::from(segment_spans(&StatuslineSegment::Cost(
                "$0.0034".into()
            )))),
            "$0.0034"
        );
        assert_eq!(
            line_text(&Line::from(segment_spans(&StatuslineSegment::Custom {
                label: "TM".into(),
                value: "2 sessions".into(),
                emphasis: StatuslineEmphasis::Normal,
            }))),
            "TM: 2 sessions"
        );
    }

    /// Pins the fix for the MEDIUM finding: `Custom`'s `emphasis` must
    /// actually change the rendered style (not just be accepted and
    /// ignored) — this is what lets a `MPM:` segment render bold while a
    /// `TM:` segment stays plain, reproducing tagent's distinction.
    #[test]
    fn segment_spans_custom_applies_emphasis() {
        let plain = segment_spans(&StatuslineSegment::Custom {
            label: "TM".into(),
            value: "3 sessions".into(),
            emphasis: StatuslineEmphasis::Normal,
        });
        let bold = segment_spans(&StatuslineSegment::Custom {
            label: "MPM".into(),
            value: "2 sessions".into(),
            emphasis: StatuslineEmphasis::Bold,
        });
        assert_eq!(plain[0].style, Style::default());
        assert_eq!(bold[0].style, Style::default().add_modifier(Modifier::BOLD));
    }

    /// `Chrome` renders free-form text with NO `label: value` structure —
    /// the seam for tagent's bracket prefix / success framing, which isn't
    /// a label/value pair at all.
    #[test]
    fn segment_spans_chrome_renders_unlabeled_styled_text() {
        let spans = segment_spans(&StatuslineSegment::Chrome {
            text: "[trusty-agents] ".into(),
            emphasis: StatuslineEmphasis::Accent,
        });
        assert_eq!(line_text(&Line::from(spans.clone())), "[trusty-agents] ");
        assert!(!line_text(&Line::from(spans.clone())).contains(':'));
        assert_eq!(
            spans[0].style,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        );
    }

    /// Every [`StatuslineEmphasis`] variant must map to a style — a missing
    /// arm would silently fall back to whatever the `match`'s catch-all
    /// does (there isn't one; this pins that every variant is handled).
    #[test]
    fn emphasis_style_maps_every_variant() {
        let variants = [
            StatuslineEmphasis::Normal,
            StatuslineEmphasis::Dim,
            StatuslineEmphasis::Bold,
            StatuslineEmphasis::Success,
            StatuslineEmphasis::Warning,
            StatuslineEmphasis::Error,
            StatuslineEmphasis::Accent,
        ];
        // Normal is the only variant that renders as the plain default
        // style; every other variant must differ from it.
        for v in variants {
            let style = emphasis_style(v);
            if v == StatuslineEmphasis::Normal {
                assert_eq!(style, Style::default());
            } else {
                assert_ne!(style, Style::default(), "{v:?} must not be unstyled");
            }
        }
    }

    #[test]
    fn build_statusline_joins_segments_with_separator() {
        let mut app = ReplApp::new("demo", "u");
        app.statusline = vec![
            StatuslineSegment::SessionId("a1b2".into()),
            StatuslineSegment::Project("trusty-tools".into()),
        ];
        let text = line_text(&build_statusline(&app));
        assert_eq!(text, "session:a1b2 · trusty-tools");
    }

    #[test]
    fn build_statusline_empty_when_no_segments() {
        let app = ReplApp::new("demo", "u");
        assert_eq!(line_text(&build_statusline(&app)), "");
    }
}
