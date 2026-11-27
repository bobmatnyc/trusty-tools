//! The Architect's instance settings from `local/architect.json`: the port of `architect_config.py` (#12028).
//!
//! Why: owner name, repo table, dashboard port, memory palace and caps
//! differ per installation and live in the instance's own file, never in a
//! shipped program (#10161, #9171).
//! What: [`load`] reads `<root>/local/architect.json`, or the file
//! `ARCHITECT_CONFIG` names (which must exist), and resolves it with the
//! script's precedence: environment, then file, then default; an empty
//! variable counts as unset. A missing or blank file at the default path
//! yields the defaults. Malformed JSON, a wrong type, an out-of-range or
//! non-finite value, a missing `ARCHITECT_CONFIG` file or an unreadable path
//! is a [`ConfigError`] that names the path and the key, never the value.
//! Unknown keys land in [`Config::unknown`]. Checks run in the script's
//! order, so the first error is the script's first error.
//! Known gap: an integer `poll_interval_s` past `u64` saturates where Python
//! keeps it exact; `HOME` comes from the `env` lookup where the script reads
//! `os.environ` (the same mapping when the caller passes the process env).
//! Test: `architect_config_recordings_replay_clean_against_the_port`.

mod paths;

use std::path::Path;

use crate::poller::pyfmt::{py_float, py_float_repr, py_int_text};
use crate::pyjson_load::{PyInt, PyValue, loads};

/// `DEFAULT_PORT`.
pub const DEFAULT_PORT: i64 = 7890;
/// `DEFAULT_POLL_INTERVAL_S` (#11369).
pub const DEFAULT_POLL_INTERVAL_S: u64 = 60;
/// `DEFAULT_PROJECTS_ROOT`.
pub const DEFAULT_PROJECTS_ROOT: &str = "~/trusty-mpm-projects";
/// `DEFAULT_CLAUDE_CONFIG_DIR`.
pub const DEFAULT_CLAUDE_CONFIG_DIR: &str = "~/.trusty-tools/trusty-mpm/claude-config";
const KNOWN: [&str; 9] = [
    "owner_name",
    "memory_palace",
    "dashboard_port",
    "tailnet_url",
    "repos",
    "caps",
    "projects_root",
    "claude_config_dir",
    "poll_interval_s",
];
const REPO_FIELDS: [&str; 3] = ["slug", "gh_config_dir", "checkout"];

/// `ConfigError`: the file or an override variable is unusable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

/// One `repos` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The GitHub slug, never empty.
    pub slug: String,
    /// Its gh config dir, `""` when unset.
    pub gh_config_dir: String,
    /// Its checkout, `""` when unset.
    pub checkout: String,
}

/// A numeric cap as Python holds it.
#[derive(Debug, Clone, PartialEq)]
pub enum Cap {
    /// An `int` that fits `i64`.
    Int(i64),
    /// An `int` past `i64`, as decimal text.
    BigInt(String),
    /// A finite `float`.
    Float(f64),
}

impl Cap {
    /// Python's `repr()` of the value.
    pub fn repr(&self) -> String {
        match self {
            Self::Int(n) => n.to_string(),
            Self::BigInt(s) => s.clone(),
            Self::Float(x) => py_float_repr(*x),
        }
    }
}

/// The resolved settings (`Config`); maps keep the file's order.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// The file read (or that would have been).
    pub path: String,
    /// `""`: no owner is configured.
    pub owner_name: String,
    /// `""`: the caller derives it.
    pub memory_palace: String,
    /// 1..65535.
    pub dashboard_port: i64,
    /// `""` or an http(s) origin.
    pub tailnet_url: String,
    /// Name to repo, in file order.
    pub repos: Vec<(String, Repo)>,
    /// Name to cap, in file order (`LOAD_FACTOR` replaces or appends).
    pub caps: Vec<(String, Cap)>,
    /// Unknown keys, sorted (`repos.<name>.<key>` for an entry's).
    pub unknown: Vec<String>,
    /// Absolute.
    pub projects_root: String,
    /// Absolute.
    pub claude_config_dir: String,
    /// Whole seconds, at least 1.
    pub poll_interval_s: u64,
}

