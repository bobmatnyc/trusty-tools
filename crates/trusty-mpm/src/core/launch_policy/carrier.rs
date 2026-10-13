//! The per-session `--settings` carrier for the owner policy (#8988 PR 1b).
//!
//! Why: Claude Code loads every user-scope MCP server and every claude.ai
//! connector into every session. A per-session settings file passed as
//! `--settings <file>` is the one carrier that subtracts them without writing
//! into a repository or into the operator's own settings (Architect df-D0d).
//! What: [`provision_settings_at`] resolves the owner layer
//! ([`super::resolve_and_record`]), turns its policy into a deny set
//! ([`deny_set`]: allow applied, then deny, the floor never named), and writes
//! `{"deniedMcpServers":[…],"disableClaudeAiConnectors":true}` (each key only
//! when it has something to say) to
//! `<state_root>/session-settings/<sha256 of the body>.json`, mode `0600`. The
//! name is the body's hash, so a file once written is never rewritten with other
//! contents: two launches in one cwd that read different `.claude.json` files
//! get two files, and identical policies share one. The directory is bounded by
//! the number of distinct policies; nothing deletes a file, because another
//! live session may have it as its `--settings`. No owner
//! layer, or a policy that subtracts nothing, writes nothing and adds no argv —
//! the launch is byte-identical to one before #8988.
//! Server names compare EXACTLY, case-sensitively, everywhere — the floor
//! check, the allow subtraction and the emitted names — because that is how
//! Claude Code matches `serverName`, so a differently-cased entry can neither
//! deny a floor server nor be dropped as one.
//! Fail-open: a bad owner file uses the last good policy; an unreadable
//! `.claude.json` keeps the explicit denies; an unwritable settings file
//! launches with NO `--settings` and a notice that says the policy was not
//! applied. Every arm prints its notice; none refuses the launch.
//! Test: `carrier_tests.rs`, `tests/owner_launch_config_8988.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::compose::OwnerPolicy;
use super::floor::is_floor;
use super::{Outcome, OwnerInputs, OwnerResolution, resolve_and_record};

/// Directory under the state root holding every per-session settings file.
pub const SESSION_SETTINGS_DIR: &str = "session-settings";

/// The Claude Code flag that loads an extra settings file.
pub const SETTINGS_FLAG: &str = "--settings";

/// What the owner layer adds to one launch.
///
/// Test: `a_written_policy_is_one_settings_argv_pair`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchSettings {
    /// The settings file written for this launch; `None` adds nothing.
    pub file: Option<PathBuf>,
    /// The warning this launch printed, if any.
    pub notice: Option<String>,
}

impl LaunchSettings {
    /// [`settings_argv`] for this launch's file.
    pub fn argv(&self) -> Vec<String> {
        settings_argv(self.file.as_deref())
    }

    /// [`Self::argv`] for a line a shell re-splits: each token single-quoted,
    /// each with a leading space.
    ///
    /// Test: `shell_words_quote_the_path`.
    pub fn shell_words(&self) -> String {
        self.argv()
            .iter()
            .map(|a| {
                format!(
                    " {}",
                    crate::core::spawn_disclaim::pane::shell_single_quote(a)
                )
            })
            .collect()
    }
}

/// `["--settings", "<file>"]`, unquoted (exec and launch-spec argv), or empty.
///
/// Test: `a_written_policy_is_one_settings_argv_pair`.
pub fn settings_argv(file: Option<&Path>) -> Vec<String> {
    file.iter()
        .flat_map(|f| [SETTINGS_FLAG.to_owned(), f.display().to_string()])
        .collect()
}

/// The two composed files a relocated CLI launch names.
///
/// Why: `tm launch`, `tm connect` and the Architect pass one value through
/// `launch.rs`, which sits at the SLOC cap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchFiles {
    /// The session-MCP file (`--mcp-config`).
    pub mcp_config: Option<PathBuf>,
    /// The owner settings file (`--settings`).
    pub settings: Option<PathBuf>,
}

/// What [`provision_settings_at`] reads from and writes to.
#[derive(Debug, Clone)]
pub struct CarrierInputs<'a> {
    /// The session's working directory.
    pub cwd: &'a Path,
    /// The managed repos root.
    pub repos_root: &'a Path,
    /// tm's state root (`~/.trusty-tools/trusty-mpm`).
    pub state_root: &'a Path,
    /// The directory whose `.claude.json` this launch's `claude` reads; `None`
    /// when it cannot be located.
    pub claude_json_dir: Option<&'a Path>,
}

