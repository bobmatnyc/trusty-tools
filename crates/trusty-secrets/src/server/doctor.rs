//! `secrets.doctor`: which backends this server opens, why the others do not,
//! and where it looks (#7519 P4, DOC-74 §7).
//!
//! Why: a bare `available: false` left an operator guessing whether to
//! rebuild, edit the account's machine config, install a CLI or fix a
//! tracked file (A4). Doctor must also never spawn `op` or `keeper` (A9)
//! and never carry the 1Password service-account token (A10).
//! What: [`doctor`] builds a [`DoctorResponse`]. The selected backend comes
//! from the same configs a request reads (`project::selecting_machine`).
//! Each row is judged without a spawn: a CLI-backed id this build does not
//! link is [`Unavailable::NotCompiled`]; otherwise its enablement is read
//! from the account's own machine config (`State::file_consent_config`,
//! ruling 74), then the factory opens it — opening spawns nothing — and an
//! open error is folded to its [`Unavailable`] kind with the error's own
//! text as the detail. A project whose tracked config is refused still gets
//! a report; its selected row carries [`Unavailable::TrackedSettingRefused`].
//! Headless readiness is whether the token was present at start, yes or no.
//! Test: `doctor_tests.rs` beside this module,
//! `server_doctor_reports_backends_and_paths_only`,
//! `server_doctor_lists_onepassword_without_spawning`,
//! `server_doctor_lists_keeper_without_spawning`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::errors::ErrorKind;
use super::methods::to_json;
use super::project::{self, ProjectContext, RefusedProject};
use super::router::State;
use crate::api::{BackendId, SecretsError};
use crate::store::config::{self, MachineSecretsConfig};
use crate::store::{Capabilities, cli_backends};

/// `secrets.doctor` — not among S1's method names.
pub const DOCTOR: &str = "secrets.doctor";

/// Every CLI-backed backend id, linked into this build or not; doctor
/// lists them in this order after `keychain` and `file`.
const CLI_IDS: [&str; 2] = [BackendId::ONEPASSWORD, BackendId::KEEPER];

/// The detail for a CLI-backed row on a build without `cli-backends`.
const NO_CLI_BACKENDS: &str = "this trusty-secrets build links no CLI-backed backends; \
     install it with `--features cli-backends` (Unix only)";

/// The detail for a CLI-backed row when the account's home is unknown.
const NO_ACCOUNT_HOME: &str = "the account's home directory is unknown, so no account \
     machine config can enable it";

/// `secrets.doctor` params: an optional project.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DoctorRequest {
    #[serde(default)]
    project: Option<PathBuf>,
}

/// Why a backend row is unavailable (#7519 P4).
///
/// Why: A4 — each cause has its own fix, so the row names the cause.
/// What: one kind per cause; [`BackendStatus::detail`] carries the fix. An
/// unknown wire value decodes as [`Unavailable::Other`].
/// Test: `doctor_reasons_name_each_cause`,
/// `doctor_unknown_reason_decodes_as_other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Unavailable {
    /// This build does not link the backend.
    NotCompiled,
    /// The account's machine config does not enable it.
    NotEnabled,
    /// Its CLI is missing, relative, or not executable.
    CliNotInstalled,
    /// A config it reads is unreadable, does not parse, or is refused.
    ConfigInvalid,
    /// The project's tracked config sets something only the machine may.
    TrackedSettingRefused,
    /// It reported itself locked or signed out.
    Locked,
    /// It failed to open for another reason; the detail says which.
    OpenFailed,
    /// A reason this client does not know.
    // #7519 P4: `serde(other)`, so a reason added later never fails a decode.
    #[serde(other)]
    Other,
}

