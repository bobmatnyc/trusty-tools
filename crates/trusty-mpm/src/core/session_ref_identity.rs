//! The gh identity a session-ref git network call runs as (#9160).
//!
//! Why: a catch-up's `git fetch` of `refs/tm/sessions/*` and a pause's
//! `ls-remote` / fetch / push ran with whatever credential the daemon
//! inherited — git config, osxkeychain — while only session SPAWNS used the
//! project's pinned gh identity (#8914). A project pinned to a non-active
//! account therefore read "Repository not found" on every resume.
//! What: [`session_ref_git_env`] answers, for one checkout, which env those
//! git calls get. A pin (registry record or `[accounts]` org map) whose token
//! `GET /user` proves is [`SessionRefGitEnv::Pinned`]: the same vars a session
//! spawn gets, plus a git credential-helper pin for the origin's host. A pin
//! with no proven token, an unreadable registry, or an unreadable origin is
//! [`SessionRefGitEnv::Refused`]: the caller makes NO network call, because
//! falling back would act as an identity the project did not pin. An
//! unpinned https origin, and every ssh or local origin, is
//! [`SessionRefGitEnv::Ambient`], with a note saying why — git keeps its own
//! credential and nothing claims a pinned identity.
//! Test: `session_ref_identity_tests.rs`.

use std::cell::RefCell;
use std::path::Path;

use crate::core::gh_account::{GH_USER_ENV_VAR, GhSpawnEnv, PinnedGhIdentity};
use crate::core::gh_account_proof::ProvenToken;
use crate::core::gh_org_accounts::{OrgAccounts, OrgAccountsError};
use crate::core::remote_url_redact::redact_stored_url;
use crate::session_manager::ssh_host_alias::SshHostAliases;

/// The gh config dir variable a config-dir pin sets.
const GH_CONFIG_DIR_VAR: &str = "GH_CONFIG_DIR";

/// The env a session-ref git network call runs with, and why.
///
/// Why: three outcomes need three different caller actions — run with these
/// vars, run with none, or do not run at all — and each failure must name the
/// credential it ran (or refused to run) under.
/// What: `Pinned.vars` holds a real token, so `Debug` redacts every `*TOKEN`
/// value; `identity` and every note and reason name logins and dirs only.
/// Test: `session_ref_identity_errors_never_carry_a_token`.
pub(crate) enum SessionRefGitEnv {
    /// git keeps its ambient credential; `note` says why, for logs and errors.
    Ambient {
        /// Why no gh identity applies.
        note: String,
    },
    /// A proven pin: inject `vars`; `identity` names the account and dir.
    Pinned {
        /// gh token/user/config-dir vars plus the git credential-helper pin.
        vars: Vec<(String, String)>,
        /// `gh account '<login>'` and, when set, `GH_CONFIG_DIR=<dir>`.
        identity: String,
    },
    /// A pin that cannot be proven or read: make no network call.
    Refused {
        /// The account, dir and reason, plus the no-fallback statement.
        reason: String,
    },
}

impl std::fmt::Debug for SessionRefGitEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ambient { note } => f.debug_struct("Ambient").field("note", note).finish(),
            Self::Pinned { vars, identity } => f
                .debug_struct("Pinned")
                .field("vars", &crate::core::gh_identity::RedactedVars(vars))
                .field("identity", identity)
                .finish(),
            Self::Refused { reason } => f.debug_struct("Refused").field("reason", reason).finish(),
        }
    }
}

impl SessionRefGitEnv {
    /// The vars to inject: a pin's own, else none.
    pub(crate) fn vars(&self) -> &[(String, String)] {
        match self {
            Self::Pinned { vars, .. } => vars,
            _ => &[],
        }
    }

    /// The refusal reason, when the call must not run.
    pub(crate) fn refusal(&self) -> Option<&str> {
        match self {
            Self::Refused { reason } => Some(reason),
            _ => None,
        }
    }

    /// `error` with the credential the git call ran under appended.
    /// Test: `a_pinned_fetch_failure_names_the_identity`.
    pub(crate) fn annotate(&self, error: &str) -> String {
        match self {
            Self::Ambient { note } => format!("{error} (git credential: {note})"),
            Self::Pinned { identity, .. } => format!("{error} (git ran as {identity})"),
            Self::Refused { reason } => reason.clone(),
        }
    }

    /// Log which credential `step` runs under; never a var value.
    pub(crate) fn log(&self, project: &Path, step: &str) {
        let project = project.display();
        match self {
            Self::Ambient { note } => tracing::info!(%project, "session-ref {step}: {note}"),
            Self::Pinned { identity, .. } => {
                tracing::debug!(%project, "session-ref {step} runs as {identity}");
            }
            Self::Refused { reason } => tracing::warn!(%project, "session-ref {step}: {reason}"),
        }
    }
}

