//! Unit tests for [`super`] — the turn-1 startup-context budget (#7424).

use super::*;

/// One transcript line in Claude Code's shape, carrying #4513's usage fields.
fn transcript_with(dir: &Path, name: &str, input: u64, creation: u64, read: u64) -> PathBuf {
    let path = dir.join(name);
    let line = format!(
        r#"{{"type":"assistant","message":{{"id":"m1","usage":{{"input_tokens":{input},"cache_creation_input_tokens":{creation},"cache_read_input_tokens":{read},"output_tokens":9}}}}}}"#
    );
    std::fs::write(&path, format!("{line}\n")).expect("write transcript");
    path
}

fn stored(root: &Path, session_id: &str, tokens: u64, project: &str, recorded_at: &str) {
    let record = StartupContextRecord {
        tokens,
        project: project.to_string(),
        recorded_at: recorded_at.to_string(),
    };
    record_session_value(
        root,
        KIND_STARTUP_CONTEXT,
        session_id,
        &serde_json::to_string(&record).expect("encode"),
    );
}

/// Why: the whole feature is one number surviving from the process that can see
/// the transcript to the two that report on it.
/// Test: itself.
#[test]
fn a_reading_round_trips_through_the_store() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("root");
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).expect("mkdir");
    let transcript = transcript_with(dir.path(), "a.jsonl", 4, 74_685, 27_297);

    let written = record_startup_context(&root, "sess-a", &project, &transcript);

    assert_eq!(written, Some(101_986));
    let read = read_startup_context(&root, "sess-a").expect("record");
    assert_eq!(read.tokens, 101_986);
    assert_eq!(read.project, project.to_string_lossy());
    assert!(!read.recorded_at.is_empty());
}

/// Why: turn 1 happens once. A later render whose head scan reached a different
/// turn must not revise the measurement — the store is the record of what the
/// session actually started with.
/// Test: itself.
#[test]
fn a_second_observation_never_overwrites_the_first() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("root");
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).expect("mkdir");
    let first = transcript_with(dir.path(), "a.jsonl", 1, 10, 0);
    let second = transcript_with(dir.path(), "b.jsonl", 1, 999_000, 0);

    assert_eq!(
        record_startup_context(&root, "s", &project, &first),
        Some(11)
    );
    assert_eq!(record_startup_context(&root, "s", &project, &second), None);
    assert_eq!(read_startup_context(&root, "s").expect("record").tokens, 11);
}

/// Why: the first renders of a session happen before any assistant turn exists.
/// Recording a `0` then would put a fabricated, very small startup into every
/// later sample.
/// Test: itself.
#[test]
fn a_transcript_with_no_assistant_turn_records_nothing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("root");
    let transcript = dir.path().join("empty.jsonl");
    std::fs::write(&transcript, "{\"type\":\"user\"}\n").expect("write");

    assert_eq!(
        record_startup_context(&root, "s", dir.path(), &transcript),
        None
    );
    assert!(read_startup_context(&root, "s").is_none());
}

/// Why (#7424 criterion): the store holds every project on the machine, and the
/// check answers for the one it was run in.
/// Test: itself.
#[test]
fn samples_exclude_another_projects_sessions() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("root");
    let mine = dir.path().join("mine");
    let theirs = dir.path().join("theirs");
    std::fs::create_dir_all(&mine).expect("mkdir");
    std::fs::create_dir_all(&theirs).expect("mkdir");
    stored(
        &root,
        "a",
        10,
        &mine.to_string_lossy(),
        "2026-09-11T10:00:00Z",
    );
    stored(
        &root,
        "b",
        99,
        &theirs.to_string_lossy(),
        "2026-09-11T11:00:00Z",
    );

    let samples = startup_context_for_project(&root, &mine, 10);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].tokens, 10);
}

/// Why: a managed session runs in a worktree under the project, so an equality
/// test on the directory would drop every worktree session from its own
/// project's sample.
/// Test: itself.
#[test]
fn a_worktree_under_the_project_is_the_same_project() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("root");
    let project = dir.path().join("proj");
    let worktree = project.join(".claude").join("worktrees").join("agent-1");
    std::fs::create_dir_all(&worktree).expect("mkdir");
    stored(
        &root,
        "a",
        42,
        &worktree.to_string_lossy(),
        "2026-09-11T10:00:00Z",
    );

    let samples = startup_context_for_project(&root, &project, 10);
    assert_eq!(samples.len(), 1, "a worktree belongs to its project");
    assert_eq!(samples[0].tokens, 42);
    // And the reverse: doctor run from inside the worktree still sees it.
    assert_eq!(startup_context_for_project(&root, &worktree, 10).len(), 1);
}

