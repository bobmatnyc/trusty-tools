//! `tm env set|keys` — edit a dotenv file without printing a value (#8939).
//!
//! Why: owner ruling item 44 lets the Architect manage a project's env files;
//! the Architect rulings on the design keep every value out of the
//! transcript. `set` takes the value from stdin or the login Keychain, never
//! argv (Q1); `keys` prints names only. Both check the Architect binding
//! themselves (Q5), so a PM that reaches the verb through a script the
//! lexical guard cannot read is still refused.
//! What: [`run`] checks the binding as `tm fleet status` does, then
//! [`list_keys`] or [`set_key`]. [`parse`] is the quote-aware dotenv reader
//! both use: a file with any line that is not blank, a comment or a
//! `[export ]KEY=value` assignment fails whole, naming only the line number.
//! No error or success message carries a value or a line of the file.
//! Test: `env_file_tests.rs`.

use std::fmt;
use std::io::{IsTerminal, Read, Write};
use std::ops::Range;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, anyhow, bail};
use trusty_mpm::core::config::MpmConfig;

use crate::cli::EnvAction;
use crate::commands::fleet::resolve_dir;
use crate::commands::fleet::status::{this_session_check, this_session_env};
use crate::commands::pm_guard_trust_anchor::HookEnv;

/// The largest env file `keys` and `set` read.
const MAX_FILE_BYTES: u64 = 1 << 20;

/// The longest value `set` writes.
const MAX_VALUE_BYTES: usize = 64 << 10;

/// Refusal for a value on the command line (Architect ruling Q1).
const ARGV_REFUSED: &str = "tm env set: the value never goes on the command line; pipe it on \
                            stdin, or use --from-keychain <service> --account <account>";

/// Where `set` reads a value from.
pub(crate) struct Sources<'a> {
    /// The process's stdin.
    pub(crate) stdin: &'a mut dyn Read,
    /// Whether stdin is a terminal; a terminal is refused.
    pub(crate) stdin_is_tty: bool,
    /// The login Keychain.
    pub(crate) keychain: &'a dyn Keychain,
}

/// A generic-password lookup in the login Keychain.
pub(crate) trait Keychain {
    /// The password of the item `service`/`account`.
    fn password(&self, service: &str, account: &str) -> anyhow::Result<String>;
}

/// The user's login Keychain, through `/usr/bin/security`.
pub(crate) struct LoginKeychain;

