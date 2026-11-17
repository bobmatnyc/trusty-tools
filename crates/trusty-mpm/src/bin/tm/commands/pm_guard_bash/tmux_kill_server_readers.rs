//! The options that turn a known-inert program or a reader into a writer or a
//! runner (#10706 H1).
//!
//! Why: the walk let a kill word through when it sat in an inert program's
//! argument or was piped only into readers. Code-critic r2 (ba1eb15878) found
//! readers that write or run: `rg --pre sh`, `sort -o x.sh`, `uniq - x.sh`,
//! `sed -n '1w x.sh'`, `sed -n '1e tmux kill-server'`.
//! What: [`reader_is_safe`] refuses `rg --pre`/`--pre-glob`, `sort -o`/
//! `--output`/`--compress-program`, `uniq` with an output operand, and `less
//! -o`/`-O`/`--log-file`. [`sed_prints_only`] accepts a `sed` whose every
//! script command is an address and `p`, or an `s` whose flags are `g`, `p`,
//! `i`, `I`, `m`, `M` or a count; anything it does not parse is refused.
//! Test: `deny_rg_pre_execution`, `deny_sort_output_file`,
//! `deny_uniq_second_file`, `deny_less_log_file`, `deny_sed_write_or_execute`,
//! `safe_reader_programs_allowed`.

/// Whether `program` (lower-cased) with `args` neither writes a file nor runs
/// a program its arguments name. A program this module has no rule for is safe.
pub(super) fn reader_is_safe(program: &str, args: &[String]) -> bool {
    match program {
        "rg" => !args
            .iter()
            .any(|a| a == "--pre" || a.starts_with("--pre=") || a.starts_with("--pre-glob")),
        "sort" => !args.iter().any(|a| {
            short_has(a, &['o']) || long_is(a, "output") || long_is(a, "compress-program")
        }),
        "less" => !args
            .iter()
            .any(|a| short_has(a, &['o', 'O']) || long_is(a, "log-file") || long_is(a, "LOG-FILE")),
        "uniq" => uniq_operands(args) < 2,
        _ => true,
    }
}

/// Whether `arg` is a short-option cluster (`-abc`) holding one of `letters`.
fn short_has(arg: &str, letters: &[char]) -> bool {
    arg.len() > 1 && arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains(letters)
}

/// Whether `arg` is `--<name>` or a GNU-getopt abbreviation of it, with or
/// without `=value`.
fn long_is(arg: &str, name: &str) -> bool {
    arg.strip_prefix("--")
        .map(|rest| rest.split('=').next().unwrap_or(rest))
        .is_some_and(|part| !part.is_empty() && name.starts_with(part))
}

/// The operands `uniq` reads (`INPUT`, then `OUTPUT`), counting `-`.
fn uniq_operands(args: &[String]) -> usize {
    let mut operands = 0;
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        if arg == "--" {
            return operands + words.count();
        }
        if arg == "-" || !arg.starts_with('-') {
            operands += 1;
        } else if arg.starts_with("--") {
            let takes = ["skip-fields", "skip-chars", "check-chars"];
            if !arg.contains('=') && takes.iter().any(|n| long_is(arg, n)) {
                words.next();
            }
        } else if let Some(at) = arg[1..].find(['f', 's', 'w']) {
            // `-f N`: the value is the rest of the cluster or the next word.
            if arg[1 + at + 1..].is_empty() {
                words.next();
            }
        }
    }
    operands
}

/// Whether a `sed` with `args` only prints: `-n`, no `-i` or `-f`, every
/// option one the reader knows, and every script [`script_prints_only`].
pub(super) fn sed_prints_only(args: &[String]) -> bool {
    let mut quiet = false;
    let mut scripts = Vec::new();
    let mut operand = None;
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        if arg == "--" {
            operand = operand.or(words.next());
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = long.split_once('=').unwrap_or((long, ""));
            let is = |full: &str| !name.is_empty() && full.starts_with(name);
            if is("expression") {
                let script = if long.contains('=') {
                    Some(value)
                } else {
                    words.next().map(String::as_str)
                };
                scripts.push(script.unwrap_or_default().to_string());
            } else if is("file") || is("in-place") {
                return false;
            } else if is("line-length") {
                if !long.contains('=') {
                    words.next();
                }
            } else if is("quiet") || is("silent") {
                quiet = true;
            } else if !SED_FLAGS.iter().any(|f| is(f)) {
                return false;
            }
            continue;
        }
        if arg == "-" || !arg.starts_with('-') {
            operand = operand.or(Some(arg));
            continue;
        }
        let cluster = &arg[1..];
        for (k, c) in cluster.char_indices() {
            let rest = &cluster[k + 1..];
            match c {
                'n' => quiet = true,
                'E' | 'r' | 's' | 'u' | 'z' => {}
                'e' | 'l' => {
                    let value = if rest.is_empty() {
                        words.next().map(String::as_str)
                    } else {
                        Some(rest)
                    };
                    if c == 'e' {
                        scripts.push(value.unwrap_or_default().to_string());
                    }
                    break;
                }
                _ => return false,
            }
        }
    }
    if scripts.is_empty() {
        scripts.extend(operand.cloned());
    }
    quiet && !scripts.is_empty() && scripts.iter().all(|s| script_prints_only(s))
}

