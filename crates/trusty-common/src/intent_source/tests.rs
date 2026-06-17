//! Unit tests for the intent-source resolver (ISR), covering AC-1..AC-7.
//!
//! Why: the ISR is the shared contract both conformance gates depend on; its
//! precedence rule, linkage, spec-resolution, and fail-open behaviour must be
//! pinned by tests that run with **no network** (spec §8 acceptance criteria).
//! What: a mock `TicketFetcher`, an in-memory `SpecLookup`, and tests for every
//! acceptance criterion AC-1..AC-7 plus the supporting linkage / extraction /
//! precedence helpers.
//! Test: this file is itself the test surface; `cargo test -p trusty-common
//! --features intent-source` runs it.

use async_trait::async_trait;

use super::backend_fetcher::FsSpecLookup;
use super::extract::{HeuristicMethodExtractor, MethodExtractor, heuristic_method};
use super::linkage::{extract_branch_ticket, extract_pr_ticket, extract_ticket_id, is_ticketed};
use super::resolve::{
    EnvTokenResolver, IntentTokenResolver, SpecLookup, TicketData, TicketFetcher, resolve,
    resolve_default,
};
use super::spec_resolve::{extract_spec_method, parse_spec_refs};
use super::types::{ChangedFile, IntentQuery, IsrError, MethodKind, Precedence, ResolvedIntent};

// ───────────────────────── Test doubles ──────────────────────────────────────

/// A mock `TicketFetcher` returning a canned body (or a canned error).
///
/// Why: the resolver must be testable offline; this lets a test inject any
/// ticket body (or simulate a fetch failure) without a GitHub client.
/// What: holds an optional `TicketData` (Ok) or an error string (Err).
/// Test: used by every `resolve`-driven test below.
struct MockFetcher {
    result: Result<TicketData, String>,
}

impl MockFetcher {
    fn ok(body: &str) -> Self {
        Self {
            result: Ok(TicketData {
                id: "#1358".to_string(),
                title: "ISR".to_string(),
                body: body.to_string(),
                url: Some("https://github.com/x/y/issues/1358".to_string()),
                backend: "github".to_string(),
            }),
        }
    }
    fn err(msg: &str) -> Self {
        Self {
            result: Err(msg.to_string()),
        }
    }
}

#[async_trait]
impl TicketFetcher for MockFetcher {
    async fn fetch(
        &self,
        _owner: &str,
        _repo: &str,
        _ticket_id: &str,
    ) -> Result<TicketData, IsrError> {
        self.result.clone().map_err(IsrError::TicketFetch)
    }
}

/// An in-memory `SpecLookup` mapping spec-file paths to markdown text.
///
/// Why: spec-resolution must run without reading the real `docs/specs/` tree.
/// What: a `Vec<(path, markdown)>`; `load` returns the first matching text.
/// Test: used by the spec-resolution `resolve` tests below.
struct MapSpecLookup {
    entries: Vec<(String, String)>,
}

impl MapSpecLookup {
    fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
    fn with(path: &str, markdown: &str) -> Self {
        Self {
            entries: vec![(path.to_string(), markdown.to_string())],
        }
    }
}

impl SpecLookup for MapSpecLookup {
    fn load(&self, spec_file: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(p, _)| p == spec_file)
            .map(|(_, md)| md.clone())
    }
}

// Helper: a ticket-only query with the given changed files.
fn ticket_query(changed: Vec<ChangedFile>) -> IntentQuery {
    IntentQuery::Ticket {
        ticket_id: "#1358".to_string(),
        owner: "x".to_string(),
        repo: "y".to_string(),
        changed_files: changed,
    }
}

// Helper: a spec markdown document whose anchor section prescribes a method.
fn spec_md(anchor: &str, method_line: &str) -> String {
    format!(
        "# Doc\n\n## Section {{#{anchor}}}\n\n**Behavior Contract (WHAT):**\n\n- {method_line}\n\n## Next\n\nunrelated\n"
    )
}

