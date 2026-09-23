//! The programs a read-only dispatch may run, and their allowed shapes (#8439).
//!
//! Why: the rule names reads rather than writers, so each program here is
//! allowed only in a form that cannot write a file or a ref. A program absent
//! from [`check_command`]'s table is refused, as is a path or quoted spelling
//! of a listed one (`/usr/bin/sed`, `"s"ed`).
//! What: [`check_command`] judges one simple command's argv. The table:
//! - `cat`, `head`, `tail`, `wc`, `ls`, `grep`: any arguments — none of them
//!   has an option that writes a file.
//! - `rg`: any arguments but `--pre` and `--hostname-bin`, which run a program.
//! - `find`: only the primaries in [`FIND_FLAGS`] / [`FIND_VALUED`]; `-exec`,
//!   `-execdir`, `-ok`, `-delete`, `-fprint*` and `-fls` are absent.
//! - `sed`: flags `-n`, `-E`, `-r` and `-e`; every script only prints
//!   ([`sed_script_prints_only`]).
//! - `plutil -p` and `plutil -lint [-s]` over file operands.
//! - `defaults read [domain [key]]`; `launchctl print <target>`,
//!   `launchctl list [label]`.
//! - `tmux capture-pane` with `-p` and no `-b`.
//! - `cargo metadata` / `cargo tree` without `--config` or `-Z`.
//! - `git`: see [`super::read_only_git`].
//! - `echo`, `pwd`.
//!
//! A pipe stage after the first must be `cat`, `head`, `tail`, `wc`, `grep`,
//! `rg` or `sed`.
//! Test: `read_only_allow_tests::legitimate_reads_stay_allowed`,
//! `read_only_allow_tests::critic_round_three_probes_are_refused`.

use super::read_only_git::check_git;

/// One argument of a judged command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Arg {
    /// A literal; `bare` when no quote appeared in it.
    Lit { text: String, bare: bool },
    /// The `for` variable: an unknown value that never starts with `-`.
    Operand,
}

impl Arg {
    /// The literal text, if any.
    pub(super) fn text(&self) -> Option<&str> {
        match self {
            Arg::Lit { text, .. } => Some(text),
            Arg::Operand => None,
        }
    }

    /// A literal or the `for` variable that is not option-shaped.
    pub(super) fn is_operand(&self) -> bool {
        self.text().is_none_or(|t| !t.starts_with('-'))
    }
}

type Verdict = Result<(), String>;

/// Programs that read their input and print, accepted as a later pipe stage.
const PIPE_READERS: &[&str] = &["cat", "head", "tail", "wc", "grep", "rg", "sed"];

/// Programs with no option that writes, so any argument is accepted.
const PLAIN_READERS: &[&str] = &["cat", "head", "tail", "wc", "ls", "grep"];

/// Valueless `find` primaries and options that neither write nor execute.
const FIND_FLAGS: &[&str] = &[
    "-print",
    "-print0",
    "-prune",
    "-o",
    "-a",
    "-or",
    "-and",
    "-not",
    "-true",
    "-false",
    "-empty",
    "-depth",
    "-d",
    "-L",
    "-H",
    "-P",
    "-E",
    "-x",
    "-xdev",
    "-mount",
    "-follow",
    "-ls",
    "-quit",
    "-nouser",
    "-nogroup",
    "-readable",
    "-executable",
];

/// `find` primaries that take one value, none of which writes or executes.
const FIND_VALUED: &[&str] = &[
    "-name",
    "-iname",
    "-path",
    "-ipath",
    "-wholename",
    "-iwholename",
    "-regex",
    "-iregex",
    "-type",
    "-xtype",
    "-maxdepth",
    "-mindepth",
    "-mtime",
    "-mmin",
    "-atime",
    "-amin",
    "-ctime",
    "-cmin",
    "-newer",
    "-size",
    "-perm",
    "-user",
    "-group",
    "-uid",
    "-gid",
    "-links",
    "-inum",
    "-lname",
    "-ilname",
    "-printf",
    "-regextype",
];

