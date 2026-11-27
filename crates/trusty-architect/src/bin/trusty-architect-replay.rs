//! `trusty-architect-replay`: record a script run, replay it, check fixtures (#12026).
//!
//! Why: the slice gates of epic #12025 record each Python script once and
//! replay the recording against the Rust port; the command line is how a
//! slice PR makes and checks its fixtures.
//! What: `record` writes one scrubbed, checked recording; `replay` runs each
//! recording against a program and prints one line per differing channel;
//! `check` runs the refusal check on fixtures. Exit 0 clean, 1 a mismatch or a
//! refusal, 2 a usage or run error. No value is printed from the environment.
//! `record --fakes FILE` declares fake executables (#12028): FILE is a JSON
//! object of name to answers, each `{"argv": [...], "stdout": "...",
//! "stderr": "...", "exit": N}` (`argv` omitted matches any call). The stubs
//! go first on `PATH` on record and on replay; replay diffs their calls.
//! `record --clock SECS` freezes the run's wall clock at that epoch second
//! and records it; replay hands it to the target (#12028).
//! The crate's dependency list (#9171 A1) has no `anyhow`, so errors are
//! the library's `ReplayError`, printed once.
//! Test: `cli_records_replays_and_checks_a_fixture`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use trusty_architect::replay::{
    EnvRule, FakeAnswer, RecordSpec, ReplayError, Target, check, load, record, replay, save,
};

