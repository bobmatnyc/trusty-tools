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
//! `stalled_tasks`, `task_wait_check` and `il_timeline` (#12028 P1d),
//! `brief-id` (#12028 P1e), `il_agent` (#12028 P1f) and `verify_claim`
//! (#12028 P1g) and `ruling_pack` (#12028 P1i), `ids36` and
//! `dashboard_schema` (#12029 P2a), `dashboard_jsonl` and `dashboard_derive`
//! (#12029 P2b, mirrored in the library), `dashboard_store` (#12029 P2c,
//! mirrored in the library). `gh-session-track` (#12087)
//! runs the script's own command line.
//! Test: `spec_drift_recordings_replay_clean_against_the_port`,
//! `level_lint_recordings_replay_clean_against_the_port`,
//! `technical_level_recordings_replay_clean_against_the_port`,
//! `architect_config_recordings_replay_clean_against_the_port`,
//! `stale_tasks_recordings_replay_clean_against_the_port`, the three
//! other `tests/replay_tasks.rs` tests, the `tests/replay_writers.rs` tests and
//! `il_agent_recordings_replay_clean_against_the_port`,
//! `verify_claim_recordings_replay_clean_against_the_port`,
//! `gh_session_track_recordings_replay_clean_against_the_port`,
//! `ruling_pack_recordings_replay_clean_against_the_port`,
//! `ids36_recordings_replay_clean_against_the_port`,
//! `dashboard_schema_recordings_replay_clean_against_the_port`,
//! `dashboard_jsonl_recordings_replay_clean_against_the_port`,
//! `dashboard_derive_recordings_replay_clean_against_the_port` and
//! `dashboard_store_recordings_replay_clean_against_the_port`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use trusty_architect::dashboard::{pyderive, pyjsonl};
use trusty_architect::gh_session_track;
use trusty_architect::ids36;
use trusty_architect::il_agent;
use trusty_architect::replay::CLOCK_ENV;
use trusty_architect::ruling_pack;
use trusty_architect::spec_drift::{self, Outcome};
use trusty_architect::tasks::{stale, stalled, timeline::Timeline, wait_check};
use trusty_architect::verify_claim;
use trusty_architect::writers::{aliases, brief_ids, state_archive};
use trusty_architect::{Clock, FakeClock, ScriptedTmux, SystemClock};
use trusty_architect::{architect_config as ac, level_lint, technical_level as tl};