// A changed file declaring an SLD ref to the given spec id/anchor.
fn sld_file(path: &str, spec_id: &str, spec_file: &str, anchor: &str) -> ChangedFile {
    ChangedFile {
        path: path.to_string(),
        content: format!(
            "//! # Spec References\n//!\n//! - [`{spec_id}`]({spec_file}#{anchor})\n\npub fn f() {{}}\n"
        ),
    }
}

// ───────────────────────── AC-1: ticket method ──────────────────────────────

/// AC-1: a ticket whose body prescribes a method →
/// `ticket_method = Some`, `precedence_winner = Ticket`.
#[tokio::test]
async fn ac1_ticket_method_present() {
    let fetcher = MockFetcher::ok("We must use cursor-based pagination for the list endpoint.");
    let lookup = MapSpecLookup::empty();
    let intent = resolve_default(ticket_query(vec![]), &fetcher, &lookup).await;

    assert!(
        intent.ticket_method.is_some(),
        "ticket method should resolve"
    );
    assert_eq!(intent.precedence_winner, Precedence::Ticket);
    assert!(intent.ticket.is_some());
    assert!(!intent.conflict);
    assert!(!intent.stale_spec);
    assert!(intent.unresolved.is_none());
    let m = intent.ticket_method.unwrap();
    assert_eq!(m.kind, MethodKind::Approach);
    assert!(m.source_excerpt.contains("cursor-based pagination"));
}

// ───────────────────────── AC-2: agree ──────────────────────────────────────

/// AC-2: a ticket and a spec section that AGREE →
/// `conflict = false`, `stale_spec = false`.
#[tokio::test]
async fn ac2_ticket_and_spec_agree() {
    let method = "use cursor-based pagination";
    let fetcher = MockFetcher::ok(method);
    let spec_file = "docs/specs/search.md";
    let anchor = "SPEC-SEARCH-04~draft";
    let lookup = MapSpecLookup::with(spec_file, &spec_md(anchor, method));
    let changed = vec![sld_file(
        "crates/x/src/lib.rs",
        "SPEC-SEARCH-04~draft",
        spec_file,
        anchor,
    )];

    let intent = resolve_default(ticket_query(changed), &fetcher, &lookup).await;

    assert!(intent.ticket_method.is_some());
    assert!(intent.spec_method.is_some(), "spec method should resolve");
    assert_eq!(intent.precedence_winner, Precedence::Ticket);
    assert!(!intent.conflict, "agreeing methods must not conflict");
    assert!(!intent.stale_spec);
    assert!(intent.spec_section.is_some());
}

// ───────────────────────── AC-3: disagree → ticket wins ─────────────────────

/// AC-3: a ticket and a spec that DISAGREE →
/// `precedence_winner = Ticket`, `conflict = true`, `stale_spec = true`.
#[tokio::test]
async fn ac3_ticket_and_spec_conflict_ticket_wins() {
    let fetcher = MockFetcher::ok("use cursor-based pagination");
    let spec_file = "docs/specs/search.md";
    let anchor = "SPEC-SEARCH-04~draft";
    // Spec prescribes a *different* method.
    let lookup = MapSpecLookup::with(spec_file, &spec_md(anchor, "use offset-based pagination"));
    let changed = vec![sld_file(
        "crates/x/src/lib.rs",
        "SPEC-SEARCH-04~draft",
        spec_file,
        anchor,
    )];

    let intent = resolve_default(ticket_query(changed), &fetcher, &lookup).await;

    assert_eq!(
        intent.precedence_winner,
        Precedence::Ticket,
        "ticket > spec"
    );
    assert!(intent.conflict, "disagreeing methods must conflict");
    assert!(intent.stale_spec, "conflicting spec is stale/advisory");
    assert!(intent.ticket_method.is_some());
    assert!(intent.spec_method.is_some());
}

// ───────────────────────── AC-4: gap ────────────────────────────────────────