/// An environment lookup: `Some` for a set variable, empty included.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

fn set(env: Env<'_>, name: &str) -> Option<String> {
    env(name).filter(|v| !v.is_empty())
}

fn fail(path: &str, key: &str, want: &str) -> ConfigError {
    ConfigError(format!("{path}: {key}: must be {want}"))
}

fn cwd() -> Result<String, ConfigError> {
    std::env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .map_err(|e| ConfigError(format!("the working directory is unreadable: {e}")))
}

fn abs(value: &str, env: Env<'_>) -> Result<String, ConfigError> {
    let home = env("HOME");
    Ok(paths::abspath(
        &paths::expanduser(value, home.as_deref()),
        &cwd()?,
    ))
}

/// `default_claude_config_dir(environ)`: `CLAUDE_CONFIG_DIR`, else the default; absolute.
///
/// # Errors
/// Only when the working directory cannot be read.
pub fn default_claude_config_dir(env: Env<'_>) -> Result<String, ConfigError> {
    let dir = set(env, "CLAUDE_CONFIG_DIR").unwrap_or_else(|| DEFAULT_CLAUDE_CONFIG_DIR.into());
    abs(&dir, env)
}

/// `default_path(root, environ)`: `ARCHITECT_CONFIG`, else `<root>/local/architect.json`.
pub fn default_path(root: &Path, env: Env<'_>) -> String {
    if let Some(p) = set(env, "ARCHITECT_CONFIG") {
        return p;
    }
    let root = root.to_string_lossy();
    let sep = if root.ends_with('/') || root.is_empty() {
        ""
    } else {
        "/"
    };
    format!("{root}{sep}local/architect.json")
}

/// `_read(path, required)`: the top-level object; empty when blank, or
/// absent and not `required`.
fn read(path: &str, required: bool) -> Result<Vec<(String, PyValue)>, ConfigError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if required {
                return Err(ConfigError(format!(
                    "{path}: named by ARCHITECT_CONFIG, but does not exist"
                )));
            }
            return Ok(Vec::new());
        }
        Err(e) => {
            let class = match e.kind() {
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                std::io::ErrorKind::IsADirectory => "IsADirectoryError",
                std::io::ErrorKind::NotADirectory => "NotADirectoryError",
                _ => "OSError",
            };
            return Err(ConfigError(format!("{path}: cannot read: {class}")));
        }
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| ConfigError(format!("{path}: cannot read: UnicodeDecodeError")))?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    if crate::pystr::strip(&text).is_empty() {
        return Ok(Vec::new());
    }
    match loads(&text) {
        Ok(PyValue::Dict(pairs)) => Ok(pairs),
        Ok(_) => Err(ConfigError(format!(
            "{path}: the top level must be a JSON object"
        ))),
        Err(e) => Err(ConfigError(format!(
            "{path}: not valid JSON ({} at line {} column {})",
            e.msg, e.lineno, e.colno
        ))),
    }
}

fn get<'a>(data: &'a [(String, PyValue)], key: &str) -> Option<&'a PyValue> {
    data.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// `_string(path, data, key)`.
fn string(path: &str, data: &[(String, PyValue)], key: &str) -> Result<String, ConfigError> {
    match get(data, key) {
        None => Ok(String::new()),
        Some(PyValue::Str(s)) => Ok(s.clone()),
        Some(_) => Err(fail(path, key, "a string")),
    }
}

/// `_url(path, key, value)`.
fn url(path: &str, key: &str, value: String) -> Result<String, ConfigError> {
    if !value.is_empty() && !value.starts_with("https://") && !value.starts_with("http://") {
        return Err(fail(path, key, "an http:// or https:// URL"));
    }
    Ok(value)
}

