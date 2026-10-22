//! Rule-id markers the bundle tests pin (#10058).
//!
//! Why: owner ruling 2026-10-19 — bundled skill and agent tests check
//! structure, and a rule a test must find carries a stable id, so rewording
//! the sentence beside it breaks nothing.
//! What: [`PINNED_RULES`], the one list of marker ids a bundle test pins and
//! the content file each lives in; [`has_rule`], the lookup every test uses;
//! [`rule_sentence`] and [`rule_section`], which read the text a marker pins
//! without pinning it; and the presence, grammar and uniqueness tests. The
//! grammar itself is `trusty_agents_common::agents::rule_marker`.
//! Test: `every_pinned_rule_marker_is_present_once_in_its_file_10058`,
//! `every_rule_marker_in_content_is_well_formed_and_unique_10058`,
//! `rule_sentence_and_section_read_the_block_under_a_marker_10058`.
use std::collections::BTreeMap;

use trusty_agents_common::agents::rule_marker::{malformed_markers, marker_id, rule_ids};

/// Every rule id a bundle test pins, grouped by the content file that holds it.
///
/// Why: one list makes "every referenced marker is present" a single test, and
/// a test naming an id absent from it fails at the call site.
/// What: `(repo-relative path, ids)`. Moving a marker to another file moves
/// its id between two rows here.
/// Test: `every_pinned_rule_marker_is_present_once_in_its_file_10058`.
pub(super) const PINNED_RULES: &[(&str, &[&str])] = &[
    (
        "content/agents/BASE-AGENT.md",
        &[
            "base-agent/pm-directives-scope",
            "base-agent/pm-authority",
            "base-agent/injection-skepticism-scope",
            "base-agent/authority-and-objective-safety",
            "base-agent/raw-output-evidence",
            "base-agent/never-block-on-ci",
            "base-agent/update-branch-treadmill",
            "base-agent/report-dont-promise",
            "base-agent/no-background-monitor",
            "base-agent/write-plainly",
            "base-agent/verbosity-scales",
            "base-agent/sparse-on-success-keeps-evidence",
        ],
    ),
    (
        "content/agents/local-ops.md",
        &[
            "local-ops/long-waits",
            "local-ops/deploy-and-poll-block",
            "local-ops/loop-margin",
            "local-ops/diagnose-only-overrides",
            "local-ops/log-mtime-check",
        ],
    ),
    (
        "content/agents/research.md",
        &["research/report-carries-findings"],
    ),
    (
        "content/agents/ticketing.md",
        &[
            "ticketing/milestone-relationships-state-mandatory",
            "ticketing/propose-a-milestone",
            "ticketing/report-milestone-relationships-state",
            "ticketing/live-label-list-before-absence",
        ],
    ),
    (
        "content/agents/vercel-ops.md",
        &["vercel-ops/deployment-commit-source"],
    ),
    (
        "content/agents/version-control.md",
        &[
            "version-control/tm-pr-open-applies-half",
            "version-control/auto-merge-read",
            "version-control/verify-before-retry-5xx",
            "version-control/brief-example-is-a-shape",
            "version-control/pm-relayed-merge-authority",
            "version-control/ci-waits",
            "version-control/actions-spend-pointer",
            "version-control/clean-tree-before-gate",
        ],
    ),
    ("content/agents/web-qa.md", &["web-qa/tab-group-recovery"]),
    (
        "content/instructions/docs/WHAT-IS-TRUSTY-MPM.md",
        &["what-is-trusty-mpm/not-claude-mpm"],
    ),
    (
        "content/instructions/output-styles/trusty-mpm.md",
        &["trusty-mpm/write-plainly", "trusty-mpm/todowrite-fallback"],
    ),
    (
        "content/instructions/output-styles/trusty-mpm-teacher.md",
        &[
            "trusty-mpm-teacher/write-plainly",
            "trusty-mpm-teacher/todowrite-fallback",
        ],
    ),
    (
        "content/instructions/output-styles/trusty-mpm-research.md",
        &[
            "trusty-mpm-research/write-plainly",
            "trusty-mpm-research/todowrite-fallback",
        ],
    ),
    (
        "content/instructions/output-styles/trusty-mpm-supervisor.md",
        &["trusty-mpm-supervisor/write-plainly"],
    ),
    (
        "content/instructions/sections/subagent-re-engagement.md",
        &["subagent-re-engagement/resume-via-pm-re-engagement"],
    ),
    (
        "content/skills/code-review-standards.md",
        &[
            "code-review-standards/github-token-protected-push",
            "code-review-standards/format-interpolated-payload",
        ],
    ),
    (
        "content/skills/git-workflow.md",
        &[
            "git-workflow/pre-pull-collision-check",
            "git-workflow/identical-content-resolves",
            "git-workflow/differing-content-is-reported",
            "git-workflow/actions-spend-four-checks",
            "git-workflow/never-rerun-a-green-run",
            "git-workflow/billing-blocker",
        ],
    ),
    (
        "content/skills/rust-delivery-workflow.md",
        &[
            "rust-delivery-workflow/defers-to-sibling-skills",
            "rust-delivery-workflow/commit-before-gates",
            "rust-delivery-workflow/ci-clippy-pin",
            "rust-delivery-workflow/crate-scope-is-not-ci-evidence",
            "rust-delivery-workflow/builds-are-cpu-and-ram-bound",
            "rust-delivery-workflow/job-cap-from-the-machine",
            "rust-delivery-workflow/env-inline-per-command",
            "rust-delivery-workflow/gate-output-is-a-verdict",
            "rust-delivery-workflow/sccache-never-silent",
            "rust-delivery-workflow/batch-installs",
            "rust-delivery-workflow/test-scope-by-stage",
        ],
    ),
    (
        "content/skills/self-improvement-loop.md",
        &[
            "self-improvement-loop/reporting-section",
            "self-improvement-loop/fixed-destination",
            "self-improvement-loop/return-a-block-not-a-filing",
            "self-improvement-loop/dedup-by-label",
            "self-improvement-loop/clean-run-reports-nothing",
            "self-improvement-loop/post-mortem-tracker",
            "self-improvement-loop/fast-loop-section",
            "self-improvement-loop/detection-heuristics",
            "self-improvement-loop/hypothesis-tag",
            "self-improvement-loop/post-mortem-query",
        ],
    ),
    (
        "content/skills/tm-delegation-patterns.md",
        &[
            "tm-delegation-patterns/tree-must-exist-before-retry",
            "tm-delegation-patterns/pm-re-engagement",
            "tm-delegation-patterns/worktree-check-first",
            "tm-delegation-patterns/no-blind-poll",
        ],
    ),
    (
        "content/skills/tm-init.md",
        &[
            "tm-init/build-performance-section",
            "tm-init/timings-baseline",
            "tm-init/no-unmeasured-sccache-claim",
        ],
    ),
    (
        "content/skills/tm-prose-style.md",
        &[
            "tm-prose-style/honest-ban-positions",
            "tm-prose-style/borrowed-metaphor-category",
            "tm-prose-style/asd-ste-100-spirit-only",
            "tm-prose-style/asd-ste-100-rules",
        ],
    ),
    (
        "content/skills/tm-session-resume.md",
        &[
            "tm-session-resume/lean-default",
            "tm-session-resume/cli-fallback",
            "tm-session-resume/paged-full-digest",
            "tm-session-resume/tmux-session-route",
        ],
    ),
    (
        "content/skills/tm-ticketing.md",
        &[
            "tm-ticketing/labels-project-milestone-standard",
            "tm-ticketing/every-issue-milestone-relationships-state",
            "tm-ticketing/propose-a-milestone-set",
            "tm-ticketing/status-and-label-name-one-stage",
        ],
    ),
    (
        "content/skills/tm-workflow.md",
        &[
            "tm-workflow/auto-merge-read",
            "tm-workflow/clean-tree-before-gate",
        ],
    ),
    (
        "content/skills/tm-workflow/references/worktree-discipline.md",
        &["tm-workflow/pre-pull-collision-pointer"],
    ),
    (
        "content/skills/verification-before-completion.md",
        &[
            "verification-before-completion/exit-code-signal-reading",
            "verification-before-completion/exit-137-exception",
        ],
    ),
];