/// GNU `sed` long options that neither take a value nor change what runs.
const SED_FLAGS: &[&str] = &[
    "debug",
    "posix",
    "regexp-extended",
    "separate",
    "sandbox",
    "unbuffered",
    "null-data",
    "zero-terminated",
    "follow-symlinks",
    "binary",
];

/// Whether every command of `script` is `[address[,address]][!]p` or
/// `[address[,address]][!]s/re/text/flags` with flags in `gpiImM0-9`.
fn script_prints_only(script: &str) -> bool {
    let b = script.as_bytes();
    let mut i = 0;
    loop {
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b';') {
            i += 1;
        }
        if i == b.len() {
            return true;
        }
        let Some(at) = address(b, i) else {
            return false;
        };
        i = at;
        if b.get(i) == Some(&b',') {
            let Some(at) = address(b, i + 1).filter(|&at| at > i + 1) else {
                return false;
            };
            i = at;
        }
        i = skip_blanks(b, i);
        if b.get(i) == Some(&b'!') {
            i = skip_blanks(b, i + 1);
        }
        i = match b.get(i) {
            Some(b'p') => i + 1,
            Some(b's') => match substitution(b, i + 1) {
                Some(at) => at,
                None => return false,
            },
            _ => return false,
        };
        i = skip_blanks(b, i);
        if i < b.len() && !matches!(b[i], b';' | b'\n') {
            return false;
        }
    }
}

/// The index after the address at `i` (`N`, `N~M`, `+N`, `~N`, `$`, `/re/`,
/// `\cREc`, a regex followed by `I`/`M`), `i` when none is there, or `None`
/// for a regex that does not close.
fn address(b: &[u8], mut i: usize) -> Option<usize> {
    match b.get(i) {
        Some(b'$') => return Some(i + 1),
        Some(b'/') => i = delimited(b, i + 1, b'/')?,
        Some(b'\\') => {
            let delim = *b.get(i + 1).filter(|d| !matches!(d, b'\\' | b'\n'))?;
            i = delimited(b, i + 2, delim)?;
        }
        Some(b'+' | b'~' | b'0'..=b'9') => {
            i += 1;
            while b.get(i).is_some_and(|c| c.is_ascii_digit() || *c == b'~') {
                i += 1;
            }
            return Some(i);
        }
        _ => return Some(i),
    }
    while matches!(b.get(i), Some(b'I' | b'M')) {
        i += 1;
    }
    Some(i)
}

/// The index of the first byte from `i` that is not a space or tab.
fn skip_blanks(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t')) {
        i += 1;
    }
    i
}

/// The index after `delim`'s next unescaped occurrence from `i`, or `None`.
fn delimited(b: &[u8], mut i: usize, delim: u8) -> Option<usize> {
    while let Some(&c) = b.get(i) {
        if c == b'\\' {
            i += 2;
            continue;
        }
        if c == delim {
            return Some(i + 1);
        }
        if c == b'\n' {
            return None;
        }
        i += 1;
    }
    None
}

/// The index after an `s` command's flags, `i` being just past the `s`, or
/// `None` for an unclosed command or a flag outside `gpiImM0-9` (`w`, `e`).
fn substitution(b: &[u8], i: usize) -> Option<usize> {
    let delim = *b.get(i).filter(|d| !matches!(d, b'\\' | b'\n'))?;
    let pattern_end = delimited(b, i + 1, delim)?;
    let mut at = delimited(b, pattern_end, delim)?;
    while b
        .get(at)
        .is_some_and(|c| matches!(c, b'g' | b'p' | b'i' | b'I' | b'm' | b'M') || c.is_ascii_digit())
    {
        at += 1;
    }
    Some(at)
}
