//! The agent kind: argv, scratch dir, output and usage, the daily cap
//! (MB1595). No test runs `claude`: the runner is a fake.

mod sched_support;

use sched_support::*;
use trusty_architect::FakeClock;
use trusty_architect::sched::agent;
use trusty_architect::sched::config::Kind;

const M08: &str = r#"{"id":"M08","title":"IL goal agent","owner":"architect","kind":"agent",
  "schedule":{"cron":"0 12 * * *"},"enabled":true,"daily_cap":1,
  "agent":{"name":"il-goal","model":"sonnet","prompt":"Review effort for {date}.",
           "tools":["Read","Grep","Glob"],"timeout_s":300,
           "output_file":"local/briefs/goal-review/{date}.md"}}"#;

const REPLY: &str = r#"{"type":"result","result":"All ranks hold.","usage":{"input_tokens":120,"output_tokens":45}}"#;

#[test]
fn agent_plan_builds_the_contract_argv() {
    let dir = tempfile::tempdir().unwrap();
    let r = reg(M08);
    let job = &r.jobs[0];
    assert_eq!(job.run_kind(), Kind::Agent);
    let scratch_root = dir.path().join("tmp");
    let plan = agent::plan(
        job,
        &layout(dir.path()),
        &scratch_root,
        vec![],
        T0 + 12 * 3600,
    )
    .unwrap();
    assert_eq!(
        plan.spec.argv,
        [
            "claude",
            "-p",
            "--agent",
            "il-goal",
            "--model",
            "sonnet",
            "--tools",
            "Read,Grep,Glob",
            "--disable-slash-commands",
            "--strict-mcp-config",
            "--mcp-config={\"mcpServers\":{}}",
            "--output-format",
            "json",
        ]
    );
    // Runs from a scratch dir outside the root; the prompt arrives on stdin.
    assert!(
        plan.spec.cwd.starts_with(&scratch_root)
            && !plan.spec.cwd.starts_with(dir.path().join("local"))
    );
    let prompt = std::fs::read_to_string(plan.spec.stdin.as_ref().unwrap()).unwrap();
    assert_eq!(prompt, "Review effort for 2023-11-15.");
    assert_eq!(plan.spec.timeout.as_secs(), 300);
    assert_eq!(
        plan.output_file,
        Some(dir.path().join("local/briefs/goal-review/2023-11-15.md"))
    );
}

#[test]
fn agent_finish_writes_output_and_usage() {
    let dir = tempfile::tempdir().unwrap();
    let r = reg(M08);
    let l = layout(dir.path());
    let plan = agent::plan(&r.jobs[0], &l, &dir.path().join("tmp"), vec![], T0).unwrap();
    std::fs::write(&plan.spec.stdout, REPLY).unwrap();
    agent::finish(&plan, "M08", &ok(), &l, T0).unwrap();
    let out =
        std::fs::read_to_string(dir.path().join("local/briefs/goal-review/2023-11-15.md")).unwrap();
    assert_eq!(out, "All ranks hold.");
    let usage = lines(&l.usage());
    assert_eq!(usage.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&usage[0]).unwrap();
    assert_eq!(
        (v["job"].as_str(), v["model"].as_str()),
        (Some("M08"), Some("sonnet"))
    );
    assert_eq!(
        (v["in"].as_u64(), v["out"].as_u64(), v["ok"].as_bool()),
        (Some(120), Some(45), Some(true))
    );
    assert!(v["ts"].is_string());
    assert!(!plan.scratch.exists(), "scratch removed");
}

#[test]
fn agent_finish_writes_nothing_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let r = reg(M08);
    let l = layout(dir.path());
    let plan = agent::plan(&r.jobs[0], &l, &dir.path().join("tmp"), vec![], T0).unwrap();
    std::fs::write(&plan.spec.stdout, REPLY).unwrap();
    agent::finish(&plan, "M08", &timed_out(), &l, T0).unwrap();
    assert!(!dir.path().join("local/briefs").exists());
    let v: serde_json::Value = serde_json::from_str(&lines(&l.usage())[0]).unwrap();
    assert_eq!(v["ok"].as_bool(), Some(false));
}

#[test]
fn agent_job_runs_through_the_engine_once_a_day() {
    let dir = tempfile::tempdir().unwrap();
    let (clock, runner) = (FakeClock::new((T0 + 11 * 3600) as f64), FakeRunner::new());
    *runner.stdout_text.lock().unwrap() = Some(REPLY.to_owned());
    let mut e = engine(dir.path(), &clock, &runner, reg(M08));
    tick_and_wait(&mut e); // first sight: anchors 12:00
    advance(&clock, 3600);
    let (started, records) = tick_and_wait(&mut e);
    assert_eq!(started, ["M08"]);
    assert_eq!(records[0].outcome, "ok");
    assert_eq!(runner.last().argv[0], "claude");
    let out = dir.path().join("local/briefs/goal-review/2023-11-15.md");
    assert_eq!(std::fs::read_to_string(out).unwrap(), "All ranks hold.");
    // The cap is 1 per UTC day: a second slot the same day (a cron that fires
    // twice) cannot spend more tokens. Simulate by rewinding next_due.
    let mut st = e.state().clone();
    st.jobs.get_mut("M08").unwrap().next_due = T0 + 12 * 3600 + 3600;
    advance(&clock, 3600);
    let mut e2 = trusty_architect::sched::Engine::new(
        layout(dir.path()),
        &clock,
        runner.clone(),
        vec![],
        reg(M08),
        st,
        dir.path().join("scratch"),
    );
    let (started, records) = tick_and_wait(&mut e2);
    assert!(started.is_empty());
    assert_eq!(records[0].outcome, "skipped");
    assert_eq!(runner.count(), 1);
}
