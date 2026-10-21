//! The python-upload row of the #8878 D4-remainder floor (owner ruling
//! 2026-09-29 14:06Z).
//!
//! Why: `python -c` and `python - <<EOF` reach the network with no program
//! the other D4 rows name, so an upload through `urllib`, `http.client`,
//! `requests` or `socket` passed the floor.
//! What: [`python_upload_reason`] finds a `python*` program in a segment and
//! reads its code: the `-c` value, a here-document body, or a here-string.
//! Code that names a network module AND reads local content (a file, stdin,
//! the environment, a subprocess) denies; a plain download passes. `-c` code
//! whose stdin is a pipe or a `<` redirect, or that takes a `$(…)` argument,
//! reads local content. `-m` is judged by the module it runs; a
//! [`CODE_RUNNERS`] module's arguments are judged as `-c` code (#8878 S5).
//! FAIL-CLOSED: code the guard cannot inspect — a shell expansion inside it, a
//! dynamic import or `exec`/`eval`, code fed from a pipe or a `<` file, or a
//! command that does not lex — denies when it names a network module.
//! Residual: a script file the command runs (#8879), including one a
//! [`CODE_RUNNERS`] module runs, and a module named only through string
//! building.
//! Test: `a_python_upload_is_denied`, `python_without_an_upload_passes`,
//! `python_code_running_modules_judge_their_arguments`.

use super::{D4_REMEDY, program_positions};

/// `-m` modules that run their arguments as code (`timeit`, `pdb -c`) or run
/// a module their arguments name (`runpy X`, `cProfile -m X`, `trace
/// --module X`).
// #8878 S5: the PR 2c review HIGH — only the module name was judged.
const CODE_RUNNERS: &[&str] = &["timeit", "pdb", "cProfile", "profile", "trace", "runpy"];

/// Modules that open a network connection.
const NETWORK_MODULES: &[&str] = &[
    "urllib",
    "urllib2",
    "urllib3",
    "http.client",
    "httplib",
    "requests",
    "socket",
    "httpx",
    "aiohttp",
    "ftplib",
    "smtplib",
    "paramiko",
    "xmlrpc",
    "telnetlib",
    "pycurl",
    "websocket",
    "websockets",
    // #8878 PR 2c r2: stdlib calls that open a connection by themselves.
    "asyncio.open_connection",
    "ssl.create_connection",
];

/// Substrings that read local content into the process.
const LOCAL_MARKERS: &[&str] = &[
    "os.open(",
    "read_bytes(",
    "read_text(",
    "sys.stdin",
    "os.environ",
    "getenv(",
    "subprocess",
    "popen(",
    "glob(",
    "listdir(",
    "os.walk",
    "scandir(",
    "fileinput",
    "sendfile(",
    "Path(",
    // #8878 PR 2c: the other spellings of a file open.
    "io.open(",
    "FileIO(",
    "codecs.open(",
    // #8878 PR 2c r2: the other reads of stdin.
    "input(",
    "os.read(0",
    "sys.__stdin__",
];

/// Substrings that load or run code the guard cannot see.
const DYNAMIC_MARKERS: &[&str] = &["__import__", "importlib", "exec(", "eval(", "compile("];

/// The python-upload deny for one segment, or `None`.
///
/// What: see the module doc. `piped` says the segment's stdin is a pipe;
/// `command` is the whole command, searched when the code is unseen.
pub(super) fn python_upload_reason(text: &str, piped: bool, command: &str) -> Option<String> {
    let reason = || {
        Some(format!(
            "Hard-floor deny (#8878 D4): this `python` code names a network module and sends \
             local content, or cannot be inspected — network exfiltration. {D4_REMEDY}"
        ))
    };
    let (head, body) = heredoc_split(text);
    let Some(argv) = shlex::split(head) else {
        let python = words(text).any(|w| is_python(&w));
        return (python && names_network(command)).then(reason).flatten();
    };
    // #8878 PR 2c r2: `-c` code whose stdin or arguments the shell fills from
    // local content reads it, whatever it calls to do so.
    let fed = piped || argv.iter().any(|t| feeds_local(t));
    let seen = match launch(&argv)? {
        PyRun::Code(code) => Some((code, fed)),
        // #8878 PR 2c r2: `-m` runs that module; its arguments are its data —
        // except a code runner's (#8878 S5), which are code or a module name.
        PyRun::Module(module, args) => {
            let args = argv.get(args..).unwrap_or_default();
            if names_network(&module) || (runs_code(&module) && args.iter().any(|a| is_network(a)))
            {
                return reason();
            }
            if !runs_code(&module) {
                return None;
            }
            Some((args.join("\n"), fed))
        }
        // #8878 PR 2c: a `pipx run` package runs code the guard cannot see.
        PyRun::Unseen => None,
        PyRun::Other(tail) => other_run(&argv[tail..], body, piped)?.map(|code| (code, false)),
    };
    let deny = match seen {
        Some((code, fed)) => {
            names_network(&code)
                && (fed
                    || reads_local(&code)
                    || expands(&code)
                    || DYNAMIC_MARKERS.iter().any(|m| code.contains(m)))
        }
        None => names_network(command),
    };
    deny.then(reason).flatten()
}

