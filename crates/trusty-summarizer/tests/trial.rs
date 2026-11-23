//! The trial commands through the built binary: `size`, `miss`, `misses`,
//! and the `text_chars` / `tokens_est` record fields, warm and `--no-warm`.
//!
//! Every test has its own config, cache, runtime directory and miss log
//! (`TRUSTY_SUMMARIZER_MISS_LOG`). No network, no model.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::write(
            tmp.path().join("config.toml"),
            format!("[cache]\ndir = {:?}\n", cache.display().to_string()),
        )
        .unwrap();
        let mut text = String::from("Field notes. The survey covers é and ü.\n\n");
        for i in 1..=30 {
            text.push_str(&format!("Line {i} of the notes names site {i}.\n"));
        }
        std::fs::write(tmp.path().join("notes.txt"), text).unwrap();
        Env { tmp }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn log(&self) -> PathBuf {
        self.path("state/sub/misses.jsonl")
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_trusty-summarizer"));
        c.current_dir(self.tmp.path())
            .env_remove("TRUSTY_SUMMARIZER_CONFIG")
            .env_remove("TRUSTY_SUMMARIZER_CACHE_DIR")
            .env("TRUSTY_SUMMARIZER_RUNTIME_DIR", self.path("run"))
            .env("TRUSTY_SUMMARIZER_MISS_LOG", self.log())
            .arg("--config")
            .arg(self.path("config.toml"));
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = self.run(&["server", "stop"]);
    }
}