/// Where [`session_ref_git_env_with`] reads pins and proves tokens.
///
/// Why: production reads the host's registry, `[accounts]` table, `gh` and
/// `GET /user`; a test supplies all four.
pub(crate) struct IdentitySeams<'a> {
    /// The directory holding `projects.json`.
    pub(crate) registry_dir: &'a Path,
    /// tm's state root, for the refusal's account-dir hint.
    pub(crate) state_root: &'a Path,
    /// `~/.ssh/config` host aliases.
    pub(crate) aliases: &'a SshHostAliases,
    /// Loads the `[accounts]` org map.
    pub(crate) load: &'a dyn Fn() -> Result<OrgAccounts, OrgAccountsError>,
    /// Proves `(pin, origin, login)`'s token with `GET /user`.
    pub(crate) prove: &'a dyn Fn(&PinnedGhIdentity, &str, &str) -> Result<ProvenToken, String>,
}

/// Production entry point: the host's registry, org map, `gh` and `GET /user`,
/// with proofs remembered in [`crate::core::gh_account_proof::PROCESS_PROOFS`].
///
/// Why: a catch-up or pause resolves the identity on every call; the cache
/// makes every call after the first proof a cache hit.
/// What: blocking (a `gh` subprocess and HTTPS); run it off the executor.
/// Test: the decisions are [`session_ref_git_env_with`]'s.
pub(crate) fn session_ref_git_env(repo: &Path) -> SessionRefGitEnv {
    let state_root = crate::core::paths::FrameworkPaths::default().root;
    let aliases = SshHostAliases::for_current_user();
    let registry_dir = crate::project::registry_data_dir();
    let prove = |pinned: &PinnedGhIdentity, origin: &str, login: &str| {
        crate::core::gh_account::prove_pinned_login(
            pinned,
            origin,
            &state_root,
            &aliases,
            login,
            Some(&crate::core::gh_account_proof::PROCESS_PROOFS),
        )
    };
    session_ref_git_env_with(
        repo,
        &IdentitySeams {
            registry_dir: &registry_dir,
            state_root: &state_root,
            aliases: &aliases,
            load: &OrgAccounts::load_default,
            prove: &prove,
        },
    )
}

/// [`session_ref_git_env`] over explicit seams (#9160).
///
/// What, in order: the origin (unreadable → `Refused`; none → `Ambient`); an
/// ssh or non-https origin → `Ambient`, saying a gh pin cannot apply (Q4); the
/// registry pin, else the `[accounts]` org map (either unreadable →
/// `Refused`; neither → `Ambient` "no gh pin", Q2); then the spawn path's own
/// [`crate::core::gh_session_account::session_spawn_env`] under `prove`. A
/// proven env gets the git credential-helper pin for its host when the spawn
/// env carries none; an unproven one is `Refused`.
/// Test: `a_proven_registry_pin_pins_gh_and_git`,
/// `a_proven_config_dir_pin_names_the_dir`,
/// `an_unproven_pin_refuses_naming_account_and_dir`,
/// `an_unpinned_origin_keeps_the_ambient_credential_and_says_so`,
/// `an_org_mapped_origin_uses_the_mapped_login`,
/// `an_unreadable_registry_refuses_without_proving`,
/// `an_ssh_origin_reports_that_the_pin_cannot_apply`,
/// `a_proven_identity_is_reused_across_two_resolutions`.
pub(crate) fn session_ref_git_env_with(repo: &Path, seams: &IdentitySeams<'_>) -> SessionRefGitEnv {
    use crate::core::remote_mode::{RemoteMode, remote_mode};
    let origin = match remote_mode(repo) {
        Ok(RemoteMode::Origin(origin)) => origin,
        // The git layer reports the missing origin in its own words.
        Ok(_) => return ambient("this checkout has no `origin` remote, so no gh pin applies"),
        Err(e) => {
            return refused(
                &format!(
                    "the git origin of {} could not be read ({e})",
                    repo.display()
                ),
                None,
                None,
            );
        }
    };
    let shown = redact_stored_url(&origin).to_string();
    match transport(&origin) {
        Transport::Https => {}
        // #9160 Q4: the helper pin is `credential.https://<host>.helper`.
        Transport::Ssh => {
            return ambient(&format!(
                "origin {shown} is an ssh remote: a gh account pin cannot apply to it (the gh \
                 credential helper serves https only), so git authenticates with its own ssh key"
            ));
        }
        Transport::Other => {
            return ambient(&format!(
                "origin {shown} is not an https remote, so no gh credential applies"
            ));
        }
    }

    let pinned = match crate::core::gh_account_registry::read_pin(seams.registry_dir, &origin) {
        Ok(Some(pin)) => crate::core::gh_account_registry::pinned_identity(&pin),
        Ok(None) => {
            match crate::core::gh_org_accounts::org_map_pin(&origin, seams.aliases, || {
                (seams.load)()
            }) {
                Ok(Some(pinned)) => pinned,
                // #9160 Q2: unpinned keeps the ambient credential, and says so.
                Ok(None) => {
                    return ambient(&format!(
                        "ambient git credential, no gh pin for {shown}. To pin one: `tm projects \
                         register <name> --repo-url {shown} --gh-account <login>`, or map the \
                         repository's org in the [accounts] table of ~/.trusty-mpm/config.toml"
                    ));
                }
                Err(env) => return refused(&warning_of(&env), None, None),
            }
        }
        Err(reason) => return refused(&reason, None, None),
    };
    if pinned.account.is_none() && pinned.config_dir.is_none() {
        return ambient(&format!(
            "the pin for {shown} names neither a gh login nor a gh config dir, so git uses the \
             ambient credential"
        ));
    }

    // The proof's own reason, which names the candidate dirs it tried.
    let failure = RefCell::new(None::<String>);
    let prove = |login: &str| {
        (seams.prove)(&pinned, &origin, login)
            .inspect_err(|e| *failure.borrow_mut() = Some(e.clone()))
    };
    let env = crate::core::gh_session_account::session_spawn_env(
        &pinned,
        &origin,
        seams.state_root,
        seams.aliases,
        prove,
    );
    let dir = pinned.config_dir.as_deref();
    match env {
        None => ambient(&format!("no gh login is pinned for {shown}")),
        Some(Err(reason)) => refused(&reason, pinned.account.as_deref(), dir),
        Some(Ok(env)) if env.warning.is_some() => {
            let reason = failure.take().unwrap_or_else(|| warning_of(&env));
            let login = var(&env.vars, GH_USER_ENV_VAR).or(pinned.account.as_deref());
            refused(&reason, login, dir)
        }
        Some(Ok(env)) => pinned_env(env.vars, &origin, seams.aliases),
    }
}

