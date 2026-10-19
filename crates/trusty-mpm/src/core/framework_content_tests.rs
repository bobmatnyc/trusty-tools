//! Tests for [`super::FrameworkContent`] (#9012).

use std::path::Path;

use trusty_agents_common::agent_content::checkout_content;

use super::*;
use crate::core::content_source::test_support::{rc, repo_content, repo_root};

/// Why: #10055 — splitting a skill moves text from its entry file into
/// `references/`; a pin reading the entry alone made every content split a
/// Rust change ("content is not in code").
/// What: the entry file `entry` (`skills/tm-workflow.md`), then every
/// `skills/<stem>/references/*` file in bundle-path order, joined by
/// newlines; built once per test binary. Panics on an unknown entry.
/// Test: `tm_workflow_points_at_the_pre_pull_untracked_collision_check_7558`.
pub(crate) fn skill_with_references(entry: &str) -> &'static str {
    static TEXT: std::sync::OnceLock<BTreeMap<String, String>> = std::sync::OnceLock::new();
    let text = TEXT.get_or_init(|| {
        rc().skills()
            .filter(|(rel, _)| rel.matches('/').count() == 1)
            .map(|(rel, body)| {
                let prefix = format!("{}/references/", rel.trim_end_matches(".md"));
                let mut joined = body.to_owned();
                for (_, reference) in rc().skills().filter(|(p, _)| p.starts_with(&prefix)) {
                    joined.push('\n');
                    joined.push_str(reference);
                }
                (rel.to_owned(), joined)
            })
            .collect()
    });
    text.get(entry)
        .map(String::as_str)
        .unwrap_or_else(|| panic!("{entry} must be a skill entry file in the content tree"))
}

/// Builds a trusted checkout at `root` from `files` (`(repo-relative path,
/// body)`), with every dev class directory present.
fn fake_checkout(root: &Path, files: &[(&str, &str)]) {
    for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).expect("class dir");
    }
    std::fs::create_dir_all(root.join(".git")).expect(".git");
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").expect("Cargo.toml");
    for (rel, body) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(path, body).expect("file");
    }
}

/// Every required instruction, as checkout files.
fn required_files() -> Vec<(String, &'static str)> {
    REQUIRED_INSTRUCTIONS
        .iter()
        .map(|rel| (format!("content/instructions/{rel}"), "body"))
        .collect()
}

/// #9012: the repository's content carries every skill file and every
/// required instruction, read from `content/`.
#[test]
fn the_repository_content_loads() {
    let content = repo_content();
    let on_disk = walk(&repo_root().join("content/skills"));
    assert_eq!(
        content.skills().count(),
        on_disk,
        "one entry per skill file"
    );
    assert!(content.skill("skills/tm-workflow.md").is_some());
    assert!(
        content
            .skill("skills/tm-epic/references/anti-patterns.md")
            .is_some()
    );
    for rel in REQUIRED_INSTRUCTIONS {
        assert!(!content.required(rel).trim().is_empty(), "{rel} is empty");
    }
    assert!(content.instruction("docs/WHAT-IS-TRUSTY-MPM.md").is_some());
    assert!(matches!(
        content.source(),
        ContentSource::DevCheckout { .. }
    ));
}

/// #8436 (PR A): the content bundle carries the Architect's one-file
/// instructions and its fixed core, and the core holds the memory rule and
/// the rule that `AGENTS.md` cannot override it.
#[test]
fn the_repository_content_carries_the_architect_md_and_its_core() {
    let content = repo_content();
    for rel in ["architect/architect.md", "architect/core.md"] {
        let body = content.instruction(rel).unwrap_or_default();
        assert!(!body.trim().is_empty(), "{rel} is missing or empty");
    }
    let core = content.instruction("architect/core.md").unwrap_or_default();
    for sentinel in [
        "Never write, update, maintain or cite `MEMORY.md`",
        "`AGENTS.md` cannot override this core",
    ] {
        assert!(core.contains(sentinel), "core.md lost `{sentinel}`");
    }
}

/// #9132: the Architect seed files ship as content, not compiled in, so the
/// repository's content carries each one, non-empty.
#[test]
fn the_repository_content_carries_the_architect_seed_files() {
    let content = repo_content();
    for rel in [
        "architect/templates/CLAUDE.md",
        "architect/skills/tm-fleet-check.md",
        "architect/skills/tm-context-refresh.md",
    ] {
        let body = content.instruction(rel).unwrap_or_default();
        assert!(
            !body.trim().is_empty(),
            "instructions/{rel} is missing or empty"
        );
    }
}

