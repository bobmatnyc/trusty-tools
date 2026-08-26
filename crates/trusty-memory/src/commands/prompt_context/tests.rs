//! Integration tests for `commands::prompt_context`.
//!
//! Why: Separated from `mod.rs` to keep the production module under the
//! 500-SLOC cap while retaining full test coverage.
//! What: exercises `build_injection_body`, filter helpers, format helpers,
//! and the full HTTP-daemon path (gated on `axum-server`).
//! Test: this file is the test suite.

use super::*;
// Why (issue #226): `serde_json::json!` is only used by the daemon-based
//      tests, which are themselves gated behind `axum-server`. Mirror the
//      gate here so `--no-default-features` builds stay warning-free.
#[cfg(feature = "daemon")]
use serde_json::json;

/// Why (issue #134): the recall query needs the actual prompt text the
/// user typed; the stdin payload carries it under `"prompt"`.
/// What: parses three shapes — full JSON with `prompt`, JSON without,
/// and raw text — and asserts each returns the expected string.
/// Test: itself.
#[test]
fn parse_user_prompt_prefers_prompt_field() {
    let json_with_prompt = serde_json::json!({
        "prompt": "what is rust?",
        "cwd": "/tmp/example",
    })
    .to_string();
    assert_eq!(parse_user_prompt(&json_with_prompt), "what is rust?");

    let json_without_prompt = serde_json::json!({"cwd": "/tmp/example"}).to_string();
    assert_eq!(parse_user_prompt(&json_without_prompt), json_without_prompt);

    assert_eq!(parse_user_prompt("plain text query"), "plain text query");
    assert_eq!(parse_user_prompt(""), "");
}

/// Why (issue #139): the deny-tag filter is the load-bearing piece of
/// the recall-quality fix; unit-test the boundary conditions in
/// isolation so a refactor cannot silently regress them.
/// What: case-insensitive matching, empty deny list = passthrough,
/// drawers with no tags = kept (no excluded tag can match).
/// Test: itself.
#[test]
fn filter_drawers_by_deny_tags_handles_edge_cases() {
    use filter::{filter_drawers_by_deny_tags, RecalledDrawer};
    let make = |tags: &[&str]| RecalledDrawer {
        content: "irrelevant".into(),
        tags: tags.iter().map(|s| s.to_string()).collect(),
        layer: Some(2),
        score: Some(0.9),
    };

    // Empty deny list → passthrough.
    let drawers = vec![make(&["claude-session"]), make(&["rust"])];
    let out = filter_drawers_by_deny_tags(drawers.clone(), &[]);
    assert_eq!(out.len(), 2, "empty deny list must pass everything");

    // Case-insensitive match (deny "claude-session" vs tag "Claude-Session").
    let drawers = vec![make(&["Claude-Session"]), make(&["rust"])];
    let out = filter_drawers_by_deny_tags(drawers, &["claude-session".to_string()]);
    assert_eq!(out.len(), 1);
    assert!(out[0].tags.iter().any(|t| t == "rust"));

    // Drawer with no tags is always kept.
    let drawers = vec![make(&[]), make(&["user-prompt"])];
    let out = filter_drawers_by_deny_tags(drawers, &["user-prompt".to_string()]);
    assert_eq!(out.len(), 1, "tagless drawers must survive the filter");
    assert!(out[0].tags.is_empty());

    // Multiple deny entries — any match excludes.
    let drawers = vec![
        make(&["claude-session"]),
        make(&["user-prompt"]),
        make(&["signal"]),
    ];
    let out = filter_drawers_by_deny_tags(
        drawers,
        &["claude-session".to_string(), "user-prompt".to_string()],
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].tags, vec!["signal".to_string()]);
}

/// Why (issue #134): KG triples should only surface when one of their
/// endpoints actually appears in the user's prompt; otherwise the
/// injection just dumps random graph noise.
/// What: build a small set of hot-predicate triples; query a prompt that
/// mentions only one subject; assert exactly the matching triple comes back.
/// #5819 changed the fixture's predicates from `is-a` to hot ones so this test
/// keeps covering the *overlap* rule rather than silently passing because the
/// predicate gate rejected everything.
/// Test: itself.
#[test]
fn select_relevant_triples_filters_by_prompt_overlap() {
    use filter::{select_relevant_triples, RawTriple};
    let triples = vec![
        RawTriple {
            subject: "tga".into(),
            predicate: "is_alias_for".into(),
            object: "trusty-git-analytics".into(),
        },
        RawTriple {
            subject: "python".into(),
            predicate: "is_fact".into(),
            object: "language".into(),
        },
        RawTriple {
            subject: "rust".into(),
            predicate: "is_fact".into(),
            object: "language".into(),
        },
    ];
    let chosen = select_relevant_triples(&triples, "tell me about rust integration", 5);
    assert_eq!(chosen.len(), 1, "only the rust triple should match");
    assert_eq!(chosen[0].subject, "rust");

    // Empty / no-overlap prompt → no triples.
    let none = select_relevant_triples(&triples, "weather forecast next week", 5);
    assert!(none.is_empty());
}

/// Why (#5819, ADR-0028 D7): the selector judged only whether a triple's
/// endpoints appeared in the prompt, never whether the triple asserted
/// knowledge. Storage plumbing reached the model under a "Relevant KG facts"
/// heading — the three shapes below are verbatim from real injected output.
/// What: builds one triple of each observed noise shape plus one genuine hot
/// triple, uses a prompt that overlaps ALL of them so the overlap rule cannot
/// be what excludes anything, and asserts only the hot triple survives.
/// Test: itself. Fails against the pre-#5819 selector, which returns all four.
#[test]
fn select_relevant_triples_drops_structural_predicates() {
    use filter::{select_relevant_triples, RawTriple};
    let triples = vec![
        RawTriple {
            subject: "tag:worktree".into(),
            predicate: "tags".into(),
            object: "drawer:58141829-0918-4198-a274-307c49b5a671".into(),
        },
        RawTriple {
            subject: "room:General".into(),
            predicate: "contains".into(),
            object: "drawer:3887130b-2630-429f-a440-979acea9edb5".into(),
        },
        RawTriple {
            subject: "topic:12fbc5c8f19b4c32bb4641d2e42c0b93ca385086".into(),
            predicate: "mentioned-in".into(),
            object: "drawer:3887130b-2630-429f-a440-979acea9edb5".into(),
        },
        RawTriple {
            subject: "worktree".into(),
            predicate: "has_convention".into(),
            object: "one worktree per reviewable PR outcome".into(),
        },
    ];
    // Every subject or object above overlaps at least one word here.
    let prompt = "worktree general topic 12fbc5c8f19b4c32bb4641d2e42c0b93ca385086 drawer";
    let chosen = select_relevant_triples(&triples, prompt, 20);
    assert_eq!(
        chosen.len(),
        1,
        "only the has_convention triple asserts knowledge; got: {chosen:?}"
    );
    assert_eq!(chosen[0].predicate, "has_convention");
}

/// Why (#5819): the specific line the owner cited —
/// `tag:creator:client=trusty-memory-mcp **tags** drawer:58141829-…` — is
/// write-path provenance rendered to the model as a fact. This pins that exact
/// shape rather than the general rule, so a future change that reintroduces
/// structural predicates for some other reason still cannot reintroduce this
/// one silently.
/// What: composes the three `creator:*` triple shapes stamped on every drawer
/// and asserts none is selected, with a prompt that overlaps them.
/// Test: itself. Fails against the pre-#5819 selector.
#[test]
fn select_relevant_triples_drops_creator_provenance_triples() {
    use filter::{select_relevant_triples, RawTriple};
    let drawer = "drawer:58141829-0918-4198-a274-307c49b5a671";
    let triples: Vec<RawTriple> = [
        "tag:creator:client=trusty-memory-mcp",
        "tag:creator:source=mcp",
        "tag:creator:version=0.23.1",
        "tag:creator:cwd=/users/bob/duetto/cto",
    ]
    .iter()
    .map(|s| RawTriple {
        subject: (*s).into(),
        predicate: "tags".into(),
        object: drawer.into(),
    })
    .collect();
    let chosen = select_relevant_triples(
        &triples,
        "who was the creator client mcp version cwd for this drawer",
        20,
    );
    assert!(
        chosen.is_empty(),
        "creator provenance is never a KG fact; got: {chosen:?}"
    );
}

/// Why: the injection has a hard 4 KB byte ceiling so a runaway palace
/// can't drown the model's prompt; truncation must end with `…` and
/// stay valid UTF-8.
/// What: synthesises drawers whose previews exceed the cap, calls
/// `compose_injection`, asserts the result is `<= INJECTION_BYTE_CAP`
/// and ends with `…`.
/// Test: itself.
#[test]
fn compose_injection_truncates_at_cap() {
    use filter::{RawTriple, RecalledDrawer};
    use format::compose_injection;
    // Stuff a giant global-facts block to push the composition past the
    // 4 KB byte cap. Drawer previews are already capped at
    // DRAWER_PREVIEW_CHARS so the cap-trigger has to come from the
    // global section.
    // #5037 raised INJECTION_BYTE_CAP from 4 KB to 8 KB; size the fixture off
    // the constant so the next raise cannot silently stop exercising truncation.
    let lines = INJECTION_BYTE_CAP / "- fact line\n".len() + 64;
    let big_global = "## Big block\n".to_string() + &"- fact line\n".repeat(lines);
    let drawers: Vec<RecalledDrawer> = (0..5)
        .map(|i| RecalledDrawer {
            content: format!("drawer {i} content"),
            tags: vec!["tag1".into()],
            layer: Some(2),
            score: Some(0.9),
        })
        .collect();
    let triples: Vec<RawTriple> = (0..5)
        .map(|i| RawTriple {
            subject: format!("subject{i}"),
            predicate: "p".into(),
            object: "object".into(),
        })
        .collect();
    let out = compose_injection(Some(&big_global), &drawers, 0, &triples, Some("alpha"));
    assert!(
        out.len() <= INJECTION_BYTE_CAP,
        "expected len <= cap; got {}",
        out.len()
    );
    // Truncation marker survives.
    assert!(
        out.ends_with('…'),
        "expected `…` truncation marker; got tail: {}",
        &out[out.len().saturating_sub(20)..]
    );
}