/// The servers to deny: `allow` applied to `user_scope`, then `deny`.
///
/// What: every `user_scope` name not in `allow` (when `allow` is set and the
/// user scope was readable), plus every `deny` name; never a floor name. All
/// comparisons are exact and case-sensitive (see the module doc).
/// Test: `allow_empty_denies_every_non_floor_server`,
/// `a_differently_cased_name_is_another_server`.
pub fn deny_set(policy: &OwnerPolicy, user_scope: Option<&[String]>) -> BTreeSet<String> {
    let mut deny: BTreeSet<String> = policy.deny.clone();
    if let (Some(allow), Some(user)) = (&policy.allow, user_scope) {
        deny.extend(user.iter().filter(|n| !allow.contains(*n)).cloned());
    }
    deny.retain(|name| !is_floor(name));
    deny
}

/// The settings JSON for a deny set, or `None` when it subtracts nothing.
///
/// What: `deniedMcpServers` (sorted `{"serverName": …}` entries) when `deny`
/// is non-empty, `"disableClaudeAiConnectors": true` when `connectors_off`.
/// Never an allow-type key and never `disableClaudeAiConnectors: false`
/// (#7422 trust gate: tm only subtracts).
/// Test: `the_settings_body_carries_only_subtractive_keys`.
pub fn settings_body(deny: &BTreeSet<String>, connectors_off: bool) -> Option<String> {
    let mut map = serde_json::Map::new();
    if !deny.is_empty() {
        let entries = deny.iter().map(|n| json!({ "serverName": n })).collect();
        map.insert("deniedMcpServers".to_owned(), Value::Array(entries));
    }
    if connectors_off {
        map.insert("disableClaudeAiConnectors".to_owned(), Value::Bool(true));
    }
    (!map.is_empty()).then(|| format!("{:#}", Value::Object(map)))
}