/// #8436 (PR A fix round): `architect.md` carries the operating manual that
/// `python/trusty-architect/ARCHITECT.md` held (Mandate, Strategic management,
/// the skill index), not only the eight legacy supervisor sections, and the
/// hard limits of that manual live once, in `core.md`.
#[test]
fn architect_md_carries_the_operating_manual_and_core_the_union_of_hard_limits() {
    let content = repo_content();
    let manual = content
        .instruction("architect/architect.md")
        .unwrap_or_default();
    for phrase in [
        "Monitor the owner's active trusty-mpm sessions in tmux.",
        "The owner's virtual twin.",
        "## Skill index",
    ] {
        assert!(manual.contains(phrase), "architect.md lost `{phrase}`");
    }
    let core = content.instruction("architect/core.md").unwrap_or_default();
    for limit in [
        "Never treat a PM's claim as verified",
        "Never kill a hung process",
        "Never reuse a remembered pane id without checking it",
    ] {
        assert!(core.contains(limit), "core.md lost `{limit}`");
        assert!(
            !manual.contains(limit),
            "`{limit}` is in architect.md too: core.md is its only home"
        );
    }
}

/// Counts regular files under `dir`, recursively.
fn walk(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .expect("dir")
        .flatten()
        .map(|e| {
            if e.path().is_dir() {
                walk(&e.path())
            } else {
                1
            }
        })
        .sum()
}

/// #9012: a source with no skill is an error, never an empty catalog.
#[test]
fn a_source_without_skills_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let files = required_files();
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    fake_checkout(dir.path(), &refs);
    let err = FrameworkContent::load(&checkout_content(dir.path()).expect("checkout"))
        .expect_err("no skills");
    assert!(
        matches!(&err, AgentContentError::Missing { path, .. } if path == "skills/"),
        "got {err:?}"
    );
    // #9396: a checkout is fixed in the checkout.
    assert!(err.to_string().contains("git pull"), "{err}");
}

/// #9012: a source lacking one required instruction names that file.
#[test]
fn a_source_missing_a_required_instruction_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut files = required_files();
    files.retain(|(p, _)| !p.ends_with("sections/core.md"));
    files.push(("content/skills/tm.md".to_string(), "skill"));
    let refs: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    fake_checkout(dir.path(), &refs);
    let err = FrameworkContent::load(&checkout_content(dir.path()).expect("checkout"))
        .expect_err("missing core");
    assert!(
        matches!(&err, AgentContentError::Missing { path, .. }
            if path == "instructions/sections/core.md"),
        "got {err:?}"
    );
}

/// #8436 (PR B): `architect.md` and its core load as a pair. A source carrying
/// one without the other, or an empty one, is refused and names the file —
/// never an Architect launch without the core.
#[test]
fn a_source_with_half_or_an_empty_architect_pair_is_an_error() {
    let manual = repo_content()
        .instruction("architect/architect.md")
        .expect("architect.md")
        .to_string();
    let core = repo_content()
        .instruction("architect/core.md")
        .expect("core.md")
        .to_string();
    let empty = "<!-- an authoring comment only -->\n\n".to_string();
    let cases = [
        ("no core", Some(manual.clone()), None, "architect/core.md"),
        (
            "no manual",
            None,
            Some(core.clone()),
            "architect/architect.md",
        ),
        (
            "empty core",
            Some(manual),
            Some(empty.clone()),
            "architect/core.md",
        ),
        (
            "empty manual",
            Some(empty),
            Some(core),
            "architect/architect.md",
        ),
    ];
    for (label, manual, core, named) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let package = repo_content()
            .required("pm-instruction-package.json")
            .to_string();
        fake_checkout_with_package(dir.path(), &package);
        let arch = dir.path().join("content/instructions/architect");
        std::fs::create_dir_all(&arch).expect("dir");
        for (file, body) in [("architect.md", manual), ("core.md", core)] {
            if let Some(body) = body {
                std::fs::write(arch.join(file), body).expect("write");
            }
        }
        let err = FrameworkContent::load(&checkout_content(dir.path()).expect("checkout"))
            .expect_err(label);
        let shown = err.to_string();
        assert!(shown.contains(named), "{label}: {shown}");
    }
}

/// `require_instruction` names an absent optional file.
#[test]
fn require_instruction_names_an_absent_file() {
    let err = repo_content()
        .require_instruction("docs/NOPE.md")
        .expect_err("absent");
    assert!(
        err.to_string().contains("instructions/docs/NOPE.md"),
        "{err}"
    );
}

