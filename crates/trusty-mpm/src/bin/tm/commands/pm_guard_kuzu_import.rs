//! The program side of the #8625 kuzu-store rule: running `kuzu-memory`, the
//! one sanctioned route (`trusty-memory import kuzu`), its `--python` check,
//! and the deprecated aliases.
//!
//! Why: `kuzu-memory memory export` dumps raw rows without naming a store
//! path, so the path-word test alone cannot see it. Ruling Q8: "the exception
//! covers `trusty-memory import kuzu` only. The deprecated `migrate
//! kuzu-data` and `migrate kuzu-memory` aliases stay denied." Ruling Q9, as
//! accepted 14:09Z: "allow when --python absent, or lexical normalised path
//! (no symlink follow) is
//! <home>/.trusty-tools/kuzu-import-venv/bin/python|python3|python3.N AND
//! canonical(arg)==canonical(<venv>/bin/python); else deny with its own slug".
//! What: [`runs_kuzu_memory`], [`trusty_memory_verdict`] and
//! [`sanctioned_argument_indices`]; every failure to resolve a program word,
//! a path or an interpreter answers on the deny side.
//! Test: `pm_guard_kuzu_store_tests.rs` — `denies_running_kuzu_memory`,
//! `allows_the_venv_interpreter`, `denies_a_non_venv_python_program`,
//! `denies_a_planted_symlink_to_the_same_cpython`,
//! `denies_when_the_venv_interpreter_cannot_be_resolved`,
//! `denies_the_deprecated_migrate_aliases`, `the_exception_cannot_be_widened`.

use std::path::{Component, Path, PathBuf};

use crate::commands::pm_guard_kuzu_names::names_kuzu_module;
use crate::commands::pm_guard_secret_read::{NESTED_COMMAND_MARKERS, command_basename};
use crate::commands::program_word::resolve_program_word;

/// The program `kuzu-memory` installs.
const KUZU_PROGRAM: &str = "kuzu-memory";

/// The sanctioned program, and its deprecated `migrate` targets (ruling Q8).
const TRUSTY_MEMORY: &str = "trusty-memory";
const MIGRATE_ALIASES: &[&str] = &["kuzu-data", "kuzu-memory"];

/// Package runners that run a later argument as a program (`uvx kuzu-memory`).
const LAUNCHERS: &[&str] = &[
    "uvx",
    "uv",
    "pipx",
    "npx",
    "bunx",
    "pipenv",
    "poetry",
    "hatch",
    "rye",
    "pdm",
    "conda",
    "mamba",
    "micromamba",
    "pixi",
];

/// The import venv's `bin` directory under the home (ruling Q1, Q9).
const VENV_BIN: &str = ".trusty-tools/kuzu-import-venv/bin";

/// The reason slug of a `--python` deny (ruling Q9: "its own slug").
pub(crate) const PYTHON_SLUG: &str = "kuzu-import-python";

/// Whether `argv` runs the `kuzu-memory` program or its Python module.
///
/// What: the program word past wrappers, `xargs` included; a launcher's later
/// argument; a Python interpreter's `-m kuzu_memory`. A wrapper the resolver
/// cannot measure counts any `kuzu-memory` word (fail closed). A lookup
/// (`command -v kuzu-memory`) runs nothing.
/// Test: `denies_running_kuzu_memory`, `keeps_ordinary_commands_allowed`.
pub(crate) fn runs_kuzu_memory(argv: &[String]) -> bool {
    let start = match resolve_program_word(argv) {
        Ok(word) if word.lookup => return false,
        Ok(word) => word.index,
        Err(_) => return argv.iter().any(|t| is_kuzu_program(t)),
    };
    let Some(program) = argv.get(start) else {
        return false;
    };
    let base = command_basename(program).to_ascii_lowercase();
    let rest = &argv[start + 1..];
    if is_kuzu_program(program) {
        return true;
    }
    if LAUNCHERS.contains(&base.as_str()) {
        return rest.iter().any(|t| is_kuzu_program(t));
    }
    is_python(&base) && python_runs_kuzu_module(rest)
}

