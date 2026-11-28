//! Python 3.14 `argparse` parsing for the ported scripts' small command lines.
//!
//! Why: a port must print what the script prints for a bad command line too
//! (#12028 AC1), and the Architect scripts parse argv with `argparse`. One
//! parser serves every port instead of one hand-written copy per script.
//! What: [`parse`] takes the parser's options (in `add_argument` order, `-h`
//! / `--help` first) and whether it has one `nargs="*"` positional. It follows
//! `ArgumentParser.parse_known_args`: an option-pattern pass (`--` ends the
//! options, unique-prefix abbreviations, `--opt=value`, `-hX` chaining, a
//! negative-number-like or spaced string is a plain argument), then the
//! consume loop, where the positional takes the first run of plain arguments
//! and later ones are extras. The first error stops the parse, as `error()`
//! exits; `-h` stops it with [`Parsed::Help`].
//! Known gap: an extra `--` after the positional has taken its run is kept in
//! the extras text; Python 3.14 handles that rare case slightly differently.
//! Test: the `bad-arg-*` and `help` recordings in
//! `tests/replay_level_lint.rs` and `tests/replay_spec_drift_check.rs`.

use crate::pystr::{is_digit, py_repr};

/// One option of the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Opt {
    /// The long form, `--name`.
    pub(crate) long: &'static str,
    /// The short form, `-x`, if any.
    pub(crate) short: Option<&'static str>,
    /// True for an option with one value (`nargs=None`), false for a flag.
    pub(crate) value: bool,
}

/// The `-h` / `--help` option every parser starts with.
pub(crate) const HELP: Opt = Opt {
    long: "--help",
    short: Some("-h"),
    value: false,
};

/// The parse result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// Run with these settings.
    Run(Args),
    /// `-h` / `--help`: print the help, exit 0.
    Help,
    /// An argparse error message (usage on stderr, exit 2).
    Error(String),
}

/// What a successful parse set, per option in the parser's order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Args {
    /// True for a flag that was given.
    pub(crate) flags: Vec<bool>,
    /// The last value given for a value option.
    pub(crate) values: Vec<Option<String>>,
    /// The positional's arguments (empty when none or when it has none).
    pub(crate) positionals: Vec<String>,
    /// Per positional of [`parse_known`], its arguments once consumed
    /// (`Some(vec![])` for a `?` or `*` that matched nothing).
    pub(crate) pos: Vec<Option<Vec<String>>>,
    /// Every value a value option took, in order (`action="append"`).
    pub(crate) all: Vec<Vec<String>>,
    /// The unrecognized arguments ([`parse_known`] only; [`parse`] errors).
    pub(crate) extras: Vec<String>,
}

/// A positional's `nargs` (#12028 P1e).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nargs {
    /// `nargs=None`: exactly one, required.
    One,
    /// `nargs="?"`.
    Optional,
    /// `nargs="*"`.
    Star,
    /// A subparsers action (`A...`): the command and everything after it.
    Parser,
}

/// One positional of a parser, in `add_argument` order (#12028 P1e).
///
/// Precondition: only the last positional is [`Nargs::Star`] or
/// [`Nargs::Parser`]; the left-to-right greedy match then equals argparse's
/// regex match, which never has to give an argument back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pos {
    /// The dest (or metavar) argparse names in its messages.
    pub(crate) name: &'static str,
    /// Its `nargs`.
    pub(crate) nargs: Nargs,
    /// `choices`, checked on the first argument; empty for none.
    pub(crate) choices: &'static [&'static str],
    /// `required` (a [`Nargs::One`] is always required).
    pub(crate) required: bool,
}

const STAR: Pos = Pos {
    name: "",
    nargs: Nargs::Star,
    choices: &[],
    required: false,
};