const USAGE: &str = "usage:
  trusty-architect-replay record --out FILE [--seed SRC=REL]... [--env NAME[=VALUE]]...
      [--stdin-file F] [--session NAME]... [--timeout SECS] [--fakes FILE]...
      [--clock SECS] -- PROGRAM [ARGS...]
      --fakes FILE: JSON {NAME: [{\"argv\": [ARG...], \"stdout\": S, \"stderr\": S, \"exit\": N}]};
      stubs first on PATH on record and replay; first matching answer wins,
      no argv matches any call; replay diffs the calls
      --clock SECS: freeze the wall clock at epoch second SECS (TZ=UTC unless
      --env TZ=...); replay runs the target at the same instant
  trusty-architect-replay replay [--timeout SECS] FILE... -- PROGRAM [PREFIX_ARGS...]
  trusty-architect-replay check [--session NAME]... FILE...";

#[derive(Default)]
struct Opts {
    out: Option<PathBuf>,
    seeds: Vec<(PathBuf, String)>,
    env: Vec<EnvRule>,
    stdin_file: Option<PathBuf>,
    sessions: Vec<String>,
    fakes: BTreeMap<String, Vec<FakeAnswer>>,
    clock: Option<i64>,
    timeout: Duration,
    files: Vec<PathBuf>,
    command: Vec<String>,
}

fn bad(msg: impl Into<String>) -> ReplayError {
    ReplayError::BadArg(msg.into())
}

fn parse(args: &[String]) -> Result<Opts, ReplayError> {
    let mut o = Opts {
        timeout: Duration::from_secs(60),
        ..Opts::default()
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| bad(format!("{a} needs a value")))
        };
        match a.as_str() {
            "--" => {
                o.command = it.cloned().collect();
                break;
            }
            "--out" => o.out = Some(value()?.into()),
            "--stdin-file" => o.stdin_file = Some(value()?.into()),
            "--session" => o.sessions.push(value()?),
            "--fakes" => {
                // See #12028: one name in two files is an author error.
                for (name, answers) in fakes_file(Path::new(&value()?))? {
                    if o.fakes.insert(name.clone(), answers).is_some() {
                        return Err(bad(format!("fake {name} is declared twice")));
                    }
                }
            }
            "--clock" => {
                // See #12028
                let secs = value()?
                    .parse()
                    .map_err(|_| bad("--clock takes epoch seconds"))?;
                o.clock = Some(secs);
            }
            "--timeout" => {
                let secs = value()?
                    .parse()
                    .map_err(|_| bad("--timeout takes seconds"))?;
                o.timeout = Duration::from_secs(secs);
            }
            "--seed" => {
                let v = value()?;
                let (src, rel) = v
                    .split_once('=')
                    .ok_or_else(|| bad("--seed takes SRC=REL"))?;
                o.seeds.push((src.into(), rel.to_owned()));
            }
            "--env" => {
                let v = value()?;
                o.env.push(match v.split_once('=') {
                    Some((k, val)) => EnvRule::Set(k.to_owned(), val.to_owned()),
                    None => EnvRule::Pass(v),
                });
            }
            flag if flag.starts_with("--") => return Err(bad(format!("unknown flag {flag}"))),
            file => o.files.push(file.into()),
        }
    }
    Ok(o)
}

fn run(cmd: &str, args: &[String]) -> Result<bool, ReplayError> {
    let parent: BTreeMap<String, String> = std::env::vars().collect();
    let o = parse(args)?;
    match cmd {
        "record" => {
            let out = o
                .out
                .clone()
                .ok_or_else(|| bad("record needs --out FILE"))?;
            let (program, argv) = o
                .command
                .split_first()
                .ok_or_else(|| bad("record needs -- PROGRAM"))?;
            let stdin = match &o.stdin_file {
                Some(p) => read(p)?,
                None => String::new(),
            };
            let spec = RecordSpec {
                program: program.into(),
                argv: argv.to_vec(),
                stdin,
                env: o.env,
                seeds: o.seeds,
                sessions: o.sessions.clone(),
                timeout: o.timeout,
                fakes: o.fakes,
                clock: o.clock,
            };
            let rec = record(&spec, &parent)?;
            save(&out, &rec, &o.sessions)?;
            println!("recorded {} (exit {})", out.display(), rec.exit);
            Ok(true)
        }
        "replay" => {
            let (program, prefix) = o
                .command
                .split_first()
                .ok_or_else(|| bad("replay needs -- PROGRAM"))?;
            let target = Target {
                program: program.into(),
                prefix: prefix.to_vec(),
            };
            let mut clean = true;
            for f in &o.files {
                let found = replay(&load(f)?, &target, &parent, o.timeout)?;
                println!(
                    "{} {}",
                    if found.is_empty() { "ok  " } else { "DIFF" },
                    f.display()
                );
                for m in &found {
                    println!("  {m}");
                }
                clean &= found.is_empty();
            }
            Ok(clean)
        }
        "check" => {
            let mut clean = true;
            for f in &o.files {
                // `load` checks without session names; this run adds them.
                match load(f).and_then(|rec| Ok(check(&rec, &o.sessions)?)) {
                    Ok(()) => println!("ok   {}", f.display()),
                    Err(e) => {
                        println!("REFUSED {}: {e}", f.display());
                        clean = false;
                    }
                }
            }
            Ok(clean)
        }
        other => Err(bad(format!("unknown command {other}"))),
    }
}

/// The fakes map in `p`; a parse error names the position, never the text.
fn fakes_file(p: &Path) -> Result<BTreeMap<String, Vec<FakeAnswer>>, ReplayError> {
    serde_json::from_str(&read(p)?).map_err(|e| {
        bad(format!(
            "--fakes {}: not a fakes map (line {}, column {})",
            p.display(),
            e.line(),
            e.column()
        ))
    })
}

fn read(p: &Path) -> Result<String, ReplayError> {
    std::fs::read_to_string(p).map_err(|source| ReplayError::Io {
        what: format!("read {}", p.display()),
        source,
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match run(cmd, rest) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e @ ReplayError::Refused(_)) => {
            eprintln!("trusty-architect-replay: {e}; nothing written");
            ExitCode::from(1)
        }
        Err(ReplayError::BadArg(m)) => {
            eprintln!("trusty-architect-replay: {m}\n{USAGE}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("trusty-architect-replay: {e}");
            ExitCode::from(2)
        }
    }
}
