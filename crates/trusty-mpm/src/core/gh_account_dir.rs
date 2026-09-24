//! The gh config dirs an account-only pin may ask for a token (#8510).
//!
//! Why: a registry record can pin `gh_account` with no `github.config_dir`.
//! #8416 put the registry ahead of the static config, so every such record
//! refused merged-PR lookups even when a config dir holding the account's
//! token exists. A dir is only a place to ASK `gh` for a candidate token; the
//! proof that the token is the pinned account's is a `GET /user`
//! ([`crate::core::gh_account_proof`]). #5851 is why a name is never trusted.
//!
//! What: [`AccountDirSources`] lists the candidate dirs, in order: the static
//! config's per-origin `github.config_dir`, tm's own `<state_root>/
//! gh-accounts/<login>`, then the daemon's own gh config dir.
//! [`refuse_unmigrated_config`] refuses a dir before `gh` ever runs in it when
//! its `config.yml` does not declare `version: "1"`: on such a dir gh 2.98.0
//! runs its multi-account migration, which can copy the active account's token
//! into another account's keyring slot. [`ensure_config_version`] is the
//! writer-side half, for tm's own dirs.
//!
//! Test: `gh_account_dir_tests`, `gh_account_registry_tests`.

use std::path::{Path, PathBuf};

use crate::core::trusty_tools_config::TrustyToolsConfig;
use crate::project::record::repo_url_matches;

/// Directory under the tm state root holding one `gh` config dir per account
/// (#7166); the daemon's `account_config_dir` bootstrap names it from here.
pub(crate) const GH_ACCOUNTS_DIR_NAME: &str = "gh-accounts";

/// The `version` a gh config dir's `config.yml` must declare before tm runs
/// `gh` in it (#8510). gh migrates a config that lacks it.
pub(crate) const GH_CONFIG_VERSION: &str = "1";

/// Reject a `login` that would escape or corrupt the
/// `<state_root>/gh-accounts/<login>` join (#7166 review follow-up MEDIUM).
///
/// Why: the CLI's `is_name_segment` restricts only the character set, so `.`
/// and `..` pass it; `..` would resolve to the state root itself.
/// What: refuses an empty `login`, exactly `.` or `..`, or one containing `/`
/// or `\`.
/// Test: `ensure_account_config_dir_refuses_dot`,
/// `ensure_account_config_dir_refuses_dotdot`,
/// `ensure_account_config_dir_refuses_empty`,
/// `ensure_account_config_dir_refuses_a_forward_slash`,
/// `ensure_account_config_dir_refuses_a_backslash`,
/// `an_account_only_pin_refuses_an_unsafe_login_segment`.
pub(crate) fn reject_unsafe_login_segment(login: &str) -> Result<(), String> {
    if login.is_empty()
        || login == "."
        || login == ".."
        || login.contains('/')
        || login.contains('\\')
    {
        return Err(format!(
            "'{login}' is not a valid account login for a config directory segment"
        ));
    }
    Ok(())
}