/// `_port(path, key, value)` for an `int` (`None`: not an int).
fn port(path: &str, key: &str, value: Option<&PyInt>) -> Result<i64, ConfigError> {
    match value {
        Some(PyInt::Small(n)) if (1..=65535).contains(n) => Ok(*n),
        _ => Err(fail(path, key, "an integer in 1..65535")),
    }
}

fn as_int(v: &PyValue) -> Option<&PyInt> {
    match v {
        PyValue::Int(i) => Some(i),
        _ => None,
    }
}

/// `_is_number` as a [`Cap`]: an int or a finite float, never a bool.
fn number(v: &PyValue) -> Option<Cap> {
    match v {
        PyValue::Int(PyInt::Small(n)) => Some(Cap::Int(*n)),
        PyValue::Int(PyInt::Big(s)) => Some(Cap::BigInt(s.clone())),
        PyValue::Float(x) if x.is_finite() => Some(Cap::Float(*x)),
        _ => None,
    }
}

fn positive(c: &Cap) -> bool {
    match c {
        Cap::Int(n) => *n > 0,
        Cap::BigInt(s) => !s.starts_with('-'),
        Cap::Float(x) => *x > 0.0,
    }
}

/// `_repos(path, value, unknown)`.
fn repos(
    path: &str,
    value: Option<&PyValue>,
    unknown: &mut Vec<String>,
) -> Result<Vec<(String, Repo)>, ConfigError> {
    let entries = match value {
        None => return Ok(Vec::new()),
        Some(PyValue::Dict(e)) => e,
        Some(_) => {
            return Err(fail(
                path,
                "repos",
                "an object of name -> {slug, gh_config_dir, checkout}",
            ));
        }
    };
    let mut out = Vec::new();
    for (name, entry) in entries {
        let key = format!("repos.{name}");
        let PyValue::Dict(fields) = entry else {
            return Err(fail(path, &key, "an object"));
        };
        let mut got = Vec::new();
        for f in REPO_FIELDS {
            got.push(
                string(path, fields, f)
                    .map_err(|_| fail(path, &format!("{key}.{f}"), "a string"))?,
            );
        }
        if got[0].is_empty() {
            return Err(fail(path, &format!("{key}.slug"), "a non-empty string"));
        }
        unknown.extend(
            fields
                .iter()
                .filter(|(k, _)| !REPO_FIELDS.contains(&k.as_str()))
                .map(|(k, _)| format!("{key}.{k}")),
        );
        let mut it = got.into_iter();
        let mut next = || it.next().unwrap_or_default();
        out.push((
            name.clone(),
            Repo {
                slug: next(),
                gh_config_dir: next(),
                checkout: next(),
            },
        ));
    }
    Ok(out)
}

/// `_caps(path, value)`.
fn caps(path: &str, value: Option<&PyValue>) -> Result<Vec<(String, Cap)>, ConfigError> {
    let items = match value {
        None => return Ok(Vec::new()),
        Some(PyValue::Dict(e)) => e,
        Some(_) => return Err(fail(path, "caps", "an object of name -> number")),
    };
    let mut out = Vec::new();
    for (name, item) in items {
        let key = format!("caps.{name}");
        let cap = number(item).ok_or_else(|| fail(path, &key, "a number"))?;
        if name == "builder_slots"
            && !(matches!(cap, Cap::Int(_) | Cap::BigInt(_)) && positive(&cap))
        {
            return Err(fail(path, &key, "an integer of 1 or more"));
        }
        if name == "load_factor" && !positive(&cap) {
            return Err(fail(path, &key, "a number above 0"));
        }
        out.push((name.clone(), cap));
    }
    Ok(out)
}

/// `_interval(path, value)`.
fn interval(path: &str, value: Option<&PyValue>) -> Result<u64, ConfigError> {
    let bad = || {
        fail(
            path,
            "poll_interval_s",
            "a whole number of seconds, at least 1",
        )
    };
    match value.map(as_int) {
        None => Ok(DEFAULT_POLL_INTERVAL_S),
        Some(Some(PyInt::Small(n))) if *n >= 1 => u64::try_from(*n).map_err(|_| bad()),
        Some(Some(PyInt::Big(s))) if !s.starts_with('-') => Ok(u64::MAX),
        Some(_) => Err(bad()),
    }
}

