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
//! mirrored in the library), `dashboard_ids` and `dashboard_qual` (#12029
//! P2d, mirrored in the library), `owner_answers` (#12029 P2e, mirrored in
//! the library), `questions_rc` (#12029 P2f, mirrored in the library).
//! `gh-session-track` (#12087), `pending-answers` (#12029 P2e) and
//! `dashboard-db` (#12029 P2g) run the script's own command line;
//! `dashboard_updates` and `dashboard_attach` (#12029 P2h) are mirrored in
//! the library. The other drivers live in `replay_port/drivers.rs`.
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
//! `dashboard_derive_recordings_replay_clean_against_the_port`,
//! `dashboard_store_recordings_replay_clean_against_the_port`,
//! `dashboard_ids_recordings_replay_clean_against_the_port`,
//! `dashboard_qual_recordings_replay_clean_against_the_port`,
//! `owner_answers_recordings_replay_clean_against_the_port`,
//! `pending_answers_recordings_replay_clean_against_the_port`,
//! `questions_rc_recordings_replay_clean_against_the_port`,
//! `dashboard_db_recordings_replay_clean_against_the_port`,
//! `dashboard_updates_recordings_replay_clean_against_the_port` and
//! `dashboard_attach_recordings_replay_clean_against_the_port`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use trusty_architect::dashboard::{pyanswers, pyderive, pyjsonl};
use trusty_architect::gh_session_track;
use trusty_architect::il_agent;
use trusty_architect::level_lint;
use trusty_architect::replay::CLOCK_ENV;
use trusty_architect::ruling_pack;
use trusty_architect::spec_drift;
use trusty_architect::tasks::{stale, stalled, wait_check};
use trusty_architect::verify_claim;
use trusty_architect::writers::{aliases, brief_ids, state_archive};
use trusty_architect::{Clock, FakeClock, ScriptedTmux, SystemClock};

#[path = "replay_port/drivers.rs"]
mod drivers;

use drivers::{
    architect_config, dashboard_schema, ids36_driver, il_timeline, out, technical_level,
};

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
        // See #12029 P2d: the ids and qual `driver.py` mirrors, on the frozen clock.
        "dashboard_ids" | "dashboard_qual" => {
            let at = Arc::new(FakeClock::new(
                frozen.map_or_else(|| SystemClock.now_unix(), |t| t as f64),
            ));
            if script == "dashboard_ids" {
                trusty_architect::dashboard::pyids::replay_cases(&rest, at)
            } else {
                trusty_architect::dashboard::pyqual::replay_cases(&rest, at)
            }
        }
        // See #12029 P2e: the `owner_answers` driver mirror, and
        // `pending-answers` on its own command line (the sandbox root
        // stands for the checkout).
        "owner_answers" | "pending-answers" => {
            let at: Arc<dyn Clock + Send + Sync> = Arc::new(FakeClock::new(
                frozen.map_or_else(|| SystemClock.now_unix(), |t| t as f64),
            ));
            if script == "owner_answers" {
                pyanswers::replay_cases(&rest, at)
            } else {
                let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
                let env = |k: &str| std::env::var(k).ok();
                pyanswers::cli::run(
                    &rest,
                    &pyanswers::cli::Options {
                        prog: "pending-answers.py".into(),
                        default_root: cwd.join("local/owner-queue"),
                        schema_dir: cwd.join("schemas"),
                        cwd,
                        env: &env,
                        clock: at,
                    },
                )
            }
        }
        // #12029 P2f: the questions driver mirror, on the frozen clock.
        "questions_rc" => trusty_architect::questions::cases::replay(
            &rest,
            frozen.map_or_else(|| SystemClock.now_unix(), |t| t as f64),
        ),
        // #12029 P2g: `ddb_driver.py` points the script's checkout at the
        // working directory, so the sandbox root stands for it.
        "dashboard-db" => trusty_architect::dashboard::pydb::replay(
            &rest,
            frozen.map_or_else(|| SystemClock.now_unix(), |t| t as f64),
        ),
        // #12029 P2h: the updates driver (its command line and its cases)
        // and the attach cases, on the frozen clock.
        "dashboard_updates" | "dashboard_attach" => {
            let at: Arc<dyn Clock + Send + Sync> = Arc::new(FakeClock::new(
                frozen.map_or_else(|| SystemClock.now_unix(), |t| t as f64),
            ));
            if script == "dashboard_updates" {
                trusty_architect::dashboard::pyupdates::cases::replay(&rest, at)
            } else {
                trusty_architect::dashboard::pyattach::cases::replay_cases(&rest, at)
            }
        }
        "technical_level" => technical_level(&rest),
        "architect_config" => architect_config(&rest),
        other => {
            eprintln!(
                "replay_port: no port named {other:?}; known: spec_drift_check, level_lint, \
                 technical_level, architect_config, stale-tasks, stalled_tasks, task_wait_check, \
                 il_timeline, state-archive, aliases, brief-id, il_agent, verify_claim, \
                 gh-session-track, ruling_pack, ids36, dashboard_schema, dashboard_jsonl, \
                 dashboard_derive, dashboard_store, dashboard_ids, dashboard_qual, \
                 owner_answers, pending-answers, questions_rc, dashboard-db, \
                 dashboard_updates, dashboard_attach"
            );
            return ExitCode::from(2);
        }
    };
    let _ = std::io::stdout().write_all(result.stdout.as_bytes());
    let _ = std::io::stderr().write_all(result.stderr.as_bytes());
    ExitCode::from(u8::try_from(result.code).unwrap_or(2))
}
