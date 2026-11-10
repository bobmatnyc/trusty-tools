//! The tm tui colour theme: the trusty-console dark palette as named roles.
//!
//! Why (#10966, goal ar-G01): `tm tui` should look like trusty-console. Panes
//! used to hard-code bare ANSI names (`Color::Cyan`, `Color::DarkGray`), so the
//! palette could not change in one place. Panes now ask for a role
//! (`palette().accent`), never a `Color`; this module is the one place that
//! owns colour values. The theme is dark only (ruling Dc4, 2026-11-10).
//! What: [`ColorDepth`] is detected once from `NO_COLOR`, `COLORTERM` and
//! `TERM` ([`detect_depth`]); [`Palette::for_depth`] resolves every role to a
//! truecolor, 256-colour or 16-colour value, or to no colour. [`palette`] is the
//! process-wide palette, fixed at first use. Values come from the Foundry
//! "night shift" tokens in `crates/trusty-console/ui/src/theme.css`; the mapping
//! is `docs/design/UI/tm-tui-console-look.md` section 7.
//! Test: `detect_depth_reads_no_color_colorterm_and_term`,
//! `roles_resolve_at_each_depth`, `no_color_gives_no_colour_and_reverse_selection`,
//! `home_tile_title_and_background_use_theme_roles`.

use std::sync::OnceLock;

use ratatui::{
    Frame,
    style::{Color, Modifier, Style},
    widgets::Block,
};

/// How many colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    /// 24-bit RGB (`COLORTERM=truecolor` or `24bit`).
    TrueColor,
    /// The xterm 256-colour index (`TERM` containing `256color`).
    Ansi256,
    /// The 16 named ANSI colours; the fallback.
    Ansi16,
    /// No colour at all (`NO_COLOR` set and non-empty, per no-color.org).
    NoColor,
}

/// Pick the colour depth from environment values.
///
/// Why: detection must be testable without mutating the process environment,
/// so the lookup is a parameter.
/// What: `NO_COLOR` (non-empty) wins and gives [`ColorDepth::NoColor`];
/// `COLORTERM` of `truecolor`/`24bit` gives [`ColorDepth::TrueColor`]; a `TERM`
/// containing `256color` gives [`ColorDepth::Ansi256`]; anything else is
/// [`ColorDepth::Ansi16`].
/// Test: `detect_depth_reads_no_color_colorterm_and_term`.
pub fn detect_depth(var: impl Fn(&str) -> Option<String>) -> ColorDepth {
    if var("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return ColorDepth::NoColor;
    }
    let colorterm = var("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColorDepth::TrueColor;
    }
    if var("TERM").is_some_and(|t| t.contains("256color")) {
        return ColorDepth::Ansi256;
    }
    ColorDepth::Ansi16
}

/// One role's value at each depth: truecolor RGB, xterm-256 index, ANSI-16 name.
struct RoleSpec {
    rgb: (u8, u8, u8),
    indexed: u8,
    ansi: Color,
}

impl RoleSpec {
    fn at(&self, depth: ColorDepth) -> Color {
        match depth {
            ColorDepth::TrueColor => Color::Rgb(self.rgb.0, self.rgb.1, self.rgb.2),
            ColorDepth::Ansi256 => Color::Indexed(self.indexed),
            ColorDepth::Ansi16 => self.ansi,
            ColorDepth::NoColor => Color::Reset,
        }
    }
}

// Foundry dark tokens (theme.css `[data-theme="dark"]`). In 16 colours the
// background, surface and text stay the terminal's own (`Reset`), so the
// fallback never paints a fill that fights the terminal's own text colour;
// accent and warn share Yellow there, so status words carry the meaning, never
// colour alone.
const BACKGROUND: RoleSpec = spec((0x20, 0x16, 0x12), 234, Color::Reset); // --trusty-content-bg
const SURFACE: RoleSpec = spec((0x2b, 0x1c, 0x12), 235, Color::Reset); // --trusty-card-bg
const TEXT: RoleSpec = spec((0xf0, 0xe7, 0xd8), 255, Color::Reset); // --trusty-text-primary
const MUTED: RoleSpec = spec((0xa5, 0x8a, 0x6b), 137, Color::DarkGray); // --trusty-text-muted
const ACCENT: RoleSpec = spec((0xd9, 0x77, 0x42), 173, Color::Yellow); // --trusty-accent
const OK: RoleSpec = spec((0x8f, 0xbf, 0x6a), 107, Color::Green); // --trusty-success
const WARN: RoleSpec = spec((0xd9, 0xa8, 0x3a), 179, Color::Yellow); // --trusty-warning
const DEGRADED: RoleSpec = spec((0xe2, 0x92, 0x4f), 173, Color::Yellow); // --trusty-status-degraded
const ERR: RoleSpec = spec((0xe0, 0x6a, 0x52), 167, Color::Red); // --trusty-danger
const INFO: RoleSpec = spec((0x7b, 0xa7, 0xc4), 110, Color::Blue); // --trusty-info (tokens.css)