/// Every `(id, path)` pair of [`PINNED_RULES`].
fn pinned() -> impl Iterator<Item = (&'static str, &'static str)> {
    PINNED_RULES
        .iter()
        .flat_map(|(path, ids)| ids.iter().map(move |id| (*id, *path)))
}

/// Whether `body` carries the marker for `id`.
///
/// Why: a test pins a rule by id, never by its wording (#10058).
/// What: panics when `id` is not in [`PINNED_RULES`], so every id a test uses
/// is covered by the presence test; otherwise reports whether one of
/// `body`'s markers is `id`.
/// Test: `every_pinned_rule_marker_is_present_once_in_its_file_10058`.
pub(super) fn has_rule(body: &str, id: &str) -> bool {
    assert!(
        pinned().any(|(pinned, _)| pinned == id),
        "rule `{id}` is not in PINNED_RULES (#10058)"
    );
    rule_ids(body).contains(&id)
}

/// The lines of `body` after the marker for `id`, up to the first blank line,
/// skipping markers stacked under it.
fn block_under<'a>(body: &'a str, id: &str) -> Option<Vec<&'a str>> {
    let mut lines = body.lines();
    lines.find(|line| marker_id(line) == Some(id))?;
    Some(
        lines
            .filter(|line| marker_id(line).is_none())
            .take_while(|line| !line.trim().is_empty())
            .collect(),
    )
}

