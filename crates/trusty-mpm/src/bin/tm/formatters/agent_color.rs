//! Per-agent identity color for `tm` terminal output (#4068).
//!
//! Why: trusty-mpm's operating model is multi-agent delegation, and every line
//! naming an agent rendered in the same default foreground, so an operator
//! watching several agents could not tell them apart at a glance. A stable
//! per-name color makes agent IDENTITY visible, which is a third axis
//! orthogonal to the status and scope colors already in use.
//! What: [`agent_rgb`] hashes an agent name with FNV-1a into a fixed
//! eight-entry truecolor palette, so the same name always renders in the same
//! color within a session, across sessions, and across builds.
//! [`agent_label_with`] paints it, taking `use_color` explicitly so it stays
//! pure; [`agent_label`] and [`agent_label_padded`] bind that flag to
//! `colored`'s own gate for production call sites.
//! Test: `agent_color_is_stable_for_the_same_name`,
//! `agent_color_differs_between_names`,
//! `agent_palette_indices_are_pinned_across_runs`,
//! `agent_palette_avoids_the_reserved_status_and_scope_colors`,
//! `agent_label_paints_the_identity_color_when_color_is_on`,
//! `agent_label_is_plain_when_color_is_off`,
//! `agent_label_padded_matches_plain_padding_when_color_is_off`.

/// The eight identity colors, as raw truecolor triples.
///
/// Why: four ANSI slots already carry meaning in this workspace and must keep
/// meaning only that, so none of them may also mean "this agent":
/// - `Color::Green` — a service is running, and health is `ok`
///   (`formatters::services`).
/// - `Color::Red` — a service is down (`formatters::services`).
/// - `Color::Yellow` — a running service's health check failed
///   (`formatters::services`), and project-level agent scope in
///   `trusty-agents`' REPL label.
/// - `Color::Cyan` — user-level agent scope in `trusty-agents`' REPL label.
///
/// What: eight triples drawn from the blue / violet / magenta / tan bands,
/// deliberately away from the pure red, green, yellow, and cyan primaries
/// those four slots render as. Emitting them as `38;2;r;g;b` means an identity
/// color can never collide with a reserved named slot, even on a terminal that
/// has remapped its 16-color table.
///
/// #4068: index order is part of the contract — reordering these repaints
/// every agent, so append rather than insert if the palette ever grows.
/// Test: `agent_palette_avoids_the_reserved_status_and_scope_colors`.
pub(crate) const AGENT_PALETTE: [(u8, u8, u8); 8] = [
    (86, 156, 214),  // steel blue
    (197, 134, 192), // orchid
    (206, 145, 120), // terracotta
    (130, 170, 255), // periwinkle
    (240, 130, 200), // hot pink
    (152, 118, 220), // indigo
    (224, 168, 108), // tan
    (176, 176, 208), // slate lavender
];

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The palette slot an agent name hashes to.
///
/// Why (#4068): the hash must be pinned in this source, not borrowed from
/// [`std::collections::hash_map::DefaultHasher`], whose output std explicitly
/// does not guarantee across releases. A borrowed hash would repaint the whole
/// roster on a toolchain bump, which is exactly the instability this function
/// exists to rule out. FNV-1a is a few lines, allocation-free, and fixed
/// forever by its published constants.
/// What: FNV-1a over the name's UTF-8 bytes, reduced modulo the palette length.
/// Pure — no environment, no clock, no global state.
/// Test: `agent_palette_indices_are_pinned_across_runs`.
pub(crate) fn palette_index(name: &str) -> usize {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    (hash % AGENT_PALETTE.len() as u64) as usize
}

/// The identity color for an agent name, as an RGB triple.
///
/// Why: the raw triple is what the escape sequence needs, and what a test can
/// assert on without constructing a `colored` value.
/// What: indexes [`AGENT_PALETTE`] by [`palette_index`]. Deterministic: the
/// same name yields the same triple in every process and every build.
/// Test: `agent_color_is_stable_for_the_same_name`,
/// `agent_color_differs_between_names`.
pub(crate) fn agent_rgb(name: &str) -> (u8, u8, u8) {
    AGENT_PALETTE[palette_index(name)]
}