impl Unavailable {
    /// The wire name, e.g. `not_enabled`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotCompiled => "not_compiled",
            Self::NotEnabled => "not_enabled",
            Self::CliNotInstalled => "cli_not_installed",
            Self::ConfigInvalid => "config_invalid",
            Self::TrackedSettingRefused => "tracked_setting_refused",
            Self::Locked => "locked",
            Self::OpenFailed => "open_failed",
            Self::Other => "other",
        }
    }

    /// The kind of a factory open error.
    fn of(error: &SecretsError) -> Self {
        match error {
            SecretsError::UnknownBackend { .. } => Self::NotCompiled,
            SecretsError::BackendNotEnabled { .. } => Self::NotEnabled,
            SecretsError::CliNotInstalled { .. } => Self::CliNotInstalled,
            SecretsError::Config { .. }
            | SecretsError::StorageRefused { .. }
            | SecretsError::Io { .. } => Self::ConfigInvalid,
            SecretsError::TrackedCliSettingRefused { .. }
            | SecretsError::TrackedBackendRefused { .. } => Self::TrackedSettingRefused,
            SecretsError::BackendLocked { .. } => Self::Locked,
            _ => Self::OpenFailed,
        }
    }
}

/// One backend's row in the doctor table.
///
/// What: `available` means this server opens the backend now. Doctor never
/// spawns a CLI, so it never says whether a CLI backend is unlocked; for
/// 1Password headless use read [`DoctorResponse::headless`]. When
/// `available` is false, `reason` and `detail` say why (#7519 P4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BackendStatus {
    /// The backend id.
    pub id: BackendId,
    /// Whether this server opens it now.
    pub available: bool,
    /// Its capability flags, by name.
    pub capabilities: Vec<String>,
    /// Why it is unavailable. `None` when available, or from a server
    /// older than #7519 P4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Unavailable>,
    /// The fix, as an error's text: paths, ids and fixed hints, never a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl BackendStatus {
    fn available(id: BackendId, caps: Capabilities) -> Self {
        Self {
            id,
            available: true,
            capabilities: capability_names(caps),
            reason: None,
            detail: None,
        }
    }

    fn unavailable(id: BackendId, reason: Unavailable, detail: String) -> Self {
        Self {
            id,
            available: false,
            capabilities: Vec::new(),
            reason: Some(reason),
            detail: Some(detail),
        }
    }
}

/// How the selected backend keeps values at rest (#9326).
///
/// Why: owner ruling f5 — the 0600 file backend is a degraded posture, and
/// doctor must say so whether config chose it or the host has no Keychain.
/// What: decided from the selected backend id alone. An unknown wire value
/// decodes as [`StoragePosture::Other`].
/// Test: `server_doctor_reports_the_file_posture`,
/// `server_unknown_posture_decodes_as_other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StoragePosture {
    /// The OS Keychain holds values.
    Keychain,
    /// Values are plaintext 0600 files in 0700 directories: degraded.
    FileDegraded,
    /// Another backend; its row in [`DoctorResponse::backends`] describes it.
    /// Also what an older client decodes a posture it does not know as.
    // #9326: `serde(other)`, so a variant added later never fails a decode.
    #[serde(other)]
    Other,
}

impl StoragePosture {
    /// The posture of backend `id`.
    pub fn of(id: &BackendId) -> Self {
        match id.as_str() {
            BackendId::KEYCHAIN => Self::Keychain,
            BackendId::FILE => Self::FileDegraded,
            _ => Self::Other,
        }
    }
}

/// What a headless run of this server can use (#7519 P4, DOC-74 §13 Q4).
///
/// Why: A4 — headless 1Password works only with a service-account token,
/// and owner ruling Q1 lets doctor say only whether one was present.
/// What: presence at server start, yes or no; never the value, its length
/// or a prefix. Keeper's device approval is not detected (ruling 6).
/// Test: `doctor_reports_token_presence_and_never_the_token`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct HeadlessReadiness {
    /// Whether `OP_SERVICE_ACCOUNT_TOKEN` was set when the server started.
    #[serde(default)]
    pub onepassword_token: bool,
}

