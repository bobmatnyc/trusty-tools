//! Right-aligned "component installed" table, rustup-style.
//!
//! Renders rows like:
//! ```text
//!         cargo installed                        8.46 MiB
//!        clippy installed                        2.89 MiB
//!      rust-std installed                       26.20 MiB
//! ```
//! The component name is right-aligned in a name column, followed by a fixed
//! verb (` installed`), then the human-readable size right-aligned in a size
//! column. Centralising the layout keeps every caller's table byte-identical.

use crate::error::Result;
use crate::output::Output;
use crate::size::format_mib;

/// Leading indent so the table sits in from the margin (matches rustup).
const INDENT: usize = 8;
/// Total width reserved for the right-aligned size column (after the verb).
const SIZE_COLUMN_WIDTH: usize = 16;
/// The action verb printed between the name and the size.
const VERB_INSTALLED: &str = "installed";

/// Why: A row needs both the component name and its byte size together so the
/// tracker can right-align names against each other (the longest name sets the
/// column width) — passing them separately would lose that grouping.
/// What: One component's name + size, as supplied by the caller.
/// Test: `components::tests::*` build these and assert the rendered table.
#[derive(Debug, Clone)]
pub struct Component {
    /// Display name, e.g. `cargo`, `rust-std`.
    pub name: String,
    /// Size in bytes; rendered via [`format_mib`].
    pub size_bytes: u64,
}

impl Component {
    /// Why: Ergonomic constructor so callers write
    /// `Component::new("cargo", 8_870_953)` instead of a struct literal.
    /// What: Builds a [`Component`] from any string-like name and a byte count.
    /// Test: Used throughout `components::tests`.
    pub fn new(name: impl Into<String>, size_bytes: u64) -> Self {
        Self {
            name: name.into(),
            size_bytes,
        }
    }
}

/// Why: The whole point of the rustup style is that the *set* of rows aligns as
/// a block (names right-aligned to the longest, sizes right-aligned in a fixed
/// column). That alignment can only be computed once all rows are known, so the
/// tracker accumulates components and renders them together.
/// What: Collects [`Component`]s and renders the aligned table through a shared
/// [`Output`], honouring its TTY/quiet/silent mode.
/// Test: `components::tests` assert the exact byte layout and the fallback.
#[derive(Debug, Clone)]
pub struct ComponentTracker {
    output: Output,
    components: Vec<Component>,
}

impl ComponentTracker {
    /// Why: Lets the tracker share a sink with a [`crate::Narrator`] so the
    /// `info:` lines and the table interleave correctly.
    /// What: Builds an empty tracker over the given [`Output`].
    /// Test: `components::tests::renders_rustup_table`.
    pub fn new(output: Output) -> Self {
        Self {
            output,
            components: Vec::new(),
        }
    }

    /// Why: Convenience for the standalone "just print a table on stderr" case.
    /// What: Builds an empty tracker over an autodetected stderr [`Output`].
    /// Test: Side-effect-only (ambient TTY); delegates to tested `Output`.
    pub fn to_stderr() -> Self {
        Self::new(Output::to_stderr())
    }

    /// Why: Callers discover component sizes incrementally (e.g. after each
    /// download finishes); buffering them lets the final render align as a block.
    /// What: Appends one component to the table, returning `&mut self` for
    /// chaining.
    /// Test: `components::tests::renders_rustup_table`.
    pub fn add(&mut self, component: Component) -> &mut Self {
        self.components.push(component);
        self
    }

    /// Why: Some callers already have a `Vec`; avoid forcing a manual loop.
    /// What: Extends the table with all supplied components.
    /// Test: `components::tests::renders_rustup_table`.
    pub fn extend(&mut self, components: impl IntoIterator<Item = Component>) -> &mut Self {
        self.components.extend(components);
        self
    }

    /// Why: Tests (and any non-TTY caller) need the exact text without writing
    /// to a sink; rendering to a string keeps the layout logic pure and
    /// testable, separate from I/O.
    /// What: Produces the fully aligned, multi-line table as a `String`
    /// (no trailing newline). Returns an empty string when there are no rows.
    /// Test: `components::tests::renders_rustup_table` /
    /// `right_aligns_names_to_longest`.
    pub fn render(&self) -> String {
        if self.components.is_empty() {
            return String::new();
        }
        let name_width = self
            .components
            .iter()
            .map(|c| c.name.len())
            .max()
            .unwrap_or(0);

        let mut lines = Vec::with_capacity(self.components.len());
        for c in &self.components {
            let size = format_mib(c.size_bytes);
            // Indent + right-aligned name + " installed" + right-aligned size.
            lines.push(format!(
                "{indent}{name:>name_width$} {verb}{size:>size_width$}",
                indent = " ".repeat(INDENT),
                name = c.name,
                name_width = name_width,
                verb = VERB_INSTALLED,
                size = size,
                size_width = SIZE_COLUMN_WIDTH,
            ));
        }
        lines.join("\n")
    }

