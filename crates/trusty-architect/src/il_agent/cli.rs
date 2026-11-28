//! The `il_agent.py` command line over [`super`] (#12028 P1f).
//!
//! Why: PMs and their agents call `il_agent.py brief | dispatch | handback`;
//! the port is their drop-in once the replay gate passes.
//! What: [`run`] parses argv as the script's argparse does (a top `--project`,
//! a required verb, each verb's own hidden `--project`), loads the ledger as
//! `datg.load_state` does and prints what `main()` prints. A reject is one
//! stdout line and exit 1; an unreadable ledger is `il_agent: <why>` on stderr
//! and exit 2. The ledger directory is `--project`, else `$ILIO_PROJECT`, else
//! `<checkout>/local/il`; the R library is `$ILIO_DIR` or
//! `<checkout>/local/ilio`. A default needed without a checkout is refused
//! with exit 1 before anything is read.
//! `$TMUX_PANE` is resolved through the [`Tmux`] trait, never a real tmux in
//! a test.
//! Test: `il_agent_recordings_replay_clean_against_the_port`,
//! `shipped_il_agent_finds_the_checkout_from_a_foreign_directory`,
//! `il_agent_asks_tmux_through_the_trait_for_the_pane_session`.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::argparse::{Args, HELP, Nargs, Opt, Pos};
use crate::argparse_sub::{Cli, Cmd, Sub, parse_top};
use crate::il::ilio::Lib;
use crate::il::json::Json;
use crate::il::store::{Where, resolve_with, shape_ok};
use crate::pyjson_file::{LoadError, load};
use crate::tmux::Tmux;
use crate::writers::{Outcome, no_checkout, outcome, read_text, universal_newlines};

const fn opt(long: &'static str) -> Opt {
    Opt {
        long,
        short: None,
        value: true,
    }
}

const TASK_POS: [Pos; 2] = [
    Pos {
        name: "task",
        nargs: Nargs::One,
        choices: &[],
        required: true,
    },
    Pos {
        name: "extras",
        nargs: Nargs::Star,
        choices: &[],
        required: false,
    },
];

const SUBS: [Sub; 3] = [
    Sub {
        name: "brief",
        opts: &[HELP, opt("--project")],
        pos: &TASK_POS,
        usage: "usage: il_agent.py brief [-h] task [extras ...]\n",
        help: "usage: il_agent.py brief [-h] task [extras ...]\n\npositional arguments:\n  task\n  extras\n\noptions:\n  -h, --help  show this help message and exit\n",
    },
    Sub {
        name: "dispatch",
        opts: &[HELP, opt("--project"), opt("--ask")],
        pos: &TASK_POS,
        usage: "usage: il_agent.py dispatch [-h] [--ask ASK] task [extras ...]\n",
        help: "usage: il_agent.py dispatch [-h] [--ask ASK] task [extras ...]\n\npositional arguments:\n  task\n  extras\n\noptions:\n  -h, --help  show this help message and exit\n  --ask ASK\n",
    },
    Sub {
        name: "handback",
        opts: &[HELP, opt("--task"), opt("--file"), opt("--project")],
        pos: &[],
        usage: "usage: il_agent.py handback [-h] [--task TASK] [--file FILE]\n",
        help: "usage: il_agent.py handback [-h] [--task TASK] [--file FILE]\n\noptions:\n  -h, --help   show this help message and exit\n  --task TASK\n  --file FILE\n",
    },
];

const CLI: Cli = Cli {
    prog: "il_agent.py",
    usage: "usage: il_agent.py [-h] [--project PROJECT] {brief,dispatch,handback} ...\n",
    help: "usage: il_agent.py [-h] [--project PROJECT] {brief,dispatch,handback} ...

il_agent.py: read-only Intent Ledger access for agents a PM dispatches (IL-
talk prototype, stdlib only).

positional arguments:
  {brief,dispatch,handback}

options:
  -h, --help            show this help message and exit
  --project PROJECT     ledger dir (default: ILIO_PROJECT or <repo>/local/il)
",
    top: &[HELP, opt("--project")],
    dest: "verb",
    names: &["brief", "dispatch", "handback"],
    required: true,
    subs: &SUBS,
};

/// Where the script's paths come from, and its streams.
pub struct Options<'a> {
    /// The Architect checkout (`REPO`), when one was found.
    pub checkout: Option<PathBuf>,
    /// `$ILIO_PROJECT`.
    pub ilio_project: Option<String>,
    /// `$ILIO_DIR`.
    pub ilio_dir: Option<String>,
    /// The command the `IL:` line of `dispatch` runs to print a brief.
    pub brief_cmd: Vec<String>,
    /// The process environment `datg.load_state` reads
    /// (`DATG_STATE_DIR`, `DATG_SESSION`, `TMUX_PANE`).
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// The tmux server `datg` asks for `$TMUX_PANE`'s session.
    pub tmux: &'a dyn Tmux,
    /// Standard input, read by `handback` without `--file`.
    pub stdin: &'a mut dyn Read,
}