/// Whether `token` names the `kuzu-memory` program, a version pin included.
fn is_kuzu_program(token: &str) -> bool {
    let base = command_basename(token).to_ascii_lowercase();
    let name = base.split(['@', '=', '<', '>', '[', '~', '!']).next();
    name == Some(KUZU_PROGRAM)
}

/// Whether `base` is a Python interpreter (`python`, `python3.13`, `pypy3`).
fn is_python(base: &str) -> bool {
    ["python", "pypy"].iter().any(|stem| {
        base.strip_prefix(stem)
            .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
    })
}

/// Whether an interpreter's `args` carry `-m kuzu_memory` anywhere.
///
/// What: `-m kuzu_memory`, `-mkuzu_memory` and a short cluster ending in `m`
/// (`-Im kuzu_memory`). Any position counts, so an option whose value count
/// is unknown (`-X dev`) cannot hide it; a script argument spelled the same
/// way denies too, on the safe side.
fn python_runs_kuzu_module(args: &[String]) -> bool {
    args.iter().enumerate().any(|(i, arg)| {
        if !arg.starts_with('-') || arg.starts_with("--") {
            return false;
        }
        let attached = arg
            .strip_prefix("-m")
            .is_some_and(|m| !m.is_empty() && names_kuzu_module(m));
        attached || (arg.ends_with('m') && args.get(i + 1).is_some_and(|m| names_kuzu_module(m)))
    })
}

/// The deny a `trusty-memory` call earns on its own, whatever it names.
///
/// What: `migrate kuzu-data|kuzu-memory` denies outright (ruling Q8); an
/// `import kuzu` call denies when any `--python` value fails
/// [`venv_python_verdict`] (ruling Q9). Every `trusty-memory` word in `argv`
/// is checked, so a wrapper the resolver cannot read hides none.
/// Test: `denies_the_deprecated_migrate_aliases`,
/// `denies_a_non_venv_python_program`, `allows_the_venv_interpreter`.
pub(crate) fn trusty_memory_verdict(
    argv: &[String],
    cwd: &Path,
    home: Option<&Path>,
) -> Option<String> {
    let calls = argv
        .iter()
        .enumerate()
        .filter(|(_, t)| command_basename(t) == TRUSTY_MEMORY);
    for (at, _) in calls {
        let Some(sub) = argv[at + 1..].iter().position(|t| !t.starts_with('-')) else {
            continue;
        };
        let sub = at + 1 + sub;
        let after = &argv[sub + 1..];
        if argv[sub] == "migrate"
            && let Some(alias) = after.iter().find(|t| is_migrate_alias(t))
        {
            return Some(format!(
                "kuzu-store-read: `trusty-memory migrate {alias}` is a deprecated alias that \
                 reads kuzu-memory stores outside the sanctioned route (#8625, ruling Q8). \
                 Use `trusty-memory import kuzu` as the tm-memory-import skill describes."
            ));
        }
        if argv[sub] == "import" && after.first().is_some_and(|t| t == "kuzu") {
            for value in python_values(&after[1..]) {
                if let Err(why) = venv_python_verdict(value, cwd, home) {
                    return Some(python_reason(value.unwrap_or_default(), why));
                }
            }
        }
    }
    None
}

/// Whether `token` is a deprecated `migrate` target, in either spelling.
fn is_migrate_alias(token: &str) -> bool {
    let normal = token.to_ascii_lowercase().replace('_', "-");
    MIGRATE_ALIASES.contains(&normal.as_str())
}

/// Every `--python` value in `args`; `None` for a flag with no value.
fn python_values(args: &[String]) -> Vec<Option<&str>> {
    let mut out = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        if arg == "--python" {
            out.push(args.get(i + 1).map(String::as_str));
        } else if let Some(value) = arg.strip_prefix("--python=") {
            out.push(Some(value));
        }
    }
    out
}

