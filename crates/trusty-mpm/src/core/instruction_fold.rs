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
//! **Whole-line semantics.** It drops only lines whose comment runs to the end
//! of the line; any line MIXING comment and content is delivered untouched —
//! verbatim, with no strip, no compaction and no trailing-whitespace trim. That
//! is the rule the bundled corpus and the three golden prompts actually need,
//! and it is deliberately blunter than the partial-line handling it replaces:
//! three review rounds showed that every attempt to strip a comment span and
//! feed the remainder back through the pipeline created a new interaction with
//! fence state, table-row indentation, or the continuation branch. A line the
//! fold cannot classify with certainty is delivered, never edited.
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

/// An open fenced code block: its marker character and run length.
///
/// Why (#8533): CommonMark closes a fence only with the character that opened
/// it, repeated at least as often. Counting lines that start with three
/// backticks mispaired a `~~~` fence and a four-backtick fence around a
/// three-backtick one.
/// Test: `tilde_and_long_backtick_fences_pair_by_character_and_length`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fence {
    marker: char,
    len: usize,
}

impl Fence {
    /// The fence `line` opens: three or more backticks or tildes after any
    /// indentation; a backtick fence's info string holds no backtick.
    fn opened_by(line: &str) -> Option<Fence> {
        let body = line.trim_start();
        let marker = body.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let len = body.chars().take_while(|&c| c == marker).count();
        let info = &body[len..];
        (len >= 3 && !(marker == '`' && info.contains('`'))).then_some(Fence { marker, len })
    }

    /// Whether `line` closes this fence: only the same character, at least as
    /// many of it, and whitespace.
    fn closed_by(self, line: &str) -> bool {
        let body = line.trim();
        body.len() >= self.len && body.chars().all(|c| c == self.marker)
    }

    /// The line that closes this fence.
    pub(crate) fn closing_line(self) -> String {
        self.marker.to_string().repeat(self.len)
    }
}

/// Advance the fence state `open` over `line`; `true` when `line` opens or
/// closes a fence.
///
/// Test: `tilde_and_long_backtick_fences_pair_by_character_and_length`.
pub(crate) fn step_fence(open: &mut Option<Fence>, line: &str) -> bool {
    match *open {
        Some(fence) if fence.closed_by(line) => {
            *open = None;
            true
        }
        Some(_) => false,
        None => {
            *open = Fence::opened_by(line);
            open.is_some()
        }
    }
}

