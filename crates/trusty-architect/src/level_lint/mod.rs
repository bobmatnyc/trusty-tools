//! The port of `level_lint.py`: warn when owner-facing text carries detail its technical level does not allow (#12028).
//!
//! Why: text written at `pm` or below must stay free of paths, SHAs, flags
//! and tool names; this format check catches the mechanical part. It
//! replaces the replay stand-in of #12026.
//! What: [`lint`] returns the script's warnings (rule, match, line) for a
//! level below `engineer`, else none: URLs are blanked first, each rule's
//! `finditer` runs in order and a span an earlier rule took is skipped.
//! [`format_warnings`] renders them. [`run`] is the command line
//! (`[--level L] [--strict] [--file F | TEXT...]`, else stdin; the level
//! defaults to the stored setting): warnings on stdout, exit 0, or 1 with
//! `--strict` and any warning; argparse errors exit 2.
//! Test: `level_lint_recordings_replay_clean_against_the_port`,
//! `shipped_level_lint_command_warns_and_exits_like_the_script`.

mod rules;

use std::io::Read;
use std::path::PathBuf;

use crate::argparse::{self, HELP, Opt, Parsed};
use crate::pystr::py_repr;
use crate::spec_drift::Outcome;
use crate::technical_level as tl;

/// One warning: the rule name, the matched text and its 1-based line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// One of `path`, `sha`, `flag`, `envvar`, `job`, `tool`, `identifier`.
    pub rule: &'static str,
    /// The matched text (URLs inside it are blanks, as the script masks them).
    pub matched: String,
    /// The line of the match's first character.
    pub line: usize,
}

/// `lint(text, level)`: the warnings for `text` at `level` (unknown: `pm`).
pub fn lint(text: &str, level: &str) -> Vec<Warning> {
    if !tl::below_engineer(level) {
        return Vec::new();
    }
    let orig: Vec<char> = text.chars().collect();
    let mut masked = orig.clone();
    rules::mask_urls(&mut masked);
    let mut taken: Vec<(usize, usize)> = Vec::new();
    let mut found: Vec<(usize, Warning)> = Vec::new();
    for rule in rules::RULES {
        for (s, e) in rules::find_iter(rule, &masked) {
            if taken.iter().any(|(ts, te)| s < *te && *ts < e) {
                continue;
            }
            taken.push((s, e));
            let line = orig[..s].iter().filter(|c| **c == '\n').count() + 1;
            found.push((
                s,
                Warning {
                    rule,
                    matched: masked[s..e].iter().collect(),
                    line,
                },
            ));
        }
    }
    found.sort_by_key(|(s, _)| *s);
    found.into_iter().map(|(_, w)| w).collect()
}

/// `format_warnings(warnings, level, label)`: one line per warning.
pub fn format_warnings(warnings: &[Warning], level: &str, label: &str) -> Vec<String> {
    let level = tl::normalize(level);
    let at = if label.is_empty() {
        String::new()
    } else {
        format!("{label}: ")
    };
    warnings
        .iter()
        .map(|w| {
            format!(
                "level_lint [{level}]: {at}{} `{}` (line {})",
                w.rule, w.matched, w.line
            )
        })
        .collect()
}

/// What the command line needs besides argv and stdin.
#[derive(Debug, Clone)]
pub struct Options {
    /// The program name argparse prints.
    pub prog: String,
    /// The dashboard store read when `--level` is absent (the script's
    /// `<checkout>/local/owner-queue`).
    pub store_root: PathBuf,
    /// `DASHBOARD_FORMAT`, as the store reads it.
    pub dashboard_format: Option<String>,
}

const OPTS: [Opt; 4] = [
    HELP,
    Opt {
        long: "--level",
        short: None,
        value: true,
    },
    Opt {
        long: "--strict",
        short: None,
        value: false,
    },
    Opt {
        long: "--file",
        short: None,
        value: true,
    },
];

const USAGE_REST: &str = "[-h] [--level LEVEL] [--strict] [--file FILE] [text ...]";