/// Why: an empty composition (no global facts, no drawers, no triples)
/// must return an empty string so the caller can substitute the
/// legacy placeholder. Section headers should never appear without
/// content beneath them.
/// What: call `compose_injection` with empty inputs and assert the
/// result is empty.
/// Test: itself.
#[test]
fn compose_injection_empty_inputs_yields_empty() {
    use format::compose_injection;
    let out = compose_injection(None, &[], 0, &[], Some("alpha"));
    assert!(out.is_empty(), "got: {out:?}");
}

/// Why (issue #5037): the truncation budget was raised from 4 KB to 8 KB
/// alongside the relevance floor. Pin the new ceiling explicitly so a future
/// edit cannot quietly shrink it back and blame the floor for lost content.
/// What: asserts the constant and that `compose_injection` respects it.
/// Test: itself.
#[test]
fn injection_byte_cap_is_eight_kib() {
    assert_eq!(INJECTION_BYTE_CAP, 8 * 1024);
    assert_eq!(DEFAULT_TOP_K, 12, "requirement 2: max size raised from 5");
}

/// Why (issue #5037, requirement 1 + the primary probe): "what is the capital
/// of France" returned five drawers all scoring exactly `0.15` — the
/// `L1_NO_SIMILARITY_PENALTY` floor — and the reader could not tell them from a
/// genuine `0.56` hit. That set must now be empty.
/// What: five 0.15-scored drawers through the relevance filter at the shipped
/// default; asserts nothing survives and all five are counted as withheld.
/// Test: itself.
#[test]
fn relevance_floor_drops_all_noise_drawers() {
    use filter::{filter_drawers_by_relevance_floor, RecalledDrawer};
    let noise: Vec<RecalledDrawer> = (0..5)
        .map(|i| RecalledDrawer {
            content: format!("off-topic session drawer {i}"),
            tags: vec!["signal".into()],
            layer: Some(1),
            // The exact value `rescore_l1_by_similarity` assigns an essential
            // drawer the HNSW search never returned.
            score: Some(0.15),
        })
        .collect();
    let out = filter_drawers_by_relevance_floor(noise, DEFAULT_RELEVANCE_FLOOR);
    assert!(
        out.kept.is_empty(),
        "0.15 L1-penalty drawers must not reach the injection; got {:?}",
        out.kept.len()
    );
    assert_eq!(out.withheld, 5, "all five must be counted for the notice");
}

/// Why (issue #5037): a floor that also cut real matches would trade one defect
/// for a worse one. `0.56` is the measured median of the self-retrieval signal
/// population against the live palace.
/// What: one genuine hit among four noise drawers; asserts only the hit
/// survives, in order, with the rest counted.
/// Test: itself.
#[test]
fn relevance_floor_keeps_high_scoring_drawer() {
    use filter::{filter_drawers_by_relevance_floor, RecalledDrawer};
    let make = |content: &str, score: f32| RecalledDrawer {
        content: content.into(),
        tags: Vec::new(),
        layer: Some(2),
        score: Some(score),
    };
    let mixed = vec![
        make("genuine on-topic hit about rust integration", 0.56),
        make("noise a", 0.15),
        make("noise b", 0.15),
        make("noise c", 0.2446),
        make("noise d", 0.3439),
    ];
    let out = filter_drawers_by_relevance_floor(mixed, DEFAULT_RELEVANCE_FLOOR);
    assert_eq!(out.kept.len(), 1, "the genuine hit must survive");
    assert!(out.kept[0].content.contains("genuine on-topic hit"));
    assert_eq!(out.withheld, 4);
}

/// Why (issue #5037): a drawer whose `score` the daemon did not send is
/// unjudgeable. Dropping it would let a wire-format change silently empty every
/// injection — the fail-open inversion this fix exists to prevent.
/// What: a drawer with `score: None` through the filter at the default floor.
/// Test: itself.
#[test]
fn relevance_floor_keeps_drawer_without_score() {
    use filter::{filter_drawers_by_relevance_floor, RecalledDrawer};
    let drawers = vec![RecalledDrawer {
        content: "daemon predates the score field".into(),
        tags: Vec::new(),
        layer: Some(2),
        score: None,
    }];
    let out = filter_drawers_by_relevance_floor(drawers, DEFAULT_RELEVANCE_FLOOR);
    assert_eq!(out.kept.len(), 1, "unknown score must not mean dropped");
    assert_eq!(out.withheld, 0);
}

/// Why (issue #5037, requirement 4): a partial drop must tell the reader more
/// exists, so "the model saw everything relevant" is never assumed wrongly.
/// What: composes with two kept drawers and three withheld; asserts the count
/// and the `memory_recall` pointer both render.
/// Test: itself.
#[test]
fn compose_injection_announces_withheld_drawers() {
    use filter::RecalledDrawer;
    use format::compose_injection;
    let drawers: Vec<RecalledDrawer> = (0..2)
        .map(|i| RecalledDrawer {
            content: format!("kept drawer {i}"),
            tags: Vec::new(),
            layer: Some(2),
            score: Some(0.7),
        })
        .collect();
    let out = compose_injection(None, &drawers, 3, &[], Some("alpha"));
    assert!(out.contains("kept drawer 0"), "kept content must render");
    assert!(
        out.contains("3 further memories withheld"),
        "the withheld count must be visible; got:\n{out}"
    );
    assert!(
        out.contains("memory_recall"),
        "the notice must point at how to see past the floor; got:\n{out}"
    );
}

/// Why (#5819, superseding #5037 requirement 4 for the total-silence case):
/// #5037 announced an all-withheld recall so the reader could tell it apart
/// from an empty palace. That sentence rendered on every prompt with no good
/// match — most prompts — and spent ~200 bytes per turn to report having
/// nothing to say. The owner's ruling is that emitting nothing costs nothing.
/// This test is the inversion of `compose_injection_announces_total_silence`,
/// which it replaces: the same two inputs, the opposite expectation.
/// What: composes with zero kept and five withheld, then with zero of both.
/// Asserts BOTH render as an empty injection, and that no fragment of the
/// retired notice survives anywhere.
/// Test: itself. Fails against the pre-#5819 composer, which returns ~200
/// bytes for the first case.
#[test]
fn compose_injection_is_silent_when_everything_was_withheld() {
    use format::compose_injection;
    let silenced = compose_injection(None, &[], 5, &[], Some("alpha"));
    assert!(
        silenced.is_empty(),
        "an all-withheld recall must emit nothing at all; got {} bytes:\n{silenced}",
        silenced.len()
    );

    let nothing_existed = compose_injection(None, &[], 0, &[], Some("alpha"));
    assert!(
        nothing_existed.is_empty(),
        "zero candidates must emit nothing; got:\n{nothing_existed}"
    );

    // The retired wording must not reappear behind some other branch.
    for probe in [
        compose_injection(None, &[], 1, &[], Some("alpha")),
        compose_injection(None, &[], 99, &[], None),
    ] {
        assert!(
            !probe.contains("cleared the relevance floor"),
            "the total-silence notice is retired; got:\n{probe}"
        );
    }
}

/// Why (#5819, ADR-0028 D7): the KG section had no length control beyond the
/// triple count, and a triple's rendered width is unbounded. D7 allocates it
/// 256 bytes.
/// What: hands the composer ten hot triples whose rendered lines total well
/// past the cap, and asserts the emitted section stays inside it while still
/// carrying at least one fact.
/// Test: itself. Fails against the pre-#5819 composer, which renders all ten.
#[test]
fn compose_injection_caps_kg_section_bytes() {
    use filter::RawTriple;
    use format::{compose_injection, KG_SECTION_BYTE_CAP};
    let triples: Vec<RawTriple> = (0..10)
        .map(|i| RawTriple {
            subject: format!("subject-with-a-long-name-{i}"),
            predicate: "has_convention".into(),
            object: format!("an object string long enough to matter, number {i}"),
        })
        .collect();
    let out = compose_injection(None, &[], 0, &triples, Some("alpha"));
    assert!(
        out.len() <= KG_SECTION_BYTE_CAP,
        "KG section must stay within {KG_SECTION_BYTE_CAP} bytes, got {}:\n{out}",
        out.len()
    );
    assert!(
        out.contains("subject-with-a-long-name-0"),
        "the cap must trim the tail, not suppress the section; got:\n{out}"
    );
}

/// Why (#5819): a drawer tagged `creator:cwd=/Users/bob/Duetto/cto` — a note
/// about a different repository — was recalled into `trusty-tools` sessions
/// turn after turn. The drawer lives in this palace, so palace resolution
/// cannot exclude it; its own provenance tag is the only signal.
/// What: three drawers — one from another repo, one untagged, one from a
/// subdirectory of the session root — filtered against a session root. Asserts
/// only the foreign one is dropped.
/// Test: itself. Fails against the pre-#5819 pipeline, which has no such
/// filter and returns all three.
#[test]
fn project_scope_drops_foreign_cwd_drawer() {
    use filter::{filter_drawers_by_project_scope, RecalledDrawer};
    let mk = |name: &str, tags: Vec<String>| RecalledDrawer {
        content: name.to_string(),
        tags,
        layer: Some(2),
        score: Some(0.7),
    };
    let drawers = vec![
        mk(
            "duetto",
            vec!["creator:cwd=/Users/bob/Duetto/cto".to_string()],
        ),
        mk("untagged", vec!["worktree".to_string()]),
        mk(
            "in-tree",
            vec!["creator:cwd=/users/bob/proj/trusty-tools/crates/trusty-memory".to_string()],
        ),
    ];
    let out = filter_drawers_by_project_scope(drawers, Some("/users/bob/proj/trusty-tools"));
    let names: Vec<&str> = out.kept.iter().map(|d| d.content.as_str()).collect();
    assert_eq!(
        names,
        vec!["untagged", "in-tree"],
        "only the other repository's drawer may be dropped"
    );
    assert_eq!(out.dropped, 1, "the drop must be counted, not silent");
}

