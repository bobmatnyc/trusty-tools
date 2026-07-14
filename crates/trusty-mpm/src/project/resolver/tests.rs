//! Unit tests for the NL-to-project resolver (WI-5, #1517).
//!
//! Why: extracted from the old monolithic `resolver.rs` into a sibling test
//! file so the production module stays under the 500-SLOC cap while tests
//! use the 1500-SLOC test-file cap.
//! What: covers exact-name, URL, keyword, multi-candidate disambiguation,
//! clamping, no-match, tie-breaking, and confidence-capping scenarios.
//! Test: this IS the test module; run with `cargo test -p trusty-mpm`.

use super::*;
use crate::session_manager::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use chrono::Utc;
use std::path::PathBuf;

// ---- helpers ----

fn proj(name: &str, repo_url: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        github: None,
        commit_name: None,
        commit_email: None,
    }
}

fn proj_tagged(name: &str, repo_url: &str, tags: &[&str], desc: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: tags.iter().map(|s| s.to_string()).collect(),
        description: Some(desc.to_string()),
        gh_user: None,
        github: None,
        commit_name: None,
        commit_email: None,
    }
}

fn session_with_repo(repo_url: &str) -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "test".into(),
        cwd: PathBuf::from("/tmp"),
        task: "task".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: Some(repo_url.to_string()),
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
    }
}

fn session_no_repo() -> SessionRecord {
    SessionRecord {
        id: ManagedSessionId::new(),
        tmux_name: "no-repo".into(),
        cwd: PathBuf::from("/tmp"),
        task: "no repo".into(),
        state: ManagedSessionState::Active,
        created_at: Utc::now(),
        last_activity_at: None,
        workspace_path: None,
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: Default::default(),
        ephemeral: false,
        workspace_owned: false,
        source_id: None,
        claude_session_id: None,
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: Default::default(),
    }
}

// ---- resolve_project: exact name ----

#[test]
fn resolver_exact_name() {
    let projects = vec![
        proj("trusty-tools", "https://github.com/org/trusty-tools"),
        proj("trusty-search", "https://github.com/org/trusty-search"),
    ];
    let res = resolve_project("trusty-tools", &projects).expect("should match");
    assert_eq!(
        res.primary.as_ref().expect("primary").project.name,
        "trusty-tools"
    );
    assert_eq!(
        res.primary.as_ref().expect("primary").reason,
        ResolutionReason::ExactNameMatch
    );
    assert!((res.primary.as_ref().expect("primary").confidence - 1.0).abs() < f32::EPSILON);
}

#[test]
fn resolver_exact_name_case_insensitive() {
    let projects = vec![proj("TrustyTools", "https://github.com/org/trusty-tools")];
    let res = resolve_project("trustytools", &projects).expect("case-insensitive match");
    assert_eq!(
        res.primary.as_ref().expect("primary").reason,
        ResolutionReason::ExactNameMatch
    );
}

#[test]
fn resolver_exact_name_does_not_partial_match_keyword() {
    // "trusty" should NOT produce an ExactNameMatch for "trusty-tools".
    let projects = vec![proj("trusty-tools", "https://github.com/org/trusty-tools")];
    let res = resolve_project("trusty", &projects).expect("keyword fallback");
    // Should fall through to keyword match, not exact.
    assert_ne!(
        res.primary.as_ref().expect("primary").reason,
        ResolutionReason::ExactNameMatch
    );
}

// ---- resolve_project: URL match ----

#[test]
fn resolver_url_match_https() {
    let projects = vec![
        proj("trusty-tools", "https://github.com/org/trusty-tools"),
        proj("other", "https://github.com/org/other"),
    ];
    let res =
        resolve_project("https://github.com/org/trusty-tools.git", &projects).expect("url match");
    assert_eq!(
        res.primary.as_ref().expect("primary").project.name,
        "trusty-tools"
    );
    assert_eq!(
        res.primary.as_ref().expect("primary").reason,
        ResolutionReason::UrlMatch
    );
    assert!((res.primary.as_ref().expect("primary").confidence - 0.95).abs() < f32::EPSILON);
}

