//! Self-location idioms in a shell script body, resolved to literal paths
//! for `pm_guard_secret_script`'s nested-run judgement (#9037), split out for
//! the 500-SLOC cap.
//!
//! Why: #9037 makes a nested script the guard cannot resolve refuse. Shell
//! scripts locate their helpers through their own path —
//! `. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/x.sh"`,
//! `exec bash "$0" "$@"` — so those idioms resolve to the file they name and
//! that file is judged, instead of refusing.
//! What: [`resolve_self_paths_with`] rewrites the body text; anything it cannot
//! pin to one literal path stays computed, and the caller refuses it.
//! Test: `self_location_idioms_resolve_9037`, `the_repo_gate_scripts_allow_8879`.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// Rewrite rounds: a variable may be defined from another (`REPO_ROOT` from
/// `SCRIPT_DIR`), each round resolves one more layer.
const MAX_ROUNDS: usize = 8;

/// The script's own path: `${BASH_SOURCE[0]}`, `$BASH_SOURCE`, `$0`.
static SELF_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{BASH_SOURCE(?:\[0\])?\}|\$BASH_SOURCE\b|\$\{0\}|\$0").expect("self regex")
});

/// `$(dirname <literal>)`, the operand optionally double-quoted.
static DIRNAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\$\(\s*dirname\s+(?:--\s+)?"?([^"$`'()\s;&|<>\\]+)"?\s*\)"#)
        .expect("dirname regex")
});

/// `$(cd <literal> && pwd)`, with `-P`/`-L` and a `/dev/null` redirect.
static CD_PWD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\$\(\s*cd\s+(?:-[PL]\s+)?(?:--\s+)?"?([^"$`'()\s;&|<>\\]+)"?(?:\s+[0-9]?>(?:&[0-9]|\s*/dev/null))*\s*(?:&&|;)\s*pwd(?:\s+-[PL])?\s*\)"#,
    )
    .expect("cd regex")
});