/// Whether `tm` should emit color right now.
///
/// Why: one place reads the decision, so every agent-name call site agrees.
/// What: delegates to `colored`'s own gate, which already accounts for
/// `NO_COLOR`, `CLICOLOR`/`CLICOLOR_FORCE`, whether stdout is a terminal, and
/// any explicit `colored::control` override.
/// Test: exercised through [`agent_label`]; the pure paths are covered
/// directly via [`agent_label_with`].
pub(crate) fn color_enabled() -> bool {
    colored::control::SHOULD_COLORIZE.should_colorize()
}

/// An agent name painted in its identity color, with the decision passed in.
///
/// Why (#1858): taking `use_color` as a parameter instead of probing
/// `colored`'s process-global override keeps this pure and deterministic, so
/// tests exercise the color-on and color-off paths directly with no shared
/// mutable state to race against. `formatters::banner::shade_image` takes the
/// same shape for the same reason, and emits the same `38;2;r;g;b` form.
/// What: with `use_color` false, returns the bare name — byte-identical to
/// printing it unstyled. With it true, wraps the name in the truecolor
/// foreground sequence for [`agent_rgb`] and a reset.
/// Test: `agent_label_paints_the_identity_color_when_color_is_on`,
/// `agent_label_is_plain_when_color_is_off`.
pub(crate) fn agent_label_with(name: &str, use_color: bool) -> String {
    if !use_color {
        return name.to_string();
    }
    let (r, g, b) = agent_rgb(name);
    format!("\u{1b}[38;2;{r};{g};{b}m{name}\u{1b}[0m")
}

/// An agent name painted in its identity color.
///
/// Why: the one call every rendering site should use, so a single decision
/// governs how agent identity looks everywhere `tm` prints it.
/// What: [`agent_label_with`] bound to [`color_enabled`].
/// Test: the pure form is covered by
/// `agent_label_paints_the_identity_color_when_color_is_on` and
/// `agent_label_is_plain_when_color_is_off`.
pub(crate) fn agent_label(name: &str) -> String {
    agent_label_with(name, color_enabled())
}

/// An agent name painted in its identity color, then padded to a column width.
///
/// Why: a `{:<24}` format spec counts the escape sequence's bytes as visible
/// width, so colorizing a table cell through `format!` misaligns every
/// following column. Padding OUTSIDE the color run keeps the alignment the
/// uncolored table already had.
/// What: emits [`agent_label_with`] followed by enough plain spaces to reach
/// `width` columns, matching `format!("{name:<width$}")` exactly whenever
/// `use_color` is false — including the case where the name already meets or
/// exceeds `width` and no padding is added.
/// Test: `agent_label_padded_matches_plain_padding_when_color_is_off`.
pub(crate) fn agent_label_padded_with(name: &str, width: usize, use_color: bool) -> String {
    let pad = width.saturating_sub(name.chars().count());
    format!("{}{}", agent_label_with(name, use_color), " ".repeat(pad))
}

/// [`agent_label_padded_with`] bound to [`color_enabled`].
pub(crate) fn agent_label_padded(name: &str, width: usize) -> String {
    agent_label_padded_with(name, width, color_enabled())
}

#[cfg(test)]
mod tests {
    use colored::Color;

    use super::*;

    #[test]
    fn agent_color_is_stable_for_the_same_name() {
        for _ in 0..100 {
            assert_eq!(agent_rgb("rust-engineer"), agent_rgb("rust-engineer"));
        }
        assert_eq!(agent_rgb("code-critic"), agent_rgb("code-critic"));
    }

    #[test]
    fn agent_color_differs_between_names() {
        assert_ne!(agent_rgb("rust-engineer"), agent_rgb("code-critic"));
        assert_ne!(agent_rgb("engineer"), agent_rgb("documentation"));
        assert_ne!(agent_rgb("research"), agent_rgb("engineer"));
    }

