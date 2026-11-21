//! Rust against Python on copies of the live ledgers (ignored: needs python3 and the ledgers).
//!
//! Why: the golden test replays a small synthetic ledger. The claim "same files, same
//! formats, switching back is just running the Python" also has to hold on the real
//! Architect ledger (24 task key orders, a megabyte of log) and tm-dogfood's legacy
//! one. These tests copy both to temp dirs, never touching a live ledger, run the
//! Python and the Rust on the same instant (`tests/support/frozen_py.py` and
//! `TRUSTY_IL_NOW`), and compare bytes.
//! What: digest byte equality at four budgets; the same IL blocks applied by both
//! (state, log, archive, stdout, exit code); a verb sweep; Python-writes-Rust-reads and
//! the reverse; mixed Python and Rust writers on one lock.
//! Run: `cargo test -p trusty-architect --test il_parity -- --include-ignored`.
//! Config (all have defaults on the Architect's machine): `ARCHITECT_SCRIPTS`,
//! `ARCHITECT_IL_LIVE` (the Architect's `.trusty-mpm/datg`), `DOGFOOD_DATG`.
//! Test: `digest_is_byte_equal_to_python`, `apply_blocks_write_the_same_ledger_as_python`,
//! `verb_sweep_writes_the_same_ledger_as_python`, `python_and_rust_read_each_others_writes`,
//! `mixed_python_and_rust_writers_serialise`.

mod il_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use il_support::*;
use trusty_architect::il::{Json, Out};

fn scripts() -> PathBuf {
    PathBuf::from(
        std::env::var("ARCHITECT_SCRIPTS")
            .unwrap_or_else(|_| "/Users/masa/trusty-mpm-projects/architect/scripts".into()),
    )
}

fn ledgers() -> Vec<(&'static str, PathBuf)> {
    let arch = std::env::var("ARCHITECT_IL_LIVE").unwrap_or_else(|_| {
        "/Users/masa/trusty-mpm-projects/architect/local/il/.trusty-mpm/datg".into()
    });
    let dog = std::env::var("DOGFOOD_DATG").unwrap_or_else(|_| {
        "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools/.trusty-mpm/datg".into()
    });
    vec![("architect", arch.into()), ("dogfood", dog.into())]
}

/// Skips (returns `None`) with a printed reason when python3 or a ledger is missing.
fn rig() -> Option<Vec<(&'static str, PathBuf)>> {
    let python = Command::new("python3").arg("--version").output().is_ok();
    let present = scripts().join("datg.py").is_file()
        && ledgers()
            .iter()
            .all(|(_, p)| p.join("state.json").is_file());
    if !python || !present {
        eprintln!(
            "SKIP: python3, {} or a live ledger is missing",
            scripts().display()
        );
        return None;
    }
    Some(ledgers())
}

fn frozen_py() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/frozen_py.py")
}

struct Side {
    project: PathBuf,
    ilio: PathBuf,
    work: PathBuf,
}

impl Side {
    fn new(root: &Path, name: &str, src: &Path) -> Side {
        let work = root.join(name);
        let project = project_with(&work, src);
        let ilio = work.join("ilio");
        fs::create_dir_all(&ilio).unwrap();
        fs::create_dir_all(work.join("dash")).unwrap();
        Side {
            project,
            ilio,
            work,
        }
    }

    fn dir(&self) -> PathBuf {
        datg_dir(&self.project)
    }

    fn env(&self) -> Vec<(String, String)> {
        let w = self.work.to_str().unwrap();
        vec![
            ("DATG_NOTIFY_CMD".into(), "/usr/bin/true".into()),
            ("ILIO_PROJECT".into(), self.project.to_str().unwrap().into()),
            ("ILIO_DIR".into(), self.ilio.to_str().unwrap().into()),
            ("ILIO_WHO".into(), "tm-architect".into()),
            ("DASHBOARD_ROOT".into(), format!("{w}/dash")),
            ("ARCHITECT_INBOX_DIR".into(), format!("{w}/inbox")),
        ]
    }