/// `load(root, environ)`: the resolved config.
///
/// Why: see the module doc; every port that needs an instance setting reads
/// it here, so the precedence and the refusals live in one place.
/// What: the script's `load`, check for check (see the module doc).
/// Test: `architect_config_recordings_replay_clean_against_the_port`.
///
/// # Errors
/// [`ConfigError`] as the module doc lists.
pub fn load(root: &Path, env: Env<'_>) -> Result<Config, ConfigError> {
    let path = default_path(root, env);
    let data = read(&path, set(env, "ARCHITECT_CONFIG").is_some())?;
    let mut unknown: Vec<String> = data
        .iter()
        .map(|(k, _)| k.clone())
        .filter(|k| !KNOWN.contains(&k.as_str()))
        .collect();
    unknown.sort();
    let mut owner = string(&path, &data, "owner_name")?;
    let mut palace = string(&path, &data, "memory_palace")?;
    let mut dashboard_port = match get(&data, "dashboard_port") {
        None => DEFAULT_PORT,
        Some(v) => port(&path, "dashboard_port", as_int(v))?,
    };
    let mut tailnet = url(&path, "tailnet_url", string(&path, &data, "tailnet_url")?)?;
    let repos = repos(&path, get(&data, "repos"), &mut unknown)?;
    let mut caps = caps(&path, get(&data, "caps"))?;
    let poll_interval_s = interval(&path, get(&data, "poll_interval_s"))?;
    let mut projects_root = string(&path, &data, "projects_root")?;
    if projects_root.is_empty() {
        projects_root = DEFAULT_PROJECTS_ROOT.into();
    }
    let mut claude_dir = string(&path, &data, "claude_config_dir")?;
    if claude_dir.is_empty() {
        claude_dir = DEFAULT_CLAUDE_CONFIG_DIR.into();
    }
    // See #10161: the environment wins over the file, one variable per key.
    if let Some(v) = set(env, "ARCHITECT_OWNER_NAME") {
        owner = v;
    }
    if let Some(v) = set(env, "ARCHITECT_MEMORY_PALACE") {
        palace = v;
    }
    if let Some(v) = set(env, "DASHBOARD_TAILNET_URL") {
        tailnet = url("environment", "DASHBOARD_TAILNET_URL", v)?;
    }
    if let Some(v) = set(env, "DASHBOARD_PORT") {
        let text =
            py_int_text(&v).ok_or_else(|| fail("environment", "DASHBOARD_PORT", "an integer"))?;
        let n = text
            .parse::<i64>()
            .map_or_else(|_| PyInt::Big(text), PyInt::Small);
        dashboard_port = port("environment", "DASHBOARD_PORT", Some(&n))?;
    }
    if let Some(v) = set(env, "ARCHITECT_PROJECTS_ROOT") {
        projects_root = v;
    }
    if let Some(v) = set(env, "CLAUDE_CONFIG_DIR") {
        claude_dir = v;
    }
    if let Some(v) = set(env, "LOAD_FACTOR") {
        let x = py_float(&v).ok_or_else(|| fail("environment", "LOAD_FACTOR", "a number"))?;
        if !(x.is_finite() && x > 0.0) {
            return Err(fail("environment", "LOAD_FACTOR", "a number above 0"));
        }
        match caps.iter_mut().find(|(k, _)| k == "load_factor") {
            Some(slot) => slot.1 = Cap::Float(x),
            None => caps.push(("load_factor".into(), Cap::Float(x))),
        }
    }
    unknown.sort();
    Ok(Config {
        path,
        owner_name: owner,
        memory_palace: palace,
        dashboard_port,
        tailnet_url: tailnet,
        repos,
        caps,
        unknown,
        projects_root: abs(&projects_root, env)?,
        claude_config_dir: abs(&claude_dir, env)?,
        poll_interval_s,
    })
}
