//! `tm env set|keys` — edit a dotenv file without printing a value (#8939).
//!
//! Why: owner ruling item 44 lets the Architect manage a project's env files;
//! the Architect rulings on the design keep every value out of the
//! transcript. `set` takes the value from the login Keychain only, never
//! argv (Q1) or stdin (#8939 delta critic); `keys` prints names only. Both check the Architect binding
//! themselves (Q5), so a PM that reaches the verb through a script the
//! lexical guard cannot read is still refused.
//! What: [`run`] checks the binding as `tm fleet status` does, then the
//! guard's own path policy (`envfile_policy`), then spends the pm-guard's
//! one-shot grant for the exact call (`env_file_grant`), then [`list_keys`]
//! or [`set_key`] on the canonical path through `env_file_fs`, which follows
//! no symlink. The binding alone cannot tell the main thread from a Claude
//! Code subagent (same `claude`, same environment); the grant can, because
//! only the guard sees the payload. [`parse`] is the quote-aware dotenv reader
//! both use: a file with any line that is not blank, a comment or a
//! `[export ]KEY=value` assignment fails whole, naming only the line number.
//! No error or success message carries a value or a line of the file.
//! Test: `env_file_tests.rs`.

use std::fmt;
use std::io::Write;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use trusty_mpm::core::config::MpmConfig;

use crate::cli::EnvAction;
use crate::commands::env_file_fs::EnvDir;
use crate::commands::env_file_grant;
use crate::commands::fleet::resolve_dir;
use crate::commands::fleet::status::{this_session_check, this_session_env};
use crate::commands::pm_guard_architect_envfile::{EnvfileCall, ScopedEnvfile, envfile_policy};
use crate::commands::pm_guard_trust_anchor::{ANCHOR_ROOT, HookEnv};

/// Refusal for a call the pm-guard did not grant (#8939 fix round).
const NO_GRANT: &str = "tm env: refused; no pm-guard grant for this exact call. The pm-guard \
                        grants `tm env` only to the Architect main thread's own direct \
                        `tm env keys|set` call, once, within a minute; a subagent or indirect \
                        call gets none, and a grant the guard failed to write, already spent \
                        or expired also ends here";

/// The longest value `set` writes.
const MAX_VALUE_BYTES: usize = 64 << 10;

/// Refusal for a value on the command line (Architect ruling Q1).
const ARGV_REFUSED: &str = "tm env set: the value never goes on the command line; use \
                            --from-keychain <service> --account <account>";

/// Refusal for `set` with no Keychain source (#8939 delta critic MEDIUM):
/// the guard-approved shape cannot pipe stdin, and a grant binds no value.
const KEYCHAIN_REQUIRED: &str = "tm env set: the value comes only from the login Keychain; use \
                                 --from-keychain <service> --account <account>";

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
    let account = match nix::unistd::User::from_uid(nix::unistd::Uid::current()) {
        Ok(Some(user)) if !user.dir.as_os_str().is_empty() => Some(user.dir),
        _ => None,
    };
    let home = verb_home(dirs::home_dir(), account)?;
    let architect_dir = resolve_dir(None, &home)?;
    let env = HookEnv {
        home: Some(home.clone()),
        ..this_session_env()
    };
    let root = home.join(ANCHOR_ROOT);
    let out = &mut std::io::stdout();
    run_with(
        action,
        env,
        || MpmConfig::load(&root),
        &architect_dir,
        &LoginKeychain,
        out,
    )
}

/// The home `tm env` trusts: the password database's, and `$HOME` must agree.
///
/// Why: #8939 delta critic HIGH 1 — the grants, the config roots and the
/// binding all live under the home, and an indirect caller sets `$HOME`, so
/// `HOME=/tmp/f tm env …` would read a forged tree built with plain writes.
/// What: `account` is `getpwuid(getuid())`'s home. Both must be present and
/// canonicalize to the same directory; any other case refuses.
/// Test: `the_verb_home_is_the_account_home_and_home_must_match`, and end to
/// end `a_forged_home_with_a_valid_grant_is_refused`.
pub(crate) fn verb_home(
    ambient: Option<PathBuf>,
    account: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    let canonical = |p: Option<PathBuf>| p.and_then(|p| std::fs::canonicalize(p).ok());
    let account = canonical(account)
        .context("tm env: refused; the password database names no home for this user")?;
    if canonical(ambient).as_ref() != Some(&account) {
        bail!(
            "tm env: refused; $HOME is not this user's home ({})",
            account.display()
        );
    }
    Ok(account)
}

