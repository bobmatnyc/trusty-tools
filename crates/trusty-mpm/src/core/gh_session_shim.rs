//! The per-call gh credential shim a pinned session runs (#9750, owner ruling
//! D6e).
//!
//! Why: before D6e a pinned session's environment carried the proven token
//! (`GH_TOKEN`, #8510). D6e removes it: no token in the child environment and
//! none in any log; `gh` obtains the pinned account's token per call from the
//! keyring. gh has no per-call credential hook and no `--user` flag on its
//! commands, so the "helper" is a tm-written script named `gh`, first on the
//! session's `PATH`. The script holds no secret.
//! What: [`write_gh_shim`] writes `<state_root>/gh-shim/<key>/gh` (dir 0700,
//! script 0755). Each run asks the real gh for `auth token --hostname <host>
//! --user <login>` under the config dir the launch check verified, bounded,
//! with every inherited token variable removed; puts the answer in the one
//! token variable gh reads for that host; and `exec`s the real gh. A spawn env
//! names the dir in [`GH_SHIM_ENV`];
//! [`crate::core::gh_shim_path::apply_gh_path`] turns that into a `PATH`
//! prefix where the child is built. [`launch_pinned_login`] is the
//! launch: verify, find the real gh, write the shim.
//! Test: `gh_session_shim_tests.rs`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::core::gh_account::PinnedGhIdentity;
use crate::core::gh_account_dir::AccountDirSources;
use crate::core::gh_account_proof::{GhTokenProbe, GhUserCheck, ProvenToken, token_var_for};
use crate::core::gh_login_verify::{VerifiedLogin, verify_pinned_login};
use crate::session_manager::ssh_host_alias::SshHostAliases;

/// The spawn-env variable naming a pinned session's shim dir (#9750).
///
/// Why: the daemon builds the env, but the `PATH` it must prefix is the pane's,
/// known only where the child is built
/// ([`crate::core::gh_shim_path::apply_gh_path`]).
/// What: also counts as selecting a gh identity, so
/// [`crate::core::gh_identity::inherited_identity_to_clear`] strips every
/// inherited token variable from the child.
pub(crate) const GH_SHIM_ENV: &str = "TM_GH_SHIM_DIR";

/// The directory under tm's state root that holds every shim.
const GH_SHIM_DIR_NAME: &str = "gh-shim";

/// How long the shim's `gh auth token` may run, in seconds: the bound tm's own
/// `gh auth` calls use ([`crate::core::gh_account::GH_ENFORCE_TIMEOUT`]), since
/// a locked keychain or a wedged `securityd` hangs gh (#6867).
pub(crate) const SHIM_TOKEN_BOUND_SECS: u64 = crate::core::gh_account::GH_ENFORCE_TIMEOUT.as_secs();

/// Every variable gh reads a token from; the shim removes all of them.
const TOKEN_VARS: [&str; 4] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

/// What a pinned spawn env carries for its credential, and the git credential
/// helper that goes with it.
///
/// Why: two credential shapes ride the same env builders. A session gets the
/// shim (#9750: no token). tm's own one-child git call keeps #8510's proven
/// token (`session_ref_identity`, plan Q4), which only that child sees.
/// What: `vars` to set; `helper` for `credential.https://<host>.helper`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PinnedCredential {
    /// The `(name, value)` pairs that select the credential.
    pub(crate) vars: Vec<(String, String)>,
    /// The git credential helper for the pinned host.
    pub(crate) helper: String,
}

/// `vars` may hold #8510's proven token; `Debug` redacts every `*TOKEN` value.
impl std::fmt::Debug for PinnedCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedCredential")
            .field("vars", &crate::core::gh_identity::RedactedVars(&self.vars))
            .field("helper", &self.helper)
            .finish()
    }
}

impl PinnedCredential {
    /// #8510's shape: the proven token in its host's token variable, and gh's
    /// own git credential helper, which hands git that token.
    /// Test: `a_proven_tm_account_dir_pin_isolates_gh_and_git`.
    pub(crate) fn from_proven(proven: &ProvenToken) -> Self {
        Self {
            vars: proven.identity_vars(),
            helper: crate::core::gh_session_account::GH_CREDENTIAL_HELPER.to_string(),
        }
    }