/// `true` when `path` exists and is ITSELF a symlink (#7166 review follow-up
/// LOW), checked with `symlink_metadata`, which does not follow the link.
fn is_symlink(path: &Path) -> bool {
    path.symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// tm's own `<state_root>/gh-accounts/<login>` dir, refused when unsafe.
///
/// Why: the daemon's bootstrap and the #8510 candidate list must apply the
/// same refusals to the same path; a second copy would drift.
/// What: [`reject_unsafe_login_segment`], then the join, then a refusal when
/// the dir, its `hosts.yml` or its `config.yml` is a symlink. `is_file()`
/// follows a link, so a symlinked file would otherwise pass as "already built",
/// and a write through it would land outside the dir.
/// Test: `ensure_account_config_dir_places_it_under_gh_accounts`,
/// `ensure_account_config_dir_refuses_a_symlinked_dir`,
/// `ensure_account_config_dir_refuses_a_symlinked_hosts_yml`,
/// `ensure_account_config_dir_refuses_a_symlinked_config_yml`,
/// `an_account_only_pin_refuses_a_symlinked_tm_account_dir`.
pub(crate) fn tm_account_dir(state_root: &Path, login: &str) -> Result<PathBuf, String> {
    reject_unsafe_login_segment(login)?;
    let dir = state_root.join(GH_ACCOUNTS_DIR_NAME).join(login);
    if is_symlink(&dir) {
        return Err(format!(
            "{} is a symlink — refusing to use it as a per-account gh config directory; \
             remove it and retry",
            dir.display()
        ));
    }
    for file in ["hosts.yml", "config.yml"] {
        let path = dir.join(file);
        if is_symlink(&path) {
            return Err(format!(
                "{} is a symlink — refusing to use it as a per-account gh config file; remove \
                 it and retry",
                path.display()
            ));
        }
    }
    Ok(dir)
}

/// The top-level `version` a `config.yml` text declares, if any.
fn declared_version(text: &str) -> Option<String> {
    let doc: serde_yaml::Value = serde_yaml::from_str(text).ok()?;
    match doc.get("version")? {
        serde_yaml::Value::String(s) => Some(s.trim().to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Refuse a dir `gh` would migrate, BEFORE `gh` runs in it (#8510 HIGH).
///
/// Why: on gh 2.98.0, any `gh` command under a config dir whose `config.yml`
/// lacks `version` runs the multi-account migration. That migration copied the
/// active account's token into another account's keyring slot and damaged the
/// operator's credentials. A read-only lookup must never trigger it.
/// What: `Ok(())` only when `<dir>/config.yml` is a regular file (not a
/// symlink) whose top-level `version` is [`GH_CONFIG_VERSION`]. A missing,
/// unreadable or unparsable file, a missing `version`, or any other version is
/// a named refusal.
/// Test: `a_candidate_without_a_config_version_is_refused_before_gh_runs`,
/// `a_candidate_without_a_config_yml_is_refused_before_gh_runs`,
/// `a_candidate_with_an_unknown_config_version_is_refused`.
pub(crate) fn refuse_unmigrated_config(dir: &Path) -> Result<(), String> {
    let path = dir.join("config.yml");
    let refuse = |why: String| {
        Err(format!(
            "{why}, so gh would run its config migration there, which can overwrite a keyring \
             slot — tm never runs gh against it"
        ))
    };
    if is_symlink(&path) {
        return refuse(format!("{} is a symlink", path.display()));
    }
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) => return refuse(format!("{} could not be read ({e})", path.display())),
    };
    match declared_version(&text) {
        Some(v) if v == GH_CONFIG_VERSION => Ok(()),
        Some(v) => refuse(format!(
            "{} declares version '{v}', not '{GH_CONFIG_VERSION}'",
            path.display()
        )),
        None => refuse(format!(
            "{} declares no `version: \"{GH_CONFIG_VERSION}\"`",
            path.display()
        )),
    }
}

/// Make tm's own account dir declare `version: "1"` in `config.yml` (#8510).
///
/// Why: a tm-built dir with no `version` is exactly the dir gh migrates, and
/// the `--account` clone path runs `gh` in it. Writing the version keeps gh's
/// migration from ever running there.
/// What: writes `version: "1"` as the whole file when `config.yml` is absent,
/// or as the first key ([`with_config_version`]) when the file declares no
/// `version`; a file that already declares one is left untouched. Refuses a
/// symlinked `config.yml`, and a file the added key would not leave as one
/// document declaring `version: "1"`. The file is written `0600`.
/// Test: `ensure_account_config_dir_writes_the_config_version`,
/// `ensure_account_config_dir_adds_the_version_to_a_copied_config`,
/// `ensure_account_config_dir_adds_the_version_to_a_reused_dir`,
/// `ensure_config_version_keeps_a_leading_document_marker`.
pub(crate) fn ensure_config_version(dir: &Path) -> Result<(), String> {
    let path = dir.join("config.yml");
    if is_symlink(&path) {
        return Err(format!(
            "{} is a symlink — refusing to write through it",
            path.display()
        ));
    }
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    if declared_version(&existing).is_some() {
        return Ok(());
    }
    let text = with_config_version(&existing);
    // #8510 r6: never write a file gh or the migration guard would misread.
    if declared_version(&text).as_deref() != Some(GH_CONFIG_VERSION) {
        return Err(format!(
            "{} cannot be given `version: \"{GH_CONFIG_VERSION}\"` as one YAML document — \
             refusing to rewrite it",
            path.display()
        ));
    }
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("cannot set permissions on {}: {e}", path.display()))?;
    }
    Ok(())
}

/// `text` with `version: "1"` added as its first key (#8510 r6).
///
/// Why: above a leading `---` marker the line opens a second YAML document,
/// so gh reads only the version and loses the operator's settings.
/// What: when the first line that is not blank, a comment or a `%` directive
/// is a `---` marker, the version goes on the line after it; otherwise it
/// goes first.
/// Test: `ensure_config_version_keeps_a_leading_document_marker`.
fn with_config_version(text: &str) -> String {
    let version = format!("version: \"{GH_CONFIG_VERSION}\"\n");
    let mut head = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('%') {
            head += line.len();
            continue;
        }
        let after_marker = line.strip_prefix("---").map(str::trim);
        if after_marker.is_some_and(|rest| rest.is_empty() || rest.starts_with('#')) {
            let (before, rest) = text.split_at(head + line.len());
            let newline = if before.ends_with('\n') { "" } else { "\n" };
            return format!("{before}{newline}{version}{rest}");
        }
        break;
    }
    format!("{version}{text}")
}