/// The `-h` text, as Python 3.14 prints it at the default 80 columns.
fn help(prog: &str) -> String {
    format!(
        "{}\nWarn on detail the technical level does not allow.\n\n\
         positional arguments:\n  text\n\noptions:\n  \
         -h, --help     show this help message and exit\n  \
         --level LEVEL  one of non-technical, vibe-coder, pm, engineer, senior-\n                 \
         engineer, cto; default: the stored setting\n  \
         --strict       exit 1 when there is any warning\n  \
         --file FILE    read the text from this file (else the TEXT arguments, else\n                 \
         stdin)\n",
        argparse::usage(prog, USAGE_REST)
    )
}

fn done(stdout: String, stderr: String, code: i32) -> Outcome {
    Outcome {
        stdout,
        stderr,
        code,
    }
}

/// `open(path, encoding="utf-8").read()`: universal newlines.
/// The last traceback line of the `OSError` Python raises for `e`.
fn os_error(e: &std::io::Error) -> String {
    let class = match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        std::io::ErrorKind::IsADirectory => "IsADirectoryError",
        _ => "OSError",
    };
    let errno = e.raw_os_error().unwrap_or(0);
    let why = e.to_string();
    let why = why.split(" (os error").next().unwrap_or(&why).to_owned();
    format!("{class}: [Errno {errno}] {why}")
}

fn read_text(path: &str) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {}", os_error(&e), py_repr(path)))?;
    let text = String::from_utf8(bytes).map_err(|_| "UnicodeDecodeError".to_owned())?;
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// The command line, argv after the program name.
///
/// Why: the shipped binary and the replay entry share one body, so the
/// replayed code is the shipped code (#12028 D6a).
/// What: see the module doc. A file the script could not open ends the run
/// with exit 1 and the exception's last traceback line on stderr; so does
/// a stdin that cannot be read.
/// Test: `level_lint_recordings_replay_clean_against_the_port`,
/// `a_stdin_that_cannot_be_read_fails_closed`.
pub fn run(args: &[String], stdin: &mut dyn Read, opts: &Options) -> Outcome {
    let usage = argparse::usage(&opts.prog, USAGE_REST);
    let a = match argparse::parse(args, &OPTS, true) {
        Parsed::Run(a) => a,
        Parsed::Help => return done(help(&opts.prog), String::new(), 0),
        Parsed::Error(msg) => {
            return done(String::new(), argparse::error(&usage, &opts.prog, &msg), 2);
        }
    };
    let (level, strict, file) = (&a.values[1], a.flags[2], &a.values[3]);
    let text = match file.as_deref() {
        Some(f) if !f.is_empty() => match read_text(f) {
            Ok(t) => t,
            Err(e) => return done(String::new(), format!("{e}\n"), 1),
        },
        _ if !a.positionals.is_empty() => a.positionals.join(" "),
        _ => {
            // #12028: a failed read is the script's OSError, never empty text
            // (an empty text would pass `--strict`).
            let mut raw = Vec::new();
            if let Err(e) = stdin.read_to_end(&mut raw) {
                return done(String::new(), format!("{}\n", os_error(&e)), 1);
            }
            String::from_utf8_lossy(&raw).into_owned()
        }
    };
    let level: String = match level {
        Some(l) => l.clone(),
        None => match tl::store_format(opts.dashboard_format.as_deref())
            .and_then(|f| tl::read(&opts.store_root, f))
        {
            Ok(l) => l.to_owned(),
            Err(e) => return done(String::new(), format!("StoreError: {e}\n"), 1),
        },
    };
    let mut stderr = String::new();
    if !tl::LEVELS.contains(&level.as_str()) {
        stderr = format!(
            "level_lint: unknown level {}; using {}\n",
            py_repr(&level),
            tl::DEFAULT
        );
    }
    let out = format_warnings(&lint(&text, &level), &level, "");
    let stdout: String = out.iter().map(|l| format!("{l}\n")).collect();
    let code = i32::from(strict && !out.is_empty());
    done(stdout, stderr, code)
}