const fn spec(rgb: (u8, u8, u8), indexed: u8, ansi: Color) -> RoleSpec {
    RoleSpec { rgb, indexed, ansi }
}

/// The console palette resolved to one colour depth.
///
/// Why: panes name a semantic role, so a palette or depth change touches only
/// this type.
/// What: one `Color` per role, plus the composite styles [`Palette::base`],
/// [`Palette::overlay`] and [`Palette::selection`].
/// Test: `roles_resolve_at_each_depth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The depth these values were resolved for.
    pub depth: ColorDepth,
    /// Screen background.
    pub background: Color,
    /// Card, panel and popup background.
    pub surface: Color,
    /// Primary text.
    pub text: Color,
    /// Secondary text: hints, details, inactive states.
    pub muted: Color,
    /// Amber/copper accent: titles, focus, the selected item.
    pub accent: Color,
    /// Healthy / running / ready.
    pub ok: Color,
    /// Warning / pending / available.
    pub warn: Color,
    /// Degraded, between warn and err.
    pub degraded: Color,
    /// Error / down.
    pub err: Color,
    /// Informational (on-demand, background work).
    pub info: Color,
    /// Sparkline bars (the console draws them in the accent).
    pub sparkline: Color,
}

impl Palette {
    /// Resolve every role for `depth`.
    pub fn for_depth(depth: ColorDepth) -> Self {
        Self {
            depth,
            background: BACKGROUND.at(depth),
            surface: SURFACE.at(depth),
            text: TEXT.at(depth),
            muted: MUTED.at(depth),
            accent: ACCENT.at(depth),
            ok: OK.at(depth),
            warn: WARN.at(depth),
            degraded: DEGRADED.at(depth),
            err: ERR.at(depth),
            info: INFO.at(depth),
            sparkline: ACCENT.at(depth),
        }
    }

    /// Text on the screen background; painted under every frame.
    pub fn base(&self) -> Style {
        Style::default().fg(self.text).bg(self.background)
    }

    /// Text on the surface colour, for popups drawn over a cleared area.
    pub fn overlay(&self) -> Style {
        Style::default().fg(self.text).bg(self.surface)
    }

    /// The highlighted row: dark text on the accent, or reverse video when
    /// there is no colour, so the selection stays visible.
    pub fn selection(&self) -> Style {
        match self.depth {
            ColorDepth::NoColor => Style::default().add_modifier(Modifier::REVERSED),
            _ => Style::default().fg(self.background_ink()).bg(self.accent),
        }
    }

    /// Ink for text on an accent fill: the screen background, or Black when
    /// the background is the terminal's own.
    fn background_ink(&self) -> Color {
        match self.background {
            Color::Reset => Color::Black,
            c => c,
        }
    }
}

/// The process-wide palette, resolved once on first use.
///
/// Why: colour depth is a property of the terminal, fixed for the session;
/// resolving it per frame would re-read the environment for nothing.
/// What: under `cfg(test)` the depth is fixed to truecolor so render tests are
/// independent of the runner's `TERM`; otherwise [`detect_depth`] reads the
/// process environment.
/// Test: `home_tile_title_and_background_use_theme_roles`.
pub fn palette() -> &'static Palette {
    static PALETTE: OnceLock<Palette> = OnceLock::new();
    PALETTE.get_or_init(|| Palette::for_depth(startup_depth()))
}

#[cfg(not(test))]
fn startup_depth() -> ColorDepth {
    detect_depth(|name| std::env::var(name).ok())
}

#[cfg(test)]
fn startup_depth() -> ColorDepth {
    ColorDepth::TrueColor
}

