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

/// #7616 REGRESSION: a comment that CLOSES on its own line must not take the
/// real content that follows it with it.
///
/// Why: the fold dropped the whole physical line whenever it began with `<!--`,
/// so `<!-- note -->REAL CONTENT HERE` delivered nothing at all. The parser did
/// not desynchronise — the damage is bounded to that one line — and no bundled
/// section or `CLAUDE.md` carries the shape today. It still breaks the fold's
/// only claim, that it removes nothing which carries instruction.
/// FAILS BEFORE THIS CHANGE: the output is `after\n`.
/// Test: itself.
#[test]
fn content_after_a_same_line_comment_close_survives() {
    let folded = fold_delivered_prompt("<!-- note -->REAL CONTENT HERE\nafter\n");
    assert_eq!(folded, "REAL CONTENT HERE\nafter\n");
}

/// #7616: two complete spans on one line — the text BETWEEN them survives, and
/// the trailing span is left in place.
///
/// Why the trailing span stays: stripping every span anywhere in a line would
/// delete the marker grammar from any override body that teaches it inside
/// backticks, which is a rule loss. Leading spans are unambiguous; a mid-line
/// one is not worth the risk for the bytes it saves. So the contract is
/// asymmetric on purpose, and this test states the asymmetry rather than
/// leaving it to be discovered.
/// Test: itself.
#[test]
fn two_comments_on_one_line_keep_the_text_between_them() {
    let folded = fold_delivered_prompt("<!-- a --> text <!-- b -->\n");
    assert_eq!(folded, "text <!-- b -->\n");
}

/// #7616: the ordinary case must not regress — a line that is ONLY a comment
/// still vanishes, and leaves no blank line behind where it stood.
///
/// Why this is asserted separately: the same-line-close fix routes that line
/// through a new path, and the obvious implementation of it emits an empty
/// string, which would insert a blank into every delivered prompt at each
/// authoring comment and change all three goldens.
/// Test: itself.
#[test]
fn a_line_that_is_only_a_comment_leaves_no_blank_behind() {
    let folded = fold_delivered_prompt("before\n<!-- note -->\nafter\n");
    assert_eq!(folded, "before\nafter\n");
}

/// #7616: an UNTERMINATED span on a line that also carries earlier complete
/// spans still opens comment state, so the lines after it are consumed to the
/// close rather than leaking.
/// Test: itself.
#[test]
fn an_unterminated_span_after_a_closed_one_still_opens_comment_state() {
    let folded = fold_delivered_prompt("<!-- a --><!-- open\nswallowed\n-->\nreal\n");
    assert_eq!(folded, "real\n");
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