/// Run one git command inside `dir` and fail the test with git's own stderr.
///
/// Why (#5819): the fixtures below need repositories git will actually answer
/// for, and a silently-failing `git init` would leave a fixture that proves the
/// opposite of what its test claims.
/// What: runs `git -C <dir> <args>` with identity and signing pinned per
/// invocation so the runner's global config cannot fail the commit, then
/// asserts a zero exit.
/// Test: used by `session_project_root_resolves_a_worktree_to_its_main_checkout`
/// and `project_scope_keeps_a_repo_root_writer_when_the_session_is_in_a_crate`.
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git must be on PATH for these fixtures");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Create a git repository at `dir` carrying one empty commit.
///
/// Why: `git worktree add` needs a HEAD, and `rev-parse --show-toplevel` needs
/// a real repository — a hand-made `.git` directory answers neither.
/// What: creates `dir`, runs `git init`, and commits `--allow-empty`.
/// Test: used by `session_project_root_resolves_a_worktree_to_its_main_checkout`
/// and `project_scope_keeps_a_repo_root_writer_when_the_session_is_in_a_crate`.
fn init_test_repo(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).expect("mkdir repo");
    git_in(dir, &["init", "-q"]);
    git_in(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
}

/// Why (#5819): the session root feeding the project-scope filter is resolved
/// from the stdin `cwd`, and a dispatched agent's cwd is almost always a
/// worktree. `git rev-parse --show-toplevel` answers a linked worktree with the
/// worktree itself, which would make every drawer written from the main checkout
/// look foreign — the filter would delete more real content than the leak it
/// exists to stop. `--git-common-dir` is the probe that does not have that
/// property, and this asserts the resolver uses it.
/// What: creates a real repository, adds a real `git worktree` under
/// `.claude/worktrees/`, asserts git wrote the `.git` FILE that makes the
/// worktree its own toplevel, and asserts the resolved root is the checkout
/// anyway.
/// Test: itself. Fails against any resolver keyed on `--show-toplevel` or on
/// `find_project_root`'s marker walk, both of which stop at the worktree.
#[test]
fn session_project_root_resolves_a_worktree_to_its_main_checkout() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("trusty-tools");
    init_test_repo(&root);
    let worktree = root.join(".claude/worktrees/agent-abc123");
    git_in(
        &root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-abc123",
            &worktree.to_string_lossy(),
        ],
    );
    // Production takes this path: git marks a linked worktree with a `.git`
    // FILE, and answers `--show-toplevel` from inside it with the worktree.
    assert!(
        worktree.join(".git").is_file(),
        "fixture must reproduce git's linked-worktree `.git` file"
    );

    let payload = serde_json::json!({ "cwd": worktree.to_string_lossy() }).to_string();
    let resolved = resolve_session_project_root(&payload).expect("root resolves");

    let expected = filter::normalise_project_path(
        &std::fs::canonicalize(&root)
            .expect("canonicalize root")
            .to_string_lossy(),
    );
    assert_eq!(
        resolved, expected,
        "a worktree cwd must resolve to the checkout that owns it"
    );
}

/// Why (#5819): the filter's own contract is that it fails OPEN, and it did the
/// opposite in the commonest shape there is. The session root came from a marker
/// walk that counts `Cargo.toml` and stops at the first hit, so a session
/// working inside `crates/trusty-memory` resolved its root to that crate while
/// `cwd_palace_slug_at` resolved the palace to the workspace. Recall then ran
/// against the right palace and this filter discarded every drawer written from
/// the repo root or a sibling crate — no count, no log line.
/// What: builds a real repository laid out as a Cargo workspace, resolves the
/// session root from a payload whose `cwd` is the crate directory, and asserts a
/// repo-root writer and a sibling-crate writer both survive while a genuinely
/// foreign repository is still dropped. The drawer cwds are the uncanonicalised
/// temp paths, so this also covers the symlink rescue — macOS hands out
/// `/var/folders/…` for a `/private/var/folders/…` tree.
/// Test: itself. Fails against `0f24dbe65`, where both in-tree writers are
/// dropped.
#[test]
fn project_scope_keeps_a_repo_root_writer_when_the_session_is_in_a_crate() {
    use filter::{filter_drawers_by_project_scope, RecalledDrawer};
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("trusty-tools");
    init_test_repo(&repo);
    let crate_dir = repo.join("crates/trusty-memory");
    let sibling_crate = repo.join("crates/trusty-search");
    let foreign = tmp.path().join("some-other-repo");
    for d in [&crate_dir, &sibling_crate, &foreign] {
        std::fs::create_dir_all(d).expect("mkdir");
    }
    // The marker that used to truncate the session root to the crate.
    std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").expect("workspace manifest");
    std::fs::write(crate_dir.join("Cargo.toml"), "[package]\n").expect("crate manifest");

    let payload = serde_json::json!({ "cwd": crate_dir.to_string_lossy() }).to_string();
    let root = resolve_session_project_root(&payload).expect("session root resolves");

    let mk = |dir: &std::path::Path| {
        let cwd = dir.to_string_lossy().to_string();
        RecalledDrawer {
            content: cwd.clone(),
            tags: vec![format!("creator:cwd={cwd}")],
            layer: Some(2),
            score: Some(0.7),
        }
    };
    let out = filter_drawers_by_project_scope(
        vec![mk(&repo), mk(&sibling_crate), mk(&foreign)],
        Some(&root),
    );
    let names: Vec<&str> = out.kept.iter().map(|d| d.content.as_str()).collect();
    assert_eq!(
        names,
        vec![
            repo.to_string_lossy().as_ref(),
            sibling_crate.to_string_lossy().as_ref()
        ],
        "a session inside one crate must keep the repo root and its sibling crates; \
         session root resolved to {root}"
    );
}

/// Why (#5819): the containment test is a path test, not a string prefix — a
/// sibling checkout named `trusty-tools-fork` must not read as inside
/// `trusty-tools`. The filter must also fail open on the three inputs it cannot
/// judge: no session root, no `creator:cwd` tag, an empty one.
/// What: asserts a worktree writer, a nested-subdirectory writer, and a writer
/// recorded with a trailing slash and mixed case all survive; that the
/// name-prefix sibling does not; and that each fail-open input keeps its drawer.
/// Test: itself. The worktree writer here is kept by plain containment — it sits
/// under the root. Worktree identity on the SESSION side is
/// `session_project_root_resolves_a_worktree_to_its_main_checkout`.
#[test]
fn project_scope_keeps_in_tree_writers_and_drops_prefix_siblings() {
    use filter::{filter_drawers_by_project_scope, RecalledDrawer};
    let root = "/users/bob/proj/trusty-tools";
    let mk = |cwd: &str| RecalledDrawer {
        content: cwd.to_string(),
        tags: vec![format!("creator:cwd={cwd}")],
        layer: Some(2),
        score: Some(0.7),
    };
    let worktree = format!("{root}/.claude/worktrees/agent-abc123");
    let nested = format!("{root}/crates/trusty-memory/src");
    let shouty = "/Users/Bob/Proj/Trusty-Tools/crates/";
    let sibling = "/users/bob/proj/trusty-tools-fork";

    let out = filter_drawers_by_project_scope(
        vec![mk(&worktree), mk(&nested), mk(shouty), mk(sibling)],
        Some(root),
    );
    let names: Vec<&str> = out.kept.iter().map(|d| d.content.as_str()).collect();
    assert_eq!(
        names,
        vec![worktree.as_str(), nested.as_str(), shouty],
        "worktrees, subdirectories, and case/slash variants are this project; \
         a name-prefix sibling is not"
    );

    // The three fail-open inputs.
    let untagged = RecalledDrawer {
        content: "untagged".into(),
        tags: vec!["worktree".into()],
        layer: Some(2),
        score: Some(0.7),
    };
    let empty_cwd = mk("   ");
    let open = filter_drawers_by_project_scope(vec![untagged, empty_cwd], Some(root));
    assert_eq!(
        open.kept.len(),
        2,
        "a drawer with no recorded cwd, or an empty one, is unjudgeable and stays"
    );
    assert_eq!(open.dropped, 0);
    let no_root = filter_drawers_by_project_scope(vec![mk(sibling)], None);
    assert_eq!(
        no_root.kept.len(),
        1,
        "an unresolvable session root disables the filter rather than dropping content"
    );
    assert_eq!(
        no_root.dropped, 0,
        "the disabled filter must report a zero drop, not an unreported one"
    );
}

/// Why (#5819): two fail-closed defects shipped through this filter unnoticed
/// because it dropped drawers silently. The relevance floor beside it has always
/// reported `withheld`; the operator had no equivalent number here, so recall
/// degrading to only the untagged drawers looked like an empty palace.
/// What: filters four drawers against a root two of them sit outside, and
/// asserts both halves of the pair — a drop count with no denominator does not
/// say whether the drop is expected.
/// Test: itself. Fails against `0ac9e1f4`, where the filter returns a bare
/// `Vec` and no count exists to assert.
#[test]
fn project_scope_counts_what_it_drops() {
    use filter::{filter_drawers_by_project_scope, RecalledDrawer};
    let root = "/users/bob/proj/trusty-tools";
    let mk = |cwd: &str| RecalledDrawer {
        content: cwd.to_string(),
        tags: vec![format!("creator:cwd={cwd}")],
        layer: Some(2),
        score: Some(0.7),
    };
    let out = filter_drawers_by_project_scope(
        vec![
            mk(root),
            mk("/users/bob/proj/trusty-tools/crates/trusty-memory"),
            mk("/users/bob/duetto/cto"),
            mk("/users/bob/proj/some-other-repo"),
        ],
        Some(root),
    );
    assert_eq!((out.kept.len(), out.dropped), (2, 2));
}