/// argparse's `invalid choice` message for `value` against `choices`.
pub(crate) fn invalid_choice(name: &str, value: &str, choices: &[&str]) -> String {
    let list: Vec<String> = choices.iter().map(|c| py_repr(c)).collect();
    format!(
        "argument {name}: invalid choice: {} (choose from {})",
        py_repr(value),
        list.join(", ")
    )
}

/// One classified argument: plain (`A`), `--` (`-`), or an option (`O`).
#[derive(Debug, Clone)]
enum Kind {
    Plain,
    Dashes,
    /// The option index (`None`: no option owns it), the option string that
    /// matched, whether a separator was seen, and the explicit argument.
    Opt(Option<usize>, String, bool, Option<String>),
    /// More than one option shares the prefix: the error `consume_optional`
    /// raises when the parse reaches it.
    Ambiguous(String),
}

fn label(o: &Opt) -> String {
    match o.short {
        Some(s) => format!("{s}/{}", o.long),
        None => o.long.to_owned(),
    }
}

/// Every option string, in `_option_string_actions` order, with its option.
fn strings(opts: &[Opt]) -> Vec<(&'static str, usize)> {
    let mut out = Vec::new();
    for (i, o) in opts.iter().enumerate() {
        if let Some(s) = o.short {
            out.push((s, i));
        }
        out.push((o.long, i));
    }
    out
}

/// `_negative_number_matcher` (`-\.?\d`, matched at the start).
fn negative_number_like(a: &str) -> bool {
    let mut c = a.chars().skip(1).peekable();
    if c.peek() == Some(&'.') {
        c.next();
    }
    c.next().is_some_and(is_digit)
}

/// `ArgumentParser._parse_optional`.
fn classify(a: &str, opts: &[Opt]) -> Kind {
    if !a.starts_with('-') {
        return Kind::Plain;
    }
    let all = strings(opts);
    let exact = |s: &str| all.iter().find(|(n, _)| *n == s).map(|(_, i)| *i);
    if let Some(i) = exact(a) {
        return Kind::Opt(Some(i), a.to_owned(), false, None);
    }
    if a.chars().count() == 1 {
        return Kind::Plain;
    }
    if let Some((name, value)) = a.split_once('=')
        && let Some(i) = exact(name)
    {
        return Kind::Opt(Some(i), name.to_owned(), true, Some(value.into()));
    }
    let mut found: Vec<Kind> = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    if a.starts_with("--") {
        let (prefix, sep, value) = match a.split_once('=') {
            Some((p, v)) => (p, true, Some(v.to_owned())),
            None => (a, false, None),
        };
        for (n, i) in &all {
            if n.starts_with(prefix) {
                found.push(Kind::Opt(Some(*i), (*n).into(), sep, value.clone()));
                names.push(n);
            }
        }
    } else {
        let short: String = a.chars().take(2).collect();
        let tail: String = a.chars().skip(2).collect();
        let (prefix, sep, value) = match a.split_once('=') {
            Some((p, v)) => (p, true, Some(v.to_owned())),
            None => (a, false, None),
        };
        for (n, i) in &all {
            if *n == short {
                found.push(Kind::Opt(Some(*i), (*n).into(), false, Some(tail.clone())));
                names.push(n);
            } else if n.starts_with(prefix) {
                found.push(Kind::Opt(Some(*i), (*n).into(), sep, value.clone()));
                names.push(n);
            }
        }
    }
    match found.len() {
        0 => {}
        1 => return found.swap_remove(0),
        _ => {
            return Kind::Ambiguous(format!(
                "ambiguous option: {a} could match {}",
                names.join(", ")
            ));
        }
    }
    if negative_number_like(a) || a.contains(' ') {
        return Kind::Plain;
    }
    Kind::Opt(None, a.to_owned(), false, None)
}

/// Parses `args` (argv after the program name) for a parser with `opts` and,
/// when `positional` is true, one `nargs="*"` positional.
pub(crate) fn parse(args: &[String], opts: &[Opt], positional: bool) -> Parsed {
    parse_checked(args, opts, positional, &|_, _| Ok(()))
}

