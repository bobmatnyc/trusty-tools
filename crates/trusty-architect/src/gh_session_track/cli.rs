//! `main()` of `gh-session-track.py` (#12087).
//!
//! Why: the binary and the replay entry point share one command line, so a
//! clean replay proves the shipped behaviour.
//! What: [`run`] parses `--repo`, `--root`, `--dry-run` and `--json` the
//! way argparse does, then needs the Architect checkout (none: exit 1 before
//! any gh call). `local/architect.json` (or `ARCHITECT_CONFIG`) gives the
//! default repo (the first `repos` entry), the gh account directory (the
//! repo owner's `gh_config_dir`, as `verify-claim` reads it, set when it is
//! a directory) and the project mark (the matching entry's name, else the
//! slug's repo part). The four gh reads run next; a gh failure is one
//! `gh-session-track: <e>` line, exit 1. The store is `--root`, else
//! `DASHBOARD_ROOT`, else `<checkout>/local/owner-queue`, in
//! `DASHBOARD_FORMAT`. `--dry-run` plans and writes nothing; otherwise
//! [`super::write::apply`] writes one batch. The plan prints as text, or
//! as `json.dumps(plan, indent=2)` with `--json`.
//! Test: `tests/replay_gh_session_track.rs`, `tests/gh_session_track_smoke.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::gh::fetch;
use super::plan::{SessionMap, build_plan};
use super::render::format_plan;
use super::write::{Fail, Pass, apply, load_sessions};
use crate::architect_config::{self, Config};
use crate::argparse::{self, HELP, Opt, Parsed};
use crate::classify::alerts::iso;
use crate::clock::Clock;
use crate::dashboard::Store;
use crate::dashboard::store::Format;
use crate::il::ilio::secret::looks_secret;
use crate::pyval::dumps;
use crate::spec_drift::Outcome;
use crate::tasks::store_read::{abspath, format_error};
use crate::verify_claim::repo_maps;
use crate::writers::no_checkout;

/// The script's name in its own messages.
pub const PROG: &str = "gh-session-track.py";
const TAG: &str = "gh-session-track";

const OPTS: [Opt; 5] = [
    HELP,
    Opt {
        long: "--repo",
        short: None,
        value: true,
    },
    Opt {
        long: "--root",
        short: None,
        value: true,
    },
    Opt {
        long: "--dry-run",
        short: None,
        value: false,
    },
    Opt {
        long: "--json",
        short: None,
        value: false,
    },
];

const USAGE: &str = "usage: gh-session-track.py [-h] [--repo REPO] [--root ROOT] [--dry-run]\n                           [--json]\n";
// #12087: the description is the docstring's first line, which names no login.
const HELP_BODY: &str = "\ngh-session-track.py: open PRs and issues of a GitHub repo, mapped onto\n\
dashboard sessions (stdlib; read-only gh).\n\n\
options:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --root ROOT\n  \
--dry-run    print the plan, write nothing\n  --json       print the plan as JSON\n";

/// What one run reads besides its arguments.
pub struct Options {
    /// The Architect checkout (`ARCHITECT.md` at or above the working directory).
    pub checkout: Option<PathBuf>,
    /// The caller's environment (`os.environ`).
    pub environ: Vec<(String, String)>,
    /// The run's clock; the store's write time comes from it too.
    pub clock: Arc<dyn Clock + Send + Sync>,
}