    /// #9750's shape: [`GH_SHIM_ENV`] naming `dir`, and the shim itself as
    /// git's helper, so git also gets the pinned account's token per call. No
    /// token variable is set, not even a placeholder.
    /// Test: `a_shim_credential_carries_no_token_and_names_the_shim_helper`,
    /// `a_global_tier_gh_call_in_a_pinned_session_resolves_through_the_shim`.
    pub(crate) fn shim(dir: &Path) -> Self {
        let gh = dir.join("gh");
        // #9750 (ruling D7c): no sentinel `GH_TOKEN`; a tm child under the
        // #8383 global tier inherits it and gh sent it, an HTTP 401.
        Self {
            vars: vec![(GH_SHIM_ENV.to_string(), dir.display().to_string())],
            helper: format!("!'{}' auth git-credential", gh.display()),
        }
    }
}

/// `PATH` with `dir` first, then `inherited` (#9750).
/// Test: `the_shim_dir_is_first_on_path`.
pub(crate) fn prefixed_path(dir: &OsStr, inherited: Option<&OsStr>) -> OsString {
    let mut path = OsString::from(dir);
    if let Some(rest) = inherited.filter(|p| !p.is_empty()) {
        path.push(":");
        path.push(rest);
    }
    path
}

/// The root every shim dir lives under.
pub(crate) fn shim_root(state_root: &Path) -> PathBuf {
    state_root.join(GH_SHIM_DIR_NAME)
}

/// The absolute `gh` on `path`, never one under `shim_root` (#9750).
///
/// Why: the shim `exec`s the real gh by absolute path; resolving through a
/// `PATH` that already holds a shim would make it call itself.
/// What: the first absolute `PATH` entry outside `shim_root` holding an
/// executable file `gh`. `Err` names the cause when there is none.
/// Test: `find_real_gh_skips_the_shim_root`, `find_real_gh_reports_a_missing_gh`.
pub(crate) fn find_real_gh(path: Option<&OsStr>, shim_root: &Path) -> Result<PathBuf, String> {
    let entries = path.map(std::env::split_paths).into_iter().flatten();
    entries
        .filter(|dir| dir.is_absolute() && !dir.starts_with(shim_root))
        .map(|dir| dir.join("gh"))
        .find(|gh| is_executable(gh))
        .ok_or_else(|| "gh is not installed on this PATH, so no gh call can run".to_string())
}

/// Is `path` a regular file with an execute bit?
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// A shell-safe single-quoted operand, or why `text` cannot be one.
///
/// What: refuses a quote, a backslash or any control character, so the
/// operand can never end its quotes or a line.
fn quoted(what: &str, text: &str) -> Result<String, String> {
    if text.is_empty()
        || text
            .chars()
            .any(|c| c == '\'' || c == '\\' || c.is_control())
    {
        return Err(format!(
            "the {what} {text:?} cannot be written into the gh shim"
        ));
    }
    Ok(format!("'{text}'"))
}