    /// Why: The terminal verb — actually emit the rendered table through the
    /// shared output, honouring silent mode.
    /// What: Renders the table and writes it (line-buffered) to the sink; a
    /// no-op when there are no rows or in silent mode.
    /// Test: `components::tests::print_writes_through_output`.
    pub fn print(&self) -> Result<()> {
        if self.components.is_empty() || !self.output.is_visible() {
            return Ok(());
        }
        self.output.writeln(&self.render())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Mode;

    fn sample() -> ComponentTracker {
        let (out, _cap) = Output::for_capture(Mode::Plain);
        let mut t = ComponentTracker::new(out);
        t.add(Component::new("cargo", 8_870_953))
            .add(Component::new("clippy", 3_030_384))
            .add(Component::new("rust-std", 27_472_691));
        t
    }

    /// Why: The whole feature is this exact layout; pin it byte-for-byte so a
    /// refactor cannot drift the column math.
    /// What: Asserts each rendered row equals `INDENT spaces + right-aligned
    /// name (width 8) + " installed" + size right-aligned in width 16`.
    /// Test: This is the test.
    #[test]
    fn renders_rustup_table() {
        let table = sample().render();
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        // Longest name is "rust-std" (8 chars); names right-align to width 8.
        let expect = |name_field: &str, size: &str| {
            format!(
                "{indent}{name_field} installed{size:>width$}",
                indent = " ".repeat(INDENT),
                width = SIZE_COLUMN_WIDTH,
            )
        };
        assert_eq!(lines[0], expect("   cargo", "8.46 MiB"));
        assert_eq!(lines[1], expect("  clippy", "2.89 MiB"));
        assert_eq!(lines[2], expect("rust-std", "26.20 MiB"));
    }

    /// Why: Right-alignment of names is what makes the block read cleanly;
    /// verify the column equals the longest name regardless of insertion order.
    /// What: Asserts the name field width matches the longest name across rows.
    /// Test: This is the test.
    #[test]
    fn right_aligns_names_to_longest() {
        let (out, _cap) = Output::for_capture(Mode::Plain);
        let mut t = ComponentTracker::new(out);
        t.add(Component::new("a", 0))
            .add(Component::new("longest-name", 0));
        let lines: Vec<String> = t.render().lines().map(str::to_owned).collect();
        // Both name fields occupy the same right-aligned column → the byte
        // offset of " installed" must be identical on both rows.
        let off0 = lines[0].find(" installed").expect("verb on row 0");
        let off1 = lines[1].find(" installed").expect("verb on row 1");
        assert_eq!(off0, off1);
    }

    /// Why: An empty tracker must render nothing (no stray blank line) and
    /// `print` must be a clean no-op.
    /// What: Asserts empty render + empty capture.
    /// Test: This is the test.
    #[test]
    fn empty_tracker_renders_nothing() {
        let (out, cap) = Output::for_capture(Mode::Plain);
        let t = ComponentTracker::new(out);
        assert_eq!(t.render(), "");
        t.print().expect("print");
        assert_eq!(cap.contents(), "");
    }

    /// Why: `print` is the production path; confirm it writes the table plus a
    /// single trailing newline to the sink.
    /// What: Builds a captured tracker, prints, and asserts the bytes.
    /// Test: This is the test.
    #[test]
    fn print_writes_through_output() {
        let (out, cap) = Output::for_capture(Mode::Plain);
        let mut t = ComponentTracker::new(out);
        t.add(Component::new("cargo", 8_870_953));
        t.print().expect("print");
        // Single component → name column width == len("cargo") == 5, so no
        // intra-name padding; just INDENT + name + verb + right-aligned size.
        let expected = format!(
            "{indent}cargo installed{size:>width$}\n",
            indent = " ".repeat(INDENT),
            size = "8.46 MiB",
            width = SIZE_COLUMN_WIDTH,
        );
        assert_eq!(cap.contents(), expected);
    }

    /// Why: The non-TTY fallback must be plain text with zero ANSI escapes so
    /// piped/CI/`--json` consumers stay clean.
    /// What: Asserts the rendered plain table contains no ESC byte.
    /// Test: This is the test.
    #[test]
    fn plain_table_has_no_ansi_escapes() {
        let table = sample().render();
        assert!(!table.contains('\u{1b}'));
    }

    /// Why: Silent mode must suppress the table even when rows are present.
    /// What: Asserts nothing is written in silent mode.
    /// Test: This is the test.
    #[test]
    fn silent_mode_prints_nothing() {
        let (out, cap) = Output::for_capture(Mode::Silent);
        let mut t = ComponentTracker::new(out);
        t.add(Component::new("cargo", 8_870_953));
        t.print().expect("print");
        assert_eq!(cap.contents(), "");
    }
}
