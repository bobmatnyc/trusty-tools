//! Reliability of the Intent Ledger writer: lock, corruption, failed steps, torn tails.
//!
//! Why: the Python scripts block forever on the lock, never fsync the directory, write
//! a log line in pieces and let an `OSError` escape as a traceback. The port must fail
//! loudly with the ledger unchanged (or, where a step cannot be undone, say exactly
//! which step landed). Each test pins one error arm and checks the bytes on disk.
//! What: lock held, corrupt and off-schema state, a failed state replace (and the
//! orphan archive line the next lock drops), a failed archive append, a failed log
//! append after the state advanced, a torn log tail, concurrent writers, readers
//! beside a writer, a read-only directory, and the size bars.
//! Test: `lock_held_changes_nothing`, `corrupt_state_is_refused_untouched`,
//! `off_schema_state_is_refused_untouched`, `failed_rename_leaves_state_and_reconciles_archive`,
//! `failed_archive_append_writes_nothing`, `failed_log_append_reports_the_advanced_state`,
//! `torn_log_tail_is_repaired_by_the_next_append`, `concurrent_writers_serialise`,
//! `readers_never_fail_beside_a_writer`, `read_only_dir_is_a_loud_failure`,
//! `size_bars_warn_on_crossing_and_refuse_growth`,
//! `rotate_never_overwrites_an_unreadable_rotated_file`.

mod il_support;

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use il_support::*;
use trusty_architect::FakeClock;
use trusty_architect::il::store::{Paths, lock};
use trusty_architect::il::{Ctx, Env, Fault, Json, Ledger};

const NOW: i64 = 1_800_000_000;

fn fresh_project(tmp: &Path) -> std::path::PathBuf {
    project_with(tmp, &fixture("base_datg"))
}

fn env(tmp: &Path) -> Env {
    Env::bare(tmp).with("TRUSTY_IL_LOCK_WAIT_MS", "150")
}

fn bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    names(dir)
        .into_iter()
        .map(|n| (n.clone(), fs::read(dir.join(&n)).unwrap()))
        .collect()
}

fn add_task(project: &Path, env: &Env) -> trusty_architect::il::Out {
    let p = project.to_str().unwrap();
    run_at(
        &[
            "datg",
            "--project",
            p,
            "add",
            "T",
            "--title",
            "x",
            "--goal",
            "fx-G01",
            "--done-when",
            "y",
        ],
        "",
        env,
        NOW,
    )
}

fn ledger(project: &Path) -> Ledger {
    Ledger::new(Paths::at(datg_dir(project)))
}

fn ops(text: &str) -> Vec<Json> {
    Json::parse(text).unwrap().as_arr().unwrap().to_vec()
}

fn ctx(clock: &FakeClock) -> Ctx<'_> {
    Ctx {
        clock,
        env_session: String::new(),
        warnings: Vec::new(),
    }
}

#[test]
fn lock_held_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    let before = bytes(&dir);
    let held = lock(&Paths::at(dir.clone()), Duration::from_secs(1)).unwrap();
    let start = std::time::Instant::now();
    let out = add_task(&project, &env(tmp.path()));
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(
        out.stderr.contains("ledger locked") && out.stderr.contains("nothing changed"),
        "{}",
        out.stderr
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the wait is bounded"
    );
    drop(held);
    let after: Vec<_> = bytes(&dir)
        .into_iter()
        .filter(|(n, _)| n != ".lock")
        .collect();
    let before: Vec<_> = before.into_iter().filter(|(n, _)| n != ".lock").collect();
    assert_eq!(after, before);
    assert_eq!(
        add_task(&project, &env(tmp.path())).code,
        0,
        "the lock frees when dropped"
    );
}

#[test]
fn corrupt_state_is_refused_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    fs::write(dir.join("state.json"), "{\"code\":\"fx\",\"v\":").unwrap();
    let before = bytes(&dir);
    let out = add_task(&project, &env(tmp.path()));
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("state.json malformed") && out.stderr.contains("refusing to write"),
        "{}",
        out.stderr
    );
    assert_eq!(
        bytes(&dir),
        before,
        "no repair, no log line, no archive line"
    );
    let digest = run_at(
        &[
            "datg",
            "--project",
            project.to_str().unwrap(),
            "digest",
            "--no-log",
        ],
        "",
        &env(tmp.path()),
        NOW,
    );
    assert_eq!(digest.code, 2);
}