/// Judge one simple command; `piped` when it reads a previous stage's output.
///
/// Why: see the module doc.
/// What: `Ok` when `args[0]` is a bare program name on the allowlist and the
/// rest fits its shape; `Err` otherwise.
/// Test: as the module doc.
pub(super) fn check_command(args: &[Arg], piped: bool) -> Verdict {
    let program = match args.first() {
        Some(Arg::Lit { text, bare: true }) => text.as_str(),
        _ => return Err("a program named by anything but a bare word".into()),
    };
    if piped && !PIPE_READERS.contains(&program) {
        return Err(format!("a pipe into `{program}`, which is not a reader"));
    }
    let rest = &args[1..];
    match program {
        p if PLAIN_READERS.contains(&p) => Ok(()),
        "rg" => rg(rest),
        "find" => find(rest),
        "sed" => sed(rest),
        "plutil" => plutil(rest),
        "defaults" => defaults(rest),
        "launchctl" => launchctl(rest),
        "tmux" => tmux(rest),
        "cargo" => cargo(rest),
        "git" => check_git(rest),
        "echo" => Ok(()),
        "pwd" if rest.is_empty() => Ok(()),
        _ => Err(format!(
            "`{program}`, which is not on the read-only allowlist"
        )),
    }
}

/// `rg` without the options that run a program.
fn rg(rest: &[Arg]) -> Verdict {
    let runs = rest
        .iter()
        .filter_map(Arg::text)
        .any(|t| t.starts_with("--pre") || t.starts_with("--hostname-bin"));
    if runs {
        return Err("an `rg` option that runs a program".into());
    }
    Ok(())
}