    /// Pins the hash itself: these indices follow from the FNV-1a constants
    /// above and must not move, because moving one repaints an agent the
    /// operator has already learned to recognise (#4068).
    #[test]
    fn agent_palette_indices_are_pinned_across_runs() {
        assert_eq!(palette_index("engineer"), 0);
        assert_eq!(palette_index("documentation"), 1);
        assert_eq!(palette_index("rust-engineer"), 3);
        assert_eq!(palette_index("research"), 4);
        assert_eq!(palette_index("code-critic"), 7);
        assert_eq!(agent_rgb("rust-engineer"), (130, 170, 255));
    }

    #[test]
    fn agent_palette_avoids_the_reserved_status_and_scope_colors() {
        // green/red/yellow are service STATUS in `formatters::services`;
        // yellow/cyan are agent SCOPE in trusty-agents' REPL label. Identity
        // is a third axis and must claim none of them.
        for reserved in [Color::Green, Color::Red, Color::Yellow, Color::Cyan] {
            let reserved_fg = reserved.to_fg_str().to_string();
            for (r, g, b) in AGENT_PALETTE {
                let identity = Color::TrueColor { r, g, b };
                assert_ne!(identity, reserved, "palette reuses {reserved:?}");
                // The same claim on the bytes actually written to the terminal.
                assert_ne!(
                    identity.to_fg_str().to_string(),
                    reserved_fg,
                    "identity color {identity:?} emits the reserved sequence {reserved_fg}"
                );
            }
        }
        // Every entry is distinct, or two agents would share a color.
        for (i, entry) in AGENT_PALETTE.iter().enumerate() {
            assert_eq!(
                AGENT_PALETTE.iter().filter(|c| *c == entry).count(),
                1,
                "palette entry {i} is duplicated"
            );
        }
    }

    #[test]
    fn agent_label_paints_the_identity_color_when_color_is_on() {
        // Slot 3 is periwinkle (130, 170, 255).
        assert_eq!(
            agent_label_with("rust-engineer", true),
            "\u{1b}[38;2;130;170;255mrust-engineer\u{1b}[0m"
        );
        // A different agent gets a visibly different sequence.
        assert_ne!(
            agent_label_with("code-critic", true),
            agent_label_with("rust-engineer", true)
        );
    }

    #[test]
    fn agent_label_is_plain_when_color_is_off() {
        for name in ["rust-engineer", "code-critic", "qa", "documentation"] {
            let rendered = agent_label_with(name, false);
            assert_eq!(rendered, name);
            assert!(
                !rendered.contains('\u{1b}'),
                "no-color rendering must carry no ANSI escape: {rendered:?}"
            );
        }
    }

    #[test]
    fn agent_label_padded_matches_plain_padding_when_color_is_off() {
        // Short name pads; a name at or beyond the width does not — the same
        // two cases `{:<24}` handles.
        assert_eq!(
            agent_label_padded_with("qa", 24, false),
            format!("{:<24}", "qa")
        );
        assert_eq!(
            agent_label_padded_with("rust-engineer", 24, false),
            format!("{:<24}", "rust-engineer")
        );
        let long = "an-agent-name-that-is-longer-than-the-column";
        assert_eq!(
            agent_label_padded_with(long, 24, false),
            format!("{long:<24}")
        );
        assert!(!agent_label_padded_with("qa", 24, false).contains('\u{1b}'));
    }

    #[test]
    fn agent_label_padded_keeps_the_column_width_visible_when_color_is_on() {
        // The escape bytes sit inside the cell, so the PLAIN width is still 24
        // and the next column stays aligned.
        let cell = agent_label_padded_with("qa", 24, true);
        assert!(cell.starts_with("\u{1b}[38;2;"));
        assert!(cell.ends_with(&("\u{1b}[0m".to_string() + &" ".repeat(22))));
        let visible: String = strip_escapes(&cell);
        assert_eq!(visible, format!("{:<24}", "qa"));
    }

    /// Drop every `ESC [ … m` sequence, leaving the visible characters.
    fn strip_escapes(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for inner in chars.by_ref() {
                    if inner == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}