/// `text` whitespace-collapsed with `*` emphasis removed, the form
/// [`rule_sentence`] returns and a "stated once" check compares against.
pub(super) fn plain(text: &str) -> String {
    text.replace('*', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The first sentence of the block `id` pins, in [`plain`] form, from its
/// first letter or digit: the text a "stated once" check looks for in other
/// files without pinning its wording (#10058).
///
/// Test: `rule_sentence_and_section_read_the_block_under_a_marker_10058`.
pub(super) fn rule_sentence(body: &str, id: &str) -> Option<String> {
    let block = plain(&block_under(body, id)?.join(" "));
    let start = block.find(|c: char| c.is_alphanumeric())?;
    let block = &block[start..];
    let end = block.find(". ").map_or(block.len(), |at| at + 1);
    Some(block[..end].to_owned())
}

/// The text of the nearest heading above the marker for `id`, `#`s trimmed:
/// the section name a pointer must use to reach the rule (#10058).
///
/// Test: `rule_sentence_and_section_read_the_block_under_a_marker_10058`.
pub(super) fn rule_section<'a>(body: &'a str, id: &str) -> Option<&'a str> {
    let mut heading = None;
    for line in body.lines() {
        if marker_id(line) == Some(id) {
            return heading;
        }
        if line.starts_with('#') {
            heading = Some(line.trim_start_matches('#').trim());
        }
    }
    None
}

/// Every `.md` file under the checkout's `content/`, as `(repo-relative path,
/// text)`, sorted.
fn content_markdown() -> &'static [(String, String)] {
    static FILES: std::sync::OnceLock<Vec<(String, String)>> = std::sync::OnceLock::new();
    FILES.get_or_init(|| {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("readable content dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "md") {
                    out.push(path);
                }
            }
        }
        let root = crate::core::content_source::test_support::repo_root();
        let mut paths = Vec::new();
        walk(&root.join("content"), &mut paths);
        let mut files: Vec<(String, String)> = paths
            .into_iter()
            .map(|path| {
                let rel = path
                    .strip_prefix(&root)
                    .expect("under the repo root")
                    .to_string_lossy()
                    .into_owned();
                let text = std::fs::read_to_string(&path).expect("utf-8 content");
                (rel, text)
            })
            .collect();
        files.sort();
        files
    })
}

