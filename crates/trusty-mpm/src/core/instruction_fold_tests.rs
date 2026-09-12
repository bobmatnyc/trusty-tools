//! Tests for the compose-time instruction fold (#7616).
//!
//! Why: the fold is the one place trusty-mpm rewrites the bytes it delivers to
//! the PM, so every test here is about what must SURVIVE it, not only about what
//! it removes. A fold that loses a table row or a rule is worse than no fold.
//! What: the removal cases (comments, padding, blank runs), the preservation
//! cases (fenced blocks, every table row, indentation), idempotence, and the
//! corpus-level assertion that the bundled sections actually shrink.
//! Test: this file IS the suite.

use super::*;
use crate::core::instruction_pipeline::SECTION_SOURCES;

#[test]
fn authoring_comments_are_not_delivered() {
    // `core.md` opens with a version stamp and a two-line PURPOSE block. Both
    // are notes to whoever edits the file; neither is an instruction.
    let src = "<!-- PM_INSTRUCTIONS_VERSION: 0024 -->\n\
               <!-- PURPOSE: notes for the\n\
               \x20    author only. -->\n\
               # PM Agent\n\n\
               Delegate by default.\n";

    let folded = fold_delivered_prompt(src);

    assert_eq!(folded, "# PM Agent\n\nDelegate by default.\n");
}

#[test]
fn a_fenced_block_is_emitted_verbatim() {
    // The override-marker grammar is taught inside a fence, and a fence is the
    // one place an HTML comment IS content. Stripping it there would delete the
    // syntax a project needs to customize anything.
    let src = "Use this:\n\
               ```\n\
               <!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n\
               |  padded  |  cell  |\n\
               ```\n\
               after\n";

    let folded = fold_delivered_prompt(src);

    assert!(
        folded.contains("<!-- TRUSTY-MPM: WORKFLOW START v=1 -->"),
        "a comment inside a fence is content: {folded}"
    );
    assert!(
        folded.contains("|  padded  |  cell  |"),
        "a fenced table row keeps its padding: {folded}"
    );
}

/// #7616 REGRESSION: a fence-shaped line inside an OPEN HTML comment must not
/// desynchronise the parser.
///
/// Why this is the worst shape the fold can get wrong: it fails in both
/// directions at once. Testing the fence before the comment flipped `in_fence`
/// on a line that was really comment content, so the comment's own text and its
/// `-->` leaked into the delivered prompt — and when that phantom fence toggled
/// back off, the parser was still `in_comment` and silently DELETED real
/// instruction lines until the next `-->`. `assemble_sections` folds the joined
/// output including a project's `CLAUDE.md` override bodies, so a project author
/// documenting fence syntax inside a multi-line comment would lose real rules
/// from every PM prompt, with nothing anywhere saying so.
/// FAILS BEFORE THIS CHANGE: `hidden` and `-->` appear in the output and
/// `CODE INSIDE A REAL FENCE` is gone.
/// Test: itself.
#[test]
fn a_fence_inside_an_open_comment_does_not_desynchronise_the_parser() {
    let src = "<!-- example:\n\
               ```\n\
               hidden\n\
               -->\n\
               REAL RULE: never do X.\n\
               ```\n\
               CODE INSIDE A REAL FENCE\n\
               ```\n\
               AFTER\n";

    let folded = fold_delivered_prompt(src);

    assert!(
        !folded.contains("hidden"),
        "comment content leaked into the delivered prompt: {folded}"
    );
    assert!(
        !folded.contains(COMMENT_CLOSE),
        "the comment's closing delimiter leaked: {folded}"
    );
    for survivor in [
        "REAL RULE: never do X.",
        "CODE INSIDE A REAL FENCE",
        "AFTER",
    ] {
        assert!(
            folded.contains(survivor),
            "the fold deleted the real line {survivor:?}: {folded}"
        );
    }
    assert_eq!(
        folded,
        "REAL RULE: never do X.\n```\nCODE INSIDE A REAL FENCE\n```\nAFTER\n"
    );
}

// ---------------------------------------------------------------------------
// #7616 — whole-line semantics.
//
// A line whose comment runs to the end of the line is dropped. A line that
// MIXES comment and content is delivered VERBATIM — no strip, no compaction, no
// trailing-whitespace trim. Three review rounds of partial-line handling each
// produced a new interaction (fence state, table-row indentation, the
// continuation branch), so the fold now edits only lines it can classify with
// certainty and delivers the rest untouched.
// ---------------------------------------------------------------------------