/// AC-4: neither a ticket method nor a spec method →
/// all method fields `None`, `precedence_winner = None`.
#[tokio::test]
async fn ac4_gap_no_methods() {
    // Body with no imperative-method cue.
    let fetcher = MockFetcher::ok("Add a new field to the response payload. See discussion.");
    let lookup = MapSpecLookup::empty();
    let intent = resolve_default(ticket_query(vec![]), &fetcher, &lookup).await;

    assert!(intent.ticket_method.is_none());
    assert!(intent.spec_method.is_none());
    assert_eq!(intent.precedence_winner, Precedence::None);
    assert!(intent.is_gap());
    assert!(!intent.conflict);
    assert!(!intent.stale_spec);
}

// ───────────────────────── AC-5: PR → ticket linkage ────────────────────────

/// AC-5: PR body `Closes #1325`, a commit trailer, and a branch `fix/1325-x`
/// each resolve to ticket `#1325`; a PR with no linkage resolves to `None`.
#[test]
fn ac5_pr_body_closes() {
    assert_eq!(
        extract_pr_ticket("Implements the thing.\n\nCloses #1325", &[], None),
        Some("#1325".to_string())
    );
}

#[test]
fn ac5_commit_trailer() {
    let commits = vec!["wip".to_string(), "done, fixes #1325".to_string()];
    assert_eq!(
        extract_pr_ticket("no linkage here", &commits, None),
        Some("#1325".to_string())
    );
}

#[test]
fn ac5_branch_name() {
    assert_eq!(
        extract_pr_ticket("no linkage", &[], Some("fix/1325-intent-source")),
        Some("#1325".to_string())
    );
}

/// Review MEDIUM #3 (tga #445): a bare `#N` mentioned only in passing in the
/// free-text PR body must NOT resolve, but a qualifying `Closes #N` must.
#[test]
fn ac5_pr_body_bare_ref_does_not_link() {
    // Bare `#42` in prose — NOT a qualifying ref — must not resolve from body.
    assert_eq!(
        extract_pr_ticket("See discussion in #42 for background", &[], None),
        None,
        "a bare #N mentioned in passing must not link (tga #445)"
    );
    // Action-keyword ref qualifies and resolves.
    assert_eq!(
        extract_pr_ticket("Closes #42", &[], None),
        Some("#42".to_string()),
        "a qualifying `Closes #N` must resolve"
    );
}

/// The bare-`#N` last resort is retained for commit messages (terse trailers),
/// even though the PR body rejects it.
#[test]
fn ac5_commit_bare_ref_still_links() {
    let commits = vec!["address #42".to_string()];
    assert_eq!(
        extract_pr_ticket("See discussion in #42", &commits, None),
        Some("#42".to_string()),
        "commit messages keep the bare-#N fallback"
    );
}

#[test]
fn ac5_no_linkage_is_none() {
    assert_eq!(
        extract_pr_ticket(
            "just a description",
            &["misc cleanup".to_string()],
            Some("main")
        ),
        None
    );
}

/// AC-5 (end-to-end): a `Pr` query with no linkage → `ResolvedIntent::none()`.
#[tokio::test]
async fn ac5_pr_without_linkage_resolves_none() {
    let query = IntentQuery::Pr {
        owner: "x".to_string(),
        repo: "y".to_string(),
        pr_number: 7,
        body: "no ticket here".to_string(),
        branch: Some("main".to_string()),
        commit_messages: vec!["misc".to_string()],
        changed_files: vec![],
    };
    // Fetcher would error if called — proves we short-circuit on no linkage.
    let fetcher = MockFetcher::err("should not be called");
    let lookup = MapSpecLookup::empty();
    let intent = resolve_default(query, &fetcher, &lookup).await;

    assert_eq!(intent.precedence_winner, Precedence::None);
    assert!(intent.ticket.is_none());
    assert!(
        intent.unresolved.is_none(),
        "non-ticketed is a clean gap, not a failure"
    );
}

