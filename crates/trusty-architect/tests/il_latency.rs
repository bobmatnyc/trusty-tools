//! Latency of `digest` and `apply`, Rust against Python, on copies of both live ledgers.
//!
//! Why: Bob asked for the digest and apply latency on the Architect's ledger and on
//! tm-dogfood's, against the Python. This prints one table; it is ignored because it
//! needs python3 and the live ledgers and its numbers depend on the machine.
//! What: for each ledger copy, wall time of whole processes (start included): digest
//! (`--no-log`), `ilio apply` of a block that sets `next`, moves `pct` and records an
//! `act`, and the ledger part of that apply as `datg batch` (the Python `apply`
//! also runs the steering and dashboard side effects that stay in Python). Each apply
//! runs on a fresh copy; the copy (and a `sync` of it) is outside the timing. An in-process Rust digest loop
//! separates the work from the process start.
//! Run: `cargo build --release -p trusty-architect --bin trusty-architect-il`, then
//! `TRUSTY_IL_BIN=<target>/release/trusty-architect-il cargo test -p trusty-architect
//! --test il_latency --release -- --include-ignored --nocapture`.
//! Test: `latency_report`.

mod il_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use il_support::*;
use trusty_architect::il::{Env, Json};

const RUNS_DIGEST: usize = 40;
const RUNS_APPLY: usize = 25;

fn scripts() -> PathBuf {
    PathBuf::from(
        std::env::var("ARCHITECT_SCRIPTS")
            .unwrap_or_else(|_| "/Users/masa/trusty-mpm-projects/architect/scripts".into()),
    )
}

fn bin() -> String {
    std::env::var("TRUSTY_IL_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_trusty-architect-il").to_owned())
}

fn timed(mut cmd: Command, stdin: &str) -> Duration {
    use std::io::Write;
    let start = Instant::now();
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let st = child.wait().unwrap();
    let d = start.elapsed();
    assert!(st.success(), "a timed command failed");
    d
}

fn stats(mut v: Vec<Duration>) -> String {
    v.sort();
    let ms = |d: &Duration| d.as_secs_f64() * 1000.0;
    format!(
        "min {:6.1}  median {:6.1}  p95 {:6.1}",
        ms(&v[0]),
        ms(&v[v.len() / 2]),
        ms(&v[(v.len() * 95 / 100).min(v.len() - 1)])
    )
}

fn median(mut v: Vec<Duration>) -> f64 {
    v.sort();
    v[v.len() / 2].as_secs_f64() * 1000.0
}

struct Copy {
    _tmp: tempfile::TempDir,
    project: PathBuf,
    ilio: PathBuf,
    work: PathBuf,
}

fn copy(src: &Path) -> Copy {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().to_path_buf();
    let project = project_with(&work, src);
    let ilio = work.join("ilio");
    fs::create_dir_all(&ilio).unwrap();
    fs::create_dir_all(work.join("dash")).unwrap();
    // Flush the copy's dirty pages now, so the timed command's fsyncs do not wait for them.
    let _ = Command::new("sync").status();
    Copy {
        _tmp: tmp,
        project,
        ilio,
        work,
    }
}

impl Copy {
    fn envs(&self, cmd: &mut Command) {
        cmd.env("ILIO_PROJECT", &self.project)
            .env("ILIO_DIR", &self.ilio)
            .env("ILIO_WHO", "tm-architect")
            .env("DASHBOARD_ROOT", self.work.join("dash"))
            .env("ARCHITECT_INBOX_DIR", self.work.join("inbox"))
            .env_remove("DATG_SESSION");
    }

    fn py(&self, script: &str, args: &[&str]) -> Command {
        let mut c = Command::new("python3");
        c.arg("-I").arg(scripts().join(script));
        if script == "datg.py" {
            c.arg("--project").arg(&self.project);
        }
        c.args(args);
        self.envs(&mut c);
        c
    }

    fn rs(&self, prog: &str, args: &[&str]) -> Command {
        let mut c = Command::new(bin());
        c.arg(prog);
        if prog == "datg" {
            c.arg("--project").arg(&self.project);
        }
        c.args(args);
        self.envs(&mut c);
        c
    }
}

fn first_live_task(src: &Path) -> (String, Option<i64>) {
    let st = Json::parse(&fs::read_to_string(src.join("state.json")).unwrap()).unwrap();
    let t = st
        .arr("tasks")
        .iter()
        .find(|t| matches!(t.s("status"), "open" | "running"))
        .unwrap();
    (t.s("id").to_owned(), t.i("pct"))
}

