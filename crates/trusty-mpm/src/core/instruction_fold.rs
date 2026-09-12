//! The compose-time instruction fold (#7616).
//!
//! Why: `instruction-compression` is measured as "sources read" minus "bytes
//! delivered", and until #7616 the composer performed no transformation at all
//! between the two — it concatenated the authored sections and appended the
//! generated roster, so the delivered prompt was always the sources PLUS
//! generated context. For a project that overrides no section the difference was
//! therefore structurally negative (the field measurement was 26,810 B of
//! sources against a 26,695 B prompt, 0.4%), and the producer declined every
//! row. A technique named "compression" that performs no compression is the
//! defect, not the reporting around it.
//!
//! What: [`fold_delivered_prompt`] is the one transformation between the
//! authored corpus and the bytes handed to `claude`. It removes only content
//! that carries no instruction: authoring metadata in HTML comments, trailing
//! whitespace, repeated blank lines, and the alignment padding inside Markdown
//! tables. It never rewrites a rule, never drops a table ROW, and never touches
//! anything inside a fenced code block.
//!
//! **What it deliberately does not do.** It does not reflow paragraphs, strip
//! issue citations, drop headings, or summarise. Every one of those changes what
//! the PM reads; this pass changes only what the PM's Markdown renderer would
//! discard anyway. The fold is small by construction — measured against the
//! bundled corpus it recovers roughly 500 B — and the honest majority of the
//! reduction for an override-free project comes from the roster dedup
//! (#4513) that [`crate::core::savings_instructions`] now accounts for.
//!
//! Test: the inline suite in `instruction_fold_tests.rs`.

/// Opening delimiter of an HTML comment.
const COMMENT_OPEN: &str = "<!--";
/// Closing delimiter of an HTML comment.
const COMMENT_CLOSE: &str = "-->";
/// Fenced-code-block delimiter prefix.
const FENCE: &str = "```";

/// Fold the composed PM prompt down to the bytes that carry instruction.
///
/// Why: see the module header — this is the compression step the
/// `instruction-compression` technique is named for, and before #7616 it did not
/// exist. Keeping it in one function means the packaged composer
/// ([`crate::core::instruction_package::InstructionPackage::compose`]) and the
/// legacy assembly ([`crate::core::instruction_overrides::assemble_sections`])
/// cannot deliver differently folded prompts, which is what
/// `composed_package_is_byte_identical_to_the_legacy_bundled_fallback` pins.
///
/// What: walks `text` line by line, tracking fenced-code and HTML-comment state.
/// Outside a fence it drops a line that opens an HTML comment (through the line
/// that closes it), trims trailing whitespace, collapses a run of blank lines to
/// one, and strips the alignment padding from an unindented Markdown table row.
/// Inside a fence every byte is emitted verbatim. A trailing newline on the
/// input is preserved on the output; trailing blank lines are not.
///
/// Determinism: pure function of `text` — no clock, no environment, no I/O. It
/// is also idempotent: `fold(fold(x)) == fold(x)`.
///
/// Test: `authoring_comments_are_not_delivered`,
/// `a_fenced_block_is_emitted_verbatim`, `table_padding_is_stripped`,
/// `blank_line_runs_collapse`, `the_fold_is_idempotent`,
/// `no_table_row_is_lost`.
pub(crate) fn fold_delivered_prompt(text: &str) -> String {
    let ends_with_newline = text.ends_with('\n');
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;
    let mut in_comment = false;

    for line in text.lines() {
        if line.trim_start().starts_with(FENCE) {
            in_fence = !in_fence;
            out.push(line.trim_end().to_string());
            continue;
        }
        if in_fence {
            out.push(line.to_string());
            continue;
        }
        if in_comment {
            // #7616: a block comment's continuation lines carry no instruction
            // either, so the whole block goes, not just its opening line.
            in_comment = !line.contains(COMMENT_CLOSE);
            continue;
        }

        let kept = line.trim_end();
        let body = kept.trim_start();

        if body.starts_with(COMMENT_OPEN) {
            in_comment = !body.contains(COMMENT_CLOSE);
            continue;
        }
        if body.is_empty() {
            // A leading blank, or a second consecutive one, carries nothing.
            if out.is_empty() || out.last().is_some_and(String::is_empty) {
                continue;
            }
            out.push(String::new());
            continue;
        }
        out.push(compact_row(kept));
    }

    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    let mut folded = out.join("\n");
    if ends_with_newline {
        folded.push('\n');
    }
    folded
}

/// Strip the alignment padding from an unindented Markdown table row.
///
/// Why (#7616): the bundled corpus's two tables — the Prohibitions and Circuit
/// Breakers tables in `enforcement.md` — carry 243 B of padding whose only
/// purpose is to make the authored file line up in a text editor. A Markdown
/// renderer discards it, so delivering it costs context for nothing. Every ROW
/// and every CELL survives; only the spaces between a cell's content and its
/// `|` go.
/// What: for a line that both starts and ends with `|` at column 0, splits on
/// `|`, trims each field, and rejoins with `|`. Split-and-join on the same
/// delimiter is lossless apart from the trimmed whitespace, so a cell containing
/// an escaped pipe round-trips unchanged. Any other line is returned as given —
/// an INDENTED table-shaped line is left alone, because indentation inside a
/// list item is structural.
/// Test: `table_padding_is_stripped`, `no_table_row_is_lost`,
/// `an_indented_table_row_keeps_its_indentation`.
fn compact_row(line: &str) -> String {
    if !line.starts_with('|') || !line.ends_with('|') || line.len() < 2 {
        return line.to_string();
    }
    line.split('|').map(str::trim).collect::<Vec<_>>().join("|")
}

#[cfg(test)]
#[path = "instruction_fold_tests.rs"]
mod tests;
