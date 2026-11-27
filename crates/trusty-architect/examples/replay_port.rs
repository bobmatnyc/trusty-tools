//! Replay entry point for the Architect script ports (#12028); never ships.
//!
//! Why: a recording made with `record --clock` hands its instant to the
//! target in `TRUSTY_REPLAY_CLOCK`, and D6a keeps that variable out of every
//! shipped binary. One dispatching example serves every port instead of one
//! stand-in per script.
//! What: `replay_port <script> [ARGS...]` runs the named port's library
//! function on a `FakeClock` at the recorded instant (the real clock when the
//! variable is absent) and writes its streams. The recordings always pass
//! `--root`, so the default root is the current directory. `technical_level`
//! and `architect_config` are libraries: their entries mirror the recording
//! drivers `tests/fixtures/replay/<script>/driver.py` line for line, as do
//! `stalled_tasks`, `task_wait_check` and `il_timeline` (#12028 P1d), and
//! `brief-id` (#12028 P1e).
//! Test: `spec_drift_recordings_replay_clean_against_the_port`,
//! `level_lint_recordings_replay_clean_against_the_port`,
//! `technical_level_recordings_replay_clean_against_the_port`,
//! `architect_config_recordings_replay_clean_against_the_port`,
//! `stale_tasks_recordings_replay_clean_against_the_port`, the three
//! other `tests/replay_tasks.rs` tests and the `tests/replay_writers.rs` tests.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use trusty_architect::replay::CLOCK_ENV;
use trusty_architect::spec_drift::{self, Outcome};
use trusty_architect::tasks::{stale, stalled, timeline::Timeline, wait_check};
use trusty_architect::writers::{aliases, brief_ids, state_archive};
use trusty_architect::{Clock, FakeClock, SystemClock};
use trusty_architect::{architect_config as ac, level_lint, technical_level as tl};

fn out(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// Python's `repr()` of an ASCII-printable `str` (the recorded values).
fn repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let body: String = s
        .chars()
        .flat_map(|c| match c {
            '\\' => vec!['\\', '\\'],
            c if c == q => vec!['\\', c],
            c => vec![c],
        })
        .collect();
    format!("{q}{body}{q}")
}

fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// `driver.py` of `tests/fixtures/replay/technical_level/`.
fn technical_level(args: &[String]) -> Outcome {
    let mut o = String::new();
    match args.split_first() {
        Some((cmd, [])) if cmd == "table" => {
            o += &format!("LEVELS {}\nDEFAULT {}\n", tl::LEVELS.join(" "), tl::DEFAULT);
            for level in tl::LEVELS {
                o += &format!("{}\n", tl::line(level));
            }
        }
        Some((cmd, rest)) if cmd == "value" && !rest.is_empty() => {
            for v in rest {
                o += &format!(
                    "{} normalize={} below_engineer={}\n{}\n",
                    repr(v),
                    tl::normalize(v),
                    py_bool(tl::below_engineer(v)),
                    tl::line(v)
                );
            }
        }
        Some((cmd, [root])) if cmd == "read" => {
            let format = std::env::var("DASHBOARD_FORMAT").ok();
            match tl::store_format(format.as_deref()).and_then(|f| tl::read(Path::new(root), f)) {
                Ok(level) => o += &format!("read {level}\n{}\n", tl::line(level)),
                Err(_) => return out(o, "read raised StoreError\n".into(), 1),
            }
        }
        _ => {
            let usage = "usage: driver.py table | value V... | read ROOT\n";
            return out(o, usage.into(), 2);
        }
    }
    out(o, String::new(), 0)
}

/// `driver.py` of `tests/fixtures/replay/architect_config/`.
fn architect_config(args: &[String]) -> Outcome {
    let [root] = args else {
        return out(String::new(), "usage: driver.py ROOT\n".into(), 2);
    };
    let env = |k: &str| std::env::var(k).ok();
    let root = Path::new(root);
    let mut o = format!("default_path: {}\n", ac::default_path(root, &env));
    match ac::default_claude_config_dir(&env) {
        Ok(d) => o += &format!("default_claude_config_dir: {d}\n"),
        Err(e) => return out(o, format!("{e}\n"), 1),
    }
    let c = match ac::load(root, &env) {
        Ok(c) => c,
        Err(e) => return out(o, format!("ConfigError: {e}\n"), 1),
    };
    o += &format!(
        "path: {}\nowner_name: {}\nmemory_palace: {}\ndashboard_port: {}\ntailnet_url: {}\n",
        c.path, c.owner_name, c.memory_palace, c.dashboard_port, c.tailnet_url
    );
    for (name, r) in &c.repos {
        o += &format!(
            "repo {name}: slug={} gh_config_dir={} checkout={}\n",
            r.slug, r.gh_config_dir, r.checkout
        );
    }
    for (name, v) in &c.caps {
        o += &format!("cap {name}: {}\n", v.repr());
    }
    o += &format!(
        "unknown: {}\nprojects_root: {}\nclaude_config_dir: {}\npoll_interval_s: {}\n",
        c.unknown.join(" "),
        c.projects_root,
        c.claude_config_dir,
        c.poll_interval_s
    );
    out(o, String::new(), 0)
}

/// Python's `repr()` of a float (the recorded epochs and keys).
fn frepr(x: f64) -> String {
    let s = format!("{x:?}");
    if s.contains('e') || s.contains('.') || s.contains("inf") || s.contains("NaN") {
        return s;
    }
    format!("{s}.0")
}