/// `$(git rev-parse --show-toplevel)`, with an optional literal `-C <dir>`
/// (#10566 round 2: the gate scripts run a helper from `$REPO_ROOT`).
static GIT_TOPLEVEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\$\(\s*git\s+(?:-C\s+"?([^"$`'()\s;&|<>\\]+)"?\s+)?rev-parse\s+--show-toplevel\s*\)"#,
    )
    .expect("toplevel regex")
});

/// A word that may move the directory a bare `git` starts from: `cd`,
/// `pushd`, `popd`, a sourced file, an `eval`, or an ANSI-C `$'\…'` string
/// that can spell any word (#10622).
static MOVES_DIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s;&|(`{!])(?:cd|pushd|popd|source|\.|eval)(?:[\s;&|)`}]|$)|\$'[^']*\\")
        .expect("cd word regex")
});

/// A git environment override, or a redefinition of what `git` runs: a
/// function, an alias, a command-table entry, `hash`/`enable`, an exported
/// function, or a new `PATH` (#10622). zsh's `path` array is [`ZSH_PATH`].
/// A shell start-up file (`BASH_ENV`, `ENV`, `ZDOTDIR`, `--rcfile`,
/// `--init-file`) runs code before the script, so it counts too (#10622).
static REDEFINES_GIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\bGIT_[A-Z0-9_]*|\bfunction\s+git\b|\bgit\s*\(\s*\)|\balias\b[^\n]*\bgit=|\[git\]|\b(?:hash|enable)\b[^\n]*\bgit\b|\bexport\s+-f\b|\bPATH\+?[=\[]|\b(?:BASH_ENV|ENV|ZDOTDIR)\+?=|--(?:rcfile|init-file)\b",
    )
    .expect("git redefinition regex")
});

/// zsh's `path` array, tied to `PATH`; in bash it is an ordinary name.
static ZSH_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bpath\+?[=\[]").expect("zsh path regex"));

/// An opener after which code may run later or again: a loop, a function,
/// a brace group, a `trap` or `eval` string, an alias body.
static DEFERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[\s;&|(`{!])(?:do|function|trap|eval|alias|while|until|for|select)(?:[\s;&|)`}]|$)|\(\s*\)|(?:^|[^$])\{",
    )
    .expect("defer regex")
});

/// What may make a script's `$(git rev-parse --show-toplevel)` answer for
/// another checkout than the one above it (#10622), read once per body.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ToplevelRisk {
    /// A `GIT_*` override or a redefinition of `git`, here or in a parent.
    redefined: bool,
    /// The first line that may change directory.
    first_move: Option<usize>,
    /// The first line that opens code which may run later or again.
    first_defer: Option<usize>,
}

impl ToplevelRisk {
    /// The risks `body` carries, with `inherited` set when a parent command
    /// or script may have changed git's environment. `zsh` counts `path`.
    ///
    /// What: one pass over the body's non-comment lines with every quote and
    /// backslash removed, so `"cd"`, `c\d` and `'GIT_DIR'=x` read as written
    /// for the shell.
    pub(crate) fn scan(body: &str, inherited: bool, zsh: bool) -> Self {
        let mut risk = Self {
            redefined: inherited,
            ..Self::default()
        };
        for (n, line) in body.split('\n').enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            let ansi = MOVES_DIR.is_match(line);
            let code = line.replace(['\'', '"', '\\'], "");
            risk.redefined |= REDEFINES_GIT.is_match(&code) || (zsh && ZSH_PATH.is_match(&code));
            if ansi || MOVES_DIR.is_match(&code) {
                risk.first_move.get_or_insert(n);
            }
            if DEFERS.is_match(&code) {
                risk.first_defer.get_or_insert(n);
            }
        }
        risk
    }

    /// Whether the scripts this body runs inherit a git change: a `GIT_*`
    /// override or a redefinition here or in a parent.
    pub(crate) fn passes_on(&self) -> bool {
        self.redefined
    }

    /// Whether an idiom on line `line` may answer for another checkout. A
    /// bare idiom (`bare`) also counts a directory change: any one at or
    /// before it, or any one at all when it sits after a deferring opener.
    fn may_differ(&self, line: usize, bare: bool) -> bool {
        let deferred = self.first_defer.is_some_and(|d| d <= line);
        self.redefined || (bare && self.first_move.is_some_and(|m| m <= line || deferred))
    }
}

/// Whether `text` may change git's environment for the scripts it runs: a
/// `GIT_*` assignment or a redefinition of `git` anywhere in it (#10622).
pub(crate) fn passes_on_git_changes(text: &str, zsh: bool) -> bool {
    ToplevelRisk::scan(text, false, zsh).redefined
}

/// The nearest ancestor of `dir` that holds a `.git` entry.
fn toplevel_of(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|a| a.join(".git").exists())
        .map(Path::to_path_buf)
}

/// A whole-line assignment of a literal: `NAME="value"`, `NAME=value`,
/// optionally `export`/`readonly`/`local`.
static ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?m)^[ \t]*(?:(?:export|readonly|local)[ \t]+)?([A-Za-z_][A-Za-z0-9_]*)=(?:"([^"$`\\]*)"|([^\s"'$`\\;&|()<>]+))[ \t]*;?[ \t]*$"#,
    )
    .expect("assign regex")
});

/// Whether `value` can stand in shell text as one plain word.
fn plain(value: &str) -> bool {
    !value.is_empty()
        && !value.contains(|c: char| c.is_whitespace() || "\"'$`\\;&|()<>".contains(c))
}

/// `path` with `.` dropped and `..` folded lexically, as `cd` does.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `body` with each self-location idiom replaced by the literal path it
/// names, given that `script` is the file the body was read from.
///
/// Why: see the module doc — strict nested resolution (#9037) must not
/// refuse a script that sources a helper beside itself.
/// What: replaces the script's own-path words, then `$(dirname …)` and
/// `$(cd … && pwd)` over a literal, `$(git rev-parse --show-toplevel)`
/// (#10566), then each variable assigned exactly once
/// from a literal and never otherwise bound, until nothing changes. A script
/// path that is not one plain word resolves nothing.
/// Test: `self_location_idioms_resolve_9037`,
/// `a_git_toplevel_idiom_resolves_10566`.
#[cfg(test)]
pub(crate) fn resolve_self_paths(body: &str, script: &Path) -> String {
    resolve_self_paths_with(body, script, ToplevelRisk::scan(body, false, false))
}