/// #7616: content after a same-line close survives, as the whole line.
///
/// Why verbatim rather than the content alone: handing a stripped remainder back
/// to the pipeline is what produced three rounds of findings. Delivering the
/// physical line costs the comment's own bytes and cannot lose a rule.
/// Test: itself.
#[test]
fn content_after_a_same_line_comment_close_survives() {
    let folded = fold_delivered_prompt("<!-- note -->REAL CONTENT HERE\nafter\n");
    assert_eq!(folded, "<!-- note -->REAL CONTENT HERE\nafter\n");
}

/// #7616: two complete spans on one line — the whole line is delivered, so the
/// text between them cannot be lost.
/// Test: itself.
#[test]
fn two_comments_on_one_line_keep_the_text_between_them() {
    let folded = fold_delivered_prompt("<!-- a --> text <!-- b -->\n");
    assert_eq!(folded, "<!-- a --> text <!-- b -->\n");
}

/// #7616: the accepted cost of classifying on the FIRST `-->`.
///
/// Why: a line of two adjacent spans carries no content, yet the first close is
/// not the end of the line, so it is delivered. Classifying on the LAST `-->`
/// would drop it — and would also drop `<!-- a --> text <!-- b -->`, deleting
/// ` text `. Keeping a few comment bytes is the safe side of that trade, and
/// this test exists so the cost is visible rather than discovered.
/// Test: itself.
#[test]
fn a_line_of_two_comment_spans_is_delivered_rather_than_risk_deleting_content() {
    let folded = fold_delivered_prompt("<!-- a --><!-- b -->\n");
    assert_eq!(folded, "<!-- a --><!-- b -->\n");
}

/// #7616: the ordinary case must not regress — a line that is ONLY a comment
/// still vanishes, and leaves no blank line behind where it stood.
///
/// Why this is asserted separately: it is the branch the three bundled goldens
/// depend on. An implementation that emitted an empty string here would insert a
/// blank at every authoring comment and change all three.
/// Test: itself.
#[test]
fn a_line_that_is_only_a_comment_leaves_no_blank_behind() {
    let folded = fold_delivered_prompt("before\n<!-- note -->\nafter\n");
    assert_eq!(folded, "before\nafter\n");
}

/// #7616 REGRESSION (critic input 1): a mixed comment/fence line is delivered
/// verbatim and does NOT open a fence.
///
/// Why the fence state matters: the delivered text ends in a fence marker, but
/// the LINE does not satisfy `trim_start().starts_with("```")`, so `in_fence`
/// stays false. The row after it is therefore outside a fence and compacts; the
/// next bare fence marker opens one, and the row inside it is verbatim. That is
/// the contract, pinned here rather than left to be rediscovered.
/// FAILS BEFORE THIS CHANGE: the stripped remainder was compacted through
/// `compact_row` without toggling fence state, so the fenced row was compacted
/// and the unfenced one was left padded.
/// Test: itself.
#[test]
fn a_mixed_comment_and_fence_line_does_not_open_a_fence() {
    let src = "<!-- lang -->```\n\
               |  a  |  b  |\n\
               ```\n\
               |  c  |  d  |\n";

    let folded = fold_delivered_prompt(src);

    assert_eq!(
        folded,
        "<!-- lang -->```\n\
         |a|b|\n\
         ```\n\
         |  c  |  d  |\n"
    );
}

/// #7616 REGRESSION (critic input 2): the line that CLOSES an open comment is
/// delivered verbatim when content follows the close.
///
/// FAILS BEFORE THIS CHANGE: the continuation branch dropped the whole closing
/// line, so the output was `\n`.
/// Test: itself.
#[test]
fn a_closing_line_that_carries_content_is_delivered_verbatim() {
    let folded = fold_delivered_prompt("<!-- open\nstuff\nclose --> REAL CONTENT\n");
    assert_eq!(folded, "close --> REAL CONTENT\n");
}

/// #7616 REGRESSION (critic input 3): an indented table row on a mixed line
/// keeps its indentation.
///
/// Why: `compact_row` exempts an INDENTED table-shaped line, because indentation
/// inside a list item is structural. The partial-line path trimmed the remainder
/// before handing it over, which silently removed that exemption.
/// FAILS BEFORE THIS CHANGE: the row came back as `|a|b|`.
/// Test: itself.
#[test]
fn an_indented_table_row_after_a_comment_is_delivered_verbatim() {
    let folded = fold_delivered_prompt("<!-- c -->  | a | b |\n");
    assert_eq!(folded, "<!-- c -->  | a | b |\n");
}

