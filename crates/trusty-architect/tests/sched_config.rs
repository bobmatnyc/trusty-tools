//! Config loading, validation and the state file (MB1595).

mod sched_support;

use std::path::Path;

use sched_support::layout;
use trusty_architect::sched::SchedError;
use trusty_architect::sched::config::{Kind, Registry};
use trusty_architect::sched::state::{JobState, SchedState};

/// Every job class the shipped 47-job registry holds, in its own shapes.
const SHIPPED_SHAPE: &str = r#"{
  "$schema": "registry.schema.json", "version": 1,
  "limits": {"agent_daily_cap": 20, "max_pending": 5, "max_triggers_per_run": 5},
  "defaults": {"il_task_agent": {"name": "il-task", "model": "haiku", "tools": ["Read"], "timeout_s": 90}},
  "jobs": [
    {"id": "meta-due", "title": "Meta-task schedule check", "owner": "architect", "kind": "script",
     "script": {"argv": ["python3", "scripts/meta_due.py", "check"]},
     "schedule": {"every": "10m", "jitter_s": 30, "catch_up": "run_once"}, "enabled": true, "daily_cap": 0},
    {"id": "M01", "title": "Poll cycle", "owner": "architect",
     "schedule": {"every": "10m", "catch_up": "run_once", "jitter_s": 0}, "enabled": true,
     "daily_cap": 0, "managed_by": "external:poller",
     "check": {"artifact": {"type": "glob", "path": "inbox/poll.log", "max_age_h": 0.5},
               "runner": "fleet-poll.py", "escalate_after": 2, "rule": "x"}},
    {"id": "M08", "title": "IL goal agent", "owner": "architect",
     "schedule": {"cron": "0 12 * * *", "catch_up": "run_once", "jitter_s": 0}, "enabled": true,
     "daily_cap": 1, "kind": "agent",
     "check": {"artifact": {"type": "glob", "path": "local/briefs/goal-review/*.md", "max_age_h": 30}},
     "agent": {"name": "il-goal", "model": "sonnet", "prompt": "Review {date}.",
               "tools": ["Read", "Grep"], "timeout_s": 300, "output_file": "local/briefs/goal-review/{date}.md"}},
    {"id": "M20", "title": "Weekly", "owner": "pm:tm-trusty-tools",
     "schedule": {"cron": "0 9 * * 1", "catch_up": "skip"}, "enabled": false,
     "reason": "owner paused it", "managed_by": "meta_due"}
  ]}"#;

fn parse(text: &str) -> Result<Registry, SchedError> {
    Registry::parse(text, Path::new("r.json"))
}

#[test]
fn shipped_shape_registry_loads() {
    let reg = parse(SHIPPED_SHAPE).expect("loads unchanged");
    assert_eq!(reg.jobs.len(), 4);
    let kinds: Vec<Kind> = reg.jobs.iter().map(|j| j.run_kind()).collect();
    assert_eq!(
        kinds,
        [Kind::Script, Kind::NotRun, Kind::Agent, Kind::NotRun]
    );
    // Extension defaults: script timeout 600 s, agent timeout from the agent.
    assert_eq!(reg.jobs[0].timeout(), 600);
    assert_eq!(reg.jobs[2].timeout(), 300);
    assert_eq!(reg.poll.every_s, 60);
    assert!(reg.poll.enabled);
}

#[test]
fn unknown_keys_are_ignored() {
    let text = SHIPPED_SHAPE.replace(
        r#""version": 1,"#,
        r#""version": 1, "future": {"a": [1]}, "#,
    );
    let text = text.replace(
        r#""id": "meta-due","#,
        r#""id": "meta-due", "extra": true,"#,
    );
    assert_eq!(parse(&text).unwrap().jobs.len(), 4);
}

#[test]
fn empty_config_is_a_hard_error() {
    for blank in ["", "  \n\t"] {
        let e = parse(blank).unwrap_err();
        assert!(matches!(e, SchedError::Empty(_)), "{e}");
        assert_eq!(e.exit_code(), 2);
    }
    // Valid JSON with no jobs is a failure too, never a pass.
    let e = parse(r#"{"version": 1, "jobs": []}"#).unwrap_err();
    assert!(matches!(e, SchedError::Invalid(_)));
    assert_eq!(e.exit_code(), 1);
    assert!(matches!(
        parse("{not json").unwrap_err(),
        SchedError::Parse { .. }
    ));
}

#[test]
fn unreadable_config_is_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    let e = Registry::load(&dir.path().join("missing.json")).unwrap_err();
    assert!(matches!(e, SchedError::Io { .. }), "{e}");
    assert_eq!(e.exit_code(), 2);
    // A directory is unreadable as a file.
    assert!(Registry::load(dir.path()).is_err());
}