/// [`run`] over explicit inputs.
///
/// Why: Architect ruling Q5 — the verb checks the binding itself; the #8939
/// fix round adds the path policy and the main-thread grant.
/// What: `this_session_check` over `env` (a missing `CLAUDE_PROJECT_DIR`
/// falls back to `architect_dir`, as for `tm fleet status`), then
/// [`authorize`]; either failing is refused before any env file is opened or
/// the Keychain read. Then `keys` prints one name per line, and `set` prints
/// `set KEY (added|replaced)`, both on the canonical path the policy checked.
/// Test: `env_verbs_refuse_a_session_that_is_not_the_architect`,
/// `env_set_reads_the_value_from_the_keychain_only`,
/// `an_architect_bound_set_on_a_non_env_file_never_reads_the_keychain`,
/// `a_subagent_shaped_caller_without_a_grant_is_refused`.
pub(crate) fn run_with(
    action: EnvAction,
    env: HookEnv,
    config: impl Fn() -> MpmConfig,
    architect_dir: &Path,
    keychain: &dyn Keychain,
    out: &mut dyn Write,
) -> anyhow::Result<()> {
    let check = this_session_check(architect_dir, env.clone(), &config);
    if !check.ok {
        bail!("tm env: refused; {}", check.detail);
    }
    let cwd = std::env::current_dir().context("tm env: the working directory is unknown")?;
    // #8939 delta critic HIGH 1: the verb's `CLAUDE_PROJECT_DIR` is the
    // caller's to set, so it grants no scope here; only the trust-anchor roots
    // do. The grant binds the canonical path the guard checked with the
    // hook's own `CLAUDE_PROJECT_DIR`.
    let scope_env = HookEnv {
        project_dir: None,
        ..env.clone()
    };
    let scope = Scope {
        cwd: &cwd,
        env: &scope_env,
        config: &config,
    };
    match action {
        EnvAction::Keys { path } => {
            let file = scope.authorize(&path, "keys", Vec::new(), None)?;
            for key in list_keys(&file)? {
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
            let (Some(service), Some(account)) = (from_keychain, account) else {
                bail!(KEYCHAIN_REQUIRED);
            };
            let source = Some((service.clone(), account.clone()));
            let file = scope.authorize(&path, "set", vec![key.clone()], source)?;
            let value = keychain.password(&service, &account)?;
            check_value(&value)?;
            let outcome = set_key(&file, &key, &value)?;
            writeln!(out, "set {key} ({outcome})")?;
        }
    }
    Ok(())
}

/// What [`Scope::authorize`] checks a call against.
struct Scope<'a> {
    cwd: &'a Path,
    env: &'a HookEnv,
    config: &'a dyn Fn() -> MpmConfig,
}

impl Scope<'_> {
    /// The scoped env file for this exact call, or a refusal.
    ///
    /// Why: #8939 fix round — the verb checked only the binding, so it wrote
    /// a Keychain value to any path; and a Claude Code subagent passes the
    /// binding, since it shares the Architect's `claude` and environment.
    /// What: [`envfile_policy`], the guard's own path policy, on `word`; then
    /// spend the pm-guard's one-shot grant for the exact call
    /// (`env_file_grant`), which the guard mints only for the main thread's
    /// exempt shape. Touches no env file and no Keychain item.
    fn authorize(
        &self,
        word: &Path,
        verb: &'static str,
        keys: Vec<String>,
        keychain: Option<(String, String)>,
    ) -> anyhow::Result<ScopedEnvfile> {
        let refused = || {
            anyhow!(
                "tm env: refused; {} is not a `.env` or `.env.*` file inside the project scope",
                word.display()
            )
        };
        let text = word.to_str().ok_or_else(refused)?;
        let file = envfile_policy(text, self.cwd, verb == "set", self.env, self.config)
            .ok_or_else(refused)?;
        let call = EnvfileCall {
            verb,
            path: file.path().to_path_buf(),
            keys,
            keychain,
        };
        let granted = self
            .env
            .home
            .as_deref()
            .is_some_and(|home| env_file_grant::consume(home, &call));
        if !granted {
            bail!(NO_GRANT);
        }
        Ok(file)
    }
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

/// Refuse an empty, multi-line or oversized value, naming no byte of it.
fn check_value(value: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        bail!("tm env set: the Keychain item is empty");
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
/// What: [`EnvDir::read`] on the canonical path (a missing file is an
/// error), then [`parse`].
/// Test: `env_keys_prints_names_only`,
/// `env_keys_never_prints_a_line_inside_a_multiline_value`,
/// `env_keys_refuses_a_non_assignment_line_and_prints_nothing`,
/// `env_keys_refuses_a_symlink`.
pub(crate) fn list_keys(file: &ScopedEnvfile) -> anyhow::Result<Vec<String>> {
    let path = file.path();
    let text = EnvDir::open(path)?
        .read()?
        .ok_or_else(|| anyhow!("tm env: {} does not exist", path.display()))?;
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
/// What: through one [`EnvDir`] on the canonical path: a symlink or
/// non-regular file is refused; a missing file is created. The first
/// assignment of `key` is replaced in place (keeping `export`), later ones
/// removed; otherwise one line is appended. The whole file is written to a
/// mode-0600 temp file beside it and renamed over it.
/// Test: `env_set_replaces_in_place_and_creates_0600`,
/// `env_set_never_echoes_the_value_on_success_or_error`,
/// `a_parent_swapped_for_a_symlink_after_the_check_is_refused`.
pub(crate) fn set_key(
    file: &ScopedEnvfile,
    key: &str,
    value: &str,
) -> anyhow::Result<&'static str> {
    let path = file.path();
    let dir = EnvDir::open(path)?;
    let text = dir.read()?.unwrap_or_default();
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
    dir.replace(&new_text)?;
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