/// How a python launch supplies its code.
enum PyRun {
    /// `-c CODE`.
    Code(String),
    /// `-m MODULE`, and the index in argv where the module's arguments start.
    Module(String, usize),
    /// A `pipx run` package: code the guard cannot see.
    Unseen,
    /// Neither: the argv after the interpreter starts at this index.
    Other(usize),
}

/// The python launch in `argv`: an interpreter in command position, or one
/// reached through `uv run [--]` / `pipx run` (#8878 PR 2c). For those, every
/// python-shaped token after `run` is tried, since a `uv` option value can
/// also be one; `pipx run` with no interpreter is a package run.
fn launch(argv: &[String]) -> Option<PyRun> {
    for (at, program) in program_positions(argv) {
        if is_python(&program) {
            return Some(python_run(argv, at + 1));
        }
        let runner = matches!(program.as_str(), "uv" | "pipx")
            && argv.get(at + 1).is_some_and(|t| t == "run");
        if !runner {
            continue;
        }
        let mut other = None;
        for i in at + 2..argv.len() {
            if !is_python(basename(&argv[i])) {
                continue;
            }
            match python_run(argv, i + 1) {
                PyRun::Other(tail) => other = other.or(Some(tail)),
                run => return Some(run),
            }
        }
        let module = argv[at + 2..]
            .iter()
            .position(|t| t == "-m" || t == "--module")
            .map(|m| {
                (
                    argv.get(at + 3 + m).cloned().unwrap_or_default(),
                    at + 4 + m,
                )
            });
        return match (other, program.as_str(), module) {
            (_, _, Some((module, args))) => Some(PyRun::Module(module, args)),
            (None, "pipx", _) => Some(PyRun::Unseen),
            (Some(tail), _, _) => Some(PyRun::Other(tail)),
            (None, _, _) => None,
        };
    }
    None
}

/// The code of a run that is neither `-c` nor `-m`: a here-document body, a
/// here-string, or unseen stdin. `None` (outer) for a script file (#8879).
fn other_run(tail: &[String], body: Option<(&str, bool)>, piped: bool) -> Option<Option<String>> {
    if let Some((body, quoted)) = body {
        return Some((quoted || !expands(body)).then(|| body.to_string()));
    }
    if let Some(code) = here_string(tail) {
        return Some(Some(code));
    }
    (piped || tail.iter().any(|t| t.starts_with('<'))).then_some(None)
}

/// A token's basename.
fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// The operator line and, when it opens a here-document, the body and
/// whether its delimiter is quoted (no expansion). Otherwise the whole text.
pub(super) fn heredoc_split(text: &str) -> (&str, Option<(&str, bool)>) {
    let Some((first, rest)) = text.split_once('\n') else {
        return (text, None);
    };
    let opens = first
        .match_indices("<<")
        .any(|(i, _)| !first[i..].starts_with("<<<") && (i == 0 || !first[..i].ends_with('<')));
    if !opens {
        return (text, None);
    }
    let quoted = first.contains("<<'")
        || first.contains("<<\"")
        || first.contains("<<-'")
        || first.contains("<<\\")
        || first.contains("<< '");
    (first, Some((rest, quoted)))
}