/// Why (#5819): inside a submodule — or any repo created with
/// `--separate-git-dir` — `git rev-parse --git-common-dir` is
/// `<outer>/.git/modules/<name>`, so the parent the resolver used to trust is
/// `<outer>/.git/modules`. That is not a working tree, so no drawer's recorded
/// cwd can sit inside it: every provenance-tagged drawer failed the containment
/// test, then canonicalised successfully (the internals directory exists) and
/// failed it again, and was dropped. Untagged drawers survived, so recall
/// degraded instead of emptying.
/// What: builds the `.git`-file structure a submodule produces, resolves the
/// session root from a payload whose `cwd` is the child checkout, asserts the
/// resolver declines rather than naming the internals directory, and asserts a
/// drawer recorded inside that child is KEPT.
/// Test: itself. Fails against `0ac9e1f4`, where the root resolves to
/// `<outer>/.git/modules` and the drawer is dropped.
#[test]
fn session_project_root_is_none_inside_a_separate_git_dir_child() {
    use filter::{filter_drawers_by_project_scope, RecalledDrawer};
    let tmp = tempfile::tempdir().expect("tempdir");
    let outer = tmp.path().join("outer");
    let child = outer.join("sub");
    std::fs::create_dir_all(&child).expect("mkdir child");
    init_test_repo(&outer);
    let modules = outer.join(".git/modules");
    std::fs::create_dir_all(&modules).expect("mkdir modules");
    git_in(
        &child,
        &[
            "init",
            "-q",
            &format!("--separate-git-dir={}", modules.join("sub").display()),
            ".",
        ],
    );
    assert!(
        child.join(".git").is_file(),
        "fixture must reproduce the `.git` FILE a submodule checkout carries"
    );

    let payload = serde_json::json!({ "cwd": child.to_string_lossy() }).to_string();
    let resolved = resolve_session_project_root(&payload);
    assert_eq!(
        resolved, None,
        "a root that is not a working tree must disable the filter, not become it"
    );

    let cwd = child.to_string_lossy().to_string();
    let drawer = RecalledDrawer {
        content: cwd.clone(),
        tags: vec![format!("creator:cwd={cwd}")],
        layer: Some(2),
        score: Some(0.7),
    };
    let out = filter_drawers_by_project_scope(vec![drawer], resolved.as_deref());
    assert_eq!(
        (out.kept.len(), out.dropped),
        (1, 0),
        "a drawer written inside the child checkout is this project's own"
    );
}

/// Why (issue #5037): the floor is required to be *configurable*, including
/// settable to zero to restore pre-fix behaviour. The clamp is the only thing
/// standing between an operator typo and a disabled or total-blackout gate.
/// What: exercises `clamp_floor` across unset, valid, zero, out-of-range,
/// unparseable, and NaN inputs.
/// Test: itself.
#[test]
fn configured_relevance_floor_clamps_to_bounds() {
    assert_eq!(clamp_floor(None), DEFAULT_RELEVANCE_FLOOR);
    assert_eq!(clamp_floor(Some("0.5")), 0.5);
    assert_eq!(clamp_floor(Some(" 0.42 ")), 0.42);
    assert_eq!(clamp_floor(Some("0")), 0.0, "zero must disable the gate");
    assert_eq!(clamp_floor(Some("-3")), 0.0);
    assert_eq!(clamp_floor(Some("9")), 1.0);
    assert_eq!(clamp_floor(Some("banana")), DEFAULT_RELEVANCE_FLOOR);
    assert_eq!(clamp_floor(Some("NaN")), DEFAULT_RELEVANCE_FLOOR);
}

/// Why (issue #5037, requirement 2): the K ceiling moved with the default, and
/// a ceiling below the default would silently cap every operator override.
/// What: exercises `clamp_top_k` across unset, valid, zero, over-ceiling, and
/// unparseable inputs.
/// Test: itself.
#[test]
fn configured_top_k_clamps_to_bounds() {
    assert_eq!(clamp_top_k(None), DEFAULT_TOP_K);
    assert_eq!(clamp_top_k(Some("7")), 7);
    assert_eq!(clamp_top_k(Some("0")), DEFAULT_TOP_K);
    assert_eq!(clamp_top_k(Some("999")), MAX_TOP_K);
    assert_eq!(clamp_top_k(Some("nope")), DEFAULT_TOP_K);
}

/// Why (issue #5037, requirement 3): the recall query must be the whole user
/// input. `hook_prompt_excerpt` exists a few lines away in the same module and
/// truncates for telemetry — wiring it into the query by mistake would fragment
/// every recall silently. Pin that `parse_user_prompt` hands back the prompt
/// entire.
/// What: a 12,000-character prompt through `parse_user_prompt`; asserts the
/// result is byte-identical, and that the telemetry excerpt is not.
/// Test: itself.
#[test]
fn recall_query_is_the_whole_prompt() {
    let long: String = "explain the retrieval floor and why it matters. "
        .repeat(400)
        .trim_end()
        .to_string();
    assert!(long.len() > 12_000, "fixture must exceed any plausible cap");
    let payload = serde_json::json!({ "prompt": long, "cwd": "/tmp" }).to_string();

    let parsed = parse_user_prompt(&payload);
    assert_eq!(parsed, long, "the recall query must not be truncated");

    // The telemetry excerpt IS truncated — that asymmetry is the point.
    assert!(
        crate::hook_prompt_excerpt(&parsed).len() < parsed.len(),
        "excerpt helper must stay distinct from the query path"
    );
}

/// Why (issue #125): when Claude Code invokes the UserPromptSubmit hook,
/// the stdin JSON carries a `cwd` field that reflects the user's actual
/// working directory at prompt time. The hook process cwd may be where
/// the hook was registered (typically a fixed install root), not where
/// the user actually is. The log palace must follow the stdin `cwd`.
/// What: build a stdin JSON payload pointing at a tempdir, derive the
/// expected slug for that tempdir via the *_at variant, and assert
/// `resolve_palace_for_log` returns the same slug — even though the
/// process cwd is unchanged and would resolve to a different slug.
/// Test: itself.
#[test]
fn resolve_palace_for_log_prefers_stdin_cwd() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let project = tmp.path().join("stdin-driven-project");
    std::fs::create_dir_all(&project).expect("create project dir");
    let payload = serde_json::json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project.to_string_lossy(),
        "prompt": "hello"
    })
    .to_string();

    let expected =
        crate::messaging::cwd_palace_slug_at(&project).expect("derive slug from stdin cwd");
    let got = resolve_palace_for_log(&payload);
    assert_eq!(
        got, expected,
        "stdin `cwd` must override the process cwd for the log palace slug"
    );
    assert!(
        got.contains("stdin-driven-project"),
        "expected slug derived from stdin path, got {got:?}"
    );
}

/// Why (issue #125): when stdin is empty or non-JSON, the helper must
/// fall through to the process-cwd resolution path so manual `trusty-
/// memory prompt-context` invocations from a TTY still get a useful
/// palace identifier.
/// What: pass an empty string and a non-JSON string; assert the result
/// is *not* the legacy `"<unknown>"` sentinel (the process cwd here is a
/// real git repo, so cwd_palace_slug succeeds).
/// Test: itself.
#[test]
fn resolve_palace_for_log_falls_back_to_process_cwd() {
    let from_empty = resolve_palace_for_log("");
    let from_garbage = resolve_palace_for_log("not json at all");
    assert_eq!(from_empty, from_garbage);
    assert_ne!(from_empty, "<unknown>");
}

/// Why: the hook is wired into every Claude Code prompt the user types;
/// failing it would block the prompt. The contract is that a missing
/// daemon-address lockfile (the canonical "daemon not running" signal)
/// must produce `Ok(())` with no stdout, not an error.
/// What: redirects `trusty_common::resolve_data_dir` at a fresh tempdir
/// via `TRUSTY_DATA_DIR_OVERRIDE` so the derived socket path is nowhere
/// and the dial is refused, then runs the handler and asserts it
/// returns `Ok(())`. Calls `handle_prompt_context_with_payload` directly
/// (issue #2079) with a fixed empty payload instead of
/// `handle_prompt_context()` so the test never spawns a real blocking
/// stdin read that could outlive the test's `Runtime` teardown.
#[tokio::test]
async fn prompt_context_returns_ok_without_daemon() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    // SAFETY: tests serialise on `TRUSTY_DATA_DIR_OVERRIDE` by convention
    // across the trusty-* workspace (see trusty-common's lib.rs notes).
    // This test only mutates the env var inside its own scope.
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
    }
    let res = handle_prompt_context_with_payload(String::new()).await;
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }
    assert!(
        res.is_ok(),
        "missing daemon lockfile must degrade to Ok(()), got {res:?}"
    );
}