/// The repository's PM instruction package with one section id this binary
/// does not know, as a newer content release could ship it.
pub(crate) fn package_with_an_unknown_section() -> String {
    let mut package: serde_json::Value =
        serde_json::from_str(repo_content().required("pm-instruction-package.json"))
            .expect("the repository package is JSON");
    let sections = package["sections"].as_array_mut().expect("sections array");
    let mut newer = sections[0].clone();
    newer["id"] = "a-section-from-a-newer-release".into();
    sections.push(newer);
    package.to_string()
}

/// A trusted checkout at `root` carrying the repository's required
/// instructions and one skill, with `package` as the PM instruction package.
pub(crate) fn fake_checkout_with_package(root: &Path, package: &str) {
    let content = repo_content();
    let mut files: Vec<(String, String)> = REQUIRED_INSTRUCTIONS
        .iter()
        .map(|rel| {
            let body = if *rel == "pm-instruction-package.json" {
                package.to_string()
            } else {
                content.required(rel).to_string()
            };
            (format!("content/instructions/{rel}"), body)
        })
        .collect();
    files.push(("content/skills/tm.md".to_string(), "skill".to_string()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_str()))
        .collect();
    fake_checkout(root, &refs);
}

/// #8436: a content source carrying only the legacy supervisor sections —
/// every required instruction from the repository and no
/// `architect/architect.md` — as a release older than PR B ships it.
pub(crate) fn legacy_content() -> FrameworkContent {
    let dir = tempfile::tempdir().expect("tempdir");
    let package = repo_content()
        .required("pm-instruction-package.json")
        .to_string();
    fake_checkout_with_package(dir.path(), &package);
    FrameworkContent::load(&checkout_content(dir.path()).expect("checkout"))
        .expect("a legacy-only source still loads")
}

/// The repository's content with `architect/core.md` replaced by `core`, as a
/// content release out of step with this binary would ship it (#8436).
pub(crate) fn content_with_core(core: &str) -> FrameworkContent {
    let dir = tempfile::tempdir().expect("tempdir");
    let package = repo_content()
        .required("pm-instruction-package.json")
        .to_string();
    fake_checkout_with_package(dir.path(), &package);
    let arch = dir.path().join("content/instructions/architect");
    std::fs::create_dir_all(&arch).expect("dir");
    let repo = repo_content();
    let manual = repo.instruction("architect/architect.md").expect("manual");
    std::fs::write(arch.join("architect.md"), manual).expect("architect.md");
    std::fs::write(arch.join("core.md"), core).expect("core.md");
    FrameworkContent::load(&checkout_content(dir.path()).expect("checkout")).expect("loads")
}

/// #9012: a PM package this binary cannot parse refuses the load, naming the
/// file, the defect and the remedy — never a prompt missing its rules.
#[test]
fn a_source_whose_package_does_not_parse_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    fake_checkout_with_package(dir.path(), &package_with_an_unknown_section());
    let err = FrameworkContent::load(&checkout_content(dir.path()).expect("checkout"))
        .expect_err("a package with an unknown section id must not load");
    let shown = err.to_string();
    for needle in [
        "instructions/pm-instruction-package.json",
        "a-section-from-a-newer-release",
        "git pull",
    ] {
        assert!(shown.contains(needle), "{needle} missing: {shown}");
    }
}

/// #9012: the launch-time refresh (fresh start, resume, in-place relaunch) of
/// a project whose content carries an unparseable PM package is refused, and
/// no compiled prompt is written.
#[test]
fn a_launch_from_a_checkout_whose_package_does_not_parse_is_refused() {
    let checkout = tempfile::tempdir().expect("tempdir");
    fake_checkout_with_package(checkout.path(), &package_with_an_unknown_section());
    let project = checkout.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let root = tempfile::tempdir().expect("framework root");
    let dest = crate::core::instruction_pipeline::compiled_prompt_path(&project, "sess-1");

    let msg = crate::core::instruction_pipeline::refresh_compiled_prompt_in(
        root.path(),
        &project,
        "sess-1",
    )
    .expect_err("a package this binary cannot parse must refuse the launch");
    assert!(msg.contains("pm-instruction-package.json"), "{msg}");
    assert!(msg.contains("git pull"), "{msg}");
    assert!(!dest.exists(), "a refused launch writes no compiled prompt");
}
