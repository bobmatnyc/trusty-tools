//! `add_subparsers` on top of [`crate::argparse`] (#12028 P1e).
//!
//! Why: `brief-id.py` and `aliases.py` parse a subcommand and its own
//! options; their ports must print what argparse prints for every bad
//! command line, in argparse's order.
//! What: [`parse`] runs the top parser (only `-h`, and the subparsers
//! action, which takes the command and every argument after it), checks the
//! command against its choices, runs that command's parser on the rest, and
//! reports the top parser's and the subparser's unrecognized arguments
//! together, top first, as `parse_args` does.
//! Test: the `bad-arg-*` and `help-*` recordings of `brief-id` and `aliases`
//! in `tests/replay_writers.rs`.

use crate::argparse::{self, Args, Nargs, Opt, Parsed, Pos};

/// One subcommand's parser.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sub {
    /// The command word.
    pub(crate) name: &'static str,
    /// Its options, `-h` first.
    pub(crate) opts: &'static [Opt],
    /// Its positionals.
    pub(crate) pos: &'static [Pos],
    /// Its usage block (`usage: ...\n`).
    pub(crate) usage: &'static str,
    /// Its full `-h` text.
    pub(crate) help: &'static str,
}

/// The top parser.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cli {
    /// The program name.
    pub(crate) prog: &'static str,
    /// The top usage block.
    pub(crate) usage: &'static str,
    /// The top `-h` text.
    pub(crate) help: &'static str,
    /// The top parser's options, `-h` first (#12028 P1f: `il_agent.py --project`).
    pub(crate) top: &'static [Opt],
    /// The subparsers action's dest.
    pub(crate) dest: &'static str,
    /// The command words, in `add_parser` order.
    pub(crate) names: &'static [&'static str],
    /// `add_subparsers(required=...)`.
    pub(crate) required: bool,
    /// The subparsers, in the order of `names`.
    pub(crate) subs: &'static [Sub],
}

/// The parse result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cmd {
    /// Run command `subs[index]` with these settings.
    Run(usize, Args),
    /// No command given (`required=False`).
    None,
    /// `-h` on some parser: print this text, exit 0.
    Help(&'static str),
    /// An argparse error: this stderr text, exit 2.
    Error(String),
}

/// Parses `args` for `cli`; `check(command, option, value)` is each
/// subparser's `type=` / `choices` conversion.
pub(crate) fn parse(
    cli: &Cli,
    args: &[String],
    check: &dyn Fn(usize, usize, &str) -> Result<(), String>,
) -> Cmd {
    parse_top(cli, args, check).0
}

/// [`parse`], plus what the top parser's own options ([`Cli::top`]) set.
pub(crate) fn parse_top(
    cli: &Cli,
    args: &[String],
    check: &dyn Fn(usize, usize, &str) -> Result<(), String>,
) -> (Cmd, Args) {
    let pos = [Pos {
        name: cli.dest,
        nargs: Nargs::Parser,
        choices: cli.names,
        required: cli.required,
    }];
    let top = match argparse::parse_known(args, cli.top, &pos, &|_, _| Ok(())) {
        Parsed::Run(a) => a,
        Parsed::Help => return (Cmd::Help(cli.help), Args::default()),
        Parsed::Error(m) => {
            let e = Cmd::Error(argparse::error(cli.usage, cli.prog, &m));
            return (e, Args::default());
        }
    };
    let cmd = parse_sub(cli, &top, check);
    (cmd, top)
}

fn parse_sub(
    cli: &Cli,
    top: &Args,
    check: &dyn Fn(usize, usize, &str) -> Result<(), String>,
) -> Cmd {
    let unrecognized = |extras: &[String]| {
        let msg = format!("unrecognized arguments: {}", extras.join(" "));
        Cmd::Error(argparse::error(cli.usage, cli.prog, &msg))
    };
    let Some(Some(words)) = top.pos.first() else {
        return if top.extras.is_empty() {
            Cmd::None
        } else {
            unrecognized(&top.extras)
        };
    };
    let Some(index) = words
        .first()
        .and_then(|w| cli.names.iter().position(|n| n == w))
    else {
        return Cmd::None;
    };
    let sub = &cli.subs[index];
    let check_sub = |opt: usize, v: &str| check(index, opt, v);
    let sub_args = match argparse::parse_known(&words[1..], sub.opts, sub.pos, &check_sub) {
        Parsed::Run(a) => a,
        Parsed::Help => return Cmd::Help(sub.help),
        Parsed::Error(m) => {
            let prog = format!("{} {}", cli.prog, sub.name);
            return Cmd::Error(argparse::error(sub.usage, &prog, &m));
        }
    };
    let mut extras = top.extras.clone();
    extras.extend(sub_args.extras.iter().cloned());
    if extras.is_empty() {
        Cmd::Run(index, sub_args)
    } else {
        unrecognized(&extras)
    }
}