/// Is `login` a gh login tm may bake into a shim?
///
/// What: ASCII letters, digits, `-`, `_` and `.`, not starting with `-`, at
/// most 64 bytes — a github.com login, or an Enterprise Server one.
fn valid_login(login: &str) -> bool {
    !login.is_empty()
        && login.len() <= 64
        && !login.starts_with('-')
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// The shim script for `login` on `host`, asking `real_gh` under `config_dir`.
///
/// Why: see the module docs; the script is the only place the token exists,
/// and only inside the one gh process it `exec`s.
/// What: a POSIX `sh` script. It runs `<real_gh> auth token --hostname <host>
/// --user <login>` with `GH_CONFIG_DIR=<config_dir>` and the four token
/// variables removed, killed after `bound_secs`. On failure or empty output it
/// prints the fix to stderr and exits 1. Otherwise it removes the four token
/// variables, exports the token as the one gh reads for `host`
/// ([`token_var_for`]) and `exec`s the real gh with the caller's arguments.
/// `Err` when a value cannot be quoted safely.
/// Test: `the_shim_script_names_the_login_and_never_a_token`,
/// `a_ghes_shim_exports_gh_enterprise_token`,
/// `the_shim_refuses_a_login_it_cannot_quote`.
pub(crate) fn shim_script(
    verified: &VerifiedLogin,
    real_gh: &Path,
    bound_secs: u64,
) -> Result<String, String> {
    if !valid_login(&verified.login) {
        return Err(format!(
            "'{}' is not a gh login tm can write into the gh shim",
            verified.login
        ));
    }
    let login = &verified.login;
    let host = quoted("host", &verified.host)?;
    let real = quoted("gh path", &real_gh.display().to_string())?;
    let dir = quoted("config dir", &verified.config_dir.display().to_string())?;
    let var = token_var_for(&verified.host);
    let unset = TOKEN_VARS.join(" ");
    let env_u = TOKEN_VARS.map(|v| format!("-u {v}")).join(" ");
    let shown = &verified.host;
    Ok(format!(
        "#!/bin/sh\n\
         # trusty-mpm gh credential shim (#9750) for gh account '{login}' on {shown}.\n\
         # It holds no token: each gh call asks gh for that account's token and\n\
         # hands it to this one gh process only.\n\
         real={real}\n\
         token=$(\n\
         \x20 env {env_u} GH_CONFIG_DIR={dir} \"$real\" auth token --hostname {host} --user '{login}' &\n\
         \x20 pid=$!\n\
         \x20 (sleep {bound_secs}; kill \"$pid\") >/dev/null 2>&1 &\n\
         \x20 watchdog=$!\n\
         \x20 wait \"$pid\"\n\
         \x20 status=$?\n\
         \x20 kill \"$watchdog\" 2>/dev/null\n\
         \x20 exit \"$status\"\n\
         )\n\
         if [ $? -ne 0 ] || [ -z \"$token\" ]; then\n\
         \x20 echo \"tm: gh account '{login}' has no token gh can read on {shown} (logged out, keychain locked, or gh did not answer within {bound_secs}s). Run: gh auth login --hostname {shown} (as {login}) or gh auth refresh -h {shown} -u {login}.\" >&2\n\
         \x20 exit 1\n\
         fi\n\
         unset {unset}\n\
         {var}=\"$token\"\n\
         export {var}\n\
         unset token\n\
         exec \"$real\" \"$@\"\n"
    ))
}

/// A stable 64-bit FNV-1a hash of `bytes`, for a shim dir's name.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The shim dir for `verified`: one per login, host and config dir, so two
/// pins never overwrite each other's shim.
/// Test: `two_config_dirs_get_two_shims`.
pub(crate) fn shim_dir(state_root: &Path, verified: &VerifiedLogin) -> PathBuf {
    let dir = verified.config_dir.display().to_string();
    let host = verified.host.replace(':', "_");
    let name = format!("{}@{host}-{:016x}", verified.login, fnv1a(dir.as_bytes()));
    shim_root(state_root).join(name)
}

/// Write `verified`'s shim under `state_root` and return its dir (#9750).
///
/// What: the shim root and the shim dir are 0700; the script is written to a
/// temp file, set 0755 and renamed over `<dir>/gh`, so a concurrent session
/// never runs a half-written or non-executable shim. An unchanged script is not
/// rewritten.
/// Test: `the_shim_is_0755_inside_a_0700_dir`,
/// `a_written_shim_runs_gh_with_the_pinned_token_only`.
pub(crate) fn write_gh_shim(
    state_root: &Path,
    verified: &VerifiedLogin,
    real_gh: &Path,
    bound_secs: u64,
) -> Result<PathBuf, String> {
    let script = shim_script(verified, real_gh, bound_secs)?;
    let dir = shim_dir(state_root, verified);
    let fail = |what: &str, path: &Path, e: std::io::Error| {
        format!("cannot {what} {} ({e})", path.display())
    };
    std::fs::create_dir_all(&dir).map_err(|e| fail("create", &dir, e))?;
    set_mode(&shim_root(state_root), 0o700).map_err(|e| fail("protect", &dir, e))?;
    set_mode(&dir, 0o700).map_err(|e| fail("protect", &dir, e))?;
    let target = dir.join("gh");
    if std::fs::read_to_string(&target).is_ok_and(|text| text == script) {
        set_mode(&target, 0o755).map_err(|e| fail("protect", &target, e))?;
        return Ok(dir);
    }
    let temp = dir.join(format!(".gh.{}.tmp", std::process::id()));
    std::fs::write(&temp, script.as_bytes()).map_err(|e| fail("write", &temp, e))?;
    set_mode(&temp, 0o755).map_err(|e| fail("protect", &temp, e))?;
    std::fs::rename(&temp, &target).map_err(|e| fail("install", &target, e))?;
    Ok(dir)
}

/// Set `path`'s permission bits.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// What a launch needs to verify a login and write its shim.
pub(crate) struct LaunchSeams<'a> {
    /// The candidate config dirs ([`crate::core::gh_login_verify::pin_sources`]).
    pub(crate) sources: &'a AccountDirSources,
    /// Asks gh for a candidate's token.
    pub(crate) probe: &'a dyn GhTokenProbe,
    /// Asks GitHub whose token it is.
    pub(crate) check: &'a dyn GhUserCheck,
    /// The real gh, or why there is none ([`find_real_gh`]).
    pub(crate) real_gh: Result<PathBuf, String>,
    /// tm's state root, which holds the shim.
    pub(crate) state_root: &'a Path,
}