/// The self-location rewrite with the body's [`ToplevelRisk`], read once by
/// the caller with what the parents pass on (#10622).
/// Test: `a_toplevel_inherits_the_invoking_git_environment_10622`.
pub(crate) fn resolve_self_paths_with(body: &str, script: &Path, risk: ToplevelRisk) -> String {
    let own = script.display().to_string();
    if !plain(&own) {
        return body.to_string();
    }
    let mut text = SELF_PATH.replace_all(body, own.as_str()).into_owned();
    let home = script.parent().unwrap_or(Path::new("."));
    for _ in 0..MAX_ROUNDS {
        let next = resolve_toplevel(&resolve_round(&text), home, &risk);
        if next == text {
            break;
        }
        text = next;
    }
    text
}

/// `text` with each `$(git rev-parse --show-toplevel)` replaced by the
/// checkout root above its literal absolute `-C` directory, or above `home`
/// (the script's directory) when it names none; any other stays (#10566).
///
/// #10622: git answers from the run-time directory and environment, so an
/// idiom `risk` says may differ ([`ToplevelRisk::may_differ`]) stays
/// unresolved, and the caller refuses it. Each match costs its own bytes:
/// the line count advances from the previous match.
/// Test: `a_toplevel_after_a_cwd_or_git_change_stays_unresolved_10622`,
/// `a_toplevel_run_later_or_spelled_in_quotes_stays_unresolved_10622`,
/// `a_body_full_of_toplevel_idioms_resolves_in_linear_time_10622`,
/// `a_shell_startup_file_counts_as_a_git_or_cwd_change_10622`.
fn resolve_toplevel(text: &str, home: &Path, risk: &ToplevelRisk) -> String {
    let (mut at, mut line) = (0usize, 0usize);
    GIT_TOPLEVEL
        .replace_all(text, |c: &Captures<'_>| {
            let start = c.get(0).map_or(at, |m| m.start());
            line += text.get(at..start).map_or(0, |s| s.matches('\n').count());
            at = start;
            // A match across lines stays, so later line numbers hold.
            if c[0].contains('\n') || risk.may_differ(line, c.get(1).is_none()) {
                return c[0].to_string();
            }
            let dir = c.get(1).map_or(home, |m| Path::new(m.as_str()));
            let root = dir.is_absolute().then(|| toplevel_of(dir)).flatten();
            root.filter(|r| plain(&r.display().to_string()))
                .map_or_else(|| c[0].to_string(), |r| r.display().to_string())
        })
        .into_owned()
}

/// One rewrite round over `text`.
fn resolve_round(text: &str) -> String {
    let text = DIRNAME.replace_all(text, |c: &Captures<'_>| {
        let parent = Path::new(&c[1]).parent().map(Path::to_path_buf);
        match parent {
            // #10622: a match across lines stays, so line numbers hold.
            _ if c[0].contains('\n') => c[0].to_string(),
            Some(dir) if dir.as_os_str().is_empty() => ".".to_string(),
            Some(dir) => dir.display().to_string(),
            None => c[0].to_string(),
        }
    });
    let text = CD_PWD.replace_all(&text, |c: &Captures<'_>| {
        let dir = Path::new(&c[1]);
        if dir.is_absolute() && !c[0].contains('\n') {
            normalize(dir).display().to_string()
        } else {
            c[0].to_string()
        }
    });
    let mut text = text.into_owned();
    for (name, value) in single_literal_assignments(&text) {
        let refs = Regex::new(&format!(r"\$\{{{name}\}}|\${name}\b")).expect("ref regex");
        text = refs.replace_all(&text, value.as_str()).into_owned();
    }
    text
}

/// Each variable `text` binds exactly once, by a whole-line literal
/// assignment, and mentions nowhere else as a bare word (no `for NAME`, no
/// `read NAME`, no second assignment), with its plain value.
fn single_literal_assignments(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for c in ASSIGN.captures_iter(text) {
        let name = &c[1];
        let value = c.get(2).or_else(|| c.get(3)).map_or("", |m| m.as_str());
        if !plain(value) || out.iter().any(|(n, _): &(String, String)| n == name) {
            continue;
        }
        let bare = Regex::new(&format!(r"(?:^|[^$\w{{]){name}\b")).expect("bare regex");
        if bare.find_iter(text).count() == 1 {
            out.push((name.to_string(), value.to_string()));
        }
    }
    out
}