/// `secrets.doctor` response: backend availability and paths only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DoctorResponse {
    /// The socket this server answers on.
    pub socket: PathBuf,
    /// The names-only index directory.
    pub index_root: PathBuf,
    /// The machine config file the selected backend is read from.
    pub machine_config: PathBuf,
    /// The project's checkout root, when a project was named.
    pub project_root: Option<PathBuf>,
    /// The project config file, when a project was named.
    pub project_config: Option<PathBuf>,
    /// The backend the §6.1 precedence selects.
    pub selected_backend: BackendId,
    /// Every backend this server knows, plus the selected one.
    pub backends: Vec<BackendStatus>,
    /// The selected backend's at-rest posture. `None` only when decoding an
    /// answer from a server older than #9326.
    #[serde(default)]
    pub posture: Option<StoragePosture>,
    /// The account's own machine config, the one file that enables a CLI
    /// backend (ruling 74). `None` when the account's home is unknown, or
    /// from a server older than #7519 P4.
    #[serde(default)]
    pub account_config: Option<PathBuf>,
    /// Headless readiness. `None` only from a server older than #7519 P4.
    #[serde(default)]
    pub headless: Option<HeadlessReadiness>,
}

/// What a doctor call reports about its project, if it named one.
struct Target {
    root: Option<PathBuf>,
    config: Option<PathBuf>,
    selected: BackendId,
    /// The tracked-config refusal every request in this project meets.
    refused: Option<String>,
}

impl Target {
    /// Resolve the project in `request`, as a request would.
    ///
    /// What: no project selects from the machine config alone. A tracked
    /// refusal answers [`RefusedProject`]; any other error is returned.
    fn of(state: &State, request: &DoctorRequest) -> Result<Self, ErrorKind> {
        let Some(dir) = request.project.as_deref() else {
            let machine = project::selecting_machine(state)?;
            return Ok(Self {
                root: None,
                config: None,
                selected: config::resolve(None, machine.as_ref()).backend,
                refused: None,
            });
        };
        match ProjectContext::resolve(state, dir) {
            Ok(project) => Ok(Self {
                root: Some(project.root().to_path_buf()),
                config: Some(project.config_path()),
                selected: project.resolved_config().backend,
                refused: None,
            }),
            // #7519 P4: report the refusal on the selected row, not as a failed call.
            Err(kind) if is_tracked_refusal(kind) => {
                let refused = RefusedProject::resolve(state, dir, kind)?;
                Ok(Self {
                    config: Some(refused.root.join(project::PROJECT_CONFIG_SUBPATH)),
                    root: Some(refused.root),
                    selected: refused.backend,
                    refused: Some(refused.detail),
                })
            }
            Err(kind) => Err(kind),
        }
    }
}

fn is_tracked_refusal(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::TrackedBackendRefused
            | ErrorKind::TrackedCliSettingRefused
            | ErrorKind::TrackedAuditRefused
    )
}

/// The account's own machine config, as doctor judges CLI rows against it.
enum Account {
    /// The account's home is unknown.
    Unknown,
    /// The file is unreadable or does not parse; the error's text.
    Invalid(String),
    /// The file, and its section when present.
    Loaded(PathBuf, Option<MachineSecretsConfig>),
}

impl Account {
    fn load(path: Option<&Path>) -> Self {
        match path {
            None => Self::Unknown,
            Some(path) => match config::load_machine_at(path) {
                Ok(machine) => Self::Loaded(path.to_path_buf(), machine),
                Err(e) => Self::Invalid(e.to_string()),
            },
        }
    }

    /// `Ok` when this file enables `id`, else the reason and detail.
    fn enables(&self, id: &BackendId) -> Result<(), (Unavailable, String)> {
        match self {
            Self::Unknown => Err((Unavailable::NotEnabled, NO_ACCOUNT_HOME.to_string())),
            // #7519 P4: unreadable is not "off"; the factory folds both to off.
            Self::Invalid(detail) => Err((Unavailable::ConfigInvalid, detail.clone())),
            Self::Loaded(_, Some(machine)) if machine.enables(id) => Ok(()),
            Self::Loaded(path, _) => Err((
                Unavailable::NotEnabled,
                format!(
                    "the account machine config {} does not enable it; add a `secrets.{id}:` \
                     section or `secrets.default_backend: {id}` there",
                    path.display()
                ),
            )),
        }
    }
}