/// Every marker id that occurs more than once across `files`, with each place
/// it occurs.
fn duplicate_rule_ids(files: &[(String, String)]) -> Vec<String> {
    let mut seen: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (path, text) in files {
        for id in rule_ids(text) {
            seen.entry(id).or_default().push(path);
        }
    }
    seen.into_iter()
        .filter(|(_, places)| places.len() > 1)
        .map(|(id, places)| format!("{id}: {places:?}"))
        .collect()
}

/// Why: #10058 closure condition — removing a marker a test references must
/// break a test, and a stale [`PINNED_RULES`] row must not linger.
/// What: each pinned id occurs once in its named file; no id is listed twice;
/// each is named by `bundle_tests.rs`. Mutation: the first file with its
/// first marker removed no longer carries the id.
/// Test: this test.
#[test]
fn every_pinned_rule_marker_is_present_once_in_its_file_10058() {
    let files: BTreeMap<&str, &str> = content_markdown()
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let tests = include_str!("bundle_tests.rs");
    let mut problems = Vec::new();
    let mut ids: Vec<&str> = pinned().map(|(id, _)| id).collect();
    ids.sort_unstable();
    for pair in ids.windows(2).filter(|pair| pair[0] == pair[1]) {
        problems.push(format!("PINNED_RULES lists `{}` twice", pair[0]));
    }
    for (id, path) in pinned() {
        let count = files.get(path).map_or(0, |text| {
            rule_ids(text).iter().filter(|got| **got == id).count()
        });
        if count != 1 {
            problems.push(format!("`{id}` occurs {count} times in {path}, want 1"));
        }
        if !tests.contains(&format!("\"{id}\"")) {
            problems.push(format!("`{id}` is pinned but no bundle test names it"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");

    let (id, path) = pinned().next().expect("a pinned rule");
    let removed = files[path].replace(&format!("<!-- rule:{id} -->\n"), "");
    assert!(
        !rule_ids(&removed).contains(&id),
        "removing the marker for `{id}` must leave {path} without it"
    );
}

/// Why: #10058 — a mistyped marker hides its rule from every test, and two
/// markers sharing an id make "the rule exists" ambiguous.
/// What: no line under `content/` opens like a marker and fails the grammar,
/// and no id occurs twice across `content/` (the Architect's `r-NNN` ids
/// included). Mutation: a copy with one id duplicated into a second file is
/// reported.
/// Test: this test.
#[test]
fn every_rule_marker_in_content_is_well_formed_and_unique_10058() {
    let files = content_markdown();
    let malformed: Vec<String> = files
        .iter()
        .flat_map(|(path, text)| {
            malformed_markers(text)
                .into_iter()
                .map(move |line| format!("{path}: {line}"))
        })
        .collect();
    assert!(
        malformed.is_empty(),
        "malformed rule markers: {malformed:#?}"
    );
    let duplicates = duplicate_rule_ids(files);
    assert!(
        duplicates.is_empty(),
        "duplicated rule ids: {duplicates:#?}"
    );

    let (id, _) = pinned().next().expect("a pinned rule");
    let mut doubled = files.to_vec();
    doubled.push(("probe.md".into(), format!("<!-- rule:{id} -->\nA copy.\n")));
    assert_eq!(
        duplicate_rule_ids(&doubled).len(),
        1,
        "a duplicated id must be reported"
    );
}

/// Why: #10058 — the "stated once" and pointer checks read a rule's block
/// and section through its marker.
/// What: the sentence skips stacked markers, emphasis and a leading emoji,
/// and ends at the first full stop; the section is the nearest heading above;
/// an absent id yields `None`.
/// Test: this test.
#[test]
fn rule_sentence_and_section_read_the_block_under_a_marker_10058() {
    let body = "## Labels\n\nIntro.\n\n<!-- rule:demo/one -->\n<!-- rule:demo/two -->\n\
                🔴 **First  line,\n  second line.** Then more.\n\nNext block.\n";
    assert_eq!(
        rule_sentence(body, "demo/one").as_deref(),
        Some("First line, second line.")
    );
    assert_eq!(rule_section(body, "demo/two"), Some("Labels"));
    assert_eq!(rule_sentence(body, "demo/absent"), None);
    assert_eq!(rule_section(body, "demo/absent"), None);
}