/// The fence `text` leaves open at its end, if any.
pub(crate) fn open_fence_at_end(text: &str) -> Option<Fence> {
    let mut open = None;
    for line in text.lines() {
        step_fence(&mut open, line);
    }
    open
}

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
/// What: walks `text` line by line, tracking HTML-comment state FIRST and
/// fenced-code state second — an open comment swallows a fence-shaped line as
/// comment content, and testing the fence first corrupted the parser both ways
/// (leaked comment text, then deleted real lines). Outside a comment and a
/// fence it classifies a comment-opening line with [`after_open_close`] — the
/// comment runs to the end of the line, so the line is dropped; content follows
/// the close, so the line is delivered VERBATIM; the comment is still open, so
/// the following lines belong to it — then trims trailing whitespace, collapses
/// a run of blank lines to one, and strips the alignment padding from an
/// unindented Markdown table row.
/// Inside a fence every byte is emitted verbatim. A trailing newline on the
/// input is preserved on the output; trailing blank lines are not.
///
/// A delivered mixed line never reaches [`compact_row`] and never toggles fence
/// state: it is pushed exactly as authored. `<!-- lang -->` followed by a fence
/// marker is therefore NOT a fence open, because the delivered line does not
/// open a fence under [`step_fence`] — pinned by
/// `a_mixed_comment_and_fence_line_does_not_open_a_fence`.
///
/// Determinism: pure function of `text` — no clock, no environment, no I/O. It
/// is also idempotent: `fold(fold(x)) == fold(x)`.
///
/// Test: `authoring_comments_are_not_delivered`,
/// `a_fenced_block_is_emitted_verbatim`, `table_padding_is_stripped`,
/// `blank_line_runs_collapse`, `the_fold_is_idempotent`,
/// `no_table_row_is_lost`,
/// `a_fence_inside_an_open_comment_does_not_desynchronise_the_parser`,
/// `content_after_a_same_line_comment_close_survives`,
/// `a_mixed_comment_and_fence_line_does_not_open_a_fence`,
/// `a_closing_line_that_carries_content_is_delivered_verbatim`,
/// `an_indented_table_row_after_a_comment_is_delivered_verbatim`.
pub(crate) fn fold_delivered_prompt(text: &str) -> String {
    let ends_with_newline = text.ends_with('\n');
    let mut out: Vec<String> = Vec::new();
    let mut fence: Option<Fence> = None;
    let mut in_comment = false;

    for line in text.lines() {
        // #7616: comment state is checked BEFORE fence state, and the order is
        // load-bearing. An open HTML comment swallows a fence-shaped line as
        // comment content; testing the fence first let that line flip `in_fence`
        // while `in_comment` stayed true, so the comment's own `-->` and the
        // real lines after it leaked into the prompt verbatim, and when the
        // fence toggled back off the parser was still inside the comment and
        // DELETED real instruction lines until the next `-->`.
        if in_comment {
            // #7616: the open block ends at the first `-->`. Nothing follows it
            // on this line — drop the line; something does — the line MIXES
            // comment and content, so it is delivered untouched.
            let Some(tail) = after_open_close(line) else {
                continue;
            };
            in_comment = false;
            if !tail.trim().is_empty() {
                out.push(line.to_string());
            }
            continue;
        }
        // #8533: a fence closes only on its own character and length.
        if step_fence(&mut fence, line) {
            out.push(line.trim_end().to_string());
            continue;
        }
        if fence.is_some() {
            out.push(line.to_string());
            continue;
        }

        let kept = line.trim_end();
        let body = kept.trim_start();

        if body.starts_with(COMMENT_OPEN) {
            // #7616: same three-way decision as the continuation above.
            match after_open_close(body) {
                None => in_comment = true,
                Some(tail) if tail.trim().is_empty() => {}
                Some(_) => out.push(line.to_string()),
            }
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

/// Fold each part ON ITS OWN, then join the non-empty results with `separator`.
///
/// Why (#8533): the roster-absent string assembly carries project override
/// bodies beside base text. Folded as one string, a body ending in an unclosed
/// `<!--` hid every later part up to the next `-->` — the agent-selection note,
/// the roster and the enforcement tables included. Folding each part alone
/// confines a comment or fence to the part that opened it, as
/// [`crate::core::instruction_package::InstructionPackage::compose`] does per block.
/// What: trims each part and runs it through [`fold_block`], drops the ones the
/// fold empties, joins the rest. A part is never folded twice, so no refold can
/// re-pair state.
/// Test: `an_unclosed_comment_in_one_part_hides_nothing_in_the_next`.
pub(crate) fn fold_parts<S: AsRef<str>>(parts: &[S], separator: &str) -> String {
    parts
        .iter()
        .map(|part| fold_block(part.as_ref().trim()))
        .filter(|folded| !folded.is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

/// Fold one block on its own and close the fence it leaves open.
///
/// Why (#8533): the next block is joined straight after this one. A project
/// body that opens a fence and never closes it turned every later block into
/// code, the safety core included. Folding per block confines a comment to its
/// block; closing the fence here does the same for a fence.
/// What: [`fold_delivered_prompt`], then, when a fence is still open, a line
/// closing it with the same character and length.
/// Test: `a_block_closes_its_own_tilde_or_long_backtick_fence`,
/// `an_override_ending_in_an_unclosed_fence_closes_it_in_its_own_block`.
pub(crate) fn fold_block(text: &str) -> String {
    let mut folded = fold_delivered_prompt(text);
    if let Some(fence) = open_fence_at_end(&folded) {
        if !folded.ends_with('\n') {
            folded.push('\n');
        }
        folded.push_str(&fence.closing_line());
    }
    folded
}

/// What follows the FIRST `-->` on a line, or `None` when the line has none.
///
/// Why (#7616): this is the whole-line classifier, and it replaces a
/// partial-line one that three review rounds could not make safe. Each attempt
/// to strip a span and hand the remainder back to the rest of the pipeline
/// created a new interaction — a stripped remainder that was fence-shaped never
/// toggled fence state, a trimmed remainder lost the indentation
/// [`compact_row`] keys its table-row exemption on, and the continuation branch
/// still dropped a closing line that carried content. The classifier answers one
/// question instead: does this line's comment run to the end of the line?
///
/// What: the substring after the first `-->`. An empty-or-whitespace answer
/// means the line is comment through to its end and is dropped; a non-empty one
/// means the line MIXES comment and content, and the caller delivers it
/// verbatim — no strip, no compaction, no trailing-whitespace trim. `None` means
/// the comment is still open and the following lines belong to it.
///
/// **FIRST, not last, and that choice is content-preserving.** With the LAST
/// `-->`, `<!-- a --> text <!-- b -->` classifies as comment-to-end-of-line and
/// ` text ` is deleted — the exact defect class this issue is about. With the
/// first, that line is mixed and survives untouched. The cost is the reverse
/// case, `<!-- a --><!-- b -->`, which is pure comment yet is delivered: a few
/// bytes kept, never a rule lost. That trade is deliberate and pinned by
/// `a_line_of_two_comment_spans_is_delivered_rather_than_risk_deleting_content`.
///
/// Test: `content_after_a_same_line_comment_close_survives`,
/// `two_comments_on_one_line_keep_the_text_between_them`,
/// `a_line_that_is_only_a_comment_leaves_no_blank_behind`,
/// `a_closing_line_that_carries_content_is_delivered_verbatim`.
fn after_open_close(line: &str) -> Option<&str> {
    line.find(COMMENT_CLOSE)
        .map(|at| &line[at + COMMENT_CLOSE.len()..])
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