/// AC-5 (end-to-end): a `Pr` body `Closes #1325` drives a successful resolve.
#[tokio::test]
async fn ac5_pr_with_linkage_resolves_ticket() {
    let query = IntentQuery::Pr {
        owner: "x".to_string(),
        repo: "y".to_string(),
        pr_number: 1358,
        body: "Implements ISR.\n\nCloses #1358".to_string(),
        branch: None,
        commit_messages: vec![],
        changed_files: vec![],
    };
    let fetcher = MockFetcher::ok("Reuse the existing ContextSource trait.");
    let lookup = MapSpecLookup::empty();
    let intent = resolve_default(query, &fetcher, &lookup).await;

    assert!(intent.ticket.is_some());
    assert_eq!(intent.precedence_winner, Precedence::Ticket);
    assert_eq!(intent.ticket_method.unwrap().kind, MethodKind::Reuse);
}

// ───────────────────────── AC-6: spec resolution ────────────────────────────

/// AC-6: a changed file with a `# Spec References` rustdoc block pointing at
/// `SPEC-X-01` resolves `spec_section = Some(SPEC-X-01)`.
#[tokio::test]
async fn ac6_spec_ref_resolves_section() {
    let spec_file = "docs/specs/x.md";
    let anchor = "SPEC-X-01~draft";
    let lookup = MapSpecLookup::with(spec_file, &spec_md(anchor, "use the existing trait T"));
    let changed = vec![sld_file(
        "crates/x/src/lib.rs",
        "SPEC-X-01~draft",
        spec_file,
        anchor,
    )];

    // Ticket silent so the spec axis is the only method.
    let fetcher = MockFetcher::ok("Add the thing.");
    let intent = resolve_default(ticket_query(changed), &fetcher, &lookup).await;

    let sref = intent.spec_section.expect("spec section should resolve");
    assert_eq!(sref.spec_id, "SPEC-X-01~draft");
    assert_eq!(sref.file, spec_file);
    assert!(intent.spec_method.is_some());
    assert_eq!(
        intent.precedence_winner,
        Precedence::Spec,
        "spec-only → Spec"
    );
}

/// AC-6: a changed file with NO SLD ref resolves `spec_section = None` (gap).
#[tokio::test]
async fn ac6_no_sld_ref_is_gap() {
    let changed = vec![ChangedFile {
        path: "crates/x/src/lib.rs".to_string(),
        content: "// plain file, no spec references\npub fn f() {}\n".to_string(),
    }];
    let fetcher = MockFetcher::ok("Add the thing."); // ticket also silent on method
    let lookup = MapSpecLookup::empty();
    let intent = resolve_default(ticket_query(changed), &fetcher, &lookup).await;

    assert!(
        intent.spec_section.is_none(),
        "no SLD ref → no spec section"
    );
    assert!(intent.spec_method.is_none());
    assert_eq!(intent.precedence_winner, Precedence::None);
}

// ───────────────────────── AC-7: fail-open ──────────────────────────────────

/// AC-7: ticket fetch error → `unresolved = Some(reason)`, all methods `None`.
#[tokio::test]
async fn ac7_fetch_error_is_fail_open() {
    let fetcher = MockFetcher::err("HTTP 503 from GitHub");
    let lookup = MapSpecLookup::empty();
    let intent = resolve_default(ticket_query(vec![]), &fetcher, &lookup).await;

    let reason = intent.unresolved.expect("fetch error must set unresolved");
    assert!(reason.contains("503") || reason.contains("ticket fetch"));
    assert!(intent.ticket_method.is_none());
    assert!(intent.spec_method.is_none());
    assert_eq!(intent.precedence_winner, Precedence::None);
}

// ───────────────────────── linkage helper coverage ──────────────────────────

#[test]
fn linkage_is_ticketed() {
    assert!(is_ticketed("closes #42"));
    assert!(is_ticketed("ENG-7 add feature"));
    assert!(is_ticketed("AB#10 work item"));
    assert!(!is_ticketed("just a note about #42"));
    assert!(!is_ticketed("misc cleanup"));
}

