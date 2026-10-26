//! `tm doctor` row for credentials and loose modes in the managed `.claude.json` (#10041).
//!
//! Why: the managed `CLAUDE_CONFIG_DIR/.claude.json` is shared by every
//! tm-launched session. A fleet session found the slack-mcp bot token in
//! plaintext in its `mcpServers.slack-mcp.env` block, in a mode-0644 file any
//! local user could read. `tm mcp add` no longer writes a credential there
//! (`core::mcp_credential_env`); this row finds what an older install left, and
//! a mode that lets anyone else read the file.
//! What: [`scan_claude_json`] is the shared scan; [`check_claude_json_secrets`]
//! the read-only row. The repair is
//! [`super::doctor_claude_json_secrets_repair`]. Findings name SERVER and KEY
//! and the file MODE, never a value.
//! Test: `doctor_claude_json_secrets_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_common::credential_registry::provider_for_env_var;
use trusty_common::launchd_secrets::is_credential_env_key;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::mcp_credential_env::reads_credential_store;

/// The `tm doctor` row name, shared by the check and every repair step.
pub(crate) const CHECK_NAME: &str = "claude_json_secrets";

/// The file this row judges, inside the managed config dir.
pub(crate) const CLAUDE_JSON: &str = ".claude.json";

/// One `mcpServers.<server>.env.<key>` entry holding a credential value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvCredential {
    /// The `mcpServers` key.
    pub server: String,
    /// The env var name.
    pub key: String,
}

impl EnvCredential {
    /// `server.KEY`, for messages.
    pub(crate) fn label(&self) -> String {
        format!("{}.{}", self.server, self.key)
    }
}

/// How `--fix` may treat one env credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialClass {
    /// Registry-mapped, on a server that reads the store: `--fix` moves it.
    Migratable,
    /// No registry mapping: there is no store slot for it.
    Unmapped,
    /// Registry-mapped, but the server never reads tm's store (#10041), so
    /// moving it would launch the server without it.
    ThirdParty,
}

/// What the managed `.claude.json` and its siblings were found to hold.
///
/// Why: the row and the repair share one scan so they cannot disagree.
/// What: the path, its mode, credential entries split by whether the registry
/// maps them to a store slot, the reason the file could not be judged (if so),
/// and every `.claude.json*` sibling (backups, quarantined copies) readable
/// beyond its owner. Never a value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeJsonFinding {
    /// `<config_dir>/.claude.json`.
    pub path: PathBuf,
    /// Whether the file exists at all.
    pub exists: bool,
    /// Unix permission bits, when readable.
    pub mode: Option<u32>,
    /// Registry-mapped credentials of store-reading servers — `--fix`
    /// migrates these.
    pub migratable: Vec<EnvCredential>,
    /// Credential-shaped keys with no store slot — reported, never stripped.
    pub unmapped: Vec<EnvCredential>,
    /// Registry-mapped credentials of servers that never read tm's store —
    /// reported, never stripped (#10041).
    pub third_party: Vec<EnvCredential>,
    /// Why the file could not be judged.
    pub unreadable: Option<String>,
    /// `.claude.json*` siblings wider than 0600, with their modes.
    pub wide_siblings: Vec<(PathBuf, u32)>,
}

impl ClaudeJsonFinding {
    /// Is the file readable or writable by anyone but its owner?
    pub(crate) fn too_wide(&self) -> bool {
        self.mode.is_some_and(is_wide)
    }

    /// `0644`-style mode, or `unknown`.
    pub(crate) fn mode_text(&self) -> String {
        self.mode
            .map_or_else(|| "unknown".to_string(), |m| format!("{m:04o}"))
    }
}

/// Does `mode` grant anything to group or other?
pub(crate) fn is_wide(mode: u32) -> bool {
    mode & 0o077 != 0
}