/// `os.environ.get(k) or default`: a set, non-empty value.
fn set(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.is_empty())
}

/// `datg.load_state(project)`: the state, or the `DatgError` text.
fn load_state(
    project: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    tmux: &dyn Tmux,
) -> Result<Json, String> {
    let w = Where {
        project: project.to_path_buf(),
        session: None,
        state_dir: env("DATG_STATE_DIR")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
        env_session: env("DATG_SESSION"),
        tmux_pane: env("TMUX_PANE").filter(|s| !s.is_empty()),
    };
    let paths = resolve_with(&w, &|pane| pane_session(tmux, pane)).map_err(|e| e.to_string())?;
    let st = match load(&paths.state) {
        Ok(st) => st,
        Err(LoadError::NotFound(_)) => return Err("state.json missing; run init".into()),
        Err(LoadError::Other(e)) => {
            return Err(format!("state.json malformed ({e}); refusing to write"));
        }
    };
    if !shape_ok(&st) {
        return Err("state.json malformed (bad shape); refusing to write".into());
    }
    Ok(st)
}

/// `datg._tmux_session`: `display-message -p -t <pane> '#{session_name}'`,
/// stripped; a failed call or an empty answer is no session.
///
/// Test: `il_agent_asks_tmux_through_the_trait_for_the_pane_session`.
pub fn pane_session(tmux: &dyn Tmux, pane: &str) -> Option<String> {
    let out = tmux
        .run(&["display-message", "-p", "-t", pane, "#{session_name}"])
        .ok()?;
    let name = out.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

fn value(a: &Args, i: usize) -> Option<&str> {
    a.values.get(i).and_then(|v| v.as_deref())
}

/// Runs the script's command line on `args`.
///
/// Why: one body for the shipped binary and the replay entry point (D6a).
/// What: parse, resolve the ledger and the R library, then the verb.
/// Test: `il_agent_recordings_replay_clean_against_the_port`.
pub fn run(args: &[String], opts: &mut Options<'_>) -> Outcome {
    let (verb, a, top) = match parse_top(&CLI, args, &|_, _, _| Ok(())) {
        (Cmd::Run(i, a), top) => (i, a, top),
        (Cmd::Help(text), _) => return outcome(text.into(), String::new(), 0),
        (Cmd::Error(e), _) => return outcome(String::new(), e, 2),
        (Cmd::None, _) => return outcome(String::new(), String::new(), 0),
    };
    let project_at = if verb == 2 { 3 } else { 1 };
    let proj = set(value(&a, project_at)).or(value(&top, 1));
    let checkout = opts.checkout.as_deref();
    let hint = "; pass --project and set ILIO_DIR";
    let project = match (set(proj), set(opts.ilio_project.as_deref()), checkout) {
        (Some(p), ..) | (None, Some(p), _) => PathBuf::from(p),
        (None, None, Some(root)) => root.join("local").join("il"),
        (None, None, None) => return no_checkout("il_agent.py", hint),
    };
    let lib_dir = match (set(opts.ilio_dir.as_deref()), checkout) {
        (Some(d), _) => PathBuf::from(d),
        (None, Some(root)) => root.join("local").join("ilio"),
        (None, None) => return no_checkout("il_agent.py", hint),
    };
    let st = match load_state(&project, opts.env, opts.tmux) {
        Ok(st) => st,
        Err(e) => return outcome(String::new(), format!("il_agent: {e}\n"), 2),
    };
    let lib = Lib::load(Some(&lib_dir));
    let pos = |i: usize| a.pos.get(i).cloned().flatten().unwrap_or_default();
    let result = match verb {
        0 => super::render_brief(&st, &lib, &pos(0).concat(), &pos(1)).map_err(|e| e.to_string()),
        1 => {
            let ask = value(&a, 2).unwrap_or_default();
            let cmd = &opts.brief_cmd;
            super::render_dispatch(&st, &lib, &pos(0).concat(), &pos(1), ask, proj, cmd)
                .map_err(|e| e.to_string())
        }
        _ => {
            let text = match value(&a, 2) {
                Some(f) => match read_text(Path::new(f)) {
                    Ok(t) => t,
                    Err(e) => return outcome(String::new(), format!("{}\n", e.last_line(f)), 1),
                },
                None => {
                    let mut buf = Vec::new();
                    let _ = opts.stdin.read_to_end(&mut buf);
                    universal_newlines(String::from_utf8_lossy(&buf).into_owned())
                }
            };
            super::handback(&text, &st, &lib, value(&a, 1))
        }
    };
    match result {
        Ok(text) => outcome(format!("{text}\n"), String::new(), 0),
        Err(line) => outcome(format!("{line}\n"), String::new(), 1),
    }
}