/// Verify `login` on `host` and write its shim: a pinned session's credential
/// (#9750).
///
/// Why: D6e — "launch verifies", then the session's gh reads the keyring per
/// call; no token reaches the env.
/// What: [`verify_pinned_login`], then the real gh, then [`write_gh_shim`];
/// `Ok` is [`PinnedCredential::shim`]. Each `Err` names its step and never
/// carries a token.
/// Test: `a_verified_login_launches_with_the_shim_and_no_token`,
/// `an_unverified_login_writes_no_shim`, `a_missing_gh_refuses_the_launch`.
pub(crate) fn launch_pinned_login(
    login: &str,
    host: &str,
    seams: LaunchSeams<'_>,
) -> Result<PinnedCredential, String> {
    let verified = verify_pinned_login(seams.sources, login, host, seams.probe, seams.check)?;
    let real_gh = seams.real_gh?;
    let dir = write_gh_shim(seams.state_root, &verified, &real_gh, SHIM_TOKEN_BOUND_SECS)?;
    Ok(PinnedCredential::shim(&dir))
}

/// Production entry point: [`launch_pinned_login`] for `pinned` on `origin`
/// with the real gh, `GET /user`, tm's static config and this process's `PATH`.
///
/// What: the host from [`crate::core::gh_login_verify::pinned_host`]; the
/// candidate dirs from [`crate::core::gh_login_verify::pin_sources`]. A
/// unit-test build never runs gh or the network (#8510), so it always refuses.
/// Test: none directly (real gh and network); the decisions are
/// [`launch_pinned_login`]'s.
pub(crate) fn launch_pinned_login_cli(
    pinned: &PinnedGhIdentity,
    origin: &str,
    state_root: &Path,
    aliases: &SshHostAliases,
    login: &str,
) -> Result<PinnedCredential, String> {
    use crate::core::gh_account_proof::{CliTokenProbe, HttpUserCheck};
    let host = crate::core::gh_login_verify::pinned_host(origin, aliases)?;
    let sources = crate::core::gh_login_verify::pin_sources(
        pinned,
        origin,
        state_root,
        &crate::core::trusty_tools_config::TrustyToolsConfig::load(),
        crate::core::gh_account::gh_config_dir(),
    );
    let path = std::env::var_os("PATH");
    launch_pinned_login(
        login,
        &host,
        LaunchSeams {
            sources: &sources,
            probe: &CliTokenProbe,
            check: &HttpUserCheck,
            real_gh: find_real_gh(path.as_deref(), &shim_root(state_root)),
            state_root,
        },
    )
}

