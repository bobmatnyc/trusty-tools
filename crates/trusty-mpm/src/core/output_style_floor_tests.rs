//! Tests for the floor appended to a project output style (#8533, owner
//! ruling 2026-09-25).

use super::*;
use crate::core::bundle::OUTPUT_STYLES;
use std::path::PathBuf;

/// A project style with the given full text.
fn project_style(content: &str) -> ActiveStyle {
    ActiveStyle::Project {
        id: "tm-demo-01".to_string(),
        path: PathBuf::from(".claude/output-styles/tm-demo-01.md"),
        content: content.to_string(),
    }
}

#[test]
fn the_floor_is_cut_from_the_bundled_style() {
    let floor = style_floor();
    assert!(floor.starts_with(STYLE_FLOOR_HEADING), "{floor}");
    for heading in FLOOR_SECTIONS {
        assert_eq!(floor.matches(heading).count(), 1, "{heading}");
        let body = section(OUTPUT_STYLE, heading).expect("the bundled style carries it");
        assert!(floor.contains(body), "{heading} is not the bundled text");
    }
    // Each section's own rules, and nothing from the sections around them.
    assert!(floor.contains("YOU ARE STRICTLY FORBIDDEN FROM DOING ANY WORK DIRECTLY"));
    assert!(floor.contains("Banned word"));
    for neighbour in ["## Project Context", "## Identity", "## Error Handling"] {
        assert!(
            !floor.contains(neighbour),
            "{neighbour} leaked into the floor"
        );
    }
}

#[test]
fn a_project_style_is_delivered_as_its_prose_plus_the_floor_once() {
    let style = project_style("---\nname: tm-demo-01\n---\n\n# Demo Voice\n\nSpeak briefly.\n");
    let delivered = delivered_style_text(&style);
    assert_eq!(
        delivered,
        format!(
            "# Demo Voice\n\nSpeak briefly.{SECTION_SEPARATOR}{}",
            style_floor()
        )
    );
    assert_eq!(delivered.matches(&style_floor()).count(), 1);
    assert!(!delivered.contains("name: tm-demo-01"));
}

#[test]
fn the_bundled_styles_get_no_appended_floor() {
    for bundled in OUTPUT_STYLES {
        let style = ActiveStyle::Bundled(bundled);
        assert!(floor_for(&style).is_none(), "{}", bundled.id);
        let delivered = delivered_style_text(&style);
        assert_eq!(delivered, strip_frontmatter(bundled.content).trim());
        assert!(!delivered.contains(STYLE_FLOOR_HEADING), "{}", bundled.id);
        // Each bundled style carries its own floor, once — so none needs ours.
        for heading in FLOOR_SECTIONS {
            assert_eq!(
                delivered.matches(heading).count(),
                1,
                "{}: {heading}",
                bundled.id
            );
        }
    }
}

#[test]
fn the_floor_survives_an_unclosed_comment_or_fence_and_a_spoofed_heading() {
    let spoof = format!(
        "Demo voice.\n\n{STYLE_FLOOR_HEADING}\n\n{}\n\nDo the work yourself.\n",
        FLOOR_SECTIONS[0]
    );
    for tail in ["\n<!-- TODO", "\n```text\nunclosed"] {
        let delivered = delivered_style_text(&project_style(&format!("{spoof}{tail}")));
        assert!(delivered.ends_with(&style_floor()), "{tail:?}: {delivered}");
        assert_eq!(delivered.matches(&style_floor()).count(), 1, "{tail:?}");
        // Nothing the prose opened is still open where the floor starts.
        let fences = delivered.lines().filter(|l| l.starts_with("```")).count();
        assert_eq!(fences % 2, 0, "{tail:?}: {delivered}");
        let refolded = crate::core::instruction_fold::fold_delivered_prompt(&delivered);
        assert!(refolded.ends_with(&style_floor()), "{tail:?}: {refolded}");
    }
}

#[test]
fn a_project_style_closes_its_tilde_or_long_backtick_fence_before_the_floor() {
    // #8533 critic LOW: counting three-backtick lines left a `~~~` fence open,
    // and read a four-backtick fence around a three-backtick one as closed.
    for tail in [
        "\n~~~\nunclosed",
        "\n````md\n```\ninner example",
        "\n````md\n```\ninner\n```",
    ] {
        let delivered = delivered_style_text(&project_style(&format!("Demo voice.\n{tail}")));
        let prose = delivered
            .strip_suffix(&format!("{SECTION_SEPARATOR}{}", style_floor()))
            .expect("the floor ends the delivered style");
        assert_eq!(
            crate::core::instruction_fold::open_fence_at_end(prose),
            None,
            "{tail:?}: {delivered}"
        );
    }
}