#[test]
fn resolver_url_match_github_fragment() {
    let projects = vec![proj("my-repo", "https://github.com/owner/my-repo")];
    // A partial URL fragment without scheme prefix.
    let res = resolve_project("github.com/owner/my-repo", &projects).expect("fragment match");
    assert_eq!(
        res.primary.as_ref().expect("primary").project.name,
        "my-repo"
    );
    assert_eq!(
        res.primary.as_ref().expect("primary").reason,
        ResolutionReason::UrlMatch
    );
}

#[test]
fn resolver_url_no_match_falls_through_to_keyword() {
    let projects = vec![proj("trusty-tools", "https://github.com/org/trusty-tools")];
    // URL that does not match any repo_url.
    let res = resolve_project("https://github.com/other/unrelated.git", &projects);
    // Should fail with NoMatch (no keyword hits either).
    assert!(res.is_err());
}

// ---- resolve_project: keyword match ----

#[test]
fn resolver_keyword_match_name() {
    let projects = vec![
        proj("trusty-search", "https://github.com/org/trusty-search"),
        proj("trusty-memory", "https://github.com/org/trusty-memory"),
    ];
    let res = resolve_project("search", &projects).expect("keyword name match");
    assert_eq!(
        res.primary.as_ref().expect("primary").project.name,
        "trusty-search"
    );
    assert_eq!(
        res.primary.as_ref().expect("primary").reason,
        ResolutionReason::KeywordMatch
    );
}

#[test]
fn resolver_keyword_match_tag() {
    let projects = vec![
        proj_tagged(
            "trusty-search",
            "https://github.com/org/trusty-search",
            &["rust", "search-daemon"],
            "hybrid code search",
        ),
        proj_tagged(
            "trusty-memory",
            "https://github.com/org/trusty-memory",
            &["rust", "memory"],
            "memory palace MCP server",
        ),
    ];
    let res = resolve_project("memory", &projects).expect("tag match");
    assert_eq!(
        res.primary.as_ref().expect("primary").project.name,
        "trusty-memory"
    );
}

#[test]
fn resolver_keyword_match_description() {
    // A description hit alone scores 0.2, which is below KEYWORD_COLLECTION_FLOOR.
    // To score above the floor we need a name hit (0.4) combined with the
    // description hit to demonstrate multi-field scoring.
    let projects = vec![proj_tagged(
        "frontend-app",
        "https://github.com/org/frontend-app",
        &[],
        "the best frontend application ever",
    )];
    // "frontend" hits both the name (+0.4) and description (+0.2) → raw 0.6.
    let res = resolve_project("frontend", &projects).expect("description+name match");
    assert_eq!(
        res.primary.as_ref().expect("primary").project.name,
        "frontend-app"
    );
}

#[test]
fn resolver_keyword_threshold_filters_low_score() {
    // A project whose name does NOT contain "xyz" and has no tags or desc
    // must NOT appear in matches.
    let projects = vec![proj("alpha", "https://github.com/org/alpha")];
    let res = resolve_project("xyz unrelated words", &projects);
    assert!(
        res.is_err(),
        "should produce NoMatch for completely unrelated query"
    );
}

// ---- confidence clamping (#1532 item 1) ----

#[test]
fn resolver_confidence_clamped_to_one() {
    // Craft a query with many words all present in name, desc, and tags
    // so the raw score exceeds 1.0 before clamping.
    let project = proj_tagged(
        "search-daemon",
        "https://github.com/org/search-daemon",
        &["search", "daemon", "backend"],
        "search daemon backend service for search",
    );
    let projects = vec![project];
    // "search daemon backend" hits name(0.4*3=1.2) + tags(0.3 each) + desc(0.2 each)
    // raw score will be well above 1.0.
    let res = resolve_project("search daemon backend", &projects).expect("should match");
    let conf = res.primary.as_ref().expect("primary").confidence;
    assert!(conf <= 1.0, "confidence must be clamped to 1.0, got {conf}");
    assert!(conf > 0.0, "confidence must be positive");
}