fn out(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// Python's `repr()` of a `str` whose characters are printable or C0
/// controls (the recorded values).
fn repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let body: String = s
        .chars()
        .map(|c| match c {
            '\\' => "\\\\".to_owned(),
            // See #12029: an ids36 recording passes `0f\n`.
            '\n' => "\\n".to_owned(),
            '\r' => "\\r".to_owned(),
            '\t' => "\\t".to_owned(),
            c if c == q => format!("\\{c}"),
            c if c < ' ' || c == '\u{7f}' => format!("\\x{:02x}", u32::from(c)),
            c => c.to_string(),
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

/// `driver.py` of `tests/fixtures/replay/ids36/` (#12029 P2a).
fn ids36_driver(args: &[String]) -> Outcome {
    let usage = "usage: driver.py cli ARGS... | table | enc N... | dec S... | next S...\n";
    let Some((cmd, rest)) = args.split_first() else {
        return out(String::new(), usage.into(), 2);
    };
    let mut o = String::new();
    match cmd.as_str() {
        "cli" => return ids36::run(rest),
        "table" if rest.is_empty() => {
            o += &format!(
                "DIGITS {}\nBASE {}\nSPACE {}\nDIGIT_RE {}\n",
                ids36::DIGITS,
                ids36::BASE,
                ids36::SPACE,
                ids36::DIGIT_RE
            );
        }
        "enc" | "dec" | "next" if !rest.is_empty() => {
            for a in rest {
                let text = |e: ids36::Ids36Error| e.to_string();
                let r = match cmd.as_str() {
                    // `int(a)` of a plain decimal; the recordings pass no other.
                    "enc" => a
                        .parse::<i64>()
                        .map_err(|_| format!("invalid literal for int() with base 10: {}", repr(a)))
                        .and_then(|n| ids36::enc(n).map_err(text))
                        .map(|d| repr(&d)),
                    "dec" => ids36::dec(a).map(|n| n.to_string()).map_err(text),
                    _ => ids36::next_digits(a).map(|d| repr(&d)).map_err(text),
                };
                o += &match r {
                    Ok(v) => format!("{cmd} {} -> {v}\n", repr(a)),
                    Err(e) => format!("{cmd} {} ValueError: {e}\n", repr(a)),
                };
            }
        }
        _ => return out(String::new(), usage.into(), 2),
    }
    out(o, String::new(), 0)
}

/// `driver.py` of `tests/fixtures/replay/dashboard_schema/` (#12029 P2a).
fn dashboard_schema(args: &[String]) -> Outcome {
    use serde_json::Value;
    use trusty_architect::dashboard::schema_check::{SchemaMode, schema_name, script};
    use trusty_architect::dashboard::{Doc, Snapshot};
    let [cases] = args else {
        return out(String::new(), "usage: driver.py CASES.json\n".into(), 2);
    };
    let parsed = std::fs::read_to_string(cases)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok());
    let Some(cases) = parsed else {
        return out(String::new(), "driver: cases unreadable\n".into(), 1);
    };
    let ok = |errs: Vec<String>| {
        if errs.is_empty() {
            vec!["ok".into()]
        } else {
            errs
        }
    };
    let mut o = String::new();
    for (i, case) in cases.iter().enumerate() {
        let s = |k: &str| case.get(k).and_then(Value::as_str).unwrap_or_default();
        let dir = Path::new(case.get("dir").and_then(Value::as_str).unwrap_or("schemas"));
        o += &format!("case {i} {}\n", s("op"));
        let lines = match s("op") {
            "name" => Ok(vec![
                schema_name(s("coll"), s("id")).unwrap_or_else(|| "None".into()),
            ]),
            "validate" => script::validate(dir, s("coll"), s("id"), &case["data"]).map(ok),
            "store" => {
                let mut snap = Snapshot::new();
                for (coll, docs) in case["docs"].as_object().into_iter().flatten() {
                    let list = docs.as_array().into_iter().flatten().map(|d| Doc {
                        id: d["id"].as_str().unwrap_or_default().to_owned(),
                        version: 1,
                        data: d["data"].as_object().cloned().unwrap_or_default(),
                    });
                    snap.insert(coll.clone(), list.collect());
                }
                script::validate_store(dir, &snap).map(ok)
            }
            "mode" => {
                let env = |k: &str| {
                    case["env"]
                        .get(k)
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                };
                script::mode(&env).map(|m| {
                    let m = match m {
                        None => "None",
                        Some(SchemaMode::Off) => "off",
                        Some(SchemaMode::Warn) => "warn",
                        Some(SchemaMode::Enforce) => "enforce",
                    };
                    vec![m.to_owned()]
                })
            }
            "missing" => {
                let miss: Vec<String> = script::architect_do_missing(&case["data"])
                    .iter()
                    .map(|m| repr(m))
                    .collect();
                Ok(vec![format!("[{}]", miss.join(", "))])
            }
            other => return out(o, format!("KeyError: {}\n", repr(other)), 1),
        };
        for line in lines.unwrap_or_else(|e| vec![format!("SchemaError: {e}")]) {
            o += &format!("  {line}\n");
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
        // See #12028 P1f: `driver.py` sets the script's HERE to the working
        // directory, so `dispatch` names `<cwd>/il_agent.py`; the sandbox root
        // stands for the checkout. The recorder clears `TMUX_PANE`, so the
        // tmux fake is never asked.
        "il_agent" => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
            let env = |k: &str| std::env::var(k).ok();
            let tmux = ScriptedTmux::new();
            let brief = cwd.join("il_agent.py").to_string_lossy().into_owned();
            il_agent::cli::run(
                &rest,
                &mut il_agent::cli::Options {
                    checkout: Some(cwd),
                    ilio_project: env("ILIO_PROJECT"),
                    ilio_dir: env("ILIO_DIR"),
                    brief_cmd: vec!["python3".into(), brief],
                    env: &env,
                    tmux: &tmux,
                    stdin: &mut std::io::stdin(),
                },
            )
        }
        // See #12028 P1g: `driver.py` points `architect_config.load()` at the
        // working directory, so the sandbox root stands for the checkout.
        "verify_claim" => {
            let environ: Vec<(String, String)> = std::env::vars().collect();
            verify_claim::cli::run(
                &rest,
                &mut verify_claim::cli::Options {
                    checkout: std::env::current_dir().ok(),
                    environ,
                    stdin: &mut std::io::stdin(),
                },
            )
        }
        // See #12087: the recordings run the script itself with `--root`;
        // the sandbox root stands for the checkout (its `local/architect.json`).
        "gh-session-track" => {
            let at = frozen.map_or_else(|| SystemClock.now_unix(), |t| t as f64);
            gh_session_track::cli::run(
                &rest,
                &gh_session_track::cli::Options {
                    checkout: std::env::current_dir().ok(),
                    environ: std::env::vars().collect(),
                    clock: Arc::new(FakeClock::new(at)),
                },
            )
        }
        // See #12028 P1i: `driver.py` sets the script's ROOT to the working
        // directory, so the sandbox root stands for the checkout.
        "ruling_pack" => ruling_pack::cli::run(
            &rest,
            &ruling_pack::cli::Options {
                checkout: std::env::current_dir().ok(),
                clock: clock.as_ref(),
            },
        ),
        // See #12029 P2a: both are their `driver.py`; the schema cases name
        // directories relative to the sandbox root.
        "ids36" => ids36_driver(&rest),
        "dashboard_schema" => dashboard_schema(&rest),
        // See #12029 P2b: the library holds each `driver.py` mirror.
        "dashboard_jsonl" => pyjsonl::replay_cases(&rest),
        "dashboard_derive" => pyderive::replay_cases(&rest, clock.as_ref()),
        // See #12029 P2c: the store's `driver.py` mirror, on the frozen clock.
        "dashboard_store" => {
            let at = frozen.map_or_else(|| SystemClock.now_unix(), |t| t as f64);
            trusty_architect::dashboard::store::py::replay_cases(
                &rest,
                Arc::new(FakeClock::new(at)),
            )
        }
        "technical_level" => technical_level(&rest),
        "architect_config" => architect_config(&rest),
        other => {
            eprintln!(
                "replay_port: no port named {other:?}; known: spec_drift_check, level_lint, \
                 technical_level, architect_config, stale-tasks, stalled_tasks, task_wait_check, \
                 il_timeline, state-archive, aliases, brief-id, il_agent, verify_claim, \
                 gh-session-track, ruling_pack, ids36, dashboard_schema, dashboard_jsonl, \
                 dashboard_derive, dashboard_store"
            );
            return ExitCode::from(2);
        }
    };
    let _ = std::io::stdout().write_all(result.stdout.as_bytes());
    let _ = std::io::stderr().write_all(result.stderr.as_bytes());
    ExitCode::from(u8::try_from(result.code).unwrap_or(2))
}
