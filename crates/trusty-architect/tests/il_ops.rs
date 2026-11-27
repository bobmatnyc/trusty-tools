//! The `datg` verbs end to end: resolution, ids, notify, usage errors, the binary.
//!
//! Why: the golden test proves bytes for one scripted session. These tests pin the
//! behaviours it cannot: state-directory resolution through the CLI, id recycling,
//! the escalation hand-off, usage errors (exit 2, nothing written), the verbs that
//! are not ported, and that the real binary agrees with the in-process run.
//! What: one test per behaviour, all on a copy of the fixture ledger.
//! Test: `session_flag_and_state_dir_pick_the_directory`, `closed_ids_are_reused`,
//! `act_records_without_bumping_the_version`, `notify_failure_is_logged_and_not_fatal`,
//! `notify_is_skipped_loudly_without_a_command`, `frozen_clock_variable_warns_on_stderr`, `usage_errors_exit_two_and_write_nothing`,
//! `unported_verbs_are_refused`, `binary_round_trip`, `batch_input_must_be_a_list`.

mod il_support;

use std::fs;

use il_support::*;
use trusty_architect::il::{Env, Json};

const NOW: i64 = 1_795_900_000;

fn v_of(dir: &std::path::Path) -> i64 {
    Json::parse(&read(dir, "state.json"))
        .unwrap()
        .i("v")
        .unwrap()
}

#[test]
fn session_flag_and_state_dir_pick_the_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    let p = project.to_str().unwrap();
    let env = Env::bare(tmp.path());
    let init = run_at(
        &[
            "datg",
            "--project",
            p,
            "--session",
            "pm-1",
            "init",
            "--code",
            "pm",
        ],
        "",
        &env,
        NOW,
    );
    assert_eq!(
        (init.code, init.stdout.as_str()),
        (0, "initialised pm\n"),
        "{}",
        init.stderr
    );
    assert!(project.join(".trusty-mpm/datg/pm-1/state.json").is_file());
    let again = run_at(
        &[
            "datg",
            "--project",
            p,
            "--session",
            "pm-1",
            "init",
            "--code",
            "pm",
        ],
        "",
        &env,
        NOW,
    );
    assert_eq!(
        (again.code, again.stderr.as_str()),
        (2, "datg: state.json exists; init refuses to overwrite\n")
    );
    let exact = tmp.path().join("exact");
    let ex = run_at(
        &[
            "datg",
            "--state-dir",
            exact.to_str().unwrap(),
            "init",
            "--code",
            "ex",
        ],
        "",
        &env,
        NOW,
    );
    assert_eq!(ex.code, 0, "{}", ex.stderr);
    assert!(exact.join("state.json").is_file());
    // $DATG_SESSION selects the per-PM directory only when it already holds a state.json.
    let via_env = env.clone().with("DATG_SESSION", "pm-1");
    let brief = run_at(
        &["datg", "--project", p, "show", "--brief"],
        "",
        &via_env,
        NOW,
    );
    assert_eq!(
        (brief.code, brief.stdout.as_str()),
        (0, "no open items\n"),
        "{}",
        brief.stderr
    );
    let bad = run_at(
        &["datg", "--project", p, "--session", "bad/name", "show"],
        "",
        &env,
        NOW,
    );
    assert_eq!(
        (bad.code, bad.stderr.as_str()),
        (2, "datg: bad session name 'bad/name'\n")
    );
}

#[test]
fn closed_ids_are_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let p = project.to_str().unwrap();
    let env = Env::bare(tmp.path());
    let add = |title: &str| {
        run_at(
            &[
                "datg",
                "--project",
                p,
                "add",
                "T",
                "--title",
                title,
                "--goal",
                "fx-G01",
                "--done-when",
                "x",
            ],
            "",
            &env,
            NOW,
        )
    };
    let first = add("first").stdout;
    let close = run_at(
        &[
            "datg",
            "--project",
            p,
            "close",
            first.trim(),
            "--outcome",
            "verified",
        ],
        "",
        &env,
        NOW,
    );
    assert_eq!(close.code, 0, "{}", close.stderr);
    let state = Json::parse(&read(&datg_dir(&project), "state.json")).unwrap();
    // The pointer moved past the closed id, so the next add does not recycle it yet...
    assert_ne!(add("second").stdout, first);
    // ...but an explicit id may reuse an archived one (ids cycle).
    let reuse = run_at(
        &[
            "datg",
            "--project",
            p,
            "add",
            "T",
            "--title",
            "third",
            "--goal",
            "fx-G01",
            "--done-when",
            "x",
            "--id",
            first.trim(),
        ],
        "",
        &env,
        NOW,
    );
    assert_eq!(
        reuse.stdout,
        first,
        "{} / next pointer {:?}",
        reuse.stderr,
        state.get("next").map(Json::dump)
    );
}

