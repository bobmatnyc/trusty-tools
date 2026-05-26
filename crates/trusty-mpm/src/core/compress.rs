//! Tiered "caveman" token-use compression.
//!
//! Why: large tool outputs (multi-KB bash stdout, huge file reads) bloat the
//! context window; catching them at the PostToolUse relay and trimming before
//! they re-enter the session cuts token spend without LLM involvement.
//! What: four compression levels (Off/Trim/Summarise/Caveman) applied to raw
//! tool-output strings; a ToolOutputStats struct tracks savings per session.
//! Test: `cargo test -p trusty-mpm-core compress` exercises every level and
//! edge cases (empty input, under-threshold input, exact-boundary input).

/// Maximum bytes a tool output may occupy before compression is applied.
pub const TRIM_THRESHOLD_BYTES: usize = 4_096; // Level 1+
/// Smaller threshold used by the more aggressive `Summarise` level.
pub const SUMMARISE_THRESHOLD_BYTES: usize = 1_024; // Level 2+ (smaller threshold, more aggressive)

/// Lines kept at the head of a large output when trimming.
pub const TRIM_HEAD_LINES: usize = 20;
/// Lines kept at the tail of a large output when trimming.
pub const TRIM_TAIL_LINES: usize = 10;

/// Compression level applied to tool outputs.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    Default,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CompressionLevel {
    /// No compression — outputs pass through unchanged.
    #[default]
    Off,
    /// Trim large outputs to head+tail with a line-count note.
    Trim,
    /// Trim + collapse repeated blank lines + strip ANSI escape codes.
    Summarise,
    /// Keep only tool name, exit status, and a byte count. Drop all content.
    Caveman,
}

/// Byte savings recorded for one compression event.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct CompressionStats {
    /// Size of the tool output before compression, in bytes.
    pub original_bytes: usize,
    /// Size of the tool output after compression, in bytes.
    pub compressed_bytes: usize,
}

impl CompressionStats {
    /// Bytes saved by compression.
    ///
    /// Why: dashboards report cumulative savings; a saturating subtraction
    /// avoids underflow if a compressor ever grew the output.
    /// What: `original_bytes - compressed_bytes`, floored at 0.
    /// Test: `stats_saved_bytes_and_ratio`.
    pub fn saved_bytes(&self) -> usize {
        self.original_bytes.saturating_sub(self.compressed_bytes)
    }

    /// Compression ratio in `[0.0, 1.0+]` (compressed / original).
    ///
    /// Why: a single scalar summarises how aggressive a level was.
    /// What: returns 1.0 for empty input to avoid division by zero.
    /// Test: `stats_saved_bytes_and_ratio`.
    pub fn ratio(&self) -> f32 {
        if self.original_bytes == 0 {
            return 1.0;
        }
        self.compressed_bytes as f32 / self.original_bytes as f32
    }
}

/// Compress a tool output string at the given level.
///
/// Why: called in the daemon's PostToolUse handler on every event; pure so it
/// is testable without a running session.
/// What: applies the level's strategy; returns (compressed_text, stats).
/// Test: `compress_output_*` tests cover every level and threshold boundary.
pub fn compress_output(text: &str, level: CompressionLevel) -> (String, CompressionStats) {
    let original_bytes = text.len();
    let compressed = match level {
        CompressionLevel::Off => text.to_string(),
        CompressionLevel::Trim => trim_output(text, TRIM_THRESHOLD_BYTES),
        CompressionLevel::Summarise => {
            let stripped = strip_ansi(text);
            let collapsed = collapse_blank_lines(&stripped);
            trim_output(&collapsed, SUMMARISE_THRESHOLD_BYTES)
        }
        CompressionLevel::Caveman => caveman_summary("tool", original_bytes),
    };
    let stats = CompressionStats {
        original_bytes,
        compressed_bytes: compressed.len(),
    };
    (compressed, stats)
}

/// Trim a large output to its head and tail with an omission note.
///
/// Why: shared by the `Trim` and `Summarise` levels, which differ only in the
/// byte threshold at which trimming kicks in.
/// What: returns `text` unchanged if `text.len() <= threshold`; otherwise keeps
/// the first `TRIM_HEAD_LINES` and last `TRIM_TAIL_LINES` lines with a
/// `[... N lines omitted ...]` marker between them.
/// Test: exercised via `compress_trim_*` tests.
fn trim_output(text: &str, threshold: usize) -> String {
    if text.len() <= threshold {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= TRIM_HEAD_LINES + TRIM_TAIL_LINES {
        return text.to_string();
    }
    let skipped = lines.len() - TRIM_HEAD_LINES - TRIM_TAIL_LINES;
    let head = lines[..TRIM_HEAD_LINES].join("\n");
    let tail = lines[lines.len() - TRIM_TAIL_LINES..].join("\n");
    format!("{head}\n[... {skipped} lines omitted ...]\n{tail}")
}

/// Strip ANSI escape sequences from a string.
///
/// Why: bash output often contains colour codes that waste tokens without
/// conveying information to the LLM.
/// What: removes all ESC[...m sequences using a simple state machine.
/// Test: `strip_ansi_removes_codes`, `strip_ansi_leaves_plain_text`.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC: an ANSI sequence may follow. CSI sequences open with '['
            // and end at a byte in the range 0x40..=0x7e (e.g. 'm').
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                for seq in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&seq) {
                        break;
                    }
                }
            }
            // A bare ESC with no '[' is simply dropped.
            continue;
        }
        out.push(c);
    }
    out
}