#[test]
fn linkage_extract_priority() {
    assert_eq!(
        extract_ticket_id("AB#10 fixes PROJ-99"),
        Some("AB#10".to_string())
    );
    assert_eq!(
        extract_ticket_id("ENG-7 closes #10"),
        Some("ENG-7".to_string())
    );
    assert_eq!(extract_ticket_id("fixes #99"), Some("#99".to_string()));
    assert_eq!(extract_ticket_id("misc cleanup"), None);
}

#[test]
fn linkage_branch_variants() {
    assert_eq!(
        extract_branch_ticket("fix/1325-x"),
        Some("#1325".to_string())
    );
    assert_eq!(extract_branch_ticket("feat/42"), Some("#42".to_string()));
    assert_eq!(
        extract_branch_ticket("1325-bare"),
        Some("#1325".to_string())
    );
    assert_eq!(extract_branch_ticket("main"), None);
    // No leading numeric work-item segment after the `/`, so no linkage.
    assert_eq!(extract_branch_ticket("release/v1.2.3"), None);
}

// ───────────────────────── method extraction coverage ───────────────────────

#[test]
fn extract_heuristic_constraint() {
    let m = heuristic_method("- No new dependency may be added.").unwrap();
    assert_eq!(m.kind, MethodKind::Constraint);
}

#[test]
fn extract_heuristic_reuse() {
    let m = heuristic_method("Reuse the existing ContextSource trait.").unwrap();
    assert_eq!(m.kind, MethodKind::Reuse);
}

#[test]
fn extract_heuristic_approach_feature_flag() {
    let m = heuristic_method("Please gate this behind a feature flag.").unwrap();
    assert_eq!(m.kind, MethodKind::Approach);
}

#[test]
fn extract_heuristic_ambiguous_is_none() {
    assert!(heuristic_method("Make it work and ship it.").is_none());
    assert!(heuristic_method("").is_none());
    assert!(heuristic_method("Add a flag to the CLI.").is_none());
}

/// A Rust `use` import must NOT be misclassified as an approach method
/// (review MEDIUM #1: the old `cue.starts_with("use ")` guard was too broad).
#[test]
fn extract_heuristic_use_import_not_approach() {
    assert!(
        heuristic_method("use std::collections::HashMap;").is_none(),
        "a Rust import must not classify as an Approach method"
    );
    assert!(
        heuristic_method("    use crate::foo::Bar;").is_none(),
        "an indented Rust import must not classify either"
    );
    assert!(
        heuristic_method("use caution when deploying").is_none(),
        "casual `use X` phrasing must not classify as Approach"
    );
}

/// A narrow `use …` directive that IS a prescribed approach still classifies —
/// proves the tightened guard did not over-reject genuine cues.
#[test]
fn extract_heuristic_use_directive_is_approach() {
    let m = heuristic_method("Use the existing pagination helper.").unwrap();
    assert_eq!(m.kind, MethodKind::Reuse);
    let m = heuristic_method("- use offset-based windows").unwrap();
    assert_eq!(m.kind, MethodKind::Approach);
}

#[test]
fn extract_trait_default_matches_fn() {
    let extractor = HeuristicMethodExtractor;
    let body = "use cursor-based pagination";
    assert_eq!(extractor.extract(body), heuristic_method(body));
}

// ───────────────────────── spec_resolve helper coverage ─────────────────────

#[test]
fn spec_resolve_parse_dedup() {
    let src = "//! # Spec References\n\
        //! - [`SPEC-CONFORMANCE-03~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-03~draft)\n\
        /// also [`SPEC-CONFORMANCE-03~draft`](docs/specs/intent-conformance.md#SPEC-CONFORMANCE-03~draft)\n";
    let refs = parse_spec_refs(src);
    assert_eq!(refs.len(), 1, "duplicate refs collapse to one");
    assert_eq!(refs[0].spec_id, "SPEC-CONFORMANCE-03~draft");
    assert_eq!(refs[0].file, "docs/specs/intent-conformance.md");
    assert_eq!(refs[0].anchor, "SPEC-CONFORMANCE-03~draft");
}