#[test]
fn act_records_without_bumping_the_version() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let dir = datg_dir(&project);
    let (v, state) = (v_of(&dir), fs::read(dir.join("state.json")).unwrap());
    let out = run_at(
        &[
            "datg",
            "--project",
            project.to_str().unwrap(),
            "act",
            "--who",
            "w",
            "--what",
            "did a thing",
        ],
        "",
        &Env::bare(tmp.path()),
        NOW,
    );
    assert_eq!((out.code, out.stdout.as_str()), (0, "act recorded\n"));
    assert_eq!(v_of(&dir), v);
    assert_eq!(
        fs::read(dir.join("state.json")).unwrap(),
        state,
        "state.json is untouched"
    );
    assert!(
        read(&dir, "log.jsonl")
            .lines()
            .last()
            .unwrap()
            .contains("\"what\":\"did a thing\"")
    );
}

#[test]
fn notify_failure_is_logged_and_not_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let dir = datg_dir(&project);
    let env = Env::bare(tmp.path()).with("DATG_NOTIFY_CMD", "/usr/bin/false");
    let out = run_at(
        &[
            "datg",
            "--project",
            project.to_str().unwrap(),
            "update",
            "fx-D01",
            "--esc",
            "architect",
        ],
        "",
        &env,
        NOW,
    );
    assert_eq!(
        (out.code, out.stdout.as_str()),
        (0, "fx-D01\n"),
        "{}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("datg: pm-notify failed for fx-D01 (exit 1"),
        "{}",
        out.stderr
    );
    let last = read(&dir, "log.jsonl").lines().last().unwrap().to_owned();
    assert!(
        last.contains("\"kind\":\"notify_failure\"")
            && last.contains("fx-D01 pm-notify failed: exit 1"),
        "{last}"
    );
    let esc = Json::parse(&read(&dir, "state.json")).unwrap();
    assert_eq!(
        esc.arr("items")
            .iter()
            .find(|i| i.s("id") == "fx-D01")
            .unwrap()
            .s("esc"),
        "architect",
        "the escalation itself is recorded"
    );
}

#[test]
fn notify_is_skipped_loudly_without_a_command() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let out = run_at(
        &[
            "datg",
            "--project",
            project.to_str().unwrap(),
            "update",
            "fx-D01",
            "--esc",
            "architect",
        ],
        "",
        &Env::bare(tmp.path()),
        NOW,
    );
    assert_eq!(out.code, 0);
    assert!(
        out.stderr
            .contains("pm-notify failed for fx-D01 (DATG_NOTIFY_CMD is not set"),
        "{}",
        out.stderr
    );
    // The skip is recorded in the log like any failed notify, not only printed.
    let dir = datg_dir(&project);
    let last = read(&dir, "log.jsonl").lines().last().unwrap().to_owned();
    assert!(
        last.contains("\"kind\":\"notify_failure\"")
            && last.contains("fx-D01 pm-notify skipped: DATG_NOTIFY_CMD unset"),
        "{last}"
    );
}

#[test]
fn frozen_clock_variable_warns_on_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let p = project.to_str().unwrap();
    let out = run_bin(
        &["datg", "--project", p, "show", "--brief"],
        "",
        &[],
        Some(NOW),
    );
    assert_eq!(out.code, 0);
    assert!(
        out.stderr
            .contains("warning: TRUSTY_IL_NOW is set; the clock is frozen at"),
        "{}",
        out.stderr
    );
    let live = run_bin(&["datg", "--project", p, "show", "--brief"], "", &[], None);
    assert!(!live.stderr.contains("TRUSTY_IL_NOW"), "{}", live.stderr);
}

#[test]
fn usage_errors_exit_two_and_write_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let dir = datg_dir(&project);
    let before = (read(&dir, "state.json"), read(&dir, "log.jsonl"));
    let p = project.to_str().unwrap();
    let env = Env::bare(tmp.path());
    for args in [
        vec!["datg", "--project", p],
        vec!["datg", "--project", p, "add", "X", "--title", "t"],
        vec![
            "datg",
            "--project",
            p,
            "add",
            "T",
            "--rank",
            "x",
            "--title",
            "t",
        ],
        vec!["datg", "--project", p, "add", "T", "--nope"],
        vec!["datg", "--project", p, "update"],
        vec!["datg", "--project", p, "close", "fx-T01"],
        vec!["datg", "--project", p, "remind-done", "fx-T01", "x"],
        vec!["datg", "--project", p, "session", "get"],
        vec!["datg", "--project", p, "frobnicate"],
        vec!["nope"],
    ] {
        let out = run_at(&args, "", &env, NOW);
        assert_eq!(out.code, 2, "{args:?}: {}", out.stderr);
        assert!(!out.stderr.is_empty() && out.stdout.is_empty(), "{args:?}");
    }
    assert_eq!((read(&dir, "state.json"), read(&dir, "log.jsonl")), before);
}