/// Judge backend `id` without spawning anything.
///
/// What: see the module docs. The factory is called only for an id that
/// passed the build and enablement checks; opening spawns no process.
fn judge(state: &State, account: Option<&Account>, id: BackendId) -> BackendStatus {
    if CLI_IDS.contains(&id.as_str()) {
        if !cli_backends().contains(&id) {
            return BackendStatus::unavailable(
                id,
                Unavailable::NotCompiled,
                NO_CLI_BACKENDS.into(),
            );
        }
        // #7519: ruling 74 — only the account's own file enables a CLI backend.
        if let Some(account) = account
            && let Err((reason, detail)) = account.enables(&id)
        {
            return BackendStatus::unavailable(id, reason, detail);
        }
    }
    match (state.backends)(&id) {
        Ok(backend) => BackendStatus::available(id, backend.capabilities()),
        Err(e) => BackendStatus::unavailable(id, Unavailable::of(&e), e.to_string()),
    }
}

/// `secrets.doctor`: which backends this server opens, why the others do
/// not, and where it looks.
///
/// Why: see the module docs.
/// What: rows for `keychain`, `file`, `onepassword` and `keeper`, in that
/// order, then the selected backend when it is none of them. Nothing here
/// reads a secret, spawns a CLI, or writes an audit record.
/// Test: `server_doctor_reports_backends_and_paths_only`,
/// `server_doctor_reports_the_file_posture`,
/// `server_doctor_lists_onepassword_without_spawning`,
/// `doctor_reasons_name_each_cause`,
/// `doctor_selected_is_the_backend_a_write_uses_when_the_configs_differ`,
/// `doctor_reports_a_refused_tracked_setting_on_the_selected_row`.
pub(crate) fn doctor(state: &State, params: Value) -> Result<Value, ErrorKind> {
    let request: DoctorRequest = match params {
        Value::Null => DoctorRequest::default(),
        other => serde_json::from_value(other).map_err(|_| ErrorKind::InvalidParams)?,
    };
    let target = Target::of(state, &request)?;
    // #9326: the file backend is listed beside the Keychain. #7519 P4: every
    // CLI-backed id is listed, linked or not, so a missing row never hides why.
    let mut ids = vec![
        BackendId::keychain(),
        BackendId::file(),
        BackendId::onepassword(),
        BackendId::keeper(),
    ];
    if !ids.contains(&target.selected) {
        ids.push(target.selected.clone());
    }
    // Read only on a build that links a CLI backend, the only rows it judges.
    let account =
        (!cli_backends().is_empty()).then(|| Account::load(state.file_consent_config.as_deref()));
    let mut backends: Vec<BackendStatus> = ids
        .into_iter()
        .map(|id| judge(state, account.as_ref(), id))
        .collect();
    if let Some(detail) = target.refused
        && let Some(row) = backends.iter_mut().find(|row| row.id == target.selected)
    {
        *row =
            BackendStatus::unavailable(row.id.clone(), Unavailable::TrackedSettingRefused, detail);
    }
    to_json(&DoctorResponse {
        socket: state.settings.socket.clone(),
        index_root: state.settings.index_root.clone(),
        machine_config: state.settings.machine_config.clone(),
        project_root: target.root,
        project_config: target.config,
        posture: Some(StoragePosture::of(&target.selected)),
        selected_backend: target.selected,
        backends,
        account_config: state.file_consent_config.clone(),
        headless: Some(HeadlessReadiness {
            onepassword_token: state.start.onepassword_token,
        }),
    })
}

fn capability_names(caps: Capabilities) -> Vec<String> {
    [
        (Capabilities::READ, "READ"),
        (Capabilities::WRITE, "WRITE"),
        (Capabilities::LIST_NAMES, "LIST_NAMES"),
        (Capabilities::SYNC_TARGET, "SYNC_TARGET"),
    ]
    .into_iter()
    .filter(|(flag, _)| caps.contains(*flag))
    .map(|(_, name)| name.to_string())
    .collect()
}