/// Why (issue #134): the hook's whole value-prop is surfacing relevant
/// drawers from the palace; previously it only returned the workspace-
/// level hot facts. Confirm that with a live daemon, a populated palace,
/// and a prompt that mentions a known keyword, the rendered injection
/// contains real drawer content — not the legacy `EMPTY_PLACEHOLDER`.
/// What: spin up a real HTTP daemon under a tempdir-pinned data root,
/// create a palace whose slug matches a project tempdir basename,
/// populate it with three keyworded drawers via the MCP dispatch
/// (which loads the real embedder), then call `build_injection_body`
/// with a stdin payload carrying `cwd = <project tempdir>` and
/// `prompt = "how does rust integration work?"`. Assert the body
/// contains the rust drawer's content and the relevant-memories
/// section header.
/// Test: itself.
/// Note (issue #226): gated on `axum-server` because it spins up the
/// real HTTP daemon via `run_http_on`.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn prompt_context_recalls_palace_drawers() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-recall-pop").await;

    // Populate the palace with three diverged drawers via MCP dispatch.
    // Using the MCP path here exercises the real embedder + KG hook.
    for (text, tags) in [
        (
            "Rust integration uses tokio for async tasks and serde for JSON",
            vec!["rust", "tokio"],
        ),
        (
            "Python bindings ship via PyO3 with custom ABI shims",
            vec!["python", "pyo3"],
        ),
        (
            "Knowledge graph stores triples in redb with valid_from intervals",
            vec!["kg", "redb"],
        ),
    ] {
        let tags_json: Vec<serde_json::Value> = tags.iter().map(|t| json!(t)).collect();
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": slug,
                "text": text,
                "room": "General",
                "tags": tags_json,
            }),
        )
        .await
        .expect("memory_remember");
    }

    // Build the stdin payload Claude Code would send: a JSON object with
    // `cwd` (the project dir) and `prompt` (mentioning "rust").
    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "how does rust integration work?"
    })
    .to_string();

    let start = std::time::Instant::now();
    let body = build_injection_body(&payload).await;
    let elapsed_ms = start.elapsed().as_millis();
    eprintln!("prompt_context_recalls_palace_drawers latency: {elapsed_ms}ms");

    // #5819: an empty body is now legal, so a `!=` against the placeholder no
    // longer proves recall worked. Assert real content instead.
    assert!(
        !body.is_empty() && body != EMPTY_PLACEHOLDER,
        "populated palace must return real content; got:\n{body}"
    );
    // The injection must mention the rust drawer's content (proves
    // recall actually targeted the resolved palace and surfaced
    // prompt-relevant memories).
    assert!(
        body.to_lowercase().contains("rust") && body.to_lowercase().contains("integration"),
        "expected rust integration drawer in injection; got:\n{body}"
    );
    // Section header should be present (proves the multi-section
    // composition is wired through).
    assert!(
        body.contains("Relevant memories") || body.contains("memories from palace"),
        "expected a `Relevant memories` section; got:\n{body}"
    );

    // Performance guardrail (issue #134 target: <200 ms p95). On
    // CI/dev machines this comfortably stays under the budget.
    assert!(
        elapsed_ms < 5_000,
        "prompt-context too slow ({elapsed_ms}ms) — investigate"
    );

    addr_handle.shutdown().await;
}

/// Why (#5810): the header named the DERIVED slug while the drawers under it were
/// recalled through a palace-level alias, so it printed a name `palace_list` does
/// not return and `memory_recall` / `palace_info` / `memory_remember` cannot take
/// as input. This is the split-brain shape from the field: an `owner-repo` slug
/// with no directory of its own, aliased to the bare-repo palace that holds the
/// drawers.
/// What: creates the canonical palace with one drawer, then a second project dir
/// pinned to a slug that owns no palace, and registers the alias between them.
/// Drives `build_injection_body` from the aliased cwd and asserts the header names
/// the canonical palace — and that the unusable derived slug appears nowhere in the
/// injection. Pre-#5810 this fails on the first assertion: the drawer arrives
/// (the daemon redirects) under a header reading the derived slug.
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn prompt_context_header_names_the_alias_target() {
    let _guard = crate::commands::env_test_lock().lock().await;
    unsafe {
        std::env::remove_var(ENV_MIN_SCORE);
        std::env::remove_var(ENV_RECALL_DENY_TAGS);
    }
    let (state, _data_dir_tmp, project_dir_tmp, _project_dir, canonical, addr_handle) =
        spin_up_test_daemon_with_palace("ptx-alias-canonical").await;

    let _ = crate::tools::dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": canonical,
            "text": "Rust integration uses tokio for async tasks and serde for JSON",
            "room": "General",
            "tags": ["rust", "tokio"],
        }),
    )
    .await
    .expect("memory_remember");

    // The derived slug: a project dir pinned to a palace that was never created.
    let derived = "ptx-alias-derived";
    let aliased_project = project_dir_tmp.path().join(derived);
    std::fs::create_dir_all(&aliased_project).expect("aliased project dir");
    crate::project_root::write_project_pin(
        &aliased_project,
        &crate::project_root::ProjectPin::new(derived),
    )
    .expect("write pin for the aliased project");

    let registry_dir =
        trusty_common::palace_alias::default_palace_registry_dir().expect("registry dir");
    assert!(
        !registry_dir.join(derived).join("palace.json").exists(),
        "the derived slug must own no palace, or this test proves nothing"
    );
    trusty_common::palace_alias::PalaceAliasStore::register_alias(
        &registry_dir,
        derived,
        &canonical,
    )
    .expect("register palace alias");

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": aliased_project.to_string_lossy(),
        "prompt": "how does rust integration work?"
    })
    .to_string();
    let body = build_injection_body(&payload).await;

    assert!(
        body.contains(&format!("## Relevant memories from palace `{canonical}`")),
        "the header must name the palace the drawers came from; got:\n{body}"
    );
    assert!(
        !body.contains(derived),
        "the pre-redirect slug is not a usable memory-tool input and must not \
         appear in the injection; got:\n{body}"
    );

    addr_handle.shutdown().await;
}

/// Why (issue #5037 end to end, outcome inverted by #5819): the unit tests
/// above pin the floor over synthetic scores. This one proves the whole chain —
/// real embedder, real HTTP recall, real `score` on the wire, real filter —
/// turns an off-topic prompt into zero injected bytes. #5037 required a visible
/// withheld notice here; #5819 requires silence, because that notice rendered
/// on every prompt nothing answered and spent tokens to report having nothing
/// to say. The probe query is unchanged ("what is the capital of France", which
/// returned five drawers at 0.15 rendered as if they matched).
/// What: populates a palace with three Rust/Python/KG drawers, then submits a
/// prompt about none of them. Asserts no drawer content reaches the injection
/// and that the body is empty.
/// Test: itself. Fails against the pre-#5819 handler, which emits the
/// "cleared the relevance floor" paragraph.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn prompt_context_off_topic_prompt_injects_nothing() {
    let _guard = crate::commands::env_test_lock().lock().await;
    unsafe {
        std::env::remove_var(ENV_MIN_SCORE);
        std::env::remove_var(ENV_RECALL_DENY_TAGS);
    }
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-floor-e2e").await;

    for (text, tags) in [
        (
            "Rust integration uses tokio for async tasks and serde for JSON encoding",
            vec!["rust", "tokio"],
        ),
        (
            "Python bindings ship via PyO3 with custom ABI shims for the runtime",
            vec!["python", "pyo3"],
        ),
        (
            "Knowledge graph stores triples in redb with valid_from intervals per edge",
            vec!["kg", "redb"],
        ),
    ] {
        let tags_json: Vec<serde_json::Value> = tags.iter().map(|t| json!(t)).collect();
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": slug,
                "text": text,
                "room": "General",
                "tags": tags_json,
            }),
        )
        .await
        .expect("memory_remember");
    }

    // The probe query from #5037 — nothing in this palace answers it.
    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "what is the capital of France"
    })
    .to_string();
    let body = build_injection_body(&payload).await;

    for leaked in ["tokio", "PyO3", "valid_from"] {
        assert!(
            !body.contains(leaked),
            "off-topic prompt must not inject `{leaked}`; got:\n{body}"
        );
    }
    // #5819: this asserted `body.contains("relevance floor")`. Going silent is
    // now the required outcome, and this is the end-to-end proof of it — the
    // whole path, through a real daemon and a real populated palace, emits
    // zero bytes for a prompt nothing answers.
    assert!(
        body.is_empty(),
        "an off-topic prompt must inject nothing at all; got {} bytes:\n{body}",
        body.len()
    );

    addr_handle.shutdown().await;
}

/// Why (issue #134, negative case; contract inverted by #5819): when the
/// resolved palace has no drawers AND no global hot facts have been asserted,
/// the hook must emit nothing. It used to emit `EMPTY_PLACEHOLDER`, which
/// spent bytes on every firing to report an absence.
/// What: spin up the same daemon shape but skip the drawer-population
/// step; assert the body is empty.
/// Test: itself. Fails against the pre-#5819 handler, which returns
/// `"No prompt facts stored yet."`.
/// Note (issue #226): gated on `axum-server`; spawns the HTTP daemon.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn prompt_context_empty_palace_falls_back_to_global() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let (_state, _data_dir_tmp, _project_dir_tmp, project_dir, _slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-recall-empty").await;

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "no drawers exist here"
    })
    .to_string();
    let body = build_injection_body(&payload).await;
    // #5819: this used to assert `EMPTY_PLACEHOLDER`. An empty palace now
    // injects nothing at all — the placeholder spent 27 bytes per firing to
    // say so, on a hook that fires on every prompt of every session.
    assert!(
        body.is_empty(),
        "empty palace + empty prompt-facts must inject nothing; got {} bytes:\n{body}",
        body.len()
    );

    addr_handle.shutdown().await;
}