/// #7616: state pair — an open comment that never closes swallows the rest of
/// the input, including fence-shaped lines, and delivers nothing.
/// Test: itself.
#[test]
fn an_unclosed_comment_swallows_the_rest_of_the_input() {
    let folded = fold_delivered_prompt("real\n<!-- open\n```\nnever closed\n");
    assert_eq!(folded, "real\n");
}

/// #7616: state pair — a comment-shaped line INSIDE a fence is content, so it is
/// emitted verbatim and never opens comment state.
/// Test: itself.
#[test]
fn a_comment_inside_a_fence_is_content_not_comment_state() {
    let src = "```\n<!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n```\nafter\n";
    assert_eq!(fold_delivered_prompt(src), src);
}

#[test]
fn table_padding_is_stripped() {
    let src = "| # | Forbidden Action | CB# |\n\
               |---|-----------------|-----|\n\
               | P1 | Edit of source  | 1   |\n";

    let folded = fold_delivered_prompt(src);

    assert_eq!(
        folded,
        "|#|Forbidden Action|CB#|\n|---|-----------------|-----|\n|P1|Edit of source|1|\n"
    );
}

#[test]
fn no_table_row_is_lost() {
    // The brief's hard constraint: P1-P11 and every circuit-breaker row must
    // survive the fold. Row COUNT is the mechanical form of that guarantee.
    for (path, body) in SECTION_SOURCES {
        let rows = |text: &str| {
            text.lines()
                .filter(|l| l.trim_start().starts_with('|'))
                .count()
        };
        assert_eq!(
            rows(&fold_delivered_prompt(body)),
            rows(body),
            "{path} lost a table row to the fold"
        );
    }
}

#[test]
fn every_prohibition_and_circuit_breaker_row_survives() {
    // Named rather than counted: a row renamed away is as bad as one deleted.
    let enforcement = SECTION_SOURCES
        .iter()
        .find(|(path, _)| *path == "sections/enforcement.md")
        .map(|(_, body)| *body)
        .expect("enforcement section is bundled");
    let folded = fold_delivered_prompt(enforcement);

    for n in 1..=11 {
        assert!(
            folded.contains(&format!("|P{n}|")),
            "prohibition P{n} is missing from the folded section"
        );
    }
    for n in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 14] {
        assert!(
            folded.contains(&format!("|{n}|")),
            "circuit breaker CB#{n} is missing from the folded section"
        );
    }
}

#[test]
fn every_skill_pointer_survives() {
    // Progressive disclosure is the corpus's whole design: a dropped pointer
    // silently removes the PM's access to a skill's contents.
    for (path, body) in SECTION_SOURCES {
        let pointers = |text: &str| text.matches("Skill(skill=").count();
        assert_eq!(
            pointers(&fold_delivered_prompt(body)),
            pointers(body),
            "{path} lost a Skill() pointer to the fold"
        );
    }
}

#[test]
fn blank_line_runs_collapse() {
    let folded = fold_delivered_prompt("a\n\n\n\n\nb\n");
    assert_eq!(folded, "a\n\nb\n");
}

#[test]
fn leading_and_trailing_blanks_go() {
    let folded = fold_delivered_prompt("\n\n a \n\n\n");
    assert_eq!(folded, " a\n");
}

#[test]
fn an_indented_table_row_keeps_its_indentation() {
    // Indentation inside a list item is structural, so a table-shaped line that
    // is indented is left exactly as authored.
    let src = "- item:\n  | a | b |\n";
    assert_eq!(fold_delivered_prompt(src), src);
}

#[test]
fn the_fold_is_idempotent() {
    for (path, body) in SECTION_SOURCES {
        let once = fold_delivered_prompt(body);
        assert_eq!(
            fold_delivered_prompt(&once),
            once,
            "{path} folds differently the second time"
        );
    }
}

#[test]
fn the_fold_never_grows_a_section() {
    for (path, body) in SECTION_SOURCES {
        assert!(
            fold_delivered_prompt(body).len() <= body.len(),
            "{path} grew under the fold"
        );
    }
}

/// #7616: the corpus-level claim. Before the fold existed this was 0 B.
#[test]
fn the_bundled_corpus_actually_shrinks() {
    let authored: usize = SECTION_SOURCES.iter().map(|(_, body)| body.len()).sum();
    let folded: usize = SECTION_SOURCES
        .iter()
        .map(|(_, body)| fold_delivered_prompt(body).len())
        .sum();

    assert!(
        folded < authored,
        "the fold recovered nothing: authored {authored} B, folded {folded} B"
    );
}