/// Collapse runs of blank lines into a single blank line.
///
/// Why: file reads and compiler output often have many consecutive blank lines.
/// What: replaces 2+ consecutive blank lines with a single blank line.
/// Test: `collapse_blanks_*`.
pub fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0usize;
    let mut first = true;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        if !first {
            out.push('\n');
        }
        out.push_str(line);
        first = false;
    }
    // Preserve a trailing newline if the input ended with one.
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Build a caveman summary for a tool output.
///
/// Why: level-3 compression drops all content; callers need a human-readable
/// placeholder so the LLM knows a tool ran and roughly how much output it had.
/// What: single line: `[{tool_name}: {byte_count}B of output suppressed]`.
/// Test: `caveman_summary_contains_tool_and_size`.
pub fn caveman_summary(tool_name: &str, original_bytes: usize) -> String {
    format!("[{tool_name}: {original_bytes}B of output suppressed]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_lines(n: usize) -> String {
        (0..n)
            .map(|i| format!("line {i} with some padding content"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn compress_off_is_passthrough() {
        let input = big_lines(500);
        let (out, stats) = compress_output(&input, CompressionLevel::Off);
        assert_eq!(out, input);
        assert_eq!(stats.original_bytes, stats.compressed_bytes);
    }

    #[test]
    fn compress_trim_below_threshold_unchanged() {
        let input = "short output\nsecond line";
        let (out, stats) = compress_output(input, CompressionLevel::Trim);
        assert_eq!(out, input);
        assert_eq!(stats.saved_bytes(), 0);
    }

    #[test]
    fn compress_trim_above_threshold_truncates() {
        let input = big_lines(500);
        assert!(input.len() > TRIM_THRESHOLD_BYTES);
        let (out, stats) = compress_output(&input, CompressionLevel::Trim);
        assert!(out.len() < input.len());
        assert!(stats.compressed_bytes < stats.original_bytes);
    }

    #[test]
    fn compress_trim_includes_omitted_count() {
        let input = big_lines(500);
        let (out, _) = compress_output(&input, CompressionLevel::Trim);
        let expected_skipped = 500 - TRIM_HEAD_LINES - TRIM_TAIL_LINES;
        assert!(out.contains(&format!("[... {expected_skipped} lines omitted ...]")));
    }

    #[test]
    fn compress_summarise_strips_ansi() {
        let mut input = String::from("\u{1b}[31mred text\u{1b}[0m\n");
        input.push_str(&big_lines(100));
        let (out, _) = compress_output(&input, CompressionLevel::Summarise);
        assert!(!out.contains('\u{1b}'));
        assert!(out.contains("red text"));
    }

    #[test]
    fn compress_summarise_collapses_blanks() {
        let mut input = String::from("alpha\n\n\n\n\nbeta\n");
        input.push_str(&big_lines(100));
        let (out, _) = compress_output(&input, CompressionLevel::Summarise);
        assert!(!out.contains("\n\n\n"));
    }

    #[test]
    fn compress_caveman_always_shrinks() {
        let input = big_lines(500);
        let (out, stats) = compress_output(&input, CompressionLevel::Caveman);
        assert!(out.len() < input.len());
        assert!(stats.compressed_bytes < stats.original_bytes);
    }

    #[test]
    fn compress_caveman_summary_contains_byte_count() {
        let input = "exactly twenty bytes";
        let (out, _) = compress_output(input, CompressionLevel::Caveman);
        assert!(out.contains(&format!("{}B", input.len())));
    }

    #[test]
    fn strip_ansi_removes_codes() {
        let input = "\u{1b}[1;32mgreen\u{1b}[0m and \u{1b}[31mred\u{1b}[0m";
        assert_eq!(strip_ansi(input), "green and red");
    }

    #[test]
    fn strip_ansi_leaves_plain_text_unchanged() {
        let input = "no escape codes here, just text";
        assert_eq!(strip_ansi(input), input);
    }

    #[test]
    fn collapse_blanks_reduces_runs() {
        let input = "a\n\n\n\nb";
        assert_eq!(collapse_blank_lines(input), "a\n\nb");
    }

    #[test]
    fn collapse_blanks_single_blank_unchanged() {
        let input = "a\n\nb";
        assert_eq!(collapse_blank_lines(input), "a\n\nb");
    }

    #[test]
    fn caveman_summary_contains_tool_name() {
        let s = caveman_summary("Bash", 1234);
        assert!(s.contains("Bash"));
    }

    #[test]
    fn caveman_summary_contains_byte_count() {
        let s = caveman_summary("Read", 4096);
        assert!(s.contains("4096B"));
    }

    #[test]
    fn stats_saved_bytes_and_ratio() {
        let stats = CompressionStats {
            original_bytes: 1000,
            compressed_bytes: 250,
        };
        assert_eq!(stats.saved_bytes(), 750);
        assert!((stats.ratio() - 0.25).abs() < f32::EPSILON);

        // Empty input: ratio is defined as 1.0, no underflow.
        let empty = CompressionStats::default();
        assert_eq!(empty.saved_bytes(), 0);
        assert!((empty.ratio() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn compress_trim_exact_boundary_unchanged() {
        // An output exactly at the threshold must not be trimmed.
        let input = "x".repeat(TRIM_THRESHOLD_BYTES);
        let (out, stats) = compress_output(&input, CompressionLevel::Trim);
        assert_eq!(out, input);
        assert_eq!(stats.saved_bytes(), 0);
    }

    #[test]
    fn compress_empty_input_all_levels() {
        for level in [
            CompressionLevel::Off,
            CompressionLevel::Trim,
            CompressionLevel::Summarise,
            CompressionLevel::Caveman,
        ] {
            let (_out, stats) = compress_output("", level);
            assert_eq!(stats.original_bytes, 0);
        }
    }
}