/// [`parse`] with a `type=` conversion: `check(option index, value)` runs
/// when the consume loop takes each value, in argparse's order, and its
/// `Err` is the parse error (#12028 P1d: `--hours` is `type=float`).
pub(crate) fn parse_checked(
    args: &[String],
    opts: &[Opt],
    positional: bool,
    check: &dyn Fn(usize, &str) -> Result<(), String>,
) -> Parsed {
    let pos: &[Pos] = if positional { &[STAR] } else { &[] };
    match parse_known(args, opts, pos, check) {
        Parsed::Run(a) if !a.extras.is_empty() => {
            Parsed::Error(format!("unrecognized arguments: {}", a.extras.join(" ")))
        }
        Parsed::Run(mut a) => {
            a.positionals = a.pos.first().cloned().flatten().unwrap_or_default();
            Parsed::Run(a)
        }
        other => other,
    }
}

/// `parse_known_args` for a parser with `opts` and the positionals `pos`:
/// the extras come back in [`Args::extras`], a missing required positional
/// is the `the following arguments are required` error (#12028 P1e).
pub(crate) fn parse_known(
    args: &[String],
    opts: &[Opt],
    pos: &[Pos],
    check: &dyn Fn(usize, &str) -> Result<(), String>,
) -> Parsed {
    parse_known_required(args, opts, pos, &[], check)
}

/// [`parse_known`] with `required=True` options: `required` holds their
/// indexes in `opts`, and one never given joins the `the following arguments
/// are required` list after the positionals (#12029 P2g: `dashboard-db.py
/// set --json`). Precondition: the parser adds its positionals before its
/// required options, so that is argparse's `_actions` order.
pub(crate) fn parse_known_required(
    args: &[String],
    opts: &[Opt],
    pos: &[Pos],
    required: &[usize],
    check: &dyn Fn(usize, &str) -> Result<(), String>,
) -> Parsed {
    let mut kinds = Vec::with_capacity(args.len());
    let mut rest = false;
    for a in args {
        if rest {
            kinds.push(Kind::Plain);
        } else if a == "--" {
            kinds.push(Kind::Dashes);
            rest = true;
        } else {
            kinds.push(classify(a, opts));
        }
    }
    let mut st = State {
        args,
        kinds: &kinds,
        opts,
        out: Args {
            flags: vec![false; opts.len()],
            values: vec![None; opts.len()],
            positionals: Vec::new(),
            pos: vec![None; pos.len()],
            all: vec![Vec::new(); opts.len()],
            extras: Vec::new(),
        },
        pos,
        next_pos: 0,
        extras: Vec::new(),
        check,
    };
    match st.run() {
        Ok(true) => Parsed::Help,
        Ok(false) => {
            let mut missing: Vec<String> = pos
                .iter()
                .zip(&st.out.pos)
                .filter(|(p, v)| v.is_none() && (p.required || p.nargs == Nargs::One))
                .map(|(p, _)| p.name.to_owned())
                .collect();
            missing.extend(
                required
                    .iter()
                    .filter(|i| {
                        st.out.all.get(**i).is_some_and(Vec::is_empty) && !st.out.flags[**i]
                    })
                    .map(|i| label(&opts[*i])),
            );
            if !missing.is_empty() {
                let names = missing.join(", ");
                return Parsed::Error(format!("the following arguments are required: {names}"));
            }
            st.out.extras = std::mem::take(&mut st.extras);
            Parsed::Run(st.out)
        }
        Err(msg) => Parsed::Error(msg),
    }
}

struct State<'a> {
    args: &'a [String],
    kinds: &'a [Kind],
    opts: &'a [Opt],
    out: Args,
    pos: &'a [Pos],
    next_pos: usize,
    extras: Vec<String>,
    check: &'a dyn Fn(usize, &str) -> Result<(), String>,
}

