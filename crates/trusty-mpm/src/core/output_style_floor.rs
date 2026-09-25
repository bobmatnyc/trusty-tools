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
//! For the native path, where Claude Code reads the style file itself,
//! [`native_style_id`] writes a composite style — the project prose, then the
//! floor — and names it in `outputStyle`, so a bare `claude` launch gets the
//! floor too; [`floor_for`] is the floor alone, for a tm launch whose composite
//! is not in place.
//! Test: `output_style_floor_tests.rs`.

use std::path::{Path, PathBuf};

use super::{ActiveStyle, PROJECT_STYLES_DIR, strip_frontmatter};
use crate::core::bundle::OUTPUT_STYLE;
use crate::core::instruction_fold::{fold_block, step_fence};
use crate::core::instruction_pipeline::SECTION_SEPARATOR;

/// Headings of the bundled `trusty-mpm` style sections that form the floor.
pub const FLOOR_SECTIONS: [&str; 2] = [
    "## 🔴 PRIMARY DIRECTIVE — MANDATORY DELEGATION",
    "## Communication — Write Plainly",
];

/// Heading of the floor block appended to a project output style.
pub const STYLE_FLOOR_HEADING: &str =
    "# trusty-mpm Floor (appended to the project output style; not overridable)";

/// Suffix of a generated composite style's id: `<id>.tm-floor`.
pub const COMPOSITE_STYLE_SUFFIX: &str = ".tm-floor";

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
///
/// What: starts at a line equal to `heading` and ends before the next `#` or
/// `##` heading line. #8533: a line inside a fenced code block is neither, so a
/// `# comment` in a shell example does not cut the section short.
/// Test: `a_heading_like_line_inside_a_fence_does_not_end_the_section`.
fn section<'a>(doc: &'a str, heading: &str) -> Option<&'a str> {
    let mut fence = None;
    let mut start = None;
    let mut offset = 0;
    for line in doc.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        let outside = !step_fence(&mut fence, text) && fence.is_none();
        match start {
            None if outside && text == heading => start = Some(offset),
            Some(from) if outside && (text.starts_with("# ") || text.starts_with("## ")) => {
                return Some(doc[from..offset].trim());
            }
            _ => {}
        }
        offset += line.len();
    }
    start.map(|from| doc[from..].trim())
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
/// `<!--` ends with the prose; an open fence of any kind is closed.
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
    let prose = fold_block(body);
    if prose.is_empty() {
        floor
    } else {
        format!("{prose}{SECTION_SEPARATOR}{floor}")
    }
}

/// The composite style id for project style `id`.
pub fn composite_style_id(id: &str) -> String {
    format!("{id}{COMPOSITE_STYLE_SUFFIX}")
}

/// Whether `id` names a generated composite, which is never selectable itself.
pub fn is_composite_style_id(id: &str) -> bool {
    id.ends_with(COMPOSITE_STYLE_SUFFIX)
}

/// Where project style `id`'s composite lives.
fn composite_path(project_dir: &Path, id: &str) -> PathBuf {
    project_dir
        .join(PROJECT_STYLES_DIR)
        .join(format!("{}.md", composite_style_id(id)))
}

/// The frontmatter lines of `content`, without the `---` delimiters.
fn frontmatter(content: &str) -> &str {
    let Some(rest) = content.trim_start().strip_prefix("---") else {
        return "";
    };
    rest.find("\n---").map_or("", |end| rest[..end].trim())
}

/// The composite style file for `style`: `None` for a bundled style.
///
/// Why (#8533): a bare `claude` launch loads the style `outputStyle` names and
/// no appended prompt, so a project style file alone lost the floor there.
/// What: frontmatter naming [`composite_style_id`] (the project's other
/// frontmatter lines kept, its `name:` replaced), then [`delivered_style_text`]
/// — the project prose, then the floor.
/// Test: `a_bare_claude_launch_loads_the_project_prose_then_the_floor`.
pub fn composite_style_text(style: &ActiveStyle) -> Option<String> {
    let ActiveStyle::Project { id, content, .. } = style else {
        return None;
    };
    let mut front = vec![
        format!("name: {}", composite_style_id(id)),
        format!("# Generated by tm from {id}.md with the trusty-mpm floor; edit {id}.md."),
    ];
    front.extend(
        frontmatter(content)
            .lines()
            .filter(|line| !line.starts_with("name:"))
            .map(str::to_string),
    );
    Some(format!(
        "---\n{}\n---\n\n{}\n",
        front.join("\n"),
        delivered_style_text(style)
    ))
}

/// Write `style`'s composite into the project and return the id the
/// `outputStyle` settings key names.
///
/// Why (#8533): Claude Code reads the named style file itself, on a tm launch
/// and on a bare `claude` launch alike. Naming the composite makes the floor
/// part of what it reads.
/// What: a bundled style → its own id, nothing written. A project style → its
/// [`composite_style_text`] written to
/// `<project>/.claude/output-styles/<id>.tm-floor.md` when the file differs,
/// and the composite id. A symlink at that path is an error, never followed.
/// Test: `a_bare_claude_launch_loads_the_project_prose_then_the_floor`,
/// `a_symlinked_composite_is_refused`.
pub fn native_style_id(project_dir: &Path, style: &ActiveStyle) -> std::io::Result<String> {
    let Some(text) = composite_style_text(style) else {
        return Ok(style.id().to_string());
    };
    let path = composite_path(project_dir, style.id());
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is a symlink; not writing through it", path.display()),
        ));
    }
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&path, &text)?;
    }
    Ok(composite_style_id(style.id()))
}

/// Whether the project's settings name `style`'s composite and the composite
/// on disk is current, so Claude Code delivers the floor itself.
///
/// Test: `a_project_style_keeps_the_floor_with_and_without_native_support`.
pub fn composite_is_active(project_dir: &Path, style: &ActiveStyle) -> bool {
    let Some(text) = composite_style_text(style) else {
        return false;
    };
    let settings = std::fs::read_to_string(project_dir.join(".claude").join("settings.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
    let named = settings
        .as_ref()
        .and_then(|value| value.get("outputStyle"))
        .and_then(serde_json::Value::as_str)
        == Some(composite_style_id(style.id()).as_str());
    named
        && std::fs::read_to_string(composite_path(project_dir, style.id()))
            .ok()
            .as_deref()
            == Some(text.as_str())
}

#[cfg(test)]
#[path = "output_style_floor_tests.rs"]
mod tests;