/// A proven spawn env, with git's credential helper pinned to gh for the host.
///
/// Why: an account-only pin's spawn env carries a token but no git config, so
/// git would still ask osxkeychain first; a config-dir pin already has it.
fn pinned_env(
    mut vars: Vec<(String, String)>,
    origin: &str,
    aliases: &SshHostAliases,
) -> SessionRefGitEnv {
    let login = var(&vars, GH_USER_ENV_VAR).map(str::to_string);
    let dir = var(&vars, GH_CONFIG_DIR_VAR).map(std::path::PathBuf::from);
    if var(&vars, "GIT_CONFIG_COUNT").is_none() {
        let host = crate::session_manager::worktree_reclaim_gh::proof_origin(origin, aliases)
            .and_then(|o| crate::core::gh_account_proof::origin_host(&o));
        match host {
            Ok(host) => vars.extend(crate::core::gh_session_account::git_credential_pin(&host)),
            Err(reason) => return refused(&reason, login.as_deref(), dir.as_deref()),
        }
    }
    SessionRefGitEnv::Pinned {
        vars,
        identity: who(login.as_deref(), dir.as_deref()),
    }
}

/// An [`SessionRefGitEnv::Ambient`] with `note`.
fn ambient(note: &str) -> SessionRefGitEnv {
    SessionRefGitEnv::Ambient {
        note: note.to_string(),
    }
}

/// The refusal text: who, the reason, the no-fallback statement (#9160).
fn refused(reason: &str, login: Option<&str>, dir: Option<&Path>) -> SessionRefGitEnv {
    let subject = match login.or(dir.map(|_| "<login>")) {
        Some(_) => format!("as {}", who(login, dir)),
        None => "as the project's pinned gh identity".to_string(),
    };
    SessionRefGitEnv::Refused {
        reason: format!(
            "session-ref git cannot run {subject}: {reason}. tm does not fall back to the ambient \
             git credential, so no session-ref network call was made (#9160)"
        ),
    }
}

/// `gh account '<login>'`, with `GH_CONFIG_DIR=<dir>` when one is set.
fn who(login: Option<&str>, dir: Option<&Path>) -> String {
    let account = format!("gh account '{}'", login.unwrap_or("<login>"));
    match dir {
        Some(dir) => format!("{account}, GH_CONFIG_DIR={}", dir.display()),
        None => account,
    }
}

/// The spawn env's diagnostic, or a generic one.
fn warning_of(env: &GhSpawnEnv) -> String {
    env.warning
        .clone()
        .unwrap_or_else(|| "the pinned gh identity could not be resolved".to_string())
}

/// The value of `name` in `vars`, if set.
fn var<'a>(vars: &'a [(String, String)], name: &str) -> Option<&'a str> {
    vars.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// How git reaches an origin, as far as a gh credential helper is concerned.
#[derive(Debug, PartialEq, Eq)]
enum Transport {
    /// `https://` — the gh credential helper pin applies.
    Https,
    /// `ssh://` or scp syntax `[user@]host:path` — git uses an ssh key.
    Ssh,
    /// A local path, `file://`, `http://`, `git://`.
    Other,
}

/// Classify `origin`'s transport.
/// Test: `transport_classifies_https_ssh_and_local_origins`.
fn transport(origin: &str) -> Transport {
    let o = origin.trim().to_ascii_lowercase();
    if o.starts_with("https://") {
        return Transport::Https;
    }
    if o.starts_with("ssh://") || o.starts_with("git+ssh://") {
        return Transport::Ssh;
    }
    let scp = !o.contains("://")
        && o.split_once(':')
            .is_some_and(|(host, _)| host.len() > 1 && !host.contains('/'));
    if scp {
        Transport::Ssh
    } else {
        Transport::Other
    }
}

#[cfg(test)]
#[path = "session_ref_identity_tests.rs"]
mod tests;
