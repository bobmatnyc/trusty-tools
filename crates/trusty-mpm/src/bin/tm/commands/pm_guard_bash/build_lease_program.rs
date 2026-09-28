//! Which word of one simple command is a heavy build's program (#8261).
//!
//! Why: split out of `build_lease_rewrite` (line cap) when round 3 added the
//! per-wrapper option tables. The rewrite decides WHERE to insert
//! `tm build-lease --`; this module decides WHETHER a simple command builds,
//! and `tm build-lease` itself asks the same question of its own argv, so the
//! two can never disagree about what a heavy build is.
//! What: [`heavy_program_offset`] walks past keywords, `KEY=value`, and command
//! wrappers WITH their option values ([`wrapper_value_options`]) and `rustup
//! run <toolchain>`, then matches the program and subcommand against the heavy
//! table. [`find_exec_commands`] extracts the commands a `find -exec` runs.
//! Test: `wrappers_with_option_values_are_seen_through` in
//! `build_lease_rewrite`'s suite.

use crate::commands::hook_rewrite::{COMMAND_WRAPPERS, is_env_assignment};

/// Shell words that may precede a command without being it.
pub(super) const LEADING_KEYWORDS: &[&str] = &[
    "!", "{", "(", "if", "then", "else", "elif", "do", "while", "until",
];

/// Cargo global options that take a value before the subcommand.
const CARGO_VALUE_FLAGS: &[&str] = &["-C", "-Z", "--config", "--color"];

/// Options of a command wrapper that take the NEXT word as their value.
///
/// Why (#8261 round 3): a generic "skip flags" walk read `env -u NAME cargo`'s
/// `NAME`, `sudo -u u cargo`'s `u` and `timeout -s KILL 600 cargo`'s `KILL` as
/// the program, so those builds ran unleased.
/// Test: `wrappers_with_option_values_are_seen_through`.
fn wrapper_value_options(wrapper: &str) -> &'static [&'static str] {
    match wrapper {
        "sudo" => &[
            "-u",
            "-g",
            "-C",
            "-D",
            "-p",
            "-r",
            "-t",
            "-U",
            "-T",
            "-R",
            "--user",
            "--group",
            "--chdir",
            "--prompt",
            "--role",
            "--type",
            "--other-user",
            "--command-timeout",
            "--chroot",
            "--close-from",
        ],
        "doas" => &["-u", "-C"],
        "env" => &["-u", "--unset", "-C", "--chdir", "-P"],
        "nice" => &["-n", "--adjustment"],
        "timeout" => &["-s", "--signal", "-k", "--kill-after"],
        "ionice" => &["-c", "--class", "-n", "--classdata", "-p", "--pid"],
        "stdbuf" => &["-i", "-o", "-e", "--input", "--output", "--error"],
        "time" => &["-f", "--format", "-o", "--output"],
        "exec" => &["-a"],
        "caffeinate" => &["-t", "-w"],
        _ => &[],
    }
}

/// One word of a segment: its byte span and its unquoted text.
pub(super) struct Word {
    pub(super) start: usize,
    pub(super) text: String,
}

/// Split a segment into words, honouring quotes and backslashes.
pub(super) fn words(seg: &str) -> Vec<Word> {
    let bytes = seg.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let start = i;
        let mut quote: Option<u8> = None;
        while i < bytes.len() {
            let b = bytes[i];
            match quote {
                Some(q) if b == q => quote = None,
                Some(b'"') if b == b'\\' => i += 1,
                Some(_) => {}
                None if b.is_ascii_whitespace() => break,
                None if b == b'\'' || b == b'"' => quote = Some(b),
                None if b == b'\\' => i += 1,
                None => {}
            }
            i += 1;
        }
        let raw = &seg[start..i.min(bytes.len())];
        let text = shlex::split(raw)
            .map(|parts| parts.concat())
            .unwrap_or_else(|| raw.to_string());
        out.push(Word { start, text });
    }
    out
}