// ---- disambiguation (#1532 items 2 & 3) ----

#[test]
fn resolver_multi_candidate_disambiguation() {
    let projects = vec![
        proj_tagged(
            "trusty-search",
            "https://github.com/org/trusty-search",
            &["rust", "search"],
            "search daemon",
        ),
        proj_tagged(
            "trusty-tools",
            "https://github.com/org/trusty-tools",
            &["rust", "workspace"],
            "the trusty search toolset",
        ),
    ];
    // "trusty search" hits both projects well.
    let res = resolve_project("trusty search", &projects).expect("multiple matches");
    assert!(res.matches.len() > 1, "should return multiple candidates");
    assert!(
        res.needs_disambiguation(),
        "both candidates above disambiguation floor should trigger disambiguation"
    );
    // primary is highest confidence.
    let primary_conf = res.primary.as_ref().expect("primary").confidence;
    for m in &res.matches {
        assert!(
            m.confidence <= primary_conf + f32::EPSILON,
            "matches must be sorted descending"
        );
    }
}

#[test]
fn resolver_single_above_floor_no_disambiguation() {
    let projects = vec![
        proj_tagged(
            "trusty-search",
            "https://github.com/org/trusty-search",
            &["search"],
            "search daemon",
        ),
        proj("other-project", "https://github.com/org/other"),
    ];
    // Only "trusty-search" matches "search".
    let res = resolve_project("search", &projects).expect("one match");
    assert!(
        !res.needs_disambiguation(),
        "single candidate above floor must not require disambiguation"
    );
}

// ---- no-match path ----

#[test]
fn resolver_no_match_returns_error() {
    let projects = vec![proj("alpha", "https://github.com/org/alpha")];
    let err = resolve_project("completely-unrelated-xyz", &projects).unwrap_err();
    assert!(
        matches!(err, ResolverError::NoMatch { .. }),
        "expected NoMatch, got: {err:?}"
    );
}

#[test]
fn resolver_empty_registry() {
    let err = resolve_project("anything", &[]).unwrap_err();
    assert!(
        matches!(err, ResolverError::EmptyRegistry),
        "expected EmptyRegistry, got: {err:?}"
    );
}

// ---- ticket-ID reason ----

#[test]
fn resolver_ticket_keyword_reason() {
    let projects = vec![proj_tagged(
        "my-proj",
        "https://github.com/org/my-proj",
        &["proj"],
        "project",
    )];
    let res = resolve_project("PROJ-123", &projects).expect("ticket keyword");
    // PROJ-123 is a ticket ID but also contains the word "proj" (lowercased).
    assert_eq!(
        res.primary.as_ref().expect("primary").reason,
        ResolutionReason::TicketKeyword
    );
}

// ---- resolve_session_project ----

#[test]
fn resolver_session_project_binding() {
    let projects = vec![
        proj("trusty-tools", "https://github.com/org/trusty-tools"),
        proj("other", "https://github.com/org/other"),
    ];
    let session = session_with_repo("https://github.com/org/trusty-tools");
    let bound = resolve_session_project(&session, &projects).expect("should bind");
    assert_eq!(bound.name, "trusty-tools");
}

#[test]
fn resolver_by_repo_url_matches() {
    // #2379: the pre-spawn Deliverable-linkage validation path binds a bare
    // `repo_url` (no `SessionRecord` exists yet) the same way a live session
    // would via `resolve_session_project`.
    let projects = vec![
        proj("trusty-tools", "https://github.com/org/trusty-tools"),
        proj("other", "https://github.com/org/other"),
    ];
    let bound = resolve_project_by_repo_url("https://github.com/org/trusty-tools", &projects)
        .expect("should match");
    assert_eq!(bound.name, "trusty-tools");
}

#[test]
fn resolver_by_repo_url_no_match() {
    let projects = vec![proj("trusty-tools", "https://github.com/org/trusty-tools")];
    assert!(
        resolve_project_by_repo_url("https://github.com/org/unregistered", &projects).is_none()
    );
}