/// Permission bits of `path`, when they can be read.
pub(crate) fn mode_of(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Every credential-keyed env entry in a parsed `.claude.json`.
///
/// What: walks `mcpServers.*.env` and returns `(entry, value, class)` for each
/// non-empty string value whose key is credential-shaped or registry-mapped;
/// `class` is [`CredentialClass`]. Values are returned for the repair only; the
/// scan drops them.
pub(crate) fn credential_entries(root: &Value) -> Vec<(EnvCredential, String, CredentialClass)> {
    let Some(servers) = root.get("mcpServers").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (server, entry) in servers {
        let Some(env) = entry.get("env").and_then(Value::as_object) else {
            continue;
        };
        // #10041: only a store-reading trusty server may lose its env credential.
        let reads_store = entry
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(reads_credential_store);
        for (key, value) in env {
            let Some(value) = value.as_str().filter(|v| !v.is_empty()) else {
                continue;
            };
            let mapped = provider_for_env_var(key).is_some();
            if mapped || is_credential_env_key(key) {
                let cred = EnvCredential {
                    server: server.clone(),
                    key: key.clone(),
                };
                let class = match (mapped, reads_store) {
                    (false, _) => CredentialClass::Unmapped,
                    (true, true) => CredentialClass::Migratable,
                    (true, false) => CredentialClass::ThirdParty,
                };
                out.push((cred, value.to_string(), class));
            }
        }
    }
    out
}

/// Scan `<config_dir>/.claude.json` and its siblings.
///
/// Why: one scan for the row and the repair. "Could not read" must never
/// render as clean, so a read or parse failure sets `unreadable`.
/// What: reads the mode, parses the JSON, classifies credential entries via
/// [`credential_entries`], and lists every `.claude.json*` sibling wider than
/// 0600. An absent file is a finding with `exists: false`.
/// Test: `scan_names_server_and_key_not_value`, `row_is_unknown_on_malformed_json`.
pub fn scan_claude_json(config_dir: &Path) -> ClaudeJsonFinding {
    let path = config_dir.join(CLAUDE_JSON);
    let mut finding = ClaudeJsonFinding {
        path: path.clone(),
        wide_siblings: wide_siblings(config_dir),
        ..ClaudeJsonFinding::default()
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return finding,
        Err(e) => {
            finding.exists = true;
            finding.mode = mode_of(&path);
            finding.unreadable = Some(format!("could not read it: {}", e.kind()));
            return finding;
        }
    };
    finding.exists = true;
    finding.mode = mode_of(&path);
    match serde_json::from_str::<Value>(&text) {
        Ok(root) => {
            for (cred, _value, class) in credential_entries(&root) {
                match class {
                    CredentialClass::Migratable => finding.migratable.push(cred),
                    CredentialClass::Unmapped => finding.unmapped.push(cred),
                    CredentialClass::ThirdParty => finding.third_party.push(cred),
                }
            }
        }
        Err(e) => {
            finding.unreadable = Some(format!(
                "not valid JSON (line {}, column {})",
                e.line(),
                e.column()
            ));
        }
    }
    finding
}

/// `.claude.json*` siblings (not the file itself) wider than 0600.
///
/// Why: `write_json_atomic` keeps a `.claude.json.bak`, and operators keep
/// dated copies; each can hold an older plaintext copy of the same credential.
fn wide_siblings(config_dir: &Path) -> Vec<(PathBuf, u32)> {
    let Ok(entries) = std::fs::read_dir(config_dir) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, u32)> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with(".claude.json") && name != CLAUDE_JSON
        })
        .filter_map(|e| {
            let p = e.path();
            mode_of(&p).filter(|m| is_wide(*m)).map(|m| (p, m))
        })
        .collect();
    out.sort();
    out
}

/// The `claude_json_secrets` row.
///
/// What: FAIL when an env block holds a credential value, when the file is
/// wider than 0600, or when a `.claude.json*` sibling is; UNKNOWN when the
/// file cannot be read or parsed or there is no managed dir; OK otherwise
/// (including an absent file).
/// Test: `row_fails_on_mode_0644`, `row_passes_on_mode_0600`,
/// `row_fails_on_a_plaintext_credential`, `row_is_unknown_on_malformed_json`,
/// `row_fails_on_a_wide_backup`.
pub(crate) fn check_claude_json_secrets(config_dir: Option<&Path>) -> DoctorCheck {
    let Some(config_dir) = config_dir else {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            "could not resolve the managed claude config dir — whether its .claude.json holds \
             a plaintext credential is UNKNOWN",
        );
    };
    build_row(&scan_claude_json(config_dir))
}

/// The pure verdict behind [`check_claude_json_secrets`].
pub(crate) fn build_row(f: &ClaudeJsonFinding) -> DoctorCheck {
    let path = f.path.display();
    let mut problems = Vec::new();
    if !f.migratable.is_empty() || !f.unmapped.is_empty() || !f.third_party.is_empty() {
        let keys: Vec<String> = f
            .migratable
            .iter()
            .chain(&f.unmapped)
            .chain(&f.third_party)
            .map(EnvCredential::label)
            .collect();
        problems.push(format!(
            "an MCP env block holds a plaintext credential ({})",
            keys.join(", ")
        ));
    }
    if !f.unmapped.is_empty() {
        let keys: Vec<String> = f.unmapped.iter().map(EnvCredential::label).collect();
        problems.push(format!(
            "`--fix` will NOT move {} — no credential provider is registered for it; move it to \
             `.env.local` by hand",
            keys.join(", ")
        ));
    }
    if !f.third_party.is_empty() {
        let keys: Vec<String> = f.third_party.iter().map(EnvCredential::label).collect();
        problems.push(format!(
            "`--fix` will NOT move {} — the server does not read tm's credential store, so it \
             would launch without it; supply it outside the MCP config by hand",
            keys.join(", ")
        ));
    }
    if f.too_wide() {
        problems.push(format!(
            "the file is mode {}, readable beyond its owner",
            f.mode_text()
        ));
    }
    if !f.wide_siblings.is_empty() {
        let list: Vec<String> = f
            .wide_siblings
            .iter()
            .map(|(p, m)| format!("{} ({m:04o})", p.display()))
            .collect();
        problems.push(format!(
            "{} sibling copies are readable beyond their owner and may hold an older \
             plaintext copy: {}",
            list.len(),
            list.join("; ")
        ));
    }
    if !problems.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "{path}: {}. Run `tm doctor --fix --yes` to move each registered credential into \
                 the credential store and tighten the files to 0600, then ROTATE any credential \
                 that sat in the file",
                problems.join("; ")
            ),
        );
    }
    if let Some(why) = &f.unreadable {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!("{path}: {why} — whether it holds a plaintext credential is UNKNOWN"),
        );
    }
    if !f.exists {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Ok,
            format!("{path} does not exist"),
        );
    }
    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Ok,
        format!(
            "{path} is mode {} and no MCP env block holds a credential value",
            f.mode_text()
        ),
    )
}

#[cfg(test)]
#[path = "doctor_claude_json_secrets_tests.rs"]
mod tests;