/// The gh env an in-place relaunch of a pinned project hands `claude` (#9750,
/// plan Q1), with `launch` deciding the credential.
///
/// Why: a relaunch is interactive, so a login the keyring cannot prove refuses
/// it rather than starting a session whose every gh call fails.
/// What: [`crate::core::gh_session_account::session_spawn_env`] over `launch`,
/// as a [`crate::core::gh_identity::GhEnv`] that clears every inherited token
/// variable and names the shim. A refused `launch` is
/// [`crate::core::gh_login_verify::launch_refusal`]; any other refusal the env
/// builder reports (no host, no login) is a message ending "No token was
/// injected."; a pin with no usable account (a blank `account`) is
/// [`crate::core::gh_login_verify::no_login_refusal`], as `check_pin` refuses.
/// Test: `a_pinned_relaunch_env_clears_inherited_tokens_and_adds_the_shim`,
/// `a_refused_pinned_relaunch_is_the_launch_refusal`,
/// `a_blank_account_pin_refuses_the_relaunch_as_check_pin_does`.
pub(crate) fn pinned_relaunch_env_with(
    pinned: &PinnedGhIdentity,
    origin: &str,
    state_root: &Path,
    aliases: &SshHostAliases,
    launch: impl FnOnce(&str) -> Result<PinnedCredential, String>,
) -> Result<crate::core::gh_identity::GhEnv, String> {
    let host = crate::core::gh_login_verify::pinned_host(origin, aliases);
    let refused = std::cell::RefCell::new(None::<String>);
    let env = crate::core::gh_session_account::session_spawn_env(
        pinned,
        origin,
        state_root,
        aliases,
        |login| {
            launch(login).inspect_err(|reason| {
                let host = host.as_deref().unwrap_or("its host");
                let message = crate::core::gh_login_verify::launch_refusal(login, host, reason);
                *refused.borrow_mut() = Some(message);
            })
        },
    );
    if let Some(message) = refused.take() {
        return Err(message);
    }
    let unusable = |why: &str| format!("tm: cannot start session: {why}. No token was injected.");
    match env {
        Some(Ok(env)) => match env.warning {
            None => Ok(crate::core::gh_identity::GhEnv::from_identity_vars(
                env.vars,
            )),
            Some(warning) => Err(unusable(&warning)),
        },
        Some(Err(reason)) => Err(unusable(&reason)),
        // #9750: the caller passes a pin, so a pin with nothing usable in it
        // refuses, as `check_pin` does — never keeps the pane's own token.
        None => Err(crate::core::gh_login_verify::no_login_refusal(
            pinned,
            host.as_deref().map_err(String::as_str),
        )),
    }
}

/// Production entry point: the pinned gh env for an in-place relaunch in
/// `cwd`, or `None` when no pin applies (#9750).
///
/// What: the checkout's origin, its pin
/// ([`crate::core::gh_login_verify::session_pin_with`] over the host's registry
/// and `[accounts]` table), then [`pinned_relaunch_env_with`] with
/// [`launch_pinned_login_cli`]. An origin git cannot read, or an unreadable
/// pin, is an `Err`, as the daemon fails it closed. Blocking (gh and
/// `GET /user`).
/// Test: `a_relaunch_whose_origin_git_cannot_read_is_refused`; the other
/// decisions are [`pinned_relaunch_env_with`]'s and `session_pin_with`'s.
pub fn pinned_relaunch_env(cwd: &Path) -> Result<Option<crate::core::gh_identity::GhEnv>, String> {
    // #9750: a git error reading `origin` refuses; it never relaunches unpinned.
    let Some(origin) = crate::core::gh_login_verify::checkout_origin(cwd)? else {
        return Ok(None);
    };
    let aliases = SshHostAliases::for_current_user();
    let pinned = crate::core::gh_login_verify::session_pin_with(
        &origin,
        &crate::project::registry_data_dir(),
        &aliases,
        crate::core::gh_org_accounts::OrgAccounts::load_default,
    )
    .map_err(|e| format!("tm: cannot start session: {e}. No token was injected."))?;
    let Some(pinned) = pinned else {
        return Ok(None);
    };
    let state_root = crate::core::paths::FrameworkPaths::default().root;
    pinned_relaunch_env_with(&pinned, &origin, &state_root, &aliases, |login| {
        launch_pinned_login_cli(&pinned, &origin, &state_root, &aliases, login)
    })
    .map(Some)
}

#[cfg(test)]
#[path = "gh_session_shim_tests.rs"]
pub(crate) mod gh_session_shim_tests;
