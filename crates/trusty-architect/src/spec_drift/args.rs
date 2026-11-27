//! argparse's behaviour for the script's three options (`-h`, `--root ROOT`,
//! `--no-log`): unique-prefix abbreviations, `--opt=value`, the error lines
//! and the help text, as Python 3.14 prints them without a terminal.
//! Test: the `bad-arg-*` recordings in `tests/replay_spec_drift_check.rs`;
//! `help_drops_the_instance_id_from_the_description` for the help text.

/// The parse result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Parsed {
    /// Run with these settings.
    Run { root: Option<String>, no_log: bool },
    /// `-h` / `--help`: print the help, exit 0.
    Help,
    /// An argparse error message (exit 2).
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opt {
    Help,
    Root,
    NoLog,
}

const LONG: [(&str, Opt); 3] = [
    ("--help", Opt::Help),
    ("--root", Opt::Root),
    ("--no-log", Opt::NoLog),
];

/// How argparse reads one argument.
enum Arg {
    /// Not an option (argparse's `A`).
    Plain,
    /// An option string no action owns.
    Unknown,
    /// An option, the string that named it, and an `=value` or `-hX` tail.
    Opt(Opt, Option<String>),
    /// A prefix more than one option shares.
    Ambiguous(String),
}

fn exact(s: &str) -> Option<Opt> {
    if s == "-h" {
        return Some(Opt::Help);
    }
    LONG.iter().find(|(n, _)| *n == s).map(|(_, o)| *o)
}

fn negative_number(a: &str) -> bool {
    let Some(rest) = a.strip_prefix('-') else {
        return false;
    };
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    match rest.split_once('.') {
        None => !rest.is_empty() && digits(rest),
        Some((int, frac)) => digits(int) && !frac.is_empty() && digits(frac),
    }
}

/// `ArgumentParser._parse_optional` for this parser.
fn classify(a: &str) -> Arg {
    if !a.starts_with('-') || a == "-" {
        return Arg::Plain;
    }
    if let Some(o) = exact(a) {
        return Arg::Opt(o, None);
    }
    if let Some((name, value)) = a.split_once('=')
        && let Some(o) = exact(name)
    {
        return Arg::Opt(o, Some(value.to_owned()));
    }
    let mut matches: Vec<(Opt, Option<String>, &str)> = if a.starts_with("--") {
        let (prefix, value) = a
            .split_once('=')
            .map_or((a, None), |(p, v)| (p, Some(v.to_owned())));
        LONG.iter()
            .filter(|(n, _)| n.starts_with(prefix))
            .map(|(n, o)| (*o, value.clone(), *n))
            .collect()
    } else if let Some(tail) = a.strip_prefix("-h") {
        vec![(Opt::Help, Some(tail.to_owned()), "-h")]
    } else {
        Vec::new()
    };
    match matches.len() {
        0 if negative_number(a) || a.contains(' ') => Arg::Plain,
        0 => Arg::Unknown,
        1 => {
            let (o, v, _) = matches.swap_remove(0);
            Arg::Opt(o, v)
        }
        _ => {
            let names: Vec<&str> = matches.iter().map(|m| m.2).collect();
            Arg::Ambiguous(format!(
                "ambiguous option: {a} could match {}",
                names.join(", ")
            ))
        }
    }
}

fn label(o: Opt) -> &'static str {
    match o {
        Opt::Help => "-h/--help",
        Opt::Root => "--root",
        Opt::NoLog => "--no-log",
    }
}

/// Parses argv after the program name.
pub(super) fn parse(args: &[String]) -> Parsed {
    let (mut root, mut no_log, mut extras) = (None, false, Vec::new());
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if a == "--" {
            extras.extend(args[i..].iter().cloned());
            break;
        }
        match classify(a) {
            Arg::Plain | Arg::Unknown => extras.push(a.clone()),
            Arg::Ambiguous(msg) => return Parsed::Error(msg),
            Arg::Opt(Opt::Root, Some(v)) => root = Some(v),
            Arg::Opt(Opt::Root, None) => match args.get(i) {
                Some(v) if v != "--" && matches!(classify(v), Arg::Plain) => {
                    root = Some(v.clone());
                    i += 1;
                }
                _ => return Parsed::Error("argument --root: expected one argument".into()),
            },
            Arg::Opt(o, Some(v)) => {
                let shown = crate::pystr::py_repr(&v);
                return Parsed::Error(format!(
                    "argument {}: ignored explicit argument {shown}",
                    label(o)
                ));
            }
            Arg::Opt(Opt::Help, None) => return Parsed::Help,
            Arg::Opt(Opt::NoLog, None) => no_log = true,
        }
    }
    if extras.is_empty() {
        Parsed::Run { root, no_log }
    } else {
        Parsed::Error(format!("unrecognized arguments: {}", extras.join(" ")))
    }
}

fn usage(prog: &str) -> String {
    format!("usage: {prog} [-h] [--root ROOT] [--no-log]\n")
}

/// The `-h` text.
// #12028: the script's description names an instance id; the port drops it.
pub(super) fn help(prog: &str) -> String {
    format!(
        "{}\nAnti-drift check for SPEC-ARCHITECT-COMPONENTS.\n\noptions:\n  \
         -h, --help   show this help message and exit\n  --root ROOT\n  \
         --no-log     do not append to the soak log\n",
        usage(prog)
    )
}

/// The stderr text of an argparse error.
pub(super) fn error(prog: &str, msg: &str) -> String {
    format!("{}{prog}: error: {msg}\n", usage(prog))
}

#[cfg(test)]
mod tests {
    use super::help;

    // #12028: the one divergence from the script; its help description ends in an instance id the public port omits.
    #[test]
    fn help_drops_the_instance_id_from_the_description() {
        let text = help("spec_drift_check.py");
        assert_eq!(
            text,
            "usage: spec_drift_check.py [-h] [--root ROOT] [--no-log]\n\n\
             Anti-drift check for SPEC-ARCHITECT-COMPONENTS.\n\noptions:\n  \
             -h, --help   show this help message and exit\n  --root ROOT\n  \
             --no-log     do not append to the soak log\n"
        );
        let description = text.lines().nth(2).unwrap();
        assert!(!description.contains('('), "{description}");
    }
}