impl State<'_> {
    /// An `O` in argparse's pattern; an ambiguous prefix is one too, so the
    /// consume loop reaches it and raises its error (#12028).
    fn is_opt(&self, i: usize) -> bool {
        matches!(self.kinds.get(i), Some(Kind::Opt(..) | Kind::Ambiguous(_)))
    }

    /// The consume loop; `Ok(true)` when `-h` was taken.
    fn run(&mut self) -> Result<bool, String> {
        let last_opt = (0..self.kinds.len()).rev().find(|i| self.is_opt(*i));
        let mut start = 0;
        if let Some(max) = last_opt {
            while start <= max {
                let next = (start..=max).find(|i| self.is_opt(*i)).unwrap_or(max + 1);
                if start != next {
                    let end = self.consume_positionals(start)?;
                    if end > start {
                        start = end;
                        continue;
                    }
                }
                if !self.is_opt(start) {
                    self.extras.extend(self.args[start..next].iter().cloned());
                    start = next;
                }
                if start > max {
                    break;
                }
                match self.consume_optional(start)? {
                    Step::Help => return Ok(true),
                    Step::Next(n) => start = n,
                }
            }
        }
        let stop = self.consume_positionals(start)?;
        self.extras.extend(self.args[stop..].iter().cloned());
        Ok(false)
    }

    /// `consume_positionals`: `_match_arguments_partial` over the positionals
    /// left, then each takes its arguments (the first `--` dropped) and its
    /// `choices` are checked. A trailing zero-length match is left for later
    /// when an option follows, as argparse does (#12028 P1e).
    fn consume_positionals(&mut self, start: usize) -> Result<usize, String> {
        let plain = |i: usize| matches!(self.kinds.get(i), Some(Kind::Plain));
        let dashes = |mut i: usize| {
            while matches!(self.kinds.get(i), Some(Kind::Dashes)) {
                i += 1;
            }
            i
        };
        let mut counts = Vec::new();
        let mut i = start;
        for p in &self.pos[self.next_pos..] {
            let j = dashes(i);
            let end = match p.nargs {
                Nargs::One if plain(j) => dashes(j + 1),
                Nargs::One => break,
                Nargs::Optional => dashes(if plain(j) { j + 1 } else { j }),
                Nargs::Star => {
                    let mut k = j;
                    while matches!(self.kinds.get(k), Some(Kind::Plain | Kind::Dashes)) {
                        k += 1;
                    }
                    k
                }
                Nargs::Parser if plain(j) => self.kinds.len(),
                Nargs::Parser => break,
            };
            counts.push(end - i);
            i = end;
        }
        if self.is_opt(i) {
            while counts.last() == Some(&0) {
                counts.pop();
            }
        }
        let mut s = start;
        for n in counts {
            let p = self.pos[self.next_pos];
            let mut taken: Vec<String> = self.args[s..s + n].to_vec();
            // A subparsers action drops a leading `--` only; the rest is the
            // subparser's to read.
            let last = if p.nargs == Nargs::Parser {
                (s + 1).min(s + n)
            } else {
                s + n
            };
            if let Some(d) = (s..last).find(|d| matches!(self.kinds[*d], Kind::Dashes)) {
                taken.remove(d - s);
            }
            if let Some(v) = taken.first()
                && !p.choices.is_empty()
                && !p.choices.contains(&v.as_str())
            {
                return Err(invalid_choice(p.name, v, p.choices));
            }
            self.out.pos[self.next_pos] = Some(taken);
            self.next_pos += 1;
            s += n;
        }
        Ok(s)
    }

    fn consume_optional(&mut self, start: usize) -> Result<Step, String> {
        let (found, mut name, mut sep, mut explicit) = match self.kinds[start].clone() {
            Kind::Opt(f, n, s, e) => (f, n, s, e),
            Kind::Ambiguous(msg) => return Err(msg),
            Kind::Plain | Kind::Dashes => return Ok(Step::Next(start + 1)),
        };
        let Some(mut idx) = found else {
            self.extras.push(self.args[start].clone());
            return Ok(Step::Next(start + 1));
        };
        let ignored = |o: &Opt, v: &str| {
            format!(
                "argument {}: ignored explicit argument {}",
                label(o),
                py_repr(v)
            )
        };
        let mut taken: Vec<(usize, Option<String>)> = Vec::new();
        let stop = loop {
            let o = self.opts[idx];
            match explicit.take() {
                Some(v) if o.value => {
                    taken.push((idx, Some(v)));
                    break start + 1;
                }
                Some(v) if !name.starts_with("--") && !v.is_empty() => {
                    if sep || v.starts_with('-') {
                        return Err(ignored(&o, &v));
                    }
                    // `-xyz` is `-x -y -z`; an unknown letter ends the chain
                    // as an extra, and the flags before it still run.
                    taken.push((idx, None));
                    let mut chars = v.chars();
                    let next = format!("-{}", chars.next().unwrap_or_default());
                    let tail: String = chars.collect();
                    match strings(self.opts).iter().find(|(n, _)| *n == next) {
                        Some((_, i)) => {
                            idx = *i;
                            name = next;
                            (sep, explicit) = match tail.strip_prefix('=') {
                                _ if tail.is_empty() => (false, None),
                                Some(t) => (true, Some(t.to_owned())),
                                None => (false, Some(tail)),
                            };
                        }
                        None => {
                            self.extras.push(format!("-{v}"));
                            break start + 1;
                        }
                    }
                }
                Some(v) => return Err(ignored(&o, &v)),
                None if o.value => match self.kinds.get(start + 1) {
                    Some(Kind::Plain) => {
                        taken.push((idx, Some(self.args[start + 1].clone())));
                        break start + 2;
                    }
                    _ => return Err(format!("argument {}: expected one argument", label(&o))),
                },
                None => {
                    taken.push((idx, None));
                    break start + 1;
                }
            }
        };
        for (i, v) in taken {
            if self.opts[i] == HELP {
                return Ok(Step::Help);
            }
            match v {
                Some(v) => {
                    (self.check)(i, &v)?;
                    self.out.all[i].push(v.clone());
                    self.out.values[i] = Some(v);
                }
                None => self.out.flags[i] = true,
            }
        }
        Ok(Step::Next(stop))
    }
}