#[test]
fn spec_resolve_parse_none() {
    assert!(parse_spec_refs("// plain comment, no spec ref\n").is_empty());
}

#[test]
fn spec_resolve_method_from_section() {
    let md = spec_md("SPEC-X-01~draft", "use the existing trait T for dispatch");
    let m = extract_spec_method(&md, "SPEC-X-01~draft").unwrap();
    assert_eq!(m.kind, MethodKind::Reuse);
    assert!(m.text.contains("existing trait T"));
}

#[test]
fn spec_resolve_method_missing_anchor() {
    let md = spec_md("SPEC-X-01~draft", "use cursor pagination");
    assert!(extract_spec_method(&md, "SPEC-OTHER-99~draft").is_none());
}

#[test]
fn spec_resolve_method_section_scoped() {
    // The method cue lives in the NEXT section, not the anchored one — it must
    // NOT be attributed to this anchor.
    let md = "# Doc\n\n## A {#SPEC-X-01~draft}\n\njust prose, no method.\n\n## B\n\n- use cursor pagination\n";
    assert!(extract_spec_method(md, "SPEC-X-01~draft").is_none());
}

/// Review LOW #4: a top-level `# ` heading must terminate the section too, so a
/// following top-level section's method does not bleed into this anchor.
#[test]
fn spec_resolve_method_section_scoped_top_level_heading() {
    let md = "## A {#SPEC-X-01~draft}\n\njust prose, no method.\n\n# Next Top Level\n\n- use cursor pagination\n";
    assert!(
        extract_spec_method(md, "SPEC-X-01~draft").is_none(),
        "a `# ` top-level heading must terminate the anchored section"
    );
}

// ───────────────────────── FsSpecLookup path-traversal guard ────────────────

/// Review MEDIUM #2 (path traversal): `FsSpecLookup::load` must refuse a
/// `..`-laden spec_file that escapes the repo root, even though such a file may
/// physically exist on disk. The in-root file loads; the traversal returns
/// `None`.
#[test]
fn fs_spec_lookup_rejects_traversal() {
    use std::fs;

    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path();

    // A legitimate spec under docs/specs/ loads fine.
    let specs_dir = root_path.join("docs/specs");
    fs::create_dir_all(&specs_dir).expect("create docs/specs");
    fs::write(specs_dir.join("real.md"), "# real spec\n").expect("write spec");

    // A "secret" file OUTSIDE the repo root that a traversal would try to read.
    let secret_dir = tempfile::tempdir().expect("secret tempdir");
    let secret = secret_dir.path().join("passwd");
    fs::write(&secret, "root:x:0:0:root\n").expect("write secret");

    let lookup = FsSpecLookup::new(root_path);

    // In-root path resolves.
    assert_eq!(
        lookup.load("docs/specs/real.md").as_deref(),
        Some("# real spec\n"),
        "an in-root spec must load"
    );

    // Build a traversal that points at the secret file relative to the root.
    // e.g. docs/specs/../../<secret-dir-basename>/passwd, then enough `..` to
    // climb out — we construct it relative to root so canonicalize would land on
    // the secret were the guard absent.
    let secret_canon = secret.canonicalize().expect("canonicalize secret");
    let root_canon = root_path.canonicalize().expect("canonicalize root");
    let traversal = pathdiff_relative(&root_canon, &secret_canon);
    assert!(
        lookup.load(&traversal).is_none(),
        "a `..` traversal escaping repo_root must return None (got Some for {traversal})"
    );

    // A blatantly bad literal also returns None (the file does not exist under
    // root, so canonicalize fails → None).
    assert!(
        lookup.load("docs/specs/../../etc/passwd").is_none(),
        "a docs/specs/../../etc/passwd traversal must return None"
    );
}