/// Test fixture: spin up a real daemon under a tempdir-pinned data
/// root, create a palace with the given slug under a project tempdir
/// whose basename matches the slug, and return everything the test
/// needs to interact with the daemon.
///
/// Why: the prompt-context hook dials a live daemon; a test with the
/// dispatcher alone cannot exercise the socket round trip the hook
/// actually makes. This helper wires it together in one place.
/// What: creates two tempdirs (one for the data root, one for the
/// project cwd whose basename equals `palace_slug`), pins
/// `TRUSTY_DATA_DIR_OVERRIDE`, builds the `AppState`, creates the
/// palace, and serves it. Returns `(state, data_dir_tmp,
/// project_dir_tmp, project_dir_path, palace_slug, addr_handle)`.
/// Test: indirectly via `prompt_context_recalls_palace_drawers` and
/// `prompt_context_empty_palace_falls_back_to_global`.
/// Note (#226, #6286): gated on `daemon` because the serving surface
/// is only compiled in behind that feature.
#[cfg(feature = "daemon")]
async fn spin_up_test_daemon_with_palace(
    palace_slug: &str,
) -> (
    crate::AppState,
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    String,
    DaemonHandle,
) {
    // Seed the process-wide `retrieval::shared_embedder()` OnceCell with
    // `MockEmbedder`: every caller of this helper writes fixture drawers via
    // `memory_remember`, which embeds. The cell is first-writer-wins for the
    // whole process, so under `cargo test` a sibling test's seed satisfied
    // these; under per-test process isolation (`cargo nextest run`) each gets a
    // virgin cell and reaches for the real ONNX model (HTTP 429 in CI) — same
    // defect class as #4413. The call is idempotent, so order does not matter.
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let data_tmp = tempfile::tempdir().expect("data tempdir");
    let project_tmp = tempfile::tempdir().expect("project tempdir");
    // Build a project directory whose basename equals the palace slug.
    // This is what `cwd_palace_slug_at` will derive from the stdin `cwd`.
    let project_dir = project_tmp.path().join(palace_slug);
    std::fs::create_dir_all(&project_dir).expect("project dir");

    // SAFETY: env_test_lock serialises this section.
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, data_tmp.path());
        std::env::remove_var(crate::prompt_log::ENV_ENABLED);
        std::env::remove_var(crate::prompt_log::ENV_DIR);
        std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
        // #5810: `TRUSTY_MEMORY_PALACE` is precedence level 1 and outranks the
        // pin file written below, so an ambient value — every trusty-mpm managed
        // session exports one — made these tests resolve the developer's own
        // palace instead of the fixture's. Every one of them then recalled
        // nothing and asserted against `EMPTY_PLACEHOLDER`. Scrub it here so the
        // pin is authoritative, matching what the comment below already claims.
        std::env::remove_var(trusty_common::PALACE_OVERRIDE_ENV);
        // Issue #88: bypass palace-slug enforcement so test palaces with
        // arbitrary names can be created without a matching project root.
        std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
    }

    // Issue #1217: the default palace ID is now derived from project identity
    // (git owner/repo, else parent/dir slug), so a bare tempdir whose basename
    // equals `palace_slug` no longer resolves to that slug (a non-git dir
    // derives `<parent>-<leaf>`). Write a committed pin file in `project_dir`
    // so the hook's `cwd_palace_slug_at` resolves deterministically to the
    // palace this fixture created. This is fully hermetic (per-tempdir, no env
    // or global state to leak into sibling tests) and exercises the #1217
    // pin-file-primacy anchor that keeps existing palaces from being orphaned.
    crate::project_root::write_project_pin(
        &project_dir,
        &crate::project_root::ProjectPin::new(palace_slug.to_string()),
    )
    .expect("write project pin for fixture");

    let data_root =
        trusty_common::resolve_data_dir("trusty-memory").expect("resolve data dir under override");
    let state = crate::AppState::new(data_root.clone());
    // Flip to Ready so the issue #911 warming preflight does not reject
    // the `memory_remember` calls that seed fixture data below.
    state.set_ready();

    // Create the palace via MCP dispatch so the on-disk metadata
    // matches what a real client would have produced. The pin file written
    // above makes `cwd_palace_slug_at` (and thus the hook) resolve to exactly
    // this slug (issue #1217).
    let _ = crate::tools::dispatch_tool(&state, "palace_create", json!({"name": palace_slug}))
        .await
        .expect("palace_create");

    // #6286: serve the real daemon on the socket the hook derives. The data-dir
    // override is already in force, so `socket_path()` resolves inside this
    // fixture's tempdir and the hook under test computes the same path — which
    // is what replaces the `http_addr` file this used to write and poll for.
    let socket = crate::transport::uds::socket_path().expect("socket path under the override");
    let state_for_server = state.clone();
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    let serve_socket = socket.clone();
    let handle = tokio::spawn(async move {
        let _ =
            crate::transport::uds::serve_with_shutdown(state_for_server, &serve_socket, async {
                let _ = shutdown.await;
            })
            .await;
    });

    // Poll for readiness rather than sleeping a fixed interval. Generous so a
    // contended host does not flake: the daemon hydrates every palace before it
    // binds, and five tests share this fixture.
    let mut attempts = 0;
    while attempts < 500 {
        if trusty_common::uds::socket_is_serving(&socket, std::time::Duration::from_millis(200))
            .await
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        attempts += 1;
    }
    assert!(
        attempts < 500,
        "daemon never began serving {} (attempts={attempts})",
        socket.display()
    );

    (
        state,
        data_tmp,
        project_tmp,
        project_dir,
        palace_slug.to_string(),
        DaemonHandle {
            socket,
            stop: Some(stop),
            join: Some(handle),
        },
    )
}

/// Test-only handle to a running daemon.
///
/// `shutdown` asks the serve loop to stop rather than aborting the task, so the
/// real unlink path runs and the next fixture in the same process can bind a
/// fresh socket. The abort is kept as the fallback for a loop that does not
/// return.
#[cfg(feature = "daemon")]
struct DaemonHandle {
    #[allow(dead_code)]
    socket: std::path::PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(feature = "daemon")]
impl DaemonHandle {
    async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(h) = self.join.take() {
            if tokio::time::timeout(std::time::Duration::from_secs(10), &mut { h })
                .await
                .is_err()
            {
                // The loop did not return inside the budget; the task is
                // dropped with the handle either way.
            }
        }
        // Release the pinned data dir override for sibling tests.
        // SAFETY: protected by env_test_lock in the caller.
        unsafe {
            std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
        }
    }
}

/// Why (issue #139): live evidence from the user's `trusty-tools`
/// session showed the prompt-context hook injecting raw past user
/// prompts (drawers tagged `claude-session` / `user-prompt` from an
/// upstream auto-capture hook) on every UserPromptSubmit, dominating
/// real palace knowledge. Filtering by deny-listed tags is the
/// cheapest in-tree fix. Verify the default deny list drops both
/// auto-capture tags AND keeps a signal drawer untouched.
/// What: populate a palace with three drawers — one tagged
/// `claude-session`, one tagged `user-prompt`, one tagged with only
/// signal tags. The prompt mentions a keyword shared by all three so
/// recall returns all three. Assert the injection contains only the
/// signal drawer's content and neither of the deny-listed drawers'
/// content surfaces.
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn prompt_context_recall_filters_deny_tags() {
    let _guard = crate::commands::env_test_lock().lock().await;
    // Defensive: scrub the env override in case a sibling test set it
    // and panicked before its cleanup ran. Both vars are pinned to a
    // known state for this test.
    unsafe {
        std::env::remove_var(ENV_RECALL_DENY_TAGS);
    }
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-deny-tags").await;

    // Three drawers, all mentioning "rust" so recall returns all three.
    // The first two carry the default deny tags and must be filtered;
    // their content is sized above the signal-filter threshold so the
    // remember path accepts them (the deny filter operates on tags,
    // not content length, so the body must be realistic).
    // The third has only signal tags and must survive.
    for (text, tags) in [
        (
            "user: how do I use rust async tokio runtime and serde derive macros in this project to glue an http handler to a kafka producer",
            vec!["claude-session", "user-prompt", "rust"],
        ),
        (
            "user: yes please go ahead and refactor the rust async producer module, this captured prompt fragment should never be surfaced",
            vec!["user-prompt", "rust"],
        ),
        (
            "Rust integration uses tokio for async tasks and serde for JSON",
            vec!["rust", "tokio"],
        ),
    ] {
        let tags_json: Vec<serde_json::Value> = tags.iter().map(|t| json!(t)).collect();
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": slug,
                "text": text,
                "room": "General",
                "tags": tags_json,
            }),
        )
        .await
        .expect("memory_remember");
    }

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "how does rust integration work?"
    })
    .to_string();
    let body = build_injection_body(&payload).await;

    assert!(
        body.contains("tokio") && body.contains("serde"),
        "signal drawer must survive deny filter; got:\n{body}"
    );
    assert!(
        !body.contains("kafka producer"),
        "claude-session-tagged drawer must be filtered out; got:\n{body}"
    );
    assert!(
        !body.contains("captured prompt fragment"),
        "user-prompt-tagged drawer must be filtered out; got:\n{body}"
    );

    addr_handle.shutdown().await;
}

/// Why (issue #139): operators need to widen the deny list at runtime
/// without rebuilding the binary — e.g. when a palace accumulates a
/// project-specific synthetic tag. The env override
/// [`ENV_RECALL_DENY_TAGS`] supplies the comma-separated list.
/// What: set the env override to a custom tag, populate a palace
/// where the only recallable drawer carries that custom tag plus a
/// keyword shared with the prompt, assert the drawer is filtered out
/// and the body falls back to the global / empty placeholder path
/// (no `Relevant memories` section).
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn prompt_context_recall_env_override_extends_deny_list() {
    let _guard = crate::commands::env_test_lock().lock().await;
    // SAFETY: env_test_lock serialises this section.
    unsafe {
        std::env::set_var(ENV_RECALL_DENY_TAGS, "noise-tag");
    }
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-env-deny").await;

    let _ = crate::tools::dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": slug,
            "text": "Rust integration uses tokio and serde for the async layer",
            "room": "General",
            "tags": ["noise-tag", "rust"],
        }),
    )
    .await
    .expect("memory_remember");

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "how does rust integration work?"
    })
    .to_string();
    let body = build_injection_body(&payload).await;

    // The single drawer is filtered, so the body should NOT carry its
    // content. The empty-palace fallback (or the global facts path)
    // takes over.
    assert!(
        !body.contains("tokio and serde"),
        "noise-tag drawer must be filtered when env override targets it; got:\n{body}"
    );

    // Clean up the env override so it does not leak to sibling tests.
    // SAFETY: env_test_lock still held until DaemonHandle::shutdown.
    unsafe {
        std::env::remove_var(ENV_RECALL_DENY_TAGS);
    }
    addr_handle.shutdown().await;
}