    fn py(&self, prog: &str, args: &[&str], stdin: &str, epoch: i64) -> Out {
        let mut cmd = Command::new("python3");
        cmd.args(["-I"]).arg(frozen_py()).arg(scripts()).arg(prog);
        if prog == "datg" {
            cmd.args(["--project"]).arg(&self.project);
        }
        cmd.args(args).env("FROZEN_EPOCH", epoch.to_string());
        for (k, v) in self.env() {
            cmd.env(k, v);
        }
        for k in ["DATG_SESSION", "DATG_STATE_DIR", "TMUX_PANE"] {
            cmd.env_remove(k);
        }
        exec(cmd, stdin)
    }

    fn rs(&self, prog: &str, args: &[&str], stdin: &str, epoch: i64) -> Out {
        let mut full: Vec<&str> = vec![prog];
        let proj = self.project.to_str().unwrap().to_owned();
        if prog == "datg" {
            full.extend(["--project", &proj]);
        }
        full.extend(args);
        let env = self.env();
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        run_bin(&full, stdin, &env, Some(epoch))
    }
}

fn exec(mut cmd: Command, stdin: &str) -> Out {
    use std::io::Write;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    Out {
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        code: o.status.code().unwrap_or(-1),
    }
}

fn clean(s: &str) -> String {
    s.lines()
        .filter(|l| !l.starts_with("task_steer:") && !l.starts_with("ilio: steering"))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// The first line two texts differ on, for a readable failure.
fn first_diff(a: &str, b: &str) -> String {
    let (la, lb): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let n = (0..la.len().max(lb.len()))
        .find(|&i| la.get(i) != lb.get(i))
        .unwrap_or(0);
    format!("line {n}:\n py: {:?}\n rs: {:?}", la.get(n), lb.get(n))
}

/// The two sides live in `<name>-py` and `<name>-rs` (same length); a path in the output differs by that only.
fn side_neutral(s: &str) -> String {
    s.replace("-py/", "-@@/").replace("-rs/", "-@@/")
}

fn same_run(label: &str, py: &Out, rs: &Out) {
    let (a, b) = (side_neutral(&py.stdout), side_neutral(&rs.stdout));
    assert!(a == b, "{label}: stdout differs at {}", first_diff(&a, &b));
    assert_eq!(
        clean(&side_neutral(&py.stderr)),
        clean(&side_neutral(&rs.stderr)),
        "{label}: stderr"
    );
    assert_eq!(py.code, rs.code, "{label}: exit code");
}

fn same_files(label: &str, a: &Side, b: &Side) {
    assert_eq!(names(&a.dir()), names(&b.dir()), "{label}: file set");
    for n in names(&a.dir()) {
        assert_eq!(read(&a.dir(), &n), read(&b.dir(), &n), "{label}: {n}");
    }
}

fn state(s: &Side) -> Json {
    Json::parse(&read(&s.dir(), "state.json")).unwrap()
}

fn written_epoch(s: &Side) -> i64 {
    state(s)
        .get("session")
        .and_then(|x| x.i("written_epoch"))
        .unwrap()
}

#[test]
#[ignore = "needs python3 and the live ledgers; run with --include-ignored"]
fn digest_is_byte_equal_to_python() {
    let Some(ledgers) = rig() else { return };
    let tmp = tempfile::tempdir().unwrap();
    for (name, src) in ledgers {
        let side = Side::new(tmp.path(), name, &src);
        let epoch = written_epoch(&side) + 90;
        for n in ["3000", "800", "200", "120"] {
            let args = ["digest", "--no-log", "--max-tokens", n];
            same_run(
                &format!("{name} digest {n}"),
                &side.py("datg", &args, "", epoch),
                &side.rs("datg", &args, "", epoch),
            );
        }
        // An hour later every task is past its progress age: the STALE flags flip the same way.
        let args = ["digest", "--no-log", "--max-tokens", "3000"];
        same_run(
            &format!("{name} digest +2d"),
            &side.py("datg", &args, "", epoch + 2 * 86_400),
            &side.rs("datg", &args, "", epoch + 2 * 86_400),
        );
        for args in [
            vec!["fresh"],
            vec!["fresh", "--max-age", "1"],
            vec!["show", "--brief"],
            vec!["show"],
            vec!["acts", "--limit", "5"],
            vec!["reminders", "--all"],
        ] {
            same_run(
                &format!("{name} {args:?}"),
                &side.py("datg", &args, "", epoch),
                &side.rs("datg", &args, "", epoch),
            );
        }
    }
}

/// An id of a live task with no reminders and one with a pct, for the blocks.
fn pick(s: &Side) -> (String, Option<i64>, Option<String>) {
    let st = state(s);
    let live = |t: &&Json| matches!(t.s("status"), "open" | "running");
    let any = st.arr("tasks").iter().find(live).expect("a live task");
    let closable = st.arr("tasks").iter().filter(live).find(|t| {
        t.arr("reminders").iter().all(|r| r.s("state") == "done") && t.s("id") != any.s("id")
    });
    (
        any.s("id").to_owned(),
        any.i("pct"),
        closable.map(|t| t.s("id").to_owned()),
    )
}

#[test]
#[ignore = "needs python3 and the live ledgers; run with --include-ignored"]
fn apply_blocks_write_the_same_ledger_as_python() {
    let Some(ledgers) = rig() else { return };
    let tmp = tempfile::tempdir().unwrap();
    for (name, src) in ledgers {
        let (py, rs) = (
            Side::new(tmp.path(), &format!("{name}-py"), &src),
            Side::new(tmp.path(), &format!("{name}-rs"), &src),
        );
        let epoch = written_epoch(&py) + 600;
        let (tid, pct, closable) = pick(&py);
        let cur = pct.unwrap_or(0);
        let (p1, p2, p3) = ((cur + 7) % 99 + 1, (cur + 13) % 99 + 1, (cur + 21) % 99 + 1);
        let mut blocks: Vec<(String, bool)> = vec![
            (
                format!(
                    "```il\ntask: {tid}\ndone: moved along\npct: {p1}\nremaining: 1h\nnext: continue the work\ndelta: smoke\ndecided: go\n```\n"
                ),
                true,
            ),
            (
                format!(
                    "```il\ntask: {tid}\ndone: reviewed\npct: {p2}\nnext: n\nact: reviewed it | owner: go ahead | ev: PR #9 | refs: PR #9\nevidence: e\n```\n"
                ),
                true,
            ),
            (
                format!(
                    "```il\ntask: {tid}\ndone: x\npct: {p3}\nremind: {tid} at=2027-01-01 every=7d \"ping the owner\"\nnext: n\n```\n"
                ),
                true,
            ),
            (
                format!("```il\ntask: {tid}\ndone: x\npct: {p3}\nnext: n\n```\n"),
                false,
            ),
            (
                format!(
                    "```il\ntask: {tid}\ndone: x\npct: {}\nnew: T \"Parity probe task\" done_when=\"probe passes\"\nnew: T \"Parity probe task two\" done_when=\"probe passes\"\nnext: n\n```\n",
                    p3 % 99 + 1
                ),
                true,
            ),
            ("no block at all".to_owned(), false),
        ];
        if let Some(c) = closable {
            blocks.push((format!("```il\ntask: {tid}\ndone: finished\npct: 100\nclose: {c}\nnext: n\nevidence: verified by hand\n```\n"), true));
        }
        for (n, (block, _)) in blocks.iter().enumerate() {
            let at = epoch + 60 * n as i64;
            let (a, b) = (
                py.py("ilio", &["apply"], block, at),
                rs.rs("ilio", &["apply"], block, at),
            );
            same_run(&format!("{name} block {n}"), &a, &b);
            let mut a_files = names(&py.dir());
            a_files.retain(|f| f != ".lock");
            for f in a_files {
                assert_eq!(
                    read(&py.dir(), &f),
                    read(&rs.dir(), &f),
                    "{name} block {n}: {f}"
                );
            }
        }
        same_files(&format!("{name} after blocks"), &py, &rs);
        // Rejects change nothing and report the same text.
        for bad in [
            "```il\ntask: zz-T99\ndone: x\npct: 1\nnext: n\n```\n",
            "```il\ntask: free: x\ndone: x\nnext: n\n```\n",
        ] {
            same_run(
                &format!("{name} reject"),
                &py.py("ilio", &["apply"], bad, epoch + 9000),
                &rs.rs("ilio", &["apply"], bad, epoch + 9000),
            );
        }
        same_files(&format!("{name} after rejects"), &py, &rs);
    }
}

#[test]
#[ignore = "needs python3 and the live ledgers; run with --include-ignored"]
fn verb_sweep_writes_the_same_ledger_as_python() {
    let Some(ledgers) = rig() else { return };
    let tmp = tempfile::tempdir().unwrap();
    for (name, src) in ledgers {
        let (py, rs) = (
            Side::new(tmp.path(), &format!("{name}-py"), &src),
            Side::new(tmp.path(), &format!("{name}-rs"), &src),
        );
        let epoch = written_epoch(&py) + 600;
        let (tid, _, closable) = pick(&py);
        let goal = state(&py)
            .arr("goals")
            .iter()
            .find(|g| g.s("status") == "active")
            .unwrap()
            .s("id")
            .to_owned();
        let mut steps: Vec<(Vec<String>, String, i64)> = Vec::new();
        let mut s = |args: &[&str], stdin: &str, at: i64| {
            steps.push((
                args.iter().map(|a| (*a).to_owned()).collect(),
                stdin.to_owned(),
                at,
            ))
        };
        s(
            &[
                "add",
                "T",
                "--title",
                "Sweep probe",
                "--goal",
                &goal,
                "--done-when",
                "sweep ok",
            ],
            "",
            epoch,
        );
        s(
            &[
                "add",
                "D",
                "--title",
                "Sweep decision",
                "--goal",
                &goal,
                "--opts",
                "yes*,no",
            ],
            "",
            epoch + 10,
        );
        s(
            &["add", "A", "--title", "Sweep action", "--goal", &goal],
            "",
            epoch + 20,
        );
        s(
            &["update", &tid, "--priority", "2", "--note", "sweep"],
            "",
            epoch + 30,
        );
        s(&["update", &tid, "--to", "someone"], "", epoch + 40);
        s(
            &[
                "session",
                "set",
                "--next",
                "sweep next",
                "--wait",
                "owner:sweep",
                "--ref",
                "sweepref=local/briefs/sweep.md",
            ],
            "",
            epoch + 50,
        );
        s(
            &[
                "act",
                "--who",
                "w",
                "--what",
                "sweep act",
                "--refs",
                "PR #1",
            ],
            "",
            epoch + 60,
        );
        s(
            &[
                "remind",
                &tid,
                "--at",
                "2027-02-02",
                "--note",
                "sweep reminder",
            ],
            "",
            epoch + 70,
        );
        s(&["rebalance"], "", epoch + 80);
        s(&["reminders", "--all", "--json"], "", epoch + 90);
        s(&["acts", "--limit", "3"], "", epoch + 100);
        s(
            &["batch"],
            r#"[{"op":"act","who":"w","what":"in a batch"},{"op":"pulse","id":"@T@","note":"batched pulse","pct":"55"}]"#,
            epoch + 110,
        );
        if let Some(c) = &closable {
            s(
                &["close", c, "--outcome", "verified in the sweep"],
                "",
                epoch + 120,
            );
            s(&["archive-show", c], "", epoch + 130);
        }
        s(&["prune", "--dry-run"], "", epoch + 140);
        s(&["prune"], "", epoch + 150);
        s(&["fresh"], "", epoch + 160);
        s(&["digest", "--no-log"], "", epoch + 170);
        s(&["rotate"], "", epoch + 10 * 86_400);
        s(
            &["digest", "--no-log", "--max-tokens", "800"],
            "",
            epoch + 10 * 86_400 + 10,
        );
        for (n, (args, stdin, at)) in steps.iter().enumerate() {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let stdin = stdin.replace("@T@", &tid);
            let (a, b) = (
                py.py("datg", &args, &stdin, *at),
                rs.rs("datg", &args, &stdin, *at),
            );
            same_run(&format!("{name} step {n} {args:?}"), &a, &b);
            for f in names(&py.dir()) {
                assert_eq!(
                    read(&py.dir(), &f),
                    read(&rs.dir(), &f),
                    "{name} step {n} {args:?}: {f}"
                );
            }
        }
        same_files(&format!("{name} after sweep"), &py, &rs);
    }
}

#[test]
#[ignore = "needs python3 and the live ledgers; run with --include-ignored"]
fn python_and_rust_read_each_others_writes() {
    let Some(ledgers) = rig() else { return };
    let tmp = tempfile::tempdir().unwrap();
    for (name, src) in ledgers {
        let side = Side::new(tmp.path(), name, &src);
        let at = written_epoch(&side) + 600;
        let goal = state(&side)
            .arr("goals")
            .iter()
            .find(|g| g.s("status") == "active")
            .unwrap()
            .s("id")
            .to_owned();
        let add = [
            "add",
            "T",
            "--title",
            "Cross probe",
            "--goal",
            &goal,
            "--done-when",
            "x",
        ];
        let digest = ["digest", "--no-log", "--max-tokens", "3000"];
        // Rust writes, Python reads.
        assert_eq!(side.rs("datg", &add, "", at).code, 0);
        let (p, r) = (
            side.py("datg", &digest, "", at + 5),
            side.rs("datg", &digest, "", at + 5),
        );
        assert!(
            p.stdout.contains("Cross probe"),
            "{name}: python reads what rust wrote"
        );
        same_run(&format!("{name} read after rust write"), &p, &r);
        // Python writes, Rust reads.
        let add2 = [
            "add",
            "T",
            "--title",
            "Cross probe two",
            "--goal",
            &goal,
            "--done-when",
            "x",
        ];
        assert_eq!(side.py("datg", &add2, "", at + 60).code, 0);
        let (p, r) = (
            side.py("datg", &digest, "", at + 65),
            side.rs("datg", &digest, "", at + 65),
        );
        assert!(
            r.stdout.contains("Cross probe two"),
            "{name}: rust reads what python wrote"
        );
        same_run(&format!("{name} read after python write"), &p, &r);
    }
}

#[test]
#[ignore = "needs python3 and the live ledgers; run with --include-ignored"]
fn mixed_python_and_rust_writers_serialise() {
    let Some(ledgers) = rig() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let (name, src) = &ledgers[1];
    let side = std::sync::Arc::new(Side::new(tmp.path(), name, src));
    let v0 = state(&side).i("v").unwrap();
    let lines0 = read(&side.dir(), "log.jsonl").lines().count();
    let mut handles = Vec::new();
    for w in 0..6 {
        let side = std::sync::Arc::clone(&side);
        handles.push(std::thread::spawn(move || {
            for n in 0..8 {
                let next = format!("mixed {w}-{n}");
                let epoch = 1_800_000_000 + i64::from(w) * 100 + n;
                let out = if w % 2 == 0 {
                    side.py("datg", &["session", "set", "--next", &next], "", epoch)
                } else {
                    side.rs("datg", &["session", "set", "--next", &next], "", epoch)
                };
                assert_eq!(out.code, 0, "writer {w}: {}", out.stderr);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        state(&side).i("v"),
        Some(v0 + 48),
        "every write landed once"
    );
    let log = read(&side.dir(), "log.jsonl");
    assert_eq!(log.lines().count(), lines0 + 48);
    assert!(
        log.lines().all(|l| Json::parse(l).is_ok()),
        "no torn log line"
    );
    assert!(
        names(&side.dir()).iter().all(|n| !n.contains(".tmp.")),
        "{:?}",
        names(&side.dir())
    );
}