/// Compute a `from`→`to` relative path as a `/`-joined string (test helper).
///
/// Why: the traversal test needs a relative path (with `..` segments) from the
/// repo root to an out-of-tree secret so the guard is exercised against a path
/// that *would* canonicalize successfully without the containment check.
/// What: walks up from `from` to the common ancestor, then down to `to`.
/// Test: used by `fs_spec_lookup_rejects_traversal`.
fn pathdiff_relative(from: &std::path::Path, to: &std::path::Path) -> String {
    let from_c: Vec<_> = from.components().collect();
    let to_c: Vec<_> = to.components().collect();
    let common = from_c
        .iter()
        .zip(to_c.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from_c.len() - common;
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), ups).collect();
    for c in &to_c[common..] {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    parts.join("/")
}

// ───────────────────────── constructors / error display ─────────────────────

#[test]
fn none_is_gap() {
    let n = ResolvedIntent::none();
    assert!(n.is_gap());
    assert_eq!(n.precedence_winner, Precedence::None);
    assert!(n.unresolved.is_none());
}

#[test]
fn unresolved_carries_reason() {
    let u = ResolvedIntent::unresolved("boom");
    assert_eq!(u.unresolved.as_deref(), Some("boom"));
    assert!(u.is_gap());
    assert!(u.ticket_method.is_none());
}

#[test]
fn error_display_variants() {
    assert_eq!(
        IsrError::NoTicketLinkage.to_string(),
        "no ticket linkage found in PR"
    );
    assert!(
        IsrError::TicketFetch("x".into())
            .to_string()
            .contains("ticket fetch failed")
    );
    assert!(
        IsrError::NoToken("y".into())
            .to_string()
            .contains("no auth token")
    );
}

// ───────────────────────── token resolver seam ──────────────────────────────

/// A custom token resolver demonstrates the §7.4 pluggable seam (App/JWT mode).
struct StaticTokenResolver;

#[async_trait]
impl IntentTokenResolver for StaticTokenResolver {
    async fn token(&self, _owner: &str, _repo: &str) -> Result<String, IsrError> {
        Ok("app-jwt-token".to_string())
    }
}

#[tokio::test]
async fn token_resolver_pluggable() {
    let r = StaticTokenResolver;
    assert_eq!(r.token("x", "y").await.unwrap(), "app-jwt-token");
}

#[tokio::test]
async fn token_resolver_env_missing_errors() {
    // Do not mutate the global env (parallel tests); just assert the error type
    // path is reachable by constructing the resolver and checking a name.
    let r = EnvTokenResolver;
    // If GITHUB_TOKEN happens to be set in CI we accept Ok; otherwise NoToken.
    match r.token("x", "y").await {
        Ok(t) => assert!(!t.is_empty()),
        Err(e) => assert!(matches!(e, IsrError::NoToken(_))),
    }
}

// ───────────────────────── explicit-extractor resolve path ──────────────────

/// `resolve` (non-default) accepts a custom extractor — proves the OQ-1 seam
/// is wired end-to-end, not just on the default path.
#[tokio::test]
async fn resolve_with_explicit_extractor() {
    struct AlwaysSome;
    impl MethodExtractor for AlwaysSome {
        fn extract(&self, _body: &str) -> Option<super::types::Method> {
            Some(super::types::Method {
                text: "forced".to_string(),
                kind: MethodKind::Constraint,
                source_excerpt: "forced".to_string(),
            })
        }
    }
    let fetcher = MockFetcher::ok("ambiguous prose the heuristic would skip");
    let lookup = MapSpecLookup::empty();
    let intent = resolve(ticket_query(vec![]), &fetcher, &AlwaysSome, &lookup).await;
    assert_eq!(intent.precedence_winner, Precedence::Ticket);
    assert_eq!(intent.ticket_method.unwrap().text, "forced");
}