impl Keychain for LoginKeychain {
    fn password(&self, service: &str, account: &str) -> anyhow::Result<String> {
        let out = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", service, "-a", account])
            .args(["-w", "login.keychain"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .context("tm env set: cannot run /usr/bin/security")?;
        if !out.status.success() {
            bail!(
                "tm env set: no login-Keychain item for service `{service}`, account `{account}`"
            );
        }
        let mut value = String::from_utf8(out.stdout)
            .map_err(|_| anyhow!("tm env set: the Keychain item is not UTF-8 text"))?;
        if value.ends_with('\n') {
            value.pop();
        }
        Ok(value)
    }
}

/// `tm env set|keys`, for the calling process.
pub(crate) fn run(action: EnvAction) -> anyhow::Result<()> {
    let home = dirs::home_dir().context("tm env: the home directory is unknown")?;
    let architect_dir = resolve_dir(None, &home)?;
    let stdin = std::io::stdin();
    let stdin_is_tty = stdin.is_terminal();
    let sources = Sources {
        stdin: &mut stdin.lock(),
        stdin_is_tty,
        keychain: &LoginKeychain,
    };
    let env = this_session_env();
    let out = &mut std::io::stdout();
    run_with(
        action,
        env,
        MpmConfig::load_default,
        &architect_dir,
        sources,
        out,
    )
}

/// [`run`] over explicit inputs.
///
/// Why: Architect ruling Q5 — the verb checks the binding itself.
/// What: `this_session_check` over `env` (a missing `CLAUDE_PROJECT_DIR`
/// falls back to `architect_dir`, as for `tm fleet status`); a failure is
/// refused before any file is touched. Then `keys` prints one name per line,
/// and `set` prints `set KEY (added|replaced)`.
/// Test: `env_verbs_refuse_a_session_that_is_not_the_architect`,
/// `env_set_reads_the_value_from_stdin_or_the_keychain`.
pub(crate) fn run_with(
    action: EnvAction,
    env: HookEnv,
    config: impl FnOnce() -> MpmConfig,
    architect_dir: &Path,
    sources: Sources<'_>,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let check = this_session_check(architect_dir, env, config);
    if !check.ok {
        bail!("tm env: refused; {}", check.detail);
    }
    match action {
        EnvAction::Keys { path } => {
            for key in list_keys(&path)? {
                writeln!(out, "{key}")?;
            }
        }
        EnvAction::Set {
            path,
            key,
            from_keychain,
            account,
            extra,
        } => {
            check_key(&key, &extra)?;
            let value = match (from_keychain, account) {
                (Some(service), Some(account)) => sources.keychain.password(&service, &account)?,
                (None, None) => read_value(sources.stdin, sources.stdin_is_tty)?,
                _ => bail!("tm env set: --from-keychain and --account go together"),
            };
            check_value(&value)?;
            let outcome = set_key(&path, &key, &value)?;
            writeln!(out, "set {key} ({outcome})")?;
        }
    }
    Ok(())
}

/// Refuse a `KEY=value` or extra argument, and a malformed key, echoing none.
fn check_key(key: &str, extra: &[String]) -> anyhow::Result<()> {
    if !extra.is_empty() || key.contains('=') {
        bail!(ARGV_REFUSED);
    }
    if !is_key_name(key) {
        bail!("tm env set: not a key name; use letters, digits and `_`, not starting with a digit");
    }
    Ok(())
}

/// A dotenv key name: `[A-Za-z_][A-Za-z0-9_]*`.
pub(crate) fn is_key_name(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The value on stdin, one trailing newline removed; a terminal is refused.
fn read_value(stdin: &mut dyn Read, is_tty: bool) -> anyhow::Result<String> {
    if is_tty {
        bail!("tm env set: stdin is a terminal; pipe the value in, or use --from-keychain");
    }
    let mut bytes = Vec::new();
    stdin
        .take(MAX_VALUE_BYTES as u64 + 2)
        .read_to_end(&mut bytes)
        .context("tm env set: cannot read stdin")?;
    let mut value =
        String::from_utf8(bytes).map_err(|_| anyhow!("tm env set: the value is not UTF-8 text"))?;
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    Ok(value)
}

/// Refuse an empty, multi-line or oversized value, naming no byte of it.
fn check_value(value: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        bail!("tm env set: no value; pipe it on stdin, or use --from-keychain");
    }
    if value.contains(['\n', '\r', '\0']) {
        bail!("tm env set: the value must be one line of text");
    }
    if value.len() > MAX_VALUE_BYTES {
        bail!("tm env set: the value is longer than {MAX_VALUE_BYTES} bytes");
    }
    Ok(())
}

/// The key names of the env file at `path`, first occurrence order.
///
/// Why: design §2 shape K — names only, and nothing at all from a file that
/// is not wholly assignments (an unquoted PEM line would read as a key).
/// What: [`read_regular`], then [`parse`].
/// Test: `env_keys_prints_names_only`,
/// `env_keys_never_prints_a_line_inside_a_multiline_value`,
/// `env_keys_refuses_a_non_assignment_line_and_prints_nothing`,
/// `env_keys_refuses_a_symlink`.
pub(crate) fn list_keys(path: &Path) -> anyhow::Result<Vec<String>> {
    let text = read_regular(path)?;
    let mut keys: Vec<String> = Vec::new();
    for assignment in parse(&text).map_err(|e| anyhow!("tm env: {}: {e}", path.display()))? {
        if !keys.contains(&assignment.key) {
            keys.push(assignment.key);
        }
    }
    Ok(keys)
}

/// Set `key` to `value` in the env file at `path`; `"added"` or `"replaced"`.
///
/// Why: design §2 shape S — the Architect's only write path (ruling Q4).
/// What: a symlink or non-regular file is refused; a missing file is created.
/// The first assignment of `key` is replaced in place (keeping `export`),
/// later ones removed; otherwise one line is appended. The whole file is
/// written to a mode-0600 temp file beside it and renamed over it.
/// Test: `env_set_replaces_in_place_and_creates_0600`,
/// `env_set_never_echoes_the_value_on_success_or_error`.
pub(crate) fn set_key(path: &Path, key: &str, value: &str) -> anyhow::Result<&'static str> {
    let text = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!("tm env: {} is a symbolic link; refused", path.display())
        }
        Ok(_) => read_regular(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => bail!("tm env: cannot stat {}: {e}", path.display()),
    };
    let parsed = parse(&text).map_err(|e| anyhow!("tm env: {}: {e}", path.display()))?;
    let line = |export: bool| {
        let prefix = if export { "export " } else { "" };
        format!("{prefix}{key}={}\n", quote_value(value))
    };
    let mut matches = parsed.iter().filter(|a| a.key == key).peekable();
    let (new_text, outcome) = if matches.peek().is_some() {
        let (mut out, mut at) = (String::new(), 0);
        for (i, assignment) in matches.enumerate() {
            out.push_str(&text[at..assignment.span.start]);
            if i == 0 {
                out.push_str(&line(assignment.export));
            }
            at = assignment.span.end;
        }
        out.push_str(&text[at..]);
        (out, "replaced")
    } else {
        let mut out = text.clone();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&line(false));
        (out, "added")
    };
    write_atomic(path, &new_text)?;
    Ok(outcome)
}

