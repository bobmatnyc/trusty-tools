//! The `state-archive.py` port: move old handoff sections out of `state.md` (#12028 P1e).
//!
//! Why: the Architect's `state.md` grows by stacked handoff sections; this
//! command moves all but the newest few to the top of `state-archive.md`
//! without touching any other section.
//! What: [`run`] parses the script's argv and does what `run()` and `main()`
//! do. A section is a `## ` line through the next one (lines split as
//! `str.splitlines` splits them); a handoff section's heading contains
//! `handoff` (any case) or `UPDATE`. The first `--keep` handoff sections stay,
//! the rest move in file order to the top of the archive body. A move that
//! would drop the only `NEXT IDS` line is refused (exit 1, nothing written).
//! Both files are rewritten in place, the archive first, as the script does;
//! a failed read or write ends with the traceback's last line on stderr and
//! exit 1, never a success line.
//! Test: the `state-archive` recordings in `tests/replay_writers.rs`,
//! `shipped_state_archive_moves_sections_in_the_working_directory`.

use std::path::{Path, PathBuf};

use super::{Outcome, ReadError, outcome, read_text};
use crate::argparse::{self, HELP, Opt, Parsed};
use crate::poller::pyfmt::py_int_text;
use crate::pystr::{is_space, splitlines};

/// The archive's first line, which the script keeps on top.
pub const ARCHIVE_HEAD: &str = "# State archive (handoff sections moved out of state.md by scripts/state-archive.py; newest first)\n";

const REFUSAL: &str = "state-archive: refusing, the move would remove the only NEXT IDS line\n";

const USAGE_REST: &str =
    "[-h] [--state STATE] [--archive ARCHIVE] [--keep KEEP]\n                        [--dry-run]";

const HELP_BODY: &str = "
state-archive.py: move old handoff/UPDATE sections out of local/state.md into
local/state-archive.md.

options:
  -h, --help         show this help message and exit
  --state STATE
  --archive ARCHIVE
  --keep KEEP
  --dry-run
";

const OPTS: [Opt; 5] = [
    HELP,
    Opt {
        long: "--state",
        short: None,
        value: true,
    },
    Opt {
        long: "--archive",
        short: None,
        value: true,
    },
    Opt {
        long: "--keep",
        short: None,
        value: true,
    },
    Opt {
        long: "--dry-run",
        short: None,
        value: false,
    },
];

/// Where the run reads its defaults.
#[derive(Debug, Clone)]
pub struct Options {
    /// The program name argparse prints.
    pub prog: String,
    /// `--state`'s default (`<checkout>/local/state.md`); `None` with no checkout.
    pub default_state: Option<PathBuf>,
    /// `--archive`'s default (`<checkout>/local/state-archive.md`); `None` with no checkout.
    pub default_archive: Option<PathBuf>,
}

/// Runs the script's command line on `args` (argv after the program name).
///
/// Why: the shipped binary and the replay entry point share one body (D6a).
/// What: argparse first (`--keep` is `type=int`), then the move; returns the
/// streams and exit code the script ends with.
/// Test: the `state-archive` recordings in `tests/replay_writers.rs`.
pub fn run(args: &[String], opts: &Options) -> Outcome {
    let usage = argparse::usage(&opts.prog, USAGE_REST);
    let check = |i: usize, v: &str| match i {
        3 if keep_value(v).is_none() => Err(format!(
            "argument --keep: invalid int value: {}",
            crate::pystr::py_repr(v)
        )),
        _ => Ok(()),
    };
    let a = match argparse::parse_checked(args, &OPTS, false, &check) {
        Parsed::Run(a) => a,
        Parsed::Help => return outcome(format!("{usage}{HELP_BODY}"), String::new(), 0),
        Parsed::Error(m) => {
            return outcome(String::new(), argparse::error(&usage, &opts.prog, &m), 2);
        }
    };
    let text_of = |i: usize, d: Option<&Path>| {
        a.values[i]
            .clone()
            .or_else(|| d.map(|d| d.to_string_lossy().into_owned()))
    };
    // #12073: a default needed with no checkout found is refused, never the cwd's.
    let (Some(state), Some(archive)) = (
        text_of(1, opts.default_state.as_deref()),
        text_of(2, opts.default_archive.as_deref()),
    ) else {
        return super::no_checkout(&opts.prog, "; pass --state and --archive");
    };
    let keep = a.values[3].as_deref().and_then(keep_value).unwrap_or(3);
    match move_sections(&state, &archive, keep, a.flags[4]) {
        Ok(Moved::Dry(report)) => outcome(report, String::new(), 0),
        Ok(Moved::Done(n)) => outcome(
            format!("moved {n} sections to {archive}\n"),
            String::new(),
            0,
        ),
        Err(Fail::Refused) => outcome(String::new(), REFUSAL.into(), 1),
        Err(Fail::Read(e, path)) => outcome(String::new(), format!("{}\n", e.last_line(&path)), 1),
        Err(Fail::Write(e, path)) => outcome(
            String::new(),
            format!("{}\n", super::os_error(&e, &path)),
            1,
        ),
    }
}