#[test]
fn validate_reports_each_broken_rule() {
    let broken = |job: &str| match parse(&format!(r#"{{"version":1,"jobs":[{job}]}}"#)) {
        Err(SchedError::Invalid(p)) => p.join(" | "),
        other => panic!("expected invalid, got {other:?}"),
    };
    let base = r#""title":"t","owner":"architect","enabled":true"#;
    let sched = r#""schedule":{"every":"1m"}"#;
    let cases = [
        (
            format!(r#"{{"id":"1bad",{base},"kind":"script","script":{{"argv":["x"]}},{sched}}}"#),
            "id must match",
        ),
        (
            format!(
                r#"{{"id":"ok1","title":"t","owner":"bob","enabled":true,"kind":"script","script":{{"argv":["x"]}},{sched}}}"#
            ),
            "owner",
        ),
        (
            format!(
                r#"{{"id":"ok1","title":"t","owner":"architect","enabled":false,"kind":"script","script":{{"argv":["x"]}},{sched}}}"#
            ),
            "reason is required",
        ),
        (
            format!(r#"{{"id":"ok1",{base},{sched}}}"#),
            "kind is required",
        ),
        (
            format!(r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":[]}},{sched}}}"#),
            "script.argv",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},"schedule":{{"every":"1m","cron":"* * * * *"}}}}"#
            ),
            "exactly one",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},"schedule":{{"every":"1w"}}}}"#
            ),
            "schedule.every",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},"schedule":{{"cron":"61 * * * *"}}}}"#
            ),
            "schedule.cron",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},"schedule":{{"every":"1m","tz":"America/New_York"}}}}"#
            ),
            "only UTC",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},"schedule":{{"every":"1m","catch_up":"later"}}}}"#
            ),
            "catch_up",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},{sched},"overlap":"allow"}}"#
            ),
            "overlap",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},{sched},"cwd":"../etc"}}"#
            ),
            "cwd",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},{sched},"managed_by":"cron"}}"#
            ),
            "managed_by",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"agent","agent":{{"name":"a","model":"m","timeout_s":5,"output_file":"/etc/x"}},{sched}}}"#
            ),
            "output_file",
        ),
        (
            format!(
                r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},{sched},"check":{{"artifact":{{"type":"glob","max_age_h":0}}}}}}"#
            ),
            "max_age_h",
        ),
    ];
    for (job, expect) in cases {
        let got = broken(&job);
        assert!(
            got.contains(expect),
            "wanted {expect:?} in {got:?} for {job}"
        );
    }
    // Two jobs with one id.
    let two = format!(r#"{{"id":"ok1",{base},"kind":"script","script":{{"argv":["x"]}},{sched}}}"#);
    assert!(broken(&format!("{two},{two}")).contains("duplicate id"));
}

#[test]
fn state_round_trips_through_an_atomic_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("local/meta/sched-state.json");
    let mut st = SchedState::default();
    st.jobs.insert(
        "meta-due".into(),
        JobState {
            next_due: 1_700_000_600,
            last_run: Some(1_700_000_000),
            last_result: Some("ok".into()),
            misses: 2,
            ..JobState::default()
        },
    );
    st.poll_next = 99;
    st.save(&path).unwrap();
    assert_eq!(SchedState::load(&path).unwrap(), st);
    // The temp file is gone after the rename.
    assert!(!path.with_extension("json.tmp").exists());
    // Absent means fresh.
    assert_eq!(
        SchedState::load(&dir.path().join("none.json")).unwrap(),
        SchedState::default()
    );
}

#[test]
fn corrupt_state_is_an_error_not_a_reset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.json");
    std::fs::write(&path, "{\"jobs\": [oops").unwrap();
    assert!(matches!(
        SchedState::load(&path),
        Err(SchedError::Parse { .. })
    ));
}

#[test]
fn layout_paths_follow_the_contract() {
    let l = layout(Path::new("/arch"));
    assert_eq!(l.registry(), Path::new("/arch/local/meta/registry.json"));
    assert_eq!(l.runs(), Path::new("/arch/local/meta/runs.jsonl"));
    assert_eq!(
        l.job_log("M08", "2026-11-21"),
        Path::new("/arch/local/logs/jobs/M08/2026-11-21.log")
    );
    assert_eq!(l.usage(), Path::new("/arch/local/usage/meta-agent.jsonl"));
    assert_eq!(l.alerts(), Path::new("/arch/inbox/alerts.jsonl"));
}