/// `find` with only the primaries that neither write nor execute.
fn find(rest: &[Arg]) -> Verdict {
    let mut i = 0;
    while let Some(arg) = rest.get(i) {
        match arg.text() {
            Some(t) if FIND_VALUED.contains(&t) => {
                if rest.get(i + 1).and_then(Arg::text).is_none() {
                    return Err(format!("`find {t}` without a literal value"));
                }
                i += 2;
            }
            Some(t) if t.starts_with('-') && !FIND_FLAGS.contains(&t) => {
                return Err(format!(
                    "`find {t}`, which is not on the read-only allowlist"
                ));
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// `sed` whose every script only prints.
fn sed(rest: &[Arg]) -> Verdict {
    let mut scripts = Vec::new();
    let mut i = 0;
    while let Some(arg) = rest.get(i) {
        match arg.text() {
            Some("-e") => {
                let script = rest.get(i + 1).and_then(Arg::text);
                scripts.push(script.ok_or("`sed -e` without a literal script")?);
                i += 2;
            }
            Some(t) if t.starts_with('-') && t.len() > 1 => {
                if t.starts_with("--") || !t[1..].chars().all(|c| matches!(c, 'n' | 'E' | 'r')) {
                    return Err(format!(
                        "`sed {t}`, which is not on the read-only allowlist"
                    ));
                }
                i += 1;
            }
            Some(t) if scripts.is_empty() => {
                scripts.push(t);
                i += 1;
            }
            _ if arg.is_operand() => i += 1,
            _ => return Err("a `sed` operand led by `-`".into()),
        }
    }
    if scripts.is_empty() || !scripts.iter().all(|s| sed_script_prints_only(s)) {
        return Err("a `sed` script that does more than print lines".into());
    }
    Ok(())
}

/// Is every command in a `sed` script an address range followed by `p`?
///
/// Why: `w`, `W`, `r`, `e` and `s///w` write files or run commands, and a
/// substitution flag list is its own grammar; only printing is allowed.
/// What: `;`-separated parts, each `[addr[,addr]]p`, where an address is
/// digits, `$`, or `/re/` with no `/` or `\` inside.
/// Test: `read_only_allow_tests::legitimate_reads_stay_allowed`.
fn sed_script_prints_only(script: &str) -> bool {
    script.split(';').all(|part| {
        let Some(addrs) = part.trim().strip_suffix('p') else {
            return false;
        };
        let mut addrs = addrs.splitn(2, ',');
        addrs.all(sed_address)
    })
}

/// A `sed` address: empty, digits, `$`, or `/re/` with a plain body.
fn sed_address(a: &str) -> bool {
    if a.is_empty() || a == "$" || a.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    a.strip_prefix('/')
        .and_then(|r| r.strip_suffix('/'))
        .is_some_and(|re| !re.contains('/') && !re.contains('\\'))
}

/// `plutil -p <files>` or `plutil -lint [-s] <files>`.
fn plutil(rest: &[Arg]) -> Verdict {
    let files = match rest.first().and_then(Arg::text) {
        Some("-p") => &rest[1..],
        Some("-lint") => {
            let quiet = rest.get(1).and_then(Arg::text) == Some("-s");
            &rest[if quiet { 2 } else { 1 }..]
        }
        _ => return Err("`plutil` in a form other than `-p` or `-lint`".into()),
    };
    if files.is_empty() || !files.iter().all(Arg::is_operand) {
        return Err("`plutil` with an option after its read verb".into());
    }
    Ok(())
}

/// `defaults read [domain [key]]`.
fn defaults(rest: &[Arg]) -> Verdict {
    let tail = &rest[rest.len().min(1)..];
    let domain_ok = tail
        .iter()
        .all(|a| a.is_operand() || a.text() == Some("-g"));
    if rest.first().and_then(Arg::text) != Some("read") || tail.len() > 2 || !domain_ok {
        return Err("`defaults` in a form other than `defaults read`".into());
    }
    Ok(())
}

/// `launchctl print <target>` or `launchctl list [label]`.
fn launchctl(rest: &[Arg]) -> Verdict {
    let tail = &rest[rest.len().min(1)..];
    let fits = match rest.first().and_then(Arg::text) {
        Some("print") => tail.len() == 1,
        Some("list") => tail.len() <= 1,
        _ => false,
    };
    if !fits || !tail.iter().all(Arg::is_operand) {
        return Err("`launchctl` in a form other than `print` or `list`".into());
    }
    Ok(())
}

/// `tmux capture-pane` that prints (`-p`) and fills no paste buffer.
fn tmux(rest: &[Arg]) -> Verdict {
    if rest.first().and_then(Arg::text) != Some("capture-pane") {
        return Err("`tmux` in a form other than `capture-pane -p`".into());
    }
    let mut printed = false;
    let mut i = 1;
    while let Some(arg) = rest.get(i) {
        let t = arg
            .text()
            .ok_or("a `tmux` argument that is not a literal")?;
        let Some(cluster) = t.strip_prefix('-').filter(|c| !c.is_empty()) else {
            return Err("a `tmux capture-pane` operand".into());
        };
        i += 1;
        for (k, c) in cluster.char_indices() {
            match c {
                'p' => printed = true,
                'a' | 'e' | 'C' | 'J' | 'N' | 'P' | 'q' | 'T' | 'M' => {}
                't' | 'S' | 'E' => {
                    // A valued flag takes the rest of the cluster, or the next word.
                    if k + 1 == cluster.len() {
                        rest.get(i)
                            .and_then(Arg::text)
                            .ok_or("`tmux -t/-S/-E` without a value")?;
                        i += 1;
                    }
                    break;
                }
                _ => return Err(format!("`tmux capture-pane -{c}`, which is not allowed")),
            }
        }
    }
    if !printed {
        return Err("`tmux capture-pane` without `-p`, which fills a paste buffer".into());
    }
    Ok(())
}

/// `cargo metadata` or `cargo tree` without a config override.
fn cargo(rest: &[Arg]) -> Verdict {
    let sub = rest.first().and_then(Arg::text);
    if !matches!(sub, Some("metadata" | "tree")) {
        return Err("`cargo` in a form other than `metadata` or `tree`".into());
    }
    let overrides = rest.iter().any(|a| {
        a.text()
            .is_none_or(|t| t.starts_with("--config") || t.starts_with("-Z"))
    });
    if overrides {
        return Err("a `cargo` config override or non-literal argument".into());
    }
    Ok(())
}