/// `int(v)`, saturated to `i64`: past it, every count compares the same.
fn keep_value(v: &str) -> Option<i64> {
    let t = py_int_text(v)?;
    Some(t.parse().unwrap_or(if t.starts_with('-') {
        i64::MIN
    } else {
        i64::MAX
    }))
}

enum Moved {
    /// `--dry-run`: the report it prints.
    Dry(String),
    /// The number of sections moved (and written).
    Done(usize),
}

enum Fail {
    Refused,
    Read(ReadError, String),
    Write(std::io::Error, String),
}

/// `str.splitlines(keepends=True)`.
fn lines_keepends(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in splitlines(text) {
        // Each piece is a subslice of `text`; its terminator runs to the next piece.
        let at = line.as_ptr() as usize - text.as_ptr() as usize;
        if at > start {
            out.push(&text[start..at]);
        }
        start = at;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// `NEXT_IDS_RE.search(text)`: `NEXT IDS:?\s+[GPADTS]\s*[0-9a-km-np-z]{2}`.
fn has_next_ids(text: &str) -> bool {
    let id = |c: char| c.is_ascii_digit() || (c.is_ascii_lowercase() && c != 'l' && c != 'o');
    text.match_indices("NEXT IDS").any(|(i, m)| {
        let mut c = text[i + m.len()..].chars().peekable();
        if c.peek() == Some(&':') {
            c.next();
        }
        let mut spaces = 0;
        while c.peek().is_some_and(|&x| is_space(x)) {
            c.next();
            spaces += 1;
        }
        if spaces == 0 || !c.next().is_some_and(|x| "GPADTS".contains(x)) {
            return false;
        }
        while c.peek().is_some_and(|&x| is_space(x)) {
            c.next();
        }
        c.next().is_some_and(id) && c.next().is_some_and(id)
    })
}

fn first_line(section: &str) -> &str {
    splitlines(section).first().copied().unwrap_or("")
}

fn is_handoff(section: &str) -> bool {
    let head = first_line(section);
    head.to_lowercase().contains("handoff") || head.contains("UPDATE")
}

fn move_sections(state: &str, archive: &str, keep: i64, dry: bool) -> Result<Moved, Fail> {
    let text = read_text(Path::new(state)).map_err(|e| Fail::Read(e, state.to_owned()))?;
    let lines = lines_keepends(&text);
    let heads: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].starts_with("## "))
        .collect();
    let (pre, secs): (String, Vec<String>) = match heads.first() {
        None => (text.clone(), Vec::new()),
        Some(&first) => {
            let mut bounds = heads.clone();
            bounds.push(lines.len());
            let secs = bounds
                .windows(2)
                .map(|w| lines[w[0]..w[1]].concat())
                .collect();
            (lines[..first].concat(), secs)
        }
    };
    let mut seen = 0i64;
    let moved: Vec<bool> = secs
        .iter()
        .map(|s| {
            if is_handoff(s) {
                seen += 1;
                seen > keep
            } else {
                false
            }
        })
        .collect();
    let mut new_state = pre;
    let mut move_text = String::new();
    let mut moves: Vec<&str> = Vec::new();
    for (s, m) in secs.iter().zip(&moved) {
        if *m {
            moves.push(s);
            move_text.push_str(s);
        } else {
            new_state.push_str(s);
        }
    }
    if has_next_ids(&text) && !has_next_ids(&new_state) {
        return Err(Fail::Refused);
    }
    if dry {
        let mut out = String::new();
        for s in &moves {
            let head: String = first_line(s).chars().take(100).collect();
            out += &format!("would move {} bytes: {head}\n", s.len());
        }
        out += &format!(
            "would move {} sections; state.md {} -> {} bytes\n",
            moves.len(),
            text.len(),
            new_state.len()
        );
        return Ok(Moved::Dry(out));
    }
    if moves.is_empty() {
        return Ok(Moved::Done(0));
    }
    let old = match read_text(Path::new(archive)) {
        Ok(t) => t,
        Err(ReadError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Fail::Read(e, archive.to_owned())),
    };
    let body = old.strip_prefix(ARCHIVE_HEAD).unwrap_or(&old);
    let joined = format!("{ARCHIVE_HEAD}{move_text}{body}");
    std::fs::write(archive, joined).map_err(|e| Fail::Write(e, archive.to_owned()))?;
    std::fs::write(state, new_state).map_err(|e| Fail::Write(e, state.to_owned()))?;
    Ok(Moved::Done(moves.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_ids_and_keepends_match_python() {
        // Expected values from Python 3.14 `re.search` and `splitlines(True)`.
        assert!(has_next_ids("x NEXT IDS: G 3a"));
        assert!(has_next_ids("NEXT IDS\tT\u{2003}zz"));
        assert!(!has_next_ids("NEXT IDS: G l3"));
        assert!(!has_next_ids("NEXT IDS:G 3a"));
        assert_eq!(
            lines_keepends("a\r\nb\u{2028}c"),
            ["a\r\n", "b\u{2028}", "c"]
        );
        assert_eq!(lines_keepends("a\n\n"), ["a\n", "\n"]);
    }
}