#[test]
#[ignore = "needs python3, the live ledgers and a release binary; run with --include-ignored --nocapture"]
fn latency_report() {
    let ledgers = [
        (
            "Architect",
            std::env::var("ARCHITECT_IL_LIVE").unwrap_or_else(|_| {
                "/Users/masa/trusty-mpm-projects/architect/local/il/.trusty-mpm/datg".into()
            }),
        ),
        (
            "tm-dogfood",
            std::env::var("DOGFOOD_DATG").unwrap_or_else(|_| {
                "/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools/.trusty-mpm/datg".into()
            }),
        ),
    ];
    println!("binary: {}", bin());
    let mut table: Vec<(String, f64, f64)> = Vec::new();
    for (name, dir) in ledgers {
        let src = PathBuf::from(dir);
        let sizes: Vec<String> = ["state.json", "log.jsonl", "archive.jsonl"]
            .iter()
            .map(|f| format!("{f} {} B", fs::metadata(src.join(f)).map_or(0, |m| m.len())))
            .collect();
        println!("\n== {name}: {}", sizes.join(", "));
        let c = copy(&src);
        // Warm the page cache and the interpreter for both sides.
        for _ in 0..3 {
            timed(c.py("datg.py", &["digest", "--no-log"]), "");
            timed(c.rs("datg", &["digest", "--no-log"]), "");
        }
        let py: Vec<Duration> = (0..RUNS_DIGEST)
            .map(|_| timed(c.py("datg.py", &["digest", "--no-log"]), ""))
            .collect();
        let rs: Vec<Duration> = (0..RUNS_DIGEST)
            .map(|_| timed(c.rs("datg", &["digest", "--no-log"]), ""))
            .collect();
        println!("digest            python {}", stats(py.clone()));
        println!("digest            rust   {}", stats(rs.clone()));
        table.push((format!("{name} digest"), median(py), median(rs)));
        // In process: the work without the process start.
        let env = Env::bare(&c.work);
        let p = c.project.to_str().unwrap();
        let loop_start = Instant::now();
        for _ in 0..200 {
            let out = run_at(
                &["datg", "--project", p, "digest", "--no-log"],
                "",
                &env,
                1_800_000_000,
            );
            assert_eq!(out.code, 0);
        }
        println!(
            "digest in process rust   {:6.2} ms per call (200 calls)",
            loop_start.elapsed().as_secs_f64() * 1000.0 / 200.0
        );

        let (tid, pct) = first_live_task(&src);
        let new_pct = (pct.unwrap_or(0) + 7) % 99 + 1;
        let block = format!(
            "```il\ntask: {tid}\ndone: latency probe\npct: {new_pct}\nremaining: 1h\nnext: latency probe next\nact: probed the ledger | owner: bob | ev: PR #1 | refs: PR #1\n```\n"
        );
        let ops = format!(
            "[{{\"op\":\"session\",\"next\":\"latency probe next\"}},{{\"op\":\"pulse\",\"id\":\"{tid}\",\"pct\":\"{new_pct}\",\"progress_note\":\"latency probe\",\"remaining\":\"1h\",\"progress_at\":\"2026-11-21T17:00:00Z\"}},{{\"op\":\"act\",\"who\":\"tm-architect\",\"what\":\"probed the ledger\",\"owner_words\":\"bob\",\"evidence\":\"PR #1\",\"goal\":\"\",\"refs\":\"PR #1\"}}]"
        );
        let bench = |label: &str, run: &dyn Fn(&Copy) -> Duration| -> (Vec<Duration>, f64) {
            let times: Vec<Duration> = (0..RUNS_APPLY + 2)
                .map(|_| run(&copy(&src)))
                .skip(2)
                .collect();
            println!("{label:<17} {}", stats(times.clone()));
            let m = median(times.clone());
            (times, m)
        };
        let (_, a_py) = bench("apply     python", &|c| {
            timed(c.py("ilio.py", &["apply"]), &block)
        });
        let (_, a_rs) = bench("apply     rust  ", &|c| {
            timed(c.rs("ilio", &["apply"]), &block)
        });
        let (_, b_py) = bench("batch     python", &|c| {
            timed(c.py("datg.py", &["batch"]), &ops)
        });
        let (_, b_rs) = bench("batch     rust  ", &|c| {
            timed(c.rs("datg", &["batch"]), &ops)
        });
        table.push((format!("{name} apply (ilio, full)"), a_py, a_rs));
        table.push((
            format!("{name} apply (ledger part, datg batch)"),
            b_py,
            b_rs,
        ));
    }
    println!(
        "\n{:<44} {:>12} {:>12} {:>8}",
        "median wall ms, process start included", "python", "rust", "speedup"
    );
    for (label, py, rs) in &table {
        println!("{label:<44} {py:>12.1} {rs:>12.1} {:>7.1}x", py / rs);
    }
}
