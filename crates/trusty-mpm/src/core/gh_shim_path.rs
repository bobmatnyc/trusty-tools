//! Which `gh` a child of tm runs when tm itself runs inside a pinned session
//! (#9750).
//!
//! Why: a pinned session's `PATH` starts with its gh shim
//! ([`crate::core::gh_session_shim`]), which answers every `gh` call as the
//! session's account. tm runs inside such sessions. A tm gh child that selects
//! its own credential — another project's binding, a scoped config dir, an
//! explicit token — must run the real gh, or the shim replaces that credential
//! with the session's account. A child that selects nothing — no binding, or
//! the #8383 global tier's bare config dir — keeps the shim: it acts as its
//! caller, the session (#8383 T1).
//! What: [`gh_child_path`] decides the child's `PATH`; [`apply_gh_path`] and
//! [`bind_gh_path`] apply it to a `std::process::Command` and a
//! [`GhCommand`]; [`bypass_session_shim`] is the form for a child that picks
//! its credential by hand.
//! Test: `gh_shim_path_tests.rs`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use trusty_common::gh::GhCommand;

use crate::core::gh_session_shim::{GH_SHIM_ENV, prefixed_path, shim_root};

/// The variables that select a gh credential for a child when set.
///
/// #9750: `GH_CONFIG_DIR` is absent. Set with nothing removed — #8383's global
/// tier — it leaves the inherited credential deciding, as an inherited
/// `GH_TOKEN` would; in a pinned session that credential is the shim.
const SELECTING_VARS: [&str; 3] = ["GH_TOKEN", "GH_ENTERPRISE_TOKEN", GH_SHIM_ENV];

/// Every variable gh reads a token from.
const TOKEN_VARS: [&str; 4] = [
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
];

/// The environment a tm gh child's `PATH` is derived from.
///
/// What: this process's `PATH`, the shim dir a pinned session named in
/// [`GH_SHIM_ENV`], and the root every tm shim lives under.
#[derive(Debug, Clone)]
pub(crate) struct GhPathEnv {
    /// This process's `PATH`.
    pub(crate) path: Option<OsString>,
    /// The inherited [`GH_SHIM_ENV`] value.
    pub(crate) shim: Option<OsString>,
    /// `<state_root>/gh-shim`.
    pub(crate) shim_root: PathBuf,
}

impl GhPathEnv {
    /// This process's values, with tm's default state root.
    pub(crate) fn current() -> Self {
        Self {
            path: std::env::var_os("PATH"),
            shim: std::env::var_os(GH_SHIM_ENV),
            shim_root: shim_root(&crate::core::paths::FrameworkPaths::default().root),
        }
    }

    /// Is `entry` a gh shim dir?
    fn is_shim(&self, entry: &Path) -> bool {
        entry.starts_with(&self.shim_root) || self.shim.as_deref().is_some_and(|s| entry == s)
    }

    /// [`Self::path`] with every shim dir removed.
    fn without_shims(&self) -> Option<OsString> {
        let path = self.path.as_deref()?;
        let kept: Vec<PathBuf> = std::env::split_paths(path)
            .filter(|entry| !self.is_shim(entry))
            .collect();
        // An entry from `split_paths` never holds the separator, so this only
        // fails on a path no shell could have produced; keep it unchanged then.
        Some(std::env::join_paths(kept).unwrap_or_else(|_| path.to_os_string()))
    }
}

/// Does a child with these overrides select its own gh credential?
///
/// What: it removes an inherited gh token or config dir, or sets a token or a
/// shim dir. `GH_USER` alone selects nothing, nor does a bare `GH_CONFIG_DIR`
/// (see [`SELECTING_VARS`]), nor removing a variable gh never reads.
fn selects_identity(vars: &[(String, String)], unset: &[String]) -> bool {
    let removes = |k: &str| TOKEN_VARS.contains(&k) || k == "GH_CONFIG_DIR";
    unset.iter().any(|k| removes(k))
        || vars
            .iter()
            .any(|(k, _)| SELECTING_VARS.contains(&k.as_str()))
}