/// Ruling Q9 for one `--python` value: `Ok` only for the venv interpreter.
///
/// What: the value, `~` and a relative path placed against `home` and `cwd`,
/// is normalised LEXICALLY (no symlink is followed) and must be
/// `<home>/.trusty-tools/kuzu-import-venv/bin/` plus `python`, `python3` or
/// `python3.N`; then its canonical path must equal the canonical
/// `<venv>/bin/python`. No home, no value, and either canonicalisation error
/// deny (#8625 Fail-Open Check).
/// Test: `allows_the_venv_interpreter`, `denies_a_non_venv_python_program`,
/// `denies_a_planted_symlink_to_the_same_cpython`,
/// `denies_when_the_venv_interpreter_cannot_be_resolved`.
pub(crate) fn venv_python_verdict(
    value: Option<&str>,
    cwd: &Path,
    home: Option<&Path>,
) -> Result<(), &'static str> {
    let value = value.ok_or("the flag has no value")?;
    let home = home.ok_or("the home directory is unknown")?;
    let venv_bin = lexical(&home.join(VENV_BIN));
    let path = if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        cwd.join(value)
    };
    let path = lexical(&path);
    let named = path.parent() == Some(venv_bin.as_path())
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_venv_python_name);
    if !named {
        return Err("it is not the import venv's bin/python, python3 or python3.N");
    }
    let arg = std::fs::canonicalize(&path).map_err(|_| "it cannot be resolved")?;
    let reference = std::fs::canonicalize(venv_bin.join("python"))
        .map_err(|_| "the venv's bin/python cannot be resolved")?;
    if arg == reference {
        Ok(())
    } else {
        Err("it does not resolve to the venv's bin/python")
    }
}

/// Whether `name` is `python`, `python3` or `python3.N` (N one or more digits).
fn is_venv_python_name(name: &str) -> bool {
    match name.strip_prefix("python3.") {
        Some(minor) => !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit()),
        None => name == "python" || name == "python3",
    }
}

/// `path` with `.` dropped and `..` applied, touching no file system.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The `--python` refusal, carrying [`PYTHON_SLUG`].
fn python_reason(value: &str, why: &str) -> String {
    format!(
        "{PYTHON_SLUG}: `trusty-memory import kuzu --python {value}` is refused because {why} \
         (#8625, ruling Q9). Omit --python, or pass \
         ~/.trusty-tools/kuzu-import-venv/bin/python."
    )
}

/// The argv indices of a sanctioned `trusty-memory import kuzu` call that
/// may name a store, a venv path or a temp export (ruling Q8).
///
/// What: empty unless the segment runs no nested command, its program word
/// resolves (not through `xargs`, not a lookup) to `trusty-memory`, and the
/// next two words are exactly `import kuzu`. Then every later argument, minus
/// a redirection operator and its target. A wrapper word or an env
/// assignment in front of the program is never exempt.
/// Test: `allows_the_sanctioned_import`, `the_exception_cannot_be_widened`.
pub(crate) fn sanctioned_argument_indices(segment: &str, argv: &[String]) -> Vec<usize> {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return Vec::new();
    }
    let Ok(word) = resolve_program_word(argv) else {
        return Vec::new();
    };
    let at = word.index;
    let sanctioned = !word.lookup
        && word.xargs_at.is_none()
        && argv
            .get(at)
            .is_some_and(|p| command_basename(p) == TRUSTY_MEMORY)
        && argv.get(at + 1).is_some_and(|t| t == "import")
        && argv.get(at + 2).is_some_and(|t| t == "kuzu");
    if !sanctioned {
        return Vec::new();
    }
    (at + 3..argv.len())
        .filter(|&i| !redirect_related(argv, i))
        .collect()
}

/// Whether token `i` is a redirection, or the target of one.
fn redirect_related(argv: &[String], i: usize) -> bool {
    let is_operator = |t: &str| {
        !t.is_empty()
            && t.contains(['<', '>'])
            && t.chars().all(|c| c.is_ascii_digit() || "&<>|".contains(c))
    };
    argv[i].contains(['<', '>']) || i.checked_sub(1).is_some_and(|p| is_operator(&argv[p]))
}