/// Why (issue #139, regression): a palace consisting entirely of
/// deny-listed drawers must NOT crash the hook and must NOT inject a
/// `Relevant memories` section. The empty-palace fallback path (the
/// existing global hot-facts route from #136) must kick in instead.
/// What: populate a palace where every drawer carries a deny tag,
/// run the hook, assert the body is either the legacy placeholder OR
/// global-facts-only — crucially, it must not contain any of the
/// drawer content nor a `Relevant memories` section header.
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn prompt_context_recall_all_filtered_falls_back_to_global() {
    let _guard = crate::commands::env_test_lock().lock().await;
    unsafe {
        std::env::remove_var(ENV_RECALL_DENY_TAGS);
    }
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-all-filtered").await;

    // Every drawer is deny-listed. Bodies are sized above the signal
    // filter threshold so memory_remember accepts them — the deny-tag
    // filter operates downstream on tags, not content length.
    for (text, tags) in [
        (
            "user: status update on the rust async rewrite, the kafka consumer should not surface in any prompt-context injection",
            vec!["claude-session", "user-prompt", "rust"],
        ),
        (
            "user: yes please continue with the rust refactor on the producer side, this prompt fragment must be filtered out of recall",
            vec!["claude-session", "rust"],
        ),
    ] {
        let tags_json: Vec<serde_json::Value> = tags.iter().map(|t| json!(t)).collect();
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": slug,
                "text": text,
                "room": "General",
                "tags": tags_json,
            }),
        )
        .await
        .expect("memory_remember");
    }

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "tell me about rust"
    })
    .to_string();
    let body = build_injection_body(&payload).await;

    // No drawer content leaks through and no `Relevant memories`
    // section is rendered — either the global hot-facts section or
    // the empty-placeholder fallback wins.
    assert!(
        !body.contains("kafka consumer") && !body.contains("producer side"),
        "filtered drawer content must not leak; got:\n{body}"
    );
    assert!(
        !body.contains("Relevant memories"),
        "no `Relevant memories` section should render when every drawer is filtered; got:\n{body}"
    );

    addr_handle.shutdown().await;
}

/// Why (issue #105): even when the daemon is down, the hook must still
/// log an attempt entry so operators can see "prompt-context fired N
/// times but the daemon was unreachable" in the JSONL stream.
/// What: pin a tempdir as the data directory, run the handler with no
/// daemon, and assert exactly one log file landed under `<tmp>/logs/`
/// with a single JSONL line whose `injection_kind` is the prompt-context
/// kind. Calls `handle_prompt_context_with_payload` directly (issue
/// #2079) with a fixed empty payload so this test never spawns a real
/// blocking stdin read.
/// Test: itself.
#[tokio::test]
async fn prompt_context_logs_attempt_without_daemon() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        std::env::remove_var(crate::prompt_log::ENV_ENABLED);
        std::env::remove_var(crate::prompt_log::ENV_DIR);
        std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
    }
    let res = handle_prompt_context_with_payload(String::new()).await;
    let logs_dir = trusty_common::resolve_data_dir("trusty-memory")
        .expect("resolve data dir")
        .join("logs");
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }
    assert!(res.is_ok());
    let files: Vec<_> = std::fs::read_dir(&logs_dir)
        .expect("logs dir should be created")
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("enriched-prompts."))
        })
        .collect();
    assert_eq!(
        files.len(),
        1,
        "expected one enriched-prompts log file, got {files:?}"
    );
    let content = std::fs::read_to_string(&files[0]).expect("read log");
    let line = content.lines().next().expect("at least one line");
    let parsed: crate::prompt_log::PromptLogEntry =
        serde_json::from_str(line).expect("parse JSONL");
    assert_eq!(parsed.hook_type, "UserPromptSubmit");
    assert_eq!(parsed.injection_kind, "prompt-context-facts");
}

/// Why (issue #2043): [`bounded_blocking`] is the mechanism that keeps the
/// stdin read from ever hanging the hook. A real `std::io::Stdin` can't be
/// swapped in-process, so this test drives the exact same
/// `spawn_blocking` + `timeout` mechanism with a synthetic closure that
/// sleeps past the deadline, proving the caller gets control back on time
/// with a `None` (fail-open) result rather than waiting for the closure.
/// What: races a closure that sleeps 400 ms against a 100 ms deadline;
/// asserts `None` is returned and wall time stays close to the deadline,
/// not the closure's sleep duration. The 400 ms sleep (rather than
/// something longer) is deliberate: `tokio::test`'s per-test `Runtime`
/// still waits for this abandoned blocking thread to finish during its
/// own teardown (blocking-pool threads cannot be cancelled — this is the
/// same reason `main.rs` calls `std::process::exit` after the
/// `prompt-context` dispatch instead of returning normally), so the test
/// itself pays that thread's full sleep as wall-clock tax regardless of
/// the assertion below. Kept short to bound that tax.
/// Test: itself.
#[tokio::test]
async fn bounded_blocking_times_out_on_slow_closure() {
    let start = std::time::Instant::now();
    let result: Option<String> = bounded_blocking(
        || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            "too-late".to_string()
        },
        std::time::Duration::from_millis(100),
    )
    .await;
    let elapsed = start.elapsed();
    assert_eq!(result, None, "slow closure must fail open to None");
    assert!(
        elapsed < std::time::Duration::from_millis(300),
        "bounded_blocking did not return promptly: elapsed={elapsed:?}"
    );
}

/// Why (issue #2043): the deadline mechanism must not clip a value that
/// finishes comfortably within budget — only genuinely slow closures
/// should fail open.
/// What: races an instantly-returning closure against a 300 ms deadline;
/// asserts the real value comes back.
/// Test: itself.
#[tokio::test]
async fn bounded_blocking_returns_value_when_fast_enough() {
    let result =
        bounded_blocking(|| "fast".to_string(), std::time::Duration::from_millis(300)).await;
    assert_eq!(result, Some("fast".to_string()));
}

/// Why (issue #2043): proves the end-to-end fix — before this issue, a
/// daemon that accepted a connection but never answered could block
/// `handle_prompt_context` for as long as the daemon took (bounded only by
/// Claude Code's own ~15 s hook ceiling, which is exactly the hang this
/// issue reports). This test stands up a real HTTP listener that accepts
/// every request and sleeps 5 s before responding — far longer than
/// [`BODY_DEADLINE`] + [`EMIT_DEADLINE`] — and asserts the hook still
/// returns `Ok(())` well inside its own deadline budget.
/// What: writes the slow listener's address as the discovered daemon addr
/// (via `write_daemon_addr`, matching what a real running daemon would
/// have written), runs `handle_prompt_context_with_payload` (issue #2079 —
/// direct call with a fixed empty payload so this test never spawns a
/// real blocking stdin read), and asserts both the `Ok(())` contract and
/// a bounded wall-clock time.
/// Test: itself.
/// Note (#6286): the stalled daemon is now a bare hardened socket that accepts
/// a connection and never answers — a listener that reads the request frame and
/// then sleeps is exactly the stall this test exists to survive, and it needs no
/// HTTP server to build.
#[cfg(feature = "daemon")]
#[tokio::test(flavor = "multi_thread")]
async fn handle_prompt_context_fails_open_on_slow_daemon() {
    let _guard = crate::commands::env_test_lock().lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");
    unsafe {
        std::env::set_var(trusty_common::DATA_DIR_OVERRIDE_ENV, tmp.path());
        std::env::remove_var(crate::prompt_log::ENV_ENABLED);
        std::env::remove_var(crate::prompt_log::ENV_DIR);
        std::env::remove_var(crate::prompt_log::ENV_HASH_PROMPTS);
    }

    // Accept, then never write. The hook's per-call budget is what has to end
    // this, not the peer.
    let socket = crate::transport::uds::socket_path().expect("socket path under the override");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the stalled socket");
    let server = tokio::spawn(async move {
        while let Ok((conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                drop(conn);
            });
        }
    });

    let start = std::time::Instant::now();
    let res = handle_prompt_context_with_payload(String::new()).await;
    let elapsed = start.elapsed();

    server.abort();
    unsafe {
        std::env::remove_var(trusty_common::DATA_DIR_OVERRIDE_ENV);
    }

    assert!(
        res.is_ok(),
        "must fail open even with a stalled daemon, got {res:?}"
    );
    let budget = BODY_DEADLINE + EMIT_DEADLINE + std::time::Duration::from_millis(750);
    assert!(
        elapsed < budget,
        "handle_prompt_context took {elapsed:?}, expected under budget {budget:?} \
         (a stalled daemon must never make the hook wait out its own timeout)"
    );
}

