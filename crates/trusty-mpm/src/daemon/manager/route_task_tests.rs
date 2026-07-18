//! Unit tests for the pure `route-task` disambiguation helpers (WI-8, #2585).

use super::*;
use crate::project::Project;
use crate::project::resolver::{ProjectMatch, ProjectResolution, ResolutionReason};

/// Build a minimal project fixture with the given name.
fn project(name: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: format!("https://github.com/acme/{name}"),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
    }
}

/// Build a candidate match with the given name and confidence.
fn candidate(name: &str, confidence: f32) -> ProjectMatch {
    ProjectMatch {
        project: project(name),
        confidence,
        reason: ResolutionReason::KeywordMatch,
    }
}

#[test]
fn pick_from_reply_exact() {
    let candidates = vec![candidate("alpha", 0.8), candidate("beta", 0.7)];
    let picked = pick_from_reply("beta", &candidates).expect("exact match");
    assert_eq!(picked.project.name, "beta");
}

#[test]
fn pick_from_reply_case_insensitive_and_trimmed() {
    let candidates = vec![candidate("Alpha", 0.8), candidate("Beta", 0.7)];
    let picked = pick_from_reply("  ALPHA \n", &candidates).expect("case-insensitive match");
    assert_eq!(picked.project.name, "Alpha");
}

#[test]
fn pick_from_reply_substring() {
    let candidates = vec![candidate("alpha", 0.8), candidate("beta", 0.7)];
    let picked =
        pick_from_reply("I would route this to beta.", &candidates).expect("substring match");
    assert_eq!(picked.project.name, "beta");
}

#[test]
fn pick_from_reply_prefers_longer_name_on_substring() {
    // "web" is a prefix of "web-api"; a reply naming the specific project must
    // resolve to the longer, more specific candidate.
    let candidates = vec![candidate("web", 0.7), candidate("web-api", 0.7)];
    let picked = pick_from_reply("the web-api project", &candidates).expect("longest wins");
    assert_eq!(picked.project.name, "web-api");
}

#[test]
fn pick_from_reply_unlisted_is_none() {
    let candidates = vec![candidate("alpha", 0.8), candidate("beta", 0.7)];
    assert!(pick_from_reply("gamma", &candidates).is_none());
}

#[test]
fn pick_from_reply_empty_is_none() {
    let candidates = vec![candidate("alpha", 0.8)];
    assert!(pick_from_reply("   ", &candidates).is_none());
}

#[test]
fn build_disambiguation_messages_lists_candidates() {
    let candidates = vec![candidate("alpha", 0.82), candidate("beta", 0.71)];
    let messages = build_disambiguation_messages("fix the flaky auth test", &candidates);
    assert_eq!(messages.len(), 2);
    // The user message must carry the task text and every candidate name.
    let user = &messages[1];
    let rendered = format!("{user:?}");
    assert!(rendered.contains("fix the flaky auth test"), "{rendered}");
    assert!(rendered.contains("alpha"), "{rendered}");
    assert!(rendered.contains("beta"), "{rendered}");
}

#[test]
fn route_from_resolution_unambiguous_is_resolver() {
    let resolution = ProjectResolution {
        matches: vec![candidate("alpha", 0.95)],
        primary: Some(candidate("alpha", 0.95)),
    };
    let resp = resolver_response(&resolution, None);
    assert_eq!(resp.project.as_deref(), Some("alpha"));
    assert_eq!(resp.resolved_by, ResolvedBy::Resolver);
    assert!((resp.confidence - 0.95).abs() < f32::EPSILON);
}

#[test]
fn route_from_resolution_resolver_note_appended() {
    let resolution = ProjectResolution {
        matches: vec![candidate("alpha", 0.7), candidate("beta", 0.65)],
        primary: Some(candidate("alpha", 0.7)),
    };
    let resp = resolver_response(&resolution, Some("no provider available"));
    assert_eq!(resp.resolved_by, ResolvedBy::Resolver);
    assert!(
        resp.rationale.contains("no provider available"),
        "{}",
        resp.rationale
    );
}

#[test]
fn route_from_resolution_no_match() {
    let resp = no_match_response("no registered project matched the task");
    assert!(resp.project.is_none());
    assert_eq!(resp.confidence, 0.0);
    assert_eq!(resp.resolved_by, ResolvedBy::NoMatch);
}
