//! Per-role Claude Code sandbox settings (#8928 step 5).
//!
//! Why: #8928 moves PM and agent sessions onto the Claude Code sandbox. The
//! owner's 2026-09-30 rulings on #8928: the policy lives in the project
//! `.claude/settings.json`, with `network.strictAllowlist` and the credentials
//! deny in tm's user-level settings file, because project settings ignore
//! both; the tm CLI reaches the daemon over `trusty-mpm.sock` with no
//! `allowLocalBinding`; there is no blanket `.claude/**` Write deny; and there
//! is one tmux server per workspace. The sccache socket is never allowed: the
//! sccache server runs whatever compiler a client names, outside the sandbox,
//! so sandboxed sessions build with `RUSTC_WRAPPER` empty (no sccache).
//! What: [`project_policy`] and [`user_policy`] render the two tiers for a
//! [`SandboxRole`] from explicit [`SandboxInputs`]; [`write_sandbox_settings`]
//! merges them into the project file (through `merge_settings`, under the
//! settings lock) and into the user file (under the same lock), then
//! [`verify_sandbox_settings`] reads both back and refuses the launch when
//! the merged result is not the sandbox it asked for. Nothing calls this yet:
//! wiring every launch path is step 6.
//! Test: `sandbox_settings_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::PrepError;
pub use super::sandbox_settings_verify::verify_sandbox_settings;

/// Which policy a session gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SandboxRole {
    /// A PM: sandboxed, and may reach its workspace's private tmux server.
    Pm,
    /// A dispatched agent: the PM policy without the tmux socket.
    Agent,
    /// The Architect: `sandbox.enabled: false` (it runs unsandboxed, #8928).
    Architect,
}

/// The absolute paths a policy names. The caller resolves them (step 6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SandboxInputs {
    /// The workspace (project root) the session runs in.
    pub workspace: PathBuf,
    /// The operator's home directory.
    pub home: PathBuf,
    /// The workspace's private tmux socket, resolved (`/private/tmp/...`).
    pub tmux_socket: Option<PathBuf>,
    /// The daemon's `trusty-mpm.sock`.
    pub mpm_socket: Option<PathBuf>,
    /// The shared `CARGO_TARGET_DIR`, which cargo must be able to write.
    pub cargo_target_dir: Option<PathBuf>,
}

impl SandboxInputs {
    /// Inputs for `workspace` under `home`, with no optional path set.
    pub fn new(workspace: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            home: home.into(),
            ..Self::default()
        }
    }
}

/// Hosts a sandboxed session may reach (#8928).
pub const ALLOWED_DOMAINS: [&str; 6] = [
    "github.com",
    "api.github.com",
    "crates.io",
    "static.crates.io",
    "index.crates.io",
    "registry.npmjs.org",
];

/// Environment variables a sandboxed command never sees, delivered through the
/// user tier because project settings drop the credentials deny (#8928). `GH_TOKEN`/`GITHUB_TOKEN` are left out: `gh` in the
/// sandbox authenticates with them when a spawn sets them.
pub const DENIED_ENV_VARS: [&str; 11] = [
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "TELEGRAM_BOT_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "CARGO_REGISTRY_TOKEN",
    "NPM_TOKEN",
];

/// The project-tier keys for `role`: `sandbox` and the `permissions.deny`
/// entries it needs.
///
/// What: PM and Agent get `enabled`, `failIfUnavailable`,
/// `autoAllowBashIfSandboxed` and `enableWeakerNetworkIsolation` true,
/// `allowUnsandboxedCommands` false, the secret `denyRead` and trust-anchor
/// `denyWrite` lists, cargo/npm `allowWrite` entries plus the target dir, the
/// [`ALLOWED_DOMAINS`], `allowUnixSockets` naming `trusty-mpm.sock` and — PM
/// only — the tmux socket, and `env.RUSTC_WRAPPER` set to `""`. No
/// `allowLocalBinding` and no blanket `.claude/**` rule (#8928 rulings). Never
/// the sccache socket: whoever connects names the compiler, and the server
/// runs it outside the sandbox, so `RUSTC_WRAPPER=""` turns sccache off (#8928
/// fix round). The Architect gets `enabled: false` only.
/// Test: `the_pm_policy_names_every_socket_and_no_local_binding`,
/// `the_agent_policy_has_no_tmux_socket`, `the_architect_policy_disables_the_sandbox`,
/// `sandboxed_roles_never_reach_the_sccache_socket`.
pub fn project_policy(role: SandboxRole, inputs: &SandboxInputs) -> Value {
    if role == SandboxRole::Architect {
        return json!({ "sandbox": { "enabled": false } });
    }
    let home = &inputs.home;
    let ws = &inputs.workspace;
    let anchors = [
        home.join(".trusty-mpm/config.toml"),
        home.join(".trusty-mpm/architect-launch"),
        home.join(".trusty-mpm/twin/armed"),
    ];
    let secrets = [home.join(".ssh"), home.join(".aws"), ws.join(".env")];
    let mut allow_write = vec![
        home.join(".cargo/registry"),
        home.join(".cargo/git"),
        home.join(".npm"),
        home.join(".trusty-mpm/build-slots"),
    ];
    allow_write.extend(inputs.cargo_target_dir.iter().cloned());
    let tmux = (role == SandboxRole::Pm)
        .then_some(inputs.tmux_socket.as_ref())
        .flatten();
    // #8928 fix round: never the sccache socket — see the doc above.
    let sockets: Vec<&PathBuf> = tmux.into_iter().chain(inputs.mpm_socket.as_ref()).collect();
    let mut deny: Vec<String> = Vec::new();
    deny.extend(
        secrets[..2]
            .iter()
            .map(|p| format!("Read(/{}/**)", p.display())),
    );
    deny.push(format!("Read(/{})", secrets[2].display()));
    deny.push(format!("Edit(/{})", anchors[0].display()));
    deny.extend(
        anchors[1..]
            .iter()
            .map(|p| format!("Edit(/{}/**)", p.display())),
    );
    // #8928: exact settings files, never a blanket `.claude/**` rule.
    for file in [
        ".claude/settings.json",
        ".claude/settings.local.json",
        ".mcp.json",
    ] {
        deny.push(format!("Edit(/{})", ws.join(file).display()));
    }
    json!({
        "env": { "RUSTC_WRAPPER": "" },
        "permissions": { "deny": deny },
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "allowUnsandboxedCommands": false,
            "autoAllowBashIfSandboxed": true,
            "enableWeakerNetworkIsolation": true,
            "filesystem": {
                "denyRead": paths(&secrets),
                "denyWrite": paths(&anchors),
                "allowWrite": paths(&allow_write),
            },
            "network": {
                "allowedDomains": ALLOWED_DOMAINS,
                "allowUnixSockets": sockets.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            },
        },
    })
}

