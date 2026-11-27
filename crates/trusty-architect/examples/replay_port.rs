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
//! drivers `tests/fixtures/replay/<script>/driver.py` line for line.
//! Test: `spec_drift_recordings_replay_clean_against_the_port`,
//! `level_lint_recordings_replay_clean_against_the_port`,
//! `technical_level_recordings_replay_clean_against_the_port`,
//! `architect_config_recordings_replay_clean_against_the_port`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use trusty_architect::replay::CLOCK_ENV;
use trusty_architect::spec_drift::{self, Outcome};
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
        "technical_level" => technical_level(&rest),
        "architect_config" => architect_config(&rest),
        other => {
            eprintln!(
                "replay_port: no port named {other:?}; known: spec_drift_check, level_lint, \
                 technical_level, architect_config"
            );
            return ExitCode::from(2);
        }
    };
    let _ = std::io::stdout().write_all(result.stdout.as_bytes());
    let _ = std::io::stderr().write_all(result.stderr.as_bytes());
    ExitCode::from(u8::try_from(result.code).unwrap_or(2))
}