/// Whether a program basename is a python interpreter (`python`, `python3`,
/// `python3.12`, or the `py` launcher).
fn is_python(program: &str) -> bool {
    program == "py"
        || program
            .strip_prefix("python")
            .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// What the interpreter options starting at `argv[from]` run: `-c CODE`
/// (attached or in a cluster), `-m` (unseen), or neither (#8878 PR 2c:
/// `-W`, `-X` and `-Q` take a value, which is skipped, not read as a script).
fn python_run(argv: &[String], from: usize) -> PyRun {
    let mut i = from;
    while let Some(tok) = argv.get(i) {
        if tok == "-" || tok == "--" || !tok.starts_with('-') {
            break;
        }
        if let Some(long) = tok.strip_prefix("--") {
            i += if long == "check-hash-based-pycs" {
                2
            } else {
                1
            };
            continue;
        }
        let cluster = &tok[1..];
        let mut step = 1;
        for (at, c) in cluster.char_indices() {
            let rest = &cluster[at + c.len_utf8()..];
            match c {
                'c' if rest.is_empty() => {
                    return PyRun::Code(argv.get(i + 1).cloned().unwrap_or_default());
                }
                'c' => return PyRun::Code(rest.to_string()),
                'm' if rest.is_empty() => {
                    let module = argv.get(i + 1).cloned().unwrap_or_default();
                    return PyRun::Module(module, i + 2);
                }
                'm' => return PyRun::Module(rest.to_string(), i + 1),
                'W' | 'X' | 'Q' => {
                    step = if rest.is_empty() { 2 } else { 1 };
                    break;
                }
                _ => {}
            }
        }
        i += step;
    }
    PyRun::Other(i)
}

/// The here-string value (`<<< CODE` or `<<<CODE`).
fn here_string(tail: &[String]) -> Option<String> {
    tail.iter().enumerate().find_map(|(i, tok)| {
        let rest = tok.strip_prefix("<<<")?;
        if rest.is_empty() {
            tail.get(i + 1).cloned()
        } else {
            Some(rest.to_string())
        }
    })
}

/// Words of `text`: runs of alphanumerics, `_` and `.`.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
}

/// Whether `text` names a [`NETWORK_MODULES`] module (or a submodule),
/// directly or as a `from <pkg> import <mod>` pair.
fn names_network(text: &str) -> bool {
    words(text).any(|word| is_network(&word)) || from_imports(text).iter().any(|w| is_network(w))
}

/// Whether one word is a [`NETWORK_MODULES`] module or a submodule of one.
fn is_network(word: &str) -> bool {
    NETWORK_MODULES
        .iter()
        .any(|m| word == *m || word.starts_with(&format!("{m}.")))
}

/// Whether `module` is a [`CODE_RUNNERS`] module.
fn runs_code(module: &str) -> bool {
    CODE_RUNNERS.contains(&module)
}

/// Every `pkg.name` a `from pkg import name[, name]` statement binds (#8878
/// PR 2c: `from http import client` is `http.client`).
fn from_imports(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for statement in text.split(['\n', ';']) {
        let mut parts = statement.split_whitespace();
        if parts.next() != Some("from") {
            continue;
        }
        let (Some(pkg), Some("import")) = (parts.next(), parts.next()) else {
            continue;
        };
        let names: String = parts.collect::<Vec<_>>().join(" ");
        for name in names.split([',', '(', ')']) {
            let name = name.split_whitespace().next().unwrap_or_default();
            if !name.is_empty() {
                out.push(format!("{pkg}.{name}"));
            }
        }
    }
    out
}

/// Whether code reads local content: a bare `open(` or a [`LOCAL_MARKERS`] hit.
fn reads_local(code: &str) -> bool {
    let bare_open = code.match_indices("open(").any(|(i, _)| {
        code[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
    });
    bare_open || LOCAL_MARKERS.iter().any(|m| code.contains(m))
}

/// Whether a token outside the code feeds the process local content: a `<`
/// redirect or process substitution, or a `$(…)` / backtick argument.
fn feeds_local(token: &str) -> bool {
    token
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .starts_with('<')
        || token.contains("$(")
        || token.contains('`')
}

/// Whether code carries a shell expansion the guard does not evaluate.
fn expands(code: &str) -> bool {
    code.contains("$(")
        || code.contains('`')
        || code.contains("${")
        || code
            .match_indices('$')
            .any(|(i, _)| code[i + 1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
}
