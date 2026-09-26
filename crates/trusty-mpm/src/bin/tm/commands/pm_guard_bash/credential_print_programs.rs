//! What each program does with a credential, for the credential-print rule
//! (#8596, #8248).
//!
//! Why: the rule's verdict turns on a few program facts — which calls print a
//! value and on which descriptor, which readers print nothing, which programs
//! run text as code, and which settings echo expanded values (xtrace).
//! Keeping them apart from the flow scan keeps each list reviewable.
//! What: pure functions over a program basename and its argv.
//! Test: `credential_print_tests` (sibling module).

use super::super::shell_lex::DASH_C_SHELLS;

/// `security find-*-password` options that take a value (BSD getopt).
const SECURITY_OPTS_WITH_ARG: &[char] = &[
    'a', 'c', 'C', 'd', 'D', 'G', 'j', 'l', 'p', 'P', 'r', 's', 't',
];

/// grep options whose value is the rest of the cluster or the next word.
const GREP_OPTS_WITH_ARG: &[char] = &['e', 'f', 'm', 'A', 'B', 'C', 'd', 'D'];

/// rg options whose value is the rest of the cluster or the next word.
const RG_OPTS_WITH_ARG: &[char] = &[
    'e', 'f', 'g', 't', 'T', 'm', 'A', 'B', 'C', 'E', 'j', 'M', 'r',
];

/// Long grep/rg options taking the next word as their value.
const GREP_LONG_WITH_ARG: &[&str] = &[
    "--regexp",
    "--file",
    "--max-count",
    "--after-context",
    "--before-context",
    "--context",
    "--include",
    "--exclude",
    "--label",
    "--glob",
    "--type",
    "--replace",
];

/// Programs that run their argument or input as code (#8596 finding 5).
const EVALUATORS: &[&str] = &[
    "eval",
    "source",
    ".",
    "osascript",
    "ssh",
    "python",
    "python3",
    "perl",
    "ruby",
    "node",
    "deno",
    "php",
    "fish",
];

/// The basename of a program word, lowercased: APFS is case-insensitive, so
/// `SECURITY` runs `security`.
pub(super) fn basename(word: &str) -> String {
    let word = word.strip_prefix('\\').unwrap_or(word);
    word.rsplit('/').next().unwrap_or(word).to_ascii_lowercase()
}

/// Whether argv word `a` is `want`, ignoring ASCII case.
fn is(a: &str, want: &str) -> bool {
    a.eq_ignore_ascii_case(want)
}

/// Which descriptors a credential CLI call prints the value on: `(stdout, stderr)`.
///
/// What: `security find-{generic,internet}-password` reads its option
/// clusters getopt-style — `-w` puts the value on stdout, `-g` on stderr, and
/// a value-taking option ends the cluster; `security dump-keychain -d` prints
/// decrypted items on stdout. `gcloud` prints on stdout for `auth …
/// print-{access,identity}-token` and for `config config-helper` unless a
/// `--format` projection leaves out the credential.
pub(super) fn credential_fds(program: &str, args: &[String]) -> (bool, bool) {
    match program {
        "security" => security_fds(args),
        "gcloud" => {
            let token = args.iter().position(|a| is(a, "auth")).is_some_and(|at| {
                args[at + 1..]
                    .iter()
                    .any(|a| is(a, "print-access-token") || is(a, "print-identity-token"))
            });
            let helper = args.iter().position(|a| is(a, "config")).is_some_and(|at| {
                args[at + 1..].iter().any(|a| is(a, "config-helper"))
                    && config_helper_prints(&args[at + 1..])
            });
            (token || helper, false)
        }
        _ => (false, false),
    }
}

/// `security` subcommand options, getopt-style.
fn security_fds(args: &[String]) -> (bool, bool) {
    let Some(sub) = args.iter().position(|a| {
        is(a, "find-generic-password") || is(a, "find-internet-password") || is(a, "dump-keychain")
    }) else {
        return (false, false);
    };
    let dump = is(&args[sub], "dump-keychain");
    let (mut w, mut g) = (false, false);
    let mut rest = args[sub + 1..].iter();
    while let Some(tok) = rest.next() {
        let Some(cluster) = tok.strip_prefix('-').filter(|c| !c.starts_with('-')) else {
            continue;
        };
        for (i, ch) in cluster.char_indices() {
            match ch {
                'd' if dump => w = true,
                _ if dump => {}
                'w' => w = true,
                'g' => g = true,
                c if SECURITY_OPTS_WITH_ARG.contains(&c) => {
                    if i + c.len_utf8() == cluster.len() {
                        rest.next();
                    }
                    break;
                }
                _ => {}
            }
        }
    }
    (w, g)
}