/// The config dirs an account-only pin may ask for a candidate token (#8510).
///
/// Why: see the module docs. A dir is never proof; every token it yields is
/// checked with `GET /user` before use.
/// What: the static config's per-project `config_dir` for this origin (the
/// global binding is not "for this origin"), tm's `<state_root>/gh-accounts/
/// <login>`, then the daemon's own gh config dir. `Default` names none: the
/// pre-#8510 refusal.
/// Test: `account_dir_sources_take_only_this_origins_static_binding`.
#[derive(Debug, Default, Clone)]
pub(crate) struct AccountDirSources {
    /// The static `projects[].github.config_dir` bound to this origin.
    pub(crate) static_config_dir: Option<PathBuf>,
    /// tm's state root, whose `gh-accounts/<login>` is the second candidate.
    pub(crate) state_root: Option<PathBuf>,
    /// The daemon's own gh config dir, asked with `-u <login>`.
    pub(crate) own_config_dir: Option<PathBuf>,
}

impl AccountDirSources {
    /// The candidates for `origin`: its static binding, tm's own account dirs,
    /// and the daemon's own gh config dir.
    /// Test: `account_dir_sources_take_only_this_origins_static_binding`.
    pub(crate) fn for_origin(
        config: &TrustyToolsConfig,
        origin: &str,
        state_root: PathBuf,
        own_config_dir: Option<PathBuf>,
    ) -> Self {
        let static_config_dir = config
            .projects
            .iter()
            .find(|p| repo_url_matches(&p.repo_url, origin))
            .and_then(|p| p.github.as_ref())
            .and_then(crate::core::gh_account_registry::selected_config_dir);
        Self {
            static_config_dir,
            state_root: Some(state_root),
            own_config_dir,
        }
    }

    /// Every candidate dir for `login`, in order, each either a path to probe
    /// or the reason it cannot be one. A path named twice is probed once.
    /// Test: `a_dir_named_twice_is_probed_once`.
    pub(crate) fn candidates(&self, login: &str) -> Vec<Result<PathBuf, String>> {
        let mut out: Vec<Result<PathBuf, String>> = Vec::new();
        let mut push = |candidate: Result<PathBuf, String>| {
            if let Ok(dir) = &candidate
                && out.iter().any(|c| c.as_ref() == Ok(dir))
            {
                return;
            }
            out.push(candidate);
        };
        if let Some(dir) = &self.static_config_dir {
            push(Ok(dir.clone()));
        }
        if let Some(root) = &self.state_root {
            push(tm_account_dir(root, login));
        }
        if let Some(dir) = &self.own_config_dir {
            push(Ok(dir.clone()));
        }
        out
    }
}

#[cfg(test)]
#[path = "gh_account_dir_tests.rs"]
pub(crate) mod gh_account_dir_tests;