/// `driver.py` of `tests/fixtures/replay/il_timeline/` (#12028 P1d).
fn il_timeline(args: &[String]) -> Outcome {
    let cut = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let pairs: Vec<Option<(&str, &str)>> = args[..cut].iter().map(|a| a.split_once('=')).collect();
    if pairs.is_empty() || pairs.iter().any(Option::is_none) {
        let usage = "usage: driver.py PROJECT=DIR... [-- EPOCH...]\n";
        return out(String::new(), usage.into(), 2);
    }
    let dirs: std::collections::BTreeMap<String, PathBuf> = pairs
        .into_iter()
        .flatten()
        .map(|(p, d)| (p.to_owned(), PathBuf::from(d)))
        .collect();
    let epochs: Vec<f64> = args
        .get(cut + 1..)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| a.parse().ok())
        .collect();
    let tl = match Timeline::load(&dirs) {
        Ok(tl) => tl,
        Err(class) => return out(String::new(), format!("load raised {class}\n"), 1),
    };
    let key = |k: Option<(&str, f64)>| {
        k.map_or_else(|| "None".to_owned(), |(i, t)| format!("{i} {}", frepr(t)))
    };
    let mut o = String::new();
    for inc in tl.incarnations() {
        let close = inc.close.map_or_else(|| "None".to_owned(), frepr);
        o += &format!(
            "inc {} close={close} title={}\n",
            key(Some((&inc.id, inc.open))),
            inc.title_repr()
        );
    }
    for (id, t) in tl.newest() {
        o += &format!("newest {id} {}\n", key(Some((id, t))));
    }
    for p in dirs.keys().map(String::as_str).chain(["-"]) {
        for e in &epochs {
            o += &format!("at {p} {} {}\n", frepr(*e), key(tl.at(p, *e)));
        }
    }
    out(o, String::new(), 0)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let script = args.next().unwrap_or_default();
    let rest: Vec<String> = args.collect();
    let frozen = std::env::var(CLOCK_ENV)
        .ok()
        .and_then(|v| v.parse::<i64>().ok());
    let clock: Box<dyn Clock> = match frozen {
        Some(t) => Box::new(FakeClock::new(t as f64)),
        None => Box::new(SystemClock),
    };
    let result = match script.as_str() {
        "spec_drift_check" => {
            let opts = spec_drift::Options {
                prog: "spec_drift_check.py".into(),
                default_root: PathBuf::from("."),
                dashboard_format: std::env::var("DASHBOARD_FORMAT").ok(),
            };
            spec_drift::run(&rest, &opts, clock.as_ref())
        }
        "level_lint" => {
            // See #12028: the script's default store is `<checkout>/local/owner-queue`;
            // a recording runs in the sandbox root, which stands for the checkout.
            let opts = level_lint::Options {
                prog: "level_lint.py".into(),
                store_root: PathBuf::from("local/owner-queue"),
                dashboard_format: std::env::var("DASHBOARD_FORMAT").ok(),
            };
            level_lint::run(&rest, &mut std::io::stdin(), &opts)
        }
        // See #12028 P1d: the recordings run in the sandbox root, which
        // stands for the Architect checkout.
        "stale-tasks" => {
            let opts = stale::Options {
                prog: "stale-tasks.py".into(),
                default_root: PathBuf::from("local/owner-queue"),
                env_root: std::env::var("DASHBOARD_ROOT").ok(),
                dashboard_format: std::env::var("DASHBOARD_FORMAT").ok(),
            };
            stale::run(&rest, &opts, clock.as_ref())
        }
        "stalled_tasks" => {
            let env = |k: &str| std::env::var(k).ok();
            let (il, items) = (
                Path::new("local/owner-queue/../il"),
                Path::new("local/owner-queue/items"),
            );
            match rest.as_slice() {
                [] => stalled::run(il, items, None, &env, clock.as_ref()),
                [h] if h == "hook" => stalled::run(il, items, Some(2), &env, clock.as_ref()),
                _ => out(String::new(), "usage: driver.py [hook]\n".into(), 2),
            }
        }
        "task_wait_check" => {
            let env = |k: &str| std::env::var(k).ok();
            wait_check::run(
                Path::new("local/il"),
                Path::new("local/owner-queue/items"),
                &env,
            )
        }
        "il_timeline" => il_timeline(&rest),
        // See #12028 P1e: the sandbox root stands for the checkout here too;
        // `brief-id` is its `driver.py`, which sets `REPO` to the working directory.
        "state-archive" => state_archive::run(
            &rest,
            &state_archive::Options {
                prog: "state-archive.py".into(),
                default_state: Some(PathBuf::from("local/state.md")),
                default_archive: Some(PathBuf::from("local/state-archive.md")),
            },
        ),
        "aliases" => aliases::run(
            &rest,
            &mut aliases::Options {
                default_path: PathBuf::from("local/aliases.json"),
                aliases_file: std::env::var("ALIASES_FILE").ok(),
                stdin: &mut std::io::stdin(),
            },
        ),
        "brief-id" => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let paths = brief_ids::Paths { repo: cwd.clone() };
            brief_ids::cli::run(&rest, &paths, &cwd, clock.as_ref())
        }
        "technical_level" => technical_level(&rest),
        "architect_config" => architect_config(&rest),
        other => {
            eprintln!(
                "replay_port: no port named {other:?}; known: spec_drift_check, level_lint, \
                 technical_level, architect_config, stale-tasks, stalled_tasks, task_wait_check, \
                 il_timeline, state-archive, aliases, brief-id"
            );
            return ExitCode::from(2);
        }
    };
    let _ = std::io::stdout().write_all(result.stdout.as_bytes());
    let _ = std::io::stderr().write_all(result.stderr.as_bytes());
    ExitCode::from(u8::try_from(result.code).unwrap_or(2))
}