#[test]
fn resolver_session_binding_strips_git_suffix() {
    let projects = vec![proj("trusty-tools", "https://github.com/org/trusty-tools")];
    // Session URL has a .git suffix — must still match.
    let session = session_with_repo("https://github.com/org/trusty-tools.git");
    let bound = resolve_session_project(&session, &projects).expect("git-suffix match");
    assert_eq!(bound.name, "trusty-tools");
}

#[test]
fn resolver_session_no_repo_returns_none() {
    let projects = vec![proj("trusty-tools", "https://github.com/org/trusty-tools")];
    let session = session_no_repo();
    assert!(
        resolve_session_project(&session, &projects).is_none(),
        "session with no repo_url must return None"
    );
}

#[test]
fn resolver_session_unregistered_url_returns_none() {
    let projects = vec![proj("trusty-tools", "https://github.com/org/trusty-tools")];
    let session = session_with_repo("https://github.com/other/unrelated");
    assert!(
        resolve_session_project(&session, &projects).is_none(),
        "unregistered repo URL must return None"
    );
}

/// Per #1532 item 5: when the same repo_url appears under two project names,
/// the first project in the slice wins (tie-break by ordering).
#[test]
fn resolver_session_url_tie_break_first_wins() {
    let projects = vec![
        proj("alpha", "https://github.com/org/shared-repo"),
        proj("beta", "https://github.com/org/shared-repo"),
    ];
    let session = session_with_repo("https://github.com/org/shared-repo");
    let bound = resolve_session_project(&session, &projects).expect("tie-break");
    assert_eq!(
        bound.name, "alpha",
        "tie-break: first project in registry order wins"
    );
}

// ---- fleet_by_project ----

#[test]
fn resolver_fleet_by_project_basic() {
    let projects = vec![
        proj("trusty-tools", "https://github.com/org/trusty-tools"),
        proj("trusty-search", "https://github.com/org/trusty-search"),
    ];
    let sessions = vec![
        session_with_repo("https://github.com/org/trusty-tools"),
        session_with_repo("https://github.com/org/trusty-tools"),
        session_with_repo("https://github.com/org/trusty-search"),
    ];
    let fleet = fleet_by_project(&sessions, &projects);
    assert_eq!(fleet.len(), 2, "one fleet entry per project");
    let tools_fleet = fleet
        .iter()
        .find(|f| f.project.name == "trusty-tools")
        .expect("tools");
    assert_eq!(tools_fleet.sessions.len(), 2);
    let search_fleet = fleet
        .iter()
        .find(|f| f.project.name == "trusty-search")
        .expect("search");
    assert_eq!(search_fleet.sessions.len(), 1);
}

#[test]
fn resolver_fleet_empty_sessions() {
    let projects = vec![proj("trusty-tools", "https://github.com/org/trusty-tools")];
    let fleet = fleet_by_project(&[], &projects);
    assert_eq!(fleet.len(), 1, "project with no sessions still appears");
    assert!(fleet[0].sessions.is_empty());
}

#[test]
fn resolver_fleet_no_repo_sessions_omitted() {
    let projects = vec![proj("trusty-tools", "https://github.com/org/trusty-tools")];
    let sessions = vec![
        session_no_repo(),
        session_with_repo("https://github.com/other/unregistered"),
    ];
    let fleet = fleet_by_project(&sessions, &projects);
    assert_eq!(
        fleet[0].sessions.len(),
        0,
        "unregistered/no-repo sessions omitted"
    );
}

#[test]
fn resolver_fleet_empty_projects() {
    let sessions = vec![session_with_repo("https://github.com/org/x")];
    let fleet = fleet_by_project(&sessions, &[]);
    assert!(fleet.is_empty(), "empty projects yields empty fleet");
}

// ---- reason label ----

#[test]
fn resolver_reason_label() {
    assert!(!ResolutionReason::ExactNameMatch.label().is_empty());
    assert!(!ResolutionReason::UrlMatch.label().is_empty());
    assert!(!ResolutionReason::KeywordMatch.label().is_empty());
    assert!(!ResolutionReason::TicketKeyword.label().is_empty());
}