#[test]
fn off_schema_state_is_refused_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    let state =
        read(&dir, "state.json").replacen("\"status\":\"open\"", "\"status\":\"sideways\"", 1);
    fs::write(dir.join("state.json"), &state).unwrap();
    let before = bytes(&dir);
    let out = add_task(&project, &env(tmp.path()));
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("state.json schema:") && out.stderr.contains(".status"),
        "{}",
        out.stderr
    );
    assert_eq!(bytes(&dir), before);
}

#[test]
fn failed_rename_leaves_state_and_reconciles_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    let state_before = fs::read(dir.join("state.json")).unwrap();
    let archive_before = fs::read(dir.join("archive.jsonl")).unwrap();
    let log_before = fs::read(dir.join("log.jsonl")).unwrap();
    let clock = FakeClock::new(NOW as f64);
    let mut l = ledger(&project);
    l.fault = Some(Fault::Rename);
    let close = ops(r#"[{"op":"update","id":"fx-D01","status":"done","outcome":"verified"}]"#);
    let err = l.write_ops(&mut ctx(&clock), &close).unwrap_err();
    assert!(err.to_string().contains("injected rename failure"), "{err}");
    assert_eq!(
        fs::read(dir.join("state.json")).unwrap(),
        state_before,
        "the old state stands"
    );
    assert_eq!(
        fs::read(dir.join("log.jsonl")).unwrap(),
        log_before,
        "no log line for a write that did not land"
    );
    assert!(
        names(&dir).iter().all(|n| !n.contains(".tmp.")),
        "no temp file left: {:?}",
        names(&dir)
    );
    let archive_after = fs::read(dir.join("archive.jsonl")).unwrap();
    assert!(
        archive_after.len() > archive_before.len(),
        "the archive line was written first (an orphan, cv > v)"
    );
    // The next writer's lock drops the orphan, then writes normally.
    l.fault = None;
    l.write_ops(
        &mut ctx(&clock),
        &ops(r#"[{"op":"act","who":"w","what":"after"}]"#),
    )
    .unwrap();
    assert_eq!(
        fs::read(dir.join("archive.jsonl")).unwrap(),
        archive_before,
        "the orphan is gone"
    );
}

#[test]
fn failed_archive_append_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    let before = bytes(&dir);
    let mut l = ledger(&project);
    l.fault = Some(Fault::Archive);
    let close = ops(r#"[{"op":"update","id":"fx-D01","status":"done","outcome":"verified"}]"#);
    let err = l
        .write_ops(&mut ctx(&FakeClock::new(NOW as f64)), &close)
        .unwrap_err();
    assert!(
        err.to_string().contains("injected archive failure"),
        "{err}"
    );
    let after: Vec<_> = bytes(&dir)
        .into_iter()
        .filter(|(n, _)| n != ".lock")
        .collect();
    assert_eq!(
        after,
        before
            .into_iter()
            .filter(|(n, _)| n != ".lock")
            .collect::<Vec<_>>()
    );
}

#[test]
fn failed_log_append_reports_the_advanced_state() {
    // The state replace cannot be undone once it landed, so the error says so: never a warning.
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    let v_before = Json::parse(&read(&dir, "state.json"))
        .unwrap()
        .i("v")
        .unwrap();
    let mut l = ledger(&project);
    l.fault = Some(Fault::Log);
    let rename = ops(r#"[{"op":"update","id":"fx-T01","title":"renamed"}]"#);
    let err = l
        .write_ops(&mut ctx(&FakeClock::new(NOW as f64)), &rename)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(&format!("state.json advanced to v{}", v_before + 1))
            && err.contains("log append failed"),
        "{err}"
    );
    assert_eq!(
        Json::parse(&read(&dir, "state.json")).unwrap().i("v"),
        Some(v_before + 1)
    );
    // Through the CLI the same failure is a non-zero exit (the binary maps every Err to 2).
}

#[test]
fn torn_log_tail_is_repaired_by_the_next_append() {
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    let mut log = fs::read(dir.join("log.jsonl")).unwrap();
    log.extend_from_slice(b"{\"ts\":\"2026-11-1");
    fs::write(dir.join("log.jsonl"), &log).unwrap();
    let p = project.to_str().unwrap();
    let out = run_at(
        &[
            "datg",
            "--project",
            p,
            "act",
            "--who",
            "w",
            "--what",
            "after the tear",
        ],
        "",
        &env(tmp.path()),
        NOW,
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    let text = read(&dir, "log.jsonl");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[lines.len() - 2],
        "{\"ts\":\"2026-11-1",
        "the torn line stays a line of its own"
    );
    assert!(
        Json::parse(lines[lines.len() - 1]).is_ok(),
        "the new line is whole"
    );
    let acts = run_at(&["datg", "--project", p, "acts"], "", &env(tmp.path()), NOW);
    assert!(acts.stdout.contains("after the tear"), "{}", acts.stdout);
}

#[test]
fn concurrent_writers_serialise() {
    let tmp = tempfile::tempdir().unwrap();
    let project = Arc::new(fresh_project(tmp.path()));
    let dir = datg_dir(&project);
    let v0 = Json::parse(&read(&dir, "state.json"))
        .unwrap()
        .i("v")
        .unwrap();
    let log0 = read(&dir, "log.jsonl").lines().count();
    let mut handles = Vec::new();
    for t in 0..4 {
        let project = Arc::clone(&project);
        let root = tmp.path().to_path_buf();
        handles.push(std::thread::spawn(move || {
            let e = Env::bare(root).with("TRUSTY_IL_LOCK_WAIT_MS", "20000");
            for n in 0..15 {
                let next = format!("writer {t} op {n}");
                let out = run_at(
                    &[
                        "datg",
                        "--project",
                        project.to_str().unwrap(),
                        "session",
                        "set",
                        "--next",
                        &next,
                    ],
                    "",
                    &e,
                    NOW + n,
                );
                assert_eq!(out.code, 0, "{}", out.stderr);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let st = Json::parse(&read(&dir, "state.json")).unwrap();
    assert_eq!(
        st.i("v"),
        Some(v0 + 60),
        "every op landed, v advanced once per commit"
    );
    let text = read(&dir, "log.jsonl");
    assert_eq!(text.lines().count(), log0 + 60);
    assert!(text.lines().all(|l| Json::parse(l).is_ok()), "no torn line");
    assert!(text.ends_with('\n'));
}

#[test]
fn readers_never_fail_beside_a_writer() {
    let tmp = tempfile::tempdir().unwrap();
    let project = Arc::new(fresh_project(tmp.path()));
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (project, stop, root) = (
            Arc::clone(&project),
            Arc::clone(&stop),
            tmp.path().to_path_buf(),
        );
        std::thread::spawn(move || {
            let e = Env::bare(root).with("TRUSTY_IL_LOCK_WAIT_MS", "20000");
            let mut n = 0;
            while !stop.load(Ordering::Relaxed) && n < 200 {
                let next = format!("op {n}");
                assert_eq!(
                    run_at(
                        &[
                            "datg",
                            "--project",
                            project.to_str().unwrap(),
                            "session",
                            "set",
                            "--next",
                            &next
                        ],
                        "",
                        &e,
                        NOW + n
                    )
                    .code,
                    0
                );
                n += 1;
            }
        })
    };
    let e = Env::bare(tmp.path());
    for _ in 0..40 {
        let p = project.to_str().unwrap();
        for args in [
            vec!["datg", "--project", p, "digest", "--no-log"],
            vec!["datg", "--project", p, "fresh"],
            vec!["datg", "--project", p, "show", "--brief"],
        ] {
            let out = run_at(&args, "", &e, NOW);
            assert!(
                out.code == 0 || args[3] == "fresh",
                "{args:?} -> {} {}",
                out.code,
                out.stderr
            );
        }
    }
    stop.store(true, Ordering::Relaxed);
    writer.join().unwrap();
}

#[test]
fn read_only_dir_is_a_loud_failure() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    fs::write(dir.join(".lock"), "").unwrap();
    let before = bytes(&dir);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let out = add_task(&project, &env(tmp.path()));
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.code, 2, "{}", out.stderr);
    assert!(!out.stderr.trim().is_empty());
    assert_eq!(bytes(&dir), before, "state, log and archive untouched");
}

#[test]
fn size_bars_warn_on_crossing_and_refuse_growth() {
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    let p = project.to_str().unwrap();
    // Pad the state to just under 32 KB with open items, then cross the bar with one add.
    let mut st = Json::parse(&read(&dir, "state.json")).unwrap();
    let pad = |n: usize| {
        let items: Vec<Json> = (0..n)
            .map(|k| {
                Json::parse(&format!(
                    "{{\"id\":\"fx-D{}\",\"kind\":\"question\",\"status\":\"open\",\"goal\":\"fx-G01\",\"title\":\"{}\",\"opts\":[],\"answer\":\"\",\"outcome\":\"\",\"esc\":null}}",
                    trusty_architect_id(k + 10),
                    "p".repeat(100)
                ))
                .unwrap()
            })
            .collect();
        Json::Arr(items)
    };
    let n = (1..).find(|n| {
        st.set("items", pad(*n));
        st.dump().len() > 31_000
    });
    let text = st.dump();
    assert!(text.len() < 32_000, "{n:?} items: {} B", text.len());
    fs::write(dir.join("state.json"), format!("{text}\n")).unwrap();
    let e = Env::bare(tmp.path());
    for k in 0..12 {
        let out = run_at(
            &[
                "datg",
                "--project",
                p,
                "add",
                "D",
                "--title",
                &format!("{}{k}", "q".repeat(100)),
                "--goal",
                "fx-G01",
            ],
            "",
            &e,
            NOW,
        );
        assert_eq!(out.code, 0, "{}", out.stderr);
    }
    let log = read(&dir, "log.jsonl");
    let warned = log
        .lines()
        .filter(|l| l.contains("\"kind\":\"size_warning\""))
        .count();
    assert_eq!(warned, 1, "logged once, on the upward crossing");
    assert!(
        log.contains("state.json 32") && log.contains("B > 32768 B bar; items "),
        "{log}"
    );
    // Past 64 KB a growing write is refused and nothing changes.
    st.set("items", pad(340));
    fs::write(dir.join("state.json"), format!("{}\n", st.dump())).unwrap();
    let before = bytes(&dir);
    let out = run_at(
        &[
            "datg",
            "--project",
            p,
            "add",
            "D",
            "--title",
            "one more",
            "--goal",
            "fx-G01",
        ],
        "",
        &e,
        NOW,
    );
    assert_eq!(out.code, 2);
    assert!(
        out.stderr.contains("> 65536 B bar")
            && out.stderr.contains("close or shorten the named items"),
        "{}",
        out.stderr
    );
    assert_eq!(bytes(&dir), before);
}

/// Two base-34 digits for the padding ids.
fn trusty_architect_id(n: usize) -> String {
    let d = "0123456789abcdefghijkmnpqrstuvwxyz".as_bytes();
    format!("{}{}", d[n / 34 % 34] as char, d[n % 34] as char)
}

#[test]
fn rotate_never_overwrites_an_unreadable_rotated_file() {
    // Today's rotated file exists but is not UTF-8: replacing it with this run's lines alone
    // would lose what it held, so the rotate fails and nothing changes.
    let tmp = tempfile::tempdir().unwrap();
    let project = fresh_project(tmp.path());
    let dir = datg_dir(&project);
    let rotated = dir.join("log-20270115.jsonl");
    fs::write(&rotated, [0xff, 0xfe, b'\n']).unwrap();
    let before = bytes(&dir);
    let p = project.to_str().unwrap();
    let out = run_at(
        &["datg", "--project", p, "rotate"],
        "",
        &env(tmp.path()),
        NOW,
    );
    assert_eq!(out.code, 2, "{} {}", out.stdout, out.stderr);
    assert_eq!(bytes(&dir), before);
    assert_eq!(fs::read(&rotated).unwrap(), [0xff, 0xfe, b'\n']);
}