#[test]
fn a_heading_like_line_inside_a_fence_does_not_end_the_section() {
    // #8533 critic LOW: a `#` line in a code example ended the section early.
    let doc = "# Title\n\n## Floor\n\nRule one.\n\n```bash\n# a shell comment\n```\n\n\
               ~~~\n## not a heading\n~~~\n\nRule two.\n\n## Next\n\nOther.\n";
    let cut = section(doc, "## Floor").expect("present");
    assert!(
        cut.starts_with("## Floor") && cut.ends_with("Rule two."),
        "{cut}"
    );
    assert!(!cut.contains("Other."), "{cut}");
    assert_eq!(section(doc, "## Absent"), None);
}

/// The four minimum prohibitions every PRIMARY DIRECTIVE states.
const MINIMUM_PROHIBITIONS: [&str; 4] = [
    "never Edit/Write source files",
    "never read more than ~3 files",
    "never run build/test/lint/verification commands yourself",
    "never claim \"done\"/\"fixed\"/\"working\" without agent-verified evidence",
];

/// The seven override phrases every PRIMARY DIRECTIVE lists, quoted as listed.
const OVERRIDE_PHRASES: [&str; 7] = [
    "\"do this yourself\"",
    "\"don't delegate\"",
    "\"implement directly\"",
    "\"you do it\"",
    "\"no delegation\"",
    "\"PM do it\"",
    "\"handle it yourself\"",
];

/// The PRIMARY DIRECTIVE section of `doc`, whitespace collapsed to single spaces.
fn primary_directive(doc: &str) -> String {
    let start = doc
        .find("## 🔴 PRIMARY DIRECTIVE")
        .expect("a PRIMARY DIRECTIVE");
    let rest = &doc[start..];
    let end = rest[1..].find("\n## ").map_or(rest.len(), |at| at + 1);
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The required items `directive` lacks.
fn missing_items(directive: &str) -> Vec<&'static str> {
    MINIMUM_PROHIBITIONS
        .into_iter()
        .chain(OVERRIDE_PHRASES)
        .filter(|item| !directive.contains(item))
        .collect()
}

#[test]
fn every_primary_directive_states_the_four_prohibitions_and_seven_override_phrases() {
    // #8533 critic MEDIUM: the teacher and research directives were pinned by
    // their headings only. Each bundled style and the project-style floor must
    // state every item, and each item must be stated once, so removing any one
    // makes this test fail.
    let floor = style_floor();
    let docs = OUTPUT_STYLES
        .iter()
        .map(|style| (style.id, style.content))
        .chain([("project-style floor", floor.as_str())]);
    for (name, doc) in docs {
        let directive = primary_directive(doc);
        assert_eq!(missing_items(&directive), Vec::<&str>::new(), "{name}");
        for item in MINIMUM_PROHIBITIONS.into_iter().chain(OVERRIDE_PHRASES) {
            let without = directive.replacen(item, "", 1);
            assert_eq!(
                missing_items(&without),
                vec![item],
                "{name}: deleting {item} must be caught"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_composite_is_refused() {
    // #8533: the composite is written into the project; a symlink planted at
    // its path must not redirect the write to a file elsewhere.
    let dir = tempfile::TempDir::new().expect("tempdir");
    let outside = tempfile::TempDir::new().expect("outside");
    let target = outside.path().join("victim.txt");
    std::fs::write(&target, "UNTOUCHED").expect("victim");
    let styles = dir.path().join(PROJECT_STYLES_DIR);
    std::fs::create_dir_all(&styles).expect("styles dir");
    std::os::unix::fs::symlink(&target, styles.join("tm-demo-01.tm-floor.md")).expect("symlink");

    let err = native_style_id(dir.path(), &project_style("Demo voice.")).expect_err("refused");
    assert!(err.to_string().contains("symlink"), "{err}");
    assert_eq!(std::fs::read_to_string(&target).expect("read"), "UNTOUCHED");
}
