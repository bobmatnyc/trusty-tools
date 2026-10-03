//! Self-location idioms in a shell script body, resolved to literal paths
//! for `pm_guard_secret_script`'s nested-run judgement (#9037), split out for
//! the 500-SLOC cap.
//!
//! Why: #9037 makes a nested script the guard cannot resolve refuse. Shell
//! scripts locate their helpers through their own path —
//! `. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib/x.sh"`,
//! `exec bash "$0" "$@"` — so those idioms resolve to the file they name and
//! that file is judged, instead of refusing.
//! What: [`resolve_self_paths`] rewrites the body text; anything it cannot
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
/// `$(cd … && pwd)` over a literal, then each variable assigned exactly once
/// from a literal and never otherwise bound, until nothing changes. A script
/// path that is not one plain word resolves nothing.
/// Test: `self_location_idioms_resolve_9037`.
pub(crate) fn resolve_self_paths(body: &str, script: &Path) -> String {
    let own = script.display().to_string();
    if !plain(&own) {
        return body.to_string();
    }
    let mut text = SELF_PATH.replace_all(body, own.as_str()).into_owned();
    for _ in 0..MAX_ROUNDS {
        let next = resolve_round(&text);
        if next == text {
            break;
        }
        text = next;
    }
    text
}

/// One rewrite round over `text`.
fn resolve_round(text: &str) -> String {
    let text = DIRNAME.replace_all(text, |c: &Captures<'_>| {
        let parent = Path::new(&c[1]).parent().map(Path::to_path_buf);
        match parent {
            Some(dir) if dir.as_os_str().is_empty() => ".".to_string(),
            Some(dir) => dir.display().to_string(),
            None => c[0].to_string(),
        }
    });
    let text = CD_PWD.replace_all(&text, |c: &Captures<'_>| {
        let dir = Path::new(&c[1]);
        if dir.is_absolute() {
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