/// Why (issue #5038, symptom 1): `memory_remember` stamps every drawer with the
/// reserved `creator:*` namespace — `creator:cwd` alone is a ~90-character
/// absolute path — and the injection rendered all of it. Over 17,176 real
/// firings those tags are 33.4% of every tag byte injected. They tell a model
/// nothing about what the drawer says.
/// What: a drawer carrying provenance tags among topical ones through
/// `render_tags`; asserts no `creator:` survives and the topical tags do.
/// Test: itself.
#[test]
fn injection_drops_provenance_tags() {
    use format::render_tags;
    let tags: Vec<String> = [
        "rust",
        "creator:client=trusty-memory-mcp",
        "tokio",
        "creator:version=0.21.2",
        "creator:source=mcp",
        "creator:cwd=/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools/.base",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let rendered = render_tags(&tags).expect("topical tags must still render");
    assert!(
        !rendered.contains("creator:"),
        "provenance must not reach the injection; got: {rendered}"
    );
    assert!(
        rendered.contains("`rust`") && rendered.contains("`tokio`"),
        "topical tags must survive; got: {rendered}"
    );

    // A drawer whose only tags are provenance renders no suffix at all — 2.8%
    // of drawers in the corpus were exactly this shape.
    let only_provenance: Vec<String> = tags
        .iter()
        .filter(|t| t.starts_with("creator:"))
        .cloned()
        .collect();
    assert_eq!(
        render_tags(&only_provenance),
        None,
        "an all-provenance tag list must render nothing, not an empty suffix"
    );
}

/// Why (issue #5038, symptom 2): tag lists rendered in full, uncapped — the
/// median drawer carried 17 tags and the `_(tags: …)_` suffix was 53.0% of
/// every byte the hook injected, outweighing the content preview it labels on
/// 82.3% of drawers.
/// What: a 12-tag drawer through `render_tags`; asserts exactly
/// `MAX_RENDERED_TAGS` render, the surplus is announced rather than dropped
/// silently, and the whole suffix stays under the content budget the cap was
/// chosen against.
/// Test: itself.
#[test]
fn injection_caps_rendered_tag_count() {
    use format::{render_tags, MAX_RENDERED_TAGS};
    // Realistic tag lengths, not `topic-N`. These are the shapes the live
    // palace actually stores — the 7-character fixture this test used to carry
    // could not have caught the byte-budget hole the #5038 review found.
    let tags: Vec<String> = [
        "slate-prioritization-in-flight",
        "trusty-search-reinstall-in-flight",
        "standing-instruction",
        "session-2eb72dca",
        "resume-target",
        "issue-2833",
        "bob-decision",
        "trusty-mpm",
        "status",
        "kg",
        "redb",
        "python",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let rendered = render_tags(&tags).expect("tags must render");
    let shown = rendered.matches('`').count() / 2;
    assert!(
        shown <= MAX_RENDERED_TAGS,
        "at most MAX_RENDERED_TAGS tags may render; got {shown}: {rendered}"
    );
    assert!(
        rendered.contains(&format!("+{} more", tags.len() - shown)),
        "held-back tags must be announced, not dropped silently; got: {rendered}"
    );
    assert!(
        !rendered.contains("python"),
        "the tail of the tag list must not render; got: {rendered}"
    );
}

/// Why (#5038 review): `MAX_RENDERED_TAGS` is argued from "a label must never
/// outweigh the payload it labels" but enforces a *count*, and a count cannot
/// bound bytes — four 55-character tags blow a 220-character budget while
/// satisfying the cap. The old test used 7-character `topic-N` fixtures, so its
/// length assertion could never fail no matter how wrong the cap was.
/// What: four tags of 55 characters each — well inside the count cap, well past
/// the char budget. Asserts the rendered suffix stays within
/// `MAX_RENDERED_TAG_CHARS`, that fewer than the count cap render as a result,
/// and that the ones dropped for length are still announced.
/// Test: itself.
#[test]
fn injection_caps_rendered_tag_bytes() {
    use format::{render_tags, MAX_RENDERED_TAGS, MAX_RENDERED_TAG_CHARS};
    const TAG_CHARS: usize = 55;
    let long: Vec<String> = (0..4)
        .map(|i| {
            let stem = format!("workstream-{i}-");
            format!("{stem}{}", "x".repeat(TAG_CHARS - stem.chars().count()))
        })
        .collect();
    assert_eq!(
        long[0].chars().count(),
        TAG_CHARS,
        "fixture must be 55 chars"
    );
    let rendered = render_tags(&long).expect("tags must render");
    assert!(
        rendered.chars().count() <= MAX_RENDERED_TAG_CHARS,
        "the tag label must stay inside its {MAX_RENDERED_TAG_CHARS}-char budget; \
         got {} chars: {rendered}",
        rendered.chars().count()
    );
    let shown = rendered.matches('`').count() / 2;
    assert!(
        shown < MAX_RENDERED_TAGS,
        "with 55-char tags the byte budget must bind before the count cap; \
         got {shown} tags: {rendered}"
    );
    assert!(
        rendered.contains(&format!("+{} more", long.len() - shown)),
        "tags held back for length must be announced too; got: {rendered}"
    );

    // The deliberate exception: one tag longer than the whole budget still
    // renders, because `_(tags: +1 more)_` communicates strictly less.
    let huge = vec![format!("w-{}", "y".repeat(400))];
    let rendered = render_tags(&huge).expect("a single oversized tag still renders");
    assert!(
        rendered.contains(&huge[0]),
        "the lone tag must survive whole"
    );
}

/// Why (issue #5038, end to end): the unit tests above pin `render_tags` in
/// isolation. This one proves the whole chain — real MCP write path stamping
/// `creator:*`, real HTTP recall, real composition — never puts provenance or
/// an uncapped tag list into what Claude Code actually receives.
/// What: seeds a palace through `memory_remember` (which attaches the
/// attribution namespace exactly as production does) with drawers carrying more
/// topical tags than the cap, recalls with a matching prompt, and asserts the
/// rendered injection carries no `creator:` and no bullet over the cap.
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn prompt_context_injection_has_no_provenance_tags() {
    use format::MAX_RENDERED_TAGS;
    let _guard = crate::commands::env_test_lock().lock().await;
    unsafe {
        std::env::remove_var(ENV_MIN_SCORE);
        std::env::remove_var(ENV_RECALL_DENY_TAGS);
    }
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-tag-render").await;

    for (text, tags) in [
        (
            "Rust integration uses tokio for async tasks and serde for JSON encoding",
            vec![
                "rust", "tokio", "serde", "async", "json", "encoding", "runtime", "crate",
            ],
        ),
        (
            "Rust error handling prefers thiserror in libraries and anyhow in binaries",
            vec![
                "rust",
                "thiserror",
                "anyhow",
                "errors",
                "libraries",
                "binaries",
                "convention",
            ],
        ),
    ] {
        let tags_json: Vec<serde_json::Value> = tags.iter().map(|t| json!(t)).collect();
        let _ = crate::tools::dispatch_tool(
            &state,
            "memory_remember",
            json!({
                "palace": slug,
                "text": text,
                "room": "General",
                "tags": tags_json,
            }),
        )
        .await
        .expect("memory_remember");
    }

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": "how does rust integration work with tokio and serde?"
    })
    .to_string();
    let body = build_injection_body(&payload).await;

    // #5819: `assert_ne!(body, EMPTY_PLACEHOLDER)` passed vacuously once an
    // empty body became legal, so this now asserts real content.
    assert!(
        !body.is_empty() && body != EMPTY_PLACEHOLDER,
        "fixture must actually recall something for this test to mean anything"
    );
    assert!(
        body.contains("_(tags:"),
        "fixture must render a tag suffix for this test to mean anything; got:\n{body}"
    );
    assert!(
        !body.contains("creator:"),
        "no provenance tag may reach the injection; got:\n{body}"
    );
    for bullet in body.lines().filter(|l| l.contains("_(tags:")) {
        let shown = bullet
            .split("_(tags:")
            .nth(1)
            .map(|suffix| suffix.matches('`').count() / 2)
            .unwrap_or(0);
        assert!(
            shown <= MAX_RENDERED_TAGS,
            "bullet renders {shown} tags, over the cap of {MAX_RENDERED_TAGS}: {bullet}"
        );
    }

    addr_handle.shutdown().await;
}

/// Why (issue #4972, end to end): the unit tests in `query` pin the shaping in
/// isolation. This one proves the metric survives the whole hook — an
/// over-window `<task-notification>` prompt must reach `/recall` reshaped and
/// leave a record of it on the enriched-prompt log line, which is the same
/// corpus the 52%-over-window rate was measured from. Before this change the
/// embedder cut the query at token 512 and nothing anywhere recorded it.
/// What: fires the hook with a 22 KB envelope-wrapped prompt, then parses the
/// JSONL log and asserts `recall_query` reports an over-budget original, a
/// within-budget send, a stripped envelope, and a non-zero dropped-unit count.
/// Test: itself.
/// Note (issue #226): gated on `axum-server`; spins up the HTTP daemon.
#[cfg(feature = "daemon")]
#[tokio::test]
async fn prompt_context_logs_recall_query_shape() {
    let _guard = crate::commands::env_test_lock().lock().await;
    unsafe {
        std::env::remove_var(ENV_MIN_SCORE);
        std::env::remove_var(super::query::ENV_QUERY_TOKEN_BUDGET);
    }
    let (state, _data_dir_tmp, _project_dir_tmp, project_dir, slug, addr_handle) =
        spin_up_test_daemon_with_palace("prompt-ctx-query-shape").await;
    let _ = crate::tools::dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": slug,
            "text": "Rust integration uses tokio for async tasks and serde for JSON encoding",
            "room": "General",
            "tags": [json!("rust")],
        }),
    )
    .await
    .expect("memory_remember");

    // The exact shape 65.3% of logged hook prompts arrive in: a machine
    // envelope whose head is task ids and absolute paths, wrapping a long
    // agent report.
    let long_result = "The retrieval relevance floor interacts with the top_k cap.\n".repeat(400);
    let prompt = format!(
        "<task-notification>\n\
         <task-id>a23c46a0439fa7881</task-id>\n\
         <tool-use-id>toolu_01PvoC76SpHX65DVbJi7sPes</tool-use-id>\n\
         <output-file>/private/tmp/claude-502/-Users-masa-projects/tasks/a23.output</output-file>\n\
         <status>completed</status>\n\
         <summary>Agent \"rust integration\" finished</summary>\n\
         <result>{long_result}</result>\n\
         </task-notification>"
    );
    assert!(prompt.len() > 20_000, "fixture must be far past the window");

    let payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "cwd": project_dir.to_string_lossy(),
        "prompt": prompt,
    })
    .to_string();
    let _ = build_injection_body(&payload).await;

    let logs_dir = trusty_common::resolve_data_dir("trusty-memory")
        .expect("resolve data dir")
        .join("logs");
    let log_file = std::fs::read_dir(&logs_dir)
        .expect("logs dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("enriched-prompts."))
        })
        .expect("an enriched-prompts log file");
    let content = std::fs::read_to_string(&log_file).expect("read log");
    let entry: crate::prompt_log::PromptLogEntry = content
        .lines()
        .filter_map(|l| serde_json::from_str::<crate::prompt_log::PromptLogEntry>(l).ok())
        .next_back()
        .expect("at least one parseable log line");

    let shape = entry
        .recall_query
        .expect("an over-window query must leave a shape record, not truncate silently");
    assert!(
        shape.original_tokens > shape.budget_tokens,
        "fixture must exceed the budget; got {shape:?}"
    );
    assert!(
        shape.sent_tokens <= shape.budget_tokens,
        "the query sent to the embedder must fit its window; got {shape:?}"
    );
    assert!(
        shape.envelope_stripped,
        "the task-notification envelope must be stripped; got {shape:?}"
    );
    assert!(
        shape.units_dropped > 0 && shape.reshaped(),
        "the reduction must be recorded, not silent; got {shape:?}"
    );

    addr_handle.shutdown().await;
}