#[test]
fn unported_verbs_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let p = project.to_str().unwrap();
    for verb in ["hook", "check-dispatch", "stamp"] {
        let out = run_at(
            &["datg", "--project", p, verb],
            "{}",
            &Env::bare(tmp.path()),
            NOW,
        );
        assert_eq!(out.code, 2);
        assert!(
            out.stderr.contains(&format!("{verb} is not ported")),
            "{}",
            out.stderr
        );
    }
}

#[test]
fn batch_input_must_be_a_list() {
    let tmp = tempfile::tempdir().unwrap();
    let project = project_with(tmp.path(), &fixture("base_datg"));
    let p = project.to_str().unwrap();
    let env = Env::bare(tmp.path());
    let obj = run_at(&["datg", "--project", p, "batch"], "{}", &env, NOW);
    assert_eq!(
        (obj.code, obj.stderr.as_str()),
        (2, "datg: batch input must be a JSON list\n")
    );
    let junk = run_at(&["datg", "--project", p, "batch"], "[1", &env, NOW);
    assert!(
        junk.code == 2 && junk.stderr.contains("batch input is not JSON"),
        "{}",
        junk.stderr
    );
    let not_op = run_at(&["datg", "--project", p, "batch"], "[1]", &env, NOW);
    assert_eq!(
        (not_op.code, not_op.stderr.as_str()),
        (2, "datg: each operation must be an object\n")
    );
    let unknown = run_at(
        &["datg", "--project", p, "batch"],
        r#"[{"op":"zap"}]"#,
        &env,
        NOW,
    );
    assert_eq!(
        (unknown.code, unknown.stderr.as_str()),
        (2, "datg: unknown operation 'zap'\n")
    );
}

#[test]
fn binary_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("p");
    let p = project.to_str().unwrap();
    let run = |args: &[&str], stdin: &str| run_bin(args, stdin, &[], Some(NOW));
    let a = run(&["datg", "--project", p, "init", "--code", "bn"], "");
    assert_eq!(
        (a.code, a.stdout.as_str()),
        (0, "initialised bn\n"),
        "{}",
        a.stderr
    );
    let g = run(
        &["datg", "--project", p, "add", "G", "--title", "Goal one"],
        "",
    );
    assert_eq!(g.stdout, "bn-G01\n");
    let t = run(
        &[
            "datg",
            "--project",
            p,
            "add",
            "T",
            "--title",
            "Do it",
            "--goal",
            "bn-G01",
            "--done-when",
            "done",
        ],
        "",
    );
    assert_eq!(t.stdout, "bn-T01\n");
    let digest = run(&["datg", "--project", p, "digest", "--no-log"], "");
    assert!(
        digest
            .stdout
            .starts_with("IL DIGEST bn (bn) v3 written_at=2026-11-28T21:06Z IL=fresh"),
        "{}",
        digest.stdout
    );
    let fresh = run_bin(&["datg", "--project", p, "fresh"], "", &[], Some(NOW + 30));
    assert_eq!((fresh.code, fresh.stdout.starts_with("fresh: ")), (0, true));
    let ilio = run_bin(&["ilio", "check"], "x", &[("ILIO_PROJECT", p)], Some(NOW));
    assert_eq!(
        (ilio.code, ilio.stdout.as_str()),
        (
            1,
            "IL-REJECT NO-BLOCK il: end the turn with one ```il fenced block\n"
        )
    );
    let usage = run(&[], "");
    assert_eq!(usage.code, 2);
    // The same session in process gives the same bytes.
    let tmp2 = tempfile::tempdir().unwrap();
    let p2 = tmp2.path().join("p");
    let env = Env::bare(tmp2.path());
    let q = p2.to_str().unwrap();
    run_at(
        &["datg", "--project", q, "init", "--code", "bn"],
        "",
        &env,
        NOW,
    );
    run_at(
        &["datg", "--project", q, "add", "G", "--title", "Goal one"],
        "",
        &env,
        NOW,
    );
    run_at(
        &[
            "datg",
            "--project",
            q,
            "add",
            "T",
            "--title",
            "Do it",
            "--goal",
            "bn-G01",
            "--done-when",
            "done",
        ],
        "",
        &env,
        NOW,
    );
    assert_eq!(
        read(&datg_dir(&project), "state.json"),
        read(&datg_dir(&p2), "state.json")
    );
    assert_eq!(
        read(&datg_dir(&project), "log.jsonl"),
        read(&datg_dir(&p2), "log.jsonl")
    );
}