fn done(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

fn fail(msg: &str) -> Outcome {
    done(String::new(), format!("{TAG}: {msg}\n"), 1)
}

/// The store's secret check: a shape name, never the value (LDG-2.3.5).
fn secret_shape(s: &str) -> Option<String> {
    looks_secret(s).then(|| "secret-shaped value".to_owned())
}

/// The repo, the gh child environment and the project mark from the config.
fn resolve(
    config: &Config,
    repo_arg: Option<String>,
    environ: &[(String, String)],
) -> Result<(String, Vec<(String, String)>, String), String> {
    let home = environ
        .iter()
        .find(|(k, _)| k == "HOME")
        .map(|(_, v)| v.as_str());
    let repo = match repo_arg {
        Some(r) => r,
        None => config
            .repos
            .first()
            .map(|(_, r)| r.slug.clone())
            .ok_or_else(|| format!("no --repo given and {} has no repos entry", config.path))?,
    };
    let (_, owners) = repo_maps(config, home).map_err(|e| e.0)?;
    let owner = repo.split('/').next().unwrap_or("").to_lowercase();
    let mut env: Vec<(String, String)> = environ
        .iter()
        .filter(|(k, _)| k != "GH_TOKEN" && k != "GITHUB_TOKEN")
        .cloned()
        .collect();
    let dir = owners
        .into_iter()
        .find(|(o, _)| *o == owner)
        .and_then(|(_, d)| d);
    if let Some(dir) = dir.filter(|d| Path::new(d).is_dir()) {
        env.retain(|(k, _)| k != "GH_CONFIG_DIR");
        env.push(("GH_CONFIG_DIR".into(), dir));
    }
    let mark = config
        .repos
        .iter()
        .find(|(_, r)| r.slug.eq_ignore_ascii_case(&repo))
        .map(|(name, _)| name.clone())
        .unwrap_or_else(|| repo.rsplit('/').next().unwrap_or("").to_owned());
    Ok((repo, env, mark))
}

/// `main(argv)`.
///
/// Why: the script's command line, for the binary and the replay port.
/// What: see the module doc.
/// Test: `gh_session_track_recordings_replay_clean_against_the_port`,
/// `shipped_gh_session_track_without_a_checkout_exits_1`.
pub fn run(args: &[String], opts: &Options) -> Outcome {
    let a = match argparse::parse(args, &OPTS, false) {
        Parsed::Run(a) => a,
        Parsed::Help => return done(format!("{USAGE}{HELP_BODY}"), String::new(), 0),
        Parsed::Error(msg) => return done(String::new(), argparse::error(USAGE, PROG, &msg), 2),
    };
    let Some(checkout) = opts.checkout.clone() else {
        return no_checkout(PROG, "");
    };
    let environ = &opts.environ;
    let env = |k: &str| environ.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let config = match architect_config::load(&checkout, &env) {
        Ok(c) => c,
        Err(e) => return fail(&e.0),
    };
    let (repo, gh_env, mark) = match resolve(&config, a.values[1].clone(), environ) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    let secs = opts.clock.now_unix().floor() as i64;
    let now = iso(secs as f64);
    let fetched = match fetch(&repo, secs, &gh_env) {
        Ok(Ok(f)) => f,
        Ok(Err(e)) => return fail(&e),
        Err(raised) => return done(String::new(), format!("{raised}\n"), 1),
    };
    let root = a.values[2].clone().unwrap_or_else(|| {
        env("DASHBOARD_ROOT")
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| {
                checkout
                    .join("local/owner-queue")
                    .to_string_lossy()
                    .into_owned()
            })
    });
    let root = abspath(&root);
    let format = match env("DASHBOARD_FORMAT").as_deref() {
        None | Some("" | "dir") => Format::Dir,
        Some("jsonl") => Format::Jsonl,
        Some(v) => return fail(&format_error(v)),
    };
    // See #12087: the crate defines no crate-label or fallback sessions; the
    // script's are instance data with no config home yet.
    let map = SessionMap {
        mark,
        ..SessionMap::default()
    };
    let result = if a.flags[3] {
        load_sessions(&root, format)
            .map_err(Fail::Store)
            .and_then(|s| build_plan(&fetched, &s, &now, &map).map_err(Fail::Raised))
    } else {
        let store = Store::new(&root, format)
            .with_secret_scan(secret_shape)
            .with_clock(opts.clock.clone())
            .with_actor(TAG);
        let pass = Pass {
            repo: &repo,
            now: &now,
            map: &map,
        };
        apply(&store, &root, &fetched, &pass, &mut |_| {})
    };
    match result {
        Ok(plan) if a.flags[4] => done(
            format!("{}\n", dumps(&plan.to_py(), Some(2), true)),
            String::new(),
            0,
        ),
        Ok(plan) => done(format_plan(&plan), String::new(), 0),
        Err(Fail::Store(e)) => fail(&e),
        Err(Fail::Raised(line)) => done(String::new(), format!("{line}\n"), 1),
    }
}
