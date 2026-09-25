//! The floor appended to a project output style (#8533, owner ruling 2026-09-25).
//!
//! Why: a project output style replaces the bundled style's voice, and with it
//! the PRIMARY DIRECTIVE and the "Communication — Write Plainly" rules. The PM
//! prompt's "Prose Style — Write Plainly" section points at that output-style
//! section, so under a project style the pointer named nothing. A project style
//! keeps its prose as authored; the floor is appended to it.
//! What: [`style_floor`] cuts the floor sections out of the bundled
//! `trusty-mpm` style — the one source, never a copy. [`delivered_style_text`]
//! is a style's body as delivered: a bundled style unchanged (each carries its
//! own floor), a project style folded on its own and followed by the floor.
//! [`floor_for`] is the floor alone, for the native path where Claude Code
//! reads the style file itself.
//! Test: `output_style_floor_tests.rs`.

use super::{ActiveStyle, strip_frontmatter};
use crate::core::bundle::OUTPUT_STYLE;
use crate::core::instruction_pipeline::SECTION_SEPARATOR;

/// Headings of the bundled `trusty-mpm` style sections that form the floor.
pub const FLOOR_SECTIONS: [&str; 2] = [
    "## 🔴 PRIMARY DIRECTIVE — MANDATORY DELEGATION",
    "## Communication — Write Plainly",
];

/// Heading of the floor block appended to a project output style.
pub const STYLE_FLOOR_HEADING: &str =
    "# trusty-mpm Floor (appended to the project output style; not overridable)";

/// The floor block: [`STYLE_FLOOR_HEADING`], then each [`FLOOR_SECTIONS`]
/// section as the bundled `trusty-mpm` style carries it.
///
/// Why: one source for the floor text — an edit to the bundled style reaches
/// every project style on the next launch.
/// What: each section runs from its heading to the next `#` or `##` heading.
/// Test: `the_floor_is_cut_from_the_bundled_style`.
pub fn style_floor() -> String {
    let sections: Vec<&str> = FLOOR_SECTIONS
        .iter()
        .filter_map(|heading| section(OUTPUT_STYLE, heading))
        .collect();
    format!("{STYLE_FLOOR_HEADING}\n\n{}", sections.join("\n\n"))
}

/// The `heading` section of `doc`, heading included, trimmed.
fn section<'a>(doc: &'a str, heading: &str) -> Option<&'a str> {
    let start = doc.find(&format!("\n{heading}\n"))? + 1;
    let body = start + heading.len();
    let end = doc[body..]
        .match_indices('\n')
        .map(|(i, _)| body + i + 1)
        .find(|&i| doc[i..].starts_with("# ") || doc[i..].starts_with("## "))
        .unwrap_or(doc.len());
    Some(doc[start..end].trim())
}

/// The floor `style` needs appended: `Some` for a project style, `None` for a
/// bundled one, which carries its own PRIMARY DIRECTIVE and Write Plainly
/// sections.
///
/// Test: `the_bundled_styles_get_no_appended_floor`.
pub fn floor_for(style: &ActiveStyle) -> Option<String> {
    match style {
        ActiveStyle::Bundled(_) => None,
        ActiveStyle::Project { .. } => Some(style_floor()),
    }
}

/// `style`'s body as delivered: frontmatter stripped, the floor appended to a
/// project style.
///
/// Why: the project prose must not be able to hide the floor. It is folded on
/// its own, as an override body is (#8533 finding 1), so a trailing unclosed
/// `<!--` ends with the prose; an unclosed fence is closed.
/// What: a bundled style → its body. A project style → the folded prose, the
/// section separator, then [`style_floor`]; the floor alone when the fold
/// leaves no prose. The floor is appended whatever the prose claims, so a
/// heading spoofing the floor adds nothing and removes nothing.
/// Test: `a_project_style_is_delivered_as_its_prose_plus_the_floor_once`,
/// `the_floor_survives_an_unclosed_comment_or_fence_and_a_spoofed_heading`.
pub fn delivered_style_text(style: &ActiveStyle) -> String {
    let body = strip_frontmatter(style.content()).trim();
    let Some(floor) = floor_for(style) else {
        return body.to_string();
    };
    let prose = isolate(body);
    if prose.is_empty() {
        floor
    } else {
        format!("{prose}{SECTION_SEPARATOR}{floor}")
    }
}

/// Fold `prose` on its own and close a fence it leaves open.
fn isolate(prose: &str) -> String {
    let mut folded = crate::core::instruction_fold::fold_delivered_prompt(prose);
    let fences = folded
        .lines()
        .filter(|l| l.trim_start().starts_with("```"))
        .count();
    if fences % 2 == 1 {
        folded.push_str("\n```");
    }
    folded
}

#[cfg(test)]
#[path = "output_style_floor_tests.rs"]
mod tests;