/// The byte offset where the lease goes in `seg`, if `seg` is a heavy build.
///
/// What: skips leading keywords and `(`, env assignments, command wrappers
/// with their flags, their option values and `timeout`'s duration, and
/// `rustup run [opts] <toolchain>`; then matches the program's basename and,
/// past `+toolchain` and cargo's global options, its subcommand. The lease goes
/// OUTSIDE `sudo`/`doas` (it runs as the caller and locks the caller's store)
/// and outside `rustup run`; otherwise directly before the program.
pub(super) fn heavy_program_offset(seg: &str, heavy: &[(String, Option<String>)]) -> Option<usize> {
    let words = words(seg);
    let mut idx = 0;
    let mut wrapper: Option<String> = None;
    let mut outside: Option<usize> = None;
    while let Some(word) = words.get(idx) {
        let text = word.text.as_str();
        let bare = text.trim_start_matches('(');
        let lead = text.len() - bare.len();
        if text == "case" {
            // `case WORD in` — the pattern label that follows is skipped below.
            idx = words
                .iter()
                .skip(idx)
                .position(|w| w.text == "in")
                .map_or(words.len(), |p| idx + p + 1);
            continue;
        }
        if LEADING_KEYWORDS.contains(&text) || bare.is_empty() || is_case_label(bare) {
            idx += 1;
            continue;
        }
        let base = bare.rsplit('/').next().unwrap_or(bare);
        if is_env_assignment(bare) || COMMAND_WRAPPERS.contains(&base) {
            if (base == "sudo" || base == "doas") && outside.is_none() {
                outside = Some(word.start + lead);
            }
            if COMMAND_WRAPPERS.contains(&base) {
                wrapper = Some(base.to_string());
            }
            idx += 1;
            continue;
        }
        if base == "rustup" && words.get(idx + 1).is_some_and(|w| w.text == "run") {
            // #8261 round 3: `rustup run <toolchain> cargo …`.
            outside.get_or_insert(word.start + lead);
            idx += 2;
            while words.get(idx).is_some_and(|w| w.text.starts_with('-')) {
                idx += 1;
            }
            idx += 1; // the toolchain
            wrapper = None;
            continue;
        }
        if let Some(w) = wrapper.as_deref() {
            if wrapper_value_options(w).contains(&bare) {
                idx += 2;
                continue;
            }
            if bare.starts_with('-') || bare.parse::<f64>().is_ok() || is_duration(bare) {
                idx += 1;
                continue;
            }
        }
        break;
    }
    let word = words.get(idx)?;
    let lead = word.text.len() - word.text.trim_start_matches('(').len();
    let program = word.text.trim_start_matches('(');
    let program = program.rsplit('/').next().unwrap_or(program);
    let sub = subcommand(&words[idx + 1..]);
    let is_heavy = heavy
        .iter()
        .any(|(p, s)| p == program && s.as_deref().is_none_or(|s| Some(s) == sub.as_deref()));
    (is_heavy && !invokes_no_compiler(program, sub.as_deref(), &words[idx + 1..]))
        .then_some(outside.unwrap_or(word.start + lead))
}

/// The commands a `find … -exec …` segment runs, re-joined, one per action.
///
/// Why (#8261 round 3): `find . -exec cargo test \;` runs cargo once per match
/// with no wrapper word the hook recognised, so it ran unleased.
/// What: for every `-exec`/`-execdir`/`-ok`/`-okdir`, the words up to `;` or
/// `+`, shell-joined. Empty when `seg` is not a `find`.
/// Test: `wrappers_with_option_values_are_seen_through`.
pub(super) fn find_exec_commands(seg: &str) -> Vec<String> {
    let words = words(seg);
    let is_find = words
        .iter()
        .find(|w| !is_env_assignment(&w.text) && !LEADING_KEYWORDS.contains(&w.text.as_str()))
        .is_some_and(|w| w.text.rsplit('/').next() == Some("find"));
    if !is_find {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut iter = words.iter().map(|w| w.text.as_str());
    while let Some(word) = iter.next() {
        if matches!(word, "-exec" | "-execdir" | "-ok" | "-okdir") {
            let argv: Vec<&str> = iter
                .by_ref()
                .take_while(|w| !matches!(*w, ";" | "+"))
                .collect();
            if let Ok(joined) = shlex::try_join(argv) {
                out.push(joined);
            }
        }
    }
    out
}

/// Flags that make `cargo` print and exit without compiling.
const CARGO_NO_COMPILE_FLAGS: &[&str] = &["--help", "-h", "--version", "-V"];

/// Whether a heavy verb's own arguments make it run no compiler (#8261).
///
/// Why: admission follows whether the command compiles, never the agent's
/// role (2026-09-25 report). `cargo build --help` or `cargo install --list`
/// compiles nothing, so it must not wait for a build slot. Round 3: the flags
/// are cargo's only — `mvn -V package` prints the version AND builds.
/// What: for `cargo`, `true` when a [`CARGO_NO_COMPILE_FLAGS`] word, or
/// `--list` after `install`, appears before any `--`. Every other program:
/// `false`.
/// Test: `non_compiling_invocations_of_heavy_verbs_are_left_alone`,
/// `no_compile_flags_apply_to_cargo_only`.
fn invokes_no_compiler(program: &str, sub: Option<&str>, rest: &[Word]) -> bool {
    program == "cargo"
        && rest
            .iter()
            .map(|w| w.text.as_str())
            .take_while(|w| *w != "--")
            .any(|w| {
                CARGO_NO_COMPILE_FLAGS.contains(&w) || (sub == Some("install") && w == "--list")
            })
}

/// A `case` arm's pattern label: `a)`, `(a)`, `*)`, `x|y)`.
fn is_case_label(word: &str) -> bool {
    word.ends_with(')') && !word.starts_with('$')
}

/// The subcommand after a program word, skipping `+toolchain` and global options.
fn subcommand(rest: &[Word]) -> Option<String> {
    let mut iter = rest.iter();
    while let Some(word) = iter.next() {
        let text = word.text.as_str();
        if text.starts_with('+') {
            continue;
        }
        if CARGO_VALUE_FLAGS.contains(&text) {
            iter.next();
            continue;
        }
        if text.starts_with('-') {
            continue;
        }
        return Some(text.trim_end_matches([')', ';', '}']).to_string());
    }
    None
}

/// `30s`, `5m`, `1h` — a `timeout` duration.
fn is_duration(word: &str) -> bool {
    word.strip_suffix(['s', 'm', 'h', 'd'])
        .is_some_and(|n| n.parse::<f64>().is_ok())
}