enum Step {
    Help,
    Next(usize),
}

/// `usage: <prog> <rest>` plus the newline, as `format_usage` prints it.
pub(crate) fn usage(prog: &str, rest: &str) -> String {
    format!("usage: {prog} {rest}\n")
}

/// The stderr text of an argparse error.
pub(crate) fn error(usage_line: &str, prog: &str, msg: &str) -> String {
    format!("{usage_line}{prog}: error: {msg}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_DRIFT: [Opt; 3] = [
        HELP,
        Opt {
            long: "--root",
            short: None,
            value: true,
        },
        Opt {
            long: "--no-log",
            short: None,
            value: false,
        },
    ];

    fn argv(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    // See #12028: the message the frozen `spec_drift_check.py` prints under
    // Python 3.14.8 for `--=x` and `--no-log --=x -h` (exit 2 for both).
    #[test]
    fn an_ambiguous_prefix_is_an_error_before_a_later_help() {
        let want =
            Parsed::Error("ambiguous option: --=x could match --help, --root, --no-log".into());
        assert_eq!(parse(&argv(&["--=x"]), &SPEC_DRIFT, false), want);
        assert_eq!(
            parse(&argv(&["--no-log", "--=x", "-h"]), &SPEC_DRIFT, false),
            want
        );
        assert_eq!(
            parse(&argv(&["-h", "--=x"]), &SPEC_DRIFT, false),
            Parsed::Help
        );
    }
}