/// `value` as a dotenv value: bare when plain, else single-quoted, else
/// double-quoted with `\`, `"`, `$` and backtick escaped.
fn quote_value(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_./:@+=,%-".contains(c))
    {
        return value.to_owned();
    }
    if !value.contains('\'') {
        return format!("'{value}'");
    }
    let mut out = String::from("\"");
    for c in value.chars() {
        if matches!(c, '\\' | '"' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The text of the regular file at `path`, opened with `O_NOFOLLOW`.
fn read_regular(path: &Path) -> anyhow::Result<String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => anyhow!("tm env: {} is a symbolic link; refused", path.display()),
            _ => anyhow!("tm env: cannot open {}: {e}", path.display()),
        })?;
    let meta = file.metadata().context("tm env: cannot stat the file")?;
    if !meta.is_file() {
        bail!("tm env: {} is not a regular file; refused", path.display());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("tm env: cannot read {}", path.display()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        bail!(
            "tm env: {} is larger than {MAX_FILE_BYTES} bytes",
            path.display()
        );
    }
    String::from_utf8(bytes).map_err(|_| anyhow!("tm env: {} is not UTF-8 text", path.display()))
}

/// Write `text` to `path` through a mode-0600 temp file and a rename.
fn write_atomic(path: &Path, text: &str) -> anyhow::Result<()> {
    let name = path
        .file_name()
        .with_context(|| format!("tm env: {} names no file", path.display()))?;
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let tmp_name = format!(
        ".{}.tm-env-{}-{nanos}.tmp",
        name.to_string_lossy(),
        std::process::id()
    );
    let tmp = dir.map_or_else(|| tmp_name.clone().into(), |d| d.join(&tmp_name));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .with_context(|| {
            format!(
                "tm env: cannot create a temp file beside {}",
                path.display()
            )
        })?;
    let written = file
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .and_then(|()| file.write_all(text.as_bytes()))
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        bail!("tm env: cannot write {}: {e}", path.display());
    }
    Ok(())
}

/// One `[export ]KEY=value` assignment and the byte range of its lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Assignment {
    /// The key name.
    pub(crate) key: String,
    /// Whether the line starts with `export `.
    pub(crate) export: bool,
    /// The assignment's lines, newline included.
    pub(crate) span: Range<usize>,
}

/// A line that is not blank, a comment or an assignment; 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NotAnAssignment {
    /// The line the assignment starts on.
    pub(crate) line: usize,
}

impl fmt::Display for NotAnAssignment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: not an assignment", self.line)
    }
}

impl std::error::Error for NotAnAssignment {}

/// Every assignment in `text`, or the first line that is not one.
///
/// What: blank and `#` lines are skipped. An assignment is
/// `[export ]KEY=value` with a [`is_key_name`] key. A value opening with `'`
/// or `"` runs to its closing quote, across lines (`\` escapes in `"`), and
/// only blanks or a `#` comment may follow it; an unclosed quote fails.
/// Test: `env_keys_never_prints_a_line_inside_a_multiline_value`,
/// `env_keys_refuses_a_non_assignment_line_and_prints_nothing`.
pub(crate) fn parse(text: &str) -> Result<Vec<Assignment>, NotAnAssignment> {
    let mut out = Vec::new();
    let mut lines = text.split_inclusive('\n');
    let (mut number, mut offset) = (0, 0);
    while let Some(raw) = lines.next() {
        number += 1;
        let start = offset;
        offset += raw.len();
        let err = NotAnAssignment { line: number };
        let body = raw
            .trim_end_matches(['\n', '\r'])
            .trim_start_matches([' ', '\t']);
        if body.trim().is_empty() || body.starts_with('#') {
            continue;
        }
        let (export, rest) = match body.strip_prefix("export ") {
            Some(rest) => (true, rest.trim_start_matches([' ', '\t'])),
            None => (false, body),
        };
        let (key, value) = rest.split_once('=').ok_or(err)?;
        if !is_key_name(key) {
            return Err(err);
        }
        if let Some(quote) = value.chars().next().filter(|c| matches!(c, '\'' | '"')) {
            let mut tail = &value[1..];
            loop {
                if let Some(end) = closing_quote(tail, quote) {
                    let after = tail[end + 1..].trim_start_matches([' ', '\t']);
                    if !(after.is_empty() || after.starts_with('#')) {
                        return Err(err);
                    }
                    break;
                }
                let next = lines.next().ok_or(err)?;
                number += 1;
                offset += next.len();
                tail = next.trim_end_matches(['\n', '\r']);
            }
        }
        out.push(Assignment {
            key: key.to_owned(),
            export,
            span: start..offset,
        });
    }
    Ok(out)
}

/// The byte index of the quote closing `text`, or `None`.
fn closing_quote(text: &str, quote: char) -> Option<usize> {
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        if quote == '"' && c == '\\' {
            chars.next();
        } else if c == quote {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
#[path = "env_file_tests.rs"]
mod tests;