/// Where the settings file for `body` lives:
/// `<state_root>/session-settings/<sha256 hex of body>.json`.
///
/// Why: the deny set depends on the `.claude.json` a launch read as well as on
/// the cwd, so a cwd-keyed name let one launch overwrite another's file inside
/// its write-to-startup window (#8988).
/// What: content-addressed, so a path never holds two different bodies.
/// Test: `launches_in_one_cwd_with_different_claude_json_get_different_files`,
/// `identical_policies_in_one_cwd_share_one_file`.
pub fn settings_path_at(state_root: &Path, body: &str) -> PathBuf {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(body.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    state_root
        .join(SESSION_SETTINGS_DIR)
        .join(format!("{hex}.json"))
}

/// Resolve the owner layer and write this launch's settings file.
///
/// Why: see the module doc.
/// What: no effective policy → no file. Otherwise the user scope is read from
/// `claude_json_dir/.claude.json` ([`crate::core::session_mcp_scope::user_scope_servers`]),
/// the deny set composed, and the body written atomically. Each failure arm is
/// a notice, printed to stderr and logged, never an error.
/// Test: every test in `carrier_tests.rs`.
pub fn provision_settings_at(inputs: &CarrierInputs<'_>) -> LaunchSettings {
    let resolution = resolve_and_record(&OwnerInputs {
        cwd: inputs.cwd,
        repos_root: inputs.repos_root,
        state_root: inputs.state_root,
    });
    let mut notices: Vec<String> = resolution_notice(&resolution).into_iter().collect();
    let settings = match resolution.effective() {
        None => None,
        Some(policy) => write_policy(inputs, policy, &mut notices),
    };
    let notice = (!notices.is_empty()).then(|| notices.join("; "));
    if let Some(text) = &notice {
        eprintln!("tm: warning: {text}");
        tracing::warn!(cwd = %inputs.cwd.display(), "owner launch policy: {text} (#8988)");
    }
    LaunchSettings {
        file: settings,
        notice,
    }
}

/// Compose and write `policy`; `None` when there is nothing to write or the
/// write failed (with a notice saying the policy was NOT applied).
fn write_policy(
    inputs: &CarrierInputs<'_>,
    policy: &OwnerPolicy,
    notices: &mut Vec<String>,
) -> Option<PathBuf> {
    let user = match inputs.claude_json_dir {
        Some(dir) => crate::core::session_mcp_scope::user_scope_servers(dir),
        None => Err("no home directory to locate .claude.json".to_owned()),
    };
    let user = match user {
        Ok(names) => Some(names),
        Err(reason) if policy.allow.is_some() => {
            notices.push(format!(
                "{reason}; the owner `allow` list could not be applied, only its explicit denies"
            ));
            None
        }
        Err(_) => None,
    };
    let deny = deny_set(policy, user.as_deref());
    let body = settings_body(&deny, policy.claude_ai_connectors == Some(false))?;
    let path = settings_path_at(inputs.state_root, &body);
    match write_owner_only(&path, body.as_bytes()) {
        Ok(()) => Some(path),
        // #8988 Fail-Open Check: launch without the flag, and say so.
        Err(err) => {
            notices.push(format!(
                "owner MCP policy NOT applied: could not write {}: {err}; this session \
                 loads every user-scope MCP server",
                path.display()
            ));
            None
        }
    }
}

/// The notice an owner resolution owes the operator, if any.
fn resolution_notice(resolution: &OwnerResolution) -> Option<String> {
    let file = resolution
        .file
        .as_deref()
        .map(|f| f.display().to_string())
        .unwrap_or_default();
    match &resolution.outcome {
        Outcome::Fallback { error, last_good } => Some(format!(
            "owner config {file} INVALID ({error}); using last good policy from {}",
            last_good.recorded_at
        )),
        Outcome::Skipped {
            error,
            last_good_error,
        } => Some(format!(
            "owner config {file} INVALID ({error}); no last good policy{}, owner layer skipped",
            last_good_error
                .as_deref()
                .map(|e| format!(" ({e})"))
                .unwrap_or_default()
        )),
        Outcome::NoFile {
            retire_error: Some(err),
            ..
        } => Some(format!(
            "owner config {file} is gone but its last good record could not be retired \
             ({err}); the owner layer is off for this launch, but a later bad file would \
             fall back to that stale record"
        )),
        Outcome::NoOwner | Outcome::NoFile { .. } | Outcome::Loaded(_) => None,
    }
}

/// Write `bytes` to `path` atomically, owner-only (`0600` file, `0700` dir).
fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    crate::core::home_write_fence::check(path); // #8545
    let dir = path.parent().unwrap_or(path);
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = options
        .open(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, bytes))
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// [`provision_settings_at`] for a production launch under `state_root`.
///
/// Why: every launch path makes the same call; only the state root and the
/// relocated config dir differ.
/// What: the repos root from [`crate::core::repo_paths::repos_root`]; the
/// `.claude.json` directory is `config_dir` for a relocated launch, else the
/// one an unrelocated `claude` reads: `$CLAUDE_CONFIG_DIR` when set, else the
/// home directory (`~/.claude.json`).
/// Test: `an_unrelocated_launch_reads_the_operators_claude_json`.
pub fn provision_settings(
    state_root: &Path,
    cwd: &Path,
    config_dir: Option<&Path>,
) -> LaunchSettings {
    let repos_root = crate::core::repo_paths::repos_root();
    let claude_json_dir = claude_json_dir(config_dir);
    provision_settings_at(&CarrierInputs {
        cwd,
        repos_root: &repos_root,
        state_root,
        claude_json_dir: claude_json_dir.as_deref(),
    })
}

/// [`provision_settings`] under the process home's state root; nothing when
/// no home resolves.
pub fn provision_settings_default(cwd: &Path, config_dir: Option<&Path>) -> LaunchSettings {
    super::state_root()
        .map(|root| provision_settings(&root, cwd, config_dir))
        .unwrap_or_default()
}

/// The directory whose `.claude.json` a launch reads.
///
/// Test: `an_unrelocated_launch_reads_the_operators_claude_json`.
pub fn claude_json_dir(config_dir: Option<&Path>) -> Option<PathBuf> {
    config_dir
        .map(Path::to_path_buf)
        .or_else(crate::core::session_record::claude_config_dir_override)
        .or_else(dirs::home_dir)
}

#[cfg(test)]
#[path = "carrier_tests.rs"]
mod tests;