/// Whether `gcloud config config-helper` prints its credential block.
///
/// What: yes by default (its YAML carries `credential.access_token`); a
/// `--format` with a `(…)` projection prints it only when the projection names
/// `credential` or `token`.
fn config_helper_prints(args: &[String]) -> bool {
    let mut format = None;
    for (n, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix("--format=") {
            format = Some(v.to_string());
        } else if a == "--format" {
            format = args.get(n + 1).cloned();
        }
    }
    match format {
        Some(f) if f.contains('(') => {
            let f = f.to_ascii_lowercase();
            f.contains("credential") || f.contains("token")
        }
        _ => true,
    }
}

/// The first word naming a credential CLI, wherever a wrapper put it
/// (`timeout 5 gcloud …`, `sudo -u x security …`) — the conservative reading.
pub(super) fn first_credential_program(argv: &[String]) -> Option<usize> {
    argv.iter()
        .position(|w| matches!(basename(w).as_str(), "security" | "gcloud"))
}

/// Whether a program reading a credential on stdin prints none of it.
///
/// What: `pbcopy`; `wc` whose options are only `-c`/`-l`/`-w`/`-m` clusters or
/// their long names; `grep`/`rg` quiet, read getopt-style so `-eq` is a
/// pattern, not `-q`; `--password-stdin` on `docker`/`podman`/`helm` login and
/// `--with-token` on `gh auth login`.
pub(super) fn consumes_stdin(program: &str, args: &[String]) -> bool {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let has_word = |w: &str| args.iter().any(|a| is(a, w));
    match program {
        "pbcopy" => true,
        "wc" => args.iter().all(|a| wc_flag_ok(a)),
        "grep" | "egrep" | "fgrep" => grep_quiet(args, GREP_OPTS_WITH_ARG),
        "rg" => grep_quiet(args, RG_OPTS_WITH_ARG),
        "docker" | "podman" | "helm" => has_word("login") && has("--password-stdin"),
        "gh" => has_word("auth") && has_word("login") && has("--with-token"),
        _ => false,
    }
}

/// A `wc` word that prints only counts.
fn wc_flag_ok(a: &str) -> bool {
    if let Some(long) = a.strip_prefix("--") {
        return matches!(long, "bytes" | "lines" | "words" | "chars" | "");
    }
    match a.strip_prefix('-') {
        Some(cluster) if !cluster.is_empty() => cluster.chars().all(|c| "clwm".contains(c)),
        // `-` is stdin; a file operand is read instead of stdin.
        _ => true,
    }
}

/// Whether grep-style `args` select quiet mode.
fn grep_quiet(args: &[String], with_arg: &[char]) -> bool {
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if a == "--" {
            return false;
        }
        if a == "--quiet" || a == "--silent" {
            return true;
        }
        if a.starts_with("--") {
            if GREP_LONG_WITH_ARG.contains(&a.as_str()) {
                i += 1;
            }
            continue;
        }
        let Some(cluster) = a.strip_prefix('-') else {
            continue;
        };
        for (at, ch) in cluster.char_indices() {
            if ch == 'q' {
                return true;
            }
            if with_arg.contains(&ch) {
                if at + ch.len_utf8() == cluster.len() {
                    i += 1;
                }
                break;
            }
        }
    }
    false
}

/// Whether a stage turns on xtrace, which prints every expanded value.
///
/// What: `set` with a short cluster holding `x` or `-o xtrace`; `setopt
/// xtrace`; a shell whose leading options hold `x` or `-o xtrace`; a
/// `SHELLOPTS=` word naming `xtrace`.
pub(super) fn enables_xtrace(program: &str, argv: &[String], args: &[String]) -> bool {
    if argv
        .iter()
        .any(|w| w.starts_with("SHELLOPTS=") && w.to_ascii_lowercase().contains("xtrace"))
    {
        return true;
    }
    match program {
        "set" => options_enable_xtrace(args),
        "setopt" => args
            .iter()
            .any(|a| a.to_ascii_lowercase().replace('_', "") == "xtrace"),
        p if DASH_C_SHELLS.contains(&p) || p == "fish" => options_enable_xtrace(args),
        _ => false,
    }
}

/// Leading `-x`-style clusters or `-o xtrace`, up to the first operand or `-c`
/// string.
fn options_enable_xtrace(args: &[String]) -> bool {
    let mut i = 0;
    while let Some(a) = args.get(i) {
        i += 1;
        if a == "-o" {
            if args.get(i).is_some_and(|v| v == "xtrace") {
                return true;
            }
            i += 1;
            continue;
        }
        let Some(cluster) = a.strip_prefix('-').filter(|c| !c.starts_with('-')) else {
            return false;
        };
        if cluster.contains('x') {
            return true;
        }
        if cluster.ends_with('c') {
            return false;
        }
    }
    false
}

/// Whether `program` runs text it is handed as code: an [`EVALUATORS`] entry,
/// or a shell reading its program from stdin or an argument.
pub(super) fn is_evaluator(program: &str) -> bool {
    EVALUATORS.contains(&program) || DASH_C_SHELLS.contains(&program)
}

/// Shell keywords that can precede the program word.
pub(super) const KEYWORDS: &[&str] = &[
    "!", "{", "}", "if", "then", "else", "elif", "do", "while", "until", "time",
];