/// The `PATH` a tm child gets, or `None` to leave it inherited (#9750).
///
/// Why: see the module docs.
/// What: a child naming its own shim gets that shim first, then the inherited
/// `PATH` without any other shim. A child selecting another credential gets
/// the inherited `PATH` without shims, when there was one to remove. A child
/// selecting nothing keeps its `PATH`.
/// Test: `a_child_selecting_a_credential_drops_every_shim_from_path`,
/// `an_ambient_child_keeps_the_session_shim`,
/// `a_shimmed_child_leads_with_its_own_shim_only`.
pub(crate) fn gh_child_path(
    vars: &[(String, String)],
    unset: &[String],
    env: &GhPathEnv,
) -> Option<OsString> {
    if let Some((_, dir)) = vars.iter().find(|(k, _)| k == GH_SHIM_ENV) {
        let rest = env.without_shims();
        return Some(prefixed_path(OsStr::new(dir), rest.as_deref()));
    }
    if !selects_identity(vars, unset) {
        return None;
    }
    stripped_path(env)
}

/// The inherited `PATH` without shims, or `None` when it holds none.
fn stripped_path(env: &GhPathEnv) -> Option<OsString> {
    let stripped = env.without_shims()?;
    (Some(&stripped) != env.path.as_ref()).then_some(stripped)
}

/// Set `cmd`'s `PATH` per [`gh_child_path`] over this process's environment.
///
/// Why: the one applier every `std::process::Command` gh or session spawn
/// shares, so the shim choice is made in one place.
/// Test: `apply_gh_path_with_drops_the_session_shim_for_a_bound_child`.
pub(crate) fn apply_gh_path(
    cmd: &mut std::process::Command,
    vars: &[(String, String)],
    unset: &[String],
) {
    apply_gh_path_with(cmd, vars, unset, &GhPathEnv::current());
}

/// [`apply_gh_path`] over an explicit environment.
pub(crate) fn apply_gh_path_with(
    cmd: &mut std::process::Command,
    vars: &[(String, String)],
    unset: &[String],
    env: &GhPathEnv,
) {
    if let Some(path) = gh_child_path(vars, unset, env) {
        cmd.env("PATH", path);
    }
}

/// [`apply_gh_path`] for a [`GhCommand`].
/// Test: `bind_gh_path_with_runs_the_real_gh_for_another_project`.
pub fn bind_gh_path(cmd: GhCommand, vars: &[(String, String)], unset: &[String]) -> GhCommand {
    bind_gh_path_with(cmd, vars, unset, &GhPathEnv::current())
}

/// [`bind_gh_path`] over an explicit environment.
pub(crate) fn bind_gh_path_with(
    cmd: GhCommand,
    vars: &[(String, String)],
    unset: &[String],
    env: &GhPathEnv,
) -> GhCommand {
    match gh_child_path(vars, unset, env) {
        Some(path) => cmd.env("PATH", path),
        None => cmd,
    }
}

/// A [`GhCommand`] that picks its credential by hand (a scoped config dir, an
/// explicit token, `--user`) runs the real gh (#9750).
///
/// What: drops every shim dir from `PATH`, and removes each inherited token
/// variable holding [`crate::core::gh_account::REFUSED_GH_TOKEN`] — the
/// sentinel a refused pin's session carries. Call it before the command sets
/// its own variables.
/// Test: `bypass_session_shim_with_drops_the_shim_and_the_sentinel`.
pub(crate) fn bypass_session_shim(cmd: GhCommand) -> GhCommand {
    let inherited: Vec<(String, String)> = TOKEN_VARS
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| ((*k).to_string(), v)))
        .collect();
    bypass_session_shim_with(cmd, &inherited, &GhPathEnv::current())
}

/// [`bypass_session_shim`] over explicit inherited token variables and
/// environment.
pub(crate) fn bypass_session_shim_with(
    cmd: GhCommand,
    inherited_tokens: &[(String, String)],
    env: &GhPathEnv,
) -> GhCommand {
    let refused = crate::core::gh_account::REFUSED_GH_TOKEN;
    let cmd = inherited_tokens
        .iter()
        .filter(|(k, v)| v == refused && TOKEN_VARS.contains(&k.as_str()))
        .fold(cmd, |cmd, (k, _)| cmd.env_remove(k));
    match stripped_path(env) {
        Some(path) => cmd.env("PATH", path),
        None => cmd,
    }
}

#[cfg(test)]
#[path = "gh_shim_path_tests.rs"]
mod gh_shim_path_tests;