/// Paint the screen background and default text colour under a frame.
///
/// Why: every top-level screen starts from the console background, so cells a
/// pane leaves unstyled read as console text, not terminal defaults.
/// What: fills the whole frame with [`Palette::base`]; a no-op look in 16
/// colours and with no colour, where both are the terminal's own.
pub fn paint_background(frame: &mut Frame) {
    let area = frame.area();
    frame.render_widget(Block::default().style(palette().base()), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| {
            owned
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn detect_depth_reads_no_color_colorterm_and_term() {
        let cases: &[(&[(&str, &str)], ColorDepth)] = &[
            (&[], ColorDepth::Ansi16),
            (&[("TERM", "xterm")], ColorDepth::Ansi16),
            (&[("TERM", "xterm-256color")], ColorDepth::Ansi256),
            (&[("TERM", "tmux-256color")], ColorDepth::Ansi256),
            (&[("COLORTERM", "truecolor")], ColorDepth::TrueColor),
            (
                &[("COLORTERM", "24bit"), ("TERM", "xterm")],
                ColorDepth::TrueColor,
            ),
            (&[("COLORTERM", "TrueColor")], ColorDepth::TrueColor),
            (
                &[("COLORTERM", "yes"), ("TERM", "xterm")],
                ColorDepth::Ansi16,
            ),
            (
                &[("NO_COLOR", "1"), ("COLORTERM", "truecolor")],
                ColorDepth::NoColor,
            ),
            // no-color.org: an empty NO_COLOR does not disable colour.
            (
                &[("NO_COLOR", ""), ("TERM", "xterm-256color")],
                ColorDepth::Ansi256,
            ),
        ];
        for (vars, want) in cases {
            assert_eq!(detect_depth(env(vars)), *want, "env {vars:?}");
        }
    }

    #[test]
    fn roles_resolve_at_each_depth() {
        let t = Palette::for_depth(ColorDepth::TrueColor);
        assert_eq!(t.background, Color::Rgb(0x20, 0x16, 0x12));
        assert_eq!(t.surface, Color::Rgb(0x2b, 0x1c, 0x12));
        assert_eq!(t.text, Color::Rgb(0xf0, 0xe7, 0xd8));
        assert_eq!(t.muted, Color::Rgb(0xa5, 0x8a, 0x6b));
        assert_eq!(t.accent, Color::Rgb(0xd9, 0x77, 0x42));
        assert_eq!(t.ok, Color::Rgb(0x8f, 0xbf, 0x6a));
        assert_eq!(t.warn, Color::Rgb(0xd9, 0xa8, 0x3a));
        assert_eq!(t.err, Color::Rgb(0xe0, 0x6a, 0x52));
        assert_eq!(t.sparkline, t.accent);

        let i = Palette::for_depth(ColorDepth::Ansi256);
        assert_eq!(i.background, Color::Indexed(234));
        assert_eq!(i.text, Color::Indexed(255));
        assert_eq!(i.accent, Color::Indexed(173));
        assert_eq!(i.ok, Color::Indexed(107));
        assert_eq!(i.err, Color::Indexed(167));

        let a = Palette::for_depth(ColorDepth::Ansi16);
        // The terminal keeps its own background and text in 16 colours.
        assert_eq!(
            (a.background, a.surface, a.text),
            (Color::Reset, Color::Reset, Color::Reset)
        );
        assert_eq!(a.muted, Color::DarkGray);
        assert_eq!(a.accent, Color::Yellow);
        assert_eq!(a.ok, Color::Green);
        assert_eq!(a.warn, Color::Yellow);
        assert_eq!(a.err, Color::Red);
        assert_eq!(a.selection().bg, Some(Color::Yellow));
        assert_eq!(a.selection().fg, Some(Color::Black));
    }

    #[test]
    fn no_color_gives_no_colour_and_reverse_selection() {
        let p = Palette::for_depth(ColorDepth::NoColor);
        let roles = [
            p.background,
            p.surface,
            p.text,
            p.muted,
            p.accent,
            p.ok,
            p.warn,
            p.degraded,
            p.err,
            p.info,
            p.sparkline,
        ];
        assert!(roles.iter().all(|c| *c == Color::Reset), "{roles:?}");
        let sel = p.selection();
        assert_eq!((sel.fg, sel.bg), (None, None));
        assert!(sel.add_modifier.contains(Modifier::REVERSED));
    }
}