fn lines(out: &Output) -> Vec<Value> {
    String::from_utf8(out.stdout.clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is JSON"))
        .collect()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn log_entries(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("each log line is JSON"))
        .collect()
}

#[test]
fn size_command_matches_summarize_on_text() {
    let env = Env::new();
    let text = std::fs::read_to_string(env.path("notes.txt")).unwrap();
    let chars = text.chars().count() as u64;
    assert!(
        chars < text.len() as u64,
        "fixture has multi-byte characters"
    );

    let mut seen = Vec::new();
    // Fresh in-process, cached in-process, then warm (cached and fresh).
    for args in [
        &["summarize", "--no-warm", "--stats", "notes.txt"][..],
        &["summarize", "--no-warm", "--stats", "notes.txt"],
        &["summarize", "--stats", "notes.txt"],
        &["summarize", "--no-cache", "--stats", "notes.txt"],
    ] {
        let out = env.run(args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        let r = lines(&out).remove(0);
        seen.push((r["cache"].as_str().unwrap().to_string(), r));
    }
    let caches: Vec<&str> = seen.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(caches, ["miss", "hit", "hit", "disabled"]);
    for (_, r) in &seen {
        assert_eq!(r["text_chars"], chars, "{r}");
        assert_eq!(r["tokens_est"], chars.div_ceil(4), "{r}");
    }

    let out = env.run(&["size", "notes.txt"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let s = lines(&out).remove(0);
    assert_eq!(s["path"], "notes.txt");
    assert_eq!(s["format"], "text");
    assert_eq!(s["converter"], "direct");
    assert_eq!(s["text_chars"], chars);
    assert_eq!(s["tokens_est"], chars.div_ceil(4));
    assert_eq!(s["lines"], seen[0].1["lines"]);

    let out = env.run(&["size", "notes.txt", "absent.txt"]);
    assert_eq!(out.status.code(), Some(2));
    let v = lines(&out);
    assert!(
        v[1]["error"].as_str().unwrap().contains("absent.txt"),
        "{}",
        v[1]
    );
}

#[test]
fn miss_records_the_fields_and_no_document_text() {
    let env = Env::new();
    // Another working directory, a relative path to the document.
    let elsewhere = env.path("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let out = env
        .cmd()
        .current_dir(&elsewhere)
        .args([
            "miss",
            "../notes.txt",
            "--question",
            "Which site is on line 7?",
            "--note",
            "outline pointed at lines 1-3",
            "--category",
            "wrong-lines",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));

    let entries = log_entries(&env.log());
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    let doc = env.path("notes.txt").canonicalize().unwrap();
    let bytes = std::fs::read(&doc).unwrap();
    let chars = String::from_utf8(bytes.clone()).unwrap().chars().count() as u64;
    assert_eq!(e["path"], doc.display().to_string());
    assert!(e["hash"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(e["format"], "text");
    assert_eq!(e["converter"], "direct");
    assert_eq!(e["bytes"], bytes.len() as u64);
    assert_eq!(e["text_chars"], chars);
    assert_eq!(e["tokens_est"], chars.div_ceil(4));
    assert_eq!(e["question"], "Which site is on line 7?");
    assert_eq!(e["note"], "outline pointed at lines 1-3");
    assert_eq!(e["category"], "wrong-lines");
    assert_eq!(e["doc_error"], Value::Null);
    assert_eq!(e["tool_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(e["outline_describe"], false);
    assert_eq!(e["references"], true);
    assert_eq!(
        Path::new(e["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        elsewhere.canonicalize().unwrap()
    );
    let ts = e["ts"].as_str().unwrap();
    assert!(
        ts.len() == 20 && ts.ends_with('Z') && ts.as_bytes()[10] == b'T',
        "{ts}"
    );
    let raw = std::fs::read_to_string(env.log()).unwrap();
    assert!(!raw.contains("names site"), "document text leaked: {raw}");

    // Owner-only directory and file.
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&env.log()), 0o600);
    assert_eq!(mode(env.log().parent().unwrap()), 0o700);

    // `misses`: path and count, then the entries.
    let out = env.run(&["misses"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = lines(&out).remove(0);
    assert_eq!(v["log"], env.log().display().to_string());
    assert_eq!(v["count"], 1);
    assert_eq!(v["exists"], true);
    let out = env.run(&["misses", "--print"]);
    assert_eq!(lines(&out), entries);

    // A bad category is refused before anything is written.
    let out = env.run(&["miss", "notes.txt", "--question", "q", "--category", "nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(log_entries(&env.log()).len(), 1);
}

#[test]
fn miss_on_unreadable_document_is_recorded_and_exits_0() {
    let env = Env::new();
    let locked = env.path("locked.txt");
    std::fs::write(&locked, "secret words").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable_anyway = std::fs::read(&locked).is_ok(); // running as root

    for (name, reason) in [("gone.pdf", "No such file"), ("locked.txt", "ermission")] {
        if name == "locked.txt" && readable_anyway {
            continue;
        }
        let out = env.run(&["miss", name, "--question", "q"]);
        assert!(out.status.success(), "{name}: {}", stderr(&out));
        assert!(stderr(&out).contains("warning"), "{name}: {}", stderr(&out));
        let e = log_entries(&env.log()).pop().unwrap();
        assert!(e["path"].as_str().unwrap().ends_with(name), "{e}");
        assert!(e["doc_error"].as_str().unwrap().contains(reason), "{e}");
        assert_eq!(e["hash"], Value::Null);
        assert_eq!(e["category"], Value::Null);
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn unwritable_log_is_an_error() {
    let env = Env::new();
    // The log's parent is a regular file: nothing can be created under it.
    std::fs::write(env.path("blocker"), "").unwrap();
    let out = env
        .cmd()
        .env(
            "TRUSTY_SUMMARIZER_MISS_LOG",
            env.path("blocker/misses.jsonl"),
        )
        .args(["miss", "notes.txt", "--question", "q"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("cannot write the miss log"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn concurrent_appends_from_many_processes_keep_every_line() {
    const PROCS: usize = 12;
    const ROUNDS: usize = 8;
    let env = Env::new();
    // Long lines make a torn or interleaved write visible.
    let long = "x".repeat(40_000);
    std::thread::scope(|s| {
        for p in 0..PROCS {
            let (env, long) = (&env, &long);
            s.spawn(move || {
                for r in 0..ROUNDS {
                    let q = format!("p{p} r{r} {long}");
                    let out = env.run(&["miss", "notes.txt", "--question", &q]);
                    assert!(out.status.success(), "{}", stderr(&out));
                }
            });
        }
    });
    let entries = log_entries(&env.log());
    assert_eq!(entries.len(), PROCS * ROUNDS);
    let mut seen: Vec<String> = entries
        .iter()
        .map(|e| {
            let q = e["question"].as_str().unwrap();
            assert!(q.ends_with(long.as_str()));
            q.split(' ').take(2).collect::<Vec<_>>().join(" ")
        })
        .collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), PROCS * ROUNDS, "every append is present once");
}

/// `miss` with the log in `dir` (a file name `misses.jsonl`).
fn miss_into(env: &Env, log: &Path) -> Output {
    env.cmd()
        .env("TRUSTY_SUMMARIZER_MISS_LOG", log)
        .args(["miss", "notes.txt", "--question", "q"])
        .output()
        .unwrap()
}

#[test]
fn existing_log_parent_keeps_its_mode() {
    // Review finding HIGH: an existing parent (think `$HOME`) must keep its mode.
    let env = Env::new();
    let dir = env.path("home");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = miss_into(&env, &dir.join("misses.jsonl"));
    assert!(out.status.success(), "{}", stderr(&out));
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o755, "the existing parent's mode changed");
    assert_eq!(log_entries(&dir.join("misses.jsonl")).len(), 1);
    // Directories the call creates are owner-only, the nearest one included.
    let deep = dir.join("a/b/misses.jsonl");
    let out = miss_into(&env, &deep);
    assert!(out.status.success(), "{}", stderr(&out));
    for d in [dir.join("a"), dir.join("a/b")] {
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", d.display());
    }
}

#[test]
fn group_writable_log_parent_is_refused() {
    let env = Env::new();
    let dir = env.path("shared");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
    let out = miss_into(&env, &dir.join("misses.jsonl"));
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("group- or world-writable"),
        "{}",
        stderr(&out)
    );
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o775);
    assert!(!dir.join("misses.jsonl").exists());
}

#[test]
fn symlink_at_log_path_is_refused() {
    // Review finding MEDIUM: a symlink at the log path must not be followed.
    let env = Env::new();
    let dir = env.path("logs");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = env.path("target.txt");
    std::fs::write(&target, "original\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    let log = dir.join("misses.jsonl");
    std::os::unix::fs::symlink(&target, &log).unwrap();
    let out = miss_into(&env, &log);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("symbolic link"), "{}", stderr(&out));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original\n");
    let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644, "the symlink target's mode changed");
}

/// The usage log the binary writes by default: beside the miss log.
fn usage_log(env: &Env) -> PathBuf {
    env.path("state/sub/usage.jsonl")
}

/// Usage entries without `ts`, for comparing warm and in-process calls.
fn without_ts(mut v: Value) -> Value {
    v.as_object_mut().unwrap().remove("ts");
    v
}

#[test]
fn usage_lines_for_each_command_warm_and_no_warm() {
    let env = Env::new();
    let doc = env.path("notes.txt").canonicalize().unwrap();
    let text = std::fs::read_to_string(&doc).unwrap();
    let tokens = (text.chars().count() as u64).div_ceil(4);
    for args in [
        &["size", "notes.txt"][..],
        &["summarize", "--no-warm", "notes.txt"],
        &["summarize", "notes.txt"],
        &["find", "--query", "site 7", "notes.txt"],
        &["read", "--lines", "3-5", "notes.txt"],
    ] {
        let out = env.run(args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
    }
    let out = env.run(&["read", "--lines", "500-600", "notes.txt"]);
    assert_eq!(out.status.code(), Some(1));
    let out = env.run(&["read", "--lines", "1", "absent.txt"]);
    assert_eq!(out.status.code(), Some(1));

    let e = log_entries(&usage_log(&env));
    let cmds: Vec<&str> = e.iter().map(|v| v["command"].as_str().unwrap()).collect();
    assert_eq!(
        cmds,
        [
            "size",
            "summarize",
            "summarize",
            "find",
            "read",
            "read",
            "read"
        ]
    );
    for v in &e[..6] {
        assert_eq!(v["path"], doc.display().to_string(), "{v}");
        assert!(v["hash"].as_str().unwrap().starts_with("sha256:"), "{v}");
        assert_eq!(v["format"], "text", "{v}");
        assert_eq!(v["tokens_est"], tokens, "{v}");
        assert_eq!(v["tool_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(
            Path::new(v["cwd"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            env.path("").canonicalize().unwrap()
        );
    }
    // Warm and --no-warm record the same line.
    assert_eq!(without_ts(e[1].clone()), without_ts(e[2].clone()));
    assert_eq!(e[1]["outcome"], "ok");
    // B2: one phrase hit (line 9) plus a partial hit on each of the other
    // 29 `site` lines; the count is taken before the max_hits cut.
    assert_eq!(e[3]["hits"], 30);
    assert_eq!(e[3]["lines_served"], Value::Null);
    assert_eq!(e[4]["lines_served"], 3);
    // B3: the requested range and the query are logged.
    assert_eq!(e[3]["query"], "site 7");
    assert_eq!(e[4]["range"], "3-5");
    assert_eq!(e[5]["range"], "500-600");
    assert_eq!(e[1].get("range"), None);
    assert_eq!(e[4]["outcome"], "ok");
    assert_eq!(e[5]["outcome"], "invalid_range");
    assert_eq!(e[6]["outcome"], "io");
    assert_eq!(e[6]["hash"], Value::Null);
    let raw = std::fs::read_to_string(usage_log(&env)).unwrap();
    assert!(!raw.contains("names site"), "document text leaked: {raw}");
    let mode = std::fs::metadata(usage_log(&env))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn usage_write_failure_warns_and_keeps_exit_status() {
    let env = Env::new();
    std::fs::write(env.path("blocker"), "").unwrap();
    let out = env
        .cmd()
        .env(
            "TRUSTY_SUMMARIZER_USAGE_LOG",
            env.path("blocker/usage.jsonl"),
        )
        .args(["read", "--lines", "1", "notes.txt"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Field notes. The survey covers é and ü.\n"
    );
    assert!(
        stderr(&out).contains("warning: usage not recorded"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn usage_command_counts_per_day_command_and_cwd() {
    let env = Env::new();
    let elsewhere = env.path("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    env.run(&["read", "--lines", "1", "notes.txt"]);
    env.run(&["find", "--query", "survey", "notes.txt"]);
    let out = env
        .cmd()
        .current_dir(&elsewhere)
        .args(["read", "--lines", "2", "../notes.txt"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    env.run(&["miss", "notes.txt", "--question", "q"]);

    let out = env.run(&["usage"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = lines(&out).remove(0);
    assert_eq!(v["count"], 3);
    assert_eq!(v["documents"], 1);
    assert_eq!(v["errors"], 0);
    assert_eq!(v["by_command"]["read"], 2);
    assert_eq!(v["by_command"]["find"], 1);
    assert_eq!(v["by_cwd"].as_object().unwrap().len(), 2);
    let today: Vec<&String> = v["by_day"].as_object().unwrap().keys().collect();
    assert_eq!(today.len(), 1);
    assert_eq!(v["log"], usage_log(&env).display().to_string());

    let out = env.run(&["usage", "--since", "2999-01-01"]);
    assert_eq!(lines(&out).remove(0)["count"], 0);
    let out = env.run(&["usage", "--since", today[0]]);
    assert_eq!(lines(&out).remove(0)["count"], 3);
    let out = env.run(&["usage", "--since", "last week"]);
    assert_eq!(out.status.code(), Some(2));

    // `misses` puts the usage count beside the miss count.
    let v = lines(&env.run(&["misses"])).remove(0);
    assert_eq!(v["count"], 1);
    assert_eq!(v["usage_count"], 3);
    assert_eq!(v["usage_log"], usage_log(&env).display().to_string());
}

#[test]
fn concurrent_usage_appends_from_many_processes_keep_every_line() {
    const PROCS: usize = 12;
    const ROUNDS: usize = 8;
    let env = Env::new();
    std::thread::scope(|s| {
        for p in 0..PROCS {
            let env = &env;
            s.spawn(move || {
                for r in 0..ROUNDS {
                    let range = format!("{}", 1 + (p * ROUNDS + r) % 30);
                    let out = env.run(&["read", "--lines", &range, "notes.txt"]);
                    assert!(out.status.success(), "{}", stderr(&out));
                }
            });
        }
    });
    let entries = log_entries(&usage_log(&env));
    assert_eq!(entries.len(), PROCS * ROUNDS);
    assert!(entries.iter().all(|e| e["lines_served"] == 1));
}