/// The user-tier keys for `role`: `network.strictAllowlist` and the
/// credentials deny, which only the user tier honors (#8928). The Architect
/// gets nothing.
/// Test: `the_user_policy_carries_strict_allowlist_and_credentials`.
pub fn user_policy(role: SandboxRole) -> Option<Value> {
    if role == SandboxRole::Architect {
        return None;
    }
    let env: Vec<Value> = DENIED_ENV_VARS
        .iter()
        .map(|name| json!({ "name": name, "mode": "deny" }))
        .collect();
    Some(json!({
        "sandbox": {
            "network": { "strictAllowlist": true },
            "credentials": { "envVars": env },
        },
    }))
}

/// Why writing or verifying the sandbox settings failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxSettingsError {
    /// The project settings write failed.
    #[error("writing the project sandbox settings failed: {0}")]
    Project(#[from] PrepError),
    /// The user settings file could not be locked, read or written.
    #[error("writing the user sandbox settings `{path}` failed: {source}")]
    User {
        /// The user settings file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A settings file could not be read back or is not a JSON object.
    #[error("sandbox settings `{path}` cannot be read back: {why}")]
    Unreadable {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        why: String,
    },
    /// The merged settings are not the sandbox the role needs; the launch is
    /// refused.
    #[error("launch refused: {0}")]
    Refused(String),
}

/// Write `role`'s policy into the project and user tiers, then verify.
///
/// What: merges [`project_policy`] into `<workspace>/.claude/settings.json`
/// and [`user_policy`] into `<user_config_dir>/settings.json` with
/// [`merge_policy`], each under the settings lock, then runs
/// [`verify_sandbox_settings`].
///
/// # Errors
///
/// [`SandboxSettingsError`] on a write failure or a refused read-back.
///
/// Test: `writing_preserves_unrelated_keys_and_verifies`,
/// `a_local_override_refuses_the_launch`.
pub fn write_sandbox_settings(
    role: SandboxRole,
    inputs: &SandboxInputs,
    user_config_dir: &Path,
) -> Result<(), SandboxSettingsError> {
    let policy = project_policy(role, inputs);
    super::settings::merge_settings(&inputs.workspace, |s| merge_policy(s, &policy))?;
    if let Some(policy) = user_policy(role) {
        let path = user_config_dir.join("settings.json");
        let user = |source| SandboxSettingsError::User {
            path: path.clone(),
            source,
        };
        std::fs::create_dir_all(user_config_dir).map_err(user)?;
        crate::core::settings_lock::with_settings_lock(&path, || {
            let mut settings = super::malformed_backup::load_settings_object(&path)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            merge_policy(&mut settings, &policy);
            crate::core::settings_lock::publish(&path, &settings)
        })
        .and_then(|inner| inner)
        .map_err(user)?;
    }
    verify_sandbox_settings(role, &inputs.workspace, user_config_dir)
}

/// Merge `policy` into `settings`: tm owns the `sandbox` keys it names, which
/// replace what was there; `permissions.deny` gains each entry it lacks, and
/// every other key and entry is kept.
/// Test: `writing_preserves_unrelated_keys_and_verifies`.
pub fn merge_policy(settings: &mut Value, policy: &Value) {
    if !settings.is_object() {
        *settings = json!({});
    }
    if let Some(sandbox) = policy.get("sandbox").and_then(Value::as_object) {
        let slot = &mut settings["sandbox"];
        if !slot.is_object() {
            *slot = json!({});
        }
        for (key, value) in sandbox {
            slot[key] = value.clone();
        }
    }
    let wanted = policy
        .pointer("/permissions/deny")
        .and_then(Value::as_array);
    if let Some(wanted) = wanted {
        let perms = &mut settings["permissions"];
        if !perms.is_object() {
            *perms = json!({});
        }
        let deny = &mut perms["deny"];
        if !deny.is_array() {
            *deny = json!([]);
        }
        if let Some(list) = deny.as_array_mut() {
            for entry in wanted {
                if !list.contains(entry) {
                    list.push(entry.clone());
                }
            }
        }
    }
}

/// Each path as a JSON string.
fn paths(list: &[PathBuf]) -> Vec<String> {
    list.iter().map(|p| p.display().to_string()).collect()
}

#[cfg(test)]
#[path = "sandbox_settings_tests.rs"]
mod tests;