/// Why: "the latest" is the reading a verdict reports as the newest, so the
/// order is load-bearing, and the cap is what keeps the read bounded on a
/// machine with hundreds of recorded sessions.
/// Test: itself.
#[test]
fn samples_are_newest_first_and_capped() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path().join("root");
    let project = dir.path().join("proj");
    std::fs::create_dir_all(&project).expect("mkdir");
    let project_str = project.to_string_lossy().to_string();
    stored(&root, "a", 1, &project_str, "2026-09-09T10:00:00Z");
    stored(&root, "b", 2, &project_str, "2026-09-10T10:00:00Z");
    stored(&root, "c", 3, &project_str, "2026-09-11T10:00:00Z");

    let samples = startup_context_for_project(&root, &project, 2);
    let tokens: Vec<u64> = samples.iter().map(|r| r.tokens).collect();
    assert_eq!(tokens, vec![3, 2]);
}

/// Why: a project with no reading has not passed and has not failed; the check
/// must have a third answer.
/// Test: itself.
#[test]
fn an_empty_sample_is_no_samples() {
    assert_eq!(
        evaluate_startup_context(&[], DEFAULT_CEILING_TOKENS),
        StartupContextVerdict::NoSamples
    );
}

/// Why: the ordinary green path, and the one that must stay quiet.
/// Test: itself.
#[test]
fn a_sample_under_the_ceiling_is_within() {
    assert_eq!(
        evaluate_startup_context(&[30_000, 28_000, 31_000], 50_000),
        StartupContextVerdict::Within {
            median: 30_000,
            latest: 30_000,
            ceiling: 50_000,
        }
    );
}

/// Why: the #4513 shape — a project whose startup cost has settled above the
/// target, which is the state the check exists to make visible.
/// Test: itself.
#[test]
fn a_median_over_the_ceiling_is_over() {
    assert_eq!(
        evaluate_startup_context(&[98_000, 107_000, 101_000], 50_000),
        StartupContextVerdict::Over {
            median: 101_000,
            latest: 98_000,
            ceiling: 50_000,
        }
    );
}

/// Why: the other half — a project whose history is fine and whose newest
/// session just crossed the line. A median-only test would report that as clean
/// for as many sessions as it takes to move the middle.
/// Test: itself.
#[test]
fn a_latest_over_the_ceiling_is_over() {
    assert_eq!(
        evaluate_startup_context(&[80_000, 10_000, 11_000, 12_000, 13_000], 50_000),
        StartupContextVerdict::Over {
            median: 12_000,
            latest: 80_000,
            ceiling: 50_000,
        }
    );
}

/// Why: the owner's stated target, pinned so a later edit to the constant is a
/// deliberate decision rather than a drift.
/// Test: itself.
#[test]
fn the_default_ceiling_is_fifty_thousand() {
    assert_eq!(DEFAULT_CEILING_TOKENS, 50_000);
    let resolved = resolve_startup_context(None);
    assert_eq!(resolved.ceiling_tokens, 50_000);
    assert_eq!(resolved.sessions, DEFAULT_SESSION_SAMPLE);
    assert!(resolved.enabled);
}

/// Why: the ceiling is an operator budget, so the config path is the feature.
/// Test: itself.
#[test]
fn config_overrides_the_ceiling_and_sample_size() {
    let config = StartupContextConfig {
        enabled: Some(false),
        ceiling_tokens: Some(120_000),
        sessions: Some(3),
    };
    let resolved = resolve_startup_context(Some(&config));
    assert!(!resolved.enabled);
    assert_eq!(resolved.ceiling_tokens, 120_000);
    assert_eq!(resolved.sessions, 3);
}

/// Why: a `0` reads as "unset the limit" in some config surfaces and as "warn on
/// everything" here; falling back to the default is the only reading that
/// cannot flood the report.
/// Test: itself.
#[test]
fn a_zeroed_config_falls_back_to_the_defaults() {
    let config = StartupContextConfig {
        enabled: None,
        ceiling_tokens: Some(0),
        sessions: Some(0),
    };
    let resolved = resolve_startup_context(Some(&config));
    assert_eq!(resolved.ceiling_tokens, DEFAULT_CEILING_TOKENS);
    assert_eq!(resolved.sessions, DEFAULT_SESSION_SAMPLE);
}
